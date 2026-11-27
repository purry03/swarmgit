//! Network sync loop (spec section 8.2) over every discovery route.

mod dial;
mod fetch;
pub mod progress;
mod relayed;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use swarm_core::about::VerifiedAbout;
use swarm_core::accept::AcceptState;
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_net::NetError;
use swarm_store::RepoStore;

use crate::net::NetState;
use dial::{Candidate, Dialer, MAX_SESSIONS};
use progress::Progress;

/// Outcome of one sync.
#[derive(Clone, Debug)]
pub struct SyncResult {
    /// `fresh` or `stale`.
    pub state: String,
    /// Canonical refs.
    pub ref_list: BTreeMap<String, Oid>,
    /// Default branch.
    pub head: Option<String>,
    /// Root sequence.
    pub seq: u64,
    /// Store path.
    pub store_path: PathBuf,
    /// Object format.
    pub object_format: String,
    /// Connected peers.
    pub peers: usize,
    /// Node IDs of connected peers that returned repository metadata.
    pub peer_ids: Vec<String>,
    /// Accepted manifests.
    pub accepted: usize,
    /// Rejected envelopes.
    pub rejected: usize,
    /// Name and description, set by an inspect.
    pub about: Option<VerifiedAbout>,
}

impl SyncResult {
    /// Number of canonical refs.
    #[must_use]
    pub fn refs(&self) -> usize {
        self.ref_list.len()
    }
}

/// Sync one repo from every route: open sessions, link hints, cached and
/// exchanged peers, LAN and the DHT (DM-1).
pub async fn sync_repo(
    net: &Arc<NetState>,
    repo: RepoId,
    hints: Vec<String>,
    progress: &Progress,
) -> Result<SyncResult, NetError> {
    sync_repo_inner(net, repo, hints, progress, false).await
}

/// Read and verify repository heads without requesting Git objects or writing
/// a local repository store.
pub async fn inspect_repo(
    net: &Arc<NetState>,
    repo: RepoId,
    hints: Vec<String>,
    progress: &Progress,
) -> Result<SyncResult, NetError> {
    sync_repo_inner(net, repo, hints, progress, true).await
}

async fn sync_repo_inner(
    net: &Arc<NetState>,
    repo: RepoId,
    hints: Vec<String>,
    progress: &Progress,
    metadata_only: bool,
) -> Result<SyncResult, NetError> {
    let lock = net.sync_lock(repo);
    let _guard = lock.lock().await;

    let row = {
        let db = net
            .db
            .lock()
            .map_err(|_| NetError::Malformed("database lock poisoned".into()))?;
        db.repo(repo)
            .map_err(|err| NetError::Malformed(err.to_string()))?
    };
    let store = net.store(repo);
    let mut net_state = if store.exists() {
        store
            .load_state()
            .map_err(|err| NetError::Malformed(err.to_string()))?
    } else {
        AcceptState::new(repo)
    };
    let has_state = store.exists() || !net_state.signers.is_empty();

    progress.say("find", "looking for peers");
    // Route C runs beside the others and is only waited for when they fall
    // short (DHT-1).
    let mut dht_lookup = Some(tokio::spawn(crate::net::dht::find(net.dht(), repo)));

    let mut candidates: Vec<Candidate> = swarm_disc::resolve_hints(&hints, net.settings.proxied())
        .await
        .into_iter()
        .map(|candidate| Candidate {
            addr: candidate.addr,
            node: candidate.node,
        })
        .collect();
    let from_link = candidates.len();
    let cached = dial::cached_candidates(net, repo);
    let cached = dial::without_link_local(cached);
    let from_cache = cached.len();
    candidates.extend(cached);
    if from_link + from_cache > 0 {
        progress.say(
            "find",
            format!("{from_link} address(es) from the link, {from_cache} remembered"),
        );
    }

    let lan = net.lan();
    if let Some(lan) = &lan {
        lan.query(repo);
        candidates.extend(dial::unpinned(lan.candidates(repo)));
    }

    let mut dialer = Dialer::new(net, repo, progress.clone());
    if !dialer.connected.is_empty() {
        progress.say(
            "find",
            format!(
                "already connected to {} peer(s); not dialing them again",
                dialer.connected.len()
            ),
        );
    }
    let retry_candidates = dial::dedupe(candidates.clone(), net.node_id);
    dialer
        .dial_all(
            net,
            repo,
            dial::dedupe(candidates, net.node_id),
            MAX_SESSIONS,
        )
        .await;
    // Peer exchange (6.5): learn and remember the peers our peers know, and
    // fill the remaining session slots from them.
    let exchanged = dial::exchange_peers(net, repo, &dialer.connected).await;
    dialer
        .dial_all(
            net,
            repo,
            dial::dedupe(dial::without_link_local(exchanged), net.node_id),
            MAX_SESSIONS,
        )
        .await;

    let mut dht_found = 0;
    if dialer.connected.is_empty() {
        if let Some(lookup) = dht_lookup.take() {
            progress.say("dht", "searching the DHT for peers");
            let found = lookup.await.unwrap_or_default();
            dht_found = found.len();
            progress.say("dht", format!("the DHT returned {dht_found} address(es)"));
            dialer
                .dial_all(
                    net,
                    repo,
                    dial::dedupe(dial::unpinned(found), net.node_id),
                    MAX_SESSIONS,
                )
                .await;
        }
    }

    // Nothing direct worked: look for the relays that hold the nodes we know
    // by ID (REL-29, REL-32).
    if dialer.connected.is_empty() {
        relayed::dial(net, repo, &mut dialer, &retry_candidates, progress).await;
    }

    // Route B answers within the query burst (0 s, 1 s, 3 s). Wait for it only
    // when nothing else connected: up to find_timeout for a first clone
    // (SYNC-6), and just past the burst when we already have state.
    if let (true, Some(lan)) = (dialer.connected.is_empty(), &lan) {
        let deadline = tokio::time::Instant::now() + dial::lan_wait(has_state);
        progress.say(
            "lan",
            format!(
                "no peer yet; waiting up to {}s for one on the local network",
                dial::lan_wait(has_state).as_secs().max(1)
            ),
        );
        let mut last_note = tokio::time::Instant::now();
        while dialer.connected.is_empty() {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            if now.duration_since(last_note) >= std::time::Duration::from_secs(5) {
                last_note = now;
                progress.say(
                    "lan",
                    format!("still looking... {}s left", (deadline - now).as_secs()),
                );
            }
            lan.wait_for_candidates((deadline - now).min(std::time::Duration::from_secs(1)))
                .await;
            let fresh = dial::dedupe(dial::unpinned(lan.candidates(repo)), net.node_id);
            dialer.dial_all(net, repo, fresh, MAX_SESSIONS).await;
        }
    }

    if dialer.connected.is_empty() {
        abort(dht_lookup);
        net.set_seeders(repo, 0);
        let resolved = store.resolve(&net_state);
        if !resolved.canonical.is_empty() || !net_state.signers.is_empty() {
            let _ = store.apply_canonical(&resolved.canonical).await;
            return Ok(result(
                "stale",
                &store,
                &resolved,
                root_seq(&net_state, repo),
                &net_state,
                Counts::default(),
            ));
        }
        return Err(if dialer.failures.auth {
            NetError::AuthFailed("every pinned peer failed authentication".into())
        } else if dialer.failures.no_shared {
            NetError::NoSharedRepo
        } else {
            NetError::NoPeersTried(no_peers_detail(
                &dialer.failures,
                dht_found,
                lan.is_some(),
                !crate::net::relay::configured().is_empty(),
            ))
        });
    }

    progress.say(
        "heads",
        format!(
            "connected to {} peer(s); reading the repo state",
            dialer.connected.len()
        ),
    );
    let (mut accepted, mut rejected) =
        fetch::fetch_heads(repo, &mut net_state, &mut dialer.connected).await;
    if net_state.signers.is_empty() && dialer.drop_failed() > 0 {
        // A listed session that cannot answer is dead (the network changed,
        // the peer restarted). Dial the same peers again on fresh sessions.
        progress.say("find", "a session was stale; reconnecting");
        let first = dialer.connected.len();
        dialer
            .dial_all(net, repo, retry_candidates, MAX_SESSIONS)
            .await;
        let (more_accepted, more_rejected) =
            fetch::fetch_heads(repo, &mut net_state, &mut dialer.connected[first..]).await;
        accepted += more_accepted;
        rejected += more_rejected;
    }
    // Only peers that answered with the repo's state count as seeders; the
    // peer cache also holds dead and never-contacted addresses.
    let seeders: BTreeSet<[u8; 32]> = dialer
        .connected
        .iter()
        .filter(|peer| !peer.failed && peer.note.is_none())
        .map(|peer| *peer.node().as_bytes())
        .filter(|node| node != net.node_id.as_bytes())
        .collect();
    net.set_seeders(repo, seeders.len());
    if net_state.signers.is_empty() {
        abort(dht_lookup);
        let detail = dialer
            .connected
            .iter()
            .map(|peer| {
                let node = peer.node().to_base32();
                let why = peer.note.as_deref().unwrap_or("gave no repo state");
                format!("{}: {why}", &node[..node.len().min(8)])
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(NetError::NoPeersTried(format!(
            "connected to {} peer(s) but none gave this repo's state ({detail}). The peer \
             may not hold this repo, or may still be setting it up; try again, or check \
             `swarm status` on the peer.",
            dialer.connected.len()
        )));
    }

    if metadata_only {
        let root = net_state.root_manifest().ok_or_else(|| {
            NetError::Malformed("repository has no verified root manifest".into())
        })?;
        let resolved = swarm_core::resolve::resolve(&net_state, &NoObjects);
        let peer_ids: Vec<String> = dialer
            .connected
            .iter()
            .filter(|peer| !peer.failed)
            .map(|peer| peer.node().to_base32())
            .collect();
        return Ok(SyncResult {
            state: "fresh".to_string(),
            ref_list: resolved.canonical,
            head: root.manifest.head.clone(),
            seq: root.manifest.seq,
            store_path: PathBuf::new(),
            object_format: net_state
                .object_format
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            peers: peer_ids.len(),
            peer_ids,
            accepted,
            rejected,
            about: fetch::fetch_about(repo, &net_state, &dialer.connected).await,
        });
    }

    // DHT-5: the head pointer says which root seq exists. While our peers
    // only offer older manifests, keep looking on the DHT (SYNC-3).
    let wanted = head_seq(net, repo, &net_state).await;
    let mut rounds = 0;
    while wanted > root_seq(&net_state, repo) && rounds < HEAD_ROUNDS {
        rounds += 1;
        let found = match dht_lookup.take() {
            Some(lookup) => lookup.await.unwrap_or_default(),
            None => crate::net::dht::find(net.dht(), repo).await,
        };
        let added = dialer
            .dial_all(
                net,
                repo,
                dial::dedupe(dial::unpinned(found), net.node_id),
                MAX_SESSIONS + HEAD_EXTRA,
            )
            .await;
        if added.is_empty() {
            continue;
        }
        let first = added[0];
        let (more_accepted, more_rejected) =
            fetch::fetch_heads(repo, &mut net_state, &mut dialer.connected[first..]).await;
        accepted += more_accepted;
        rejected += more_rejected;
    }
    abort(dht_lookup);

    let object_format = net_state
        .object_format
        .clone()
        .or_else(|| row.as_ref().map(|row| row.object_format.clone()));
    let Some(object_format) = object_format else {
        return Err(NetError::Malformed(
            "root manifest has no object format".into(),
        ));
    };
    if !store.exists() {
        store
            .create(&object_format)
            .await
            .map_err(|err| NetError::Malformed(err.to_string()))?;
    }
    let resolved = store.resolve(&net_state);
    let haves = existing_haves(&store, &resolved).await;
    let missing_now = || missing(&store, &store.resolve(&net_state), &net_state);
    progress.say("pack", "downloading objects");
    fetch::fetch_missing(
        &store,
        repo,
        &mut dialer.connected,
        missing_now,
        haves,
        net.max_pack_bytes,
        progress,
    )
    .await
    .map_err(|err| name_the_relay(net, &dialer.connected, err))?;
    progress.say("done", "download complete");

    // Manifests are stored only once their objects are: a node never
    // advertises heads it cannot serve (a fetch that fails is retried by
    // the next sync, which starts again from the stored state).
    store
        .adopt_newer(&mut net_state)
        .and_then(|()| store.persist_state(&net_state))
        .map_err(|err| NetError::Malformed(err.to_string()))?;
    persist_db(net, repo, &object_format, &net_state, row.is_none())?;
    if let Some(about) = fetch::fetch_about(repo, &net_state, &dialer.connected).await {
        if let Err(err) = store.write_about(&about, &net_state, crate::net::now() as u64) {
            tracing::debug!(error = %err, "cannot store the repo name");
        }
    }

    let resolved = store.resolve(&net_state);
    store
        .apply_canonical(&resolved.canonical)
        .await
        .map_err(|err| NetError::Malformed(err.to_string()))?;
    {
        let db = net
            .db
            .lock()
            .map_err(|_| NetError::Malformed("database lock poisoned".into()))?;
        let _ = db.set_last_sync(repo, crate::net::now());
    }
    if row.is_none() {
        // A first clone seeds (SEED-1): announce it on the DHT right away.
        net.announce_dht(repo);
    }
    Ok(result(
        "fresh",
        &store,
        &resolved,
        root_seq(&net_state, repo),
        &net_state,
        Counts {
            accepted,
            rejected,
            peers: dialer.connected.len(),
        },
    ))
}

/// A transfer cut off while it went through a relay says which relay, since
/// that is the first thing to check.
fn name_the_relay(net: &NetState, peers: &[dial::Connected], err: NetError) -> NetError {
    let cut = matches!(
        err,
        NetError::Io(_) | NetError::Timeout(_) | NetError::Closed
    );
    let relay = peers
        .iter()
        .find_map(|peer| net.relay_of(peer.handle.info().node_id));
    match relay {
        Some(relay) if cut => NetError::Relay(format!(
            "the transfer through the relay {relay} was cut off ({err}); try again"
        )),
        _ => err,
    }
}

/// The text of the error when no peer could be reached.
fn no_peers_detail(
    failures: &dial::Failures,
    dht_found: usize,
    lan: bool,
    relays_configured: bool,
) -> String {
    let mut text = format!("could not reach any peer ({}", failures.summary());
    if lan {
        text.push_str("; nobody answered on the local network");
    }
    text.push_str(&format!("; the DHT returned {dht_found})."));
    if failures.timed_out > 0 || failures.unreachable > 0 {
        text.push_str(
            " The peer may be offline, or behind NAT or mobile data and not reachable from \
             here. Put both machines on the same network or VPN, or have the seed forward \
             a port.",
        );
    }
    let relays_off = crate::net::relay::mode() == crate::net::relay::Mode::Off;
    if (failures.tried == 0 || failures.timed_out > 0 || failures.unreachable > 0)
        && failures.relay_tried == 0
        && !relays_configured
        && !relays_off
    {
        text.push_str(
            " Or run a relay (swarm-relay) and set SWARM_RELAYS=host:port on both \
             machines so they can reach each other through it.",
        );
    }
    text
}

/// Extra rounds of DHT lookups while the head pointer is ahead.
const HEAD_ROUNDS: usize = 3;
/// Sessions allowed beyond [`MAX_SESSIONS`] to reach a newer head.
const HEAD_EXTRA: usize = 4;

fn abort(lookup: Option<tokio::task::JoinHandle<Vec<std::net::SocketAddr>>>) {
    if let Some(lookup) = lookup {
        lookup.abort();
    }
}

/// The root seq the DHT head pointer announces, or 0.
async fn head_seq(net: &NetState, repo: RepoId, state: &AcceptState) -> u64 {
    // The DHT head pointer is signed by the original root key (DHT-5); it
    // says nothing once a recovery rotation has moved the root.
    if state.root != KeyId::from_bytes(*repo.as_bytes()) {
        return 0;
    }
    let Some(root) = state.root_manifest() else {
        return 0;
    };
    net.dht_head(repo, &root.manifest.signer_keys)
        .await
        .map_or(0, |head| head.seq)
}

/// Objects to download: the canonical tips, and the tips every authorized
/// signer lists. A ref whose candidates diverge can only be resolved (or
/// reported as a conflict) once all of them are here (5.5).
fn missing(store: &RepoStore, resolved: &swarm_core::Resolved, state: &AcceptState) -> Vec<Oid> {
    let signed = state
        .signers
        .iter()
        .filter(|(signer, _)| state.is_authorized(**signer))
        .flat_map(|(_, stored)| stored.manifest.refs.values());
    let mut missing: Vec<Oid> = resolved
        .canonical
        .values()
        .chain(signed)
        .filter(|oid| !object_present(store, oid))
        .cloned()
        .collect();
    missing.sort_by(|a, b| a.as_slice().cmp(b.as_slice()));
    missing.dedup_by(|a, b| a.as_slice() == b.as_slice());
    missing
}

fn root_seq(state: &AcceptState, repo: RepoId) -> u64 {
    let _ = repo;
    state
        .root_manifest()
        .map(|stored| stored.manifest.seq)
        .unwrap_or(0)
}

fn object_present(store: &RepoStore, oid: &Oid) -> bool {
    // `history_is_complete` is async; this helper is only used after a pack
    // indexed, so a cheap synchronous check is enough here.
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(store.store_path())
        .args(["cat-file", "-e", &oid.to_hex()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

async fn existing_haves(store: &RepoStore, resolved: &swarm_core::Resolved) -> Vec<Oid> {
    let mut haves = Vec::new();
    for oid in resolved.canonical.values() {
        if haves.len() >= 1024 {
            break;
        }
        if store.git().has_object(oid).await {
            haves.push(oid.clone());
        }
    }
    haves
}

fn persist_db(
    net: &NetState,
    repo: RepoId,
    object_format: &str,
    state: &AcceptState,
    is_new: bool,
) -> Result<(), NetError> {
    let db = net
        .db
        .lock()
        .map_err(|_| NetError::Malformed("database lock poisoned".into()))?;
    let now = crate::net::now();
    db.ensure_repo(repo, object_format, now)
        .map_err(|err| NetError::Malformed(err.to_string()))?;
    for (signer, stored) in &state.signers {
        db.upsert_signer(repo, *signer, stored.manifest.seq, &stored.manifest_id)
            .map_err(|err| NetError::Malformed(err.to_string()))?;
    }
    if is_new {
        db.set_seeding(repo, true)
            .map_err(|err| NetError::Malformed(err.to_string()))?;
    }
    Ok(())
}

#[derive(Default)]
struct Counts {
    accepted: usize,
    rejected: usize,
    peers: usize,
}

/// A result built from what the store holds, without touching the network.
pub(crate) fn local_result(net: &Arc<NetState>, repo: RepoId, label: &str) -> Option<SyncResult> {
    let store = net.store(repo);
    let state = store.load_state().ok()?;
    let resolved = store.resolve(&state);
    Some(result(
        label,
        &store,
        &resolved,
        root_seq(&state, repo),
        &state,
        Counts::default(),
    ))
}

fn result(
    state: &str,
    store: &RepoStore,
    resolved: &swarm_core::Resolved,
    seq: u64,
    net_state: &AcceptState,
    counts: Counts,
) -> SyncResult {
    let object_format = net_state
        .object_format
        .clone()
        .unwrap_or_else(|| "sha1".to_string());
    SyncResult {
        state: state.to_string(),
        ref_list: resolved.canonical.clone(),
        head: resolved.head.clone(),
        seq,
        store_path: store.store_path(),
        object_format,
        peers: counts.peers,
        peer_ids: Vec::new(),
        accepted: counts.accepted,
        rejected: counts.rejected,
        about: None,
    }
}

struct NoObjects;

impl swarm_core::resolve::Ancestry for NoObjects {
    fn is_ancestor(&self, _a: &Oid, _b: &Oid) -> Option<bool> {
        None
    }
}
