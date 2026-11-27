//! CBOR wire messages (spec section 7.3).
//!
//! Every message is a canonical CBOR array whose first element is the type
//! number. Handshake messages 14 to 16 use the same codec but only appear in
//! their phase.

use swarm_core::cbor::{self, Value};
use swarm_core::ids::Oid;

use crate::NetError;

/// `Hello` capability of a node that understands messages 19 to 21.
pub const ABOUT_CAP: &str = "about";

/// `Hello` capability of a node that understands messages 22 to 24.
pub const MR_CAP: &str = "mr";

/// Most merge request refs one message may list.
pub const MAX_MR_TIPS: usize = swarm_core::consts::MAX_MR_REFS;

/// One author's chain on one merge request: the ref `refs/mr/<mr>/<author>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MrTip {
    /// Object ID of the opening event.
    pub mr: Oid,
    /// Key ID of the author.
    pub author: [u8; 32],
    /// Object ID of the author's latest event.
    pub tip: Oid,
}

/// A request ID chosen by the requester.
pub type Rid = u64;

/// One protocol message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Type 0.
    Hello {
        /// Protocol version, 1.
        version: u8,
        /// Capability names.
        caps: Vec<String>,
        /// How the sender sees the receiver's address (`PeerAddr` encoding).
        observed: Vec<u8>,
        /// Interest tokens for repos the sender holds or wants.
        interests: Vec<Vec<u8>>,
    },
    /// Type 1.
    Error {
        /// Request this answers, or 0.
        rid: Rid,
        /// Protocol code.
        code: String,
        /// Human message, at most 200 bytes.
        msg: String,
    },
    /// Type 2.
    GetHeads {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
    },
    /// Type 3.
    Heads {
        /// Request ID.
        rid: Rid,
        /// Latest envelope per signer.
        envelopes: Vec<Vec<u8>>,
    },
    /// Type 4.
    GetManifest {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
        /// Signer key ID.
        signer: [u8; 32],
        /// Only answer when the stored seq is at least this.
        min_seq: u64,
    },
    /// Type 5.
    Manifest {
        /// Request ID.
        rid: Rid,
        /// The envelope, or none when there is no newer one.
        envelope: Option<Vec<u8>>,
    },
    /// Type 6.
    Announce {
        /// Shared repo index.
        repo_ix: u16,
        /// One envelope.
        envelope: Vec<u8>,
    },
    /// Type 7.
    GetPeers {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
    },
    /// Type 8.
    Peers {
        /// Request ID.
        rid: Rid,
        /// Encoded peer addresses.
        addresses: Vec<Vec<u8>>,
    },
    /// Type 9.
    Want {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
        /// Tips the requester needs.
        wants: Vec<Oid>,
        /// Tips the requester already has.
        haves: Vec<Oid>,
    },
    /// Type 10.
    PackHeader {
        /// Request ID.
        rid: Rid,
        /// Total pack size when known.
        size_hint: Option<u64>,
    },
    /// Type 11.
    PackData {
        /// Request ID.
        rid: Rid,
        /// Up to 256 KiB of pack bytes.
        chunk: Vec<u8>,
        /// True on the last chunk.
        final_chunk: bool,
    },
    /// Type 12.
    Ping {
        /// Nonce echoed by `Pong`.
        nonce: u64,
    },
    /// Type 13.
    Pong {
        /// Echoed nonce.
        nonce: u64,
    },
    /// Type 17, experimental: "punch a UDP path to me". Sent only to peers
    /// with the `udp-punch` capability.
    PunchOffer {
        /// Request ID.
        rid: Rid,
        /// Marks the probes of this attempt.
        token: [u8; 16],
        /// Encoded candidate addresses of the sender.
        addresses: Vec<Vec<u8>>,
    },
    /// Type 18, experimental: the reply to `PunchOffer`.
    PunchAnswer {
        /// Request ID.
        rid: Rid,
        /// Encoded candidate addresses of the sender.
        addresses: Vec<Vec<u8>>,
    },
    /// Type 19: ask for the repo's name and description. Types 19 to 21 are
    /// sent only to peers with the `about` capability.
    GetAbout {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
    },
    /// Type 20: the reply to `GetAbout`.
    About {
        /// Request ID.
        rid: Rid,
        /// The signed record, or none when there is none.
        record: Option<Vec<u8>>,
    },
    /// Type 21: a newly set name and description.
    AnnounceAbout {
        /// Shared repo index.
        repo_ix: u16,
        /// The signed record.
        record: Vec<u8>,
    },
    /// Type 22: ask for the repo's merge requests. Types 22 to 24 are sent
    /// only to peers with the `mr` capability.
    GetMrs {
        /// Request ID.
        rid: Rid,
        /// Shared repo index.
        repo_ix: u16,
    },
    /// Type 23: the reply to `GetMrs`.
    Mrs {
        /// Request ID.
        rid: Rid,
        /// Every `refs/mr` ref held.
        tips: Vec<MrTip>,
    },
    /// Type 24: merge request events the receiver may not have. The receiver
    /// asks for their objects with `Want` on the same session.
    AnnounceMrs {
        /// Shared repo index.
        repo_ix: u16,
        /// The refs that may be new to the receiver.
        tips: Vec<MrTip>,
    },
    /// Type 14, phase B only.
    PqInit {
        /// ML-KEM-768 encapsulation key, 1184 bytes.
        ek: Vec<u8>,
        /// Initiator randomness.
        r_i: [u8; 32],
        /// Random padding, 0 to 255 bytes.
        pad: Vec<u8>,
    },
    /// Type 15, phase B only.
    PqResp {
        /// ML-KEM-768 ciphertext, 1088 bytes.
        ct: Vec<u8>,
        /// Responder randomness.
        r_r: [u8; 32],
        /// Random padding, 0 to 255 bytes.
        pad: Vec<u8>,
    },
    /// Type 16, phase C only.
    PqAuth {
        /// Node public key bundle, 1984 bytes.
        bundle: Vec<u8>,
        /// Hybrid signature over `auth_input`.
        sig: Vec<u8>,
    },
}

impl Message {
    /// Type number.
    #[must_use]
    pub fn type_number(&self) -> u8 {
        match self {
            Self::Hello { .. } => 0,
            Self::Error { .. } => 1,
            Self::GetHeads { .. } => 2,
            Self::Heads { .. } => 3,
            Self::GetManifest { .. } => 4,
            Self::Manifest { .. } => 5,
            Self::Announce { .. } => 6,
            Self::GetPeers { .. } => 7,
            Self::Peers { .. } => 8,
            Self::Want { .. } => 9,
            Self::PackHeader { .. } => 10,
            Self::PackData { .. } => 11,
            Self::Ping { .. } => 12,
            Self::Pong { .. } => 13,
            Self::PqInit { .. } => 14,
            Self::PqResp { .. } => 15,
            Self::PqAuth { .. } => 16,
            Self::PunchOffer { .. } => 17,
            Self::PunchAnswer { .. } => 18,
            Self::GetAbout { .. } => 19,
            Self::About { .. } => 20,
            Self::AnnounceAbout { .. } => 21,
            Self::GetMrs { .. } => 22,
            Self::Mrs { .. } => 23,
            Self::AnnounceMrs { .. } => 24,
        }
    }

    /// Request ID for messages that carry one.
    #[must_use]
    pub fn rid(&self) -> Option<Rid> {
        match self {
            Self::Error { rid, .. }
            | Self::GetHeads { rid, .. }
            | Self::Heads { rid, .. }
            | Self::GetManifest { rid, .. }
            | Self::Manifest { rid, .. }
            | Self::GetPeers { rid, .. }
            | Self::Peers { rid, .. }
            | Self::Want { rid, .. }
            | Self::PackHeader { rid, .. }
            | Self::PackData { rid, .. }
            | Self::PunchOffer { rid, .. }
            | Self::PunchAnswer { rid, .. }
            | Self::GetAbout { rid, .. }
            | Self::About { rid, .. }
            | Self::GetMrs { rid, .. }
            | Self::Mrs { rid, .. } => Some(*rid),
            _ => None,
        }
    }

    /// True for a message that answers a request. Both ends number their
    /// own requests, so only these may be matched to ours.
    #[must_use]
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            Self::Error { .. }
                | Self::Heads { .. }
                | Self::Manifest { .. }
                | Self::Peers { .. }
                | Self::PackHeader { .. }
                | Self::PackData { .. }
                | Self::PunchAnswer { .. }
                | Self::About { .. }
                | Self::Mrs { .. }
        )
    }

    /// Set the request ID when the message has one.
    pub fn set_rid(&mut self, value: Rid) {
        match self {
            Self::Error { rid, .. }
            | Self::GetHeads { rid, .. }
            | Self::Heads { rid, .. }
            | Self::GetManifest { rid, .. }
            | Self::Manifest { rid, .. }
            | Self::GetPeers { rid, .. }
            | Self::Peers { rid, .. }
            | Self::Want { rid, .. }
            | Self::PackHeader { rid, .. }
            | Self::PackData { rid, .. }
            | Self::PunchOffer { rid, .. }
            | Self::PunchAnswer { rid, .. }
            | Self::GetAbout { rid, .. }
            | Self::About { rid, .. }
            | Self::GetMrs { rid, .. }
            | Self::Mrs { rid, .. } => *rid = value,
            _ => {}
        }
    }

    /// Encode canonically.
    pub fn encode(&self) -> Result<Vec<u8>, NetError> {
        let value = match self {
            Self::Hello {
                version,
                caps,
                observed,
                interests,
            } => array(vec![
                uint(0),
                uint(u64::from(*version)),
                Value::Array(caps.iter().map(|cap| text(cap)).collect()),
                Value::Bytes(observed.clone()),
                Value::Array(
                    interests
                        .iter()
                        .map(|token| Value::Bytes(token.clone()))
                        .collect(),
                ),
            ]),
            Self::Error { rid, code, msg } => {
                array(vec![uint(1), uint(*rid), text(code), text(msg)])
            }
            Self::GetHeads { rid, repo_ix } => {
                array(vec![uint(2), uint(*rid), uint(u64::from(*repo_ix))])
            }
            Self::Heads { rid, envelopes } => array(vec![
                uint(3),
                uint(*rid),
                Value::Array(envelopes.iter().cloned().map(Value::Bytes).collect()),
            ]),
            Self::GetManifest {
                rid,
                repo_ix,
                signer,
                min_seq,
            } => array(vec![
                uint(4),
                uint(*rid),
                uint(u64::from(*repo_ix)),
                Value::Bytes(signer.to_vec()),
                uint(*min_seq),
            ]),
            Self::Manifest { rid, envelope } => array(vec![
                uint(5),
                uint(*rid),
                match envelope {
                    Some(bytes) => Value::Bytes(bytes.clone()),
                    None => Value::Null,
                },
            ]),
            Self::Announce { repo_ix, envelope } => array(vec![
                uint(6),
                uint(u64::from(*repo_ix)),
                Value::Bytes(envelope.clone()),
            ]),
            Self::GetPeers { rid, repo_ix } => {
                array(vec![uint(7), uint(*rid), uint(u64::from(*repo_ix))])
            }
            Self::Peers { rid, addresses } => array(vec![
                uint(8),
                uint(*rid),
                Value::Array(addresses.iter().cloned().map(Value::Bytes).collect()),
            ]),
            Self::Want {
                rid,
                repo_ix,
                wants,
                haves,
            } => array(vec![
                uint(9),
                uint(*rid),
                uint(u64::from(*repo_ix)),
                Value::Array(
                    wants
                        .iter()
                        .map(|oid| Value::Bytes(oid.as_slice().to_vec()))
                        .collect(),
                ),
                Value::Array(
                    haves
                        .iter()
                        .map(|oid| Value::Bytes(oid.as_slice().to_vec()))
                        .collect(),
                ),
            ]),
            Self::PackHeader { rid, size_hint } => array(vec![
                uint(10),
                uint(*rid),
                match size_hint {
                    Some(size) => uint(*size),
                    None => Value::Null,
                },
            ]),
            Self::PackData {
                rid,
                chunk,
                final_chunk,
            } => array(vec![
                uint(11),
                uint(*rid),
                Value::Bytes(chunk.clone()),
                Value::Bool(*final_chunk),
            ]),
            Self::Ping { nonce } => array(vec![uint(12), uint(*nonce)]),
            Self::Pong { nonce } => array(vec![uint(13), uint(*nonce)]),
            Self::PunchOffer {
                rid,
                token,
                addresses,
            } => array(vec![
                uint(17),
                uint(*rid),
                Value::Bytes(token.to_vec()),
                Value::Array(addresses.iter().cloned().map(Value::Bytes).collect()),
            ]),
            Self::PunchAnswer { rid, addresses } => array(vec![
                uint(18),
                uint(*rid),
                Value::Array(addresses.iter().cloned().map(Value::Bytes).collect()),
            ]),
            Self::GetAbout { rid, repo_ix } => {
                array(vec![uint(19), uint(*rid), uint(u64::from(*repo_ix))])
            }
            Self::About { rid, record } => array(vec![
                uint(20),
                uint(*rid),
                match record {
                    Some(bytes) => Value::Bytes(bytes.clone()),
                    None => Value::Null,
                },
            ]),
            Self::AnnounceAbout { repo_ix, record } => array(vec![
                uint(21),
                uint(u64::from(*repo_ix)),
                Value::Bytes(record.clone()),
            ]),
            Self::GetMrs { rid, repo_ix } => {
                array(vec![uint(22), uint(*rid), uint(u64::from(*repo_ix))])
            }
            Self::Mrs { rid, tips } => array(vec![uint(23), uint(*rid), mr_tips(tips)]),
            Self::AnnounceMrs { repo_ix, tips } => {
                array(vec![uint(24), uint(u64::from(*repo_ix)), mr_tips(tips)])
            }
            Self::PqInit { ek, r_i, pad } => array(vec![
                uint(14),
                Value::Bytes(ek.clone()),
                Value::Bytes(r_i.to_vec()),
                Value::Bytes(pad.clone()),
            ]),
            Self::PqResp { ct, r_r, pad } => array(vec![
                uint(15),
                Value::Bytes(ct.clone()),
                Value::Bytes(r_r.to_vec()),
                Value::Bytes(pad.clone()),
            ]),
            Self::PqAuth { bundle, sig } => array(vec![
                uint(16),
                Value::Bytes(bundle.clone()),
                Value::Bytes(sig.clone()),
            ]),
        };
        cbor::encode(&value).map_err(NetError::from)
    }
}

fn mr_tips(tips: &[MrTip]) -> Value {
    Value::Array(
        tips.iter()
            .map(|tip| {
                array(vec![
                    Value::Bytes(tip.mr.as_slice().to_vec()),
                    Value::Bytes(tip.author.to_vec()),
                    Value::Bytes(tip.tip.as_slice().to_vec()),
                ])
            })
            .collect(),
    )
}

fn uint(value: u64) -> Value {
    Value::Uint(value)
}

fn text(value: &str) -> Value {
    Value::Text(value.to_string())
}

fn array(items: Vec<Value>) -> Value {
    Value::Array(items)
}

mod decode;
