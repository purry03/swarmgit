//! Command-line interface (spec section 9.1).

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use swarm_core::ids::{Link, RepoId};
use swarm_daemon::client::ClientError;
use swarm_daemon::{repo as daemon_repo, Client};
use swarm_store::git::{self, Git};
use swarm_store::{keyio, Home};

#[derive(Parser)]
#[command(name = "swarm", version, about = "Zero-setup peer-to-peer git")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a repo key and publish the first manifest.
    Init {
        /// Object format for a newly created git repo.
        #[arg(long, value_name = "FORMAT")]
        object_format: Option<String>,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Print the link for a repo.
    Link {
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
        repo: Option<String>,
    },
    /// Show repos, sequence numbers and conflicts.
    Status {
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
        repo: Option<String>,
    },
    /// Key management.
    Key {
        #[command(subcommand)]
        command: KeyCmd,
    },
    /// Run, start or stop the daemon.
    Daemon {
        /// Stay in the foreground.
        #[arg(long)]
        foreground: bool,
        /// Run as an already-detached process (used by autostart).
        #[arg(long, hide = true)]
        detach: bool,
        #[command(subcommand)]
        action: Option<DaemonCmd>,
    },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Copy a repo signing key to FILE with mode 0600.
    Backup {
        repo: String,
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Stop the running daemon.
    Stop,
}

fn parse_repo(value: &str) -> Result<RepoId, String> {
    daemon_repo::parse_repo(value).map_err(|err| err.msg)
}

fn remote_code(code: &str) -> i32 {
    match code {
        "no_peers" => 3,
        "no_signing_key" => 4,
        "too_large"
        | "non_canonical"
        | "malformed"
        | "bad_version"
        | "unknown_suite"
        | "bad_keys"
        | "bad_signature"
        | "bad_refname"
        | "too_many_refs"
        | "bad_oid_len"
        | "wrong_repo"
        | "unauthorized"
        | "forbidden_field"
        | "object_format_changed"
        | "out_of_scope"
        | "stale"
        | "equivocation"
        | "bad_prev" => 6,
        _ => 1,
    }
}

fn client_code(err: &ClientError) -> i32 {
    match err {
        ClientError::Unreachable(_) => 5,
        ClientError::Remote(remote) => remote_code(&remote.code),
        _ => 1,
    }
}

async fn current_repo() -> Option<RepoId> {
    let cwd = std::env::current_dir().ok()?;
    let url = Git::worktree(&cwd)
        .output(&["remote", "get-url", "swarm"])
        .await
        .ok()?;
    Link::parse(url.trim()).ok().map(|link| link.repo)
}

async fn resolve_repo(value: Option<String>) -> Result<RepoId, i32> {
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

async fn init(object_format: Option<String>, json_output: bool) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    if let Some(format) = &object_format {
        if format != "sha1" && format != "sha256" {
            eprintln!("swarm: --object-format must be sha1 or sha256");
            return 2;
        }
    }
    let existing = git::is_git_worktree(&cwd).await;
    if !existing {
        match git::init_worktree(&cwd, object_format.as_deref()).await {
            Ok(format) => {
                if format == "sha1" && object_format.is_none() {
                    eprintln!("swarm: this git does not support sha256; using sha1");
                }
            }
            Err(err) => {
                eprintln!("swarm: {err}");
                return 1;
            }
        }
    } else if let Some(requested) = &object_format {
        match git::worktree_object_format(&cwd).await {
            Ok(actual) if &actual != requested => {
                eprintln!("swarm: this repo uses {actual}, not the requested {requested}");
                return 1;
            }
            Err(err) => {
                eprintln!("swarm: {err}");
                return 1;
            }
            _ => {}
        }
    }

    let worktree = Git::worktree(&cwd);
    if worktree
        .output(&["remote", "get-url", "swarm"])
        .await
        .is_ok()
    {
        eprintln!("swarm: this repository already has a remote named swarm");
        return 1;
    }
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    let mut client = match Client::connect(home).await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    let result = match client
        .call("init_repo", json!({ "git_dir": cwd.to_string_lossy() }))
        .await
    {
        Ok(result) => result,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    let link = result
        .get("link")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Err(err) = worktree.output(&["remote", "add", "swarm", link]).await {
        eprintln!("swarm: could not add remote swarm: {err}");
        return 1;
    }
    if json_output {
        println!(
            "{}",
            json!({
                "link": link,
                "repo_id": result.get("repo_id"),
                "object_format": result.get("object_format"),
            })
        );
    } else {
        println!("{link}");
    }
    0
}

async fn link(repo: Option<String>, json_output: bool) -> i32 {
    let repo = match resolve_repo(repo).await {
        Ok(repo) => repo,
        Err(code) => return code,
    };
    let link = format!("swarm://{}", repo.to_base32());
    if json_output {
        println!("{}", json!({ "link": link }));
    } else {
        println!("{link}");
    }
    0
}

async fn status(json_output: bool, repo: Option<String>) -> i32 {
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    let mut client = match Client::connect(home).await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    let mut args = json!({});
    if let Some(repo) = repo {
        match parse_repo(&repo) {
            Ok(repo) => args["repo"] = Value::String(repo.to_base32()),
            Err(err) => {
                eprintln!("swarm: {err}");
                return 2;
            }
        }
    }
    let result = match client.call("status", args).await {
        Ok(result) => result,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    if json_output {
        match serde_json::to_string(&result) {
            Ok(text) => println!("{text}"),
            Err(err) => {
                eprintln!("swarm: {err}");
                return 1;
            }
        }
        return 0;
    }
    if let Some(repos) = result.get("repos").and_then(Value::as_array) {
        for repo in repos {
            eprintln!(
                "{} seq={} refs={} peers={} seeding={}",
                repo.get("repo").and_then(Value::as_str).unwrap_or("?"),
                repo.get("seq").and_then(Value::as_u64).unwrap_or(0),
                repo.get("refs").and_then(Value::as_u64).unwrap_or(0),
                repo.get("peers").and_then(Value::as_u64).unwrap_or(0),
                repo.get("seeding")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            if let Some(conflicts) = repo.get("conflicts").and_then(Value::as_array) {
                for name in conflicts {
                    eprintln!("  conflict {}", name.as_str().unwrap_or("?"));
                }
            }
            if let Some(items) = repo.get("equivocations").and_then(Value::as_array) {
                for item in items {
                    eprintln!(
                        "  equivocation {} seq={}",
                        item.get("signer").and_then(Value::as_str).unwrap_or("?"),
                        item.get("seq").and_then(Value::as_u64).unwrap_or(0),
                    );
                }
            }
        }
    }
    0
}

fn key_backup(repo: &str, output: &Path) -> i32 {
    let repo = match parse_repo(repo) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    let source = home.repo_key_path(repo);
    if !source.is_file() {
        eprintln!("swarm: no signing key for this repo");
        return 1;
    }
    if let Err(err) = keyio::copy_private(&source, output) {
        eprintln!("swarm: {err}");
        return 1;
    }
    eprintln!("swarm: key written to {}", output.display());
    0
}

async fn daemon(foreground: bool, detach: bool, action: Option<DaemonCmd>) -> i32 {
    let home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    if let Some(DaemonCmd::Stop) = action {
        match Client::connect_existing(home).await {
            Ok(mut client) => {
                if let Err(err) = client.call("stop", json!({})).await {
                    eprintln!("swarm: {err}");
                    return client_code(&err);
                }
                0
            }
            Err(ClientError::Unreachable(_)) => {
                eprintln!("swarm: daemon is not running");
                0
            }
            Err(err) => {
                eprintln!("swarm: {err}");
                client_code(&err)
            }
        }
    } else if foreground || detach {
        match swarm_daemon::daemon::run(home).await {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("swarm: {err}");
                1
            }
        }
    } else {
        match Client::connect(home).await {
            Ok(_) => 0,
            Err(err) => {
                eprintln!("swarm: {err}");
                client_code(&err)
            }
        }
    }
}

/// Parse arguments and run one CLI command.
pub async fn run(args: Vec<String>) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return if err.use_stderr() { 2 } else { 0 };
        }
    };
    match cli.command {
        Cmd::Init {
            object_format,
            json,
        } => init(object_format, json).await,
        Cmd::Link { json, repo } => link(repo, json).await,
        Cmd::Status { json, repo } => status(json, repo).await,
        Cmd::Key {
            command: KeyCmd::Backup { repo, output },
        } => key_backup(&repo, &output),
        Cmd::Daemon {
            foreground,
            detach,
            action,
        } => daemon(foreground, detach, action).await,
    }
}
