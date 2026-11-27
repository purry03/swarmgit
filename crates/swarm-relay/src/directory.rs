//! Finding relays and finding nodes among them (relay design spec 5.4).
//!
//! Used by both sides of the network: a relay keeps one to answer `Where` and
//! `RGetRelays`, and a client keeps one to pick home relays and to find a
//! node it only knows by ID.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use swarm_core::consts::CTX_RELAY_DIR;
use swarm_core::crypto::derive_key;
use swarm_core::ids::NodeId;
use swarm_disc::dht::{Dht, MainlineDht};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::client;
use crate::name;
use crate::proto::{MAX_RELAYS_REPLY, VERSION};
use crate::set::{closest, Known, RelaySet, HOME_RELAYS};

/// Probes running at once.
const PROBES: usize = 16;
/// New candidates probed in one round.
const MAX_CANDIDATES: usize = 64;
/// Known relays asked for more in one round.
const GOSSIP_FANOUT: usize = 8;
/// Referral hops followed (REL-29).
const MAX_HOPS: usize = 2;

/// The infohash relays announce themselves under (REL-23).
#[must_use]
pub fn directory_topic() -> [u8; 20] {
    let mut out = [0u8; 20];
    out.copy_from_slice(&derive_key(CTX_RELAY_DIR, b"v1")[..20]);
    out
}

/// How a directory behaves.
#[derive(Clone, Debug)]
pub struct DirectoryConfig {
    /// Accept loopback and private addresses from the DHT and from gossip.
    /// Off on the open network, where they would point a node at its own LAN.
    pub allow_private: bool,
    /// Time for one probe or request.
    pub probe_timeout: Duration,
    /// Least time between dial-back checks asked by one address.
    pub dial_back_spacing: Duration,
}

impl Default for DirectoryConfig {
    fn default() -> Self {
        Self {
            allow_private: false,
            probe_timeout: Duration::from_secs(5),
            dial_back_spacing: Duration::from_secs(10),
        }
    }
}

/// True when `ip` is on this host or a private network.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                // Carrier-grade NAT, 100.64.0.0/10.
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            // Loopback, unique local fc00::/7 and link local fe80::/10.
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    }
}

/// True when an address learned from others may be dialed.
#[must_use]
pub fn usable(addr: SocketAddr, allow_private: bool) -> bool {
    let ip = addr.ip().to_canonical();
    let broadcast = matches!(ip, IpAddr::V4(ip) if ip.is_broadcast());
    addr.port() != 0
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !broadcast
        && (allow_private || !is_private(ip))
}

/// A relay set with the means to fill it.
pub struct Directory {
    set: Mutex<RelaySet>,
    dht: Option<MainlineDht>,
    own_id: Option<[u8; 32]>,
    config: DirectoryConfig,
}

impl Directory {
    /// An empty directory. `own_id` keeps a relay from listing itself.
    #[must_use]
    pub fn new(
        dht: Option<MainlineDht>,
        own_id: Option<[u8; 32]>,
        config: DirectoryConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            set: Mutex::new(RelaySet::default()),
            dht,
            own_id,
            config,
        })
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &DirectoryConfig {
        &self.config
    }

    /// The DHT node, when the directory has one.
    #[must_use]
    pub fn dht(&self) -> Option<&MainlineDht> {
        self.dht.as_ref()
    }

    fn set(&self) -> MutexGuard<'_, RelaySet> {
        self.set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Relays known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.set().len()
    }

    /// True when no relay is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.set().is_empty()
    }

    /// All relays, best first, to save or show.
    #[must_use]
    pub fn ranked(&self) -> Vec<Known> {
        self.set().ranked()
    }

    /// Restore saved relays (REL-27). They are probed again in the next round.
    pub fn load(&self, saved: impl IntoIterator<Item = Known>) {
        let mut set = self.set();
        for relay in saved {
            set.insert(relay);
        }
    }

    /// A random sample of healthy relays.
    #[must_use]
    pub fn sample(&self, n: usize) -> Vec<SocketAddr> {
        self.set().sample(n)
    }

    /// The healthy relays closest to `node`.
    #[must_use]
    pub fn closest(&self, node: &[u8; 32], n: usize) -> Vec<SocketAddr> {
        self.set().closest(node, n)
    }

    /// Record a relay that answered.
    pub fn verified(&self, addr: SocketAddr, relay_id: [u8; 32], rtt: Duration) {
        if self.own_id != Some(relay_id) {
            self.set().verified(addr, relay_id, rtt);
        }
    }

    /// The checked domain name of the relay at `addr`.
    #[must_use]
    pub fn name_of(&self, addr: SocketAddr) -> Option<String> {
        self.set().name_of(addr)
    }

    /// The address of the relay with the checked domain name `name`.
    #[must_use]
    pub fn addr_named(&self, name: &str) -> Option<SocketAddr> {
        self.set().addr_named(name)
    }

    /// Take the domain name a probed relay claims, if it points at it.
    async fn learn_name(&self, addr: SocketAddr, caps: &[String]) {
        let Some(claim) = name::claimed(caps) else {
            self.set().set_name(addr, None);
            return;
        };
        let check = tokio::time::timeout(self.config.probe_timeout, name::check(claim, addr));
        // A lookup that fails leaves what was learned before.
        if let Ok(Some(points_here)) = check.await {
            let claim = points_here.then(|| claim.to_string());
            self.set().set_name(addr, claim);
        }
    }

    /// Record a relay that did not.
    pub fn failed(&self, addr: SocketAddr) {
        self.set().failed(addr);
    }

    /// Probe one candidate (REL-24). Returns true when it is now trusted.
    pub async fn probe(&self, addr: SocketAddr) -> bool {
        let started = Instant::now();
        let answer =
            tokio::time::timeout(self.config.probe_timeout, client::info(&addr.to_string())).await;
        match answer {
            Ok(Ok(info))
                if info.version == VERSION
                    && info
                        .observed
                        .parse::<SocketAddr>()
                        .is_ok_and(|seen| usable(seen, true)) =>
            {
                if self.own_id == Some(info.relay_id) {
                    self.set().reject(addr);
                    return false;
                }
                self.set().verified(addr, info.relay_id, started.elapsed());
                self.learn_name(addr, &info.caps).await;
                true
            }
            _ => {
                self.set().failed(addr);
                false
            }
        }
    }

    /// Probe several candidates at once, skipping unusable and rejected ones.
    pub async fn probe_all(self: &Arc<Self>, candidates: impl IntoIterator<Item = SocketAddr>) {
        let mut seen = HashSet::new();
        let wanted: Vec<SocketAddr> = {
            let set = self.set();
            candidates
                .into_iter()
                .filter(|addr| usable(*addr, self.config.allow_private))
                .filter(|addr| !set.rejected(*addr) && seen.insert(*addr))
                .take(MAX_CANDIDATES)
                .collect()
        };
        let slots = Arc::new(Semaphore::new(PROBES));
        let mut tasks = JoinSet::new();
        for addr in wanted {
            let directory = Arc::clone(self);
            let slots = Arc::clone(&slots);
            tasks.spawn(async move {
                let _permit = slots.acquire().await;
                directory.probe(addr).await;
            });
        }
        while tasks.join_next().await.is_some() {}
    }

    /// Ask known relays for theirs and probe the new ones (REL-26).
    pub async fn gossip(self: &Arc<Self>) {
        let mut tasks = JoinSet::new();
        for relay in self.sample(GOSSIP_FANOUT) {
            let wait = self.config.probe_timeout;
            tasks.spawn(async move {
                let max = MAX_RELAYS_REPLY as u64;
                tokio::time::timeout(wait, client::get_relays(&relay.to_string(), max)).await
            });
        }
        let mut learned = Vec::new();
        while let Some(done) = tasks.join_next().await {
            if let Ok(Ok(Ok(addrs))) = done {
                learned.extend(addrs);
            }
        }
        let new: Vec<SocketAddr> = {
            let set = self.set();
            learned
                .into_iter()
                .filter(|addr| !set.contains(*addr))
                .collect()
        };
        self.probe_all(new).await;
    }

    /// One round: re-check known relays, read the DHT directory, then gossip.
    /// `extra` are configured or hinted relays (REL-27).
    pub async fn refresh(self: &Arc<Self>, extra: &[SocketAddr]) {
        let known: Vec<SocketAddr> = self.ranked().iter().map(|relay| relay.addr).collect();
        // Configured and known relays were chosen or checked before.
        let mut again = JoinSet::new();
        for addr in extra.iter().chain(&known).take(MAX_CANDIDATES).copied() {
            let directory = Arc::clone(self);
            again.spawn(async move {
                directory.probe(addr).await;
            });
        }
        while again.join_next().await.is_some() {}
        if let Some(dht) = &self.dht {
            let found = dht.get_peers(directory_topic()).await;
            let new: Vec<SocketAddr> = found
                .into_iter()
                .filter(|addr| !known.contains(addr))
                .collect();
            self.probe_all(new).await;
        }
        self.gossip().await;
    }

    /// The relays that hold `node` (REL-29): ask its home relays as this
    /// directory sees them, plus `extra`, then follow referrals.
    pub async fn locate(self: &Arc<Self>, node: NodeId, extra: &[SocketAddr]) -> Vec<SocketAddr> {
        let mut asked: HashSet<SocketAddr> = HashSet::new();
        let mut next = self.closest(node.as_bytes(), HOME_RELAYS);
        for relay in extra {
            if !next.contains(relay) {
                next.push(*relay);
            }
        }
        for _hop in 0..=MAX_HOPS {
            let mut tasks = JoinSet::new();
            for relay in next.drain(..) {
                if !asked.insert(relay) {
                    continue;
                }
                let wait = self.config.probe_timeout;
                tasks.spawn(async move {
                    let name = relay.to_string();
                    let reply = tokio::time::timeout(wait, client::where_is(&name, node));
                    (relay, reply.await)
                });
            }
            let mut found = Vec::new();
            let mut closer = Vec::new();
            while let Some(done) = tasks.join_next().await {
                let Ok((relay, reply)) = done else { continue };
                match reply {
                    Ok(Ok((here, others))) => {
                        if here {
                            found.push(relay);
                        }
                        closer.extend(others);
                    }
                    _ => self.failed(relay),
                }
            }
            if !found.is_empty() {
                return closest(node.as_bytes(), found, HOME_RELAYS);
            }
            closer.retain(|relay| {
                !asked.contains(relay) && usable(*relay, self.config.allow_private)
            });
            // Referred relays are probed before they are trusted.
            self.probe_all(closer.iter().copied()).await;
            let verified = {
                let set = self.set();
                closer.retain(|relay| set.contains(*relay));
                closest(node.as_bytes(), closer, HOME_RELAYS)
            };
            if verified.is_empty() {
                break;
            }
            next = verified;
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_addresses_are_filtered() {
        let ok = |text: &str, private| usable(text.parse().unwrap(), private);
        assert!(ok("93.184.216.34:42425", false));
        assert!(ok("[2606:4700::1]:1", false));
        for private in [
            "127.0.0.1:1",
            "10.0.0.1:1",
            "192.168.1.1:1",
            "100.64.0.1:1",
            "169.254.0.1:1",
            "[::1]:1",
            "[fd00::1]:1",
            "[fe80::1]:1",
            "[::ffff:127.0.0.1]:1",
        ] {
            assert!(!ok(private, false), "{private}");
            assert!(ok(private, true), "{private}");
        }
        for never in ["0.0.0.0:1", "224.0.0.1:1", "255.255.255.255:1", "1.1.1.1:0"] {
            assert!(!ok(never, true), "{never}");
        }
    }

    #[test]
    fn the_directory_infohash_is_fixed() {
        assert_eq!(directory_topic(), directory_topic());
        assert_ne!(directory_topic(), [0; 20]);
    }
}
