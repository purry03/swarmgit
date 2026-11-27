//! Fixed protocol constants from spec section 4.2.
//!
//! These are never built dynamically. Change them only with a protocol version bump.

/// Domain separator prefixed to manifest bytes before signing.
pub const CTX_SIG_MANIFEST: &[u8] = b"swarmgit/v1/manifest\0";

/// Domain separator prefixed to a repo's name and description before signing.
pub const CTX_SIG_ABOUT: &[u8] = b"swarmgit/v1/about\0";

/// Domain separator prefixed to a merge request event before signing.
pub const CTX_SIG_MR: &[u8] = b"swarmgit/v1/mr\0";

/// BLAKE3 derive-key context for DHT topics, and for the key of the relay
/// rendezvous topics (relay design spec milestone R6).
pub const CTX_TOPIC: &str = "swarmgit/v1/topic";

/// BLAKE3 derive-key context for interest tokens.
pub const CTX_INTEREST: &str = "swarmgit/v1/interest";

/// BLAKE3 derive-key context for LAN tokens.
pub const CTX_LAN: &str = "swarmgit/v1/lan";

/// BLAKE3 derive-key context for key IDs.
pub const CTX_KEYID: &str = "swarmgit/v1/keyid";

/// BLAKE3 derive-key context for session keys.
pub const CTX_SESSION: &str = "swarmgit/v1/session";

/// Domain separator for the connection transcript signature.
pub const CTX_NODEAUTH: &[u8] = b"swarmgit/v1/nodeauth";

/// Domain separator for the relay listener proof (relay design spec REL-1).
pub const CTX_RELAY: &[u8] = b"swarmgit/v1/relayauth";

/// BLAKE3 derive-key context for the public relay directory infohash
/// (relay design spec REL-23).
pub const CTX_RELAY_DIR: &str = "swarmgit/v1 relay directory";

/// BLAKE3 derive-key context for rendezvous hashing of nodes onto relays
/// (relay design spec REL-28).
pub const CTX_RELAY_PLACE: &str = "swarmgit/v1 relay placement";

/// BEP 44 salt prefix for the DHT head pointer.
pub const SALT_HEAD: &[u8] = b"swarmgit/v1/head";

/// BLAKE3 derive-key context that hides the repo ID inside the head salt.
/// The repo ID is secret (see DECISIONS.md), and BEP 44 salts travel in
/// the clear to every DHT node that stores the item.
pub const CTX_HEAD_SALT: &str = "swarmgit/v1/head-salt";

/// Bundle file magic.
pub const MAGIC_BUNDLE: &[u8] = b"SWGB1\n";

/// LAN discovery packet magic.
pub const MAGIC_LAN: &[u8] = b"SWG1";

/// Key file magic.
pub const MAGIC_KEY: &[u8; 4] = b"SWK1";

/// Noise prologue for phase A.
pub const PROLOGUE: &[u8] = b"swarmgit/v1";

/// Protocol version carried in manifests and `Hello`.
pub const PROTOCOL_VERSION: u8 = 1;

/// Manifest version.
pub const MANIFEST_VERSION: u64 = 1;

/// Suite 1: Ed25519 plus ML-DSA-65.
pub const SUITE_HYBRID: u8 = 1;

/// Suite 2: SLH-DSA-SHAKE-128s, recovery keys only.
pub const SUITE_RECOVERY: u8 = 2;

/// Size of the suite 1 public key bundle.
pub const SUITE1_BUNDLE_LEN: usize = 1984;

/// Size of the suite 1 signature.
pub const SUITE1_SIG_LEN: usize = 3373;

/// Size of the suite 2 public key bundle.
pub const SUITE2_BUNDLE_LEN: usize = 32;

/// Size of the suite 2 signature.
pub const SUITE2_SIG_LEN: usize = 7856;

/// Maximum envelope size (1 MiB).
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

/// Maximum number of refs in one manifest.
pub const MAX_REFS: usize = 4096;

/// Maximum ref name length.
pub const MAX_REFNAME_BYTES: usize = 255;

/// Maximum repo name length.
pub const MAX_NAME_BYTES: usize = 64;

/// Maximum repo description length.
pub const MAX_DESCRIPTION_BYTES: usize = 512;

/// Maximum size of one signed name and description record.
pub const MAX_ABOUT_BYTES: usize = 8 * 1024;

/// How far past the local clock a record's revision may lie. It stops one
/// record from outranking every later edit.
pub const MAX_ABOUT_SKEW_SECS: u64 = 24 * 60 * 60;

/// Maximum size of one merge request event, signature included.
pub const MAX_MR_EVENT_BYTES: usize = 64 * 1024;

/// Maximum display name length on a merge request event.
pub const MAX_MR_NAME_BYTES: usize = 64;

/// Maximum `refs/mr` refs (one per merge request and author) kept for a repo.
pub const MAX_MR_REFS: usize = 2000;

/// Maximum `refs/mr` refs one author may hold in a repo.
pub const MAX_MR_REFS_PER_AUTHOR: usize = 200;

/// Maximum events one author may write on one merge request.
pub const MAX_MR_CHAIN: usize = 1000;

/// Maximum size of the pack that brings another peer's merge requests.
pub const MAX_MR_PACK_BYTES: usize = 64 * 1024 * 1024;

/// Maximum maintainers in one root manifest.
pub const MAX_MAINTAINERS: usize = 64;

/// Maximum globs per maintainer.
pub const MAX_GLOBS: usize = 16;

/// Maximum hints in a link.
pub const MAX_HINTS: usize = 8;

/// Maximum link length in bytes.
pub const MAX_LINK_BYTES: usize = 512;

/// Allowed ref prefixes.
pub const REF_PREFIXES: [&str; 2] = ["refs/heads/", "refs/tags/"];
