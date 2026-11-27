//! A small reliable byte stream over one punched UDP path: numbered segments,
//! cumulative acknowledgements, a fixed window and retransmission on timeout.
//! It has no congestion control, which is one reason the feature is
//! experimental.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::{packet, PROBE, PROBE_ACK};

/// Payload per datagram, below the usual path MTU.
const SEGMENT: usize = 1200;
/// Segments in flight.
const WINDOW: usize = 64;
const FIRST_RTO: Duration = Duration::from_millis(200);
const MAX_RTO: Duration = Duration::from_secs(2);
/// No acknowledged progress for this long ends the stream.
const GIVE_UP: Duration = Duration::from_secs(20);
/// No datagram at all for this long ends the stream. Sessions ping far more
/// often than this.
const IDLE: Duration = Duration::from_secs(120);

const DATA: u8 = 1;
const ACK: u8 = 2;
const FIN: u8 = 3;
/// Type, token tag, sequence number.
const HEADER: usize = 1 + 4 + 4;

struct Segment {
    seq: u32,
    kind: u8,
    payload: Vec<u8>,
}

fn encode(kind: u8, tag: &[u8], seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.push(kind);
    out.extend_from_slice(tag);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// True when `a` comes before `b` in wrapping sequence space.
fn before(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < u32::MAX / 2
}

struct Sender {
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    tag: [u8; 4],
    next: u32,
    unacked: VecDeque<Segment>,
    rto: Duration,
    resend_at: Instant,
    progressed: Instant,
}

impl Sender {
    async fn transmit(&self, segment: &Segment) {
        let bytes = encode(segment.kind, &self.tag, segment.seq, &segment.payload);
        // A lost send is the same as a lost datagram.
        let _ = self.socket.send_to(&bytes, self.peer).await;
    }

    async fn push(&mut self, kind: u8, payload: Vec<u8>) {
        if self.unacked.is_empty() {
            self.progressed = Instant::now();
            self.resend_at = self.progressed + self.rto;
        }
        let segment = Segment {
            seq: self.next,
            kind,
            payload,
        };
        self.next = self.next.wrapping_add(1);
        self.transmit(&segment).await;
        self.unacked.push_back(segment);
    }

    /// `ack` is the next sequence number the peer wants.
    fn acknowledged(&mut self, ack: u32) {
        let mut moved = false;
        while self.unacked.front().is_some_and(|s| before(s.seq, ack)) {
            self.unacked.pop_front();
            moved = true;
        }
        if moved {
            self.rto = FIRST_RTO;
            self.progressed = Instant::now();
            self.resend_at = self.progressed + self.rto;
        }
    }

    async fn resend(&mut self) {
        // The oldest few are enough to restart the acknowledgements.
        for segment in self.unacked.iter().take(8) {
            self.transmit(segment).await;
        }
        self.rto = (self.rto * 2).min(MAX_RTO);
        self.resend_at = Instant::now() + self.rto;
    }
}

/// Turn a punched socket into a byte stream to `peer`. The returned half
/// reads and writes like a TCP stream; a task carries the bytes.
#[must_use]
pub fn stream(socket: Arc<UdpSocket>, peer: SocketAddr, token: [u8; 16]) -> DuplexStream {
    let (app, inner) = tokio::io::duplex(256 * 1024);
    tokio::spawn(run(socket, peer, token, inner));
    app
}

async fn run(socket: Arc<UdpSocket>, peer: SocketAddr, token: [u8; 16], inner: DuplexStream) {
    let (mut from_app, mut to_app) = tokio::io::split(inner);
    // Delivery runs apart, so a slow reader cannot stall acknowledgements.
    // A full queue drops the segment unacknowledged: that is the flow control.
    let (deliver, mut delivered) = mpsc::channel::<Option<Vec<u8>>>(WINDOW);
    let writer = tokio::spawn(async move {
        while let Some(Some(bytes)) = delivered.recv().await {
            if to_app.write_all(&bytes).await.is_err() {
                return;
            }
        }
        let _ = to_app.shutdown().await;
    });
    let mut tag = [0u8; 4];
    tag.copy_from_slice(&token[..4]);
    let mut sender = Sender {
        socket: Arc::clone(&socket),
        peer,
        tag,
        next: 0,
        unacked: VecDeque::new(),
        rto: FIRST_RTO,
        resend_at: Instant::now(),
        progressed: Instant::now(),
    };
    let mut expected = 0u32;
    let mut early: BTreeMap<u32, (u8, Vec<u8>)> = BTreeMap::new();
    let mut app_open = true;
    let mut peer_open = true;
    let mut heard = Instant::now();
    let mut datagram = vec![0u8; 2048];
    let mut chunk = vec![0u8; SEGMENT];
    while app_open || peer_open || !sender.unacked.is_empty() {
        let waiting = !sender.unacked.is_empty();
        tokio::select! {
            read = from_app.read(&mut chunk), if app_open && sender.unacked.len() < WINDOW => {
                match read {
                    Ok(0) | Err(_) => {
                        app_open = false;
                        sender.push(FIN, Vec::new()).await;
                    }
                    Ok(len) => sender.push(DATA, chunk[..len].to_vec()).await,
                }
            }
            received = socket.recv_from(&mut datagram) => {
                let Ok((len, from)) = received else { continue };
                if from != peer {
                    continue;
                }
                let bytes = &datagram[..len];
                // A peer that has not seen our probe answer yet asks again.
                if len == 20 && &bytes[..4] == PROBE && bytes[4..] == token {
                    let _ = socket.send_to(&packet(PROBE_ACK, &token), peer).await;
                    continue;
                }
                if len < HEADER || bytes[1..5] != tag {
                    continue;
                }
                heard = Instant::now();
                let seq = u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]);
                match bytes[0] {
                    ACK => sender.acknowledged(seq),
                    kind @ (DATA | FIN) => {
                        if !before(seq, expected) && seq.wrapping_sub(expected) < WINDOW as u32 {
                            early.insert(seq, (kind, bytes[HEADER..].to_vec()));
                        }
                        while let Some((kind, payload)) = early.get(&expected) {
                            let item = (*kind == DATA).then(|| payload.clone());
                            // A closed reader still has to be acknowledged,
                            // or the peer retransmits until it gives up.
                            match deliver.try_send(item) {
                                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
                                Err(mpsc::error::TrySendError::Full(_)) => break,
                            }
                            if *kind == FIN {
                                peer_open = false;
                            }
                            early.remove(&expected);
                            expected = expected.wrapping_add(1);
                        }
                        let _ = socket.send_to(&encode(ACK, &tag, expected, &[]), peer).await;
                    }
                    _ => {}
                }
            }
            () = tokio::time::sleep_until(sender.resend_at), if waiting => {
                if sender.progressed.elapsed() >= GIVE_UP {
                    break;
                }
                sender.resend().await;
            }
            () = tokio::time::sleep_until(heard + IDLE) => break,
        }
    }
    drop(deliver);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forwards between two sockets, dropping about one datagram in 7 and
    /// holding back about one in 11 so it arrives after the next. Which ones
    /// is pseudo-random but seeded: a fixed stride can fall into step with the
    /// retransmissions and drop the same segment forever.
    async fn lossy(a: SocketAddr, b: SocketAddr, socket: UdpSocket) {
        let mut buffer = vec![0u8; 2048];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut held: Option<(Vec<u8>, SocketAddr)> = None;
        while let Ok((len, from)) = socket.recv_from(&mut buffer).await {
            let to = if from == a { b } else { a };
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if matches!(state % 7, 0) {
                continue;
            }
            if matches!((state >> 32) % 11, 0) {
                held = Some((buffer[..len].to_vec(), to));
                continue;
            }
            let _ = socket.send_to(&buffer[..len], to).await;
            if let Some((bytes, to)) = held.take() {
                let _ = socket.send_to(&bytes, to).await;
            }
        }
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31) ^ seed)
            .collect()
    }

    #[tokio::test]
    async fn bytes_survive_loss_and_reordering_both_ways() {
        let a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let wire = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let wire_addr = wire.local_addr().unwrap();
        tokio::spawn(lossy(
            a.local_addr().unwrap(),
            b.local_addr().unwrap(),
            wire,
        ));
        let token = [9u8; 16];
        let mut left = stream(a, wire_addr, token);
        let mut right = stream(b, wire_addr, token);
        let (up, down) = (pattern(200_000, 1), pattern(90_000, 2));
        let echo = {
            let (up_len, down) = (up.len(), down.clone());
            tokio::spawn(async move {
                let mut got = vec![0u8; up_len];
                right.read_exact(&mut got).await.unwrap();
                right.write_all(&down).await.unwrap();
                right.shutdown().await.unwrap();
                got
            })
        };
        left.write_all(&up).await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(60), left.read_to_end(&mut got))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.len(), down.len());
        assert!(got == down);
        assert_eq!(echo.await.unwrap(), up);
    }

    #[test]
    fn sequence_numbers_wrap() {
        assert!(before(u32::MAX, 0));
        assert!(before(5, 6));
        assert!(!before(6, 6));
        assert!(!before(0, u32::MAX));
    }
}
