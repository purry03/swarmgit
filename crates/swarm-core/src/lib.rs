//! Swarmgit core: identifiers, canonical CBOR, manifests, resolution and
//! hybrid post-quantum crypto.
//!
//! This crate performs no I/O, starts no threads and knows nothing about the
//! network or the store. Every function is pure except key generation and
//! signing, which read the operating system RNG.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod about;
pub mod accept;
pub mod cbor;
pub mod consts;
pub mod crypto;
pub mod error;
pub mod glob;
pub mod ids;
pub mod keyfile;
pub mod manifest;
pub mod pq;
pub mod refname;
pub mod resolve;

pub use about::{verify_about, About, VerifiedAbout};
pub use accept::{AcceptOutcome, AcceptState, Equivocation, StoredManifest};
pub use error::ErrorCode;
pub use ids::{KeyId, Link, NodeId, Oid, RepoId};
pub use manifest::{encode_envelope, verify_envelope, Envelope, Manifest, VerifiedEnvelope};
pub use resolve::{resolve, Ancestry, Resolved};
