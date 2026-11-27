//! Inbound limits (spec 7.4): connection caps, a connection rate, the
//! persisted ban list, and the upload rate (WANT-5).
//!
//! Loopback is never banned or rate limited: only local tools connect from
//! there. Visitors through our hidden service arrive on a listener of their
//! own. Tor makes them indistinguishable, so they cannot be banned one by one;
//! together they get a configurable connection cap and rate, which leaves the
//! other slots for direct peers.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use swarm_core::ids::NodeId;
use swarm_store::Db;
use tokio::time::Instant;

/// Concurrent inbound connections.
pub const MAX_TOTAL_CONNECTIONS: usize = 64;
/// Concurrent inbound connections from one address.
pub const MAX_PER_IP: usize = 4;
/// New connections one address may open within [`RATE_WINDOW`].
pub const MAX_NEW_PER_WINDOW: usize = 60;
/// Window of the connection rate and of violation counting.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Violations that earn a ban.
pub const MAX_VIOLATIONS: usize = 3;
/// How long a ban lasts, and how long a violation is remembered.
pub const BAN_SECONDS: i64 = 600;
/// Addresses tracked before stale entries are dropped.
const MAX_TRACKED: usize = 4096;

/// Why a connection was turned away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The address is on the ban list.
    Banned,
    /// Too many connections are open.
    Busy,
    /// The address connects too often.
    TooFast,
}

/// Limits shared by all visitors through the hidden service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnionLimits {
    /// Connections open at once.
    pub max_connections: usize,
    /// New connections within [`RATE_WINDOW`].
    pub max_per_minute: usize,
}

impl Default for OnionLimits {
    fn default() -> Self {
        // A quarter of the slots; each visitor may hold more than one.
        Self {
            max_connections: MAX_TOTAL_CONNECTIONS / 4,
            max_per_minute: 120,
        }
    }
}

/// Who a connection counts against.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    Ip(IpAddr),
    Onion,
    /// A visitor through a relay, known only by its node ID (REL-15).
    Node([u8; 32]),
}

#[derive(Default)]
struct Seen {
    open: HashMap<Source, usize>,
    total: usize,
    recent: HashMap<Source, VecDeque<Instant>>,
    violations: HashMap<Source, VecDeque<Instant>>,
}

/// Inbound limits, shared by every listener.
pub struct Limits {
    seen: Mutex<Seen>,
    db: Arc<Mutex<Db>>,
    onion: OnionLimits,
}

/// The ban list key of a node seen through a relay.
fn node_key(node: [u8; 32]) -> String {
    format!("node:{}", NodeId::from_bytes(node).to_hex())
}

/// An admitted connection; dropping it frees the slot.
pub struct Guard {
    limits: Arc<Limits>,
    source: Source,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut seen = self.limits.lock();
        seen.total = seen.total.saturating_sub(1);
        if let Some(count) = seen.open.get_mut(&self.source) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                seen.open.remove(&self.source);
            }
        }
    }
}

fn expire(times: &mut VecDeque<Instant>, now: Instant, keep: Duration) {
    while times
        .front()
        .is_some_and(|at| now.duration_since(*at) >= keep)
    {
        times.pop_front();
    }
}

impl Limits {
    /// Limits backed by the ban list in `db`.
    #[must_use]
    pub fn new(db: Arc<Mutex<Db>>, onion: OnionLimits) -> Arc<Self> {
        if let Ok(db) = db.lock() {
            let _ = db.prune_bans(super::now());
        }
        Arc::new(Self {
            seen: Mutex::new(Seen::default()),
            db,
            onion,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn listed(&self, key: &str) -> bool {
        self.db
            .lock()
            .ok()
            .and_then(|db| db.is_banned(key, super::now()).ok())
            .unwrap_or(false)
    }

    /// Admit a connection from `ip`, or say why not.
    pub fn enter(self: &Arc<Self>, ip: IpAddr) -> Result<Guard, Refused> {
        let ip = ip.to_canonical();
        let local = ip.is_loopback();
        if !local && self.listed(&ip.to_string()) {
            return Err(Refused::Banned);
        }
        let rate = (!local).then_some(MAX_NEW_PER_WINDOW);
        self.admit(Source::Ip(ip), MAX_PER_IP, rate)
    }

    /// Admit a visitor through the hidden service.
    pub fn enter_onion(self: &Arc<Self>) -> Result<Guard, Refused> {
        self.admit(
            Source::Onion,
            self.onion.max_connections,
            Some(self.onion.max_per_minute),
        )
    }

    fn admit(
        self: &Arc<Self>,
        source: Source,
        max_open: usize,
        rate: Option<usize>,
    ) -> Result<Guard, Refused> {
        let mut seen = self.lock();
        if seen.total >= MAX_TOTAL_CONNECTIONS
            || seen.open.get(&source).copied().unwrap_or(0) >= max_open
        {
            return Err(Refused::Busy);
        }
        if let Some(max_new) = rate {
            let now = Instant::now();
            if seen.recent.len() >= MAX_TRACKED {
                seen.recent.retain(|_, times| {
                    expire(times, now, RATE_WINDOW);
                    !times.is_empty()
                });
                if seen.recent.len() >= MAX_TRACKED && !seen.recent.contains_key(&source) {
                    return Err(Refused::Busy);
                }
            }
            let times = seen.recent.entry(source).or_default();
            expire(times, now, RATE_WINDOW);
            if times.len() >= max_new {
                return Err(Refused::TooFast);
            }
            times.push_back(now);
        }
        seen.total += 1;
        *seen.open.entry(source).or_insert(0) += 1;
        drop(seen);
        Ok(Guard {
            limits: Arc::clone(self),
            source,
        })
    }

    /// Admit a session that came through a relay. It has no address of its
    /// own, so it counts against its node ID (REL-15).
    pub fn enter_node(self: &Arc<Self>, node: [u8; 32]) -> Result<Guard, Refused> {
        if self.listed(&node_key(node)) {
            return Err(Refused::Banned);
        }
        self.admit(Source::Node(node), MAX_PER_IP, Some(MAX_NEW_PER_WINDOW))
    }

    /// Count a protocol violation. Returns true when it earned a ban.
    pub fn violation(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        if ip.is_loopback() {
            return false;
        }
        self.strike(Source::Ip(ip), &ip.to_string())
    }

    /// Count a violation by a node seen through a relay.
    pub fn violation_node(&self, node: [u8; 32]) -> bool {
        self.strike(Source::Node(node), &node_key(node))
    }

    fn strike(&self, source: Source, key: &str) -> bool {
        let keep = Duration::from_secs(BAN_SECONDS.unsigned_abs());
        let now = Instant::now();
        {
            let mut seen = self.lock();
            if seen.violations.len() >= MAX_TRACKED {
                seen.violations.retain(|_, times| {
                    expire(times, now, keep);
                    !times.is_empty()
                });
                if seen.violations.len() >= MAX_TRACKED {
                    seen.violations.clear();
                }
            }
            let times = seen.violations.entry(source).or_default();
            expire(times, now, keep);
            times.push_back(now);
            if times.len() < MAX_VIOLATIONS {
                return false;
            }
            seen.violations.remove(&source);
        }
        if let Ok(db) = self.db.lock() {
            let _ = db.ban(key, super::now() + BAN_SECONDS);
        }
        tracing::warn!(peer = %key, "banned for 10 minutes after repeated protocol violations");
        true
    }
}

/// Paces uploads to a rate shared by every peer.
pub struct Throttle {
    bytes_per_second: u64,
    free_at: Mutex<Option<Instant>>,
}

impl Throttle {
    /// `kbps` is kilobits per second; 0 means no limit.
    #[must_use]
    pub fn new(kbps: u64) -> Self {
        Self {
            bytes_per_second: kbps.saturating_mul(1000) / 8,
            free_at: Mutex::new(None),
        }
    }

    /// Largest chunk to send at once: about a quarter second of the rate.
    #[must_use]
    pub fn chunk(&self, unlimited: usize) -> usize {
        if self.bytes_per_second == 0 {
            return unlimited;
        }
        usize::try_from(self.bytes_per_second / 4)
            .unwrap_or(unlimited)
            .clamp(1024, unlimited)
    }

    /// Wait until `bytes` more may be sent.
    pub async fn take(&self, bytes: usize) {
        if self.bytes_per_second == 0 {
            return;
        }
        let cost = Duration::from_secs_f64(bytes as f64 / self.bytes_per_second as f64);
        let start = {
            let mut free_at = self
                .free_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            let start = free_at.map_or(now, |at| at.max(now));
            *free_at = Some(start + cost);
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Arc<Mutex<Db>>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("swarm.db")).unwrap();
        (dir, Arc::new(Mutex::new(db)))
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn three_violations_ban_the_address_across_restarts() {
        let (_dir, db) = db();
        let limits = Limits::new(Arc::clone(&db), OnionLimits::default());
        let bad = ip("198.51.100.7");
        assert!(!limits.violation(bad));
        assert!(!limits.violation(bad));
        assert!(limits.enter(bad).is_ok());
        assert!(limits.violation(bad));
        assert_eq!(limits.enter(bad).err(), Some(Refused::Banned));
        // The mapped form of the same address is the same address.
        assert_eq!(
            limits.enter(ip("::ffff:198.51.100.7")).err(),
            Some(Refused::Banned)
        );
        let restarted = Limits::new(db, OnionLimits::default());
        assert_eq!(restarted.enter(bad).err(), Some(Refused::Banned));
        assert!(restarted.enter(ip("198.51.100.8")).is_ok());
    }

    #[test]
    fn caps_and_rate_apply_per_address_and_spare_loopback() {
        let (_dir, db) = db();
        let limits = Limits::new(db, OnionLimits::default());
        let peer = ip("203.0.113.5");
        let held: Vec<_> = (0..MAX_PER_IP)
            .map(|_| limits.enter(peer).unwrap())
            .collect();
        assert_eq!(limits.enter(peer).err(), Some(Refused::Busy));
        drop(held);
        for _ in MAX_PER_IP..MAX_NEW_PER_WINDOW {
            drop(limits.enter(peer).unwrap());
        }
        assert_eq!(limits.enter(peer).err(), Some(Refused::TooFast));
        let local = ip("127.0.0.1");
        for _ in 0..MAX_NEW_PER_WINDOW * 2 {
            assert!(!limits.violation(local));
            drop(limits.enter(local).unwrap());
        }
    }

    #[test]
    fn onion_visitors_leave_slots_for_everyone_else() {
        let (_dir, db) = db();
        let onion = OnionLimits {
            max_connections: 6,
            max_per_minute: 8,
        };
        let limits = Limits::new(db, onion);
        let held: Vec<_> = (0..6).map(|_| limits.enter_onion().unwrap()).collect();
        assert_eq!(limits.enter_onion().err(), Some(Refused::Busy));
        // Direct and local peers still get in.
        assert!(limits.enter(ip("203.0.113.5")).is_ok());
        assert!(limits.enter(ip("127.0.0.1")).is_ok());
        drop(held);
        for _ in 6..8 {
            drop(limits.enter_onion().unwrap());
        }
        assert_eq!(limits.enter_onion().err(), Some(Refused::TooFast));
    }

    #[tokio::test(start_paused = true)]
    async fn the_throttle_spreads_uploads_over_time() {
        let throttle = Throttle::new(80); // 10,000 bytes per second.
        let start = Instant::now();
        for _ in 0..5 {
            throttle.take(10_000).await;
        }
        // The first leaves at once; each later one waits for the one before.
        assert_eq!(start.elapsed(), Duration::from_secs(4));
        assert_eq!(throttle.chunk(262_144), 2500);
        assert_eq!(Throttle::new(0).chunk(262_144), 262_144);
    }
}
