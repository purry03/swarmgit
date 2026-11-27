//! Peer addresses and their wire encoding (spec 7.3, `Peers`).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use swarm_core::ids::NodeId;

use crate::proxy::Endpoint;
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
    /// A node behind an IPv4 relay: relay IP, port, then the node ID.
    RelayIpv4 = 8,
    /// A node behind an IPv6 relay: relay IP, port, then the node ID.
    RelayIpv6 = 9,
}

/// A peer reachable over TCP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PeerAddr {
    /// TCP address.
    Tcp(SocketAddr),
    /// An onion service, or a name only the proxy may resolve. Never sent in
    /// `Peers`, which carries IP addresses only.
    Named {
        /// Host name.
        host: String,
        /// TCP port.
        port: u16,
    },
    /// A node that cannot accept inbound connections, reached through a
    /// relay (relay design spec 5.1). It always carries the node ID, so it is
    /// always pinned.
    Relay {
        /// The relay's address.
        relay: Endpoint,
        /// The node behind the relay.
        node: NodeId,
    },
}

impl PeerAddr {
    /// Socket address, when there is a literal one.
    ///
    /// For a relay address this is the relay's own address.
    #[must_use]
    pub fn socket(&self) -> Option<SocketAddr> {
        match self {
            Self::Tcp(addr) => Some(*addr),
            Self::Named { .. } => None,
            Self::Relay { relay, .. } => relay.socket(),
        }
    }

    /// What to open a TCP connection to.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        match self {
            Self::Tcp(addr) => Endpoint::Ip(*addr),
            Self::Named { host, port } => Endpoint::Name {
                host: host.clone(),
                port: *port,
            },
            Self::Relay { relay, .. } => relay.clone(),
        }
    }

    /// True for an address reached through a relay.
    #[must_use]
    pub fn is_relay(&self) -> bool {
        matches!(self, Self::Relay { .. })
    }

    /// Encode for the `Peers` message. `node` adds kinds 5 and 7. An address
    /// without an IP has no wire form and encodes to nothing.
    #[must_use]
    pub fn encode(&self, node: Option<NodeId>) -> Vec<u8> {
        match self {
            Self::Named { .. } => Vec::new(),
            Self::Relay { relay, node } => {
                let Some(relay) = relay.socket() else {
                    return Vec::new();
                };
                let mut out = Vec::with_capacity(51);
                match relay.ip() {
                    IpAddr::V4(ip) => {
                        out.push(PeerAddrKind::RelayIpv4 as u8);
                        out.extend_from_slice(&ip.octets());
                    }
                    IpAddr::V6(ip) => {
                        out.push(PeerAddrKind::RelayIpv6 as u8);
                        out.extend_from_slice(&ip.octets());
                    }
                }
                out.extend_from_slice(&relay.port().to_be_bytes());
                out.extend_from_slice(node.as_bytes());
                out
            }
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
            8 | 9 => {
                let width = if kind == 8 { 4 } else { 16 };
                let (addr, node) = take_addr(rest, width, true)?;
                let node =
                    node.ok_or_else(|| NetError::Malformed("relay address has no node ID".into()))?;
                Ok((
                    PeerAddr::Relay {
                        relay: addr.endpoint(),
                        node,
                    },
                    Some(node),
                ))
            }
            other => Err(NetError::Malformed(format!(
                "unknown peer address kind {other}"
            ))),
        }
    }

    /// Parse `host:port` (resolving nothing), or the `relay:host:port@node`
    /// form that [`fmt::Display`] writes for relay addresses.
    pub fn parse_socket(value: &str) -> Result<Self, NetError> {
        if let Some(rest) = value.strip_prefix("relay:") {
            let bad = || NetError::Malformed(format!("bad address {value}"));
            let (relay, node) = rest.rsplit_once('@').ok_or_else(bad)?;
            return Ok(Self::Relay {
                relay: Endpoint::parse(relay).ok_or_else(bad)?,
                node: NodeId::from_base32(node).map_err(|_| bad())?,
            });
        }
        match Endpoint::parse(value) {
            Some(Endpoint::Ip(addr)) => Ok(Self::Tcp(addr)),
            Some(Endpoint::Name { host, port }) => Ok(Self::Named { host, port }),
            None => Err(NetError::Malformed(format!("bad address {value}"))),
        }
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
            Self::Named { host, port } => write!(f, "{host}:{port}"),
            Self::Relay { relay, node } => write!(f, "relay:{relay}@{}", node.to_base32()),
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
    fn relay_addresses_round_trip_and_always_carry_the_node() {
        let node = NodeId::from_bytes([9u8; 32]);
        for relay in ["203.0.113.5:42425", "[2001:db8::1]:42425"] {
            let addr = PeerAddr::Relay {
                relay: Endpoint::Ip(relay.parse().unwrap()),
                node,
            };
            // The node argument is ignored: the node is part of the address.
            let bytes = addr.encode(None);
            assert_eq!(
                PeerAddr::decode(&bytes).unwrap(),
                (addr.clone(), Some(node))
            );
            assert_eq!(PeerAddr::parse_socket(&addr.to_string()).unwrap(), addr);
            assert!(addr.is_relay());
        }
        // Kind 8 without the node bytes is malformed.
        assert!(PeerAddr::decode(&[8, 1, 2, 3, 4, 0, 1]).is_err());
        assert!(PeerAddr::parse_socket("relay:1.2.3.4:5").is_err());
    }

    #[test]
    fn named_addresses_stay_out_of_the_wire_format() {
        let onion = PeerAddr::parse_socket("abcd.onion:42424").unwrap();
        assert!(matches!(onion, PeerAddr::Named { .. }));
        assert!(onion.encode(None).is_empty() && onion.socket().is_none());
        assert_eq!(onion.to_string(), "abcd.onion:42424");
        let relayed = PeerAddr::Relay {
            relay: onion.endpoint(),
            node: NodeId::from_bytes([1; 32]),
        };
        assert!(relayed.encode(None).is_empty());
        assert_eq!(
            PeerAddr::parse_socket(&relayed.to_string()).unwrap(),
            relayed
        );
    }

    #[test]
    fn rejects_bad_kinds_and_lengths() {
        assert!(PeerAddr::decode(&[10, 0, 0]).is_err());
        assert!(PeerAddr::decode(&[4, 1, 2, 3]).is_err());
    }
}
