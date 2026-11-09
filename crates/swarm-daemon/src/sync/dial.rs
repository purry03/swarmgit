//! Candidates, dialing and peer exchange for a sync (DM-1 to DM-5, 6.5).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::{NodeId, Oid, RepoId};
use swarm_net::addr::PeerAddr;
use swarm_net::handshake;
use swarm_net::{Message, NetError, Session, SessionHandle};
use swarm_relay::RelayError;
use tokio::net::TcpStream;
use tokio::task::JoinSet;

use super::progress::Progress;
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
    /// Addresses dialed.
    pub tried: usize,
    /// Dials that got no answer in time.
    pub timed_out: usize,
    /// Dials the other side actively refused or that failed at the network.
    pub unreachable: usize,
    /// Addresses dialed through a relay (included in `tried`).
    pub relay_tried: usize,
}

impl Failures {
    /// One line on what the dials ran into.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.tried == 0 {
            return "no addresses to try".to_string();
        }
        let other = self.tried.saturating_sub(self.timed_out + self.unreachable);
        let mut parts = Vec::new();
        if self.timed_out > 0 {
            parts.push(format!("{} timed out", self.timed_out));
        }
        if self.unreachable > 0 {
            parts.push(format!("{} unreachable or refused", self.unreachable));
        }
        if other > 0 {
            parts.push(format!("{other} failed"));
        }
        let via = if self.relay_tried > 0 {
            format!(", {} of them through a relay", self.relay_tried)
        } else {
            String::new()
        };
        format!(
            "tried {} address(es){via}: {}",
            self.tried,
            parts.join(", ")
        )
    }
}

/// A short reason a dial failed, for the user.
fn reason(err: &NetError) -> String {
    match err {
        NetError::Timeout(_) => "no answer (timed out)".to_string(),
        NetError::Io(io) => match io.kind() {
            std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
            kind => format!("{kind}"),
        },
        NetError::NoSharedRepo => "peer does not have this repo".to_string(),
        NetError::AuthFailed(_) => "authentication failed".to_string(),
        other => other.to_string(),
    }
}

/// Dial bookkeeping for one sync.
pub struct Dialer {
    pub connected: Vec<Connected>,
    pub failures: Failures,
    tried: Vec<PeerAddr>,
    progress: Progress,
    ok: usize,
    failed: usize,
}

impl Dialer {
    /// Start from the sessions that are already open for `repo` (7.4).
    pub fn new(net: &NetState, repo: RepoId, progress: Progress) -> Self {
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
            progress,
            ok: 0,
            failed: 0,
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
        let relays_off = crate::net::relay::mode() == crate::net::relay::Mode::Off;
        let mut queue: Vec<Candidate> = candidates
            .into_iter()
            .filter(|candidate| !(relays_off && candidate.addr.is_relay()))
            .filter(|candidate| !self.tried.contains(&candidate.addr))
            .filter(|candidate| !self.has_node(candidate.node))
            .collect();
        // REL-14: a relay dial waits when a direct address for the same node
        // is being tried too, so the direct path wins when it works.
        let direct_nodes: Vec<NodeId> = queue
            .iter()
            .filter(|candidate| !candidate.addr.is_relay())
            .filter_map(|candidate| candidate.node)
            .collect();
        queue.reverse();
        let mut added = Vec::new();
        let mut set: JoinSet<(PeerAddr, Result<SessionHandle, NetError>)> = JoinSet::new();
        loop {
            while set.len() < MAX_DIALS && self.connected.len() < limit {
                let Some(candidate) = queue.pop() else { break };
                self.tried.push(candidate.addr.clone());
                self.failures.tried += 1;
                if candidate.addr.is_relay() {
                    self.failures.relay_tried += 1;
                }
                let wait = match (&candidate.addr, candidate.node) {
                    (PeerAddr::Relay { node, .. }, _) if direct_nodes.contains(node) => {
                        crate::net::relay::dial_delay()
                    }
                    _ => Duration::ZERO,
                };
                self.progress
                    .detail("dial", format!("trying {}", candidate.addr));
                self.progress
                    .dials(self.ok, self.failed, set.len() + 1, queue.len());
                let net = Arc::clone(net);
                set.spawn(async move {
                    let result = dial(&net, repo, &candidate, wait).await;
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
                    self.ok += 1;
                    self.progress.detail("dial", format!("connected to {addr}"));
                    added.push(self.connected.len());
                    self.connected.push(Connected::new(handle));
                }
                Err(err) => {
                    net.peer_fail(repo, &addr);
                    self.failures.auth |= matches!(err, NetError::AuthFailed(_));
                    self.failures.no_shared |= matches!(err, NetError::NoSharedRepo);
                    match &err {
                        NetError::Timeout(_) => self.failures.timed_out += 1,
                        NetError::Io(_) => self.failures.unreachable += 1,
                        _ => {}
                    }
                    self.failed += 1;
                    self.progress
                        .detail("dial", format!("{addr}: {}", reason(&err)));
                    tracing::debug!(peer = %addr, code = err.code(), error = %err, "sync peer failed");
                }
            }
            self.progress
                .dials(self.ok, self.failed, set.len(), queue.len());
            if self.connected.len() >= limit {
                // Sessions that still finish stay registered for later use.
                set.detach_all();
                break;
            }
        }
        added
    }
}

fn relay_error(err: RelayError) -> NetError {
    match err {
        RelayError::Timeout(what) => NetError::Timeout(format!("relay: {what}")),
        RelayError::Io(io) => NetError::Io(io),
        other => NetError::Relay(other.to_string()),
    }
}

async fn dial(
    net: &Arc<NetState>,
    repo: RepoId,
    candidate: &Candidate,
    wait: Duration,
) -> Result<SessionHandle, NetError> {
    let transport = if let PeerAddr::Relay { relay, node } = &candidate.addr {
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
            // The direct address won while this dial waited.
            if let Some(handle) = net
                .sessions_for(repo)
                .into_iter()
                .find(|handle| handle.info().node_id == *node)
            {
                return Ok(handle);
            }
        }
        let stream = swarm_relay::connect(&relay.to_string(), *node)
            .await
            .map_err(relay_error)?;
        // The relay's address says nothing about how the peer sees us, so the
        // handshake gets a neutral address (observed-address voting ignores
        // an unspecified IP).
        let neutral = PeerAddr::Tcp(SocketAddr::from(([0, 0, 0, 0], 0)));
        handshake::connect(
            stream,
            &net.node,
            std::slice::from_ref(&repo),
            Some(*node),
            neutral,
            HANDSHAKE_TIMEOUT,
        )
        .await?
    } else {
        let socket: SocketAddr = candidate.addr.socket();
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(socket))
            .await
            .map_err(|_| NetError::Timeout("connect timed out".into()))??;
        handshake::connect(
            stream,
            &net.node,
            std::slice::from_ref(&repo),
            candidate.node,
            candidate.addr.clone(),
            HANDSHAKE_TIMEOUT,
        )
        .await?
    };
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

/// True for addresses that only work on one physical network: IPv6
/// link-local (`fe80::/10`). Remembered or exchanged ones are useless from
/// another network, so only the LAN route may offer them.
fn is_link_local(addr: &PeerAddr) -> bool {
    match addr.socket().ip() {
        std::net::IpAddr::V6(ip) => ip.segments()[0] & 0xffc0 == 0xfe80,
        std::net::IpAddr::V4(_) => false,
    }
}

/// Drop link-local addresses from `candidates`.
pub fn without_link_local(candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates
        .into_iter()
        .filter(|candidate| !is_link_local(&candidate.addr))
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
