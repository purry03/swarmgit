//! Experimental UDP path between two NATed peers (spec 11.10, off unless
//! `--experimental-udp`).
//!
//! Three pieces: [`reflect`] asks a relay how our UDP socket looks from
//! outside, [`punch`] sends probes both ways until one gets through, and
//! [`stream`] turns the socket into a reliable byte stream. The ordinary
//! three-phase handshake runs over that stream, so nothing here is trusted:
//! the token only keeps stray datagrams out.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;

use swarm_core::crypto;

mod stream;

pub use stream::stream;

/// Prefix of a reflector request.
pub const REFLECT_REQUEST: &[u8; 8] = b"SWRUDP1?";
/// Prefix of a reflector reply.
pub const REFLECT_REPLY: &[u8; 8] = b"SWRUDP1!";
/// Requests are padded to this size, and a reply is smaller, so a reflector
/// cannot be used to amplify traffic.
pub const REFLECT_REQUEST_LEN: usize = 64;

const PROBE: &[u8; 4] = b"SWP?";
const PROBE_ACK: &[u8; 4] = b"SWP!";
const PROBE_EVERY: Duration = Duration::from_millis(200);

/// The reply a reflector sends for `request` from `from`, if it is one.
#[must_use]
pub fn reflect_reply(request: &[u8], from: SocketAddr) -> Option<Vec<u8>> {
    if request.len() != REFLECT_REQUEST_LEN || !request.starts_with(REFLECT_REQUEST) {
        return None;
    }
    let mut reply = REFLECT_REPLY.to_vec();
    reply.extend_from_slice(&request[8..16]);
    reply.extend_from_slice(from.to_string().as_bytes());
    Some(reply)
}

/// Ask the reflector at `server` for this socket's address as it sees it.
pub async fn reflect(socket: &UdpSocket, server: SocketAddr) -> Option<SocketAddr> {
    let nonce = crypto::random_bytes::<8>().ok()?;
    let mut request = vec![0u8; REFLECT_REQUEST_LEN];
    request[..8].copy_from_slice(REFLECT_REQUEST);
    request[8..16].copy_from_slice(&nonce);
    let mut buffer = [0u8; 128];
    for _ in 0..3 {
        socket.send_to(&request, server).await.ok()?;
        let wait = tokio::time::timeout(Duration::from_millis(500), socket.recv_from(&mut buffer));
        let Ok(Ok((len, from))) = wait.await else {
            continue;
        };
        let reply = &buffer[..len];
        if from == server
            && reply.len() > 16
            && reply.starts_with(REFLECT_REPLY)
            && reply[8..16] == nonce
        {
            return std::str::from_utf8(&reply[16..]).ok()?.parse().ok();
        }
    }
    None
}

fn packet(kind: &[u8; 4], token: &[u8; 16]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out[..4].copy_from_slice(kind);
    out[4..].copy_from_slice(token);
    out
}

/// Probe every candidate until a path works in both directions: returns the
/// first address that acknowledged one of our probes.
pub async fn punch(
    socket: &Arc<UdpSocket>,
    token: [u8; 16],
    candidates: &[SocketAddr],
    timeout: Duration,
) -> io::Result<SocketAddr> {
    let probe = packet(PROBE, &token);
    let ack = packet(PROBE_ACK, &token);
    let deadline = Instant::now() + timeout;
    // Addresses the peer was seen probing from, which a symmetric NAT may not
    // have listed.
    let mut seen: Vec<SocketAddr> = Vec::new();
    let mut next_round = Instant::now();
    let mut buffer = [0u8; 2048];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no UDP path was found",
            ));
        }
        if now >= next_round {
            for addr in candidates.iter().chain(&seen) {
                // One unreachable candidate does not stop the others.
                let _ = socket.send_to(&probe, addr).await;
            }
            next_round = now + PROBE_EVERY;
        }
        let wait = next_round.min(deadline);
        let Ok(received) = tokio::time::timeout_at(wait, socket.recv_from(&mut buffer)).await
        else {
            continue;
        };
        // Errors such as an ICMP "port unreachable" from a wrong candidate.
        let Ok((len, from)) = received else { continue };
        if len != 20 || buffer[4..20] != token {
            continue;
        }
        if &buffer[..4] == PROBE {
            let _ = socket.send_to(&ack, from).await;
            if !candidates.contains(&from) && !seen.contains(&from) && seen.len() < 4 {
                seen.push(from);
            }
        } else if &buffer[..4] == PROBE_ACK {
            return Ok(from);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_sockets_punch_to_each_other() {
        let a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let token = [3u8; 16];
        let wait = Duration::from_secs(5);
        // A dead candidate beside the live one.
        let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let other = {
            let b = Arc::clone(&b);
            tokio::spawn(async move {
                let peer = punch(&b, token, &[dead, a_addr], wait).await?;
                // Late probes still need an answer: the stream does that.
                let _stream = stream(b, peer, token);
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok::<_, io::Error>(peer)
            })
        };
        assert_eq!(punch(&a, token, &[b_addr], wait).await.unwrap(), b_addr);
        assert_eq!(other.await.unwrap().unwrap(), a_addr);
    }

    #[tokio::test]
    async fn a_wrong_token_does_not_punch() {
        let a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        let wait = Duration::from_millis(600);
        let other = tokio::spawn(async move { punch(&b, [1u8; 16], &[a_addr], wait).await });
        assert!(punch(&a, [2u8; 16], &[b_addr], wait).await.is_err());
        assert!(other.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn the_reflector_reports_the_source_and_never_amplifies() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buffer = [0u8; 128];
            while let Ok((len, from)) = server.recv_from(&mut buffer).await {
                if let Some(reply) = reflect_reply(&buffer[..len], from) {
                    assert!(reply.len() < len);
                    let _ = server.send_to(&reply, from).await;
                }
            }
        });
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(
            reflect(&client, server_addr).await,
            Some(client.local_addr().unwrap())
        );
        assert!(reflect_reply(b"SWRUDP1?short", server_addr).is_none());
    }
}
