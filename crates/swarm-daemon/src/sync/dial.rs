//! Candidates, dialing and peer exchange for a sync (DM-1 to DM-5, 6.5).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::{NodeId, Oid, RepoId};
use swarm_net::addr::PeerAddr;
use swarm_net::handshake;
use swarm_net::{Message, NetError, Session, SessionHandle};
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use crate::net::NetState;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PEX_TIMEOUT: Duration = Duration::from_secs(5);
/// DM-3: stop dialing once this many sessions carry the repo.
pub const MAX_SESSIONS: usize = 4;
/// DM-2: dials in flight at once.
const MAX_DIALS: usize = 8;
/// 6.5: addresses taken from one `Peers` reply.
const MAX_PEX: usize = 32;

/// An address to try, with the node ID pin when one is known.
pub struct Candidate {
    pub addr: PeerAddr,
    pub node: Option<NodeId>,
}

/// A session that carries the repo.
pub struct Connected {
    pub handle: SessionHandle,
    pub tips: Vec<Oid>,
    pub failed: bool,
}

impl Connected {
    fn new(handle: SessionHandle) -> Self {
        Self {
            handle,
            tips: Vec::new(),
            failed: false,
        }
    }

    pub fn node(&self) -> NodeId {
        self.handle.info().node_id
    }
}

/// Why dials failed, for the error when nothing connected.
#[derive(Default)]
pub struct Failures {
    pub auth: bool,
    pub no_shared: bool,
}

/// Dial bookkeeping for one sync.
pub struct Dialer {
    pub connected: Vec<Connected>,
    pub failures: Failures,
    tried: Vec<PeerAddr>,
}

impl Dialer {
    /// Start from the sessions that are already open for `repo` (7.4).
    pub fn new(net: &NetState, repo: RepoId) -> Self {
        let mut connected: Vec<Connected> = Vec::new();
        for handle in net.sessions_for(repo) {
            let node = handle.info().node_id;
            if connected.iter().any(|peer| peer.node() == node) {
                continue;
            }
            connected.push(Connected::new(handle));
        }
        Self {
            connected,
            failures: Failures::default(),
            tried: Vec::new(),
        }
    }

    fn has_node(&self, node: Option<NodeId>) -> bool {
        node.is_some_and(|node| self.connected.iter().any(|peer| peer.node() == node))
    }

    /// Dial candidates, at most [`MAX_DIALS`] at a time, until `limit`
    /// sessions are open. Returns the indexes of new sessions.
    pub async fn dial_all(
        &mut self,
        net: &Arc<NetState>,
        repo: RepoId,
        candidates: Vec<Candidate>,
        limit: usize,
    ) -> Vec<usize> {
        let mut queue: Vec<Candidate> = candidates
            .into_iter()
            .filter(|candidate| !self.tried.contains(&candidate.addr))
            .filter(|candidate| !self.has_node(candidate.node))
            .collect();
        queue.reverse();
        let mut added = Vec::new();
        let mut set: JoinSet<(PeerAddr, Result<SessionHandle, NetError>)> = JoinSet::new();
        loop {
            while set.len() < MAX_DIALS && self.connected.len() < limit {
                let Some(candidate) = queue.pop() else { break };
                self.tried.push(candidate.addr.clone());
                let net = Arc::clone(net);
                set.spawn(async move {
                    let result = dial(&net, repo, &candidate).await;
                    (candidate.addr, result)
                });
            }
            let Some(joined) = set.join_next().await else {
                break;
            };
            let Ok((addr, result)) = joined else { continue };
            match result {
                Ok(handle) => {
                    let node = handle.info().node_id;
                    if self.has_node(Some(node)) {
                        continue;
                    }
                    added.push(self.connected.len());
                    self.connected.push(Connected::new(handle));
                }
                Err(err) => {
                    net.peer_fail(repo, &addr);
                    self.failures.auth |= matches!(err, NetError::AuthFailed(_));
                    self.failures.no_shared |= matches!(err, NetError::NoSharedRepo);
                    tracing::debug!(peer = %addr, code = err.code(), error = %err, "sync peer failed");
                }
            }
            if self.connected.len() >= limit {
                // Sessions that still finish stay registered for later use.
                set.detach_all();
                break;
            }
        }
        added
    }
}

async fn dial(
    net: &Arc<NetState>,
    repo: RepoId,
    candidate: &Candidate,
) -> Result<SessionHandle, NetError> {
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
    Ok(crate::net::serve::spawn_session(
        net,
        Session::start(transport),
    ))
}

/// Cached peers for the repo, best first.
pub fn cached_candidates(net: &NetState, repo: RepoId) -> Vec<Candidate> {
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

/// Drop our own node and merge duplicate addresses (DM-1).
pub fn dedupe(candidates: Vec<Candidate>, own: NodeId) -> Vec<Candidate> {
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

/// Unpinned candidates from plain socket addresses (LAN, DHT).
pub fn unpinned(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<Candidate> {
    addrs
        .into_iter()
        .map(|addr| Candidate {
            addr: PeerAddr::Tcp(addr),
            node: None,
        })
        .collect()
}

/// How long a sync waits for LAN answers when nothing else connected.
pub fn lan_wait(has_state: bool) -> Duration {
    if has_state {
        return Duration::from_millis(3500);
    }
    std::env::var("SWARM_FIND_TIMEOUT")
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(Duration::from_secs(30), Duration::from_secs)
}

/// Ask every session for its peers (6.5) and remember up to 32 from each.
/// An address that comes with a node ID is stored pinned.
pub async fn exchange_peers(
    net: &NetState,
    repo: RepoId,
    connected: &[Connected],
) -> Vec<Candidate> {
    let mut found = Vec::new();
    for peer in connected.iter().filter(|peer| !peer.failed) {
        let Some(repo_ix) = peer.handle.info().repo_ix(repo) else {
            continue;
        };
        let reply = peer
            .handle
            .request(Message::GetPeers { rid: 0, repo_ix }, PEX_TIMEOUT)
            .await;
        let Ok(Message::Peers { addresses, .. }) = reply else {
            continue;
        };
        for bytes in addresses.iter().take(MAX_PEX) {
            let Ok((addr, node)) = PeerAddr::decode(bytes) else {
                continue;
            };
            if node == Some(net.node_id) || addr.socket().ip().is_unspecified() {
                continue;
            }
            if let Ok(db) = net.db.lock() {
                let _ = db.record_peer_hint(
                    repo,
                    &addr.to_string(),
                    node.as_ref().map(NodeId::as_bytes),
                );
            }
            found.push(Candidate { addr, node });
        }
    }
    found
}
