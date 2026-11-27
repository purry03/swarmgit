//! The M9 reach and privacy settings, read once at start.

use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::sync::Arc;

use swarm_net::transport::{ObfsTransport, Tcp, Transport};
use swarm_net::Proxy;

use super::limits::{OnionLimits, MAX_TOTAL_CONNECTIONS};

/// Settings from the environment (`config.yaml` fills it in).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// SOCKS5 proxy for every outbound connection (`SWARM_PROXY`).
    pub proxy: Option<Proxy>,
    /// Randomized handshake and frame sizes (`SWARM_OBFS`).
    pub obfs: bool,
    /// Tor control port hosting a hidden service (`SWARM_ONION_CONTROL`).
    pub onion_control: Option<String>,
    /// Password for it (`SWARM_ONION_PASSWORD`); cookie auth needs none.
    pub onion_password: Option<String>,
    /// Caps for visitors through the hidden service
    /// (`SWARM_ONION_MAX_CONNECTIONS`, `SWARM_ONION_MAX_PER_MINUTE`).
    pub onion_limits: OnionLimits,
    /// UDP hole punching (`SWARM_EXPERIMENTAL_UDP`).
    pub experimental_udp: bool,
    /// Upload cap in kilobits per second, 0 for none (`SWARM_UPLOAD_KBPS`).
    pub upload_kbps: u64,
    /// Bytes one relayed session may carry (`SWARM_RELAY_MAX_BYTES`).
    pub relay_max_bytes: u64,
    /// Secret for configured relays (`SWARM_RELAY_PASSWORD`).
    pub relay_password: Option<String>,
}

fn value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A whole number within `range`, or `default` when unset.
fn number(
    name: &str,
    key: &str,
    default: usize,
    range: RangeInclusive<usize>,
) -> anyhow::Result<usize> {
    let Some(text) = value(name) else {
        return Ok(default);
    };
    text.parse()
        .ok()
        .filter(|number| range.contains(number))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{key} must be a whole number from {} to {}",
                range.start(),
                range.end()
            )
        })
}

fn flag(name: &str) -> bool {
    value(name).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl Settings {
    /// Read the settings. A proxy or upload cap that does not parse stops
    /// the daemon: carrying on would drop the protection asked for.
    pub fn from_env() -> anyhow::Result<Self> {
        let upload_kbps = match value("SWARM_UPLOAD_KBPS") {
            Some(text) => text
                .parse()
                .map_err(|_| anyhow::anyhow!("upload_kbps must be a whole number"))?,
            None => 0,
        };
        let relay_max_bytes = match value("SWARM_RELAY_MAX_BYTES") {
            Some(text) => text
                .parse()
                .ok()
                .filter(|bytes| *bytes > 0)
                .ok_or_else(|| anyhow::anyhow!("relay_max_bytes must be a positive number"))?,
            None => super::capped::DEFAULT_MAX_BYTES,
        };
        let proxy = Proxy::from_env()?;
        let experimental_udp = flag("SWARM_EXPERIMENTAL_UDP");
        if experimental_udp && proxy.is_some() {
            tracing::warn!("experimental UDP is off: UDP cannot go through the SOCKS5 proxy");
        }
        let defaults = OnionLimits::default();
        let onion_limits = OnionLimits {
            max_connections: number(
                "SWARM_ONION_MAX_CONNECTIONS",
                "onion_max_connections",
                defaults.max_connections,
                1..=MAX_TOTAL_CONNECTIONS,
            )?,
            max_per_minute: number(
                "SWARM_ONION_MAX_PER_MINUTE",
                "onion_max_per_minute",
                defaults.max_per_minute,
                1..=10_000,
            )?,
        };
        Ok(Self {
            onion_limits,
            experimental_udp: experimental_udp && proxy.is_none(),
            proxy,
            obfs: flag("SWARM_OBFS"),
            onion_control: value("SWARM_ONION_CONTROL"),
            onion_password: value("SWARM_ONION_PASSWORD"),
            upload_kbps,
            relay_max_bytes,
            relay_password: value("SWARM_RELAY_PASSWORD"),
        })
    }

    /// True when a proxy makes every outbound connection.
    #[must_use]
    pub fn proxied(&self) -> bool {
        self.proxy.is_some()
    }

    /// The transport for peer sessions.
    #[must_use]
    pub fn transport(&self) -> Arc<dyn Transport> {
        if self.obfs {
            Arc::new(ObfsTransport(Tcp))
        } else {
            Arc::new(Tcp)
        }
    }

    /// The default listen address. Behind a proxy the node stays on loopback
    /// unless told otherwise, so nobody reaches it around the proxy.
    #[must_use]
    pub fn default_listen(proxied: bool) -> SocketAddr {
        if proxied {
            SocketAddr::from(([127, 0, 0, 1], 42424))
        } else {
            SocketAddr::from(([0, 0, 0, 0], 42424))
        }
    }
}
