//! Swarmgit relay (relay design spec).
//!
//! A relay splices two byte streams so that a peer that cannot accept inbound
//! connections can still be reached. The normal swarm handshake runs end to
//! end through the pipe, so the relay sees only ciphertext.
//!
//! * [`proto`] is the relay wire protocol and its framing.
//! * [`server`] is the relay itself.
//! * [`client`] lets a peer listen through a relay and lets a dialer connect.
//! * [`directory`] finds other relays through the DHT and gossip.
//! * [`cli`] is the command line of a relay server.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod client;
pub mod directory;
pub mod error;
pub mod proto;
pub mod secure;
pub mod server;
pub mod set;
pub mod topics;

mod config;
mod info;
mod metrics;
mod name;
mod network;
mod proof;
mod quota;
mod splice;

pub use client::{
    connect, connect_with, listen, listen_with, lookup, publish, ListenState, RelayListener,
};
pub use directory::{directory_topic, Directory, DirectoryConfig};
pub use error::RelayError;
pub use name::is_domain_name;
pub use network::NetworkConfig;
pub use secure::{key_text, split_key, RelayKey};
pub use server::{Limits, Relay, RelayConfig};
pub use set::{closest, Known, RelaySet, HOME_RELAYS};
pub use topics::topic;

/// Default relay TCP port.
pub const DEFAULT_PORT: u16 = 42425;
