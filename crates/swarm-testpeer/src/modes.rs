//! Hostile peer modes used by the M2 acceptance tests.

use std::net::SocketAddr;
use std::time::Duration;

use swarm_core::crypto::{interest, session_keys};
use swarm_core::ids::RepoId;
use swarm_core::keyfile::NodeKey;
use swarm_core::pq::{Kem, MlKem768Kem};
use swarm_net::frame::FrameWriter;
use swarm_net::raw;
use swarm_net::{Message, NetError};
use tokio::io::AsyncWriteExt;

/// How a test peer should misbehave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Complete all phases and behave normally.
    Honest,
    /// After Phase A, send `PqInit` twice.
    DoublePqInit,
    /// After Phase A, send `Hello` instead of `PqInit`.
    HelloFirst,
    /// After Phase A, send a plaintext frame instead of `PqInit`.
    SkipPhaseB,
    /// Complete phases A and B and then send `Hello` before `PqAuth`.
    HelloBeforeAuth,
    /// Answer `Want` with a `PackData` chunk that flips a byte.
    FlipPack,
    /// Serve a manifest with one signature byte flipped.
    BadSignature,
    /// Serve a manifest with a lower sequence number than the client stores.
    LowerSeq,
    /// Serve a manifest signed by a key the root does not list.
    UnlistedSigner,
    /// Answer `Want` for a non-tip object ID.
    WantNonTip,
}

impl Mode {
    /// Stable name used in test logs.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Honest => "honest",
            Self::DoublePqInit => "double_pq_init",
            Self::HelloFirst => "hello_first",
            Self::SkipPhaseB => "skip_phase_b",
            Self::HelloBeforeAuth => "hello_before_auth",
            Self::FlipPack => "flip_pack",
            Self::BadSignature => "bad_signature",
            Self::LowerSeq => "lower_seq",
            Self::UnlistedSigner => "unlisted_signer",
            Self::WantNonTip => "want_non_tip",
        }
    }
}

/// Result of a hostile probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// True when the server closed without completing the handshake.
    pub closed: bool,
    /// True when the peer sent any application message after Phase A.
    pub got_application_message: bool,
}

fn hello(repo: &RepoId, hash: &[u8; 32]) -> Message {
    Message::Hello {
        version: 1,
        caps: vec!["want".into()],
        observed: Vec::new(),
        interests: vec![interest(repo, hash).to_vec()],
    }
}

fn logical(message: &Message) -> Result<Vec<u8>, NetError> {
    let encoded = message.encode()?;
    let mut data = Vec::with_capacity(4 + encoded.len());
    data.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
    data.extend_from_slice(&encoded);
    Ok(data)
}

fn decode_logical(payload: &[u8]) -> Option<Message> {
    if payload.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    if payload.len() != len + 4 {
        return None;
    }
    Message::decode(&payload[4..]).ok()
}

async fn probe_hello_first(addr: SocketAddr, repo: RepoId) -> Result<Probe, NetError> {
    let key = NodeKey::generate()?;
    let mut phase = raw::connect_tcp(addr, &key).await?;
    let message = hello(&repo, &phase.handshake_hash());
    phase.send_payload(&logical(&message)?).await?;
    closed_after(phase).await
}

async fn probe_skip_phase_b(addr: SocketAddr) -> Result<Probe, NetError> {
    let key = NodeKey::generate()?;
    let phase = raw::connect_tcp(addr, &key).await?;
    let (_reader, mut writer) = phase.into_io();
    // A plaintext frame where the server expects Noise ciphertext.
    writer.write_all(&32u16.to_be_bytes()).await?;
    writer.write_all(&[0u8; 32]).await?;
    writer.flush().await?;
    Ok(Probe {
        closed: true,
        got_application_message: false,
    })
}

async fn probe_double_pq_init(addr: SocketAddr) -> Result<Probe, NetError> {
    let key = NodeKey::generate()?;
    let mut phase = raw::connect_tcp(addr, &key).await?;
    let kem = MlKem768Kem;
    let (ek, _dk) = kem.keygen()?;
    let r_i = swarm_core::crypto::random_bytes::<32>()?;
    let init = Message::PqInit {
        ek: ek.0,
        r_i,
        pad: vec![0; 3],
    };
    phase.send_payload(&logical(&init)?).await?;
    let reply = phase.recv_payload().await?;
    let Some(Message::PqResp { .. }) = decode_logical(&reply) else {
        return Ok(Probe {
            closed: true,
            got_application_message: false,
        });
    };
    // Send a second PqInit where the server now expects `PqAuth`.
    phase.send_payload(&logical(&init)?).await?;
    closed_after(phase).await
}

async fn probe_hello_before_auth(addr: SocketAddr, repo: RepoId) -> Result<Probe, NetError> {
    let key = NodeKey::generate()?;
    let mut phase = raw::connect_tcp(addr, &key).await?;
    let hash = phase.handshake_hash();
    let kem = MlKem768Kem;
    let (ek, dk) = kem.keygen()?;
    let r_i = swarm_core::crypto::random_bytes::<32>()?;
    let init = Message::PqInit {
        ek: ek.0,
        r_i,
        pad: vec![0; 3],
    };
    phase.send_payload(&logical(&init)?).await?;
    let reply = phase.recv_payload().await?;
    let Some(Message::PqResp { ct, r_r, .. }) = decode_logical(&reply) else {
        return Ok(Probe {
            closed: true,
            got_application_message: false,
        });
    };
    let ss = kem.decaps(&dk, &swarm_core::pq::Ciphertext(ct))?;
    let (i2r, _r2i) = session_keys(&hash, &r_i, &r_r, &ss);
    let (reader, writer) = phase.into_io();
    let _reader = reader;
    let mut writer = FrameWriter::new(writer, &i2r);
    let message = hello(&repo, &hash);
    writer.write_message(&message.encode()?).await?;
    Ok(Probe {
        closed: true,
        got_application_message: false,
    })
}

async fn closed_after(mut phase: raw::RawPhaseA) -> Result<Probe, NetError> {
    match tokio::time::timeout(Duration::from_secs(2), phase.recv_payload()).await {
        Ok(Ok(payload)) => {
            let application = decode_logical(&payload).is_some();
            Ok(Probe {
                closed: !application,
                got_application_message: application,
            })
        }
        Ok(Err(_)) => Ok(Probe {
            closed: true,
            got_application_message: false,
        }),
        Err(_) => Ok(Probe {
            closed: false,
            got_application_message: false,
        }),
    }
}

/// Run one hostile probe against a daemon listener.
pub async fn probe(addr: SocketAddr, mode: Mode, repo: RepoId) -> Result<Probe, NetError> {
    match mode {
        Mode::HelloFirst => probe_hello_first(addr, repo).await,
        Mode::SkipPhaseB => probe_skip_phase_b(addr).await,
        Mode::DoublePqInit => probe_double_pq_init(addr).await,
        Mode::HelloBeforeAuth => probe_hello_before_auth(addr, repo).await,
        other => Err(NetError::Malformed(format!(
            "mode {} is handled by a serving fixture",
            other.name()
        ))),
    }
}
