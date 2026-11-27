//! Relay wire protocol (design spec section 4).
//!
//! Before the splice, each frame is a `u32` big-endian length followed by one
//! canonical CBOR array whose first element is the type. Frames are at most
//! [`MAX_FRAME`] bytes. After `Ready` the connection carries raw bytes.
//!
//! Frames are read with exact reads and no read-ahead, so no splice byte is
//! ever consumed by the framing layer.

use std::net::SocketAddr;

use swarm_core::cbor::{self, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::RelayError;

#[path = "proto_fields.rs"]
mod fields;
use fields::{
    addresses, fixed, flag, ids, malformed, read_addresses, read_ids, read_info, text, uint,
};

/// Relay protocol version.
pub const VERSION: u8 = 1;
/// Largest frame accepted before the splice.
pub const MAX_FRAME: usize = 8192;
/// Most relay addresses in one `RRelays`.
pub const MAX_RELAYS_REPLY: usize = 32;
/// Most relay addresses in one `Referral`.
pub const MAX_REFERRALS: usize = 8;
/// Most topics in one `Publish`.
pub const MAX_TOPICS_FRAME: usize = 64;
/// Most nodes in one `Holders`.
pub const MAX_HOLDERS: usize = 32;
/// Most capabilities or limits in one `RInfoReply`.
const MAX_INFO_ITEMS: usize = 16;

/// Role requested in `RHello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Register as reachable through this relay.
    Listen = 1,
    /// Ask for a pipe to a registered node.
    Connect = 2,
    /// Accept a pipe the relay announced with `Incoming`.
    Accept = 3,
    /// Ask the relay about itself and the relay network.
    Info = 4,
    /// Publish rendezvous topics, after the same proof as listening.
    Publish = 5,
}

/// Longest relay password.
pub const MAX_PASSWORD: usize = 128;

/// One relay protocol message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    /// Type 0.
    Hello {
        /// Protocol version.
        version: u8,
        /// What the client wants.
        role: Role,
    },
    /// Type 0 with a third field: `RHello` to a relay that needs a password
    /// (REL-21).
    HelloAuth {
        /// Protocol version.
        version: u8,
        /// What the client wants.
        role: Role,
        /// The relay's shared secret.
        password: String,
    },
    /// Type 1. The sender closes afterwards.
    Error {
        /// Code from the design spec.
        code: String,
        /// Human message.
        msg: String,
    },
    /// Type 2.
    Challenge {
        /// Random nonce to sign.
        nonce: [u8; 32],
    },
    /// Type 3.
    Prove {
        /// Node key bundle.
        bundle: Vec<u8>,
        /// Signature over `relay_auth_input(nonce, name)`.
        sig: Vec<u8>,
        /// The relay address the listener dialed.
        name: String,
    },
    /// Type 4.
    Listening {
        /// How long the registration lasts without a ping.
        ttl_secs: u64,
        /// The listener's address as the relay sees it.
        observed: String,
    },
    /// Type 5.
    Incoming {
        /// Single-use token for the accept connection.
        token: [u8; 16],
    },
    /// Type 6.
    Connect {
        /// Node ID to reach.
        target: [u8; 32],
    },
    /// Type 7.
    Accept {
        /// Token from `Incoming`.
        token: [u8; 16],
    },
    /// Type 8. The raw splice begins after it.
    Ready,
    /// Type 9.
    Ping {
        /// Echoed in the pong.
        nonce: u64,
    },
    /// Type 10.
    Pong {
        /// Nonce of the ping.
        nonce: u64,
    },
    /// Type 11.
    Info,
    /// Type 12.
    InfoReply {
        /// Protocol version.
        version: u8,
        /// What the relay offers.
        caps: Vec<String>,
        /// Named limits.
        limits: Vec<(String, u64)>,
        /// Identifies this relay process.
        relay_id: [u8; 32],
        /// The caller's address as the relay sees it.
        observed: String,
    },
    /// Type 13.
    GetRelays {
        /// Most addresses wanted.
        max: u64,
    },
    /// Type 14.
    Relays {
        /// Other relays this one has verified.
        addrs: Vec<SocketAddr>,
    },
    /// Type 15.
    Where {
        /// Node ID looked for.
        target: [u8; 32],
    },
    /// Type 16.
    Referral {
        /// True when the node is registered with this relay.
        found: bool,
        /// Relays closer to the node by rendezvous hashing.
        closer: Vec<SocketAddr>,
    },
    /// Type 17. Ask a relay to check that the caller is reachable on `port`
    /// of the address the request came from (REL-25).
    DialBack {
        /// The caller's relay port.
        port: u16,
        /// The caller's relay ID, which the dial must find.
        relay_id: [u8; 32],
    },
    /// Type 18.
    DialBackReply {
        /// True when the dial reached the caller.
        reachable: bool,
    },
    /// Type 19. The proven node serves these topics.
    Publish {
        /// Hashed repo IDs.
        topics: Vec<[u8; 32]>,
    },
    /// Type 20.
    Published {
        /// How long the publication lasts.
        ttl_secs: u64,
    },
    /// Type 21.
    Lookup {
        /// Topic looked for.
        topic: [u8; 32],
    },
    /// Type 22.
    Holders {
        /// Node IDs that published the topic here.
        nodes: Vec<[u8; 32]>,
    },
}

impl Msg {
    /// Encode canonically.
    pub fn encode(&self) -> Result<Vec<u8>, RelayError> {
        let items = match self {
            Self::Hello { version, role } => vec![
                Value::Uint(0),
                Value::Uint(u64::from(*version)),
                Value::Uint(*role as u64),
            ],
            Self::HelloAuth {
                version,
                role,
                password,
            } => vec![
                Value::Uint(0),
                Value::Uint(u64::from(*version)),
                Value::Uint(*role as u64),
                Value::Text(password.clone()),
            ],
            Self::Error { code, msg } => vec![
                Value::Uint(1),
                Value::Text(code.clone()),
                Value::Text(msg.chars().take(200).collect()),
            ],
            Self::Challenge { nonce } => vec![Value::Uint(2), Value::Bytes(nonce.to_vec())],
            Self::Prove { bundle, sig, name } => vec![
                Value::Uint(3),
                Value::Bytes(bundle.clone()),
                Value::Bytes(sig.clone()),
                Value::Text(name.clone()),
            ],
            Self::Listening { ttl_secs, observed } => vec![
                Value::Uint(4),
                Value::Uint(*ttl_secs),
                Value::Text(observed.clone()),
            ],
            Self::Incoming { token } => vec![Value::Uint(5), Value::Bytes(token.to_vec())],
            Self::Connect { target } => vec![Value::Uint(6), Value::Bytes(target.to_vec())],
            Self::Accept { token } => vec![Value::Uint(7), Value::Bytes(token.to_vec())],
            Self::Ready => vec![Value::Uint(8)],
            Self::Ping { nonce } => vec![Value::Uint(9), Value::Uint(*nonce)],
            Self::Pong { nonce } => vec![Value::Uint(10), Value::Uint(*nonce)],
            Self::Info => vec![Value::Uint(11)],
            Self::InfoReply {
                version,
                caps,
                limits,
                relay_id,
                observed,
            } => vec![
                Value::Uint(12),
                Value::Uint(u64::from(*version)),
                Value::Array(caps.iter().cloned().map(Value::Text).collect()),
                Value::Map(
                    limits
                        .iter()
                        .map(|(key, value)| (Value::Text(key.clone()), Value::Uint(*value)))
                        .collect(),
                ),
                Value::Bytes(relay_id.to_vec()),
                Value::Text(observed.clone()),
            ],
            Self::GetRelays { max } => vec![Value::Uint(13), Value::Uint(*max)],
            Self::Relays { addrs } => vec![Value::Uint(14), addresses(addrs, MAX_RELAYS_REPLY)],
            Self::Where { target } => vec![Value::Uint(15), Value::Bytes(target.to_vec())],
            Self::Referral { found, closer } => vec![
                Value::Uint(16),
                Value::Bool(*found),
                addresses(closer, MAX_REFERRALS),
            ],
            Self::DialBack { port, relay_id } => vec![
                Value::Uint(17),
                Value::Uint(u64::from(*port)),
                Value::Bytes(relay_id.to_vec()),
            ],
            Self::DialBackReply { reachable } => {
                vec![Value::Uint(18), Value::Bool(*reachable)]
            }
            Self::Publish { topics } => vec![Value::Uint(19), ids(topics, MAX_TOPICS_FRAME)],
            Self::Published { ttl_secs } => vec![Value::Uint(20), Value::Uint(*ttl_secs)],
            Self::Lookup { topic } => vec![Value::Uint(21), Value::Bytes(topic.to_vec())],
            Self::Holders { nodes } => vec![Value::Uint(22), ids(nodes, MAX_HOLDERS)],
        };
        cbor::encode(&Value::Array(items)).map_err(|err| RelayError::Protocol(err.to_string()))
    }

    /// Decode one canonical message.
    pub fn decode(bytes: &[u8]) -> Result<Self, RelayError> {
        let value = cbor::decode(bytes).map_err(|err| RelayError::Protocol(err.to_string()))?;
        let items = value.as_array().ok_or_else(|| malformed("not an array"))?;
        let kind = items
            .first()
            .and_then(Value::as_uint)
            .ok_or_else(|| malformed("no type"))?;
        let want = |n: usize| -> Result<(), RelayError> {
            if items.len() == n {
                Ok(())
            } else {
                Err(malformed("wrong field count"))
            }
        };
        match kind {
            0 => {
                if items.len() != 3 && items.len() != 4 {
                    return Err(malformed("wrong field count"));
                }
                let role = match uint(&items[2], "role")? {
                    1 => Role::Listen,
                    2 => Role::Connect,
                    3 => Role::Accept,
                    4 => Role::Info,
                    5 => Role::Publish,
                    _ => return Err(malformed("unknown role")),
                };
                let version =
                    u8::try_from(uint(&items[1], "version")?).map_err(|_| malformed("version"))?;
                let Some(password) = items.get(3) else {
                    return Ok(Self::Hello { version, role });
                };
                let password = text(password, "password")?;
                if password.len() > MAX_PASSWORD {
                    return Err(malformed("password too long"));
                }
                Ok(Self::HelloAuth {
                    version,
                    role,
                    password,
                })
            }
            1 => {
                want(3)?;
                Ok(Self::Error {
                    code: text(&items[1], "code")?,
                    msg: text(&items[2], "msg")?,
                })
            }
            2 => {
                want(2)?;
                Ok(Self::Challenge {
                    nonce: fixed(&items[1], "nonce")?,
                })
            }
            3 => {
                want(4)?;
                let bundle = items[1]
                    .as_bytes()
                    .ok_or_else(|| malformed("bundle"))?
                    .to_vec();
                let sig = items[2]
                    .as_bytes()
                    .ok_or_else(|| malformed("sig"))?
                    .to_vec();
                Ok(Self::Prove {
                    bundle,
                    sig,
                    name: text(&items[3], "name")?,
                })
            }
            4 => {
                want(3)?;
                Ok(Self::Listening {
                    ttl_secs: uint(&items[1], "ttl")?,
                    observed: text(&items[2], "observed")?,
                })
            }
            5 => {
                want(2)?;
                Ok(Self::Incoming {
                    token: fixed(&items[1], "token")?,
                })
            }
            6 => {
                want(2)?;
                Ok(Self::Connect {
                    target: fixed(&items[1], "target")?,
                })
            }
            7 => {
                want(2)?;
                Ok(Self::Accept {
                    token: fixed(&items[1], "token")?,
                })
            }
            8 => {
                want(1)?;
                Ok(Self::Ready)
            }
            9 => {
                want(2)?;
                Ok(Self::Ping {
                    nonce: uint(&items[1], "nonce")?,
                })
            }
            10 => {
                want(2)?;
                Ok(Self::Pong {
                    nonce: uint(&items[1], "nonce")?,
                })
            }
            11 => {
                want(1)?;
                Ok(Self::Info)
            }
            12 => {
                want(6)?;
                read_info(items)
            }
            13 => {
                want(2)?;
                Ok(Self::GetRelays {
                    max: uint(&items[1], "max")?,
                })
            }
            14 => {
                want(2)?;
                Ok(Self::Relays {
                    addrs: read_addresses(&items[1], MAX_RELAYS_REPLY)?,
                })
            }
            15 => {
                want(2)?;
                Ok(Self::Where {
                    target: fixed(&items[1], "target")?,
                })
            }
            16 => {
                want(3)?;
                Ok(Self::Referral {
                    found: flag(&items[1], "found")?,
                    closer: read_addresses(&items[2], MAX_REFERRALS)?,
                })
            }
            17 => {
                want(3)?;
                let port =
                    u16::try_from(uint(&items[1], "port")?).map_err(|_| malformed("port"))?;
                Ok(Self::DialBack {
                    port,
                    relay_id: fixed(&items[2], "relay id")?,
                })
            }
            18 => {
                want(2)?;
                Ok(Self::DialBackReply {
                    reachable: flag(&items[1], "reachable")?,
                })
            }
            19 => {
                want(2)?;
                Ok(Self::Publish {
                    topics: read_ids(&items[1], MAX_TOPICS_FRAME)?,
                })
            }
            20 => {
                want(2)?;
                Ok(Self::Published {
                    ttl_secs: uint(&items[1], "ttl")?,
                })
            }
            21 => {
                want(2)?;
                Ok(Self::Lookup {
                    topic: fixed(&items[1], "topic")?,
                })
            }
            22 => {
                want(2)?;
                Ok(Self::Holders {
                    nodes: read_ids(&items[1], MAX_HOLDERS)?,
                })
            }
            other => Err(RelayError::Protocol(format!("unknown type {other}"))),
        }
    }
}

/// Write one framed message.
pub async fn write_msg<W>(writer: &mut W, msg: &Msg) -> Result<(), RelayError>
where
    W: AsyncWrite + Unpin,
{
    let body = msg.encode()?;
    if body.len() > MAX_FRAME {
        return Err(RelayError::Protocol("frame too large".into()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one framed message. Reads exactly the frame and nothing after it.
pub async fn read_msg<R>(reader: &mut R) -> Result<Msg, RelayError>
where
    R: AsyncRead + Unpin,
{
    let mut length = [0u8; 4];
    reader.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(RelayError::Protocol("bad frame length".into()));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    Msg::decode(&body)
}

#[cfg(test)]
#[path = "proto_tests.rs"]
mod tests;
