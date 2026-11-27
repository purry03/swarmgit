//! The transport seam (spec 3.4 and 11.10).
//!
//! A [`Transport`] opens connections and says how the handshake and frames on
//! them are shaped. [`ObfsTransport`] wraps another transport and randomizes
//! that shape. A pluggable transport that disguises the byte stream itself
//! implements `dial` and `wrap`; sync code does not change.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use swarm_core::crypto;

use crate::frame::MAX_FRAME_PLAINTEXT;
use crate::proxy::{self, Endpoint};

/// A byte stream a session can run over.
pub trait Conn: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Conn for T {}

/// A boxed [`Conn`].
pub type BoxConn = Box<dyn Conn>;

/// The future a dial returns.
pub type Dial<'a> = Pin<Box<dyn Future<Output = io::Result<BoxConn>> + Send + 'a>>;

/// What an observer can see of a session: sizes and timing. All of it stays
/// within the wire format, so shaped and unshaped nodes interoperate.
pub trait Shaper: Send + Sync + std::fmt::Debug {
    /// Padding for a Phase B message, 0 to 255 bytes (NET-4). The peer
    /// rejects more.
    fn padding(&self) -> Vec<u8> {
        random_padding(255)
    }

    /// Padding for a Phase A message (NET-3). Its length is not checked by
    /// the peer.
    fn opening_padding(&self) -> Vec<u8> {
        random_padding(255)
    }

    /// Pause before sending a handshake message.
    fn delay(&self) -> Duration {
        Duration::ZERO
    }

    /// Largest plaintext to put in the next frame.
    fn frame_len(&self) -> usize {
        MAX_FRAME_PLAINTEXT
    }
}

/// The unshaped behaviour of the protocol.
#[derive(Clone, Copy, Debug, Default)]
pub struct Plain;

impl Shaper for Plain {}

/// Longest random pause before a handshake message.
pub const MAX_JITTER: Duration = Duration::from_millis(30);
/// Longest Phase A padding of the randomized policy.
pub const MAX_OBFS_OPENING: usize = 1024;
/// Smallest frame the randomized policy cuts.
pub const MIN_OBFS_FRAME: usize = 256;
/// Largest frame the randomized policy cuts.
pub const MAX_OBFS_FRAME: usize = 16 * 1024;

fn random_below(bound: u32) -> u32 {
    let bytes = crypto::random_bytes::<4>().unwrap_or([0u8; 4]);
    u32::from_le_bytes(bytes) % bound.max(1)
}

/// Between 0 and `max` random bytes, the length uniform.
fn random_padding(max: usize) -> Vec<u8> {
    let bound = u32::try_from(max).unwrap_or(0).saturating_add(1);
    let mut out = Vec::with_capacity(max);
    let len = random_below(bound) as usize;
    while out.len() < len {
        let block = crypto::random_bytes::<64>().unwrap_or([0u8; 64]);
        out.extend_from_slice(&block);
    }
    out.truncate(len);
    out
}

/// Random padding, random pauses in the handshake and random frame sizes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Randomized;

impl Shaper for Randomized {
    fn opening_padding(&self) -> Vec<u8> {
        random_padding(MAX_OBFS_OPENING)
    }

    fn delay(&self) -> Duration {
        let max = u32::try_from(MAX_JITTER.as_micros()).unwrap_or(0);
        Duration::from_micros(u64::from(random_below(max + 1)))
    }

    fn frame_len(&self) -> usize {
        let span = (MAX_OBFS_FRAME - MIN_OBFS_FRAME + 1) as u32;
        MIN_OBFS_FRAME + random_below(span) as usize
    }
}

/// Opens connections for peer sessions.
pub trait Transport: Send + Sync {
    /// Connect to a peer.
    fn dial<'a>(&'a self, to: &'a Endpoint) -> Dial<'a>;

    /// Wrap a stream this node did not dial: accepted or relayed.
    fn wrap(&self, conn: BoxConn) -> BoxConn {
        conn
    }

    /// How sessions on this transport are shaped.
    fn shaper(&self) -> Arc<dyn Shaper> {
        Arc::new(Plain)
    }
}

/// TCP, through the configured SOCKS5 proxy when there is one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tcp;

impl Transport for Tcp {
    fn dial<'a>(&'a self, to: &'a Endpoint) -> Dial<'a> {
        Box::pin(async move {
            let stream = proxy::connect(to).await?;
            Ok(Box::new(stream) as BoxConn)
        })
    }
}

/// Randomized shaping over another transport.
#[derive(Clone, Copy, Debug, Default)]
pub struct ObfsTransport<T: Transport>(pub T);

impl<T: Transport> Transport for ObfsTransport<T> {
    fn dial<'a>(&'a self, to: &'a Endpoint) -> Dial<'a> {
        self.0.dial(to)
    }

    fn wrap(&self, conn: BoxConn) -> BoxConn {
        self.0.wrap(conn)
    }

    fn shaper(&self) -> Arc<dyn Shaper> {
        Arc::new(Randomized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randomized_shape_stays_inside_the_protocol_limits() {
        let shape = ObfsTransport(Tcp).shaper();
        let mut sizes = std::collections::HashSet::new();
        for _ in 0..200 {
            let len = shape.frame_len();
            assert!((MIN_OBFS_FRAME..=MAX_OBFS_FRAME).contains(&len));
            assert!(shape.delay() <= MAX_JITTER);
            assert!(shape.padding().len() <= 255);
            assert!(shape.opening_padding().len() <= MAX_OBFS_OPENING);
            sizes.insert(len);
        }
        assert!(sizes.len() > 100);
        assert_eq!(Tcp.shaper().frame_len(), MAX_FRAME_PLAINTEXT);
        assert_eq!(Tcp.shaper().delay(), Duration::ZERO);
    }
}
