//! The splice: copy bytes both ways until one side closes, both are idle
//! (REL-5), or the node reaches its daily quota (REL-19).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use crate::quota::Pacer;
use crate::server::Relay;

/// State shared by the two directions of one splice.
struct Shared<'a> {
    relay: &'a Relay,
    /// The registered node this pipe leads to.
    node: [u8; 32],
    pacer: Pacer,
    start: Instant,
    /// Milliseconds after `start` of the last byte.
    last: Arc<AtomicU64>,
}

impl Relay {
    pub(crate) async fn splice(&self, a: TcpStream, b: TcpStream, node: [u8; 32]) {
        let state = &self.state;
        state.splices.fetch_add(1, Ordering::SeqCst);
        state.counters.splices_total.fetch_add(1, Ordering::SeqCst);
        let (ar, aw) = a.into_split();
        let (br, bw) = b.into_split();
        let shared = Shared {
            relay: self,
            node,
            pacer: Pacer::new(state.config.limits.splice_bytes_per_second),
            start: Instant::now(),
            last: Arc::new(AtomicU64::new(0)),
        };
        let idle = state.config.limits.idle;
        // An `Err` ends both directions at once; end of file only its own.
        let forward = async { tokio::try_join!(pump(ar, bw, &shared), pump(br, aw, &shared)) };
        let watchdog = async {
            loop {
                tokio::time::sleep(Duration::from_millis(500).min(idle)).await;
                let since = u64::try_from(shared.start.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .saturating_sub(shared.last.load(Ordering::SeqCst));
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

/// The node used up its quota.
struct OverQuota;

/// Copy one direction, noting activity. Half-closes the writer at EOF.
async fn pump(
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    shared: &Shared<'_>,
) -> Result<(), OverQuota> {
    let state = &shared.relay.state;
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        shared.pacer.take(read).await;
        if writer.write_all(&buffer[..read]).await.is_err() {
            break;
        }
        state
            .counters
            .bytes
            .fetch_add(read as u64, Ordering::Relaxed);
        shared.last.store(
            u64::try_from(shared.start.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
        let max = state.config.limits.node_daily_bytes;
        if !state.quotas.carry(shared.node, read as u64, max) {
            return Err(OverQuota);
        }
    }
    let _ = writer.shutdown().await;
    Ok(())
}
