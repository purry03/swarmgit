//! The set of verified relays and rendezvous placement (relay design spec
//! REL-24, REL-26, REL-28, REL-31).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use swarm_core::consts::CTX_RELAY_PLACE;
use swarm_core::crypto::{derive_key, random_bytes};
use swarm_net::PeerAddr;

/// Most verified relays kept (REL-26).
pub const MAX_RELAYS: usize = 256;
/// Home relays of a node (REL-28).
pub const HOME_RELAYS: usize = 3;
/// A candidate that failed its probe is not retried for this long (REL-24).
pub const REJECT_FOR: Duration = Duration::from_secs(10 * 60);
/// A known relay is dropped after this many failures in a row.
const MAX_FAILURES: u32 = 3;
/// Rejected candidates remembered.
const MAX_REJECTED: usize = 1024;

/// Rendezvous weight of `relay` for `node`.
#[must_use]
pub fn weight(node: &[u8; 32], relay: SocketAddr) -> [u8; 32] {
    let mut material = node.to_vec();
    material.extend_from_slice(&PeerAddr::Tcp(relay).encode(None));
    derive_key(CTX_RELAY_PLACE, &material)
}

/// The `n` relays with the highest weight for `node`, best first.
#[must_use]
pub fn closest(
    node: &[u8; 32],
    relays: impl IntoIterator<Item = SocketAddr>,
    n: usize,
) -> Vec<SocketAddr> {
    let mut weighted: Vec<([u8; 32], SocketAddr)> = relays
        .into_iter()
        .map(|relay| (weight(node, relay), relay))
        .collect();
    weighted.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    weighted.dedup_by_key(|entry| entry.1);
    weighted
        .into_iter()
        .take(n)
        .map(|(_, relay)| relay)
        .collect()
}

/// One verified relay and its track record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    /// Where it answered.
    pub addr: SocketAddr,
    /// The ID it reported.
    pub relay_id: [u8; 32],
    /// Probes and splices that worked.
    pub successes: u32,
    /// Failures since the last success.
    pub failures: u32,
    /// Last round-trip time in milliseconds.
    pub rtt_ms: u32,
    /// A domain name, checked to point here. Not saved: the next probe
    /// learns it again.
    pub name: Option<String>,
}

impl Known {
    /// Healthy relays first, then the better record, then the faster.
    fn rank(&self) -> (u32, u32, u32) {
        (self.failures, u32::MAX - self.successes, self.rtt_ms)
    }
}

/// Verified relays, capped at [`MAX_RELAYS`].
#[derive(Debug, Default)]
pub struct RelaySet {
    relays: HashMap<SocketAddr, Known>,
    rejected: HashMap<SocketAddr, Instant>,
}

impl RelaySet {
    /// Number of relays known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.relays.len()
    }

    /// True when no relay is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.relays.is_empty()
    }

    /// True when `addr` is a known relay.
    #[must_use]
    pub fn contains(&self, addr: SocketAddr) -> bool {
        self.relays.contains_key(&addr)
    }

    /// True while a failed candidate is left alone.
    #[must_use]
    pub fn rejected(&self, addr: SocketAddr) -> bool {
        self.rejected
            .get(&addr)
            .is_some_and(|at| at.elapsed() < REJECT_FOR)
    }

    /// Record a probe that worked.
    pub fn verified(&mut self, addr: SocketAddr, relay_id: [u8; 32], rtt: Duration) {
        self.rejected.remove(&addr);
        // One relay answering on two addresses counts once.
        self.relays
            .retain(|known, relay| *known == addr || relay.relay_id != relay_id);
        let rtt_ms = u32::try_from(rtt.as_millis()).unwrap_or(u32::MAX);
        if let Some(relay) = self.relays.get_mut(&addr) {
            relay.relay_id = relay_id;
            relay.successes = relay.successes.saturating_add(1);
            relay.failures = 0;
            relay.rtt_ms = rtt_ms;
            return;
        }
        self.insert(Known {
            addr,
            relay_id,
            successes: 1,
            failures: 0,
            rtt_ms,
            name: None,
        });
    }

    /// Set or clear the checked domain name of a known relay.
    pub fn set_name(&mut self, addr: SocketAddr, name: Option<String>) {
        if let Some(relay) = self.relays.get_mut(&addr) {
            relay.name = name;
        }
    }

    /// The checked domain name of `addr`.
    #[must_use]
    pub fn name_of(&self, addr: SocketAddr) -> Option<String> {
        self.relays.get(&addr)?.name.clone()
    }

    /// The address of the relay with the checked domain name `name`.
    #[must_use]
    pub fn addr_named(&self, name: &str) -> Option<SocketAddr> {
        self.relays
            .values()
            .find(|relay| relay.name.as_deref() == Some(name))
            .map(|relay| relay.addr)
    }

    /// Add a saved or verified entry. A full set only makes room by dropping a
    /// relay that is currently failing, so newcomers cannot flush out relays
    /// with a record (REL-31).
    pub fn insert(&mut self, relay: Known) {
        if self.relays.len() >= MAX_RELAYS && !self.relays.contains_key(&relay.addr) {
            let worst = self
                .relays
                .values()
                .max_by_key(|known| known.rank())
                .filter(|known| known.failures > 0)
                .map(|known| known.addr);
            let Some(worst) = worst else { return };
            self.relays.remove(&worst);
        }
        self.relays.insert(relay.addr, relay);
    }

    /// Record a failed probe or connection. A candidate that was never
    /// verified is set aside; a known relay is dropped after a few in a row.
    pub fn failed(&mut self, addr: SocketAddr) {
        let drop_it = match self.relays.get_mut(&addr) {
            Some(relay) => {
                relay.failures = relay.failures.saturating_add(1);
                relay.failures >= MAX_FAILURES
            }
            None => true,
        };
        if drop_it {
            self.relays.remove(&addr);
            self.reject(addr);
        }
    }

    /// Leave `addr` alone for a while.
    pub fn reject(&mut self, addr: SocketAddr) {
        if self.rejected.len() >= MAX_REJECTED {
            self.rejected.retain(|_, at| at.elapsed() < REJECT_FOR);
        }
        if self.rejected.len() < MAX_REJECTED {
            self.rejected.insert(addr, Instant::now());
        }
    }

    /// All relays, best first.
    #[must_use]
    pub fn ranked(&self) -> Vec<Known> {
        let mut out: Vec<Known> = self.relays.values().cloned().collect();
        out.sort_by_key(|relay| (relay.rank(), relay.addr));
        out
    }

    fn healthy(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.relays
            .values()
            .filter(|relay| relay.failures == 0)
            .map(|relay| relay.addr)
    }

    /// A random sample of healthy relays (REL-26).
    #[must_use]
    pub fn sample(&self, n: usize) -> Vec<SocketAddr> {
        let seed = random_bytes::<32>().unwrap_or_default();
        closest(&seed, self.healthy(), n)
    }

    /// The healthy relays closest to `node` (REL-28).
    #[must_use]
    pub fn closest(&self, node: &[u8; 32], n: usize) -> Vec<SocketAddr> {
        closest(node, self.healthy(), n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u16) -> SocketAddr {
        SocketAddr::from(([192, 0, 2, 1], n))
    }

    fn id(n: u16) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[..2].copy_from_slice(&n.to_be_bytes());
        id
    }

    #[test]
    fn placement_is_stable_and_depends_on_the_node() {
        let relays: Vec<SocketAddr> = (1..=20).map(addr).collect();
        let first = closest(&[1; 32], relays.clone(), 3);
        let mut reversed = relays.clone();
        reversed.reverse();
        assert_eq!(first, closest(&[1; 32], reversed, 3));
        assert_eq!(first.len(), 3);
        assert_ne!(first, closest(&[2; 32], relays.clone(), 3));
        // Removing a relay that was not chosen changes nothing.
        let other = relays.iter().find(|relay| !first.contains(relay)).copied();
        let fewer = relays.into_iter().filter(|relay| Some(*relay) != other);
        assert_eq!(first, closest(&[1; 32], fewer, 3));
    }

    #[test]
    fn failures_drop_relays_and_reject_candidates() {
        let mut set = RelaySet::default();
        set.failed(addr(1));
        assert!(set.rejected(addr(1)));
        set.verified(addr(2), id(2), Duration::from_millis(5));
        set.failed(addr(2));
        assert!(set.contains(addr(2)) && set.closest(&[0; 32], 3).is_empty());
        set.verified(addr(2), id(2), Duration::from_millis(5));
        assert_eq!(set.closest(&[0; 32], 3), vec![addr(2)]);
        for _ in 0..MAX_FAILURES {
            set.failed(addr(2));
        }
        assert!(set.is_empty() && set.rejected(addr(2)));
    }

    #[test]
    fn the_set_is_capped_and_keeps_relays_with_a_record() {
        let mut set = RelaySet::default();
        for n in 0..MAX_RELAYS as u16 {
            set.verified(addr(n + 1), id(n + 1), Duration::from_millis(1));
        }
        set.verified(addr(1000), id(1000), Duration::ZERO);
        assert_eq!(set.len(), MAX_RELAYS);
        assert!(!set.contains(addr(1000)));
        set.failed(addr(7));
        set.verified(addr(1000), id(1000), Duration::ZERO);
        assert!(set.contains(addr(1000)) && !set.contains(addr(7)));
    }

    #[test]
    fn one_relay_on_two_addresses_counts_once() {
        let mut set = RelaySet::default();
        set.verified(addr(1), id(9), Duration::ZERO);
        set.verified(addr(2), id(9), Duration::ZERO);
        assert_eq!(set.sample(8), vec![addr(2)]);
    }
}
