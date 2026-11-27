//! Candidates, dialing and peer exchange for a sync (DM-1 to DM-5, 6.5).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::{NodeId, Oid, RepoId};
use swarm_net::addr::PeerAddr;
use swarm_net::handshake;
use swarm_net::{Endpoint, Message, NetError, Session, SessionHandle};
use swarm_relay::RelayError;
use tokio::task::JoinSet;

use super::progress::Progress;
use crate::net::capped::Capped;
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
#[derive(Clone)]
pub struct Candidate {
    pub addr: PeerAddr,
    pub node: Option<NodeId>,
}

/// A session that carries the repo.
pub struct Connected {
    pub handle: SessionHandle,
    /// The relayed session kept while `handle` runs over a punched UDP path,
    /// to carry on with if that path stops working.
    pub fallback: Option<SessionHandle>,
    pub tips: Vec<Oid>,
    pub failed: bool,
    /// What went wrong asking this peer for the repo state, for the error
    /// shown when no peer gave one.
    pub note: Option<String>,
}

impl Connected {
    fn new(dialed: Dialed) -> Self {
        Self {
            handle: dialed.handle,
            fallback: dialed.fallback,
            tips: Vec::new(),
            failed: false,
            note: None,
        }
    }

    pub fn node(&self) -> NodeId {
        self.handle.info().node_id
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        // The punched path did its job: give the relay its splice back.
        if let Some(relayed) = self.fallback.take() {
            relayed.close();
        }
    }
}

/// What one successful dial produced.
struct Dialed {
    handle: SessionHandle,
    fallback: Option<SessionHandle>,
}

impl From<SessionHandle> for Dialed {
    fn from(handle: SessionHandle) -> Self {
        Self {
            handle,
            fallback: None,
        }
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
    /// A relay on the way to a peer was at capacity.
    pub relay_full: bool,
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
            connected.push(Connected::new(handle.into()));
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

    /// Close sessions whose state request failed (a dead connection that is
    /// still listed, for example after a network change) and forget that
    /// their addresses were tried, so the next dial round opens fresh ones.
    /// Returns how many were dropped.
    pub fn drop_failed(&mut self) -> usize {
        let before = self.connected.len();
        self.connected.retain(|peer| {
            if peer.failed {
                peer.handle.close();
            }
            !peer.failed
        });
        let dropped = before - self.connected.len();
        if dropped > 0 {
            self.tried.clear();
        }
        dropped
    }

    fn has_node(&self, node: Option<NodeId>) -> bool {
        node.is_some_and(|node| self.connected.iter().any(|peer| peer.node() == node))
    }

    /// True when `candidate` would add nothing. A direct address is still
    /// tried for a node we only reach through a relay (REL-18).
    fn covered(&self, net: &NetState, candidate: &Candidate) -> bool {
        self.has_node(candidate.node)
            && !candidate
                .node
                .is_some_and(|node| !candidate.addr.is_relay() && net.is_relayed(node))
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
            .filter(|candidate| !self.covered(net, candidate))
            .collect();
        // A peer whose node ID is known (a pinned hint, a remembered peer, peer
        // exchange) may be registered with a relay we use too. Try it there as
        // well, without needing a relay hint in the link.
        if !relays_off {
            let relays: Vec<Endpoint> = net
                .relay_statuses()
                .iter()
                .filter(|status| status.registered)
                .filter_map(|status| status.addr.clone())
                .collect();
            let mut extra = Vec::new();
            for candidate in &queue {
                let Some(node) = candidate.node else { continue };
                if candidate.addr.is_relay() || node == net.node_id {
                    continue;
                }
                for relay in &relays {
                    let addr = PeerAddr::Relay {
                        relay: relay.clone(),
                        node,
                    };
                    if !queue
                        .iter()
                        .chain(extra.iter())
                        .any(|other: &Candidate| other.addr == addr)
                        && !self.tried.contains(&addr)
                    {
                        extra.push(Candidate {
                            addr,
                            node: Some(node),
                        });
                    }
                }
            }
            queue.extend(extra);
        }
        // REL-14: a relay dial waits when a direct address for the same node
        // is being tried too, so the direct path wins when it works.
        let direct_nodes: Vec<NodeId> = queue
            .iter()
            .filter(|candidate| !candidate.addr.is_relay())
            .filter_map(|candidate| candidate.node)
            .collect();
        queue.reverse();
        let mut added = Vec::new();
        let mut set: JoinSet<(PeerAddr, Result<Dialed, NetError>)> = JoinSet::new();
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
                Ok(dialed) => {
                    let node = dialed.handle.info().node_id;
                    if self.has_node(Some(node)) {
                        // A direct path replaces a relayed one (REL-18).
                        if !addr.is_relay() && net.is_relayed(node) {
                            net.set_relayed(node, None);
                            for peer in self.connected.iter_mut() {
                                if peer.node() == node {
                                    peer.handle.close();
                                    peer.handle = dialed.handle.clone();
                                    peer.failed = false;
                                }
                            }
                        }
                        continue;
                    }
                    self.ok += 1;
                    if addr.is_relay() && net.is_relayed(node) {
                        // REL-16.
                        tracing::info!(peer = %addr, "connected via relay");
                        self.progress
                            .say("dial", format!("connected via relay: {addr}"));
                    } else {
                        self.progress.detail("dial", format!("connected to {addr}"));
                    }
                    added.push(self.connected.len());
                    self.connected.push(Connected::new(dialed));
                }
                Err(err) => {
                    net.peer_fail(repo, &addr);
                    self.failures.auth |= matches!(err, NetError::AuthFailed(_));
                    self.failures.no_shared |= matches!(err, NetError::NoSharedRepo);
                    if matches!(&err, NetError::Relay(why) if why == swarm_relay::FULL) {
                        // Said once, even without -v.
                        if !self.failures.relay_full {
                            self.progress.say("relay", format!("{addr}: relay is full"));
                        }
                        self.failures.relay_full = true;
                    }
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
    if err.is_full() {
        return NetError::Relay(swarm_relay::FULL.into());
    }
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
) -> Result<Dialed, NetError> {
    let options = net.handshake_options(HANDSHAKE_TIMEOUT);
    // A relayed pair may then try for a direct UDP path.
    let mut punch_via = None;
    let transport = if let PeerAddr::Relay { relay, node } = &candidate.addr {
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
            // The direct address won while this dial waited.
            if let Some(handle) = net
                .sessions_for(repo)
                .into_iter()
                .find(|handle| handle.info().node_id == *node)
            {
                return Ok(handle.into());
            }
        }
        punch_via = Some((relay.clone(), *node));
        let (target, password) = net.relay_access(relay).await;
        let stream = swarm_relay::connect_with(&target, *node, password.as_deref())
            .await
            .map_err(relay_error)?;
        // The relay's address says nothing about how the peer sees us, so the
        // handshake gets a neutral address (observed-address voting ignores
        // an unspecified IP).
        let neutral = PeerAddr::Tcp(SocketAddr::from(([0, 0, 0, 0], 0)));
        let stream = Capped::new(stream, net.settings.relay_max_bytes);
        handshake::connect(
            net.transport.wrap(Box::new(stream)),
            &net.node,
            std::slice::from_ref(&repo),
            Some(*node),
            neutral,
            options,
        )
        .await?
    } else {
        let endpoint = candidate.addr.endpoint();
        if endpoint.is_onion() && !net.settings.proxied() {
            return Err(NetError::Io(std::io::Error::other(
                "an onion address needs the proxy setting",
            )));
        }
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, net.transport.dial(&endpoint))
            .await
            .map_err(|_| NetError::Timeout("connect timed out".into()))??;
        handshake::connect(
            stream,
            &net.node,
            std::slice::from_ref(&repo),
            candidate.node,
            candidate.addr.clone(),
            options,
        )
        .await?
    };
    let node = transport.info.node_id;
    net.peer_ok(repo, &candidate.addr, node);
    let via = match &candidate.addr {
        PeerAddr::Relay { relay, .. } => Some(relay.to_string()),
        _ => None,
    };
    net.set_relayed(node, via);
    let handle = crate::net::serve::spawn_session(net, Session::start(transport));
    if let Some((relay, node)) = punch_via {
        if let Some(direct) = crate::net::punch::upgrade(net, repo, &handle, node, &relay).await {
            net.set_relayed(node, None);
            return Ok(Dialed {
                handle: direct,
                fallback: Some(handle),
            });
        }
    }
    Ok(handle.into())
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
    match addr.socket().map(|socket| socket.ip()) {
        Some(std::net::IpAddr::V6(ip)) => ip.segments()[0] & 0xffc0 == 0xfe80,
        _ => false,
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
            let unspecified = addr
                .socket()
                .is_some_and(|socket| socket.ip().is_unspecified());
            // Link-local addresses from another node carry no interface to
            // dial them on.
            if node == Some(net.node_id) || unspecified || is_link_local(&addr) {
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
