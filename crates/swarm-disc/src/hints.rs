//! Route A: link hints (spec section 6.2).

use std::net::{IpAddr, SocketAddr};

use swarm_core::ids::{Hint, HintScheme, Host, NodeId};
use swarm_net::PeerAddr;
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

/// Resolve the `hints` array sent by the remote helper.
///
/// Strings have the form `tcp:host:port[@node-id]`. Unknown schemes and
/// malformed entries are skipped, as section 4.4 requires.
pub async fn resolve_hints(hints: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for raw in hints {
        let Some(hint) = Hint::parse(raw) else {
            continue;
        };
        let addresses: Vec<SocketAddr> = match &hint.host {
            Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(*ip), hint.port)],
            Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(*ip), hint.port)],
            Host::Name(name) => match lookup_host((name.as_str(), hint.port)).await {
                Ok(iter) => iter.collect(),
                Err(_) => Vec::new(),
            },
        };
        for addr in addresses {
            let candidate = if hint.scheme == HintScheme::Relay {
                // A relay hint names the node behind the relay, so without
                // the pin there is nothing to ask the relay for.
                let Some(node) = hint.node else { continue };
                Candidate {
                    addr: PeerAddr::Relay { relay: addr, node },
                    node: Some(node),
                    source: raw.clone(),
                }
            } else {
                Candidate {
                    addr: PeerAddr::Tcp(addr),
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
        let candidates = resolve_hints(&[raw]).await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].node, Some(node));
        assert_eq!(
            candidates[0].addr.socket(),
            "127.0.0.1:42501".parse().unwrap()
        );
    }

    #[tokio::test]
    async fn relay_hints_need_a_node_and_become_relay_addresses() {
        let node = NodeId::from_bytes([6u8; 32]);
        let raw = format!("relay:127.0.0.1:42425@{}", node.to_base32());
        let candidates = resolve_hints(&[raw]).await;
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].addr,
            PeerAddr::Relay {
                relay: "127.0.0.1:42425".parse().unwrap(),
                node
            }
        );
        assert_eq!(candidates[0].node, Some(node));
        assert!(resolve_hints(&["relay:127.0.0.1:42425".to_string()])
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn skips_unknown_schemes() {
        assert!(resolve_hints(&["udp:127.0.0.1:1".to_string()])
            .await
            .is_empty());
    }
}
