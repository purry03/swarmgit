//! The metrics endpoint (REL-22): off unless asked for, and counters only.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::server::{stopped, Relay};

/// Time to read a request and write the answer.
const TIMEOUT: Duration = Duration::from_secs(5);

impl Relay {
    /// The counters in the Prometheus text format.
    #[must_use]
    pub fn metrics_text(&self) -> String {
        let stats = self.stats();
        [
            ("swarm_relay_listeners", "gauge", stats.listeners as u64),
            ("swarm_relay_splices", "gauge", stats.splices as u64),
            ("swarm_relay_splices_total", "counter", stats.splices_total),
            ("swarm_relay_bytes_total", "counter", stats.bytes),
            (
                "swarm_relay_auth_failures_total",
                "counter",
                stats.auth_failures,
            ),
            ("swarm_relay_errors_total", "counter", stats.errors),
        ]
        .iter()
        .map(|(name, kind, value)| format!("# TYPE {name} {kind}\n{name} {value}\n"))
        .collect()
    }

    /// Answer every HTTP request on `listener` with the counters, until
    /// shutdown. One request at a time is plenty for a scraper.
    pub async fn serve_metrics(&self, listener: TcpListener) {
        let mut stop = self.stop_signal();
        loop {
            let accepted = tokio::select! {
                () = stopped(&mut stop) => return,
                accepted = listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    let _ = tokio::time::timeout(TIMEOUT, self.answer(stream)).await;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    async fn answer(&self, mut stream: TcpStream) -> std::io::Result<()> {
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request).await?;
        let body = self.metrics_text();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await?;
        stream.shutdown().await
    }
}
