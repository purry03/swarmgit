//! Resolving which repo a CLI command should act on.

use serde_json::{json, Value};
use swarm_core::ids::{Link, RepoId};
use swarm_daemon::Client;
use swarm_store::git::Git;
use swarm_store::Home;

use super::{client_code, parse_repo};

async fn current_repo() -> Option<RepoId> {
    let cwd = std::env::current_dir().ok()?;
    let url = Git::worktree(&cwd)
        .output(&["remote", "get-url", "swarm"])
        .await
        .ok()?;
    Link::parse(url.trim()).ok().map(|link| link.repo)
}

pub(super) async fn resolve_repo(value: Option<String>) -> Result<RepoId, i32> {
    if let Some(value) = value {
        return parse_repo(&value).map_err(|err| {
            eprintln!("swarm: {err}");
            2
        });
    }
    if let Some(repo) = current_repo().await {
        return Ok(repo);
    }
    let home = Home::resolve().map_err(|err| {
        eprintln!("swarm: {err}");
        1
    })?;
    let mut client = Client::connect(home).await.map_err(|err| {
        eprintln!("swarm: {err}");
        client_code(&err)
    })?;
    let repos = client.call("list_repos", json!({})).await.map_err(|err| {
        eprintln!("swarm: {err}");
        client_code(&err)
    })?;
    let Some(items) = repos.as_array() else {
        return Err(1);
    };
    if items.len() == 1 {
        let value = items[0].get("repo").and_then(Value::as_str).ok_or(1)?;
        return parse_repo(value).map_err(|_| 1);
    }
    eprintln!("swarm: specify a repo");
    Err(2)
}
