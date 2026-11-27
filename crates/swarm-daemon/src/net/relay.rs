//! Relay registration (relay design spec R2, REL-8, REL-9, REL-12, REL-13).
//!
//! A node that cannot accept inbound connections keeps one control connection
//! to each relay it is registered with: the configured ones and its home
//! relays from discovery. Dialers that learn `relay:host:port@node` from a
//! link, a `Peers` reply or the peers table reach the node through it. The
//! swarm handshake runs end to end over the relayed stream, so the relay sees
//! only ciphertext.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use swarm_core::ids::{Hint, HintScheme, Host, NodeId};
use swarm_net::{Endpoint, PeerAddr, Session};
use swarm_relay::ListenState;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::capped::Capped;
use super::discovery::Discovery;
use super::NetState;

/// Relayed handshakes in flight at once.
const MAX_RELAYED_HANDSHAKES: usize = 16;
/// First and largest delay between failed first registrations.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// How the daemon uses relays (`SWARM_RELAY_MODE`, REL-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Use relays; register only while direct inbound is not confirmed.
    Auto,
    /// Register even when direct inbound works.
    Always,
    /// Never register and never dial through a relay.
    Off,
}

/// The configured mode. Unknown values mean `Auto`.
#[must_use]
pub fn mode() -> Mode {
    match std::env::var("SWARM_RELAY_MODE").as_deref() {
        Ok("off") => Mode::Off,
        Ok("always") => Mode::Always,
        _ => Mode::Auto,
    }
}

/// `name` without a pinned relay key.
#[must_use]
pub fn address_of(name: &str) -> &str {
    name.split_once('#').map_or(name, |(addr, _)| addr)
}

/// Relays from `SWARM_RELAYS` (comma separated `host:port`, each optionally
/// followed by `#<relay key>`), in order.
#[must_use]
pub fn configured() -> Vec<String> {
    std::env::var("SWARM_RELAYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| {
            swarm_relay::split_key(name).is_ok_and(|(addr, _)| {
                addr.rsplit_once(':')
                    .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
            })
        })
        .map(str::to_string)
        .collect()
}

/// Delay before a relay dial when a direct address for the same node is also
/// being tried (REL-14). `SWARM_RELAY_DELAY_MS` overrides it.
#[must_use]
pub fn dial_delay() -> Duration {
    std::env::var("SWARM_RELAY_DELAY_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map_or(Duration::from_millis(1500), Duration::from_millis)
}

/// One relay this node tries to stay registered with.
#[derive(Clone, Debug)]
pub struct RelayStatus {
    /// `host:port` as configured. The proof is bound to this exact string.
    pub name: String,
    /// The relay's address once resolved, or its name behind a proxy.
    pub addr: Option<Endpoint>,
    /// True while the control connection is up.
    pub registered: bool,
    /// Our address as the relay sees it.
    pub observed: Option<String>,
    /// The last error, while not registered.
    pub error: Option<String>,
}

/// A link hint for reaching `node` through the relay named `name`.
#[must_use]
pub fn hint_for(name: &str, node: NodeId) -> Option<Hint> {
    let (host, port) = address_of(name).rsplit_once(':')?;
    Some(Hint {
        scheme: HintScheme::Relay,
        host: Host::parse(host)?,
        port: port.parse().ok().filter(|port| *port != 0)?,
        node: Some(node),
    })
}

impl NetState {
    /// Snapshot of relay registrations.
    #[must_use]
    pub fn relay_statuses(&self) -> Vec<RelayStatus> {
        self.relays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn add_relay(&self, name: &str) {
        let mut relays = self
            .relays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        relays.retain(|status| status.name != name);
        relays.push(RelayStatus {
            name: name.to_string(),
            addr: None,
            registered: false,
            observed: None,
            error: None,
        });
    }

    fn remove_relay(&self, name: &str) {
        self.relays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|status| status.name != name);
    }

    fn update_relay(&self, name: &str, change: impl FnOnce(&mut RelayStatus)) {
        let mut relays = self
            .relays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(status) = relays.iter_mut().find(|status| status.name == name) {
            let was_registered = status.registered;
            change(status);
            if status.registered && !was_registered {
                self.registrations.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// How many times a relay registration has been made.
    pub(super) fn registrations(&self) -> u64 {
        self.registrations.load(Ordering::Relaxed)
    }

    /// Link hints for every relay this node is registered with (REL-8).
    #[must_use]
    pub fn relay_hints(&self) -> Vec<Hint> {
        self.relay_statuses()
            .iter()
            .filter(|status| status.registered)
            .filter_map(|status| hint_for(&status.name, self.node_id))
            .collect()
    }

    /// This node's own relay addresses, for `Peers` replies (REL-9).
    #[must_use]
    pub fn relay_peer_addrs(&self) -> Vec<PeerAddr> {
        self.relay_statuses()
            .iter()
            .filter(|status| status.registered)
            .filter_map(|status| status.addr.clone())
            .map(|relay| PeerAddr::Relay {
                relay,
                node: self.node_id,
            })
            .collect()
    }
}

/// Relays registered with at once, configured and discovered (REL-28).
const MAX_HOMES: usize = 3;
/// Discovered home relays among them.
const MAX_DISCOVERED: usize = 2;
/// How often the wanted set of relays is worked out again.
const REVIEW_EVERY: Duration = Duration::from_secs(5);
/// How long a proof of direct reachability holds.
const REACHABLE_FOR: Duration = Duration::from_secs(90 * 60);

impl NetState {
    /// Record that a direct inbound path works (a UPnP mapping on a public
    /// address, or a peer on the internet that got through).
    pub fn mark_reachable(&self) {
        *self
            .reachable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }

    /// True while direct inbound reachability is confirmed (REL-13).
    #[must_use]
    pub fn directly_reachable(&self) -> bool {
        self.reachable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|at| at.elapsed() < REACHABLE_FOR)
    }

    /// Relay discovery, unless it is off.
    #[must_use]
    pub fn discovery(&self) -> Option<Arc<Discovery>> {
        self.discovery.get().cloned()
    }

    /// The password to present to the relay `name`: only ever to a relay
    /// the user configured, never to a discovered or hinted one.
    #[must_use]
    pub fn relay_password_for(&self, name: &str) -> Option<String> {
        configured()
            .iter()
            .any(|configured| configured == name)
            .then(|| self.settings.relay_password.clone())
            .flatten()
    }

    /// What to dial, and the password to present, for a relay known by
    /// address. When that address is a configured relay, or what its name
    /// resolves to, its pinned key and the password apply.
    pub async fn relay_access(&self, relay: &Endpoint) -> (String, Option<String>) {
        for name in configured() {
            let addr = address_of(&name);
            let mut matches = Endpoint::parse(addr).as_ref() == Some(relay);
            // Behind a proxy names are not resolved here, and onion names
            // never are.
            let resolvable = !self.settings.proxied()
                && !Endpoint::parse(addr).is_some_and(|endpoint| endpoint.is_onion());
            if let (false, true, Endpoint::Ip(ip)) = (matches, resolvable, relay) {
                matches = tokio::net::lookup_host(addr)
                    .await
                    .is_ok_and(|mut found| found.any(|found| found == *ip));
            }
            if matches {
                let pin = &name[addr.len()..];
                return (
                    format!("{relay}{pin}"),
                    self.settings.relay_password.clone(),
                );
            }
        }
        (relay.to_string(), None)
    }

    /// Remember how `node` was reached (REL-18): through `relay`, or not
    /// through any.
    pub fn set_relayed(&self, node: NodeId, relay: Option<String>) {
        let mut nodes = self
            .relayed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match relay {
            Some(relay) => nodes.insert(node, relay),
            None => nodes.remove(&node),
        };
    }

    /// The relay the open session to `node` goes through.
    #[must_use]
    pub fn relay_of(&self, node: NodeId) -> Option<String> {
        self.relayed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&node)
            .cloned()
    }

    /// True when the open session to `node` goes through a relay.
    #[must_use]
    pub fn is_relayed(&self, node: NodeId) -> bool {
        self.relay_of(node).is_some()
    }
}

/// Set up discovery and keep the registrations current. Does nothing when
/// relays are off.
pub fn start(net: &Arc<NetState>) {
    if mode() == Mode::Off {
        tracing::info!("relays disabled by SWARM_RELAY_MODE=off");
        return;
    }
    let names = configured();
    // Behind a proxy only the configured relays are used: gossip would have
    // the daemon connect to strangers on their say-so.
    if super::discovery::enabled() && !net.settings.proxied() {
        let _ = net.discovery.set(Discovery::new(net, names.clone()));
    }
    if names.is_empty() && net.discovery().is_none() {
        return;
    }
    super::topics::start(net);
    let weak = Arc::downgrade(net);
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(MAX_RELAYED_HANDSHAKES));
        let mut running: HashMap<String, JoinHandle<()>> = HashMap::new();
        loop {
            let Some(net) = weak.upgrade() else { break };
            let wanted = wanted_relays(&net, &names).await;
            running.retain(|name, task| {
                let keep = wanted.contains(name);
                if !keep {
                    task.abort();
                    net.remove_relay(name);
                }
                keep
            });
            for name in wanted {
                if running.contains_key(&name) {
                    continue;
                }
                net.add_relay(&name);
                let task =
                    tokio::spawn(maintain(Arc::clone(&net), name.clone(), Arc::clone(&slots)));
                running.insert(name, task);
            }
            drop(net);
            tokio::time::sleep(REVIEW_EVERY).await;
        }
    });
}

/// The relays to be registered with now (REL-13, REL-28): the configured
/// ones, plus up to two discovered home relays when this node serves
/// something. `auto` wants none while direct inbound works.
async fn wanted_relays(net: &Arc<NetState>, configured: &[String]) -> Vec<String> {
    if mode() == Mode::Auto && net.directly_reachable() {
        return Vec::new();
    }
    let mut wanted: Vec<String> = Vec::new();
    for name in configured {
        if wanted.len() < MAX_HOMES && !wanted.contains(name) {
            wanted.push(name.clone());
        }
    }
    let Some(discovery) = net.discovery() else {
        return wanted;
    };
    let room = (MAX_HOMES - wanted.len()).min(MAX_DISCOVERED);
    if room == 0 || net.serving_repos().is_empty() {
        return wanted;
    }
    discovery.refresh_if_stale().await;
    let configured_addrs: Vec<Option<Endpoint>> = net
        .relay_statuses()
        .into_iter()
        .filter(|status| configured.contains(&status.name))
        .map(|status| status.addr)
        .collect();
    let homes: Vec<String> = discovery
        .home(net.node_id, MAX_HOMES)
        .into_iter()
        .filter(|addr| !configured_addrs.contains(&Some(Endpoint::Ip(*addr))))
        .map(|addr| discovery.name_of(addr))
        .filter(|name| !wanted.iter().any(|known| address_of(known) == name))
        .take(room)
        .collect();
    wanted.extend(homes);
    wanted
}

async fn maintain(net: Arc<NetState>, name: String, slots: Arc<Semaphore>) {
    let mut delay = RETRY_MIN;
    let mut listener = loop {
        let password = net.relay_password_for(&name);
        match swarm_relay::listen_with(&name, Arc::clone(&net.node), password).await {
            Ok(listener) => break listener,
            Err(err) => {
                tracing::debug!(relay = %name, error = %err, "relay registration failed");
                net.update_relay(&name, |status| {
                    status.registered = false;
                    status.error = Some(err.to_string());
                });
                // A discovered relay that will not take us drops down the
                // ranking, so the next review can pick another.
                if let Some(discovery) = net.discovery() {
                    if !configured().contains(&name) {
                        discovery.failed(&name);
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(RETRY_MAX);
            }
        }
    };
    // Behind a proxy the name is left for the proxy to resolve.
    let addr = if net.settings.proxied() {
        Endpoint::parse(address_of(&name))
    } else {
        tokio::net::lookup_host(address_of(&name))
            .await
            .ok()
            .and_then(|mut found| found.next())
            .map(Endpoint::Ip)
    };
    tracing::info!(relay = %name, "registered with relay");
    net.update_relay(&name, |status| {
        status.addr = addr.clone();
        apply(
            status,
            &ListenState::Registered {
                observed: String::new(),
            },
        );
    });
    // The first state carries the real observed address.
    let mut watch = listener.watch();
    let watcher = {
        let net = Arc::clone(&net);
        let name = name.clone();
        async move {
            loop {
                let state = watch.borrow_and_update().clone();
                net.update_relay(&name, |status| apply(status, &state));
                if watch.changed().await.is_err() {
                    break;
                }
            }
        }
    };
    let accepting = async {
        while let Some(stream) = listener.accept().await {
            let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let net = Arc::clone(&net);
            let name = name.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_relayed(net, stream, &name).await;
            });
        }
    };
    // Aborting this task ends both and drops the registration; sessions
    // already running carry on.
    tokio::join!(watcher, accepting);
}

/// Serve one dialer that came through the relay `name` (REL-15).
async fn serve_relayed(net: Arc<NetState>, stream: TcpStream, name: &str) {
    // How the dialer reaches us is not known here, so no address is reported
    // back to it; the relay is the only thing in between.
    let unknown = PeerAddr::Tcp(SocketAddr::from(([0, 0, 0, 0], 0)));
    let stream = Capped::new(stream, net.settings.relay_max_bytes);
    let label = format!("relay {name}");
    // A failure before the handshake ends cannot be held against anyone: the
    // relay is the only address seen.
    let Some(Ok(transport)) = super::serve::accept(&net, stream, unknown, &label).await else {
        return;
    };
    let node = *transport.info.node_id.as_bytes();
    let Ok(_guard) = net.limits.enter_node(node) else {
        tracing::debug!(relay = %name, "relayed peer refused by its limits");
        return;
    };
    if super::serve::run_session(Arc::clone(&net), Session::start(transport)).await {
        net.limits.violation_node(node);
    }
}

fn apply(status: &mut RelayStatus, state: &ListenState) {
    match state {
        ListenState::Registered { observed } => {
            status.registered = true;
            status.error = None;
            if !observed.is_empty() {
                status.observed = Some(observed.clone());
            }
        }
        ListenState::Reconnecting { error } => {
            status.registered = false;
            status.error = Some(error.clone());
        }
    }
}
