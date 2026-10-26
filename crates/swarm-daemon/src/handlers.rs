//! IPC command dispatch.

use serde_json::{json, Value};

use crate::protocol::{IpcError, Request, Response};
use crate::repo;
use crate::state::DaemonState;

/// True when the command asks the daemon to exit after replying.
#[must_use]
pub fn is_stop(cmd: &str) -> bool {
    cmd == "stop"
}

/// Handle one request.
pub async fn handle(state: &DaemonState, request: Request) -> Response {
    match dispatch(state, &request).await {
        Ok(result) => Response::ok(request.id, result),
        Err(err) => Response::error(request.id, err.code, err.msg),
    }
}

async fn dispatch(state: &DaemonState, request: &Request) -> Result<Value, IpcError> {
    let args = &request.args;
    match request.cmd.as_str() {
        "hello" => Ok(json!({
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "home": state.home.root().to_string_lossy(),
            "port": 0,
        })),
        "init_repo" => repo::init_repo(state, args).await,
        "sync" => repo::sync(state, args).await,
        "publish" => repo::publish(state, args).await,
        "repo_info" => repo::repo_info(state, args).await,
        "list_repos" => repo::list_repos(state, args).await,
        "status" => repo::status(state, args).await,
        "seed" => repo::set_seeding(state, args, true).await,
        "unseed" => repo::set_seeding(state, args, false).await,
        "stop" => Ok(json!({ "stopped": true })),
        other => Err(IpcError::new(
            "malformed",
            format!("unsupported command {other}"),
        )),
    }
}
