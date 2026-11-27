//! Per-address and per-node limits (relay design spec REL-19).
//!
//! Everything is held in memory and bounded. IPv6 callers are counted by /64,
//! since one host holds a whole one. Loopback is spared unless
//! [`crate::Limits::limit_loopback`] is set.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::time::Instant;

/// Window of the connect rate limit.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Window of the per-node byte quota.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// Addresses and nodes tracked at once.
const MAX_TRACKED: usize = 10_000;

/// The key an address is counted under.
pub(crate) fn address_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) if !v6.is_loopback() => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        other => other,
    }
}

#[derive(Default)]
struct Seen {
    connects: HashMap<IpAddr, VecDeque<Instant>>,
    splices: HashMap<IpAddr, usize>,
    daily: HashMap<[u8; 32], (Instant, u64)>,
}

#[derive(Default)]
pub(crate) struct Quotas {
    seen: Mutex<Seen>,
}

impl Quotas {
    fn lock(&self) -> MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Count a connect attempt. False when the address made too many.
    pub(crate) fn connect(&self, ip: IpAddr, max_per_minute: usize) -> bool {
        let now = Instant::now();
        let mut seen = self.lock();
        let recent = |times: &VecDeque<Instant>| {
            times
                .back()
                .is_some_and(|at| now.duration_since(*at) < RATE_WINDOW)
        };
        if seen.connects.len() >= MAX_TRACKED && !seen.connects.contains_key(&ip) {
            seen.connects.retain(|_, times| recent(times));
            if seen.connects.len() >= MAX_TRACKED {
                return false;
            }
        }
        let times = seen.connects.entry(ip).or_default();
        while times
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
        {
            times.pop_front();
        }
        if times.len() >= max_per_minute {
            return false;
        }
        times.push_back(now);
        true
    }

    /// Take one of the address's splice places, freed when the guard drops.
    pub(crate) fn splice(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<SpliceGuard> {
        let mut seen = self.lock();
        let open = seen.splices.get(&ip).copied().unwrap_or(0);
        if open >= max {
            return None;
        }
        seen.splices.insert(ip, open + 1);
        drop(seen);
        Some(SpliceGuard {
            quotas: Arc::clone(self),
            ip,
        })
    }

    /// Add `bytes` to what was carried for `node` today. False once the node
    /// is over `max`.
    pub(crate) fn carry(&self, node: [u8; 32], bytes: u64, max: u64) -> bool {
        let now = Instant::now();
        let mut seen = self.lock();
        if seen.daily.len() >= MAX_TRACKED && !seen.daily.contains_key(&node) {
            seen.daily
                .retain(|_, (start, _)| now.duration_since(*start) < DAY);
            if seen.daily.len() >= MAX_TRACKED {
                return false;
            }
        }
        let entry = seen.daily.entry(node).or_insert((now, 0));
        if now.duration_since(entry.0) >= DAY {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(bytes);
        entry.1 < max
    }
}

/// One splice place held by an address.
pub(crate) struct SpliceGuard {
    quotas: Arc<Quotas>,
    ip: IpAddr,
}

impl Drop for SpliceGuard {
    fn drop(&mut self) {
        let mut seen = self.quotas.lock();
        if let Some(open) = seen.splices.get_mut(&self.ip) {
            *open = open.saturating_sub(1);
            if *open == 0 {
                seen.splices.remove(&self.ip);
            }
        }
    }
}

/// Paces one splice to a byte rate. A rate of 0 means no limit.
pub(crate) struct Pacer {
    rate: u64,
    free_at: Mutex<Instant>,
}

impl Pacer {
    pub(crate) fn new(rate: u64) -> Self {
        Self {
            rate,
            free_at: Mutex::new(Instant::now()),
        }
    }

    pub(crate) async fn take(&self, bytes: usize) {
        if self.rate == 0 {
            return;
        }
        let cost = Duration::from_secs_f64(bytes as f64 / self.rate as f64);
        let start = {
            let mut free_at = self
                .free_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let start = (*free_at).max(Instant::now());
            *free_at = start + cost;
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn connect_rate_splice_places_and_daily_bytes() {
        let quotas = Arc::new(Quotas::default());
        let ip = address_key("198.51.100.1".parse().unwrap());
        assert!(quotas.connect(ip, 2) && quotas.connect(ip, 2));
        assert!(!quotas.connect(ip, 2));
        tokio::time::advance(RATE_WINDOW).await;
        assert!(quotas.connect(ip, 2));

        let first = quotas.splice(ip, 1).unwrap();
        assert!(quotas.splice(ip, 1).is_none());
        drop(first);
        assert!(quotas.splice(ip, 1).is_some());

        assert!(quotas.carry([1; 32], 60, 100));
        assert!(!quotas.carry([1; 32], 60, 100));
        assert!(quotas.carry([2; 32], 60, 100));
        tokio::time::advance(DAY).await;
        assert!(quotas.carry([1; 32], 60, 100));
    }

    #[test]
    fn one_v6_network_is_one_caller() {
        let key = |text: &str| address_key(text.parse().unwrap());
        assert_eq!(key("2001:db8:1:2::1"), key("2001:db8:1:2:ffff::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:192.0.2.1"), key("192.0.2.1"));
    }
}
