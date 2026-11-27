//! Three-phase handshake (spec sections NET-1 to NET-12).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use snow::params::NoiseParams;
use snow::{Builder, HandshakeState, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroize;

use swarm_core::consts::{PROLOGUE, SUITE_HYBRID};
use swarm_core::crypto::{self, auth_input, interest, key_id, session_keys};
use swarm_core::ids::{NodeId, RepoId};
use swarm_core::keyfile::NodeKey;
use swarm_core::pq::{self, Kem, MlKem768Kem, Signer};

use crate::addr::PeerAddr;
use crate::error::NetError;
use crate::frame::{FrameReader, FrameWriter, MAX_MESSAGE_BYTES};
use crate::message::Message;
use crate::transport::{Plain, Shaper};

/// Noise parameters from NET-2.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Largest Phase A or Phase B frame payload.
const MAX_NOISE_PAYLOAD: usize = 65535;
/// ML-KEM-768 encapsulation key length.
pub const KEM_ENCAP_KEY_LEN: usize = 1184;
/// ML-KEM-768 ciphertext length.
pub const KEM_CIPHERTEXT_LEN: usize = 1088;

/// What the handshake learned about the peer.
#[derive(Clone, Debug)]
pub struct PeerInfo {
    /// Authenticated node ID.
    pub node_id: NodeId,
    /// Node public key bundle used by `PqAuth`.
    pub bundle: Vec<u8>,
    /// Peer's protocol version from `Hello`.
    pub version: u8,
    /// Peer capabilities.
    pub caps: Vec<String>,
    /// How the peer says it sees our address.
    pub observed: Vec<u8>,
    /// Shared repos with their sorted index.
    pub shared: Vec<(RepoId, u16)>,
    /// Phase A handshake hash, used for interest tokens.
    pub handshake_hash: [u8; 32],
}

impl PeerInfo {
    /// Index of a repo in the shared set.
    #[must_use]
    pub fn repo_ix(&self, repo: RepoId) -> Option<u16> {
        self.shared
            .iter()
            .find(|(id, _)| *id == repo)
            .map(|(_, ix)| *ix)
    }
}

/// An established, authenticated and encrypted connection before the session
/// actor takes it over.
pub struct Established {
    /// Peer facts.
    pub info: PeerInfo,
    /// Decrypting half.
    pub reader: FrameReader<Box<dyn AsyncRead + Unpin + Send>>,
    /// Encrypting half.
    pub writer: FrameWriter<Box<dyn AsyncWrite + Unpin + Send>>,
}

/// Limits and shaping for one handshake. A bare [`Duration`] converts into
/// options with that timeout and no shaping.
#[derive(Clone, Debug)]
pub struct Options {
    /// Time allowed for the whole handshake.
    pub timeout: Duration,
    /// Padding, timing and frame sizes.
    pub shaper: Arc<dyn Shaper>,
    /// Capabilities to name in `Hello` beside the standard ones.
    pub extra_caps: Vec<String>,
}

impl From<Duration> for Options {
    fn from(timeout: Duration) -> Self {
        Self {
            timeout,
            shaper: Arc::new(Plain),
            extra_caps: Vec::new(),
        }
    }
}

fn noise_params() -> Result<NoiseParams, NetError> {
    NOISE_PARAMS
        .parse()
        .map_err(|_| NetError::Noise("bad Noise parameters".into()))
}

async fn write_noise_frame<W: AsyncWrite + Unpin>(
    state: &mut HandshakeState,
    writer: &mut W,
    payload: &[u8],
) -> Result<(), NetError> {
    let mut buffer = vec![0u8; MAX_NOISE_PAYLOAD];
    let len = state
        .write_message(payload, &mut buffer)
        .map_err(|err| NetError::Noise(err.to_string()))?;
    if len > MAX_NOISE_PAYLOAD {
        return Err(NetError::LimitExceeded("noise frame is too large".into()));
    }
    // Length and payload leave together, so the 2-byte prefix is never a
    // packet of its own.
    let mut frame = Vec::with_capacity(2 + len);
    frame.extend_from_slice(&(len as u16).to_be_bytes());
    frame.extend_from_slice(&buffer[..len]);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_noise_frame<R: AsyncRead + Unpin>(
    state: &mut HandshakeState,
    reader: &mut R,
) -> Result<Vec<u8>, NetError> {
    let mut length = [0u8; 2];
    reader.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if !(16..=MAX_NOISE_PAYLOAD).contains(&length) {
        return Err(NetError::Noise("noise frame length is invalid".into()));
    }
    let mut ciphertext = vec![0u8; length];
    reader.read_exact(&mut ciphertext).await?;
    let mut payload = vec![0u8; MAX_NOISE_PAYLOAD];
    let len = state
        .read_message(&ciphertext, &mut payload)
        .map_err(|err| NetError::Noise(err.to_string()))?;
    payload.truncate(len);
    Ok(payload)
}

async fn write_transport_message<W: AsyncWrite + Unpin>(
    state: &mut TransportState,
    writer: &mut W,
    message: &Message,
) -> Result<(), NetError> {
    let encoded = message.encode()?;
    let mut data = Vec::with_capacity(4 + encoded.len());
    let len = u32::try_from(encoded.len())
        .map_err(|_| NetError::LimitExceeded("message is too large".into()))?;
    data.extend_from_slice(&len.to_be_bytes());
    data.extend_from_slice(&encoded);
    while !data.is_empty() {
        let take = data.len().min(MAX_NOISE_PAYLOAD - 16);
        let mut buffer = vec![0u8; MAX_NOISE_PAYLOAD];
        let len = state
            .write_message(&data[..take], &mut buffer)
            .map_err(|err| NetError::PqHandshake(err.to_string()))?;
        let mut frame = Vec::with_capacity(2 + len);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
        frame.extend_from_slice(&buffer[..len]);
        writer.write_all(&frame).await?;
        writer.flush().await?;
        data.drain(..take);
    }
    Ok(())
}

async fn read_transport_message<R: AsyncRead + Unpin>(
    state: &mut TransportState,
    reader: &mut R,
) -> Result<Message, NetError> {
    // PqInit and PqResp fit in one frame. Reject anything that tries to use
    // several frames here, because no other Phase B message is expected.
    let mut length = [0u8; 2];
    reader.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if !(16..=MAX_NOISE_PAYLOAD).contains(&length) {
        return Err(NetError::PqHandshake(
            "phase B frame length is invalid".into(),
        ));
    }
    let mut ciphertext = vec![0u8; length];
    reader.read_exact(&mut ciphertext).await?;
    let mut payload = vec![0u8; MAX_NOISE_PAYLOAD];
    let len = state
        .read_message(&ciphertext, &mut payload)
        .map_err(|err| NetError::PqHandshake(err.to_string()))?;
    payload.truncate(len);
    if payload.len() < 4 {
        return Err(NetError::PqHandshake("phase B message is too short".into()));
    }
    let declared = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    if declared > MAX_MESSAGE_BYTES || declared != payload.len() - 4 {
        return Err(NetError::PqHandshake(
            "phase B message has a bad length".into(),
        ));
    }
    Message::decode(&payload[4..])
        .map_err(|_| NetError::PqHandshake("phase B message did not decode".into()))
}

struct AuthTranscript<'a> {
    hh: &'a [u8; 32],
    ek: &'a [u8],
    ct: &'a [u8],
    r_i: &'a [u8; 32],
    r_r: &'a [u8; 32],
}

fn verify_auth(
    message: Message,
    role: u8,
    transcript: &AuthTranscript<'_>,
    pin: Option<NodeId>,
) -> Result<(NodeId, Vec<u8>), NetError> {
    let Message::PqAuth { bundle, sig } = message else {
        return Err(NetError::AuthFailed(
            "expected PqAuth as the first session message".into(),
        ));
    };
    if bundle.len() != swarm_core::consts::SUITE1_BUNDLE_LEN
        || sig.len() != swarm_core::consts::SUITE1_SIG_LEN
    {
        return Err(NetError::AuthFailed("PqAuth lengths are wrong".into()));
    }
    let node_id = NodeId::from_bytes(*key_id(&bundle).as_bytes());
    let input = auth_input(
        role,
        transcript.hh,
        transcript.ek,
        transcript.ct,
        transcript.r_i,
        transcript.r_r,
    );
    pq::verify(SUITE_HYBRID, &bundle, &input, &sig)
        .map_err(|err| NetError::AuthFailed(format!("node signature: {err}")))?;
    if let Some(pin) = pin {
        if pin != node_id {
            return Err(NetError::AuthFailed(
                "peer node ID does not match the pin".into(),
            ));
        }
    }
    Ok((node_id, bundle))
}

fn build_hello(
    local_repos: &[RepoId],
    hh: &[u8; 32],
    peer: &PeerAddr,
    extra_caps: &[String],
) -> Message {
    let interests = local_repos
        .iter()
        .take(16)
        .map(|repo| interest(repo, hh).to_vec())
        .collect();
    Message::Hello {
        version: swarm_core::consts::PROTOCOL_VERSION,
        caps: [
            "want",
            "announce",
            crate::message::ABOUT_CAP,
            crate::message::MR_CAP,
        ]
        .into_iter()
        .map(String::from)
        .chain(extra_caps.iter().cloned())
        .collect(),
        observed: peer.encode(None),
        interests,
    }
}

fn shared_repos(local_repos: &[RepoId], peer: &Message, hh: &[u8; 32]) -> Vec<(RepoId, u16)> {
    let Message::Hello { interests, .. } = peer else {
        return Vec::new();
    };
    let mut matches: Vec<([u8; 16], RepoId)> = local_repos
        .iter()
        .take(16)
        .map(|repo| (interest(repo, hh), *repo))
        .filter(|(token, _)| interests.iter().any(|peer_token| peer_token == token))
        .collect();
    matches.sort_by_key(|a| a.0);
    matches
        .into_iter()
        .enumerate()
        .map(|(index, (_, repo))| (repo, index as u16))
        .collect()
}

struct Builder3<'a> {
    key: &'a NodeKey,
    local_repos: &'a [RepoId],
    peer: PeerAddr,
    pin: Option<NodeId>,
    shaper: Arc<dyn Shaper>,
    extra_caps: Vec<String>,
}

/// Wait as long as the shaper asks before the next handshake message.
async fn pause(shaper: &dyn Shaper) {
    let delay = shaper.delay();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

async fn run_handshake<S>(
    stream: S,
    ctx: Builder3<'_>,
    initiator: bool,
) -> Result<Established, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let static_key = ctx.key.x25519_secret();

    // Phase A, NET-2 and NET-3.
    let mut handshake = if initiator {
        Builder::new(noise_params()?)
            .local_private_key(static_key)
            .map_err(|err| NetError::Noise(err.to_string()))?
            .prologue(PROLOGUE)
            .map_err(|err| NetError::Noise(err.to_string()))?
            .build_initiator()
            .map_err(|err| NetError::Noise(err.to_string()))?
    } else {
        Builder::new(noise_params()?)
            .local_private_key(static_key)
            .map_err(|err| NetError::Noise(err.to_string()))?
            .prologue(PROLOGUE)
            .map_err(|err| NetError::Noise(err.to_string()))?
            .build_responder()
            .map_err(|err| NetError::Noise(err.to_string()))?
    };

    let shaper = ctx.shaper.as_ref();
    if initiator {
        pause(shaper).await;
        let pad = shaper.opening_padding();
        write_noise_frame(&mut handshake, &mut write_half, &pad).await?;
        read_noise_frame(&mut handshake, &mut read_half).await?;
        pause(shaper).await;
        let pad = shaper.opening_padding();
        write_noise_frame(&mut handshake, &mut write_half, &pad).await?;
    } else {
        read_noise_frame(&mut handshake, &mut read_half).await?;
        pause(shaper).await;
        let pad = shaper.opening_padding();
        write_noise_frame(&mut handshake, &mut write_half, &pad).await?;
        read_noise_frame(&mut handshake, &mut read_half).await?;
    }
    if !handshake.is_handshake_finished() {
        return Err(NetError::Noise("phase A did not complete".into()));
    }
    let mut hh = [0u8; 32];
    hh.copy_from_slice(handshake.get_handshake_hash());
    let mut phase_a = handshake
        .into_transport_mode()
        .map_err(|err| NetError::Noise(err.to_string()))?;

    // Phase B, NET-4 to NET-7.
    let kem = MlKem768Kem;
    let mut r_i: [u8; 32];
    let mut r_r: [u8; 32];
    let mut shared_secret: [u8; 32];
    let ek: Vec<u8>;
    let ct: Vec<u8>;

    if initiator {
        let (local_ek, decap) = kem
            .keygen()
            .map_err(|err| NetError::PqHandshake(err.to_string()))?;
        ek = local_ek.0;
        r_i = crypto::random_bytes::<32>()
            .map_err(|_| NetError::PqHandshake("randomness unavailable".into()))?;
        pause(shaper).await;
        write_transport_message(
            &mut phase_a,
            &mut write_half,
            &Message::PqInit {
                ek: ek.clone(),
                r_i,
                pad: shaper.padding(),
            },
        )
        .await?;
        let reply = read_transport_message(&mut phase_a, &mut read_half).await?;
        let Message::PqResp {
            ct: remote_ct,
            r_r: remote_r_r,
            pad,
        } = reply
        else {
            return Err(NetError::PqHandshake(
                "expected PqResp as the first Phase B reply".into(),
            ));
        };
        if remote_ct.len() != KEM_CIPHERTEXT_LEN || pad.len() > 255 {
            return Err(NetError::PqHandshake("PqResp has a bad length".into()));
        }
        ct = remote_ct;
        r_r = remote_r_r;
        shared_secret = kem
            .decaps(&decap, &swarm_core::pq::Ciphertext(ct.clone()))
            .map_err(|err| NetError::PqHandshake(err.to_string()))?;
        let mut decap = decap;
        decap.0.zeroize();
    } else {
        let init = read_transport_message(&mut phase_a, &mut read_half).await?;
        let Message::PqInit {
            ek: remote_ek,
            r_i: remote_r_i,
            pad,
        } = init
        else {
            return Err(NetError::PqHandshake(
                "expected PqInit as the first Phase B message".into(),
            ));
        };
        if remote_ek.len() != KEM_ENCAP_KEY_LEN || pad.len() > 255 {
            return Err(NetError::PqHandshake("PqInit has a bad length".into()));
        }
        ek = remote_ek;
        r_i = remote_r_i;
        let (local_ct, secret) = kem
            .encaps(&swarm_core::pq::EncapKey(ek.clone()))
            .map_err(|err| NetError::PqHandshake(err.to_string()))?;
        ct = local_ct.0;
        shared_secret = secret;
        r_r = crypto::random_bytes::<32>()
            .map_err(|_| NetError::PqHandshake("randomness unavailable".into()))?;
        pause(shaper).await;
        write_transport_message(
            &mut phase_a,
            &mut write_half,
            &Message::PqResp {
                ct: ct.clone(),
                r_r,
                pad: shaper.padding(),
            },
        )
        .await?;
    }
    let (i2r, r2i) = session_keys(&hh, &r_i, &r_r, &shared_secret);
    shared_secret.zeroize();
    drop(phase_a);

    // Phase C, NET-10. Both sides send at once under the new keys.
    let (reader_key, writer_key) = if initiator { (r2i, i2r) } else { (i2r, r2i) };
    let boxed_read: Box<dyn AsyncRead + Unpin + Send> = Box::new(read_half);
    let boxed_write: Box<dyn AsyncWrite + Unpin + Send> = Box::new(write_half);
    let mut reader = FrameReader::new(boxed_read, &reader_key);
    let mut writer = FrameWriter::new(boxed_write, &writer_key).shaped(Arc::clone(&ctx.shaper));

    let role = if initiator { 1u8 } else { 2u8 };
    let input = auth_input(role, &hh, &ek, &ct, &r_i, &r_r);
    let signer = ctx
        .key
        .signer()
        .map_err(|err| NetError::AuthFailed(err.to_string()))?;
    let bundle = ctx
        .key
        .bundle()
        .map_err(|err| NetError::AuthFailed(err.to_string()))?;
    let sig = signer
        .try_sign(&input)
        .map_err(|err| NetError::AuthFailed(err.to_string()))?;
    let auth = Message::PqAuth { bundle, sig };
    let encoded = auth.encode()?;
    writer.write_message(&encoded).await?;
    let peer_encoded = reader.read_message(MAX_MESSAGE_BYTES).await?;
    let peer_auth = Message::decode(&peer_encoded)?;
    let peer_role = if initiator { 2u8 } else { 1u8 };
    let transcript = AuthTranscript {
        hh: &hh,
        ek: &ek,
        ct: &ct,
        r_i: &r_i,
        r_r: &r_r,
    };
    let (node_id, peer_bundle) = verify_auth(peer_auth, peer_role, &transcript, ctx.pin)?;

    // Hello and interest matching, 7.2.
    let hello = build_hello(ctx.local_repos, &hh, &ctx.peer, &ctx.extra_caps);
    let encoded = hello.encode()?;
    writer.write_message(&encoded).await?;
    let peer_encoded = reader.read_message(MAX_MESSAGE_BYTES).await?;
    let peer_hello = Message::decode(&peer_encoded)?;
    if let Message::Error { rid, code, msg } = peer_hello {
        if code == "no_shared_repo" {
            return Err(NetError::NoSharedRepo);
        }
        return Err(NetError::Remote {
            code,
            msg: format!("{msg} (rid {rid})"),
        });
    }
    let Message::Hello {
        version,
        caps,
        observed,
        ..
    } = &peer_hello
    else {
        return Err(NetError::Malformed("expected Hello after PqAuth".into()));
    };
    if *version != swarm_core::consts::PROTOCOL_VERSION {
        return Err(NetError::Malformed(format!(
            "peer protocol version {version} is not supported"
        )));
    }
    let shared = shared_repos(ctx.local_repos, &peer_hello, &hh);
    if shared.is_empty() {
        let error = Message::Error {
            rid: 0,
            code: "no_shared_repo".into(),
            msg: "no shared repo".into(),
        };
        let encoded = error.encode()?;
        let _ = writer.write_message(&encoded).await;
        return Err(NetError::NoSharedRepo);
    }

    r_i.zeroize();
    r_r.zeroize();

    Ok(Established {
        info: PeerInfo {
            node_id,
            bundle: peer_bundle,
            version: *version,
            caps: caps.clone(),
            observed: observed.clone(),
            shared,
            handshake_hash: hh,
        },
        reader,
        writer,
    })
}

/// Dial a peer and complete phases A, B and C.
pub async fn connect<S>(
    stream: S,
    key: &NodeKey,
    local_repos: &[RepoId],
    pin: Option<NodeId>,
    peer: PeerAddr,
    options: impl Into<Options>,
) -> Result<Established, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let options = options.into();
    let ctx = Builder3 {
        key,
        local_repos,
        peer,
        pin,
        shaper: options.shaper,
        extra_caps: options.extra_caps,
    };
    tokio::time::timeout(options.timeout, run_handshake(stream, ctx, true))
        .await
        .map_err(|_| NetError::Timeout("handshake timed out".into()))?
}

/// Accept a peer and complete phases A, B and C.
pub async fn accept<S>(
    stream: S,
    key: &NodeKey,
    local_repos: &[RepoId],
    peer: PeerAddr,
    options: impl Into<Options>,
) -> Result<Established, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let options = options.into();
    let ctx = Builder3 {
        key,
        local_repos,
        peer,
        pin: None,
        shaper: options.shaper,
        extra_caps: options.extra_caps,
    };
    tokio::time::timeout(options.timeout, run_handshake(stream, ctx, false))
        .await
        .map_err(|_| NetError::Timeout("handshake timed out".into()))?
}

/// Peer address from a socket, used by the listener.
#[must_use]
pub fn peer_addr(addr: SocketAddr) -> PeerAddr {
    PeerAddr::Tcp(addr)
}
