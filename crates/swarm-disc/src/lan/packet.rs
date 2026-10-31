//! LAN packet encoding, token matching and rate limiting (spec 6.3).
//!
//! Everything here is pure: no sockets and no clock. The service in
//! `lan::service` feeds it bytes, addresses and times.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use swarm_core::consts::MAGIC_LAN;
use swarm_core::crypto::lan_token;
use swarm_core::ids::RepoId;

/// Largest LAN packet we send or accept.
pub const MAX_PACKET_BYTES: usize = 1200;

/// Size of the fixed header before the tokens.
pub const HEADER_BYTES: usize = 8;

/// Size of one token.
pub const TOKEN_BYTES: usize = 16;

/// Most tokens in one packet.
pub const MAX_TOKENS: usize = 16;

/// Tokens each repo contributes: previous, current and next hour.
pub const TOKENS_PER_REPO: usize = 3;

/// Most repos that fit in one packet.
pub const REPOS_PER_PACKET: usize = MAX_TOKENS / TOKENS_PER_REPO;

/// Packets per second from one source before LAN-3 drops the rest.
pub const RATE_LIMIT_PER_SEC: u32 = 20;

/// Sources the rate limiter remembers before it forgets the oldest.
const RATE_LIMIT_SOURCES: usize = 1024;

/// One 16-byte LAN token.
pub type Token = [u8; TOKEN_BYTES];

/// Packet kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// "I seed these repos on `tcp_port`."
    Announce = 1,
    /// "Who seeds these repos?"
    Query = 2,
}

/// A decoded LAN packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanPacket {
    /// Announce or query.
    pub kind: Kind,
    /// The sender's TCP listening port.
    pub tcp_port: u16,
    /// 1 to 16 tokens.
    pub tokens: Vec<Token>,
}

/// Why a packet was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketError {
    /// Shorter than the header, longer than 1200 bytes, or not `8 + 16n`.
    Size,
    /// Not `SWG1`.
    Magic,
    /// Kind other than 1 or 2.
    Kind,
    /// `n` outside 1 to 16.
    Count,
}

impl LanPacket {
    /// Encode to wire bytes. Returns `None` when the token count is not 1 to 16.
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        let n = self.tokens.len();
        if n == 0 || n > MAX_TOKENS {
            return None;
        }
        let mut out = Vec::with_capacity(HEADER_BYTES + TOKEN_BYTES * n);
        out.extend_from_slice(MAGIC_LAN);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.tcp_port.to_be_bytes());
        out.push(u8::try_from(n).ok()?);
        for token in &self.tokens {
            out.extend_from_slice(token);
        }
        Some(out)
    }

    /// Strictly decode wire bytes. Never panics.
    pub fn decode(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_PACKET_BYTES {
            return Err(PacketError::Size);
        }
        if &bytes[..4] != MAGIC_LAN {
            return Err(PacketError::Magic);
        }
        let kind = match bytes[4] {
            1 => Kind::Announce,
            2 => Kind::Query,
            _ => return Err(PacketError::Kind),
        };
        let tcp_port = u16::from_be_bytes([bytes[5], bytes[6]]);
        let n = usize::from(bytes[7]);
        if n == 0 || n > MAX_TOKENS {
            return Err(PacketError::Count);
        }
        if bytes.len() != HEADER_BYTES + TOKEN_BYTES * n {
            return Err(PacketError::Size);
        }
        let tokens = bytes[HEADER_BYTES..]
            .chunks_exact(TOKEN_BYTES)
            .map(|chunk| {
                let mut token = [0u8; TOKEN_BYTES];
                token.copy_from_slice(chunk);
                token
            })
            .collect();
        Ok(Self {
            kind,
            tcp_port,
            tokens,
        })
    }
}

/// The three tokens of `repo` around `hour`: previous, current, next.
#[must_use]
pub fn tokens_for(repo: &RepoId, hour: u64) -> [Token; TOKENS_PER_REPO] {
    [
        lan_token(repo, hour.saturating_sub(1)),
        lan_token(repo, hour),
        lan_token(repo, hour.saturating_add(1)),
    ]
}

/// Build the packets that carry `repos`, five repos per packet.
#[must_use]
pub fn build_packets(kind: Kind, tcp_port: u16, repos: &[RepoId], hour: u64) -> Vec<LanPacket> {
    repos
        .chunks(REPOS_PER_PACKET)
        .map(|chunk| LanPacket {
            kind,
            tcp_port,
            tokens: chunk
                .iter()
                .flat_map(|repo| tokens_for(repo, hour))
                .collect(),
        })
        .collect()
}

/// LAN-1: the repos in `known` that any token of `packet` matches, for the
/// previous, current and next hour of our own clock.
#[must_use]
pub fn match_repos(packet: &LanPacket, known: &[RepoId], hour: u64) -> Vec<RepoId> {
    known
        .iter()
        .filter(|repo| {
            let ours = tokens_for(repo, hour);
            packet
                .tokens
                .iter()
                .any(|token| ours.iter().any(|mine| mine == token))
        })
        .copied()
        .collect()
}

/// LAN-3 per-source limiter: at most 20 packets in any one-second window.
#[derive(Debug, Default)]
pub struct RateLimiter {
    sources: HashMap<IpAddr, Window>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

impl RateLimiter {
    /// An empty limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Count one packet from `source` at `now`. Returns `false` if it is over
    /// the limit and must be dropped.
    pub fn allow(&mut self, source: IpAddr, now: Instant) -> bool {
        if self.sources.len() >= RATE_LIMIT_SOURCES && !self.sources.contains_key(&source) {
            self.sources
                .retain(|_, window| now.saturating_duration_since(window.start) < SECOND);
            if self.sources.len() >= RATE_LIMIT_SOURCES {
                return false;
            }
        }
        let window = self.sources.entry(source).or_insert(Window {
            start: now,
            count: 0,
        });
        if now.saturating_duration_since(window.start) >= SECOND {
            *window = Window {
                start: now,
                count: 0,
            };
        }
        window.count = window.count.saturating_add(1);
        window.count <= RATE_LIMIT_PER_SEC
    }
}

const SECOND: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(byte: u8) -> RepoId {
        RepoId::from_bytes([byte; 32])
    }

    #[test]
    fn round_trip() {
        let packet = LanPacket {
            kind: Kind::Announce,
            tcp_port: 42424,
            tokens: tokens_for(&repo(1), 490_000).to_vec(),
        };
        let bytes = packet.encode().unwrap();
        assert_eq!(bytes.len(), HEADER_BYTES + 3 * TOKEN_BYTES);
        assert_eq!(&bytes[..4], b"SWG1");
        assert_eq!(bytes[4], 1);
        assert_eq!(&bytes[5..7], &42424u16.to_be_bytes());
        assert_eq!(bytes[7], 3);
        assert_eq!(LanPacket::decode(&bytes).unwrap(), packet);
    }

    #[test]
    fn query_kind_round_trips() {
        let packet = LanPacket {
            kind: Kind::Query,
            tcp_port: 1,
            tokens: vec![[9u8; 16]; MAX_TOKENS],
        };
        let bytes = packet.encode().unwrap();
        assert_eq!(LanPacket::decode(&bytes).unwrap(), packet);
    }

    #[test]
    fn encode_rejects_bad_counts() {
        let mut packet = LanPacket {
            kind: Kind::Query,
            tcp_port: 1,
            tokens: Vec::new(),
        };
        assert!(packet.encode().is_none());
        packet.tokens = vec![[0u8; 16]; MAX_TOKENS + 1];
        assert!(packet.encode().is_none());
    }

    #[test]
    fn wrong_magic() {
        let mut bytes = build_packets(Kind::Announce, 7, &[repo(2)], 5)[0]
            .encode()
            .unwrap();
        bytes[0] = b'X';
        assert_eq!(LanPacket::decode(&bytes), Err(PacketError::Magic));
    }

    #[test]
    fn wrong_kind() {
        let mut bytes = build_packets(Kind::Announce, 7, &[repo(2)], 5)[0]
            .encode()
            .unwrap();
        bytes[4] = 3;
        assert_eq!(LanPacket::decode(&bytes), Err(PacketError::Kind));
    }

    #[test]
    fn bad_count() {
        let mut bytes = build_packets(Kind::Announce, 7, &[repo(2)], 5)[0]
            .encode()
            .unwrap();
        bytes[7] = 0;
        assert_eq!(LanPacket::decode(&bytes), Err(PacketError::Count));
        bytes[7] = 17;
        assert_eq!(LanPacket::decode(&bytes), Err(PacketError::Count));
    }

    #[test]
    fn size_must_match_count() {
        let bytes = build_packets(Kind::Announce, 7, &[repo(2)], 5)[0]
            .encode()
            .unwrap();
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(LanPacket::decode(&longer), Err(PacketError::Size));
        assert_eq!(
            LanPacket::decode(&bytes[..bytes.len() - 1]),
            Err(PacketError::Size)
        );
        assert_eq!(LanPacket::decode(&bytes[..7]), Err(PacketError::Size));
    }

    #[test]
    fn oversize() {
        let mut bytes = vec![0u8; MAX_PACKET_BYTES + 1];
        bytes[..4].copy_from_slice(b"SWG1");
        bytes[4] = 1;
        bytes[7] = 16;
        assert_eq!(LanPacket::decode(&bytes), Err(PacketError::Size));
    }

    #[test]
    fn packets_hold_five_repos() {
        let repos: Vec<RepoId> = (0..11).map(repo).collect();
        let packets = build_packets(Kind::Announce, 9, &repos, 100);
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0].tokens.len(), 15);
        assert_eq!(packets[2].tokens.len(), 3);
        for packet in &packets {
            assert!(packet.encode().unwrap().len() <= MAX_PACKET_BYTES);
        }
    }

    #[test]
    fn matching_tolerates_one_hour_of_skew() {
        let target = repo(3);
        let known = [repo(4), target, repo(5)];
        let packet = &build_packets(Kind::Announce, 9, &[target], 1000)[0];
        for hour in [998, 999, 1000, 1001, 1002] {
            assert_eq!(match_repos(packet, &known, hour), vec![target], "{hour}");
        }
        assert!(match_repos(packet, &known, 1003).is_empty());
        assert!(match_repos(packet, &known, 997).is_empty());
    }

    #[test]
    fn unknown_repos_do_not_match() {
        let packet = &build_packets(Kind::Query, 9, &[repo(6)], 10)[0];
        assert!(match_repos(packet, &[repo(7), repo(8)], 10).is_empty());
    }

    #[test]
    fn tokens_never_contain_the_repo_id() {
        let target = repo(0x5a);
        let bytes = build_packets(Kind::Announce, 9, &[target], 10)[0]
            .encode()
            .unwrap();
        let id = target.as_bytes();
        assert!(!bytes
            .windows(8)
            .any(|window| id.windows(8).any(|w| w == window)));
    }

    #[test]
    fn rate_limit_drops_above_twenty_per_second() {
        let mut limiter = RateLimiter::new();
        let source: IpAddr = "10.0.0.7".parse().unwrap();
        let other: IpAddr = "10.0.0.8".parse().unwrap();
        let start = Instant::now();
        for i in 0..20 {
            assert!(limiter.allow(source, start + Duration::from_millis(i)));
        }
        assert!(!limiter.allow(source, start + Duration::from_millis(500)));
        assert!(limiter.allow(other, start + Duration::from_millis(500)));
        assert!(limiter.allow(source, start + Duration::from_millis(1001)));
    }

    #[test]
    fn rate_limit_bounds_memory() {
        let mut limiter = RateLimiter::new();
        let start = Instant::now();
        for i in 0..RATE_LIMIT_SOURCES as u32 {
            let ip = IpAddr::from(i.to_be_bytes());
            assert!(limiter.allow(ip, start));
        }
        assert!(!limiter.allow("192.0.2.1".parse().unwrap(), start));
        let later = start + Duration::from_secs(2);
        assert!(limiter.allow("192.0.2.1".parse().unwrap(), later));
    }
}
