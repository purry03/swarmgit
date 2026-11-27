//! UDP hole punching (spec 11.10), on unless the `udp` setting is false.
//!
//! Two peers that only reach each other through a relay use that session to
//! swap UDP candidates, punch a path, and run a second, ordinary handshake
//! over it. When any step fails the relayed session carries on.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::crypto;
use swarm_core::ids::{NodeId, RepoId};
use swarm_net::handshake;
use swarm_net::udp;
use swarm_net::{Endpoint, Message, PeerAddr, Session, SessionHandle};
use tokio::net::UdpSocket;

use super::NetState;

/// `Hello` capability of a node that answers `PunchOffer`.
pub const CAP: &str = "udp-punch";
const PUNCH_TIMEOUT: Duration = Duration::from_secs(5);
const OFFER_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Most candidates sent or probed.
const MAX_CANDIDATES: usize = 8;
/// Punches this node answers at once.
pub const MAX_ANSWERING: usize = 2;

/// Our addresses for `socket`: what the relays see, then each interface.
async fn candidates(socket: &UdpSocket, relays: &[SocketAddr]) -> Vec<SocketAddr> {
    let Ok(local) = socket.local_addr() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut loopback = false;
    for relay in relays.iter().take(2) {
        loopback |= relay.ip().is_loopback();
        if let Some(mapped) = udp::reflect(socket, *relay).await {
            out.push(mapped);
        }
    }
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        let ip = interface.ip();
        if let IpAddr::V4(v4) = ip {
            if !v4.is_loopback() && !v4.is_link_local() {
                out.push(SocketAddr::new(ip, local.port()));
            }
        }
    }
    // Only useful when the relay itself is on this machine, as in tests.
    if loopback {
        out.push(SocketAddr::from(([127, 0, 0, 1], local.port())));
    }
    let mut unique = Vec::new();
    for addr in out {
        if !unique.contains(&addr) && unique.len() < MAX_CANDIDATES {
            unique.push(addr);
        }
    }
    unique
}

fn encode(addresses: &[SocketAddr]) -> Vec<Vec<u8>> {
    addresses
        .iter()
        .map(|addr| PeerAddr::Tcp(*addr).encode(None))
        .collect()
}

/// IPv4 candidates a peer offered. Unspecified, multicast and broadcast
/// addresses are never probed, nor loopback ones unless we offered loopback.
fn decode(addresses: &[Vec<u8>], loopback: bool) -> Vec<SocketAddr> {
    addresses
        .iter()
        .take(MAX_CANDIDATES)
        .filter_map(|bytes| PeerAddr::decode(bytes).ok()?.0.socket())
        .filter(|addr| match addr.ip() {
            IpAddr::V4(ip) => {
                !ip.is_unspecified()
                    && !ip.is_multicast()
                    && !ip.is_broadcast()
                    && (loopback || !ip.is_loopback())
            }
            IpAddr::V6(_) => false,
        })
        .filter(|addr| addr.port() != 0)
        .collect()
}

/// The relay's IPv4 address, where its UDP reflector answers. A relay known
/// by name (a link or `relays` may carry one) is looked up.
async fn reflector(relay: &Endpoint) -> Option<SocketAddr> {
    match relay {
        Endpoint::Ip(addr) => Some(*addr),
        Endpoint::Name { host, port } if !relay.is_onion() => {
            tokio::net::lookup_host((host.as_str(), *port))
                .await
                .ok()?
                .find(SocketAddr::is_ipv4)
        }
        Endpoint::Name { .. } => None,
    }
}

async fn bind() -> Option<Arc<UdpSocket>> {
    UdpSocket::bind("0.0.0.0:0").await.ok().map(Arc::new)
}

/// Try to replace the relayed session with `node` by a direct UDP one.
pub async fn upgrade(
    net: &Arc<NetState>,
    repo: RepoId,
    relayed: &SessionHandle,
    node: NodeId,
    relay: &Endpoint,
) -> Option<SessionHandle> {
    // Never on behind a proxy, so resolving the relay's name leaks nothing.
    if !net.settings.udp || !relayed.info().caps.iter().any(|cap| cap == CAP) {
        return None;
    }
    let relay = reflector(relay).await?;
    let socket = bind().await?;
    let ours = candidates(&socket, &[relay]).await;
    let token = crypto::random_bytes::<16>().ok()?;
    let reply = relayed
        .request(
            Message::PunchOffer {
                rid: 0,
                token,
                addresses: encode(&ours),
            },
            OFFER_TIMEOUT,
        )
        .await;
    let Ok(Message::PunchAnswer { addresses, .. }) = reply else {
        return None;
    };
    let theirs = decode(&addresses, relay.ip().is_loopback());
    let peer = match udp::punch(&socket, token, &theirs, PUNCH_TIMEOUT).await {
        Ok(peer) => peer,
        Err(err) => {
            tracing::debug!(error = %err, "udp punch failed; staying on the relay");
            return None;
        }
    };
    let stream = udp::stream(socket, peer, token);
    let transport = handshake::connect(
        stream,
        &net.node,
        std::slice::from_ref(&repo),
        Some(node),
        PeerAddr::Tcp(peer),
        net.handshake_options(HANDSHAKE_TIMEOUT),
    )
    .await
    .map_err(|err| tracing::debug!(error = %err, "handshake over the udp path failed"))
    .ok()?;
    tracing::info!(peer = %peer, "udp path established");
    Some(super::serve::spawn_session(net, Session::start(transport)))
}

/// Serve the session arriving over a punched path. Boxed, because a session
/// can itself lead back here and the future types would otherwise be circular.
fn serve_path(
    net: Arc<NetState>,
    stream: tokio::io::DuplexStream,
    peer: SocketAddr,
) -> Pin<Box<dyn Future<Output = bool> + Send>> {
    Box::pin(super::serve::handle_stream(
        net,
        stream,
        PeerAddr::Tcp(peer),
        "udp",
    ))
}

/// Answer a `PunchOffer`: reply with our candidates, punch, and serve the
/// session that comes over the new path.
pub async fn answer(
    net: Arc<NetState>,
    handle: SessionHandle,
    rid: u64,
    token: [u8; 16],
    addresses: Vec<Vec<u8>>,
) -> bool {
    if !net.settings.udp {
        return false;
    }
    let Ok(_permit) = Arc::clone(&net.punches).try_acquire_owned() else {
        return false;
    };
    let Some(socket) = bind().await else {
        return false;
    };
    let relays: Vec<SocketAddr> = net
        .relay_statuses()
        .iter()
        .filter(|status| status.registered)
        .filter_map(|status| status.addr.as_ref().and_then(|addr| addr.socket()))
        .collect();
    let ours = candidates(&socket, &relays).await;
    let theirs = decode(&addresses, relays.iter().any(|r| r.ip().is_loopback()));
    let reply = Message::PunchAnswer {
        rid,
        addresses: encode(&ours),
    };
    if handle.send(&reply).await.is_err() {
        return true;
    }
    match udp::punch(&socket, token, &theirs, PUNCH_TIMEOUT).await {
        Ok(peer) => {
            let stream = udp::stream(socket, peer, token);
            tokio::spawn(serve_path(Arc::clone(&net), stream, peer));
        }
        Err(err) => tracing::debug!(error = %err, "udp punch failed"),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_candidates_are_filtered() {
        let offered: Vec<SocketAddr> = [
            "192.0.2.1:5",
            "0.0.0.0:5",
            "224.0.0.1:5",
            "255.255.255.255:5",
            "127.0.0.1:5",
        ]
        .iter()
        .map(|text| text.parse().unwrap())
        .collect();
        let encoded = encode(&offered);
        assert_eq!(decode(&encoded, false), ["192.0.2.1:5".parse().unwrap()]);
        assert_eq!(decode(&encoded, true).len(), 2);
    }
}
