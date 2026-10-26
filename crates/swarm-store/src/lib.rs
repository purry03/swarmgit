//! Swarmgit storage: data directory, sqlite metadata, key files and the git
//! command wrapper. All git work shells out to the `git` binary.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod db;
pub mod git;
pub mod home;
pub mod keyio;
pub mod store;

use swarm_core::error::ErrorCode;
use swarm_core::keyfile::KeyFileError;
use thiserror::Error;

pub use db::{Db, RepoRow};
pub use home::Home;
pub use store::RepoStore;

/// Errors raised by the storage layer.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A git command failed.
    #[error("git: {0}")]
    Git(String),
    /// Sqlite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A protocol-level error code.
    #[error("{0}")]
    Core(#[from] ErrorCode),
    /// A key-file error.
    #[error("{0}")]
    Key(#[from] KeyFileError),
    /// Any other message.
    #[error("{0}")]
    Message(String),
}
