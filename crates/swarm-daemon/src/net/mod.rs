//! Daemon network state: listener address, node key, peer sessions (M2),
//! and the DHT route, observed addresses and port mapping (M4, M5).

pub mod capped;
pub mod dht;
pub mod discovery;
pub mod limits;
pub mod onion;
pub mod punch;
pub mod relay;
pub mod serve;
pub mod settings;
pub mod topics;
pub mod upnp;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use swarm_core::ids::{Hint, HintScheme, Host, NodeId, RepoId};
use swarm_core::keyfile::NodeKey;
use swarm_net::transport::Transport;
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

/// The hidden service, when one is configured.
#[derive(Clone, Debug, Default)]
pub struct OnionState {
    /// `<id>.onion` while the service is up.
    pub host: Option<String>,
    /// Why it is not.
    pub error: Option<String>,
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
    /// Proxy, obfuscation, hidden service and the other M9 settings.
    pub settings: settings::Settings,
    /// Opens and shapes peer connections.
    pub transport: Arc<dyn Transport>,
    /// Upload rate shared by every peer (`upload_kbps`).
    pub throttle: limits::Throttle,
    /// Inbound caps and the ban list.
    pub limits: Arc<limits::Limits>,
    /// UDP punches this node is answering.
    pub punches: Arc<tokio::sync::Semaphore>,
    onion: Mutex<OnionState>,
    sessions: Mutex<Vec<Registered>>,
    next_session: AtomicU64,
    sync_locks: Mutex<HashMap<[u8; 32], Arc<tokio::sync::Mutex<()>>>>,
    /// Announces being handled (7.4).
    pub announcing: serve::Announcing,
    lan: OnceLock<Arc<swarm_disc::lan::LanService>>,
    dht: OnceLock<swarm_disc::dht::MainlineDht>,
    observed: Mutex<HashMap<IpAddr, HashSet<[u8; 32]>>>,
    external: Mutex<Option<IpAddr>>,
    relays: Mutex<Vec<relay::RelayStatus>>,
    /// Counts relay registrations made, so topics are published to each anew.
    registrations: AtomicU64,
    discovery: OnceLock<Arc<discovery::Discovery>>,
    /// When a direct inbound connection was last shown to work.
    reachable: Mutex<Option<std::time::Instant>>,
    /// Nodes we currently reach only through a relay.
    relayed: Mutex<HashMap<NodeId, String>>,
    /// Per repo, how many other nodes returned its state at the last check,
    /// and when that was. Not persisted: unknown after a restart.
    seeders: Mutex<HashMap<RepoId, (usize, std::time::Instant)>>,
    /// Repos the background check has queued or is checking.
    checking: Mutex<std::collections::HashSet<RepoId>>,
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
        let settings = settings::Settings::from_env()?;
        Ok(Arc::new(Self {
            home,
            db: Arc::clone(&db),
            node,
            node_id,
            listen,
            max_pack_bytes,
            uploads: Arc::new(tokio::sync::Semaphore::new(UPLOAD_SLOTS as usize)),
            transport: settings.transport(),
            limits: limits::Limits::new(Arc::clone(&db), settings.onion_limits),
            throttle: limits::Throttle::new(settings.upload_kbps),
            punches: Arc::new(tokio::sync::Semaphore::new(punch::MAX_ANSWERING)),
            onion: Mutex::new(OnionState::default()),
            settings,
            sessions: Mutex::new(Vec::new()),
            next_session: AtomicU64::new(1),
            sync_locks: Mutex::new(HashMap::new()),
            announcing: serve::Announcing::default(),
            lan: OnceLock::new(),
            dht: OnceLock::new(),
            observed: Mutex::new(HashMap::new()),
            external: Mutex::new(None),
            relays: Mutex::new(Vec::new()),
            registrations: AtomicU64::new(0),
            discovery: OnceLock::new(),
            reachable: Mutex::new(None),
            relayed: Mutex::new(HashMap::new()),
            seeders: Mutex::new(HashMap::new()),
            checking: Mutex::default(),
        }))
    }

    /// Record the result of a check for other seeders of `repo`.
    pub fn set_seeders(&self, repo: RepoId, count: usize) {
        self.seeders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(repo, (count, std::time::Instant::now()));
    }

    /// Other seeders of `repo` and the age of that count, once checked.
    #[must_use]
    pub fn seeders(&self, repo: RepoId) -> Option<(usize, Duration)> {
        self.seeders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&repo)
            .map(|(count, when)| (*count, when.elapsed()))
    }

    /// Mark `repo` as waiting for, or done with, its background check.
    pub fn set_checking(&self, repo: RepoId, checking: bool) {
        let mut set = self
            .checking
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if checking {
            set.insert(repo);
        } else {
            set.remove(&repo);
        }
    }

    /// Whether the background check of `repo` is still to finish.
    #[must_use]
    pub fn is_checking(&self, repo: RepoId) -> bool {
        self.checking
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&repo)
    }

    /// Handshake limits and shaping for a new session.
    #[must_use]
    pub fn handshake_options(&self, timeout: Duration) -> swarm_net::Options {
        let mut extra_caps = Vec::new();
        if self.settings.udp {
            extra_caps.push(punch::CAP.to_string());
        }
        swarm_net::Options {
            timeout,
            shaper: self.transport.shaper(),
            extra_caps,
        }
    }

    /// The hidden service state.
    #[must_use]
    pub fn onion(&self) -> OnionState {
        self.onion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_onion(&self, host: Option<String>) {
        let mut onion = self
            .onion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *onion = OnionState { host, error: None };
    }

    fn set_onion_error(&self, error: String) {
        let mut onion = self
            .onion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *onion = OnionState {
            host: None,
            error: Some(error),
        };
    }

    /// The `onion:` hint for this node while its hidden service is up.
    fn onion_hint(&self) -> Option<Hint> {
        Some(Hint {
            scheme: HintScheme::Onion,
            host: Host::parse(&self.onion().host?)?,
            port: self.listen.port(),
            node: Some(self.node_id),
        })
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
        let Some(ip) = addr.socket().map(|socket| socket.ip()) else {
            return;
        };
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
        let mut link = link_with_hint(repo, self.listen, self.node_id, host.as_deref());
        // Behind a proxy our own addresses stay out of links, unless the
        // user names one with `advertise`.
        let advertised = std::env::var("SWARM_ADVERTISE").is_ok_and(|value| !value.is_empty());
        if self.settings.proxied() && !advertised {
            link.hints.clear();
        }
        link.hints.extend(self.onion_hint());
        // REL-8: a node reached through a relay says so in its link.
        link.hints.extend(self.relay_hints());
        link
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

    /// Close every session that shares `repo`, for a repo that is going away.
    pub fn close_sessions(&self, repo: RepoId) {
        for handle in self.sessions_for(repo) {
            handle.close();
        }
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

    /// Send a new name and description to every session that shares `repo`
    /// and understands it.
    pub async fn announce_about(&self, repo: RepoId, record: Vec<u8>) {
        for handle in self.sessions_for(repo) {
            let capable = handle
                .info()
                .caps
                .iter()
                .any(|cap| cap == swarm_net::message::ABOUT_CAP);
            let Some(repo_ix) = handle.shared_repo(repo).filter(|_| capable) else {
                continue;
            };
            let message = swarm_net::Message::AnnounceAbout {
                repo_ix,
                record: record.clone(),
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
        .unwrap_or_else(|| {
            let proxied = !matches!(swarm_net::Proxy::from_env(), Ok(None));
            settings::Settings::default_listen(proxied)
        })
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
    // No packet is sent here, but behind a proxy we do not look at all.
    if !matches!(swarm_net::Proxy::from_env(), Ok(None)) {
        return None;
    }
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
