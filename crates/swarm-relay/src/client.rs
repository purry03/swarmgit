//! Relay client: listen through a relay, or connect to a node behind one.
//!
//! Both return plain [`TcpStream`]s that are already spliced to the other end.
//! Run the normal swarm handshake over them. A dialer must pin the target node
//! ID in that handshake, so a relay cannot impersonate the target (NET-11).

use std::sync::Arc;
use std::time::Duration;

use swarm_core::crypto::relay_auth_input;
use swarm_core::ids::NodeId;
use swarm_core::keyfile::NodeKey;
use swarm_core::pq::Signer;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::proto::{read_msg, write_msg, Msg, Role, VERSION};
use crate::RelayError;

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

async fn open(relay: &str, role: Role) -> Result<TcpStream, RelayError> {
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(relay))
        .await
        .map_err(|_| RelayError::Timeout("connecting to the relay"))?
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::InvalidInput {
                RelayError::Resolve(relay.to_string())
            } else {
                RelayError::Io(err)
            }
        })?;
    let _ = stream.set_nodelay(true);
    write_msg(
        &mut stream,
        &Msg::Hello {
            version: VERSION,
            role,
        },
    )
    .await?;
    Ok(stream)
}

async fn reply(stream: &mut TcpStream) -> Result<Msg, RelayError> {
    let msg = tokio::time::timeout(REPLY_TIMEOUT, read_msg(stream))
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
    let mut stream = open(relay, Role::Connect).await?;
    write_msg(
        &mut stream,
        &Msg::Connect {
            target: *target.as_bytes(),
        },
    )
    .await?;
    match reply(&mut stream).await? {
        Msg::Ready => Ok(stream),
        _ => Err(RelayError::Protocol("expected Ready".into())),
    }
}

async fn accept_one(relay: &str, token: [u8; 16]) -> Result<TcpStream, RelayError> {
    let mut stream = open(relay, Role::Accept).await?;
    write_msg(&mut stream, &Msg::Accept { token }).await?;
    match reply(&mut stream).await? {
        Msg::Ready => Ok(stream),
        _ => Err(RelayError::Protocol("expected Ready".into())),
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

async fn register(relay: &str, key: &NodeKey) -> Result<(TcpStream, String), RelayError> {
    let mut stream = open(relay, Role::Listen).await?;
    let Msg::Challenge { nonce } = reply(&mut stream).await? else {
        return Err(RelayError::Protocol("expected Challenge".into()));
    };
    let auth = |err: swarm_core::crypto::CryptoError| RelayError::Auth(err.to_string());
    let bundle = key.bundle().map_err(auth)?;
    let sig = key
        .signer()
        .map_err(auth)?
        .try_sign(&relay_auth_input(&nonce, relay))
        .map_err(auth)?;
    write_msg(
        &mut stream,
        &Msg::Prove {
            bundle,
            sig,
            name: relay.to_string(),
        },
    )
    .await?;
    match reply(&mut stream).await? {
        Msg::Listening { observed, .. } => Ok((stream, observed)),
        _ => Err(RelayError::Protocol("expected Listening".into())),
    }
}

/// Register `key`'s node ID with `relay` (`host:port`).
///
/// The first registration is made before this returns, so a wrong address or a
/// rejected proof is an immediate error. After that the listener keeps the
/// registration alive and reconnects with backoff (REL-6).
pub async fn listen(relay: &str, key: Arc<NodeKey>) -> Result<RelayListener, RelayError> {
    let (stream, observed) = register(relay, &key).await?;
    let (state_tx, state) = watch::channel(ListenState::Registered { observed });
    let (stream_tx, streams) = mpsc::channel(8);
    let relay = relay.to_string();
    let task = tokio::spawn(async move {
        let mut current = Some(stream);
        let mut backoff = BACKOFF_MIN;
        loop {
            let control = match current.take() {
                Some(stream) => stream,
                None => match register(&relay, &key).await {
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
    stream: TcpStream,
    relay: &str,
    out: &mpsc::Sender<TcpStream>,
) -> Result<(), RelayError> {
    let (mut read_half, mut write_half) = stream.into_split();
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
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut nonce = 0u64;
    let mut deadline = tokio::time::Instant::now() + CONTROL_SILENCE;
    let outcome = loop {
        tokio::select! {
            _ = ping.tick() => {
                nonce += 1;
                if let Err(err) = write_msg(&mut write_half, &Msg::Ping { nonce }).await {
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
