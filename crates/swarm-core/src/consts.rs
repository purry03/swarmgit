//! Fixed protocol constants from spec section 4.2.
//!
//! These are never built dynamically. Change them only with a protocol version bump.

/// Domain separator prefixed to manifest bytes before signing.
pub const CTX_SIG_MANIFEST: &[u8] = b"swarmgit/v1/manifest\0";

/// BLAKE3 derive-key context for DHT topics.
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
