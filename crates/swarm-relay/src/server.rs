//! The relay server (design spec sections 4 and 7).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::{node_id, random_bytes, relay_auth_input};
use swarm_core::pq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};

use crate::proto::{read_msg, write_msg, Msg, Role, VERSION};
use crate::RelayError;

/// Resource limits (REL-5, REL-6, REL-19, REL-20). Per-IP and bandwidth limits
/// arrive with milestone R4.
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
}

struct Entry {
    gen: u64,
    tx: mpsc::Sender<[u8; 16]>,
}

struct Pending {
    node: [u8; 32],
    reply: oneshot::Sender<TcpStream>,
}

/// Counters only; nothing identifying is kept (REL-7).
#[derive(Default)]
struct Counters {
    splices_total: AtomicU64,
    bytes: AtomicU64,
    auth_failures: AtomicU64,
}

struct State {
    config: RelayConfig,
    listeners: Mutex<HashMap<[u8; 32], Entry>>,
    pending: Mutex<HashMap<[u8; 16], Pending>>,
    next_gen: AtomicU64,
    splices: AtomicUsize,
    counters: Counters,
    shutdown: watch::Sender<bool>,
}

/// A relay. Cheap to clone.
#[derive(Clone)]
pub struct Relay {
    state: Arc<State>,
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
    /// Failed listener proofs.
    pub auth_failures: u64,
}

/// Resolve once shutdown is requested.
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|stopped| *stopped).await;
}

async fn send_error(stream: &mut TcpStream, code: &str, msg: &str) {
    let _ = write_msg(
        stream,
        &Msg::Error {
            code: code.into(),
            msg: msg.into(),
        },
    )
    .await;
}

impl Relay {
    /// Create a relay.
    #[must_use]
    pub fn new(config: RelayConfig) -> Self {
        Self {
            state: Arc::new(State {
                config,
                listeners: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                next_gen: AtomicU64::new(1),
                splices: AtomicUsize::new(0),
                counters: Counters::default(),
                shutdown: watch::channel(false).0,
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
        }
    }

    /// Stop accepting and drop every open connection. [`Relay::serve`]
    /// returns and releases its listening socket.
    pub fn shutdown(&self) {
        let _ = self.state.shutdown.send(true);
    }

    /// Accept connections until [`Relay::shutdown`] is called or the task is
    /// dropped or aborted.
    pub async fn serve(&self, listener: TcpListener) {
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
                            tracing::debug!("relay connection {peer}: {err}");
                        }
                    }
                }
            });
        }
    }

    async fn handle(&self, mut stream: TcpStream, peer: SocketAddr) -> Result<(), RelayError> {
        let limits = &self.state.config.limits;
        let hello = tokio::time::timeout(limits.hello_timeout, read_msg(&mut stream))
            .await
            .map_err(|_| RelayError::Timeout("hello"))??;
        let Msg::Hello { version, role } = hello else {
            send_error(&mut stream, "malformed", "expected RHello").await;
            return Err(RelayError::Protocol("expected RHello".into()));
        };
        if version != VERSION {
            send_error(&mut stream, "unsupported_version", "version 1 only").await;
            return Err(RelayError::Protocol("unsupported version".into()));
        }
        match role {
            Role::Listen => self.listen_session(stream, peer).await,
            Role::Connect => self.connect_session(stream).await,
            Role::Accept => self.accept_session(stream).await,
        }
    }

    async fn listen_session(
        &self,
        mut stream: TcpStream,
        peer: SocketAddr,
    ) -> Result<(), RelayError> {
        let limits = self.state.config.limits.clone();
        let nonce = random_bytes::<32>().map_err(|err| RelayError::Auth(err.to_string()))?;
        write_msg(&mut stream, &Msg::Challenge { nonce }).await?;
        let proof = tokio::time::timeout(limits.hello_timeout, read_msg(&mut stream))
            .await
            .map_err(|_| RelayError::Timeout("prove"))??;
        let Msg::Prove { bundle, sig, name } = proof else {
            send_error(&mut stream, "malformed", "expected Prove").await;
            return Err(RelayError::Protocol("expected Prove".into()));
        };
        let names = &self.state.config.names;
        let name_ok = names.is_empty() || names.contains(&name);
        let signature_ok = name_ok
            && bundle.len() == swarm_core::consts::SUITE1_BUNDLE_LEN
            && sig.len() == swarm_core::consts::SUITE1_SIG_LEN
            && pq::verify(
                SUITE_HYBRID,
                &bundle,
                &relay_auth_input(&nonce, &name),
                &sig,
            )
            .is_ok();
        if !signature_ok {
            self.state
                .counters
                .auth_failures
                .fetch_add(1, Ordering::SeqCst);
            send_error(&mut stream, "auth_failed", "proof rejected").await;
            return Err(RelayError::Auth("proof rejected".into()));
        }
        let node = *node_id(&bundle).as_bytes();

        let gen = self.state.next_gen.fetch_add(1, Ordering::SeqCst);
        let (tx, mut incoming) = mpsc::channel::<[u8; 16]>(16);
        let admitted = {
            let mut listeners = self
                .state
                .listeners
                .lock()
                .map_err(|_| RelayError::Protocol("state poisoned".into()))?;
            if !listeners.contains_key(&node) && listeners.len() >= limits.max_listeners {
                false
            } else {
                // REL-2: a newer control connection replaces the older one.
                // The old entry's sender drops here, ending its control loop.
                listeners.insert(node, Entry { gen, tx });
                true
            }
        };
        if !admitted {
            send_error(&mut stream, "limit_exceeded", "too many listeners").await;
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
        stream: TcpStream,
        peer: SocketAddr,
        incoming: &mut mpsc::Receiver<[u8; 16]>,
        silence: Duration,
    ) -> Result<(), RelayError> {
        let (mut read_half, mut write_half) = stream.into_split();
        write_msg(
            &mut write_half,
            &Msg::Listening {
                ttl_secs: silence.as_secs(),
                observed: peer.to_string(),
            },
        )
        .await?;
        // Frames are read by a task because `read_msg` is not cancel safe
        // inside `select!`.
        let (frame_tx, mut frames) = mpsc::channel::<Result<Msg, RelayError>>(8);
        let reader = tokio::spawn(async move {
            loop {
                let msg = read_msg(&mut read_half).await;
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
                        if let Err(err) = write_msg(&mut write_half, &Msg::Pong { nonce }).await {
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
                        if let Err(err) = write_msg(&mut write_half, &Msg::Incoming { token }).await {
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

    async fn connect_session(&self, mut stream: TcpStream) -> Result<(), RelayError> {
        let limits = self.state.config.limits.clone();
        let request = tokio::time::timeout(limits.hello_timeout, read_msg(&mut stream))
            .await
            .map_err(|_| RelayError::Timeout("connect"))??;
        let Msg::Connect { target } = request else {
            send_error(&mut stream, "malformed", "expected Connect").await;
            return Err(RelayError::Protocol("expected Connect".into()));
        };
        if self.state.splices.load(Ordering::SeqCst) >= limits.max_splices {
            send_error(&mut stream, "limit_exceeded", "relay is full").await;
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
            send_error(&mut stream, "limit_exceeded", "too many pending").await;
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
        write_msg(&mut stream, &Msg::Ready).await?;
        self.splice(stream, acceptor).await;
        Ok(())
    }

    fn drop_pending(&self, token: &[u8; 16]) {
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.remove(token);
        }
    }

    async fn accept_session(&self, mut stream: TcpStream) -> Result<(), RelayError> {
        let limits = &self.state.config.limits;
        let request = tokio::time::timeout(limits.hello_timeout, read_msg(&mut stream))
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
        write_msg(&mut stream, &Msg::Ready).await?;
        // If the dialer already gave up, the stream is dropped and closes.
        let _ = reply.send(stream);
        Ok(())
    }

    async fn splice(&self, a: TcpStream, b: TcpStream) {
        let state = &self.state;
        state.splices.fetch_add(1, Ordering::SeqCst);
        state.counters.splices_total.fetch_add(1, Ordering::SeqCst);
        let (ar, aw) = a.into_split();
        let (br, bw) = b.into_split();
        let start = Instant::now();
        let last = Arc::new(AtomicU64::new(0));
        let idle = state.config.limits.idle;
        let forward = async {
            tokio::join!(
                pump(ar, bw, &last, start, &state.counters.bytes),
                pump(br, aw, &last, start, &state.counters.bytes)
            )
        };
        let watchdog = async {
            loop {
                tokio::time::sleep(Duration::from_millis(500).min(idle)).await;
                let since = u64::try_from(start.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .saturating_sub(last.load(Ordering::SeqCst));
                if Duration::from_millis(since) >= idle {
                    return;
                }
            }
        };
        tokio::select! {
            _ = forward => {}
            () = watchdog => {}
        }
        state.splices.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Copy one direction, noting activity. Half-closes the writer at EOF.
async fn pump(
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    last: &AtomicU64,
    start: Instant,
    bytes: &AtomicU64,
) {
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if writer.write_all(&buffer[..read]).await.is_err() {
            break;
        }
        bytes.fetch_add(read as u64, Ordering::Relaxed);
        last.store(
            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
    }
    let _ = writer.shutdown().await;
}
