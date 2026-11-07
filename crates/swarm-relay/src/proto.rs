//! Relay wire protocol (design spec section 4).
//!
//! Before the splice, each frame is a `u32` big-endian length followed by one
//! canonical CBOR array whose first element is the type. Frames are at most
//! [`MAX_FRAME`] bytes. After `Ready` the connection carries raw bytes.
//!
//! Frames are read with exact reads and no read-ahead, so no splice byte is
//! ever consumed by the framing layer.

use swarm_core::cbor::{self, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::RelayError;

/// Relay protocol version.
pub const VERSION: u8 = 1;
/// Largest frame accepted before the splice.
pub const MAX_FRAME: usize = 8192;

/// Role requested in `RHello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Register as reachable through this relay.
    Listen = 1,
    /// Ask for a pipe to a registered node.
    Connect = 2,
    /// Accept a pipe the relay announced with `Incoming`.
    Accept = 3,
}

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
}

fn malformed(what: &str) -> RelayError {
    RelayError::Protocol(what.to_string())
}

fn fixed<const N: usize>(value: &Value, what: &str) -> Result<[u8; N], RelayError> {
    value
        .as_bytes()
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| malformed(what))
}

fn text(value: &Value, what: &str) -> Result<String, RelayError> {
    value
        .as_text()
        .map(str::to_string)
        .ok_or_else(|| malformed(what))
}

fn uint(value: &Value, what: &str) -> Result<u64, RelayError> {
    value.as_uint().ok_or_else(|| malformed(what))
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
                want(3)?;
                let role = match uint(&items[2], "role")? {
                    1 => Role::Listen,
                    2 => Role::Connect,
                    3 => Role::Accept,
                    _ => return Err(malformed("unknown role")),
                };
                let version =
                    u8::try_from(uint(&items[1], "version")?).map_err(|_| malformed("version"))?;
                Ok(Self::Hello { version, role })
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
mod tests {
    use super::*;

    fn samples() -> Vec<Msg> {
        vec![
            Msg::Hello {
                version: VERSION,
                role: Role::Listen,
            },
            Msg::Error {
                code: "no_such_node".into(),
                msg: "x".into(),
            },
            Msg::Challenge { nonce: [7; 32] },
            Msg::Prove {
                bundle: vec![1; 10],
                sig: vec![2; 10],
                name: "relay:1".into(),
            },
            Msg::Listening {
                ttl_secs: 75,
                observed: "1.2.3.4:5".into(),
            },
            Msg::Incoming { token: [9; 16] },
            Msg::Connect { target: [3; 32] },
            Msg::Accept { token: [4; 16] },
            Msg::Ready,
            Msg::Ping { nonce: 1 },
            Msg::Pong { nonce: 1 },
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for msg in samples() {
            let bytes = msg.encode().unwrap();
            assert_eq!(Msg::decode(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn rejects_unknown_type_and_bad_lengths() {
        let bytes = cbor::encode(&Value::Array(vec![Value::Uint(99)])).unwrap();
        assert!(Msg::decode(&bytes).is_err());
        let bytes = cbor::encode(&Value::Array(vec![
            Value::Uint(5),
            Value::Bytes(vec![0; 15]),
        ]))
        .unwrap();
        assert!(Msg::decode(&bytes).is_err());
        let bytes = cbor::encode(&Value::Array(vec![Value::Uint(8), Value::Uint(1)])).unwrap();
        assert!(Msg::decode(&bytes).is_err());
        assert!(Msg::decode(b"\xff").is_err());
    }

    #[tokio::test]
    async fn frame_limit_is_enforced_and_no_read_ahead() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let mut huge = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        huge.extend(vec![0u8; 16]);
        a.write_all(&huge).await.unwrap();
        assert!(read_msg(&mut b).await.is_err());

        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        write_msg(&mut a, &Msg::Ready).await.unwrap();
        a.write_all(b"RAW").await.unwrap();
        assert_eq!(read_msg(&mut b).await.unwrap(), Msg::Ready);
        let mut rest = [0u8; 3];
        b.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"RAW");
    }
}
