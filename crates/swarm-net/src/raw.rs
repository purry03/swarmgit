//! Low-level handshake pieces for the hostile test peer.
//!
//! Ship code uses [`crate::handshake`]. This module deliberately exposes the
//! phase boundaries so `swarm-testpeer` can misbehave in the acceptance tests.

use std::time::Duration;

use snow::params::NoiseParams;
use snow::{Builder, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use swarm_core::consts::PROLOGUE;
use swarm_core::keyfile::NodeKey;

use crate::error::NetError;

const MAX_NOISE_PAYLOAD: usize = 65535;

/// Phase A completed, with the Noise transport still active.
pub struct RawPhaseA {
    state: TransportState,
    reader: Box<dyn AsyncRead + Unpin + Send>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    hash: [u8; 32],
}

impl RawPhaseA {
    /// Phase A handshake hash.
    #[must_use]
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.hash
    }

    /// Send one Noise transport payload.
    pub async fn send_payload(&mut self, payload: &[u8]) -> Result<(), NetError> {
        if payload.len() > MAX_NOISE_PAYLOAD - 16 {
            return Err(NetError::LimitExceeded("raw payload is too large".into()));
        }
        let mut buffer = vec![0u8; MAX_NOISE_PAYLOAD];
        let len = self
            .state
            .write_message(payload, &mut buffer)
            .map_err(|err| NetError::Noise(err.to_string()))?;
        self.writer.write_all(&(len as u16).to_be_bytes()).await?;
        self.writer.write_all(&buffer[..len]).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read one Noise transport payload.
    pub async fn recv_payload(&mut self) -> Result<Vec<u8>, NetError> {
        let mut length = [0u8; 2];
        self.reader.read_exact(&mut length).await?;
        let length = usize::from(u16::from_be_bytes(length));
        if !(16..=MAX_NOISE_PAYLOAD).contains(&length) {
            return Err(NetError::Noise("raw frame length is invalid".into()));
        }
        let mut ciphertext = vec![0u8; length];
        self.reader.read_exact(&mut ciphertext).await?;
        let mut payload = vec![0u8; MAX_NOISE_PAYLOAD];
        let len = self
            .state
            .read_message(&ciphertext, &mut payload)
            .map_err(|err| NetError::Noise(err.to_string()))?;
        payload.truncate(len);
        Ok(payload)
    }

    /// Drop the Noise state and hand back the raw I/O halves.
    pub fn into_io(
        self,
    ) -> (
        Box<dyn AsyncRead + Unpin + Send>,
        Box<dyn AsyncWrite + Unpin + Send>,
    ) {
        (self.reader, self.writer)
    }
}

fn params() -> Result<NoiseParams, NetError> {
    "Noise_XX_25519_ChaChaPoly_BLAKE2s"
        .parse()
        .map_err(|_| NetError::Noise("bad Noise parameters".into()))
}

fn pad() -> Vec<u8> {
    let bytes = swarm_core::crypto::random_bytes::<256>().unwrap_or([0u8; 256]);
    bytes[..usize::from(bytes[0])].to_vec()
}

async fn frame_write<W: AsyncWrite + Unpin>(
    state: &mut snow::HandshakeState,
    writer: &mut W,
    payload: &[u8],
) -> Result<(), NetError> {
    let mut buffer = vec![0u8; MAX_NOISE_PAYLOAD];
    let len = state
        .write_message(payload, &mut buffer)
        .map_err(|err| NetError::Noise(err.to_string()))?;
    writer.write_all(&(len as u16).to_be_bytes()).await?;
    writer.write_all(&buffer[..len]).await?;
    writer.flush().await?;
    Ok(())
}

async fn frame_read<R: AsyncRead + Unpin>(
    state: &mut snow::HandshakeState,
    reader: &mut R,
) -> Result<(), NetError> {
    let mut length = [0u8; 2];
    reader.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if !(16..=MAX_NOISE_PAYLOAD).contains(&length) {
        return Err(NetError::Noise("phase A frame length is invalid".into()));
    }
    let mut ciphertext = vec![0u8; length];
    reader.read_exact(&mut ciphertext).await?;
    let mut payload = vec![0u8; MAX_NOISE_PAYLOAD];
    state
        .read_message(&ciphertext, &mut payload)
        .map_err(|err| NetError::Noise(err.to_string()))?;
    Ok(())
}

async fn exchange<S>(stream: S, key: &NodeKey, initiator: bool) -> Result<RawPhaseA, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let static_key = key.x25519_secret();
    let builder = Builder::new(params()?)
        .local_private_key(static_key)
        .map_err(|err| NetError::Noise(err.to_string()))?
        .prologue(PROLOGUE)
        .map_err(|err| NetError::Noise(err.to_string()))?;
    let mut handshake = if initiator {
        builder
            .build_initiator()
            .map_err(|err| NetError::Noise(err.to_string()))?
    } else {
        builder
            .build_responder()
            .map_err(|err| NetError::Noise(err.to_string()))?
    };
    let padding = pad();
    if initiator {
        frame_write(&mut handshake, &mut writer, &padding).await?;
        frame_read(&mut handshake, &mut reader).await?;
        frame_write(&mut handshake, &mut writer, &padding).await?;
    } else {
        frame_read(&mut handshake, &mut reader).await?;
        frame_write(&mut handshake, &mut writer, &padding).await?;
        frame_read(&mut handshake, &mut reader).await?;
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(handshake.get_handshake_hash());
    let state = handshake
        .into_transport_mode()
        .map_err(|err| NetError::Noise(err.to_string()))?;
    Ok(RawPhaseA {
        state,
        reader: Box::new(reader),
        writer: Box::new(writer),
        hash,
    })
}

/// Run Phase A as the initiator.
pub async fn initiator<S>(stream: S, key: &NodeKey) -> Result<RawPhaseA, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    exchange(stream, key, true).await
}

/// Run Phase A as the responder.
pub async fn responder<S>(stream: S, key: &NodeKey) -> Result<RawPhaseA, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    exchange(stream, key, false).await
}

/// Connect with a timeout, used by the fixture.
pub async fn connect_tcp(addr: std::net::SocketAddr, key: &NodeKey) -> Result<RawPhaseA, NetError> {
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| NetError::Timeout("connect timed out".into()))??;
    initiator(stream, key).await
}
