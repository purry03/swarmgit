//! Control channel privacy (relay design spec milestone R5): Noise NK to the
//! relay's static key, so a path observer sees neither node IDs nor tokens.
//!
//! NK authenticates the relay only. Everything after `Ready` is the pipe's own
//! end-to-end traffic and is sent as it is.
//!
//! A relay address may pin the key: `host:port#<key>`, the key in base32.

use std::net::SocketAddr;
use std::sync::Arc;

use snow::{Builder, StatelessTransportState};
use swarm_core::ids::NodeId;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use crate::proto::{read_msg, write_msg, Msg, MAX_FRAME};
use crate::RelayError;

const PARAMS: &str = "Noise_NK_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"swarmgit/v1 relay control";
/// First bytes of an encrypted connection. As a frame length it is far over
/// [`MAX_FRAME`], so it cannot be mistaken for a plain frame.
pub(crate) const MAGIC: [u8; 4] = *b"SRNK";
const TAG: usize = 16;
/// Largest handshake message.
const MAX_HANDSHAKE: usize = 96;
/// What a relay lists in `RInfoReply.caps` when it accepts NK.
pub const CAP_NK: &str = "nk";

fn noise(err: snow::Error) -> RelayError {
    RelayError::Auth(format!("noise: {err}"))
}

fn builder<'a>() -> Result<Builder<'a>, RelayError> {
    let params = PARAMS.parse().map_err(noise)?;
    Builder::new(params).prologue(PROLOGUE).map_err(noise)
}

/// A relay's static X25519 key. Its public half is the relay ID.
#[derive(Clone)]
pub struct RelayKey {
    private: [u8; 32],
    public: [u8; 32],
}

impl RelayKey {
    /// A new random key.
    pub fn generate() -> Result<Self, RelayError> {
        let params = PARAMS.parse().map_err(noise)?;
        let pair = Builder::new(params).generate_keypair().map_err(noise)?;
        let half = |bytes: &[u8]| {
            <[u8; 32]>::try_from(bytes).map_err(|_| RelayError::Auth("bad key size".into()))
        };
        Ok(Self {
            private: half(&pair.private)?,
            public: half(&pair.public)?,
        })
    }

    /// Restore from [`RelayKey::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, RelayError> {
        let (private, public) = bytes
            .split_first_chunk::<32>()
            .ok_or_else(|| RelayError::Auth("not a relay key".into()))?;
        let public =
            <[u8; 32]>::try_from(public).map_err(|_| RelayError::Auth("not a relay key".into()))?;
        Ok(Self {
            private: *private,
            public,
        })
    }

    /// Both halves, to be saved with owner-only access.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        [self.private, self.public].concat()
    }

    /// The public half.
    #[must_use]
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

impl std::fmt::Debug for RelayKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The private half never reaches a log.
        write!(f, "RelayKey({})", key_text(self.public))
    }
}

/// The key as it is written after `#` in a relay address.
#[must_use]
pub fn key_text(public: [u8; 32]) -> String {
    NodeId::from_bytes(public).to_base32()
}

/// Split `host:port#key` into the address and the pinned key.
pub fn split_key(relay: &str) -> Result<(&str, Option<[u8; 32]>), RelayError> {
    let Some((addr, key)) = relay.split_once('#') else {
        return Ok((relay, None));
    };
    let key = NodeId::from_base32(key)
        .map_err(|_| RelayError::Resolve(format!("bad relay key in {relay}")))?;
    Ok((addr, Some(*key.as_bytes())))
}

/// One direction of the cipher.
struct Seal {
    state: Arc<StatelessTransportState>,
    nonce: u64,
}

impl Seal {
    fn pair(state: StatelessTransportState) -> (Self, Self) {
        let state = Arc::new(state);
        (
            Self {
                state: Arc::clone(&state),
                nonce: 0,
            },
            Self { state, nonce: 0 },
        )
    }
}

async fn send<W>(writer: &mut W, seal: Option<&mut Seal>, msg: &Msg) -> Result<(), RelayError>
where
    W: AsyncWrite + Unpin,
{
    let Some(seal) = seal else {
        return write_msg(writer, msg).await;
    };
    let body = msg.encode()?;
    if body.len() > MAX_FRAME {
        return Err(RelayError::Protocol("frame too large".into()));
    }
    let mut frame = vec![0u8; 4 + body.len() + TAG];
    let len = seal
        .state
        .write_message(seal.nonce, &body, &mut frame[4..])
        .map_err(noise)?;
    seal.nonce += 1;
    frame[..4].copy_from_slice(&(len as u32).to_be_bytes());
    frame.truncate(4 + len);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

async fn recv<R>(reader: &mut R, seal: Option<&mut Seal>) -> Result<Msg, RelayError>
where
    R: AsyncRead + Unpin,
{
    let Some(seal) = seal else {
        return read_msg(reader).await;
    };
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length <= TAG || length > MAX_FRAME + TAG {
        return Err(RelayError::Protocol("bad frame length".into()));
    }
    let mut sealed = vec![0u8; length];
    reader.read_exact(&mut sealed).await?;
    let mut body = vec![0u8; length];
    let len = seal
        .state
        .read_message(seal.nonce, &sealed, &mut body)
        .map_err(noise)?;
    seal.nonce += 1;
    Msg::decode(&body[..len])
}

async fn read_handshake(stream: &mut TcpStream) -> Result<Vec<u8>, RelayError> {
    let mut length = [0u8; 2];
    stream.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if length == 0 || length > MAX_HANDSHAKE {
        return Err(RelayError::Protocol("bad handshake length".into()));
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).await?;
    Ok(body)
}

fn framed(prefix: &[u8], message: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(&(message.len() as u16).to_be_bytes());
    out.extend_from_slice(message);
    out
}

/// A control connection, encrypted or not.
pub struct Channel {
    stream: TcpStream,
    seals: Option<(Seal, Seal)>,
    /// A frame length already taken from the stream while telling the two
    /// kinds of connection apart.
    peeked: Option<[u8; 4]>,
}

impl Channel {
    /// A connection in the clear.
    #[must_use]
    pub fn plain(stream: TcpStream) -> Self {
        Self {
            stream,
            seals: None,
            peeked: None,
        }
    }

    /// Dialer side: run NK against the relay key `public`.
    pub async fn initiate(mut stream: TcpStream, public: [u8; 32]) -> Result<Self, RelayError> {
        let mut handshake = builder()?
            .remote_public_key(&public)
            .map_err(noise)?
            .build_initiator()
            .map_err(noise)?;
        let mut buffer = [0u8; MAX_HANDSHAKE];
        let len = handshake.write_message(&[], &mut buffer).map_err(noise)?;
        stream.write_all(&framed(&MAGIC, &buffer[..len])).await?;
        let reply = read_handshake(&mut stream).await?;
        handshake.read_message(&reply, &mut buffer).map_err(noise)?;
        let state = handshake.into_stateless_transport_mode().map_err(noise)?;
        Ok(Self {
            stream,
            seals: Some(Seal::pair(state)),
            peeked: None,
        })
    }

    /// Relay side: look at the first bytes and answer NK if that is what they
    /// start.
    pub(crate) async fn respond(mut stream: TcpStream, key: &RelayKey) -> Result<Self, RelayError> {
        let mut first = [0u8; 4];
        stream.read_exact(&mut first).await?;
        if first != MAGIC {
            return Ok(Self {
                stream,
                seals: None,
                peeked: Some(first),
            });
        }
        let mut handshake = builder()?
            .local_private_key(&key.private)
            .map_err(noise)?
            .build_responder()
            .map_err(noise)?;
        let hello = read_handshake(&mut stream).await?;
        let mut buffer = [0u8; MAX_HANDSHAKE];
        handshake.read_message(&hello, &mut buffer).map_err(noise)?;
        let len = handshake.write_message(&[], &mut buffer).map_err(noise)?;
        stream.write_all(&framed(&[], &buffer[..len])).await?;
        let state = handshake.into_stateless_transport_mode().map_err(noise)?;
        Ok(Self {
            stream,
            seals: Some(Seal::pair(state)),
            peeked: None,
        })
    }

    /// Send one message.
    pub async fn send(&mut self, msg: &Msg) -> Result<(), RelayError> {
        let seal = self.seals.as_mut().map(|(seal, _)| seal);
        send(&mut self.stream, seal, msg).await
    }

    /// Receive one message.
    pub async fn recv(&mut self) -> Result<Msg, RelayError> {
        if let Some(first) = self.peeked.take() {
            let mut reader = std::io::Cursor::new(first).chain(&mut self.stream);
            return read_msg(&mut reader).await;
        }
        let seal = self.seals.as_mut().map(|(_, seal)| seal);
        recv(&mut self.stream, seal).await
    }

    /// The address the connection arrived on.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.local_addr()
    }

    /// The bare socket, for the pipe that follows `Ready`. Frames are read
    /// exactly, so nothing is buffered here.
    #[must_use]
    pub fn into_raw(self) -> TcpStream {
        self.stream
    }

    /// Separate halves for a long-lived control connection.
    #[must_use]
    pub fn into_split(self) -> (Receiver, Sender) {
        let (read_half, write_half) = self.stream.into_split();
        let (out, in_) = match self.seals {
            Some((out, in_)) => (Some(out), Some(in_)),
            None => (None, None),
        };
        (
            Receiver {
                half: read_half,
                seal: in_,
            },
            Sender {
                half: write_half,
                seal: out,
            },
        )
    }
}

/// The reading half of a [`Channel`].
pub struct Receiver {
    half: OwnedReadHalf,
    seal: Option<Seal>,
}

impl Receiver {
    /// Receive one message. Not cancel safe.
    pub async fn recv(&mut self) -> Result<Msg, RelayError> {
        recv(&mut self.half, self.seal.as_mut()).await
    }
}

/// The writing half of a [`Channel`].
pub struct Sender {
    half: OwnedWriteHalf,
    seal: Option<Seal>,
}

impl Sender {
    /// Send one message.
    pub async fn send(&mut self, msg: &Msg) -> Result<(), RelayError> {
        send(&mut self.half, self.seal.as_mut(), msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (dialed, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        (dialed.unwrap(), accepted.unwrap().0)
    }

    #[tokio::test]
    async fn nk_hides_the_message_and_rejects_a_wrong_key() {
        let key = RelayKey::generate().unwrap();
        let key = RelayKey::from_bytes(&key.to_bytes()).unwrap();
        let (near, far) = pair().await;
        let secret = Msg::Connect { target: [0x5a; 32] };
        let relay = key.clone();
        let server = tokio::spawn(async move {
            // Tap the bytes in passing by reading them raw first.
            let mut peek = vec![0u8; 4];
            far.peek(&mut peek).await.unwrap();
            assert_eq!(peek, MAGIC);
            let mut channel = Channel::respond(far, &relay).await.unwrap();
            let got = channel.recv().await.unwrap();
            channel.send(&Msg::Ready).await.unwrap();
            (got, channel)
        });
        let mut channel = Channel::initiate(near, key.public()).await.unwrap();
        channel.send(&secret).await.unwrap();
        assert_eq!(channel.recv().await.unwrap(), Msg::Ready);
        let (got, _channel) = server.await.unwrap();
        assert_eq!(got, secret);

        let (near, far) = pair().await;
        let relay = key.clone();
        let server = tokio::spawn(async move { Channel::respond(far, &relay).await.map(|_| ()) });
        let wrong = RelayKey::generate().unwrap().public();
        assert!(Channel::initiate(near, wrong).await.is_err());
        assert!(server.await.unwrap().is_err());
    }

    #[test]
    fn addresses_may_pin_a_key() {
        let text = format!("relay.example:42425#{}", key_text([9; 32]));
        assert_eq!(
            split_key(&text).unwrap(),
            ("relay.example:42425", Some([9; 32]))
        );
        assert_eq!(split_key("a:1").unwrap(), ("a:1", None));
        assert!(split_key("a:1#nonsense").is_err());
    }
}
