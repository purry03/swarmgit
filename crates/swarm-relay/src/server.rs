//! The relay server (design spec sections 4 and 7).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use swarm_core::crypto::random_bytes;
use swarm_net::udp;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot, watch};

pub use crate::config::{Limits, RelayConfig};
use crate::directory::Directory;
use crate::proto::{Msg, Role, VERSION};
use crate::quota::{address_key, Quotas};
use crate::secure::{Channel, Receiver, RelayKey, Sender};
use crate::topics::Topics;
use crate::{RelayError, FULL};

struct Entry {
    gen: u64,
    ip: IpAddr,
    tx: mpsc::Sender<[u8; 16]>,
}

struct Pending {
    node: [u8; 32],
    reply: oneshot::Sender<TcpStream>,
}

/// Counters only; nothing identifying is kept (REL-7).
#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) splices_total: AtomicU64,
    pub(crate) bytes: AtomicU64,
    pub(crate) auth_failures: AtomicU64,
    errors: AtomicU64,
}

pub(crate) struct State {
    pub(crate) config: RelayConfig,
    listeners: Mutex<HashMap<[u8; 32], Entry>>,
    pending: Mutex<HashMap<[u8; 16], Pending>>,
    next_gen: AtomicU64,
    pub(crate) splices: AtomicUsize,
    pub(crate) counters: Counters,
    pub(crate) quotas: Arc<Quotas>,
    shutdown: watch::Sender<bool>,
    pub(crate) topics: Topics,
    /// The static key. Absent only if none could be made, which leaves
    /// control connections in the clear.
    pub(crate) key: Option<RelayKey>,
    /// The public key, or random bytes without one. It identifies the relay
    /// in the relay network.
    pub(crate) relay_id: [u8; 32],
    /// Set once the relay joins the relay network (REL-33).
    pub(crate) network: OnceLock<Arc<Directory>>,
    /// Recent dial-back checks by caller address.
    pub(crate) dial_backs: Mutex<HashMap<IpAddr, Instant>>,
    /// When "relay is full" was last logged.
    said_full: Mutex<Option<Instant>>,
}

/// Least time between "relay is full" log lines.
const FULL_LOG_EVERY: Duration = Duration::from_secs(60);

/// A relay. Cheap to clone.
#[derive(Clone)]
pub struct Relay {
    pub(crate) state: Arc<State>,
}

/// A snapshot of relay counters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Registered listeners.
    pub listeners: usize,
    /// Splices running now.
    pub splices: usize,
    /// Splices started since the relay began.
    pub splices_total: u64,
    /// Bytes copied since the relay began.
    pub bytes: u64,
    /// Failed listener proofs and passwords.
    pub auth_failures: u64,
    /// Connections that ended in an error, refusals included.
    pub errors: u64,
}

/// Resolve once shutdown is requested.
pub(crate) async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stopped| *stopped).await;
}

pub(crate) async fn send_error(stream: &mut Channel, code: &str, msg: &str) {
    let _ = stream
        .send(&Msg::Error {
            code: code.into(),
            msg: msg.into(),
        })
        .await;
}

impl Relay {
    /// Create a relay.
    #[must_use]
    pub fn new(config: RelayConfig) -> Self {
        let key = config.key.clone().or_else(|| RelayKey::generate().ok());
        Self {
            state: Arc::new(State {
                config,
                listeners: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                next_gen: AtomicU64::new(1),
                splices: AtomicUsize::new(0),
                counters: Counters::default(),
                quotas: Arc::new(Quotas::default()),
                shutdown: watch::channel(false).0,
                relay_id: key
                    .as_ref()
                    .map(RelayKey::public)
                    .or_else(|| random_bytes::<32>().ok())
                    .unwrap_or_default(),
                key,
                topics: Topics::default(),
                network: OnceLock::new(),
                dial_backs: Mutex::new(HashMap::new()),
                said_full: Mutex::default(),
            }),
        }
    }

    /// Current counters.
    #[must_use]
    pub fn stats(&self) -> Stats {
        Stats {
            listeners: self.state.listeners.lock().map(|l| l.len()).unwrap_or(0),
            splices: self.state.splices.load(Ordering::SeqCst),
            splices_total: self.state.counters.splices_total.load(Ordering::SeqCst),
            bytes: self.state.counters.bytes.load(Ordering::SeqCst),
            auth_failures: self.state.counters.auth_failures.load(Ordering::SeqCst),
            errors: self.state.counters.errors.load(Ordering::SeqCst),
        }
    }

    /// This process's identifier in the relay network.
    #[must_use]
    pub fn relay_id(&self) -> [u8; 32] {
        self.state.relay_id
    }

    /// True when `node` has a live registration here.
    pub(crate) fn has_listener(&self, node: &[u8; 32]) -> bool {
        self.state
            .listeners
            .lock()
            .is_ok_and(|listeners| listeners.contains_key(node))
    }

    /// A receiver that turns true on [`Relay::shutdown`].
    pub(crate) fn stop_signal(&self) -> watch::Receiver<bool> {
        self.state.shutdown.subscribe()
    }

    /// Stop accepting and drop every open connection. [`Relay::serve`]
    /// returns and releases its listening socket.
    pub fn shutdown(&self) {
        let _ = self.state.shutdown.send(true);
    }

    /// Accept connections until [`Relay::shutdown`] is called or the task is
    /// dropped or aborted.
    pub async fn serve(&self, listener: TcpListener) {
        if let Ok(addr) = listener.local_addr() {
            self.start_reflector(addr).await;
        }
        let mut stop = self.state.shutdown.subscribe();
        loop {
            let (stream, peer) = tokio::select! {
                () = stopped(&mut stop) => return,
                accepted = listener.accept() => match accepted {
                    Ok(pair) => pair,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                },
            };
            let relay = self.clone();
            let mut stop = self.state.shutdown.subscribe();
            tokio::spawn(async move {
                let _ = stream.set_nodelay(true);
                tokio::select! {
                    () = stopped(&mut stop) => {}
                    outcome = relay.handle(stream, peer) => {
                        if let Err(err) = outcome {
                            relay.state.counters.errors.fetch_add(1, Ordering::SeqCst);
                            tracing::debug!("relay connection {peer}: {err}");
                        }
                    }
                }
            });
        }
    }

    /// Answer UDP "what is my address" requests on the listening port, for
    /// clients trying experimental hole punching. A reply is smaller than its
    /// request, so this cannot amplify traffic. Without it the relay works as
    /// before.
    async fn start_reflector(&self, addr: SocketAddr) {
        let socket = match UdpSocket::bind(addr).await {
            Ok(socket) => socket,
            Err(err) => {
                tracing::warn!("no UDP reflector on {addr}: {err}");
                return;
            }
        };
        let mut stop = self.state.shutdown.subscribe();
        tokio::spawn(async move {
            let mut buffer = [0u8; 128];
            loop {
                let received = tokio::select! {
                    () = stopped(&mut stop) => return,
                    received = socket.recv_from(&mut buffer) => received,
                };
                let Ok((len, from)) = received else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                if let Some(reply) = udp::reflect_reply(&buffer[..len], from) {
                    let _ = socket.send_to(&reply, from).await;
                }
            }
        });
    }

    async fn handle(&self, stream: TcpStream, peer: SocketAddr) -> Result<(), RelayError> {
        let limits = &self.state.config.limits;
        let first = async {
            let mut channel = match &self.state.key {
                Some(key) => Channel::respond(stream, key).await?,
                None => Channel::plain(stream),
            };
            let hello = channel.recv().await?;
            Ok::<_, RelayError>((channel, hello))
        };
        let (mut stream, hello) = tokio::time::timeout(limits.hello_timeout, first)
            .await
            .map_err(|_| RelayError::Timeout("hello"))??;
        let (version, role, password) = match hello {
            Msg::Hello { version, role } => (version, role, None),
            Msg::HelloAuth {
                version,
                role,
                password,
            } => (version, role, Some(password)),
            _ => {
                send_error(&mut stream, "malformed", "expected RHello").await;
                return Err(RelayError::Protocol("expected RHello".into()));
            }
        };
        if version != VERSION {
            send_error(&mut stream, "unsupported_version", "version 1 only").await;
            return Err(RelayError::Protocol("unsupported version".into()));
        }
        // Only registering and dialing use the relay; accepting needs a
        // token that one of those produced.
        if matches!(role, Role::Listen | Role::Connect | Role::Publish)
            && !self.password_ok(password.as_deref())
        {
            self.state
                .counters
                .auth_failures
                .fetch_add(1, Ordering::SeqCst);
            send_error(&mut stream, "auth_failed", "this relay needs its password").await;
            return Err(RelayError::Auth("wrong or missing password".into()));
        }
        match role {
            Role::Listen => self.listen_session(stream, peer).await,
            Role::Connect => self.connect_session(stream, peer).await,
            Role::Accept => self.accept_session(stream).await,
            Role::Info => self.info_session(stream, peer).await,
            Role::Publish => self.publish_session(stream, peer).await,
        }
    }

    fn password_ok(&self, given: Option<&str>) -> bool {
        match &self.state.config.password {
            // Hash equality takes the same time wherever the two differ.
            Some(wanted) => given.is_some_and(|given| {
                blake3::hash(given.as_bytes()) == blake3::hash(wanted.as_bytes())
            }),
            None => true,
        }
    }

    /// Tell the operator the relay is full, at most once a minute.
    fn note_full(&self, what: &str, max: usize) {
        let Ok(mut said) = self.state.said_full.lock() else {
            return;
        };
        if said.is_some_and(|at| at.elapsed() < FULL_LOG_EVERY) {
            return;
        }
        *said = Some(Instant::now());
        tracing::warn!("relay is full: {max} {what}; refusing new ones");
    }

    /// The address `peer` is counted under, unless it is spared.
    pub(crate) fn counted(&self, peer: SocketAddr) -> Option<IpAddr> {
        let spared =
            peer.ip().to_canonical().is_loopback() && !self.state.config.limits.limit_loopback;
        (!spared).then(|| address_key(peer.ip()))
    }

    async fn listen_session(
        &self,
        mut stream: Channel,
        peer: SocketAddr,
    ) -> Result<(), RelayError> {
        let limits = self.state.config.limits.clone();
        let node = self.prove(&mut stream).await?;

        let allowed = &self.state.config.allow_nodes;
        if !allowed.is_empty() && !allowed.contains(&node) {
            send_error(&mut stream, "auth_failed", "node is not allowed here").await;
            return Err(RelayError::Auth("node not on the allow list".into()));
        }

        let gen = self.state.next_gen.fetch_add(1, Ordering::SeqCst);
        let (tx, mut incoming) = mpsc::channel::<[u8; 16]>(16);
        let refused = {
            let mut listeners = self
                .state
                .listeners
                .lock()
                .map_err(|_| RelayError::Protocol("state poisoned".into()))?;
            let new = !listeners.contains_key(&node);
            let crowded = self.counted(peer).is_some_and(|ip| {
                let held = listeners.values().filter(|entry| entry.ip == ip).count();
                held >= limits.listeners_per_ip
            });
            let full = new && listeners.len() >= limits.max_listeners;
            if full || (new && crowded) {
                Some(full)
            } else {
                // REL-2: a newer control connection replaces the older one.
                // The old entry's sender drops here, ending its control loop.
                let ip = address_key(peer.ip());
                listeners.insert(node, Entry { gen, ip, tx });
                None
            }
        };
        if let Some(full) = refused {
            let reason = if full {
                self.note_full("registered listeners", limits.max_listeners);
                FULL
            } else {
                "too many listeners"
            };
            send_error(&mut stream, "limit_exceeded", reason).await;
            return Err(RelayError::Protocol("listener limit".into()));
        }
        let result = self
            .control_loop(stream, peer, &mut incoming, limits.control_silence)
            .await;
        if let Ok(mut listeners) = self.state.listeners.lock() {
            if listeners.get(&node).is_some_and(|entry| entry.gen == gen) {
                listeners.remove(&node);
            }
        }
        result
    }

    async fn control_loop(
        &self,
        stream: Channel,
        peer: SocketAddr,
        incoming: &mut mpsc::Receiver<[u8; 16]>,
        silence: Duration,
    ) -> Result<(), RelayError> {
        let (mut read_half, mut write_half): (Receiver, Sender) = stream.into_split();
        write_half
            .send(&Msg::Listening {
                ttl_secs: silence.as_secs(),
                observed: peer.to_string(),
            })
            .await?;
        // Frames are read by a task because `read_msg` is not cancel safe
        // inside `select!`.
        let (frame_tx, mut frames) = mpsc::channel::<Result<Msg, RelayError>>(8);
        let reader = tokio::spawn(async move {
            loop {
                let msg = read_half.recv().await;
                let stop = msg.is_err();
                if frame_tx.send(msg).await.is_err() || stop {
                    break;
                }
            }
        });
        let mut deadline = Instant::now() + silence;
        let outcome = loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline.into()) => {
                    break Err(RelayError::Timeout("control connection silent"));
                }
                frame = frames.recv() => match frame {
                    Some(Ok(Msg::Ping { nonce })) => {
                        deadline = Instant::now() + silence;
                        if let Err(err) = write_half.send(&Msg::Pong { nonce }).await {
                            break Err(err);
                        }
                    }
                    Some(Ok(Msg::Pong { .. })) => deadline = Instant::now() + silence,
                    Some(Ok(_)) => break Err(RelayError::Protocol("unexpected message".into())),
                    Some(Err(err)) => break Err(err),
                    None => break Ok(()),
                },
                token = incoming.recv() => match token {
                    Some(token) => {
                        if let Err(err) = write_half.send(&Msg::Incoming { token }).await {
                            break Err(err);
                        }
                    }
                    None => break Ok(()),
                },
            }
        };
        reader.abort();
        outcome
    }

    async fn connect_session(
        &self,
        mut stream: Channel,
        peer: SocketAddr,
    ) -> Result<(), RelayError> {
        let limits = self.state.config.limits.clone();
        let request = tokio::time::timeout(limits.hello_timeout, stream.recv())
            .await
            .map_err(|_| RelayError::Timeout("connect"))??;
        let Msg::Connect { target } = request else {
            send_error(&mut stream, "malformed", "expected Connect").await;
            return Err(RelayError::Protocol("expected Connect".into()));
        };
        let counted = self.counted(peer);
        if let Some(ip) = counted {
            if !self.state.quotas.connect(ip, limits.connects_per_minute) {
                send_error(&mut stream, "limit_exceeded", "too many connects").await;
                return Err(RelayError::Protocol("connect rate".into()));
            }
        }
        // Held until the splice ends.
        let _place = match counted {
            Some(ip) => match self.state.quotas.splice(ip, limits.splices_per_ip) {
                Some(place) => Some(place),
                None => {
                    send_error(&mut stream, "limit_exceeded", "too many open pipes").await;
                    return Err(RelayError::Protocol("splices per address".into()));
                }
            },
            None => None,
        };
        if !self.state.quotas.carry(target, 0, limits.node_daily_bytes) {
            send_error(&mut stream, "limit_exceeded", "daily quota used up").await;
            return Err(RelayError::Protocol("daily quota".into()));
        }
        if self.state.splices.load(Ordering::SeqCst) >= limits.max_splices {
            self.note_full("relayed sessions", limits.max_splices);
            send_error(&mut stream, "limit_exceeded", FULL).await;
            return Err(RelayError::Protocol("splice limit".into()));
        }
        let tx = self
            .state
            .listeners
            .lock()
            .ok()
            .and_then(|listeners| listeners.get(&target).map(|entry| entry.tx.clone()));
        let Some(tx) = tx else {
            send_error(&mut stream, "no_such_node", "node is not registered here").await;
            return Err(RelayError::Protocol("no such node".into()));
        };
        let token = random_bytes::<16>().map_err(|err| RelayError::Auth(err.to_string()))?;
        let (reply, accepted) = oneshot::channel();
        let queued = {
            let mut pending = self
                .state
                .pending
                .lock()
                .map_err(|_| RelayError::Protocol("state poisoned".into()))?;
            if pending.len() >= limits.max_pending {
                false
            } else {
                pending.insert(
                    token,
                    Pending {
                        node: target,
                        reply,
                    },
                );
                true
            }
        };
        if !queued {
            self.note_full("waiting connects", limits.max_pending);
            send_error(&mut stream, "limit_exceeded", FULL).await;
            return Err(RelayError::Protocol("pending limit".into()));
        }
        if tx.try_send(token).is_err() {
            self.drop_pending(&token);
            send_error(&mut stream, "busy", "listener is not taking connections").await;
            return Err(RelayError::Protocol("listener busy".into()));
        }
        let acceptor = match tokio::time::timeout(limits.pending_ttl, accepted).await {
            Ok(Ok(acceptor)) => acceptor,
            _ => {
                self.drop_pending(&token);
                send_error(&mut stream, "timeout", "listener did not answer").await;
                return Err(RelayError::Timeout("accept"));
            }
        };
        stream.send(&Msg::Ready).await?;
        self.splice(stream.into_raw(), acceptor, target).await;
        Ok(())
    }

    fn drop_pending(&self, token: &[u8; 16]) {
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.remove(token);
        }
    }

    async fn accept_session(&self, mut stream: Channel) -> Result<(), RelayError> {
        let limits = &self.state.config.limits;
        let request = tokio::time::timeout(limits.hello_timeout, stream.recv())
            .await
            .map_err(|_| RelayError::Timeout("accept"))??;
        let Msg::Accept { token } = request else {
            send_error(&mut stream, "malformed", "expected Accept").await;
            return Err(RelayError::Protocol("expected Accept".into()));
        };
        let entry = self
            .state
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&token));
        let Some(Pending { node, reply }) = entry else {
            send_error(&mut stream, "bad_token", "unknown or expired token").await;
            return Err(RelayError::Protocol("bad token".into()));
        };
        // The token only ever travelled over the authenticated control
        // connection of `node`, so holding it proves this is that node.
        let _ = node;
        stream.send(&Msg::Ready).await?;
        // If the dialer already gave up, the stream is dropped and closes.
        let _ = reply.send(stream.into_raw());
        Ok(())
    }
}
