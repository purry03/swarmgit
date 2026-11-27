//! Relay errors.

/// Errors from the relay protocol, server and client.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// Socket I/O failed.
    #[error("relay io: {0}")]
    Io(#[from] std::io::Error),
    /// The peer sent something that is not valid relay protocol.
    #[error("relay protocol: {0}")]
    Protocol(String),
    /// The relay answered with an error.
    #[error("relay error {code}: {msg}")]
    Remote {
        /// Error code, such as `no_such_node`.
        code: String,
        /// Human message.
        msg: String,
    },
    /// Something took too long.
    #[error("relay timeout: {0}")]
    Timeout(&'static str),
    /// The relay address could not be resolved.
    #[error("cannot resolve relay address {0}")]
    Resolve(String),
    /// Proving node ownership failed.
    #[error("relay auth: {0}")]
    Auth(String),
}

impl RelayError {
    /// The error code: the relay's own, or `auth_failed` for a proof or a
    /// relay key that did not check out.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Remote { code, .. } => Some(code),
            Self::Auth(_) => Some("auth_failed"),
            _ => None,
        }
    }
}
