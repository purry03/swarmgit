//! Newline-delimited JSON IPC protocol (spec section 9.3).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One request line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Request ID chosen by the client.
    pub id: u64,
    /// Command name.
    pub cmd: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Value,
}

/// One reply line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Request ID.
    pub id: u64,
    /// Whether the command succeeded.
    pub ok: bool,
    /// Result object on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error object on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// Error payload with a machine code from section 5.7.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Lowercase snake-case code.
    pub code: String,
    /// Human-readable message.
    pub msg: String,
}

impl Response {
    /// Successful reply.
    #[must_use]
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// Failed reply.
    #[must_use]
    pub fn error(id: u64, code: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: code.into(),
                msg: msg.into(),
            }),
        }
    }
}

/// An error carrying an IPC code.
#[derive(Debug, Clone)]
pub struct IpcError {
    /// Lowercase snake-case code.
    pub code: String,
    /// Human-readable message.
    pub msg: String,
}

impl IpcError {
    /// Build an error.
    #[must_use]
    pub fn new(code: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.msg)
    }
}

impl std::error::Error for IpcError {}

impl From<swarm_store::StoreError> for IpcError {
    fn from(value: swarm_store::StoreError) -> Self {
        match value {
            swarm_store::StoreError::Core(code) => Self::new(code.to_string(), code.to_string()),
            swarm_store::StoreError::Key(err) => Self::new("bad_keys", err.to_string()),
            other => Self::new("malformed", other.to_string()),
        }
    }
}
