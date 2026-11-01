//! Route C: Mainline DHT rendezvous and the BEP 44 head pointer (spec 6.4).
//!
//! Every call has a timeout and a failure is never an error for the caller
//! (DHT-4): lookups return what they found so far, puts return `false`.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_lite::StreamExt;
use swarm_core::consts::{CTX_HEAD_SALT, SALT_HEAD};
use swarm_core::crypto::{derive_key, topic};
use swarm_core::ids::RepoId;

/// Timeout for one lookup or put.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// A head pointer: the newest manifest a signer published (DHT-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    /// `manifest_id` of the newest manifest.
    pub manifest_id: [u8; 32],
    /// Its `seq`.
    pub seq: u64,
}

/// The operations the daemon needs from a DHT.
pub trait Dht: Send + Sync {
    /// Peers announced under a topic.
    fn get_peers(
        &self,
        topic: [u8; 20],
    ) -> impl std::future::Future<Output = Vec<SocketAddr>> + Send;
    /// Announce our TCP port under a topic.
    fn announce(
        &self,
        topic: [u8; 20],
        port: u16,
    ) -> impl std::future::Future<Output = bool> + Send;
    /// Read the newest head pointer for a signer key and repo.
    fn get_head(
        &self,
        ed_key: [u8; 32],
        repo: RepoId,
    ) -> impl std::future::Future<Output = Option<Head>> + Send;
    /// Store a head pointer signed with the Ed25519 seed.
    fn put_head(
        &self,
        ed_seed: [u8; 32],
        repo: RepoId,
        head: Head,
    ) -> impl std::future::Future<Output = bool> + Send;
}

/// Route settings, read from the environment.
#[derive(Clone, Debug, Default)]
pub struct DhtConfig {
    /// Entry nodes. When `replace_default` is set, only these are used.
    pub bootstrap: Vec<String>,
    /// Use `bootstrap` instead of the library's built-in list.
    pub replace_default: bool,
    /// UDP bind address.
    pub bind: Option<Ipv4Addr>,
    /// UDP port, ephemeral when unset.
    pub port: Option<u16>,
}

impl DhtConfig {
    /// `SWARM_DHT_BOOTSTRAP` (comma separated `host:port`) replaces the
    /// built-in list, which is how tests point at a local testnet.
    /// `SWARM_DHT_BIND` and `SWARM_DHT_PORT` set the UDP socket.
    #[must_use]
    pub fn from_env(persisted: Vec<String>) -> Self {
        let explicit: Vec<String> = std::env::var("SWARM_DHT_BOOTSTRAP")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        let replace_default = !explicit.is_empty();
        let mut bootstrap = explicit;
        for node in persisted {
            if !bootstrap.contains(&node) {
                bootstrap.push(node);
            }
        }
        Self {
            bootstrap,
            replace_default,
            bind: std::env::var("SWARM_DHT_BIND")
                .ok()
                .and_then(|value| value.parse().ok()),
            port: std::env::var("SWARM_DHT_PORT")
                .ok()
                .and_then(|value| value.parse().ok()),
        }
    }
}

/// True when `SWARM_NO_DHT=1` turns the route off (DM-5).
#[must_use]
pub fn disabled() -> bool {
    std::env::var("SWARM_NO_DHT").is_ok_and(|value| value == "1")
}

/// Days since the Unix epoch.
#[must_use]
pub fn day_number(now_secs: u64) -> u64 {
    now_secs / 86_400
}

fn today() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    day_number(secs)
}

/// Lookup topics: today and yesterday (DHT-1).
#[must_use]
pub fn lookup_topics(repo: &RepoId, day: u64) -> Vec<[u8; 20]> {
    vec![topic(repo, day), topic(repo, day.saturating_sub(1))]
}

/// Announce topics: yesterday, today and tomorrow (DHT-2).
#[must_use]
pub fn announce_topics(repo: &RepoId, day: u64) -> Vec<[u8; 20]> {
    vec![
        topic(repo, day.saturating_sub(1)),
        topic(repo, day),
        topic(repo, day + 1),
    ]
}

/// BEP 44 salt: `SALT_HEAD || derive_key(CTX_HEAD_SALT, repo)` (48 bytes).
///
/// BEP 44 sends the salt in the clear to every storing node, and the repo
/// ID is a secret: knowing it is enough to find and clone the repo. So the
/// salt carries a derivation of the ID, never the ID itself (spec DHT-5).
#[must_use]
pub fn head_salt(repo: &RepoId) -> Vec<u8> {
    let mut salt = SALT_HEAD.to_vec();
    salt.extend_from_slice(&derive_key(CTX_HEAD_SALT, repo.as_bytes()));
    salt
}

/// Bencode `{m: manifest_id, s: seq}`. Keys are in sorted order.
#[must_use]
pub fn encode_head(head: &Head) -> Vec<u8> {
    let mut out = b"d1:m32:".to_vec();
    out.extend_from_slice(&head.manifest_id);
    out.extend_from_slice(format!("1:si{}ee", head.seq).as_bytes());
    out
}

/// Decode a head value. Anything but the exact canonical form is refused.
#[must_use]
pub fn decode_head(bytes: &[u8]) -> Option<Head> {
    let rest = bytes.strip_prefix(b"d1:m32:")?;
    if rest.len() < 32 {
        return None;
    }
    let mut manifest_id = [0u8; 32];
    manifest_id.copy_from_slice(&rest[..32]);
    let rest = rest[32..].strip_prefix(b"1:si")?;
    let rest = rest.strip_suffix(b"ee")?;
    let text = std::str::from_utf8(rest).ok()?;
    if text.is_empty() || (text.len() > 1 && text.starts_with('0')) {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seq = text.parse().ok()?;
    let head = Head { manifest_id, seq };
    (encode_head(&head) == bytes).then_some(head)
}

/// The Mainline DHT behind [`Dht`].
#[derive(Clone)]
pub struct MainlineDht {
    inner: mainline::async_dht::AsyncDht,
    timeout: Duration,
}

/// Errors from starting the DHT node.
#[derive(Debug, thiserror::Error)]
pub enum DhtError {
    /// The UDP socket could not be opened.
    #[error("dht start failed: {0}")]
    Start(#[from] std::io::Error),
}

impl MainlineDht {
    /// Start a node.
    pub fn start(config: &DhtConfig) -> Result<Self, DhtError> {
        let mut builder = mainline::Dht::builder();
        if config.replace_default {
            builder.bootstrap(&config.bootstrap);
        } else if !config.bootstrap.is_empty() {
            builder.extra_bootstrap(&config.bootstrap);
        }
        if let Some(bind) = config.bind {
            builder.bind_address(bind);
        }
        if let Some(port) = config.port {
            builder.port(port);
        }
        builder.request_timeout(Duration::from_secs(2));
        let dht = builder.build()?;
        Ok(Self {
            inner: dht.as_async(),
            timeout: CALL_TIMEOUT,
        })
    }

    /// Wrap an existing node, used by tests on a testnet.
    #[must_use]
    pub fn from_node(node: mainline::Dht) -> Self {
        Self {
            inner: node.as_async(),
            timeout: CALL_TIMEOUT,
        }
    }

    /// Nodes that answered recently, to persist (DHT-3).
    pub async fn known_nodes(&self) -> Vec<String> {
        tokio::time::timeout(self.timeout, self.inner.to_bootstrap())
            .await
            .unwrap_or_default()
    }

    /// Local UDP address of this node.
    pub async fn local_addr(&self) -> Option<SocketAddrV4> {
        tokio::time::timeout(self.timeout, self.inner.info())
            .await
            .ok()
            .map(|info| info.local_addr())
    }

    /// Look up every peer of a repo for today and yesterday (DHT-1).
    pub async fn find(&self, repo: RepoId) -> Vec<SocketAddr> {
        let mut out: Vec<SocketAddr> = Vec::new();
        for topic in lookup_topics(&repo, today()) {
            for addr in self.get_peers(topic).await {
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
        }
        out
    }

    /// Announce a repo for yesterday, today and tomorrow (DHT-2).
    pub async fn announce_repo(&self, repo: RepoId, port: u16) -> usize {
        let mut ok = 0;
        for topic in announce_topics(&repo, today()) {
            if self.announce(topic, port).await {
                ok += 1;
            }
        }
        ok
    }
}

impl Dht for MainlineDht {
    async fn get_peers(&self, topic: [u8; 20]) -> Vec<SocketAddr> {
        let id = mainline::Id::from_bytes(topic);
        let Ok(id) = id else {
            return Vec::new();
        };
        let mut stream = self.inner.get_peers(id);
        let mut out: Vec<SocketAddr> = Vec::new();
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            match tokio::time::timeout(deadline - now, stream.next()).await {
                Ok(Some(batch)) => {
                    for addr in batch {
                        let addr = SocketAddr::V4(addr);
                        if !out.contains(&addr) {
                            out.push(addr);
                        }
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        out
    }

    async fn announce(&self, topic: [u8; 20], port: u16) -> bool {
        let Ok(id) = mainline::Id::from_bytes(topic) else {
            return false;
        };
        match tokio::time::timeout(self.timeout, self.inner.announce_peer(id, Some(port))).await {
            Ok(Ok(_)) => true,
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "dht announce failed");
                false
            }
            Err(_) => {
                tracing::debug!("dht announce timed out");
                false
            }
        }
    }

    async fn get_head(&self, ed_key: [u8; 32], repo: RepoId) -> Option<Head> {
        let salt = head_salt(&repo);
        let item = tokio::time::timeout(
            self.timeout,
            self.inner.get_mutable_most_recent(&ed_key, Some(&salt)),
        )
        .await
        .ok()??;
        let head = decode_head(item.value())?;
        // The BEP 44 seq must agree with the value; otherwise ignore it.
        (u64::try_from(item.seq()).ok() == Some(head.seq)).then_some(head)
    }

    async fn put_head(&self, ed_seed: [u8; 32], repo: RepoId, head: Head) -> bool {
        let Ok(seq) = i64::try_from(head.seq) else {
            return false;
        };
        let salt = head_salt(&repo);
        let key = mainline::SigningKey::from_bytes(&ed_seed);
        let item = mainline::MutableItem::new(key, &encode_head(&head), seq, Some(&salt));
        match tokio::time::timeout(self.timeout, self.inner.put_mutable(item, None)).await {
            Ok(Ok(_)) => true,
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "dht head put failed");
                false
            }
            Err(_) => {
                tracing::debug!("dht head put timed out");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests;
