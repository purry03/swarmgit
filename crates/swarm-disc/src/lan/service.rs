//! The LAN multicast route at runtime (spec 6.3): sockets, announce timer,
//! query bursts and the candidate table the sync loop reads.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use socket2::{Domain, Protocol, SockRef, Socket, Type};
use swarm_core::crypto::hour;
use swarm_core::ids::RepoId;
use tokio::net::UdpSocket;
use tokio::sync::Notify;

use super::packet::{build_packets, match_repos, Kind, LanPacket, RateLimiter};

/// IPv4 multicast group.
pub const GROUP_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 99);

/// IPv6 link-local multicast group.
pub const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0x4242);

/// Default UDP port.
pub const DEFAULT_PORT: u16 = 42099;

/// Announce interval for seeded repos.
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(60);

/// Query burst offsets from the start of a sync: 0 s, 1 s and 3 s.
pub const QUERY_BURST: [Duration; 3] = [
    Duration::from_secs(0),
    Duration::from_secs(1),
    Duration::from_secs(3),
];

/// How long a LAN candidate stays usable without a fresh announce.
const CANDIDATE_TTL: Duration = Duration::from_secs(300);

/// How long a queried repo keeps matching announces.
const INTEREST_TTL: Duration = Duration::from_secs(120);

/// Most candidates remembered per repo.
const MAX_CANDIDATES: usize = 32;

/// Repos the service needs to know about, read on every packet and tick.
#[derive(Clone, Debug, Default)]
pub struct LanRepos {
    /// Every repo this node has a row or store for (LAN-1).
    pub known: Vec<RepoId>,
    /// Repos this node seeds and can serve (LAN-2, announces).
    pub seeded: Vec<RepoId>,
}

/// Callback that lists the node's repos.
pub type RepoSource = Arc<dyn Fn() -> LanRepos + Send + Sync>;

/// Static configuration.
#[derive(Clone, Copy, Debug)]
pub struct LanConfig {
    /// UDP port, 42099 unless `SWARM_LAN_PORT` overrides it.
    pub port: u16,
    /// Our TCP listening port, carried in every packet.
    pub tcp_port: u16,
}

impl LanConfig {
    /// Read `SWARM_LAN_PORT` (LAN-4).
    #[must_use]
    pub fn from_env(tcp_port: u16) -> Self {
        let port = std::env::var("SWARM_LAN_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        Self { port, tcp_port }
    }
}

#[derive(Default)]
struct Inner {
    candidates: HashMap<[u8; 32], Vec<(SocketAddr, Instant)>>,
    interest: HashMap<[u8; 32], Instant>,
    own_ips: HashSet<IpAddr>,
    v4_ifaces: Vec<Ipv4Addr>,
    v6_ifaces: Vec<u32>,
    limiter: RateLimiter,
}

/// A running LAN route.
pub struct LanService {
    config: LanConfig,
    repos: RepoSource,
    v4: Option<UdpSocket>,
    v6: Option<UdpSocket>,
    inner: Mutex<Inner>,
    found: Notify,
    send_lock: tokio::sync::Mutex<()>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn bind_v4(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    UdpSocket::from_std(socket.into())
}

fn bind_v6(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_multicast_hops_v6(1)?;
    socket.set_multicast_loop_v6(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0)).into())?;
    UdpSocket::from_std(socket.into())
}

/// Interfaces and local addresses, read from the OS.
struct Interfaces {
    own: HashSet<IpAddr>,
    v4: Vec<Ipv4Addr>,
    v6: Vec<u32>,
}

fn interfaces() -> Interfaces {
    let mut out = Interfaces {
        own: HashSet::new(),
        v4: Vec::new(),
        v6: Vec::new(),
    };
    let Ok(list) = if_addrs::get_if_addrs() else {
        return out;
    };
    for iface in list {
        let ip = iface.ip();
        out.own.insert(ip);
        match ip {
            IpAddr::V4(v4) => {
                if !out.v4.contains(&v4) {
                    out.v4.push(v4);
                }
            }
            IpAddr::V6(_) => {
                if let Some(index) = iface.index {
                    if !iface.is_loopback() && !out.v6.contains(&index) {
                        out.v6.push(index);
                    }
                }
            }
        }
    }
    out
}

impl LanService {
    /// Bind the sockets, join the groups and start the receive loops and the
    /// announce timer. Fails only if neither IPv4 nor IPv6 can be bound.
    pub async fn start(config: LanConfig, repos: RepoSource) -> io::Result<Arc<Self>> {
        let v4 = bind_v4(config.port)
            .map_err(|err| tracing::debug!(error = %err, "lan ipv4 bind failed"))
            .ok();
        let v6 = bind_v6(config.port)
            .map_err(|err| tracing::debug!(error = %err, "lan ipv6 bind failed"))
            .ok();
        if v4.is_none() && v6.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "cannot bind the LAN port",
            ));
        }
        let service = Arc::new(Self {
            config,
            repos,
            v4,
            v6,
            inner: Mutex::new(Inner::default()),
            found: Notify::new(),
            send_lock: tokio::sync::Mutex::new(()),
        });
        service.refresh_interfaces();
        if service.v4.is_some() {
            let me = Arc::clone(&service);
            tokio::spawn(async move { me.recv_loop(false).await });
        }
        if service.v6.is_some() {
            let me = Arc::clone(&service);
            tokio::spawn(async move { me.recv_loop(true).await });
        }
        let me = Arc::clone(&service);
        tokio::spawn(async move { me.announce_loop().await });
        Ok(service)
    }

    /// The configured UDP port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.config.port
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Re-read interfaces and join the groups on any new ones.
    fn refresh_interfaces(&self) {
        let found = interfaces();
        let mut inner = self.lock();
        if let Some(socket) = &self.v4 {
            let sock = SockRef::from(socket);
            for ip in &found.v4 {
                if inner.v4_ifaces.contains(ip) {
                    continue;
                }
                match sock.join_multicast_v4(&GROUP_V4, ip) {
                    Ok(()) => inner.v4_ifaces.push(*ip),
                    Err(err) => tracing::debug!(iface = %ip, error = %err, "lan join failed"),
                }
            }
        }
        if let Some(socket) = &self.v6 {
            let sock = SockRef::from(socket);
            for index in &found.v6 {
                if inner.v6_ifaces.contains(index) {
                    continue;
                }
                match sock.join_multicast_v6(&GROUP_V6, *index) {
                    Ok(()) => inner.v6_ifaces.push(*index),
                    Err(err) => tracing::debug!(iface = index, error = %err, "lan join failed"),
                }
            }
        }
        inner.own_ips = found.own;
    }

    async fn announce_loop(self: Arc<Self>) {
        loop {
            self.refresh_interfaces();
            self.announce_now().await;
            tokio::time::sleep(ANNOUNCE_INTERVAL).await;
        }
    }

    /// Multicast an announce for every seeded repo now.
    pub async fn announce_now(&self) {
        let seeded = (self.repos)().seeded;
        if seeded.is_empty() {
            return;
        }
        let packets = build_packets(
            Kind::Announce,
            self.config.tcp_port,
            &seeded,
            hour(unix_now()),
        );
        for packet in packets {
            self.multicast(&packet).await;
        }
    }

    /// Start a query burst for `repo` (0 s, 1 s, 3 s) and match its announces
    /// for the next two minutes even if the repo has no row yet.
    pub fn query(self: &Arc<Self>, repo: RepoId) {
        self.lock()
            .interest
            .insert(repo.to_bytes(), Instant::now() + INTEREST_TTL);
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            for offset in QUERY_BURST {
                tokio::time::sleep_until(started + offset).await;
                let packets =
                    build_packets(Kind::Query, me.config.tcp_port, &[repo], hour(unix_now()));
                for packet in packets {
                    me.multicast(&packet).await;
                }
            }
        });
    }

    /// Current TCP candidates for `repo`, newest first.
    #[must_use]
    pub fn candidates(&self, repo: RepoId) -> Vec<SocketAddr> {
        let now = Instant::now();
        let mut inner = self.lock();
        let Some(list) = inner.candidates.get_mut(repo.as_bytes()) else {
            return Vec::new();
        };
        list.retain(|(_, seen)| now.saturating_duration_since(*seen) < CANDIDATE_TTL);
        let mut sorted = list.clone();
        sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        sorted.into_iter().map(|(addr, _)| addr).collect()
    }

    /// Wait until a new candidate arrives for any repo, or `timeout` passes.
    pub async fn wait_for_candidates(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.found.notified()).await;
    }

    async fn multicast(&self, packet: &LanPacket) {
        let Some(bytes) = packet.encode() else {
            return;
        };
        let _guard = self.send_lock.lock().await;
        let (v4_ifaces, v6_ifaces) = {
            let inner = self.lock();
            (inner.v4_ifaces.clone(), inner.v6_ifaces.clone())
        };
        if let Some(socket) = &self.v4 {
            let target = SocketAddr::V4(SocketAddrV4::new(GROUP_V4, self.config.port));
            for ip in &v4_ifaces {
                if SockRef::from(socket).set_multicast_if_v4(ip).is_err() {
                    continue;
                }
                if let Err(err) = socket.send_to(&bytes, target).await {
                    tracing::debug!(iface = %ip, error = %err, "lan send failed");
                }
            }
        }
        if let Some(socket) = &self.v6 {
            for index in &v6_ifaces {
                if SockRef::from(socket).set_multicast_if_v6(*index).is_err() {
                    continue;
                }
                let target =
                    SocketAddr::V6(SocketAddrV6::new(GROUP_V6, self.config.port, 0, *index));
                if let Err(err) = socket.send_to(&bytes, target).await {
                    tracing::debug!(iface = index, error = %err, "lan send failed");
                }
            }
        }
    }

    async fn recv_loop(self: Arc<Self>, v6: bool) {
        let socket = if v6 { &self.v6 } else { &self.v4 };
        let Some(socket) = socket else {
            return;
        };
        let mut buf = vec![0u8; 2048];
        loop {
            let (len, source) = match socket.recv_from(&mut buf).await {
                Ok(received) => received,
                Err(err) => {
                    tracing::debug!(error = %err, "lan receive failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            self.handle(&buf[..len], source, socket).await;
        }
    }

    async fn handle(&self, bytes: &[u8], source: SocketAddr, socket: &UdpSocket) {
        let ip = canonical_ip(source.ip());
        {
            let mut inner = self.lock();
            if !inner.limiter.allow(ip, Instant::now()) {
                return;
            }
        }
        let Ok(packet) = LanPacket::decode(bytes) else {
            return;
        };
        // LAN-3: drop our own packets. A packet from one of our addresses with
        // a different TCP port is another daemon on this machine (LAN-4).
        if packet.tcp_port == self.config.tcp_port && self.lock().own_ips.contains(&ip) {
            return;
        }
        if packet.tcp_port == 0 {
            return;
        }
        let now_hour = hour(unix_now());
        match packet.kind {
            Kind::Announce => {
                let known = self.known_repos();
                let matched = match_repos(&packet, &known, now_hour);
                if matched.is_empty() {
                    return;
                }
                let candidate = tcp_candidate(source, packet.tcp_port);
                self.record(&matched, candidate);
            }
            Kind::Query => {
                let seeded = (self.repos)().seeded;
                let matched = match_repos(&packet, &seeded, now_hour);
                if matched.is_empty() {
                    return;
                }
                // LAN-2: unicast announce back to the querier's LAN port.
                let target = match source {
                    SocketAddr::V4(addr) => {
                        SocketAddr::V4(SocketAddrV4::new(*addr.ip(), self.config.port))
                    }
                    SocketAddr::V6(addr) => SocketAddr::V6(SocketAddrV6::new(
                        *addr.ip(),
                        self.config.port,
                        0,
                        addr.scope_id(),
                    )),
                };
                for reply in build_packets(Kind::Announce, self.config.tcp_port, &matched, now_hour)
                {
                    if let Some(bytes) = reply.encode() {
                        if let Err(err) = socket.send_to(&bytes, target).await {
                            tracing::debug!(peer = %target, error = %err, "lan reply failed");
                        }
                    }
                }
            }
        }
    }

    fn known_repos(&self) -> Vec<RepoId> {
        let mut known = (self.repos)().known;
        let now = Instant::now();
        let mut inner = self.lock();
        inner.interest.retain(|_, until| *until > now);
        for bytes in inner.interest.keys() {
            let repo = RepoId::from_bytes(*bytes);
            if !known.contains(&repo) {
                known.push(repo);
            }
        }
        known
    }

    fn record(&self, repos: &[RepoId], candidate: SocketAddr) {
        let now = Instant::now();
        let mut fresh = false;
        {
            let mut inner = self.lock();
            for repo in repos {
                let list = inner.candidates.entry(repo.to_bytes()).or_default();
                if let Some(entry) = list.iter_mut().find(|(addr, _)| *addr == candidate) {
                    entry.1 = now;
                    continue;
                }
                if list.len() >= MAX_CANDIDATES {
                    list.sort_by_key(|entry| std::cmp::Reverse(entry.1));
                    list.truncate(MAX_CANDIDATES - 1);
                }
                list.push((candidate, now));
                fresh = true;
                tracing::debug!(repo = %repo.to_base32(), peer = %candidate, "lan candidate");
            }
        }
        if fresh {
            self.found.notify_waiters();
        }
    }
}

/// Map IPv4-mapped IPv6 sources to plain IPv4.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

/// `(source IP, tcp_port)`, keeping the IPv6 scope for link-local sources.
fn tcp_candidate(source: SocketAddr, tcp_port: u16) -> SocketAddr {
    match source {
        SocketAddr::V4(addr) => SocketAddr::V4(SocketAddrV4::new(*addr.ip(), tcp_port)),
        SocketAddr::V6(addr) => {
            SocketAddr::V6(SocketAddrV6::new(*addr.ip(), tcp_port, 0, addr.scope_id()))
        }
    }
}
