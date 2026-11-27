//! IPC command dispatch.

use serde_json::{json, Value};

use crate::protocol::{IpcError, Request, Response};
use crate::repo;
use crate::state::DaemonState;
use crate::sync::progress::Progress;

/// True when the command asks the daemon to exit after replying.
#[must_use]
pub fn is_stop(cmd: &str) -> bool {
    cmd == "stop"
}

/// True when the client wants progress events while the command runs.
#[must_use]
pub fn wants_progress(request: &Request) -> bool {
    matches!(request.cmd.as_str(), "sync" | "inspect")
        && request
            .args
            .get("progress")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Handle one request.
pub async fn handle(state: &DaemonState, request: Request) -> Response {
    handle_with(state, request, Progress::none()).await
}

/// Handle one request, reporting progress to `progress`.
pub async fn handle_with(state: &DaemonState, request: Request, progress: Progress) -> Response {
    match dispatch(state, &request, &progress).await {
        Ok(result) => Response::ok(request.id, result),
        Err(err) => Response::error(request.id, err.code, err.msg),
    }
}

async fn dispatch(
    state: &DaemonState,
    request: &Request,
    progress: &Progress,
) -> Result<Value, IpcError> {
    let args = &request.args;
    match request.cmd.as_str() {
        "hello" => Ok(json!({
            // With a `repo`, the link to share it.
            "link": state.net().and_then(|net| {
                let repo = args.get("repo").and_then(Value::as_str)?;
                let repo = swarm_core::RepoId::from_base32(repo).ok()?;
                Some(net.link(repo).to_string())
            }),
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "home": state.home.root().to_string_lossy(),
            "port": state.net().map(|net| net.listen.port()).unwrap_or(0),
            "listen": state.net().map(|net| net.listen.to_string()).unwrap_or_default(),
            "node_id": state.net().map(|net| net.node_id.to_base32()).unwrap_or_default(),
            "relay_hints": state
                .net()
                .map(|net| net.relay_hints().iter().map(ToString::to_string).collect::<Vec<_>>())
                .unwrap_or_default(),
        })),
        "init_repo" => repo::init_repo(state, args).await,
        "sync" => repo::sync(state, args, progress).await,
        "inspect" => repo::inspect(state, args, progress).await,
        "about" => repo::about(state, args).await,
        "publish" => repo::publish(state, args).await,
        "repo_info" => repo::repo_info(state, args).await,
        "list_repos" => repo::list_repos(state, args).await,
        "status" => repo::status(state, args).await,
        "peers" => repo::peers(state, args).await,
        "seed" => repo::set_seeding(state, args, true).await,
        "unseed" => repo::set_seeding(state, args, false).await,
        "purge" => repo::purge(state, args).await,
        "workdir" => repo::record_workdir(state, args).await,
        "fork" => repo::fork(state, args).await,
        "key_new" => repo::key_new(state, args).await,
        "key_id" => repo::key_id_of(state, args).await,
        "maintainers" => repo::maintainers(state, args).await,
        "accept_ref" => repo::accept_ref(state, args).await,
        "recovery_set" => repo::recovery_set(state, args).await,
        "rotate" => repo::rotate(state, args).await,
        "mirror_export" => repo::mirror_export(state, args).await,
        "export" => repo::export(state, args).await,
        "import" => repo::import(state, args).await,
        "gc" => repo::gc(state, args).await,
        "stop" => Ok(json!({ "stopped": true })),
        other => Err(IpcError::new(
            "malformed",
            format!("unsupported command {other}"),
        )),
    }
}
