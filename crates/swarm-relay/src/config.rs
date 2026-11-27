//! What a relay is allowed to do.

use std::time::Duration;

use crate::secure::RelayKey;

/// Resource limits (REL-5, REL-6, REL-19, REL-20).
#[derive(Clone, Debug)]
pub struct Limits {
    /// Most listeners registered at once.
    pub max_listeners: usize,
    /// Most splices running at once.
    pub max_splices: usize,
    /// Most connect requests waiting for an accept.
    pub max_pending: usize,
    /// A splice with no bytes in either direction this long is closed.
    pub idle: Duration,
    /// A control connection silent this long is closed.
    pub control_silence: Duration,
    /// How long a dialer waits for the listener to accept.
    pub pending_ttl: Duration,
    /// How long a new connection has to say what it wants.
    pub hello_timeout: Duration,
    /// Most nodes registered from one address.
    pub listeners_per_ip: usize,
    /// Most splices one dialing address may have open.
    pub splices_per_ip: usize,
    /// Most `Connect` attempts from one address in a minute.
    pub connects_per_minute: usize,
    /// Rate of one splice, both directions together. 0 means no limit.
    pub splice_bytes_per_second: u64,
    /// Most bytes carried to and from one node in a day.
    pub node_daily_bytes: u64,
    /// Apply the per-address limits to loopback too (tests).
    pub limit_loopback: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_listeners: 10_000,
            max_splices: 500,
            max_pending: 1_000,
            idle: Duration::from_secs(120),
            control_silence: Duration::from_secs(75),
            pending_ttl: Duration::from_secs(10),
            hello_timeout: Duration::from_secs(10),
            listeners_per_ip: 2,
            splices_per_ip: 8,
            connects_per_minute: 20,
            splice_bytes_per_second: 8 * 1024 * 1024,
            node_daily_bytes: 10 * 1024 * 1024 * 1024,
            limit_loopback: false,
        }
    }
}

/// Relay configuration.
#[derive(Clone, Debug, Default)]
pub struct RelayConfig {
    /// Names clients may dial this relay by, as `host:port`. A listener's
    /// proof is bound to the name it dialed (REL-1). Empty means any name is
    /// accepted, which gives up that binding and suits tests only.
    pub names: Vec<String>,
    /// Resource limits.
    pub limits: Limits,
    /// Nodes that may register. Empty means any (REL-21).
    pub allow_nodes: Vec<[u8; 32]>,
    /// A secret that listeners and dialers must present (REL-21).
    pub password: Option<String>,
    /// The relay's static key for private control connections. A new one is
    /// made for this run when absent, which breaks pinned addresses.
    pub key: Option<RelayKey>,
}
