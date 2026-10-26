//! `git-remote-swarm`: the git remote helper (spec section 8.3).
//!
//! The helper never opens a socket of its own. It talks to the daemon over the
//! local IPC socket, reads refs from the daemon's store, and moves objects with
//! the `git` binary. All human output goes to stderr.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};
use swarm_core::ids::RepoId;
use swarm_daemon::client::{Client, ClientError};
use swarm_daemon::repo as daemon_repo;
use swarm_store::git;
use swarm_store::Home;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Cached answer from the daemon's `sync` command.
#[derive(Clone)]
struct Listing {
    store_path: PathBuf,
    object_format: Option<String>,
    refs: BTreeMap<String, String>,
    head: Option<String>,
}

struct Helper {
    client: Client,
    repo: RepoId,
    verbosity: u64,
}

fn repo_from_url(url: &str) -> Result<RepoId, String> {
    if url.starts_with("swarm://") {
        swarm_core::ids::Link::parse(url)
            .map(|link| link.repo)
            .map_err(|err| err.to_string())
    } else {
        daemon_repo::parse_repo(url).map_err(|err| err.msg)
    }
}

fn fail(code: i32) -> ! {
    std::process::exit(code)
}

async fn write_line(text: &str) {
    let mut stdout = tokio::io::stdout();
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(b'\n');
    let _ = stdout.write_all(&bytes).await;
    let _ = stdout.flush().await;
}

impl Helper {
    fn new(client: Client, repo: RepoId) -> Self {
        Self {
            client,
            repo,
            verbosity: 1,
        }
    }

    fn note(&self, level: u64, message: impl AsRef<str>) {
        if self.verbosity >= level {
            eprintln!("swarm: {}", message.as_ref());
        }
    }

    async fn sync(&mut self) -> Result<Listing, ClientError> {
        let value = self
            .client
            .call("sync", json!({ "repo": self.repo.to_base32() }))
            .await?;
        let store_path = value
            .get("store_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| ClientError::Protocol("sync reply is missing store_path".into()))?;
        let object_format = value
            .get("object_format")
            .and_then(Value::as_str)
            .map(str::to_string);
        let head = value
            .get("head")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut refs = BTreeMap::new();
        if let Some(map) = value
            .get("ref_list")
            .or_else(|| value.get("refs").filter(|value| value.is_object()))
            .and_then(Value::as_object)
        {
            for (name, oid) in map {
                if let Some(oid) = oid.as_str() {
                    refs.insert(name.clone(), oid.to_string());
                }
            }
        }
        Ok(Listing {
            store_path,
            object_format,
            refs,
            head,
        })
    }

    async fn list(&mut self) -> Result<(), ClientError> {
        let listing = self.sync().await?;
        let mut text = String::new();
        if let Some(format) = &listing.object_format {
            text.push_str(&format!(":object-format {format}\n"));
        }
        for (name, oid) in &listing.refs {
            text.push_str(&format!("{oid} {name}\n"));
        }
        if let Some(head) = &listing.head {
            text.push_str(&format!("@{head} HEAD\n"));
        }
        let body = text.trim_end_matches('\n');
        if !body.is_empty() {
            write_line(body).await;
        }
        write_line("").await;
        Ok(())
    }

    async fn fetch(&mut self, oids: &[String]) -> Result<(), ClientError> {
        if oids.is_empty() {
            write_line("").await;
            return Ok(());
        }
        let listing = self.sync().await?;
        let store = listing.store_path.clone();
        match git::fetch_oids_inherited(&store, oids).await {
            Ok(()) => {
                write_line("").await;
                Ok(())
            }
            Err(err) => {
                self.note(0, format!("fetch failed: {err}"));
                write_line("").await;
                Err(ClientError::Unreachable(err.to_string()))
            }
        }
    }

    async fn has_key_and_store(&mut self) -> Result<(bool, PathBuf), ClientError> {
        let value = self
            .client
            .call("repo_info", json!({ "repo": self.repo.to_base32() }))
            .await?;
        let has_key = value
            .get("has_key")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let store = value
            .get("store_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| ClientError::Protocol("repo_info has no store_path".into()))?;
        Ok((has_key, store))
    }

    async fn push(&mut self, specs: &[String]) -> Result<(), ClientError> {
        if specs.is_empty() {
            write_line("").await;
            return Ok(());
        }
        let (has_key, store) = self.has_key_and_store().await?;
        if !has_key {
            for spec in specs {
                let dst = destination(spec);
                write_line(&format!("error {dst} no signing key (use: git swarm fork)")).await;
            }
            write_line("").await;
            return Ok(());
        }

        let mut results: Vec<(String, Result<(), String>)> = Vec::new();
        for spec in specs {
            let (force, body) = match spec.strip_prefix('+') {
                Some(body) => (true, body),
                None => (false, spec.as_str()),
            };
            let dst = destination(body);
            let refspec = if body.contains(':') {
                body.to_string()
            } else {
                format!("{body}:{body}")
            };
            match git::push_ref_inherited(&store, &refspec, force).await {
                Ok(()) => results.push((dst, Ok(()))),
                Err(err) => results.push((dst, Err(err.to_string()))),
            }
        }

        let all_ok = results.iter().all(|(_, result)| result.is_ok());
        let mut publish_error = None;
        if all_ok {
            if let Err(err) = self
                .client
                .call("publish", json!({ "repo": self.repo.to_base32() }))
                .await
            {
                publish_error = Some(match &err {
                    ClientError::Remote(remote) if remote.code == "no_signing_key" => {
                        "no signing key (use: git swarm fork)".to_string()
                    }
                    other => other.to_string(),
                });
            }
        }

        for (dst, result) in results {
            match (result, &publish_error) {
                (Ok(()), None) => write_line(&format!("ok {dst}")).await,
                (Ok(()), Some(err)) => {
                    write_line(&format!("error {dst} {err}")).await;
                }
                (Err(err), _) => {
                    let first = err.lines().next().unwrap_or("push failed");
                    write_line(&format!("error {dst} {first}")).await;
                }
            }
        }
        write_line("").await;
        Ok(())
    }
}

fn destination(spec: &str) -> String {
    let body = spec.strip_prefix('+').unwrap_or(spec);
    match body.split_once(':') {
        Some((_src, dst)) if !dst.is_empty() => dst.to_string(),
        Some((src, _)) => src.to_string(),
        None => body.to_string(),
    }
}

/// Run the helper. Returns the process exit code.
pub async fn run(args: Vec<String>) -> i32 {
    let url = match args.get(2).or_else(|| args.get(1)) {
        Some(url) => url.clone(),
        None => {
            eprintln!("swarm: git-remote-swarm needs a remote URL");
            return 2;
        }
    };
    let repo = match repo_from_url(&url) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("swarm: bad swarm URL {url}: {err}");
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
    let client = match Client::connect(home).await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("swarm: {err}");
            return 1;
        }
    };
    let mut helper = Helper::new(client, repo);
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let mut fetches: Vec<String> = Vec::new();
    let mut pushes: Vec<String> = Vec::new();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                eprintln!("swarm: helper stdin: {err}");
                return 1;
            }
        };
        if line.is_empty() {
            if !fetches.is_empty() {
                let batch = std::mem::take(&mut fetches);
                if helper.fetch(&batch).await.is_err() {
                    return 1;
                }
                continue;
            }
            if !pushes.is_empty() {
                let batch = std::mem::take(&mut pushes);
                if let Err(err) = helper.push(&batch).await {
                    eprintln!("swarm: {err}");
                    return 1;
                }
                continue;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("option ") {
            let (name, value) = rest.split_once(' ').unwrap_or((rest, ""));
            if name == "verbosity" {
                helper.verbosity = value.parse().unwrap_or(1);
            }
            write_line("ok").await;
            continue;
        }
        if line == "capabilities" {
            write_line("option").await;
            write_line("fetch").await;
            write_line("push").await;
            write_line("object-format").await;
            write_line("").await;
            continue;
        }
        if line == "list" || line.starts_with("list ") {
            if let Err(err) = helper.list().await {
                eprintln!("swarm: {err}");
                fail(1);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("fetch ") {
            if let Some(oid) = rest.split_whitespace().next() {
                fetches.push(oid.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("push ") {
            pushes.push(rest.to_string());
            continue;
        }
        eprintln!("swarm: helper ignoring unknown command {line:?}");
    }
    0
}
