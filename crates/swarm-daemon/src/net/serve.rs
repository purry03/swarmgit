//! Server side of the wire protocol (spec 7.3, WANT-1 to WANT-5).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use swarm_core::accept::AcceptState;
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_core::manifest::verify_envelope;
use swarm_net::handshake;
use swarm_net::{Message, PeerAddr, Session, SessionHandle};
use swarm_store::StoreError;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use super::NetState;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TOTAL_CONNECTIONS: usize = 64;
const MAX_PER_IP: usize = 4;
const PACK_CHUNK: usize = 256 * 1024;

struct Limits {
    total: AtomicUsize,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
}

struct Guard {
    limits: Arc<Limits>,
    ip: IpAddr,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.limits.total.fetch_sub(1, Ordering::SeqCst);
        if let Ok(mut map) = self.limits.per_ip.lock() {
            if let Some(count) = map.get_mut(&self.ip) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(&self.ip);
                }
            }
        }
    }
}

impl Limits {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            total: AtomicUsize::new(0),
            per_ip: Mutex::new(HashMap::new()),
        })
    }

    fn enter(self: &Arc<Self>, ip: IpAddr) -> Option<Guard> {
        if self.total.fetch_add(1, Ordering::SeqCst) >= MAX_TOTAL_CONNECTIONS {
            self.total.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let mut map = self.per_ip.lock().ok()?;
        let count = map.entry(ip).or_insert(0);
        if *count >= MAX_PER_IP {
            *count = count.saturating_sub(1);
            drop(map);
            self.total.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        *count += 1;
        drop(map);
        Some(Guard {
            limits: Arc::clone(self),
            ip,
        })
    }
}

/// Accept peer connections until shutdown.
pub async fn serve(net: Arc<NetState>, listener: TcpListener, shutdown: Arc<Notify>) {
    let limits = Limits::new();
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let Some(guard) = limits.enter(peer.ip()) else {
                    drop(stream);
                    continue;
                };
                let net = Arc::clone(&net);
                tokio::spawn(async move {
                    let _guard = guard;
                    handle_connection(net, stream, peer).await;
                });
            }
        }
    }
    tracing::info!("peer listener stopped");
}

async fn handle_connection(net: Arc<NetState>, stream: TcpStream, peer: SocketAddr) {
    let repos = net.serving_repos();
    let transport = match handshake::accept(
        stream,
        &net.node,
        &repos,
        PeerAddr::Tcp(peer),
        HANDSHAKE_TIMEOUT,
    )
    .await
    {
        Ok(transport) => transport,
        Err(err) => {
            tracing::debug!(peer = %peer, code = err.code(), error = %err, "inbound handshake rejected");
            return;
        }
    };
    let node = transport.info.node_id;
    tracing::debug!(peer = %peer, node = %node.to_hex(), "inbound session ready");

    run_session(net, Session::start(transport)).await;
}

/// Register a session and serve its incoming messages until it closes.
/// Inbound and outbound sessions both run here, so a session dialed for a
/// sync stays open for `Announce` and later syncs (7.4).
pub async fn run_session(net: Arc<NetState>, mut session: Session) {
    let id = net.register(session.handle());
    let want_busy = Arc::new(AtomicBool::new(false));
    while let Some(message) = session.recv_incoming().await {
        let net = Arc::clone(&net);
        let handle = session.handle();
        let busy = Arc::clone(&want_busy);
        tokio::spawn(async move {
            serve_message(net, handle, message, busy).await;
        });
    }
    session.handle().close();
    net.unregister(id);
}

/// Spawn [`run_session`] and return a handle for requests.
pub fn spawn_session(net: &Arc<NetState>, session: Session) -> SessionHandle {
    let handle = session.handle();
    tokio::spawn(run_session(Arc::clone(net), session));
    handle
}

fn repo_for(handle: &SessionHandle, repo_ix: u16) -> Option<RepoId> {
    handle
        .info()
        .shared
        .iter()
        .find(|(_, ix)| *ix == repo_ix)
        .map(|(repo, _)| *repo)
}

fn error(handle: &SessionHandle, rid: u64, code: &str, msg: impl Into<String>) {
    let message = Message::Error {
        rid,
        code: code.to_string(),
        msg: msg.into(),
    };
    let handle = handle.clone();
    tokio::spawn(async move {
        let _ = handle.send(&message).await;
    });
}

fn reply(handle: &SessionHandle, message: Message) {
    let handle = handle.clone();
    tokio::spawn(async move {
        let _ = handle.send(&message).await;
    });
}

fn store_error(handle: &SessionHandle, rid: u64, err: StoreError) {
    let code = match &err {
        StoreError::Core(code) => code.to_string(),
        _ => "malformed".to_string(),
    };
    error(handle, rid, &code, err.to_string());
}

async fn serve_message(
    net: Arc<NetState>,
    handle: SessionHandle,
    message: Message,
    busy: Arc<AtomicBool>,
) {
    match message {
        Message::GetHeads { rid, repo_ix } => serve_heads(&net, &handle, rid, repo_ix),
        Message::GetManifest {
            rid,
            repo_ix,
            signer,
            min_seq,
        } => serve_manifest(&net, &handle, rid, repo_ix, signer, min_seq),
        Message::Want {
            rid,
            repo_ix,
            wants,
            haves,
        } => serve_want(&net, &handle, rid, repo_ix, wants, haves, busy).await,
        Message::Announce { repo_ix, envelope } => {
            if let Some(repo) = repo_for(&handle, repo_ix) {
                match announce_is_fresh(&net, repo, &envelope) {
                    Ok(true) => {
                        // Fetch the manifest and its objects over the open
                        // sessions, then forward the announce once (7.4): a
                        // repeat is no longer fresh, so it stops here. A
                        // sync that fails forwards nothing we cannot serve.
                        match crate::sync::sync_repo(&net, repo, Vec::new()).await {
                            Ok(_) => net.announce(repo, envelope).await,
                            Err(err) => {
                                tracing::debug!(error = %err, "sync after announce failed");
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(err) => {
                        tracing::debug!(error = %err, "announce rejected");
                    }
                }
            }
        }
        Message::GetPeers { rid, repo_ix } => serve_peers(&net, &handle, rid, repo_ix),
        Message::Error { code, msg, .. } => {
            tracing::debug!(code = %code, msg = %msg, "peer reported an error");
        }
        other => {
            error(
                &handle,
                0,
                "malformed",
                format!("unsupported message type {}", other.type_number()),
            );
        }
    }
}

fn serve_heads(net: &NetState, handle: &SessionHandle, rid: u64, repo_ix: u16) {
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    let store = net.store(repo);
    match store.load_state() {
        Ok(state) => {
            let envelopes: Vec<Vec<u8>> = state
                .signers
                .values()
                .map(|stored| stored.envelope.clone())
                .take(65)
                .collect();
            reply(handle, Message::Heads { rid, envelopes });
        }
        Err(err) => store_error(handle, rid, err),
    }
}

fn serve_manifest(
    net: &NetState,
    handle: &SessionHandle,
    rid: u64,
    repo_ix: u16,
    signer: [u8; 32],
    min_seq: u64,
) {
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    let store = net.store(repo);
    let Ok(state) = store.load_state() else {
        error(handle, rid, "malformed", "cannot read manifests");
        return;
    };
    let signer = KeyId::from_bytes(signer);
    let envelope = state
        .signers
        .get(&signer)
        .filter(|stored| stored.manifest.seq >= min_seq)
        .map(|stored| stored.envelope.clone());
    reply(handle, Message::Manifest { rid, envelope });
}

struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn serve_want(
    net: &NetState,
    handle: &SessionHandle,
    rid: u64,
    repo_ix: u16,
    wants: Vec<Oid>,
    haves: Vec<Oid>,
    busy: Arc<AtomicBool>,
) {
    if busy.swap(true, Ordering::SeqCst) {
        error(handle, rid, "busy", "a Want is already in flight");
        return;
    }
    let _busy = BusyGuard(busy);
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    let store = net.store(repo);
    let Ok(state) = store.load_state() else {
        error(handle, rid, "malformed", "cannot read manifests");
        return;
    };
    let tips = tips_of(&state);
    if wants.iter().any(|want| !tips.iter().any(|tip| tip == want)) {
        error(handle, rid, "unauthorized", "Want is not a verified tip");
        return;
    }
    let haves = match store.git().filter_existing(&haves).await {
        Ok(haves) => haves,
        Err(err) => {
            store_error(handle, rid, err);
            return;
        }
    };
    let pack = match store
        .git()
        .pack_objects_limited(&wants, &haves, net.max_pack_bytes)
        .await
    {
        Ok(pack) => pack,
        Err(err) => {
            store_error(handle, rid, err);
            return;
        }
    };

    let _permit = match net.uploads.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            error(handle, rid, "busy", "upload slots are closed");
            return;
        }
    };
    if handle
        .send(&Message::PackHeader {
            rid,
            size_hint: Some(pack.len() as u64),
        })
        .await
        .is_err()
    {
        return;
    }
    if pack.is_empty() {
        let _ = handle
            .send(&Message::PackData {
                rid,
                chunk: Vec::new(),
                final_chunk: true,
            })
            .await;
        return;
    }
    let mut offset = 0;
    while offset < pack.len() {
        let end = (offset + PACK_CHUNK).min(pack.len());
        let message = Message::PackData {
            rid,
            chunk: pack[offset..end].to_vec(),
            final_chunk: end == pack.len(),
        };
        if handle.send(&message).await.is_err() {
            return;
        }
        offset = end;
    }
}

fn tips_of(state: &AcceptState) -> Vec<Oid> {
    let mut tips = Vec::new();
    for stored in state.signers.values() {
        for oid in stored.manifest.refs.values() {
            if !tips.iter().any(|item| item == oid) {
                tips.push(oid.clone());
            }
        }
    }
    tips
}

fn serve_peers(net: &NetState, handle: &SessionHandle, rid: u64, repo_ix: u16) {
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    let rows = {
        let Ok(db) = net.db.lock() else {
            error(handle, rid, "malformed", "database lock poisoned");
            return;
        };
        db.peers(repo).unwrap_or_default()
    };
    let mut addresses = Vec::new();
    for row in rows {
        if let Ok(addr) = PeerAddr::parse_socket(&row.addr) {
            addresses.push(addr.encode(row.node_id.map(swarm_core::ids::NodeId::from_bytes)));
        }
    }
    reply(handle, Message::Peers { rid, addresses });
}

/// Whether an `Announce` carries a manifest newer than the stored one.
/// Nothing is stored here: the sync that follows fetches the objects first
/// and stores the manifest after them.
pub fn announce_is_fresh(
    net: &NetState,
    repo: RepoId,
    envelope: &[u8],
) -> Result<bool, StoreError> {
    let verified = verify_envelope(envelope).map_err(StoreError::Core)?;
    let mut state = net.store(repo).load_state()?;
    Ok(matches!(
        state.accept(verified)?,
        swarm_core::accept::AcceptOutcome::Accepted { .. }
    ))
}

/// Convenience for tests: a request timeout constant.
#[must_use]
pub fn request_timeout() -> Duration {
    REQUEST_TIMEOUT
}
