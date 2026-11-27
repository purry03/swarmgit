//! Relay client: listen through a relay, or connect to a node behind one.
//!
//! Both return plain [`TcpStream`]s that are already spliced to the other end.
//! Run the normal swarm handshake over them. A dialer must pin the target node
//! ID in that handshake, so a relay cannot impersonate the target (NET-11).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use swarm_core::crypto::relay_auth_input;
use swarm_core::ids::NodeId;
use swarm_core::keyfile::NodeKey;
use swarm_core::pq::Signer;
use swarm_net::proxy::{self, Endpoint};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::proto::{Msg, Role, MAX_TOPICS_FRAME, VERSION};
use crate::secure::{split_key, Channel, CAP_NK};
use crate::RelayError;

/// Relay keys remembered at once.
const MAX_LEARNED: usize = 1024;
/// Time to open the TCP connection to a relay.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time for the relay to answer a request. Covers the relay's own wait for the
/// listener to accept (10 s by default).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
/// Ping interval on the control connection (REL-6).
pub const PING_EVERY: Duration = Duration::from_secs(25);
/// A control connection with no frame from the relay for this long is dead.
pub const CONTROL_SILENCE: Duration = Duration::from_secs(75);
/// First and largest reconnect delays (REL-6).
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// Largest reconnect delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Relay keys learned from `RInfoReply`, by address.
fn learned() -> MutexGuard<'static, HashMap<String, Option<[u8; 32]>>> {
    static LEARNED: OnceLock<Mutex<HashMap<String, Option<[u8; 32]>>>> = OnceLock::new();
    LEARNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The NK key to use with `addr`: asked once, then remembered. This keeps a
/// passive observer out; only a pinned key (`host:port#key`) also keeps out
/// one that can alter traffic.
async fn key_of(addr: &str) -> Result<Option<[u8; 32]>, RelayError> {
    if let Some(key) = learned().get(addr) {
        return Ok(*key);
    }
    let info = info(addr).await?;
    let key = info
        .caps
        .iter()
        .any(|cap| cap == CAP_NK)
        .then_some(info.relay_id);
    let mut learned = learned();
    if learned.len() >= MAX_LEARNED {
        learned.clear();
    }
    learned.insert(addr.to_string(), key);
    Ok(key)
}

async fn dial(addr: &str) -> Result<TcpStream, RelayError> {
    // Through the SOCKS5 proxy when one is configured (M9).
    let endpoint = Endpoint::parse(addr).ok_or_else(|| RelayError::Resolve(addr.to_string()))?;
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, proxy::connect(&endpoint))
        .await
        .map_err(|_| RelayError::Timeout("connecting to the relay"))?
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::InvalidInput {
                RelayError::Resolve(addr.to_string())
            } else {
                RelayError::Io(err)
            }
        })?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

fn hello(role: Role, password: Option<&str>) -> Msg {
    match password {
        Some(password) => Msg::HelloAuth {
            version: VERSION,
            role,
            password: password.to_string(),
        },
        None => Msg::Hello {
            version: VERSION,
            role,
        },
    }
}

/// A password goes only to a relay whose key is included
/// (`host:port#key`): a key learned on the spot could be an impostor's.
fn allowed<'a>(relay: &str, password: Option<&'a str>) -> Option<&'a str> {
    password.filter(|_| split_key(relay).is_ok_and(|(_, key)| key.is_some()))
}

/// Explain a refusal that came of holding the password back.
fn held_back(relay: &str, password: Option<&str>, err: RelayError) -> RelayError {
    let held = password.is_some() && allowed(relay, password).is_none();
    match err {
        RelayError::Remote { code, msg } if held && code == "auth_failed" => RelayError::Remote {
            code,
            msg: format!(
                "{msg}; it was not sent: add the relay's key to its address (host:port#key)"
            ),
        },
        other => other,
    }
}

/// Open a control connection and say hello, encrypted when the relay can.
async fn open(relay: &str, role: Role, password: Option<&str>) -> Result<Channel, RelayError> {
    let (addr, pinned) = split_key(relay)?;
    let key = match pinned {
        Some(key) => Some(key),
        // `info` calls `open` for nothing, so this cannot recurse.
        None => key_of(addr).await?,
    };
    let stream = dial(addr).await?;
    let mut channel = match key {
        Some(key) => {
            let handshake = Channel::initiate(stream, key);
            match tokio::time::timeout(CONNECT_TIMEOUT, handshake).await {
                Ok(Ok(channel)) => channel,
                outcome => {
                    // The relay may have restarted with a new key.
                    learned().remove(addr);
                    return Err(match outcome {
                        Ok(Err(err)) => RelayError::Auth(format!("relay key: {err}")),
                        _ => RelayError::Timeout("relay handshake"),
                    });
                }
            }
        }
        None => Channel::plain(stream),
    };
    channel.send(&hello(role, allowed(relay, password))).await?;
    Ok(channel)
}

async fn reply(stream: &mut Channel) -> Result<Msg, RelayError> {
    let msg = tokio::time::timeout(REPLY_TIMEOUT, stream.recv())
        .await
        .map_err(|_| RelayError::Timeout("waiting for the relay"))??;
    if let Msg::Error { code, msg } = msg {
        return Err(RelayError::Remote { code, msg });
    }
    Ok(msg)
}

/// Ask `relay` (`host:port`) for a pipe to `target`.
///
/// Returns the spliced stream once the target has accepted. The caller runs
/// [`swarm_net::handshake::connect`] on it with `pin = Some(target)`.
pub async fn connect(relay: &str, target: NodeId) -> Result<TcpStream, RelayError> {
    connect_with(relay, target, None).await
}

/// [`connect`] to a relay that wants a password (REL-21).
pub async fn connect_with(
    relay: &str,
    target: NodeId,
    password: Option<&str>,
) -> Result<TcpStream, RelayError> {
    let mut stream = open(relay, Role::Connect, password).await?;
    stream
        .send(&Msg::Connect {
            target: *target.as_bytes(),
        })
        .await?;
    match reply(&mut stream)
        .await
        .map_err(|err| held_back(relay, password, err))?
    {
        Msg::Ready => Ok(stream.into_raw()),
        _ => Err(RelayError::Protocol("expected Ready".into())),
    }
}

async fn accept_one(relay: &str, token: [u8; 16]) -> Result<TcpStream, RelayError> {
    let mut stream = open(relay, Role::Accept, None).await?;
    stream.send(&Msg::Accept { token }).await?;
    match reply(&mut stream).await? {
        Msg::Ready => Ok(stream.into_raw()),
        _ => Err(RelayError::Protocol("expected Ready".into())),
    }
}

/// What a relay says about itself (`RInfoReply`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// Protocol version.
    pub version: u8,
    /// What the relay offers.
    pub caps: Vec<String>,
    /// Named limits.
    pub limits: Vec<(String, u64)>,
    /// Identifies the relay process.
    pub relay_id: [u8; 32],
    /// Our address as the relay sees it.
    pub observed: String,
}

/// One `role=4` request and its reply.
async fn ask(relay: &str, request: &Msg) -> Result<Msg, RelayError> {
    let mut stream = open(relay, Role::Info, None).await?;
    stream.send(request).await?;
    reply(&mut stream).await
}

/// Ask a relay about itself.
pub async fn info(relay: &str) -> Result<Info, RelayError> {
    // In the clear: this is how the key is learned, and it names no one.
    let (addr, _) = split_key(relay)?;
    let mut channel = Channel::plain(dial(addr).await?);
    channel.send(&hello(Role::Info, None)).await?;
    channel.send(&Msg::Info).await?;
    match reply(&mut channel).await? {
        Msg::InfoReply {
            version,
            caps,
            limits,
            relay_id,
            observed,
        } => Ok(Info {
            version,
            caps,
            limits,
            relay_id,
            observed,
        }),
        _ => Err(RelayError::Protocol("expected RInfoReply".into())),
    }
}

/// Ask a relay for up to `max` other relays it has verified (REL-26).
pub async fn get_relays(relay: &str, max: u64) -> Result<Vec<SocketAddr>, RelayError> {
    match ask(relay, &Msg::GetRelays { max }).await? {
        Msg::Relays { addrs } => Ok(addrs),
        _ => Err(RelayError::Protocol("expected RRelays".into())),
    }
}

/// Ask whether `target` is registered with a relay, and which relays are
/// closer to it (REL-29).
pub async fn where_is(relay: &str, target: NodeId) -> Result<(bool, Vec<SocketAddr>), RelayError> {
    let request = Msg::Where {
        target: *target.as_bytes(),
    };
    match ask(relay, &request).await? {
        Msg::Referral { found, closer } => Ok((found, closer)),
        _ => Err(RelayError::Protocol("expected Referral".into())),
    }
}

/// Ask a relay to dial us back on `port` (REL-25).
pub async fn dial_back(relay: &str, port: u16, relay_id: [u8; 32]) -> Result<bool, RelayError> {
    match ask(relay, &Msg::DialBack { port, relay_id }).await? {
        Msg::DialBackReply { reachable } => Ok(reachable),
        _ => Err(RelayError::Protocol("expected DialBackReply".into())),
    }
}

/// Where a listener's registration stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenState {
    /// Registered. `observed` is our address as the relay sees it.
    Registered {
        /// Our address as the relay sees it.
        observed: String,
    },
    /// The control connection dropped; retrying.
    Reconnecting {
        /// Why it dropped.
        error: String,
    },
}

/// A registration with one relay. Dropping it unregisters.
pub struct RelayListener {
    streams: mpsc::Receiver<TcpStream>,
    state: watch::Receiver<ListenState>,
    task: JoinHandle<()>,
}

impl RelayListener {
    /// Wait for the next dialer. The stream is spliced and ready for the
    /// responder side of the swarm handshake. `None` once the listener is shut.
    pub async fn accept(&mut self) -> Option<TcpStream> {
        self.streams.recv().await
    }

    /// Current registration state.
    #[must_use]
    pub fn state(&self) -> ListenState {
        self.state.borrow().clone()
    }

    /// Watch registration changes.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<ListenState> {
        self.state.clone()
    }
}

impl Drop for RelayListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn register(
    relay: &str,
    key: &NodeKey,
    password: Option<&str>,
) -> Result<(Channel, String), RelayError> {
    let mut stream = open(relay, Role::Listen, password).await?;
    prove(&mut stream, relay, key)
        .await
        .map_err(|err| held_back(relay, password, err))?;
    match reply(&mut stream).await? {
        Msg::Listening { observed, .. } => Ok((stream, observed)),
        _ => Err(RelayError::Protocol("expected Listening".into())),
    }
}

/// Answer the relay's challenge with the node key.
async fn prove(stream: &mut Channel, relay: &str, key: &NodeKey) -> Result<(), RelayError> {
    let Msg::Challenge { nonce } = reply(stream).await? else {
        return Err(RelayError::Protocol("expected Challenge".into()));
    };
    let auth = |err: swarm_core::crypto::CryptoError| RelayError::Auth(err.to_string());
    // The signature binds the address, not the pinned key (REL-1).
    let (name, _) = split_key(relay)?;
    let bundle = key.bundle().map_err(auth)?;
    let sig = key
        .signer()
        .map_err(auth)?
        .try_sign(&relay_auth_input(&nonce, name))
        .map_err(auth)?;
    stream
        .send(&Msg::Prove {
            bundle,
            sig,
            name: name.to_string(),
        })
        .await
}

/// Tell `relay` that `key`'s node serves `topics` (see [`crate::topic`]).
pub async fn publish(
    relay: &str,
    key: &NodeKey,
    topics: &[[u8; 32]],
    password: Option<&str>,
) -> Result<(), RelayError> {
    let mut stream = open(relay, Role::Publish, password).await?;
    prove(&mut stream, relay, key)
        .await
        .map_err(|err| held_back(relay, password, err))?;
    let topics = topics.iter().take(MAX_TOPICS_FRAME).copied().collect();
    stream.send(&Msg::Publish { topics }).await?;
    match reply(&mut stream).await? {
        Msg::Published { .. } => Ok(()),
        _ => Err(RelayError::Protocol("expected Published".into())),
    }
}

/// Ask `relay` which nodes serve `topic`.
pub async fn lookup(relay: &str, topic: [u8; 32]) -> Result<Vec<NodeId>, RelayError> {
    match ask(relay, &Msg::Lookup { topic }).await? {
        Msg::Holders { nodes } => Ok(nodes.into_iter().map(NodeId::from_bytes).collect()),
        _ => Err(RelayError::Protocol("expected Holders".into())),
    }
}

/// Register `key`'s node ID with `relay` (`host:port`).
///
/// The first registration is made before this returns, so a wrong address or a
/// rejected proof is an immediate error. After that the listener keeps the
/// registration alive and reconnects with backoff (REL-6).
pub async fn listen(relay: &str, key: Arc<NodeKey>) -> Result<RelayListener, RelayError> {
    listen_with(relay, key, None).await
}

/// [`listen`] at a relay that wants a password (REL-21).
pub async fn listen_with(
    relay: &str,
    key: Arc<NodeKey>,
    password: Option<String>,
) -> Result<RelayListener, RelayError> {
    let (stream, observed) = register(relay, &key, password.as_deref()).await?;
    let (state_tx, state) = watch::channel(ListenState::Registered { observed });
    let (stream_tx, streams) = mpsc::channel(8);
    let relay = relay.to_string();
    let task = tokio::spawn(async move {
        let mut current = Some(stream);
        let mut backoff = BACKOFF_MIN;
        loop {
            let control = match current.take() {
                Some(stream) => stream,
                None => match register(&relay, &key, password.as_deref()).await {
                    Ok((stream, observed)) => {
                        backoff = BACKOFF_MIN;
                        let _ = state_tx.send(ListenState::Registered { observed });
                        stream
                    }
                    Err(err) => {
                        let _ = state_tx.send(ListenState::Reconnecting {
                            error: err.to_string(),
                        });
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(BACKOFF_MAX);
                        continue;
                    }
                },
            };
            let error = match control_loop(control, &relay, &stream_tx).await {
                Ok(()) => "relay closed the connection".to_string(),
                Err(err) => err.to_string(),
            };
            if stream_tx.is_closed() {
                return;
            }
            let _ = state_tx.send(ListenState::Reconnecting { error });
            tokio::time::sleep(backoff).await;
        }
    });
    Ok(RelayListener {
        streams,
        state,
        task,
    })
}

async fn control_loop(
    stream: Channel,
    relay: &str,
    out: &mpsc::Sender<TcpStream>,
) -> Result<(), RelayError> {
    let (mut read_half, mut write_half) = stream.into_split();
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
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut nonce = 0u64;
    let mut deadline = tokio::time::Instant::now() + CONTROL_SILENCE;
    let outcome = loop {
        tokio::select! {
            _ = ping.tick() => {
                nonce += 1;
                if let Err(err) = write_half.send(&Msg::Ping { nonce }).await {
                    break Err(err);
                }
            }
            () = tokio::time::sleep_until(deadline) => {
                break Err(RelayError::Timeout("relay is silent"));
            }
            frame = frames.recv() => match frame {
                Some(Ok(Msg::Incoming { token })) => {
                    deadline = tokio::time::Instant::now() + CONTROL_SILENCE;
                    let relay = relay.to_string();
                    let out = out.clone();
                    tokio::spawn(async move {
                        match accept_one(&relay, token).await {
                            Ok(stream) => { let _ = out.send(stream).await; }
                            Err(err) => tracing::debug!("relay accept failed: {err}"),
                        }
                    });
                }
                Some(Ok(Msg::Pong { .. } | Msg::Ping { .. })) => {
                    deadline = tokio::time::Instant::now() + CONTROL_SILENCE;
                }
                Some(Ok(Msg::Error { code, msg })) => break Err(RelayError::Remote { code, msg }),
                Some(Ok(_)) => break Err(RelayError::Protocol("unexpected message".into())),
                Some(Err(err)) => break Err(err),
                None => break Ok(()),
            },
        }
    };
    reader.abort();
    outcome
}
