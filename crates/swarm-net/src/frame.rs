//! Post-handshake framing (spec section 7.2).
//!
//! The encrypted stream is a sequence of `u16` big-endian frames. Each frame
//! holds at most 65535 bytes of ChaCha20-Poly1305 ciphertext, tag included,
//! with a 12-byte nonce made of four zero bytes and a `u64` frame counter.
//! Logical messages are `u32` big-endian length followed by one CBOR array and
//! may span several frames.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroize;

use crate::NetError;

/// Largest encrypted frame, tag included.
pub const MAX_FRAME_CIPHERTEXT: usize = 65535;
/// Largest plaintext in one frame.
pub const MAX_FRAME_PLAINTEXT: usize = MAX_FRAME_CIPHERTEXT - 16;
/// Largest logical message accepted from a peer (8 MiB, spec Defaults).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
/// No nonce may repeat.
const MAX_FRAMES: u64 = 1 << 48;

fn nonce_for(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

/// Encrypting side of a session.
pub struct FrameWriter<W> {
    inner: W,
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl<W> Drop for FrameWriter<W> {
    fn drop(&mut self) {
        // The cipher owns a key copy that chacha20poly1305 zeroizes on drop.
        self.counter = 0;
    }
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    /// Build a writer with a 32-byte direction key.
    pub fn new(inner: W, key: &[u8; 32]) -> Self {
        Self {
            inner,
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
        }
    }

    async fn write_frame(&mut self, plaintext: &[u8]) -> Result<(), NetError> {
        if plaintext.len() > MAX_FRAME_PLAINTEXT {
            return Err(NetError::LimitExceeded(
                "frame plaintext is too large".into(),
            ));
        }
        if self.counter >= MAX_FRAMES {
            return Err(NetError::LimitExceeded("frame counter exhausted".into()));
        }
        let nonce = nonce_for(self.counter);
        self.counter += 1;
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| NetError::Malformed("frame encryption failed".into()))?;
        if ciphertext.len() > MAX_FRAME_CIPHERTEXT {
            return Err(NetError::LimitExceeded(
                "frame ciphertext is too large".into(),
            ));
        }
        self.inner
            .write_all(&(ciphertext.len() as u16).to_be_bytes())
            .await?;
        self.inner.write_all(&ciphertext).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Write arbitrary bytes, splitting them into frames.
    pub async fn write_all(&mut self, mut data: &[u8]) -> Result<(), NetError> {
        while !data.is_empty() {
            let take = data.len().min(MAX_FRAME_PLAINTEXT);
            self.write_frame(&data[..take]).await?;
            data = &data[take..];
        }
        Ok(())
    }

    /// Write one logical message.
    pub async fn write_message(&mut self, message: &[u8]) -> Result<(), NetError> {
        let len = u32::try_from(message.len())
            .map_err(|_| NetError::LimitExceeded("message is too large".into()))?;
        self.write_all(&len.to_be_bytes()).await?;
        self.write_all(message).await
    }

    /// Flush the inner stream.
    pub async fn flush(&mut self) -> Result<(), NetError> {
        self.inner.flush().await?;
        Ok(())
    }
}

/// Decrypting side of a session.
pub struct FrameReader<R> {
    inner: R,
    cipher: ChaCha20Poly1305,
    counter: u64,
    buffer: Vec<u8>,
    position: usize,
}

impl<R> Drop for FrameReader<R> {
    fn drop(&mut self) {
        self.buffer.zeroize();
        self.position = 0;
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Build a reader with a 32-byte direction key.
    pub fn new(inner: R, key: &[u8; 32]) -> Self {
        Self {
            inner,
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
            buffer: Vec::new(),
            position: 0,
        }
    }

    async fn read_frame(&mut self) -> Result<(), NetError> {
        let mut length = [0u8; 2];
        self.inner.read_exact(&mut length).await?;
        let length = usize::from(u16::from_be_bytes(length));
        if !(16..=MAX_FRAME_CIPHERTEXT).contains(&length) {
            return Err(NetError::Malformed("frame length is out of range".into()));
        }
        if self.counter >= MAX_FRAMES {
            return Err(NetError::LimitExceeded("frame counter exhausted".into()));
        }
        let mut ciphertext = vec![0u8; length];
        self.inner.read_exact(&mut ciphertext).await?;
        let nonce = nonce_for(self.counter);
        self.counter += 1;
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
            .map_err(|_| NetError::Malformed("frame authentication failed".into()))?;
        self.buffer = plaintext;
        self.position = 0;
        Ok(())
    }

    async fn read_exact_plain(&mut self, out: &mut [u8]) -> Result<(), NetError> {
        let mut filled = 0;
        while filled < out.len() {
            if self.position >= self.buffer.len() {
                self.read_frame().await?;
                if self.buffer.is_empty() {
                    return Err(NetError::Malformed("empty frame".into()));
                }
            }
            let take = (out.len() - filled).min(self.buffer.len() - self.position);
            out[filled..filled + take]
                .copy_from_slice(&self.buffer[self.position..self.position + take]);
            self.position += take;
            filled += take;
        }
        Ok(())
    }

    /// Read one logical message, rejecting anything above `max`.
    pub async fn read_message(&mut self, max: usize) -> Result<Vec<u8>, NetError> {
        let mut length = [0u8; 4];
        self.read_exact_plain(&mut length).await?;
        let length = u32::from_be_bytes(length) as usize;
        if length > max {
            return Err(NetError::LimitExceeded(format!(
                "message of {length} bytes exceeds {max}"
            )));
        }
        let mut message = vec![0u8; length];
        self.read_exact_plain(&mut message).await?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_large_and_small_messages() {
        let (client, server) = duplex(128 * 1024);
        let key = [7u8; 32];
        let big = vec![9u8; 300_000];
        let to_send = big.clone();
        let writer = tokio::spawn(async move {
            let mut writer = FrameWriter::new(client, &key);
            writer.write_message(b"hello").await.unwrap();
            writer.write_message(&to_send).await.unwrap();
        });
        let mut reader = FrameReader::new(server, &key);
        assert_eq!(reader.read_message(1024).await.unwrap(), b"hello");
        assert_eq!(reader.read_message(MAX_MESSAGE_BYTES).await.unwrap(), big);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn a_flipped_byte_fails_authentication() {
        let key = [1u8; 32];
        let (mut wire, peer) = duplex(4096);
        let mut writer = FrameWriter::new(peer, &key);
        writer.write_message(b"secret").await.unwrap();
        let mut raw = vec![0u8; 2 + 6 + 16];
        wire.read_exact(&mut raw).await.unwrap();
        raw[8] ^= 1;

        let (mut inject, read_side) = duplex(4096);
        inject.write_all(&raw).await.unwrap();
        let mut reader = FrameReader::new(read_side, &key);
        let result = reader.read_message(64).await;
        assert!(matches!(result, Err(NetError::Malformed(_))));
    }
}
