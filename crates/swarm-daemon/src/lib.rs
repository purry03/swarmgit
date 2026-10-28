//! Swarmgit daemon: IPC server, repo orchestration and local store updates.
//!
//! M1 has no network transport yet. `sync` resolves what is already in the
//! store, which is enough for a single-machine clone through the remote helper.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod daemon;
pub mod handlers;
pub mod net;
pub mod protocol;
pub mod repo;
pub mod state;
pub mod sync;

pub use client::{call, Client, ClientError};
pub use protocol::IpcError;
