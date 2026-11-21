//! Command-line interface (spec section 9.1).

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use swarm_core::ids::{Link, RepoId};
use swarm_daemon::client::ClientError;
use swarm_daemon::{repo as daemon_repo, Client};
use swarm_store::git::{self, Git};
use swarm_store::{keyio, Home};

mod repo;
use repo::{
    accept, export, gc, import, key_cmd, maintainers_cmd, recovery_cmd, resolve_repo, rotate_cmd,
    unseed,
};

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
    /// Sync a repo and keep serving it.
    Seed {
        /// A `swarm://` link, optionally with hints.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Stop serving a repo but keep its store.
    Unseed {
        /// A repo ID or link.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Repack a repo's store and drop unreachable objects, when idle (ST-7).
    Gc {
        /// A repo ID or link.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Create an independent repo from another one.
    Fork {
        /// The source link, optionally with hints.
        link: String,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Write a repo and its signed state to a bundle file for offline transfer.
    Export {
        /// A repo ID or link.
        link: String,
        /// Bundle file to write.
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Read a bundle file, making its repo available offline.
    Import {
        /// Bundle file to read.
        file: PathBuf,
        /// Print one JSON object on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Add or remove maintainers, who may publish without the root key.
    Maintainers {
        #[command(subcommand)]
        command: MaintainersCmd,
    },
    /// Settle a ref conflict here by taking one signer's value.
    Accept {
        /// A repo ID or link.
        repo: String,
        /// The ref in conflict, for example refs/heads/main.
        refname: String,
        /// The signer (key ID, hex or base32) whose value to take.
        signer: String,
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
enum MaintainersCmd {
    /// Authorize a key (root key only), optionally limited to ref globs.
    Add {
        repo: String,
        /// The maintainer's key ID, from `swarm key new` or `swarm key id`.
        key: String,
        /// Ref globs the key may publish (default refs/heads/*).
        globs: Vec<String>,
    },
    /// Remove a maintainer (root key only).
    Remove { repo: String, key: String },
    /// List the maintainers.
    List { repo: String },
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Create a recovery key, name it in the repo, and save it to FILE (keep it offline).
    Recovery {
        repo: String,
        #[arg(short = 'o', long = "output", value_name = "FILE")]
        output: PathBuf,
    },
    /// Move the root to a new key, authorized by the recovery key file.
    Rotate {
        repo: String,
        #[arg(long, value_name = "FILE")]
        recovery: PathBuf,
    },
    /// Create this machine's own key for a repo, to be authorized by the root.
    New { repo: String },
    /// Print the key ID of this machine's key for a repo.
    Id { repo: String },
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
        "auth_failed"
        | "pq_handshake"
        | "pack_invalid"
        | "too_large"
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
        println!("{}", shell_safe(link));
    }
    0
}

/// A link with several hints contains `&`, which a shell treats as "run in the
/// background" and silently cuts the link at the first one. When a person is
/// reading the terminal, print it single-quoted so it can be pasted as is.
/// Piped output stays bare so scripts keep working.
fn shell_safe(link: &str) -> String {
    use std::io::IsTerminal;
    if link.contains('&') && std::io::stdout().is_terminal() {
        format!("'{link}'")
    } else {
        link.to_string()
    }
}

async fn link(repo: Option<String>, json_output: bool) -> i32 {
    let repo = match resolve_repo(repo).await {
        Ok(repo) => repo,
        Err(code) => return code,
    };
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
    let hello = match client.call("hello", json!({})).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    let port = hello.get("port").and_then(Value::as_u64).unwrap_or(0);
    let listen = hello
        .get("listen")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<std::net::SocketAddr>().ok());
    let node = hello
        .get("node_id")
        .and_then(Value::as_str)
        .and_then(|value| swarm_core::ids::NodeId::from_base32(value).ok());
    let link = match (listen, node) {
        (Some(listen), Some(node)) if port != 0 => {
            // REL-8: relay hints come from the daemon, which knows its registrations.
            let mut link = swarm_daemon::net::link_with_hint(repo, listen, node, None).to_string();
            let relays = hello.get("relay_hints").and_then(Value::as_array);
            for hint in relays.into_iter().flatten().filter_map(Value::as_str) {
                link.push('&');
                link.push_str(hint);
            }
            link
        }
        _ => format!("swarm://{}", repo.to_base32()),
    };
    if json_output {
        println!("{}", json!({ "link": link }));
    } else {
        println!("{}", shell_safe(&link));
    }
    0
}

fn parse_link(value: &str) -> Result<(RepoId, Vec<String>), String> {
    if value.starts_with("swarm://") {
        let link = Link::parse(value).map_err(|err| err.to_string())?;
        let hints = link
            .hints
            .iter()
            .map(|hint| hint.to_string().trim_start_matches("h=").to_string())
            .collect();
        Ok((link.repo, hints))
    } else {
        Ok((parse_repo(value)?, Vec::new()))
    }
}

async fn with_client<F>(action: F) -> i32
where
    F: std::future::Future<Output = Result<i32, ClientError>>,
{
    let _home = match Home::resolve() {
        Ok(home) => home,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    match action.await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("swarm: {err}");
            client_code(&err)
        }
    }
}

async fn seed(link: &str, json_output: bool) -> i32 {
    let (repo, hints) = match parse_link(link) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 2;
        }
    };
    with_client(async move {
        let home = Home::resolve().map_err(|err| ClientError::Unreachable(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        let synced = client
            .call("sync", json!({ "repo": repo.to_base32(), "hints": hints }))
            .await?;
        let seeded = client
            .call("seed", json!({ "repo": repo.to_base32() }))
            .await?;
        if json_output {
            println!(
                "{}",
                json!({ "repo": repo.to_base32(), "sync": synced, "seed": seeded })
            );
        } else {
            println!("swarm://{}", repo.to_base32());
        }
        Ok(0)
    })
    .await
}

async fn fork(link: &str, json_output: bool) -> i32 {
    let (repo, hints) = match parse_link(link) {
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
            .call("fork", json!({ "repo": repo.to_base32(), "hints": hints }))
            .await?;
        if json_output {
            println!("{}", result);
        } else {
            let link = result
                .get("link")
                .and_then(Value::as_str)
                .unwrap_or_default();
            println!("{}", shell_safe(link));
        }
        Ok(0)
    })
    .await
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
    if let Some(relays) = result.get("relays").and_then(Value::as_array) {
        for relay in relays {
            let name = relay.get("relay").and_then(Value::as_str).unwrap_or("?");
            if relay
                .get("registered")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                eprintln!("relay {name} registered");
            } else {
                eprintln!(
                    "relay {name} not registered: {}",
                    relay
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("connecting"),
                );
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
        Cmd::Seed { link, json } => seed(&link, json).await,
        Cmd::Unseed { link, json } => unseed(&link, json).await,
        Cmd::Gc { link, json } => gc(&link, json).await,
        Cmd::Fork { link, json } => fork(&link, json).await,
        Cmd::Export { link, output, json } => export(&link, &output, json).await,
        Cmd::Import { file, json } => import(&file, json).await,
        Cmd::Key {
            command: KeyCmd::Backup { repo, output },
        } => key_backup(&repo, &output),
        Cmd::Key {
            command: KeyCmd::Recovery { repo, output },
        } => recovery_cmd(&repo, &output).await,
        Cmd::Key {
            command: KeyCmd::Rotate { repo, recovery },
        } => rotate_cmd(&repo, &recovery).await,
        Cmd::Key {
            command: KeyCmd::New { repo },
        } => key_cmd("key_new", &repo).await,
        Cmd::Key {
            command: KeyCmd::Id { repo },
        } => key_cmd("key_id", &repo).await,
        Cmd::Maintainers { command } => match command {
            MaintainersCmd::Add { repo, key, globs } => {
                maintainers_cmd("add", &repo, Some(&key), globs).await
            }
            MaintainersCmd::Remove { repo, key } => {
                maintainers_cmd("remove", &repo, Some(&key), Vec::new()).await
            }
            MaintainersCmd::List { repo } => maintainers_cmd("list", &repo, None, Vec::new()).await,
        },
        Cmd::Accept {
            repo,
            refname,
            signer,
        } => accept(&repo, &refname, &signer).await,
        Cmd::Daemon {
            foreground,
            detach,
            action,
        } => daemon(foreground, detach, action).await,
    }
}
