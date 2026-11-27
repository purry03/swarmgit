//! The per-session byte cap on relayed connections (`relay_max_bytes`,
//! relay design spec REL-12). Both directions count.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Default cap: 2 GiB, the same as the largest pack.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A stream that fails once `left` bytes have passed.
pub struct Capped<S> {
    inner: S,
    left: u64,
}

impl<S> Capped<S> {
    /// Cap `inner` at `max` bytes in total.
    pub fn new(inner: S, max: u64) -> Self {
        Self { inner, left: max }
    }
}

fn over() -> io::Error {
    io::Error::other("relayed session reached relay_max_bytes")
}

impl<S: AsyncRead + Unpin> AsyncRead for Capped<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.left == 0 {
            return Poll::Ready(Err(over()));
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.inner).poll_read(cx, buf);
        let read = (buf.filled().len() - before) as u64;
        this.left = this.left.saturating_sub(read);
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Capped<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.left == 0 {
            return Poll::Ready(Err(over()));
        }
        let polled = Pin::new(&mut this.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(written)) = &polled {
            this.left = this.left.saturating_sub(*written as u64);
        }
        polled
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn the_cap_counts_both_directions() {
        let (near, mut far) = tokio::io::duplex(64);
        let mut capped = Capped::new(near, 8);
        capped.write_all(b"abcd").await.unwrap();
        far.write_all(b"efgh").await.unwrap();
        let mut got = [0u8; 4];
        capped.read_exact(&mut got).await.unwrap();
        assert!(capped.write_all(b"x").await.is_err());
        assert!(capped.read_exact(&mut got).await.is_err());
    }
}
