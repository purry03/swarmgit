//! Resolving which repo a CLI command should act on.

use serde_json::{json, Value};
use swarm_core::ids::{Link, RepoId};
use swarm_daemon::Client;
use swarm_store::git::Git;
use swarm_store::Home;

use super::{client_code, parse_link, parse_repo, with_client};
use swarm_daemon::client::ClientError;

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

pub(super) async fn unseed(link: &str, json_output: bool) -> i32 {
    let (repo, _) = match parse_link(link) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call("unseed", json!({ "repo": repo.to_base32() }))
            .await?;
        if json_output {
            println!("{}", json!({ "repo": repo.to_base32(), "unseed": result }));
        } else {
            println!("swarm://{}", repo.to_base32());
        }
        Ok(0)
    })
    .await
}

pub(super) async fn gc(link: &str, json_output: bool) -> i32 {
    let (repo, _) = match parse_link(link) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call("gc", json!({ "repo": repo.to_base32() }))
            .await?;
        if json_output {
            println!("{}", json!({ "repo": repo.to_base32(), "gc": result }));
        } else {
            println!("swarm://{}", repo.to_base32());
        }
        Ok(0)
    })
    .await
}
