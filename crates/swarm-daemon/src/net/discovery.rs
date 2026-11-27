//! Relay discovery in the daemon (relay design spec REL-24 to REL-32): keep a
//! set of verified relays, fill it from the saved list, the DHT directory and
//! gossip, and find the relays that hold a node.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::NodeId;
use swarm_net::Endpoint;
use swarm_relay::{Directory, DirectoryConfig, Known};
use swarm_store::relaydb::{self, RelayRow};
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::NetState;

/// How often a non-empty relay set is refreshed (REL-26).
const ROUND: Duration = Duration::from_secs(30 * 60);
/// How soon an empty one is tried again.
const EMPTY_RETRY: Duration = Duration::from_secs(60);

/// True unless `SWARM_RELAY_DISCOVERY` turns discovery off (REL-32).
#[must_use]
pub fn enabled() -> bool {
    !std::env::var("SWARM_RELAY_DISCOVERY").is_ok_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "off" | "false" | "no" | "0"
        )
    })
}

/// `SWARM_RELAY_ROUND_SECS` shortens the refresh interval for tests.
pub(super) fn round() -> Duration {
    std::env::var("SWARM_RELAY_ROUND_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(ROUND, Duration::from_secs)
}

/// The daemon's relay set.
pub struct Discovery {
    directory: Arc<Directory>,
    /// Configured relays, which seed the set (REL-27).
    configured: Vec<String>,
    path: PathBuf,
    refreshed: Mutex<Option<Instant>>,
}

async fn resolve(names: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for name in names {
        let addr = super::relay::address_of(name);
        // An onion relay has no address to publish or compare.
        if swarm_net::Endpoint::parse(addr).is_some_and(|endpoint| endpoint.is_onion()) {
            continue;
        }
        if let Ok(mut found) = tokio::net::lookup_host(addr).await {
            out.extend(found.next());
        }
    }
    out
}

impl Discovery {
    /// A discovery over the daemon's DHT node, restoring the saved relays.
    pub fn new(net: &NetState, configured: Vec<String>) -> Arc<Self> {
        let allow_private =
            std::env::var("SWARM_RELAY_ALLOW_PRIVATE").is_ok_and(|value| value == "1");
        let directory = Directory::new(
            net.dht(),
            None,
            DirectoryConfig {
                allow_private,
                ..DirectoryConfig::default()
            },
        );
        let path = net.home.root().join("relays.db");
        match relaydb::load(&path) {
            Ok(rows) => directory.load(rows.into_iter().filter_map(|row| {
                Some(Known {
                    addr: row.addr.parse().ok()?,
                    relay_id: row.relay_id,
                    successes: row.successes,
                    failures: row.failures,
                    rtt_ms: row.rtt_ms,
                    name: None,
                })
            })),
            Err(err) => tracing::warn!(error = %err, "cannot read relays.db"),
        }
        Arc::new(Self {
            directory,
            configured,
            path,
            refreshed: Mutex::new(None),
        })
    }

    /// Relays known.
    #[must_use]
    pub fn known(&self) -> usize {
        self.directory.len()
    }

    /// The relays with the highest weight for `node` (REL-28).
    #[must_use]
    pub fn home(&self, node: NodeId, n: usize) -> Vec<SocketAddr> {
        self.directory.closest(node.as_bytes(), n)
    }

    /// What to call the relay at `addr`: its checked domain name, else the
    /// address.
    #[must_use]
    pub fn name_of(&self, addr: SocketAddr) -> String {
        self.directory
            .name_of(addr)
            .unwrap_or_else(|| addr.to_string())
    }

    /// Record that the relay called `name` could not be used, so the next one
    /// is tried.
    pub fn failed(&self, name: &str) {
        let addr = name
            .parse()
            .ok()
            .or_else(|| self.directory.addr_named(name));
        if let Some(addr) = addr {
            self.directory.failed(addr);
        }
    }

    /// Fill the set if it is empty or old. Concurrent callers share one round.
    pub async fn refresh_if_stale(&self) {
        let mut refreshed = self.refreshed.lock().await;
        let wait = if self.directory.is_empty() {
            EMPTY_RETRY.min(round())
        } else {
            round()
        };
        if refreshed.is_some_and(|at| at.elapsed() < wait) {
            return;
        }
        let extra = resolve(&self.configured).await;
        self.directory.refresh(&extra).await;
        *refreshed = Some(Instant::now());
        tracing::debug!(relays = self.directory.len(), "relay discovery round");
        self.save();
    }

    fn save(&self) {
        let rows: Vec<RelayRow> = self
            .directory
            .ranked()
            .into_iter()
            .map(|relay| RelayRow {
                addr: relay.addr.to_string(),
                relay_id: relay.relay_id,
                successes: relay.successes,
                failures: relay.failures,
                rtt_ms: relay.rtt_ms,
            })
            .collect();
        if let Err(err) = relaydb::save(&self.path, &rows) {
            tracing::debug!(error = %err, "cannot save relays.db");
        }
    }

    /// The relays a repo topic belongs on (R6).
    pub async fn topic_homes(&self, topic: [u8; 32]) -> Vec<SocketAddr> {
        self.refresh_if_stale().await;
        self.directory.closest(&topic, swarm_relay::HOME_RELAYS)
    }

    /// The relays that hold `node` (REL-29).
    pub async fn locate(&self, node: NodeId) -> Vec<Endpoint> {
        self.refresh_if_stale().await;
        let extra = resolve(&self.configured).await;
        self.directory
            .locate(node, &extra)
            .await
            .into_iter()
            .map(Endpoint::Ip)
            .collect()
    }
}
