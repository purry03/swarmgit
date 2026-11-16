//! Relay registration (relay design spec R2, REL-8, REL-9, REL-12, REL-13).
//!
//! A node that cannot accept inbound connections keeps one control connection
//! to each configured relay. Dialers that learn `relay:host:port@node` from a
//! link, a `Peers` reply or the peers table reach the node through it. The
//! swarm handshake runs end to end over the relayed stream, so the relay sees
//! only ciphertext.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use swarm_core::ids::{Hint, HintScheme, Host, NodeId};
use swarm_net::PeerAddr;
use swarm_relay::ListenState;
use tokio::sync::Semaphore;

use super::NetState;

/// Relayed handshakes in flight at once.
const MAX_RELAYED_HANDSHAKES: usize = 16;
/// First and largest delay between failed first registrations.
const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// How the daemon uses relays (`SWARM_RELAY_MODE`, REL-12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Register with configured relays and use relay addresses.
    Auto,
    /// Same as `Auto` in R2. Reachability checks that let `Auto` skip
    /// registration arrive with the relay network (R3).
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

/// Relays from `SWARM_RELAYS` (comma separated `host:port`), in order.
#[must_use]
pub fn configured() -> Vec<String> {
    std::env::var("SWARM_RELAYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| {
            name.rsplit_once(':')
                .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
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
    /// The relay's address once resolved.
    pub addr: Option<SocketAddr>,
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
    let (host, port) = name.rsplit_once(':')?;
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

    fn update_relay(&self, name: &str, change: impl FnOnce(&mut RelayStatus)) {
        let mut relays = self
            .relays
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(status) = relays.iter_mut().find(|status| status.name == name) {
            change(status);
        }
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
            .filter_map(|status| status.addr)
            .map(|relay| PeerAddr::Relay {
                relay,
                node: self.node_id,
            })
            .collect()
    }
}

/// Register with the configured relays. Does nothing when relays are off or
/// none are configured.
pub fn start(net: &Arc<NetState>) {
    if mode() == Mode::Off {
        tracing::info!("relays disabled by SWARM_RELAY_MODE=off");
        return;
    }
    let names: Vec<String> = configured();
    if names.is_empty() {
        return;
    }
    let slots = Arc::new(Semaphore::new(MAX_RELAYED_HANDSHAKES));
    for name in names {
        {
            let mut relays = net
                .relays
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            relays.push(RelayStatus {
                name: name.clone(),
                addr: None,
                registered: false,
                observed: None,
                error: None,
            });
        }
        let net = Arc::clone(net);
        let slots = Arc::clone(&slots);
        tokio::spawn(async move { maintain(net, name, slots).await });
    }
}

async fn maintain(net: Arc<NetState>, name: String, slots: Arc<Semaphore>) {
    let mut delay = RETRY_MIN;
    let mut listener = loop {
        match swarm_relay::listen(&name, Arc::clone(&net.node)).await {
            Ok(listener) => break listener,
            Err(err) => {
                tracing::debug!(relay = %name, error = %err, "relay registration failed");
                net.update_relay(&name, |status| {
                    status.registered = false;
                    status.error = Some(err.to_string());
                });
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(RETRY_MAX);
            }
        }
    };
    let addr = tokio::net::lookup_host(&name)
        .await
        .ok()
        .and_then(|mut found| found.next());
    tracing::info!(relay = %name, "registered with relay");
    net.update_relay(&name, |status| {
        status.addr = addr;
        apply(
            status,
            &ListenState::Registered {
                observed: String::new(),
            },
        );
    });
    // The first state carries the real observed address.
    let mut watch = listener.watch();
    {
        let net = Arc::clone(&net);
        let name = name.clone();
        tokio::spawn(async move {
            loop {
                let state = watch.borrow_and_update().clone();
                net.update_relay(&name, |status| apply(status, &state));
                if watch.changed().await.is_err() {
                    break;
                }
            }
        });
    }
    while let Some(stream) = listener.accept().await {
        let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let net = Arc::clone(&net);
        let name = name.clone();
        tokio::spawn(async move {
            let _permit = permit;
            // How the dialer reaches us is not known here, so no address is
            // reported back to it; the relay is the only thing in between.
            let unknown = PeerAddr::Tcp(SocketAddr::from(([0, 0, 0, 0], 0)));
            super::serve::handle_stream(net, stream, unknown, &format!("relay {name}")).await;
        });
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
