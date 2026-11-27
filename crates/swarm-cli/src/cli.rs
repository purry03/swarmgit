//! Command-line interface (spec section 9.1).

use std::path::Path;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use serde_json::{json, Value};
use swarm_core::ids::{Link, NodeId, RepoId};
use swarm_daemon::client::ClientError;
use swarm_daemon::{repo as daemon_repo, Client};
use swarm_disc::dht::{DhtConfig, MainlineDht};
use swarm_relay::{Directory, DirectoryConfig};
use swarm_store::git::{self, Git};
use swarm_store::{keyio, Home};

mod args;
mod daemon;
mod mr;
mod repo;
mod status;
use args::{Cli, Cmd, KeyCmd, MaintainersCmd, MirrorCmd};
use daemon::daemon;
use mr::mr;
use repo::{
    accept, export, gc, import, key_cmd, maintainers_cmd, mirror_export, purge, recovery_cmd,
    resolve_repo, rotate_cmd, unseed,
};
use status::status;

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
    // The daemon builds the link: it knows its proxy, onion service and
    // relay registrations.
    let hello = match client
        .call("hello", json!({ "repo": repo.to_base32() }))
        .await
    {
        Ok(value) => value,
        Err(err) => {
            eprintln!("swarm: {err}");
            return client_code(&err);
        }
    };
    let link = match hello.get("link").and_then(Value::as_str) {
        Some(link) => link.to_string(),
        None => format!("swarm://{}", repo.to_base32()),
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

async fn inspect(link: &str, json_output: bool, show_progress: bool) -> i32 {
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
        let mut on_event = |event: &Value| {
            if show_progress {
                use std::io::Write as _;
                let line = json!({ "type": "progress", "data": event });
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            }
        };
        let result = client
            .call_with_events(
                "inspect",
                json!({
                    "repo": repo.to_base32(),
                    "hints": hints,
                    "progress": show_progress,
                }),
                &mut on_event,
            )
            .await?;
        if json_output {
            if show_progress {
                println!("{}", json!({ "type": "result", "data": result }));
            } else {
                println!("{result}");
            }
        } else {
            print_about(&result);
            print_lineage(&result);
            print_names(&result);
            println!(
                "{} seq={} refs={} peers={}",
                result.get("repoId").and_then(Value::as_str).unwrap_or("?"),
                result.get("sequence").and_then(Value::as_u64).unwrap_or(0),
                result
                    .get("refs")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
                result
                    .get("peers")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
            );
        }
        Ok(0)
    })
    .await
}

fn print_about(result: &Value) {
    for key in ["name", "description"] {
        if let Some(text) = result.get(key).and_then(Value::as_str) {
            println!("{key}: {text}");
        }
    }
}

/// The repos on either side of a fork, when known.
fn print_lineage(result: &Value) {
    let line = |label: &str, repo: &Value| {
        let Some(link) = repo.get("link").and_then(Value::as_str) else {
            return;
        };
        match repo.get("name").and_then(Value::as_str) {
            Some(name) => println!("{label}: {link} ({name})"),
            None => println!("{label}: {link}"),
        }
    };
    if let Some(upstream) = result.get("upstream") {
        line("forked from", upstream);
    }
    for fork in result
        .get("downstream")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line("fork", fork);
    }
}

/// Branch and tag names, one per line, the default branch marked.
fn print_names(result: &Value) {
    let head = result
        .get("head")
        .and_then(Value::as_str)
        .and_then(|head| head.strip_prefix("refs/heads/"));
    for (key, label) in [("branches", "branches"), ("tags", "tags")] {
        let names: Vec<&str> = result
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if names.is_empty() {
            continue;
        }
        println!("{label}:");
        for name in names {
            let default = key == "branches" && Some(name) == head;
            println!("  {name}{}", if default { " (default)" } else { "" });
        }
    }
}

async fn about_cmd(
    repo: &str,
    name: Option<String>,
    description: Option<String>,
    json_output: bool,
) -> i32 {
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
            .call(
                "about",
                json!({ "repo": repo.to_base32(), "name": name, "description": description }),
            )
            .await?;
        if json_output {
            println!("{result}");
        } else {
            print_about(&result);
        }
        Ok(0)
    })
    .await
}

async fn discover_relays(json_output: bool) -> i32 {
    if swarm_disc::dht::disabled() {
        if json_output {
            println!("{}", json!({ "relays": [] }));
        } else {
            eprintln!("swarm: relay discovery is disabled by SWARM_NO_DHT=1");
        }
        return 0;
    }
    let dht = match MainlineDht::start(&DhtConfig::from_env(Vec::new())) {
        Ok(dht) => dht,
        Err(err) => {
            eprintln!("swarm: cannot start DHT relay discovery: {err}");
            return 1;
        }
    };
    let directory = Directory::new(Some(dht), None, DirectoryConfig::default());
    directory.refresh(&[]).await;
    let relays: Vec<_> = directory
        .ranked()
        .into_iter()
        .take(32)
        .map(|relay| {
            json!({
                "address": relay.addr.to_string(),
                "name": relay.name,
                "relayId": NodeId::from_bytes(relay.relay_id).to_base32(),
                "rttMs": relay.rtt_ms,
                "successes": relay.successes,
                "failures": relay.failures,
            })
        })
        .collect();
    if json_output {
        println!("{}", json!({ "relays": relays }));
    } else if relays.is_empty() {
        println!("No public relays found.");
    } else {
        for relay in relays {
            let address = relay["address"].as_str().unwrap_or("?");
            let shown = relay["name"]
                .as_str()
                .map_or_else(|| address.to_string(), |name| format!("{name} ({address})"));
            println!(
                "{shown} relay={} rtt={}ms",
                relay["relayId"].as_str().unwrap_or("?"),
                relay["rttMs"].as_u64().unwrap_or(0),
            );
        }
    }
    0
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

/// The completion script for `shell`, as shipped in `completions/`.
#[must_use]
pub fn completions(shell: Shell) -> String {
    let mut out = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "swarm", &mut out);
    String::from_utf8_lossy(&out).into_owned()
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
        Cmd::Completions { shell } => {
            print!("{}", completions(shell));
            0
        }
        Cmd::Link { json, repo } => link(repo, json).await,
        Cmd::Status { json, repo, view } => status(json, repo, view).await,
        Cmd::Inspect {
            link,
            json,
            progress,
        } => inspect(&link, json, progress).await,
        Cmd::About {
            repo,
            name,
            description,
            json,
        } => about_cmd(&repo, name, description, json).await,
        Cmd::DiscoverRelays { json } => discover_relays(json).await,
        Cmd::Seed { link, json } => seed(&link, json).await,
        Cmd::Unseed { link, json } => unseed(&link, json).await,
        Cmd::Gc { link, json } => gc(&link, json).await,
        Cmd::Purge {
            repo,
            yes,
            delete_key,
            json,
        } => purge(&repo, yes, delete_key, json).await,
        Cmd::Relay(args) => match swarm_relay::cli::run(args).await {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("swarm: {err:#}");
                1
            }
        },
        Cmd::Fork { link, json } => fork(&link, json).await,
        Cmd::Mr {
            repo,
            offline,
            command,
        } => mr(repo, offline, command).await,
        Cmd::Mirror {
            command: MirrorCmd::Export { link, dir },
        } => mirror_export(&link, &dir).await,
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
            experimental_udp,
            action,
        } => {
            // A daemon started from here inherits it; a running one keeps
            // its settings until restarted.
            if experimental_udp {
                std::env::set_var("SWARM_EXPERIMENTAL_UDP", "1");
            }
            daemon(foreground || detach, experimental_udp, action).await
        }
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    /// `completions/` holds the scripts the packages install; regenerate them
    /// with `scripts/gen-completions.sh` when the command line changes.
    #[test]
    fn shipped_completions_are_current() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../completions");
        for (file, shell) in [
            ("swarm.bash", Shell::Bash),
            ("_swarm", Shell::Zsh),
            ("swarm.fish", Shell::Fish),
        ] {
            let have = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
            assert!(
                have == completions(shell),
                "completions/{file} is stale: run scripts/gen-completions.sh"
            );
        }
    }
}
