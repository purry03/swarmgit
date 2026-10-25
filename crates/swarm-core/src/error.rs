//! Error codes from spec sections 5.7 and 7.3.

use thiserror::Error;

/// Machine-readable error code used in logs, IPC replies and wire errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum ErrorCode {
    /// Envelope larger than 1 MiB.
    #[error("too_large")]
    TooLarge,
    /// CBOR bytes are not canonical.
    #[error("non_canonical")]
    NonCanonical,
    /// CBOR or schema is malformed.
    #[error("malformed")]
    Malformed,
    /// Manifest version is not 1.
    #[error("bad_version")]
    BadVersion,
    /// Suite is unknown or not allowed for this manifest.
    #[error("unknown_suite")]
    UnknownSuite,
    /// Key bundle is the wrong length or its ID does not match `signer`.
    #[error("bad_keys")]
    BadKeys,
    /// A signature half failed to verify.
    #[error("bad_signature")]
    BadSignature,
    /// A ref name is invalid.
    #[error("bad_refname")]
    BadRefname,
    /// Too many refs, maintainers or globs.
    #[error("too_many_refs")]
    TooManyRefs,
    /// Object ID length does not match the object format.
    #[error("bad_oid_len")]
    BadOidLen,
    /// Manifest belongs to another repo.
    #[error("wrong_repo")]
    WrongRepo,
    /// Signer is not root or an authorized maintainer.
    #[error("unauthorized")]
    Unauthorized,
    /// A non-root signer set a root-only field.
    #[error("forbidden_field")]
    ForbiddenField,
    /// The object format changed after seq 1.
    #[error("object_format_changed")]
    ObjectFormatChanged,
    /// A maintainer listed a ref outside its globs.
    #[error("out_of_scope")]
    OutOfScope,
    /// Sequence number is lower than the stored one.
    #[error("stale")]
    Stale,
    /// Two different manifests share a signer and sequence number.
    #[error("equivocation")]
    Equivocation,
    /// `prev` does not match the previous manifest.
    #[error("bad_prev")]
    BadPrev,
    /// No signing key exists for this repo.
    #[error("no_signing_key")]
    NoSigningKey,
    /// No peer was found.
    #[error("no_peers")]
    NoPeers,
    /// An operation timed out.
    #[error("timeout")]
    Timeout,
    /// A pack was rejected.
    #[error("pack_invalid")]
    PackInvalid,
    /// Phase B of the handshake failed.
    #[error("pq_handshake")]
    PqHandshake,
    /// Node authentication or a pin failed.
    #[error("auth_failed")]
    AuthFailed,
    /// A size or count limit was exceeded.
    #[error("limit_exceeded")]
    LimitExceeded,
    /// No repo is shared on this connection.
    #[error("no_shared_repo")]
    NoSharedRepo,
    /// The requested repo is unknown.
    #[error("unknown_repo")]
    UnknownRepo,
    /// The peer is busy with another request.
    #[error("busy")]
    Busy,
}
