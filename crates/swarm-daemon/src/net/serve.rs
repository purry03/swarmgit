//! Server side of the wire protocol (spec 7.3, WANT-1 to WANT-5).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use swarm_core::accept::AcceptState;
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_core::manifest::verify_envelope;
use swarm_net::handshake;
use swarm_net::message::MrTip;
use swarm_net::{BoxConn, Message, PeerAddr, Session, SessionHandle};
use swarm_store::StoreError;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use super::NetState;
use crate::sync::mrs;
use crate::sync::progress::Progress;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const PACK_CHUNK: usize = 256 * 1024;

/// Accept peer connections until shutdown.
pub async fn serve(net: Arc<NetState>, listener: TcpListener, shutdown: Arc<Notify>) {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let guard = match net.limits.enter(peer.ip()) {
                    Ok(guard) => guard,
                    Err(why) => {
                        tracing::debug!(peer = %peer, ?why, "inbound connection refused");
                        drop(stream);
                        continue;
                    }
                };
                let net = Arc::clone(&net);
                tokio::spawn(async move {
                    let _guard = guard;
                    let limits = Arc::clone(&net.limits);
                    if handle_connection(net, stream, peer).await {
                        limits.violation(peer.ip());
                    }
                });
            }
        }
    }
    tracing::info!("peer listener stopped");
}

/// Accept the connections Tor forwards from our hidden service. They all come
/// from this machine, so they are limited together and never banned.
pub async fn serve_onion(net: Arc<NetState>, listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            // Out of file descriptors, most likely: do not spin.
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let guard = match net.limits.enter_onion() {
            Ok(guard) => guard,
            Err(why) => {
                tracing::debug!(?why, "onion connection refused");
                continue;
            }
        };
        let net = Arc::clone(&net);
        tokio::spawn(async move {
            let _guard = guard;
            let unknown = PeerAddr::Tcp(SocketAddr::from(([0, 0, 0, 0], 0)));
            let _violated = handle_stream(net, stream, unknown, "onion").await;
        });
    }
}

async fn handle_connection(net: Arc<NetState>, stream: TcpStream, peer: SocketAddr) -> bool {
    let label = peer.to_string();
    let Some(transport) = accept(&net, stream, PeerAddr::Tcp(peer), &label).await else {
        return false;
    };
    let Ok(transport) = transport else {
        return true;
    };
    // A stranger from the internet got through: no relay is needed (REL-13).
    if swarm_relay::directory::usable(SocketAddr::new(peer.ip(), 1), false) {
        net.mark_reachable();
    }
    run_session(net, Session::start(transport)).await
}

/// Run the responder handshake on any stream and serve the session. Used for
/// direct TCP connections and for streams a relay or a UDP path hands over.
/// Returns true when the peer broke the protocol (7.4).
pub async fn handle_stream<S>(net: Arc<NetState>, stream: S, addr: PeerAddr, label: &str) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match accept(&net, stream, addr, label).await {
        Some(Ok(transport)) => run_session(net, Session::start(transport)).await,
        Some(Err(())) => true,
        None => false,
    }
}

/// The responder handshake. `None` when it failed without blame, `Err` when
/// the peer broke the protocol (7.4).
pub async fn accept<S>(
    net: &Arc<NetState>,
    stream: S,
    addr: PeerAddr,
    label: &str,
) -> Option<Result<swarm_net::Established, ()>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let repos = net.serving_repos();
    let stream: BoxConn = net.transport.wrap(Box::new(stream));
    let options = net.handshake_options(HANDSHAKE_TIMEOUT);
    let transport = match handshake::accept(stream, &net.node, &repos, addr, options).await {
        Ok(transport) => transport,
        Err(err) => {
            tracing::debug!(peer = %label, code = err.code(), error = %err, "inbound handshake rejected");
            return err.is_violation().then_some(Err(()));
        }
    };
    let node = transport.info.node_id;
    tracing::debug!(peer = %label, node = %node.to_hex(), "inbound session ready");
    Some(Ok(transport))
}

/// Register a session and serve its incoming messages until it closes.
/// Inbound and outbound sessions both run here, so a session dialed for a
/// sync stays open for `Announce` and later syncs (7.4). Returns true when it
/// ended because the peer broke the protocol.
pub async fn run_session(net: Arc<NetState>, mut session: Session) -> bool {
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
    let violated = session.handle().violated();
    session.handle().close();
    net.unregister(id);
    violated
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
                serve_announce(&net, repo, envelope).await;
            }
        }
        Message::GetAbout { rid, repo_ix } => serve_about(&net, &handle, rid, repo_ix),
        Message::AnnounceAbout { repo_ix, record } => {
            if let Some(repo) = repo_for(&handle, repo_ix) {
                serve_announce_about(&net, repo, record).await;
            }
        }
        Message::GetMrs { rid, repo_ix } => serve_mrs(&net, &handle, rid, repo_ix).await,
        Message::AnnounceMrs { repo_ix, tips } => {
            if let Some(repo) = repo_for(&handle, repo_ix) {
                serve_announce_mrs(&net, repo, &handle, tips).await;
            }
        }
        Message::GetPeers { rid, repo_ix } => serve_peers(&net, &handle, rid, repo_ix),
        Message::PunchOffer {
            rid,
            token,
            addresses,
        } => {
            if !super::punch::answer(net, handle.clone(), rid, token, addresses).await {
                error(&handle, rid, "busy", "udp punching is not available");
            }
        }
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

/// The announced envelopes being handled right now. Copies arrive from many
/// peers at once; one of them is enough.
#[derive(Default)]
pub struct Announcing(Arc<Mutex<HashSet<Vec<u8>>>>);

/// Held while one envelope is handled.
struct Claim(Arc<Mutex<HashSet<Vec<u8>>>>, Vec<u8>);

impl Announcing {
    fn claim(&self, envelope: &[u8]) -> Option<Claim> {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        set.insert(envelope.to_vec())
            .then(|| Claim(Arc::clone(&self.0), envelope.to_vec()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        set.remove(&self.1);
    }
}

/// Fetch an announced manifest and its objects over the open sessions, then
/// forward the announce once (7.4). Only a manifest that is now stored goes
/// on: forwarding one we could not fetch, or forwarding every copy of one,
/// makes peers announce it back and forth without end.
async fn serve_announce(net: &Arc<NetState>, repo: RepoId, envelope: Vec<u8>) {
    // Claimed before the freshness check, so that a copy cannot pass the
    // check and then run after the first one has finished.
    let Some(_claim) = net.announcing.claim(&envelope) else {
        return;
    };
    match announce_is_fresh(net, repo, &envelope) {
        Ok(true) => {}
        Ok(false) => return,
        Err(err) => {
            tracing::debug!(error = %err, "announce rejected");
            return;
        }
    }
    if let Err(err) = crate::sync::sync_repo(net, repo, Vec::new(), &Progress::none()).await {
        tracing::debug!(error = %err, "sync after announce failed");
    } else if !matches!(announce_is_fresh(net, repo, &envelope), Ok(true)) {
        net.announce(repo, envelope).await;
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

fn serve_about(net: &NetState, handle: &SessionHandle, rid: u64, repo_ix: u16) {
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    let store = net.store(repo);
    let record = store.load_state().ok().and_then(|state| {
        store
            .load_about(&state, super::now() as u64)
            .map(|about| about.bytes)
    });
    reply(handle, Message::About { rid, record });
}

/// Store an announced name and description and pass it on. Only a record that
/// replaced ours is forwarded, so copies stop at the peers that have it.
async fn serve_announce_about(net: &Arc<NetState>, repo: RepoId, record: Vec<u8>) {
    let Ok(about) = swarm_core::verify_about(&record) else {
        return;
    };
    let store = net.store(repo);
    if !store.exists() {
        return;
    }
    let written = store
        .load_state()
        .and_then(|state| store.write_about(&about, &state, super::now() as u64));
    match written {
        Ok(true) => net.announce_about(repo, record).await,
        Ok(false) => {}
        Err(err) => tracing::debug!(error = %err, "announced name rejected"),
    }
}

async fn serve_mrs(net: &NetState, handle: &SessionHandle, rid: u64, repo_ix: u16) {
    let Some(repo) = repo_for(handle, repo_ix) else {
        error(handle, rid, "unknown_repo", "unknown repo index");
        return;
    };
    match net.store(repo).mr_tips().await {
        Ok(tips) => reply(
            handle,
            Message::Mrs {
                rid,
                tips: mrs::wire_tips(&tips),
            },
        ),
        Err(err) => store_error(handle, rid, err),
    }
}

/// Take the announced merge request events from the sender and pass on the
/// ones that were new here, so copies stop at the peers that have them.
async fn serve_announce_mrs(
    net: &Arc<NetState>,
    repo: RepoId,
    handle: &SessionHandle,
    tips: Vec<MrTip>,
) {
    if !net.store(repo).exists() {
        return;
    }
    let accepted = {
        let lock = net.sync_lock(repo);
        let _guard = lock.lock().await;
        mrs::ingest(net, repo, handle, &tips).await
    };
    match accepted {
        Ok(accepted) => mrs::announce(net, repo, &accepted).await,
        Err(err) => tracing::debug!(error = %err, "announced merge request not taken"),
    }
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
    let mut tips = tips_of(&state);
    if let Ok(mr) = store.mr_tips().await {
        tips.extend(mr.into_values());
    }
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
    let step = net.throttle.chunk(PACK_CHUNK);
    while offset < pack.len() {
        let end = (offset + step).min(pack.len());
        net.throttle.take(end - offset).await;
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
    // REL-9: a node registered with a relay offers its own relay address,
    // first so the cap below never drops it.
    let mut addresses: Vec<Vec<u8>> = net
        .relay_peer_addrs()
        .iter()
        .map(|addr| addr.encode(None))
        .filter(|bytes| !bytes.is_empty())
        .collect();
    for row in rows {
        if let Ok(addr) = PeerAddr::parse_socket(&row.addr) {
            // Names (onion services) have no wire form.
            let bytes = addr.encode(row.node_id.map(swarm_core::ids::NodeId::from_bytes));
            if !bytes.is_empty() {
                addresses.push(bytes);
            }
        }
    }
    addresses.truncate(32);
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

#[cfg(test)]
mod announce_tests {
    use super::Announcing;

    #[test]
    fn one_copy_of_an_announce_is_handled_at_a_time() {
        let announcing = Announcing::default();
        let first = announcing.claim(b"one");
        assert!(first.is_some());
        assert!(announcing.claim(b"one").is_none());
        assert!(announcing.claim(b"two").is_some());
        drop(first);
        assert!(announcing.claim(b"one").is_some());
    }
}
