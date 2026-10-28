//! Swarmgit peer transport (spec sections 7.1 to 7.4).
//!
//! Phase A is Noise `XX` over TCP. Phase B exchanges an ML-KEM-768
//! encapsulation inside the Phase A channel. Phase C authenticates the
//! transcript with the node's hybrid key. Only after all three phases does the
//! session carry application messages.
//!
//! This crate knows nothing about the store or the daemon. It exposes a
//! [`Session`] with request/response matching and an [`Message`] codec.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod addr;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod message;
pub mod raw;
pub mod session;

pub use addr::{PeerAddr, PeerAddrKind};
pub use error::NetError;
pub use handshake::{PeerInfo, Transport};
pub use message::{Message, Rid};
pub use session::{PackStream, Session, SessionHandle};
