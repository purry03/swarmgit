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

/// The daemon writes and reads the file, so hand it an absolute path.
fn absolute(path: &std::path::Path) -> Result<String, ClientError> {
    std::path::absolute(path)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|err| ClientError::Unreachable(err.to_string()))
}

pub(super) async fn export(link: &str, output: &std::path::Path, json_output: bool) -> i32 {
    let (repo, _) = match parse_link(link) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let output = output.to_path_buf();
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "export",
                json!({ "repo": repo.to_base32(), "path": absolute(&output)? }),
            )
            .await?;
        if json_output {
            println!("{result}");
        } else {
            println!("{}", output.display());
        }
        Ok(0)
    })
    .await
}

pub(super) async fn import(file: &std::path::Path, json_output: bool) -> i32 {
    let file = file.to_path_buf();
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call("import", json!({ "path": absolute(&file)? }))
            .await?;
        if json_output {
            println!("{result}");
        } else if let Some(link) = result.get("link").and_then(Value::as_str) {
            println!("{link}");
        }
        Ok(0)
    })
    .await
}

/// `swarm key new|id`: print the key ID so it can be sent to the root owner.
pub(super) async fn key_cmd(cmd: &'static str, repo: &str) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(cmd, json!({ "repo": repo.to_base32() }))
            .await?;
        println!(
            "{}",
            result
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        if let Some(role) = result.get("role").and_then(Value::as_str) {
            eprintln!("swarm: this key is {role} for the repo");
        }
        Ok(0)
    })
    .await
}

pub(super) async fn maintainers_cmd(
    op: &'static str,
    repo: &str,
    key: Option<&str>,
    globs: Vec<String>,
) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let key = key.map(str::to_string);
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "maintainers",
                json!({ "repo": repo.to_base32(), "op": op, "key": key, "globs": globs }),
            )
            .await?;
        for item in result
            .get("maintainers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let globs = item
                .get("globs")
                .and_then(Value::as_array)
                .map(|g| {
                    g.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            println!(
                "{} {globs}",
                item.get("key").and_then(Value::as_str).unwrap_or_default()
            );
        }
        Ok(0)
    })
    .await
}

pub(super) async fn accept(repo: &str, refname: &str, signer: &str) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let (refname, signer) = (refname.to_string(), signer.to_string());
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "accept_ref",
                json!({ "repo": repo.to_base32(), "ref": refname, "signer": signer }),
            )
            .await?;
        println!(
            "{} {}",
            refname,
            result
                .get("oid")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        Ok(0)
    })
    .await
}

pub(super) async fn recovery_cmd(repo: &str, output: &std::path::Path) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let output = output.to_path_buf();
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "recovery_set",
                json!({ "repo": repo.to_base32(), "path": absolute(&output)? }),
            )
            .await?;
        println!("{}", result.get("recovery").and_then(Value::as_str).unwrap_or_default());
        eprintln!(
            "swarm: recovery key saved to {}; keep it offline. It is the only way back if the root key is lost or stolen.",
            output.display()
        );
        Ok(0)
    })
    .await
}

pub(super) async fn rotate_cmd(repo: &str, recovery: &std::path::Path) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let recovery = recovery.to_path_buf();
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "rotate",
                json!({ "repo": repo.to_base32(), "recovery": absolute(&recovery)? }),
            )
            .await?;
        println!(
            "{}",
            result
                .get("new_root")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        Ok(0)
    })
    .await
}

pub(super) async fn mirror_export(link: &str, dir: &std::path::Path) -> i32 {
    let (repo, _) = match parse_link(link) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let dir = dir.to_path_buf();
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let result = client
            .call(
                "mirror_export",
                json!({ "repo": repo.to_base32(), "dir": absolute(&dir)? }),
            )
            .await?;
        println!(
            "{}",
            result
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
        Ok(0)
    })
    .await
}
