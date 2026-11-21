//! Manifests and packs from connected sessions (8.2), with wants split
//! across sessions and resumed on another session after a failure.

use std::time::Duration;

use swarm_core::accept::AcceptState;
use swarm_core::ids::{Oid, RepoId};
use swarm_core::manifest::verify_envelope;
use swarm_net::{Message, NetError, SessionHandle};
use swarm_store::RepoStore;
use tokio::task::JoinSet;

use super::dial::Connected;
use super::progress::{Progress, Transfer};
use std::sync::Arc;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PACK_CHUNK: usize = 256 * 1024;

/// Ask `peers` for their heads and accept what verifies (SYNC-3 keeps the
/// highest seq per signer). Returns (accepted, rejected).
pub async fn fetch_heads(
    repo: RepoId,
    state: &mut AcceptState,
    peers: &mut [Connected],
) -> (usize, usize) {
    let mut rejected = 0;
    // Collect first, accept after: a root rotation and the new root's own
    // manifest may come from different peers, in any order (5.4 step 2).
    let mut verified_all = Vec::new();
    let mut owners = Vec::new();
    for (peer_ix, peer) in peers.iter_mut().enumerate() {
        let Some(repo_ix) = peer.handle.info().repo_ix(repo) else {
            continue;
        };
        let reply = peer
            .handle
            .request(Message::GetHeads { rid: 0, repo_ix }, REQUEST_TIMEOUT)
            .await;
        let envelopes = match reply {
            Ok(Message::Heads { envelopes, .. }) => envelopes,
            Ok(_) => {
                tracing::debug!(node = %peer.node().to_hex(), "unexpected reply to GetHeads");
                peer.note = Some("sent an unexpected reply to the state request".into());
                continue;
            }
            Err(err) => {
                tracing::debug!(node = %peer.node().to_hex(), code = err.code(), error = %err, "GetHeads failed");
                peer.note = Some(format!("state request failed: {err}"));
                peer.failed = true;
                continue;
            }
        };
        if envelopes.is_empty() {
            tracing::debug!(node = %peer.node().to_hex(), "peer has no manifest for this repo");
            peer.note = Some("has no manifest for this repo".into());
        }
        for bytes in envelopes {
            match verify_envelope(&bytes) {
                Ok(verified) => {
                    verified_all.push(verified);
                    owners.push(peer_ix);
                }
                Err(_) => rejected += 1,
            }
        }
    }
    let tips: Vec<Vec<Oid>> = verified_all
        .iter()
        .map(|verified| verified.manifest.refs.values().cloned().collect())
        .collect();
    let mut accepted = 0;
    for (index, outcome) in state.accept_batch_indexed(verified_all) {
        if outcome.is_err() {
            rejected += 1;
            continue;
        }
        accepted += 1;
        let peer = &mut peers[owners[index]];
        for tip in &tips[index] {
            if !peer.tips.iter().any(|known| known == tip) {
                peer.tips.push(tip.clone());
            }
        }
    }
    (accepted, rejected)
}

async fn fetch_pack(
    handle: SessionHandle,
    repo_ix: u16,
    wants: Vec<Oid>,
    haves: Vec<Oid>,
    max_bytes: usize,
    progress: Progress,
    transfer: Arc<Transfer>,
) -> Result<Vec<u8>, NetError> {
    let mut stream = handle
        .request_stream(
            Message::Want {
                rid: 0,
                repo_ix,
                wants,
                haves,
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
                if size_hint.is_some_and(|size| size as usize > max_bytes) {
                    return Err(NetError::LimitExceeded("pack is too large".into()));
                }
                if let Some(size) = size_hint {
                    transfer.expect(size);
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
                let (done, total) = transfer.add(chunk.len() as u64);
                progress.bytes(done, total);
                if final_chunk {
                    break;
                }
            }
            Message::Error { code, msg, .. } => return Err(NetError::Remote { code, msg }),
            _ => return Err(NetError::Malformed("unexpected reply to Want".into())),
        }
    }
    Ok(pack)
}

/// Spread `missing` over the sessions: each want goes to a session that
/// advertised it, round robin, else to any session still working.
fn assign(peers: &[Connected], missing: &[Oid]) -> Vec<(usize, Vec<Oid>)> {
    let live: Vec<usize> = (0..peers.len()).filter(|&ix| !peers[ix].failed).collect();
    let mut plan: Vec<(usize, Vec<Oid>)> = Vec::new();
    for (turn, want) in missing.iter().enumerate() {
        let advertising: Vec<usize> = live
            .iter()
            .copied()
            .filter(|&ix| peers[ix].tips.iter().any(|tip| tip == want))
            .collect();
        let pool = if advertising.is_empty() {
            &live
        } else {
            &advertising
        };
        let Some(&ix) = pool.get(turn % pool.len().max(1)) else {
            continue;
        };
        match plan.iter_mut().find(|(peer, _)| *peer == ix) {
            Some((_, wants)) => wants.push(want.clone()),
            None => plan.push((ix, vec![want.clone()])),
        }
    }
    plan
}

/// Fetch every object in `missing`, one `Want` per session at a time and
/// in parallel. A session whose pack fails, or does not bring what it was
/// asked for, is marked failed and its wants move to the others. Packs
/// already indexed stay, so the next round resumes where this one stopped.
pub async fn fetch_missing(
    store: &RepoStore,
    repo: RepoId,
    peers: &mut [Connected],
    missing_now: impl Fn() -> Vec<Oid>,
    haves: Vec<Oid>,
    max_bytes: usize,
    progress: &Progress,
) -> Result<(), NetError> {
    let transfer = Transfer::new();
    let mut last_error: Option<NetError> = None;
    loop {
        let missing = missing_now();
        if missing.is_empty() {
            return Ok(());
        }
        let plan = assign(peers, &missing);
        if plan.is_empty() {
            return Err(last_error
                .unwrap_or_else(|| NetError::PackInvalid("no peer served the objects".into())));
        }
        let mut set = JoinSet::new();
        for (ix, wants) in &plan {
            let Some(repo_ix) = peers[*ix].handle.info().repo_ix(repo) else {
                peers[*ix].failed = true;
                continue;
            };
            let handle = peers[*ix].handle.clone();
            let (ix, wants, haves) = (*ix, wants.clone(), haves.clone());
            let (progress, transfer) = (progress.clone(), Arc::clone(&transfer));
            set.spawn(async move {
                (
                    ix,
                    fetch_pack(handle, repo_ix, wants, haves, max_bytes, progress, transfer).await,
                )
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok((ix, result)) = joined else { continue };
            let indexed = match result {
                Ok(pack) => {
                    progress.say("index", "checking and indexing the objects");
                    store
                        .git()
                        .index_pack_limited(&pack, max_bytes)
                        .await
                        .map(|_| ())
                        .map_err(|err| NetError::PackInvalid(err.to_string()))
                }
                Err(err) => Err(err),
            };
            if let Err(err) = indexed {
                tracing::debug!(code = err.code(), error = %err, "pack from peer failed");
                peers[ix].failed = true;
                last_error = Some(err);
            }
        }
        // A session that answered but left its wants missing gave a bad
        // pack: stop asking it, so every round makes progress.
        let still = missing_now();
        for (ix, wants) in plan {
            if wants.iter().any(|want| still.contains(want)) {
                peers[ix].failed = true;
            }
        }
    }
}
