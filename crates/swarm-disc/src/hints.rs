//! Route A: link hints (spec section 6.2).

use std::net::{IpAddr, SocketAddr};

use swarm_core::ids::{Hint, HintScheme, Host, NodeId};
use swarm_net::{Endpoint, PeerAddr};
use thiserror::Error;
use tokio::net::lookup_host;

/// Errors while resolving hints.
#[derive(Debug, Error)]
pub enum HintError {
    /// The hint string did not parse.
    #[error("bad hint")]
    BadHint,
    /// DNS resolution failed.
    #[error("cannot resolve {0}")]
    Resolve(String),
}

/// One dialable candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// TCP address.
    pub addr: PeerAddr,
    /// Optional node pin.
    pub node: Option<NodeId>,
    /// Where the candidate came from, for logs.
    pub source: String,
}

fn literal(hint: &Hint) -> Option<SocketAddr> {
    match &hint.host {
        Host::Ipv4(ip) => Some(SocketAddr::new(IpAddr::V4(*ip), hint.port)),
        Host::Ipv6(ip) => Some(SocketAddr::new(IpAddr::V6(*ip), hint.port)),
        Host::Name(_) => None,
    }
}

/// What to connect to for one hint. With a proxy, a name is passed on as it
/// is: looking it up here would tell the local resolver whom we contact. An
/// onion name only means something to the proxy, so without one it is skipped.
async fn endpoints(hint: &Hint, proxied: bool) -> Vec<Endpoint> {
    if let Some(addr) = literal(hint) {
        // `onion:` followed by an IP address is not an onion service.
        return if hint.scheme == HintScheme::Onion {
            Vec::new()
        } else {
            vec![Endpoint::Ip(addr)]
        };
    }
    let Host::Name(name) = &hint.host else {
        return Vec::new();
    };
    let named = Endpoint::Name {
        host: name.clone(),
        port: hint.port,
    };
    if proxied {
        return vec![named];
    }
    if named.is_onion() || hint.scheme == HintScheme::Onion {
        tracing::debug!(host = %name, "onion hint needs a proxy; skipped");
        return Vec::new();
    }
    match lookup_host((name.as_str(), hint.port)).await {
        Ok(found) => found.map(Endpoint::Ip).collect(),
        Err(_) => Vec::new(),
    }
}

fn address(endpoint: Endpoint) -> PeerAddr {
    match endpoint {
        Endpoint::Ip(addr) => PeerAddr::Tcp(addr),
        Endpoint::Name { host, port } => PeerAddr::Named { host, port },
    }
}

/// Resolve the `hints` array sent by the remote helper.
///
/// Strings have the form `tcp:host:port[@node-id]`. Unknown schemes and
/// malformed entries are skipped, as section 4.4 requires. `proxied` says
/// that a SOCKS5 proxy makes the connections.
pub async fn resolve_hints(hints: &[String], proxied: bool) -> Vec<Candidate> {
    let mut out = Vec::new();
    for raw in hints {
        let Some(hint) = Hint::parse(raw) else {
            continue;
        };
        // Mirrors are static file hosts, not peers to dial.
        if matches!(hint.scheme, HintScheme::Https | HintScheme::Http) {
            continue;
        }
        for endpoint in endpoints(&hint, proxied).await {
            let candidate = if hint.scheme == HintScheme::Relay {
                // A relay hint names the node behind the relay, so without
                // the pin there is nothing to ask the relay for.
                let Some(node) = hint.node else { continue };
                Candidate {
                    addr: PeerAddr::Relay {
                        relay: endpoint,
                        node,
                    },
                    node: Some(node),
                    source: raw.clone(),
                }
            } else {
                Candidate {
                    addr: address(endpoint),
                    node: hint.node,
                    source: raw.clone(),
                }
            };
            out.push(candidate);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_literal_ipv4_with_pin() {
        let node = NodeId::from_bytes([5u8; 32]);
        let raw = format!("tcp:127.0.0.1:42501@{}", node.to_base32());
        let candidates = resolve_hints(&[raw], false).await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].node, Some(node));
        assert_eq!(
            candidates[0].addr.socket(),
            Some("127.0.0.1:42501".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn relay_hints_need_a_node_and_become_relay_addresses() {
        let node = NodeId::from_bytes([6u8; 32]);
        let raw = format!("relay:127.0.0.1:42425@{}", node.to_base32());
        let candidates = resolve_hints(&[raw], false).await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].addr,
            PeerAddr::Relay {
                relay: Endpoint::Ip("127.0.0.1:42425".parse().unwrap()),
                node
            }
        );
        assert_eq!(candidates[0].node, Some(node));
        assert!(resolve_hints(&["relay:127.0.0.1:42425".to_string()], false)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn skips_unknown_schemes() {
        assert!(resolve_hints(&["udp:127.0.0.1:1".to_string()], false)
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn names_are_left_to_the_proxy() {
        let node = NodeId::from_bytes([7u8; 32]).to_base32();
        let hints = [
            "tcp:peer.invalid:1".to_string(),
            "onion:abcdefgh.onion:2".to_string(),
            format!("relay:relay.invalid:3@{node}"),
        ];
        let proxied = resolve_hints(&hints, true).await;
        let shown: Vec<String> = proxied.iter().map(|c| c.addr.to_string()).collect();
        assert_eq!(
            shown,
            [
                "peer.invalid:1".to_string(),
                "abcdefgh.onion:2".to_string(),
                format!("relay:relay.invalid:3@{node}"),
            ]
        );
        // Without a proxy nothing here can be reached.
        assert!(resolve_hints(&hints[1..2], false).await.is_empty());
    }
}
