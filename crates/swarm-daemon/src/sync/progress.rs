//! Progress events for a sync, streamed to the client that asked for them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

/// Where a sync reports what it is doing. Does nothing when nobody listens.
#[derive(Clone, Default)]
pub struct Progress(Option<UnboundedSender<Value>>);

impl Progress {
    /// A reporter that drops every event.
    #[must_use]
    pub fn none() -> Self {
        Self(None)
    }

    /// A reporter and the receiver of its events.
    #[must_use]
    pub fn channel() -> (Self, UnboundedReceiver<Value>) {
        let (tx, rx) = unbounded_channel();
        (Self(Some(tx)), rx)
    }

    fn send(&self, event: Value) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(event);
        }
    }

    /// A status line for `stage` (`find`, `dial`, `lan`, `dht`, `heads`, ...).
    pub fn say(&self, stage: &str, msg: impl Into<String>) {
        self.send(json!({ "event": "progress", "stage": stage, "msg": msg.into() }));
    }

    /// A line only shown when the user asked for detail (`-v`).
    pub fn detail(&self, stage: &str, msg: impl Into<String>) {
        self.send(json!({
            "event": "progress",
            "stage": stage,
            "msg": msg.into(),
            "detail": true,
        }));
    }

    /// Running totals of the dials of one sync.
    pub fn dials(&self, connected: usize, failed: usize, trying: usize, queued: usize) {
        self.send(json!({
            "event": "progress",
            "stage": "dial",
            "dials": {
                "connected": connected,
                "failed": failed,
                "trying": trying,
                "queued": queued,
            },
        }));
    }

    /// Bytes received so far, with the expected total when it is known.
    pub fn bytes(&self, done: u64, total: u64) {
        let total = (total >= done && total > 0).then_some(total);
        self.send(json!({
            "event": "progress",
            "stage": "pack",
            "done": done,
            "total": total,
        }));
    }
}

/// Bytes moved by the packs of one fetch round, shared by parallel sessions.
#[derive(Default)]
pub struct Transfer {
    received: AtomicU64,
    expected: AtomicU64,
}

impl Transfer {
    /// A new shared counter.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A peer announced a pack of `size` bytes.
    pub fn expect(&self, size: u64) {
        self.expected.fetch_add(size, Ordering::Relaxed);
    }

    /// `len` more bytes arrived; returns (received, expected).
    pub fn add(&self, len: u64) -> (u64, u64) {
        let received = self.received.fetch_add(len, Ordering::Relaxed) + len;
        (received, self.expected.load(Ordering::Relaxed))
    }
}
