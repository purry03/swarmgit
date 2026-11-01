//! Daemon network state: listener address, node key, peer sessions (M2),
//! and the DHT route, observed addresses and port mapping (M4, M5).

pub mod dht;
pub mod serve;
pub mod upnp;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use swarm_core::ids::{Hint, HintScheme, Host, NodeId, RepoId};
use swarm_core::keyfile::NodeKey;
use swarm_net::{PeerAddr, SessionHandle};
use swarm_store::{Db, Home, RepoStore};
use tokio::net::TcpListener;

/// Concurrent uploads (WANT-5).
pub const UPLOAD_SLOTS: u32 = 4;

/// A connected peer that can receive `Announce`.
pub struct Registered {
    /// Session handle.
    pub handle: SessionHandle,
    /// Repos shared on this session and their indexes.
    pub shared: Vec<(RepoId, u16)>,
    /// Unique registration number.
    pub id: u64,
}

/// Shared network state used by the listener, sync and IPC handlers.
pub struct NetState {
    /// Data directory.
    pub home: Home,
    /// Sqlite.
    pub db: Arc<Mutex<Db>>,
    /// Node key.
    pub node: Arc<NodeKey>,
    /// Node ID derived from the key.
    pub node_id: NodeId,
    /// Actual listening address.
    pub listen: SocketAddr,
    /// Largest pack served or accepted.
    pub max_pack_bytes: usize,
    /// Concurrent uploads (WANT-5).
    pub uploads: Arc<tokio::sync::Semaphore>,
    sessions: Mutex<Vec<Registered>>,
    next_session: AtomicU64,
    sync_locks: Mutex<HashMap<[u8; 32], Arc<tokio::sync::Mutex<()>>>>,
    lan: OnceLock<Arc<swarm_disc::lan::LanService>>,
    dht: OnceLock<swarm_disc::dht::MainlineDht>,
    observed: Mutex<HashMap<IpAddr, HashSet<[u8; 32]>>>,
    external: Mutex<Option<IpAddr>>,
}

impl NetState {
    /// Build network state.
    pub fn new(
        home: Home,
        db: Arc<Mutex<Db>>,
        node: Arc<NodeKey>,
        listen: SocketAddr,
        max_pack_bytes: usize,
    ) -> anyhow::Result<Arc<Self>> {
        let node_id = node.node_id()?;
        Ok(Arc::new(Self {
            home,
            db,
            node,
            node_id,
            listen,
            max_pack_bytes,
            uploads: Arc::new(tokio::sync::Semaphore::new(UPLOAD_SLOTS as usize)),
            sessions: Mutex::new(Vec::new()),
            next_session: AtomicU64::new(1),
            sync_locks: Mutex::new(HashMap::new()),
            lan: OnceLock::new(),
            dht: OnceLock::new(),
            observed: Mutex::new(HashMap::new()),
            external: Mutex::new(None),
        }))
    }

    /// Attach the running DHT route. Later calls are ignored.
    pub fn set_dht(&self, dht: swarm_disc::dht::MainlineDht) {
        let _ = self.dht.set(dht);
    }

    /// The DHT route, unless `SWARM_NO_DHT=1` or it failed to start.
    pub fn dht(&self) -> Option<swarm_disc::dht::MainlineDht> {
        self.dht.get().cloned()
    }

    /// Count the address a peer says it sees for us (6.5). Two different
    /// peers reporting the same IP make it our external address.
    fn observe(&self, info: &swarm_net::PeerInfo) {
        let Ok((addr, _)) = PeerAddr::decode(&info.observed) else {
            return;
        };
        let ip = addr.socket().ip();
        if ip.is_unspecified() {
            return;
        }
        let reporters = {
            let mut observed = self
                .observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if observed.len() >= 64 && !observed.contains_key(&ip) {
                return;
            }
            let set = observed.entry(ip).or_default();
            set.insert(*info.node_id.as_bytes());
            set.len()
        };
        if reporters >= 2 {
            let mut external = self
                .external
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *external != Some(ip) {
                tracing::debug!(ip = %ip, "external address confirmed by two peers");
                *external = Some(ip);
            }
        }
    }

    /// Our external IP, once two distinct peers agree on it.
    pub fn external_ip(&self) -> Option<IpAddr> {
        *self
            .external
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The one-hint link this node advertises for `repo`.
    pub fn link(&self, repo: RepoId) -> swarm_core::ids::Link {
        let configured = std::env::var("SWARM_ADVERTISE")
            .ok()
            .filter(|value| !value.is_empty());
        let listen_ip = self.listen.ip();
        let host = match (configured, self.external_ip()) {
            (Some(host), _) => Some(host),
            (None, Some(ip)) if listen_ip.is_unspecified() && !ip.is_loopback() => Some(match ip {
                IpAddr::V4(ip) => ip.to_string(),
                IpAddr::V6(ip) => format!("[{ip}]"),
            }),
            _ => None,
        };
        link_with_hint(repo, self.listen, self.node_id, host.as_deref())
    }

    /// Live sessions that carry `repo`, inbound or outbound.
    pub fn sessions_for(&self, repo: RepoId) -> Vec<SessionHandle> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions
            .iter()
            .filter(|session| !session.handle.is_closed())
            .filter(|session| session.shared.iter().any(|(shared, _)| *shared == repo))
            .map(|session| session.handle.clone())
            .collect()
    }

    /// Attach the running LAN route. Later calls are ignored.
    pub fn set_lan(&self, lan: Arc<swarm_disc::lan::LanService>) {
        let _ = self.lan.set(lan);
    }

    /// The LAN route, unless `SWARM_NO_LAN=1` or it failed to bind.
    pub fn lan(&self) -> Option<Arc<swarm_disc::lan::LanService>> {
        self.lan.get().cloned()
    }

    /// Every repo with a database row (LAN-1).
    pub fn known_repos(&self) -> Vec<RepoId> {
        let Ok(db) = self.db.lock() else {
            return Vec::new();
        };
        db.repos()
            .unwrap_or_default()
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// A per-repo lock that serializes sync (SYNC-5).
    pub fn sync_lock(&self, repo: RepoId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .sync_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            locks
                .entry(repo.to_bytes())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Register a session for `Announce` forwarding. Returns its ID.
    pub fn register(&self, handle: SessionHandle) -> u64 {
        let id = self.next_session.fetch_add(1, Ordering::SeqCst);
        self.observe(handle.info());
        let shared = handle.info().shared.clone();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.push(Registered { handle, shared, id });
        id
    }

    /// Remove a session.
    pub fn unregister(&self, id: u64) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|session| session.id != id);
    }

    /// Send `Announce` to every session that shares `repo` (7.4).
    pub async fn announce(&self, repo: RepoId, envelope: Vec<u8>) {
        let targets: Vec<(SessionHandle, u16)> = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            sessions
                .iter()
                .filter_map(|session| {
                    session
                        .shared
                        .iter()
                        .find(|(shared, _)| *shared == repo)
                        .map(|(_, ix)| (session.handle.clone(), *ix))
                })
                .collect()
        };
        for (handle, ix) in targets {
            let message = swarm_net::Message::Announce {
                repo_ix: ix,
                envelope: envelope.clone(),
            };
            let _ = handle.send(&message).await;
        }
    }

    /// Repos the daemon serves: seeded rows with an existing store.
    pub fn serving_repos(&self) -> Vec<RepoId> {
        let rows = {
            let Ok(db) = self.db.lock() else {
                return Vec::new();
            };
            db.repos().unwrap_or_default()
        };
        rows.into_iter()
            .filter(|row| row.seeding)
            .filter(|row| RepoStore::new(self.home.clone(), row.id).exists())
            .map(|row| row.id)
            .take(16)
            .collect()
    }

    /// Bare store for a repo.
    #[must_use]
    pub fn store(&self, repo: RepoId) -> RepoStore {
        RepoStore::new(self.home.clone(), repo)
    }

    /// Record a successful peer contact with its node ID (NET-11).
    pub fn peer_ok(&self, repo: RepoId, addr: &PeerAddr, node: NodeId) {
        if let Ok(db) = self.db.lock() {
            let _ = db.record_peer_ok(repo, &addr.to_string(), node.as_bytes(), now());
        }
    }

    /// Record a failed peer contact.
    pub fn peer_fail(&self, repo: RepoId, addr: &PeerAddr) {
        if let Ok(db) = self.db.lock() {
            let _ = db.record_peer_fail(repo, &addr.to_string());
        }
    }
}

/// Unix seconds.
#[must_use]
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse `SWARM_LISTEN`, or use the default from the spec.
#[must_use]
pub fn configured_listen() -> SocketAddr {
    std::env::var("SWARM_LISTEN")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 42424))
}

/// Largest served or accepted pack, with a test override.
#[must_use]
pub fn max_pack_bytes() -> usize {
    std::env::var("SWARM_MAX_PACK_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2 * 1024 * 1024 * 1024)
}

/// Bind the listener, falling back to an ephemeral port when busy (NET-1).
pub async fn bind(configured: SocketAddr) -> std::io::Result<(TcpListener, SocketAddr)> {
    match TcpListener::bind(configured).await {
        Ok(listener) => {
            let addr = listener.local_addr()?;
            Ok((listener, addr))
        }
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let fallback = SocketAddr::new(configured.ip(), 0);
            let listener = TcpListener::bind(fallback).await?;
            let addr = listener.local_addr()?;
            Ok((listener, addr))
        }
        Err(err) => Err(err),
    }
}

/// Write `daemon.json` with the actual port (NET-1).
pub fn write_daemon_json(home: &Home, listen: SocketAddr) -> anyhow::Result<()> {
    let value = serde_json::json!({
        "port": listen.port(),
        "pid": std::process::id(),
        "listen": listen.to_string(),
    });
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    std::fs::write(home.daemon_json_path(), bytes)?;
    Ok(())
}

/// Host to put in a link hint.
///
/// `SWARM_ADVERTISE` wins, then a concrete `SWARM_LISTEN` host, then the
/// machine's primary IPv4 address. This is what makes `swarm link` useful
/// across two containers without any discovery route.
#[must_use]
pub fn advertise_host(listen: SocketAddr) -> String {
    if let Ok(value) = std::env::var("SWARM_ADVERTISE") {
        if !value.is_empty() {
            return value;
        }
    }
    let ip = listen.ip();
    if !ip.is_unspecified() && !ip.is_loopback() {
        return match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
    }
    if ip.is_loopback() {
        return match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(_) => "[::1]".to_string(),
        };
    }
    primary_ipv4()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

pub(crate) fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// Build the one-hint link this node advertises.
#[must_use]
pub fn link_with_hint(
    repo: RepoId,
    listen: SocketAddr,
    node: NodeId,
    advertise: Option<&str>,
) -> swarm_core::ids::Link {
    let host_text = advertise
        .map(str::to_string)
        .unwrap_or_else(|| advertise_host(listen));
    let host = if let Some(inner) = host_text
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        match inner.parse::<std::net::Ipv6Addr>() {
            Ok(ip) => Host::Ipv6(ip),
            Err(_) => Host::Name(inner.to_string()),
        }
    } else if let Ok(ip) = host_text.parse::<Ipv4Addr>() {
        Host::Ipv4(ip)
    } else {
        Host::Name(host_text)
    };
    swarm_core::ids::Link {
        repo,
        hints: vec![Hint {
            scheme: HintScheme::Tcp,
            host,
            port: listen.port(),
            node: Some(node),
        }],
    }
}
