//! Network errors mapped to the protocol codes of spec 5.7 and 7.3.

use swarm_core::cbor::CborError;

/// An error from the transport or the wire protocol.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Socket I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Canonical CBOR failed.
    #[error("cbor: {0}")]
    Cbor(#[from] CborError),
    /// A core crypto operation failed.
    #[error("crypto: {0}")]
    Crypto(#[from] swarm_core::crypto::CryptoError),
    /// Phase A failed.
    #[error("noise: {0}")]
    Noise(String),
    /// Phase B failed.
    #[error("pq_handshake: {0}")]
    PqHandshake(String),
    /// Phase C or a pin failed.
    #[error("auth_failed: {0}")]
    AuthFailed(String),
    /// The peer does not hold the requested repo.
    #[error("no_shared_repo")]
    NoSharedRepo,
    /// No peer could be found.
    #[error("no_peers")]
    NoPeers,
    /// No peer could be reached; says what was tried.
    #[error("{0}")]
    NoPeersTried(String),
    /// A relay could not be used to reach a node.
    #[error("relay: {0}")]
    Relay(String),
    /// A pack was rejected or could not be fetched.
    #[error("pack_invalid: {0}")]
    PackInvalid(String),
    /// A message is malformed or of an unexpected type.
    #[error("malformed: {0}")]
    Malformed(String),
    /// A size or count limit was exceeded.
    #[error("limit_exceeded: {0}")]
    LimitExceeded(String),
    /// A request timed out.
    #[error("timeout: {0}")]
    Timeout(String),
    /// The peer closed the connection.
    #[error("closed")]
    Closed,
    /// An error code received from the peer.
    #[error("{code}: {msg}")]
    Remote {
        /// Protocol code.
        code: String,
        /// Human message.
        msg: String,
    },
}

impl NetError {
    /// The machine-readable code used by IPC and the wire `Error` message.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Io(_) => "malformed",
            Self::Cbor(_) => "malformed",
            Self::Crypto(_) => "malformed",
            Self::Noise(_) => "malformed",
            Self::PqHandshake(_) => "pq_handshake",
            Self::AuthFailed(_) => "auth_failed",
            Self::NoSharedRepo => "no_shared_repo",
            Self::NoPeers | Self::NoPeersTried(_) => "no_peers",
            Self::Relay(_) => "no_peers",
            Self::PackInvalid(_) => "pack_invalid",
            Self::Malformed(_) => "malformed",
            Self::LimitExceeded(_) => "limit_exceeded",
            Self::Timeout(_) => "timeout",
            Self::Closed => "timeout",
            Self::Remote { code, .. } => code,
        }
    }

    /// True when the other side sent something the protocol forbids. Going
    /// away, stalling or not holding a repo is not a violation (7.4).
    #[must_use]
    pub fn is_violation(&self) -> bool {
        matches!(
            self,
            Self::Cbor(_)
                | Self::Crypto(_)
                | Self::Noise(_)
                | Self::PqHandshake(_)
                | Self::AuthFailed(_)
                | Self::Malformed(_)
                | Self::LimitExceeded(_)
        )
    }

    /// True when another peer may still be tried.
    #[must_use]
    pub fn is_peer_failure(&self) -> bool {
        matches!(
            self,
            Self::PqHandshake(_)
                | Self::AuthFailed(_)
                | Self::NoSharedRepo
                | Self::NoPeers
                | Self::NoPeersTried(_)
                | Self::Relay(_)
                | Self::PackInvalid(_)
                | Self::Timeout(_)
                | Self::Closed
                | Self::Remote { .. }
        )
    }
}
