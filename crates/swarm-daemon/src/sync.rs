//! Network sync loop (spec section 8.2) over link hints only.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use swarm_core::accept::AcceptState;
use swarm_core::ids::{KeyId, NodeId, Oid, RepoId};
use swarm_core::manifest::verify_envelope;
use swarm_net::addr::PeerAddr;
use swarm_net::handshake;
use swarm_net::{Message, NetError, Session};
use swarm_store::RepoStore;
use tokio::net::TcpStream;

use crate::net::NetState;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SESSIONS: usize = 4;
const PACK_CHUNK: usize = 256 * 1024;

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

struct Candidate {
    addr: PeerAddr,
    node: Option<NodeId>,
}

struct Connected {
    session: Session,
    tips: Vec<Oid>,
    failed: bool,
}

fn cached_candidates(net: &NetState, repo: RepoId) -> Vec<Candidate> {
    let Ok(db) = net.db.lock() else {
        return Vec::new();
    };
    let Ok(rows) = db.peers(repo) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|row| {
            let addr = PeerAddr::parse_socket(&row.addr).ok()?;
            Some(Candidate {
                addr,
                node: row.node_id.map(NodeId::from_bytes),
            })
        })
        .collect()
}

fn dedupe(candidates: Vec<Candidate>, own: NodeId) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        if candidate.node == Some(own) {
            continue;
        }
        if let Some(existing) = out
            .iter_mut()
            .find(|existing| existing.addr == candidate.addr)
        {
            // A cached entry may carry the trust-on-first-use pin that an
            // unpinned link omits (NET-11).
            if existing.node.is_none() {
                existing.node = candidate.node;
            }
            continue;
        }
        out.push(candidate);
    }
    out
}

async fn dial(net: &NetState, repo: RepoId, candidate: &Candidate) -> Result<Connected, NetError> {
    let socket: SocketAddr = candidate.addr.socket();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(socket))
        .await
        .map_err(|_| NetError::Timeout("connect timed out".into()))??;
    let transport = handshake::connect(
        stream,
        &net.node,
        std::slice::from_ref(&repo),
        candidate.node,
        candidate.addr.clone(),
        HANDSHAKE_TIMEOUT,
    )
    .await?;
    let node = transport.info.node_id;
    net.peer_ok(repo, &candidate.addr, node);
    Ok(Connected {
        session: Session::start(transport),
        tips: Vec::new(),
        failed: false,
    })
}

async fn fetch_heads(
    net: &NetState,
    repo: RepoId,
    net_state: &mut AcceptState,
    connected: &mut [Connected],
) -> (usize, usize) {
    let mut accepted = 0;
    let mut rejected = 0;
    for peer in connected.iter_mut() {
        let Some(repo_ix) = peer.session.info().repo_ix(repo) else {
            continue;
        };
        let reply = peer
            .session
            .handle()
            .request(Message::GetHeads { rid: 0, repo_ix }, REQUEST_TIMEOUT)
            .await;
        let Message::Heads { envelopes, .. } = (match reply {
            Ok(message) => message,
            Err(err) => {
                tracing::debug!(code = err.code(), error = %err, "GetHeads failed");
                peer.failed = true;
                continue;
            }
        }) else {
            continue;
        };
        for bytes in envelopes {
            let Ok(verified) = verify_envelope(&bytes) else {
                rejected += 1;
                continue;
            };
            let tips: Vec<Oid> = verified.manifest.refs.values().cloned().collect();
            if net_state.accept(verified).is_ok() {
                accepted += 1;
                for tip in tips {
                    if !peer.tips.iter().any(|known| known == &tip) {
                        peer.tips.push(tip);
                    }
                }
            } else {
                rejected += 1;
            }
        }
    }
    let _ = net;
    (accepted, rejected)
}

fn choose_session(connected: &[Connected], want: &Oid) -> Option<usize> {
    connected
        .iter()
        .position(|peer| !peer.failed && peer.tips.iter().any(|tip| tip == want))
        .or_else(|| connected.iter().position(|peer| !peer.failed))
}

async fn fetch_pack(
    peer: &mut Connected,
    repo_ix: u16,
    wants: &[Oid],
    haves: &[Oid],
    max_bytes: usize,
) -> Result<Vec<u8>, NetError> {
    let mut stream = peer
        .session
        .handle()
        .request_stream(
            Message::Want {
                rid: 0,
                repo_ix,
                wants: wants.to_vec(),
                haves: haves.to_vec(),
            },
            32,
        )
        .await?;
    let mut pack = Vec::new();
    loop {
        let message = tokio::time::timeout(REQUEST_TIMEOUT, stream.recv())
            .await
            .map_err(|_| NetError::Timeout("pack chunk timed out".into()))??;
        match message {
            Message::PackHeader { size_hint, .. } => {
                if let Some(size) = size_hint {
                    if size as usize > max_bytes {
                        return Err(NetError::LimitExceeded("pack is too large".into()));
                    }
                }
            }
            Message::PackData {
                chunk, final_chunk, ..
            } => {
                if chunk.len() > PACK_CHUNK {
                    return Err(NetError::LimitExceeded("pack chunk is too large".into()));
                }
                if pack.len() + chunk.len() > max_bytes {
                    return Err(NetError::LimitExceeded("pack is too large".into()));
                }
                pack.extend_from_slice(&chunk);
                if final_chunk {
                    break;
                }
            }
            Message::Error { code, msg, .. } => {
                return Err(NetError::Remote { code, msg });
            }
            _ => {
                return Err(NetError::Malformed("unexpected reply to Want".into()));
            }
        }
    }
    Ok(pack)
}

/// Sync one repo from link hints plus cached peers.
pub async fn sync_repo(
    net: &NetState,
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

    let mut candidates = swarm_disc::resolve_hints(&hints).await;
    let mut mapped: Vec<Candidate> = candidates
        .drain(..)
        .map(|candidate| Candidate {
            addr: candidate.addr,
            node: candidate.node,
        })
        .collect();
    mapped.extend(cached_candidates(net, repo));
    let candidates = dedupe(mapped, net.node_id);

    let mut connected: Vec<Connected> = Vec::new();
    let mut last_auth = false;
    let mut last_no_shared = false;
    for candidate in candidates {
        if connected.len() >= MAX_SESSIONS {
            break;
        }
        match dial(net, repo, &candidate).await {
            Ok(peer) => connected.push(peer),
            Err(err) => {
                net.peer_fail(repo, &candidate.addr);
                last_auth |= matches!(err, NetError::AuthFailed(_));
                last_no_shared |= matches!(err, NetError::NoSharedRepo);
                tracing::debug!(
                    peer = %candidate.addr,
                    code = err.code(),
                    error = %err,
                    "sync peer failed"
                );
            }
        }
    }

    if connected.is_empty() {
        let resolved = store.resolve(&net_state);
        if !resolved.canonical.is_empty() || !net_state.signers.is_empty() {
            let _ = store.apply_canonical(&resolved.canonical).await;
            return Ok(result(
                "stale",
                &store,
                &resolved,
                root_seq(&net_state, repo),
                &net_state,
                Counts {
                    accepted: 0,
                    rejected: 0,
                    peers: 0,
                },
            ));
        }
        return Err(if last_auth {
            NetError::AuthFailed("every pinned peer failed authentication".into())
        } else if last_no_shared {
            NetError::NoSharedRepo
        } else {
            NetError::NoPeers
        });
    }

    let (accepted, rejected) = fetch_heads(net, repo, &mut net_state, &mut connected).await;
    if net_state.signers.is_empty() {
        return Err(NetError::NoPeers);
    }

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
    store
        .persist_state(&net_state)
        .map_err(|err| NetError::Malformed(err.to_string()))?;
    persist_db(net, repo, &object_format, &net_state, row.is_none())?;

    let mut resolved = store.resolve(&net_state);
    let mut missing: Vec<Oid> = resolved
        .canonical
        .values()
        .filter(|oid| !object_present(&store, oid))
        .cloned()
        .collect();
    missing.sort_by(|a, b| a.as_slice().cmp(b.as_slice()));
    missing.dedup_by(|a, b| a.as_slice() == b.as_slice());

    while !missing.is_empty() {
        let want = missing[0].clone();
        let Some(index) = choose_session(&connected, &want) else {
            return Err(NetError::PackInvalid("no peer served the objects".into()));
        };
        let repo_ix = connected[index]
            .session
            .info()
            .repo_ix(repo)
            .ok_or_else(|| NetError::Malformed("session lost the repo".into()))?;
        let haves = existing_haves(&store, &resolved).await;
        match fetch_pack(
            &mut connected[index],
            repo_ix,
            &missing.clone(),
            &haves,
            net.max_pack_bytes,
        )
        .await
        {
            Ok(pack) => match store
                .git()
                .index_pack_limited(&pack, net.max_pack_bytes)
                .await
            {
                Ok(_) => {
                    connected[index].failed = false;
                    resolved = store.resolve(&net_state);
                    missing = resolved
                        .canonical
                        .values()
                        .filter(|oid| !object_present(&store, oid))
                        .cloned()
                        .collect();
                }
                Err(err) => {
                    connected[index].failed = true;
                    tracing::debug!(error = %err, "pack rejected");
                    if connected.iter().all(|peer| peer.failed) {
                        return Err(NetError::PackInvalid(
                            "every peer failed to serve the objects".into(),
                        ));
                    }
                }
            },
            Err(err) => {
                connected[index].failed = true;
                if connected.iter().all(|peer| peer.failed) {
                    return Err(err);
                }
            }
        }
    }

    resolved = store.resolve(&net_state);
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
    Ok(result(
        "fresh",
        &store,
        &resolved,
        root_seq(&net_state, repo),
        &net_state,
        Counts {
            accepted,
            rejected,
            peers: connected.len(),
        },
    ))
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
