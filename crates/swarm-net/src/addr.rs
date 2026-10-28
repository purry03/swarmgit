//! Peer addresses and their wire encoding (spec 7.3, `Peers`).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use swarm_core::ids::NodeId;

use crate::NetError;

/// Kinds used by the `Peers` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAddrKind {
    /// IPv4 without a node ID.
    Ipv4 = 4,
    /// IPv4 followed by a 32-byte node ID.
    Ipv4Node = 5,
    /// IPv6 without a node ID.
    Ipv6 = 6,
    /// IPv6 followed by a 32-byte node ID.
    Ipv6Node = 7,
}

/// A peer reachable over TCP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerAddr {
    /// TCP address.
    Tcp(SocketAddr),
}

impl PeerAddr {
    /// Socket address.
    #[must_use]
    pub fn socket(&self) -> SocketAddr {
        match self {
            Self::Tcp(addr) => *addr,
        }
    }

    /// Encode for the `Peers` message. `node` adds kinds 5 and 7.
    #[must_use]
    pub fn encode(&self, node: Option<NodeId>) -> Vec<u8> {
        match self {
            Self::Tcp(addr) => {
                let mut out = Vec::with_capacity(35);
                let (kind, ip_bytes) = match addr.ip() {
                    IpAddr::V4(ip) => (
                        if node.is_some() {
                            PeerAddrKind::Ipv4Node
                        } else {
                            PeerAddrKind::Ipv4
                        },
                        ip.octets().to_vec(),
                    ),
                    IpAddr::V6(ip) => (
                        if node.is_some() {
                            PeerAddrKind::Ipv6Node
                        } else {
                            PeerAddrKind::Ipv6
                        },
                        ip.octets().to_vec(),
                    ),
                };
                out.push(kind as u8);
                out.extend_from_slice(&ip_bytes);
                out.extend_from_slice(&addr.port().to_be_bytes());
                if let Some(node) = node {
                    out.extend_from_slice(node.as_bytes());
                }
                out
            }
        }
    }

    /// Decode one `Peers` entry.
    pub fn decode(bytes: &[u8]) -> Result<(Self, Option<NodeId>), NetError> {
        let Some((&kind, rest)) = bytes.split_first() else {
            return Err(NetError::Malformed("empty peer address".into()));
        };
        match kind {
            4 | 5 => {
                let width = 4;
                take_addr(rest, width, kind == 5)
            }
            6 | 7 => {
                let width = 16;
                take_addr(rest, width, kind == 7)
            }
            other => Err(NetError::Malformed(format!(
                "unknown peer address kind {other}"
            ))),
        }
    }

    /// Parse `host:port`, resolving nothing.
    pub fn parse_socket(value: &str) -> Result<Self, NetError> {
        value
            .parse::<SocketAddr>()
            .map(Self::Tcp)
            .map_err(|_| NetError::Malformed(format!("bad address {value}")))
    }
}

fn take_addr(
    rest: &[u8],
    width: usize,
    with_node: bool,
) -> Result<(PeerAddr, Option<NodeId>), NetError> {
    let expected = width + 2 + if with_node { 32 } else { 0 };
    if rest.len() != expected {
        return Err(NetError::Malformed(
            "peer address has the wrong length".into(),
        ));
    }
    let ip = if width == 4 {
        let octets: [u8; 4] = rest[..4]
            .try_into()
            .map_err(|_| NetError::Malformed("peer address has the wrong length".into()))?;
        IpAddr::V4(Ipv4Addr::from(octets))
    } else {
        let octets: [u8; 16] = rest[..16]
            .try_into()
            .map_err(|_| NetError::Malformed("peer address has the wrong length".into()))?;
        IpAddr::V6(Ipv6Addr::from(octets))
    };
    let port = u16::from_be_bytes([rest[width], rest[width + 1]]);
    let node = if with_node {
        let bytes: [u8; 32] = rest[width + 2..]
            .try_into()
            .map_err(|_| NetError::Malformed("peer address has the wrong length".into()))?;
        Some(NodeId::from_bytes(bytes))
    } else {
        None
    };
    Ok((PeerAddr::Tcp(SocketAddr::new(ip, port)), node))
}

impl fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tcp(addr) => write!(f, "{addr}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_round_trip_with_and_without_node() {
        let addr = PeerAddr::Tcp("127.0.0.1:42501".parse().unwrap());
        let node = NodeId::from_bytes([7u8; 32]);
        let plain = addr.encode(None);
        assert_eq!(PeerAddr::decode(&plain).unwrap(), (addr.clone(), None));
        let pinned = addr.encode(Some(node));
        assert_eq!(PeerAddr::decode(&pinned).unwrap(), (addr, Some(node)));
    }

    #[test]
    fn ipv6_round_trip() {
        let addr = PeerAddr::Tcp("[::1]:9000".parse().unwrap());
        let bytes = addr.encode(None);
        assert_eq!(PeerAddr::decode(&bytes).unwrap().0, addr);
    }

    #[test]
    fn rejects_bad_kinds_and_lengths() {
        assert!(PeerAddr::decode(&[9, 0, 0]).is_err());
        assert!(PeerAddr::decode(&[4, 1, 2, 3]).is_err());
    }
}
