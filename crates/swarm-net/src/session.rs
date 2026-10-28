//! Session actor: request/response matching, unsolicited messages and packs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::NetError;
use crate::frame::MAX_MESSAGE_BYTES;
use crate::handshake::{PeerInfo, Transport};
use crate::message::Message;

enum Pending {
    One(oneshot::Sender<Message>),
    Stream(mpsc::Sender<Message>),
}

type PendingMap = Arc<Mutex<HashMap<u64, Pending>>>;

/// A cloneable handle used to send requests and replies.
#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::Sender<Vec<u8>>,
    pending: PendingMap,
    next_rid: Arc<AtomicU64>,
    info: Arc<PeerInfo>,
}

impl SessionHandle {
    /// Facts about the authenticated peer.
    #[must_use]
    pub fn info(&self) -> &PeerInfo {
        &self.info
    }

    /// True when the session carries `repo`.
    #[must_use]
    pub fn shared_repo(&self, repo: swarm_core::ids::RepoId) -> Option<u16> {
        self.info.repo_ix(repo)
    }

    fn new_rid(&self) -> u64 {
        self.next_rid.fetch_add(1, Ordering::SeqCst)
    }

    async fn send_encoded(&self, message: &Message) -> Result<(), NetError> {
        let bytes = message.encode()?;
        self.tx.send(bytes).await.map_err(|_| NetError::Closed)
    }

    /// Send one message without expecting a reply.
    pub async fn send(&self, message: &Message) -> Result<(), NetError> {
        self.send_encoded(message).await
    }

    /// Send a request and wait for its single reply.
    pub async fn request(
        &self,
        mut message: Message,
        timeout: Duration,
    ) -> Result<Message, NetError> {
        let rid = self.new_rid();
        message.set_rid(rid);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| NetError::Malformed("pending map is poisoned".into()))?;
            pending.insert(rid, Pending::One(tx));
        }
        if let Err(err) = self.send_encoded(&message).await {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&rid);
            }
            return Err(err);
        }
        let reply = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| NetError::Timeout("request timed out".into()))?;
        let reply = reply.map_err(|_| NetError::Closed)?;
        if let Message::Error { code, msg, .. } = reply {
            return Err(NetError::Remote { code, msg });
        }
        Ok(reply)
    }

    /// Send a `Want` and return the stream of `PackHeader` and `PackData`.
    pub async fn request_stream(
        &self,
        mut message: Message,
        channel: usize,
    ) -> Result<PackStream, NetError> {
        let rid = self.new_rid();
        message.set_rid(rid);
        let (tx, rx) = mpsc::channel(channel);
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| NetError::Malformed("pending map is poisoned".into()))?;
            pending.insert(rid, Pending::Stream(tx));
        }
        if let Err(err) = self.send_encoded(&message).await {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&rid);
            }
            return Err(err);
        }
        Ok(PackStream { rx, rid })
    }
}

/// A stream of pack messages for one `Want`.
pub struct PackStream {
    rx: mpsc::Receiver<Message>,
    rid: u64,
}

impl PackStream {
    /// Request ID of this pack.
    #[must_use]
    pub fn rid(&self) -> u64 {
        self.rid
    }

    /// Receive the next message, or an error when the peer closes the stream.
    pub async fn recv(&mut self) -> Result<Message, NetError> {
        self.rx.recv().await.ok_or(NetError::Closed)
    }
}

/// One established session. The reader task feeds `incoming` with requests and
/// unsolicited messages, and answers pings on its own.
pub struct Session {
    handle: SessionHandle,
    incoming: mpsc::Receiver<Message>,
    reader_task: JoinHandle<()>,
}

impl Session {
    /// Start the actor over an established transport.
    #[must_use]
    pub fn start(transport: Transport) -> Self {
        let Transport {
            info,
            mut reader,
            mut writer,
        } = transport;
        let info = Arc::new(info);
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(128);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let next_rid = Arc::new(AtomicU64::new(1));
        let (in_tx, in_rx) = mpsc::channel::<Message>(64);

        tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if writer.write_message(&bytes).await.is_err() {
                    break;
                }
            }
        });

        let reader_pending = Arc::clone(&pending);
        let reader_out = out_tx.clone();
        let reader_task = tokio::spawn(async move {
            loop {
                let bytes = match reader.read_message(MAX_MESSAGE_BYTES).await {
                    Ok(bytes) => bytes,
                    Err(_) => break,
                };
                let message = match Message::decode(&bytes) {
                    Ok(message) => message,
                    Err(_) => break,
                };
                if let Some(rid) = message.rid() {
                    let mut delivered = false;
                    let mut closed_stream = false;
                    if let Ok(mut pending) = reader_pending.lock() {
                        match pending.get(&rid) {
                            Some(Pending::One(_)) => {
                                if let Some(Pending::One(tx)) = pending.remove(&rid) {
                                    let _ = tx.send(message.clone());
                                    delivered = true;
                                }
                            }
                            Some(Pending::Stream(_)) => {
                                let stream_done = matches!(
                                    message,
                                    Message::Error { .. }
                                        | Message::PackData {
                                            final_chunk: true,
                                            ..
                                        }
                                );
                                if let Some(Pending::Stream(tx)) = pending.get(&rid) {
                                    if tx.try_send(message.clone()).is_ok() && stream_done {
                                        closed_stream = true;
                                    }
                                }
                                delivered = true;
                            }
                            None => {}
                        }
                        if closed_stream {
                            pending.remove(&rid);
                        }
                    }
                    if delivered {
                        continue;
                    }
                }
                match message {
                    Message::Ping { nonce } => {
                        if let Ok(bytes) = (Message::Pong { nonce }).encode() {
                            let _ = reader_out.send(bytes).await;
                        }
                    }
                    Message::Pong { .. } => {}
                    Message::Hello { .. } => break,
                    other => {
                        if in_tx.send(other).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            handle: SessionHandle {
                tx: out_tx,
                pending,
                next_rid,
                info,
            },
            incoming: in_rx,
            reader_task,
        }
    }

    /// Facts about the authenticated peer.
    #[must_use]
    pub fn info(&self) -> &PeerInfo {
        self.handle.info()
    }

    /// A cloneable handle for concurrent replies.
    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        self.handle.clone()
    }

    /// The next request or unsolicited message from the peer.
    pub async fn recv_incoming(&mut self) -> Option<Message> {
        self.incoming.recv().await
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}
