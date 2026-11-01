//! Network sync loop (spec section 8.2) over every discovery route.

mod dial;
mod fetch;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use swarm_core::accept::AcceptState;
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_net::NetError;
use swarm_store::RepoStore;

use crate::net::NetState;
use dial::{Candidate, Dialer, MAX_SESSIONS};

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
    /// Accepted manifests.
    pub accepted: usize,
    /// Rejected envelopes.
    pub rejected: usize,
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

    // Route C runs beside the others and is only waited for when they fall
    // short (DHT-1).
    let mut dht_lookup = Some(tokio::spawn(crate::net::dht::find(net.dht(), repo)));

    let mut candidates: Vec<Candidate> = swarm_disc::resolve_hints(&hints)
        .await
        .into_iter()
        .map(|candidate| Candidate {
            addr: candidate.addr,
            node: candidate.node,
        })
        .collect();
    candidates.extend(dial::cached_candidates(net, repo));
    let lan = net.lan();
    if let Some(lan) = &lan {
        lan.query(repo);
        candidates.extend(dial::unpinned(lan.candidates(repo)));
    }

    let mut dialer = Dialer::new(net, repo);
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
            dial::dedupe(exchanged, net.node_id),
            MAX_SESSIONS,
        )
        .await;

    if dialer.connected.is_empty() {
        if let Some(lookup) = dht_lookup.take() {
            let found = lookup.await.unwrap_or_default();
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

    // Route B answers within the query burst (0 s, 1 s, 3 s). Wait for it only
    // when nothing else connected: up to find_timeout for a first clone
    // (SYNC-6), and just past the burst when we already have state.
    if let (true, Some(lan)) = (dialer.connected.is_empty(), &lan) {
        let deadline = tokio::time::Instant::now() + dial::lan_wait(has_state);
        while dialer.connected.is_empty() {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            lan.wait_for_candidates(deadline - now).await;
            let fresh = dial::dedupe(dial::unpinned(lan.candidates(repo)), net.node_id);
            dialer.dial_all(net, repo, fresh, MAX_SESSIONS).await;
        }
    }

    if dialer.connected.is_empty() {
        abort(dht_lookup);
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
            NetError::NoPeers
        });
    }

    let (mut accepted, mut rejected) =
        fetch::fetch_heads(repo, &mut net_state, &mut dialer.connected).await;
    if net_state.signers.is_empty() {
        abort(dht_lookup);
        return Err(NetError::NoPeers);
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
    let missing_now = || missing(&store, &store.resolve(&net_state));
    fetch::fetch_missing(
        &store,
        repo,
        &mut dialer.connected,
        missing_now,
        haves,
        net.max_pack_bytes,
    )
    .await?;

    // Manifests are stored only once their objects are: a node never
    // advertises heads it cannot serve (a fetch that fails is retried by
    // the next sync, which starts again from the stored state).
    store
        .persist_state(&net_state)
        .map_err(|err| NetError::Malformed(err.to_string()))?;
    persist_db(net, repo, &object_format, &net_state, row.is_none())?;

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
    let Some(root) = state.signers.get(&KeyId::from_bytes(*repo.as_bytes())) else {
        return 0;
    };
    net.dht_head(repo, &root.manifest.signer_keys)
        .await
        .map_or(0, |head| head.seq)
}

fn missing(store: &RepoStore, resolved: &swarm_core::Resolved) -> Vec<Oid> {
    let mut missing: Vec<Oid> = resolved
        .canonical
        .values()
        .filter(|oid| !object_present(store, oid))
        .cloned()
        .collect();
    missing.sort_by(|a, b| a.as_slice().cmp(b.as_slice()));
    missing.dedup_by(|a, b| a.as_slice() == b.as_slice());
    missing
}

fn root_seq(state: &AcceptState, repo: RepoId) -> u64 {
    state
        .signers
        .get(&KeyId::from_bytes(*repo.as_bytes()))
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
        accepted: counts.accepted,
        rejected: counts.rejected,
    }
}
