//! Swarmgit relay (relay design spec, milestone R1).
//!
//! A relay splices two byte streams so that a peer that cannot accept inbound
//! connections can still be reached. The normal swarm handshake runs end to
//! end through the pipe, so the relay sees only ciphertext.
//!
//! * [`proto`] is the relay wire protocol and its framing.
//! * [`server`] is the relay itself.
//! * [`client`] lets a peer listen through a relay and lets a dialer connect.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod error;
pub mod proto;
pub mod server;

pub use client::{connect, listen, ListenState, RelayListener};
pub use error::RelayError;
pub use server::{Limits, Relay, RelayConfig};

/// Default relay TCP port.
pub const DEFAULT_PORT: u16 = 42425;
