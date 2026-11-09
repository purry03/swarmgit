//! Identifiers, base32, links and object IDs (spec sections 4.3, 4.4 and 5.3).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use data_encoding::{BASE32_NOPAD, HEXLOWER};
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::consts::{MAX_HINTS, MAX_LINK_BYTES};

/// Errors while parsing or formatting an identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdError {
    /// The identifier is not the expected length.
    #[error("identifier has the wrong length")]
    BadLength,
    /// A character is outside the base32 alphabet.
    #[error("identifier contains a character outside the base32 alphabet")]
    BadAlphabet,
    /// The final base32 character has non-zero padding bits.
    #[error("identifier is not canonical (non-zero padding bits)")]
    NonCanonical,
    /// Hex input contained a non-hex character.
    #[error("identifier contains invalid hex")]
    BadHex,
}

/// Decode 52 lowercase base32 characters into 32 bytes.
///
/// Uppercase input is accepted and lowered first, per spec 4.4. The final
/// character must have zero padding bits, so only one encoding of a value is
/// accepted.
fn decode_base32_32(input: &str) -> Result<[u8; 32], IdError> {
    if input.len() != 52 {
        return Err(IdError::BadLength);
    }
    let lower = input.to_ascii_lowercase();
    if !lower
        .bytes()
        .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    {
        return Err(IdError::BadAlphabet);
    }
    // 52 chars carry 260 bits, 4 more than 32 bytes. Only the top bit of the
    // final symbol may be used.
    let last = lower.as_bytes()[51];
    let value = match last {
        b'a'..=b'z' => last - b'a',
        b'2'..=b'7' => last - b'2' + 26,
        _ => return Err(IdError::BadAlphabet),
    };
    if value & 0x0f != 0 {
        return Err(IdError::NonCanonical);
    }
    let decoded = BASE32_NOPAD
        .decode(lower.to_ascii_uppercase().as_bytes())
        .map_err(|_| IdError::BadAlphabet)?;
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| IdError::BadLength)
}

fn encode_base32_32(bytes: &[u8; 32]) -> String {
    BASE32_NOPAD.encode(bytes).to_ascii_lowercase()
}

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialOrd, Ord)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Length in bytes.
            pub const LEN: usize = 32;

            /// Wrap raw bytes.
            #[must_use]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// Borrow the raw bytes.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Copy the raw bytes.
            #[must_use]
            pub const fn to_bytes(self) -> [u8; 32] {
                self.0
            }

            /// Parse exactly 32 bytes.
            pub fn from_slice(bytes: &[u8]) -> Result<Self, IdError> {
                let arr: [u8; 32] = bytes.try_into().map_err(|_| IdError::BadLength)?;
                Ok(Self(arr))
            }

            /// Lowercase hex, used in logs and file names.
            #[must_use]
            pub fn to_hex(self) -> String {
                HEXLOWER.encode(&self.0)
            }

            /// Parse lowercase or uppercase hex.
            pub fn from_hex(input: &str) -> Result<Self, IdError> {
                let bytes = HEXLOWER
                    .decode(input.to_ascii_lowercase().as_bytes())
                    .map_err(|_| IdError::BadHex)?;
                Self::from_slice(&bytes)
            }

            /// Lowercase base32 without padding, 52 characters.
            #[must_use]
            pub fn to_base32(self) -> String {
                encode_base32_32(&self.0)
            }

            /// Parse 52 characters of lowercase or uppercase base32.
            pub fn from_base32(input: &str) -> Result<Self, IdError> {
                decode_base32_32(input).map(Self)
            }
        }

        impl std::hash::Hash for $name {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.0.hash(state);
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.0.as_slice().ct_eq(other.0.as_slice()).into()
            }
        }

        impl Eq for $name {}

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.to_hex())
            }
        }

        impl From<[u8; 32]> for $name {
            fn from(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }

        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }
    };
}

id_type!(RepoId, "The BLAKE3 key ID of a repo's root key bundle.");
id_type!(KeyId, "The BLAKE3 hash of a public key bundle.");
id_type!(NodeId, "The key ID of a peer's node key bundle.");

/// A git object ID. Length depends on the repo object format (20 or 32 bytes).
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Oid(Vec<u8>);

impl Oid {
    /// Wrap raw object ID bytes.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Lowercase hex.
    #[must_use]
    pub fn to_hex(&self) -> String {
        HEXLOWER.encode(&self.0)
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl From<Vec<u8>> for Oid {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for Oid {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Hint transport schemes defined by v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintScheme {
    /// Plain TCP.
    Tcp,
    /// A Tor onion service reached through a SOCKS5 proxy (M9).
    Onion,
    /// A node reached through a relay. The host and port are the relay's and
    /// the pin after `@` is the node behind it, so the pin is required.
    Relay,
}

impl HintScheme {
    fn parse(input: &str) -> Option<Self> {
        match input {
            "tcp" => Some(Self::Tcp),
            "onion" => Some(Self::Onion),
            "relay" => Some(Self::Relay),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Onion => "onion",
            Self::Relay => "relay",
        }
    }
}

/// A host in a link hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// Dotted-quad IPv4.
    Ipv4(Ipv4Addr),
    /// Bracketed IPv6.
    Ipv6(Ipv6Addr),
    /// DNS name or onion name.
    Name(String),
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ipv4(addr) => write!(f, "{addr}"),
            Self::Ipv6(addr) => write!(f, "[{addr}]"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

impl Host {
    /// Parse a hint host: bracketed IPv6, dotted-quad IPv4, or a DNS name.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        if let Some(inner) = input.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return inner.parse::<Ipv6Addr>().ok().map(Self::Ipv6);
        }
        if let Ok(addr) = input.parse::<Ipv4Addr>() {
            return Some(Self::Ipv4(addr));
        }
        if input.is_empty()
            || input
                .chars()
                .any(|c| matches!(c, ':' | '/' | '@' | '[' | ']' | '?' | '#') || c.is_whitespace())
        {
            return None;
        }
        Some(Self::Name(input.to_string()))
    }
}

/// One `h=` hint inside a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// Transport scheme.
    pub scheme: HintScheme,
    /// Host or address.
    pub host: Host,
    /// TCP port.
    pub port: u16,
    /// Optional node ID pin.
    pub node: Option<NodeId>,
}

impl Hint {
    /// Parse the value after `h=`.
    ///
    /// Unknown schemes and malformed addresses return `None` so the caller can
    /// ignore the hint. A malformed pin is ignored and the hint stays unpinned.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (addr, pin) = match value.rsplit_once('@') {
            Some((addr, pin)) => (addr, NodeId::from_base32(pin).ok()),
            None => (value, None),
        };
        let (scheme_str, rest) = addr.split_once(':')?;
        let scheme = HintScheme::parse(scheme_str)?;
        let (host_str, port_str) = if let Some(after_bracket) = rest.strip_prefix('[') {
            let (inner, after) = after_bracket.split_once(']')?;
            (format!("[{inner}]"), after.strip_prefix(':')?)
        } else {
            let (host, port) = rest.rsplit_once(':')?;
            (host.to_string(), port)
        };
        let host = Host::parse(&host_str)?;
        let port = port_str.parse::<u16>().ok().filter(|p| *p != 0)?;
        Some(Self {
            scheme,
            host,
            port,
            node: pin,
        })
    }
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "h={}:{}:{}", self.scheme.as_str(), self.host, self.port)?;
        if let Some(node) = self.node {
            write!(f, "@{}", node.to_base32())?;
        }
        Ok(())
    }
}

/// A parsed `swarm://` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Repo key ID.
    pub repo: RepoId,
    /// Hints, at most eight.
    pub hints: Vec<Hint>,
}

/// Errors while parsing a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LinkError {
    /// The link exceeds 512 bytes.
    #[error("link is longer than 512 bytes")]
    TooLong,
    /// The link does not begin with `swarm://`.
    #[error("link does not start with swarm://")]
    BadScheme,
    /// The repo ID failed base32 validation.
    #[error("link has an invalid repo ID")]
    BadRepoId,
    /// The link has no repo ID at all.
    #[error("link has no repo ID")]
    MissingRepo,
}

impl Link {
    /// Parse a link. Unknown hint schemes and malformed hints are ignored.
    pub fn parse(input: &str) -> Result<Self, LinkError> {
        if input.len() > MAX_LINK_BYTES {
            return Err(LinkError::TooLong);
        }
        let rest = input.strip_prefix("swarm://").ok_or(LinkError::BadScheme)?;
        if rest.is_empty() {
            return Err(LinkError::MissingRepo);
        }
        let (repo_str, query) = match rest.split_once('?') {
            Some((repo, query)) => (repo, Some(query)),
            None => (rest, None),
        };
        let repo = RepoId::from_base32(repo_str).map_err(|_| LinkError::BadRepoId)?;
        let mut hints = Vec::new();
        if let Some(query) = query {
            for part in query.split('&') {
                if hints.len() == MAX_HINTS {
                    break;
                }
                if let Some(value) = part.strip_prefix("h=") {
                    if let Some(hint) = Hint::parse(value) {
                        hints.push(hint);
                    }
                }
            }
        }
        Ok(Self { repo, hints })
    }
}

impl FromStr for Link {
    type Err = LinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "swarm://{}", self.repo.to_base32())?;
        for (i, hint) in self.hints.iter().enumerate() {
            f.write_str(if i == 0 { "?" } else { "&" })?;
            write!(f, "{hint}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trip() {
        let bytes = [7u8; 32];
        let id = RepoId::from_bytes(bytes);
        let text = id.to_base32();
        assert_eq!(text.len(), 52);
        assert_eq!(RepoId::from_base32(&text).unwrap(), id);
        assert_eq!(RepoId::from_base32(&text.to_uppercase()).unwrap(), id);
    }

    #[test]
    fn base32_rejects_padding_bits() {
        let mut text = RepoId::from_bytes([0u8; 32]).to_base32();
        assert_eq!(text.pop(), Some('a'));
        text.push('b');
        assert_eq!(RepoId::from_base32(&text), Err(IdError::NonCanonical));
    }

    #[test]
    fn link_round_trip() {
        let repo = RepoId::from_bytes([9u8; 32]);
        let node = NodeId::from_bytes([4u8; 32]);
        let link = Link {
            repo,
            hints: vec![
                Hint {
                    scheme: HintScheme::Tcp,
                    host: Host::Ipv4("127.0.0.1".parse().unwrap()),
                    port: 42424,
                    node: Some(node),
                },
                Hint {
                    scheme: HintScheme::Tcp,
                    host: Host::Ipv6("::1".parse().unwrap()),
                    port: 1,
                    node: None,
                },
            ],
        };
        let text = link.to_string();
        assert!(text.contains("h=tcp:127.0.0.1:42424@"));
        assert!(text.contains("h=tcp:[::1]:1"));
        assert_eq!(Link::parse(&text).unwrap(), link);
    }

    #[test]
    fn link_ignores_unknown_and_malformed_hints() {
        let repo = RepoId::from_bytes([1u8; 32]).to_base32();
        let link = Link::parse(&format!(
            "swarm://{repo}?h=quic:host:1&h=tcp:host:notaport&h=tcp:host:2@not-a-node"
        ))
        .unwrap();
        assert_eq!(link.hints.len(), 1);
        assert_eq!(link.hints[0].node, None);
    }
}
