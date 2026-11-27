//! `swarm mr`: merge requests.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use swarm_core::ids::{Link, RepoId};
use swarm_daemon::client::{Client, ClientError};
use swarm_store::Home;
use tokio::process::Command;

use super::args::MrCmd;
use super::{client_code, parse_link};
use crate::propose::{author_name, delivery, describe, propose, short, Proposal};

enum Failure {
    /// Say this and exit 1.
    Message(String),
    /// A command that has already explained itself failed.
    Code(i32),
    Daemon(ClientError),
}

impl From<ClientError> for Failure {
    fn from(err: ClientError) -> Self {
        Self::Daemon(err)
    }
}

impl From<std::io::Error> for Failure {
    fn from(err: std::io::Error) -> Self {
        Self::Message(err.to_string())
    }
}

fn say(message: impl Into<String>) -> Failure {
    Failure::Message(message.into())
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Peers write this text, so keep terminal escapes out of it.
fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
        .collect()
}

fn branch(name: &str) -> &str {
    name.trim_start_matches("refs/heads/")
}

fn age(ts: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    match now.saturating_sub(ts) {
        secs @ 0..=59 => format!("{secs}s ago"),
        secs @ 60..=3599 => format!("{}m ago", secs / 60),
        secs @ 3600..=86_399 => format!("{}h ago", secs / 3600),
        secs => format!("{}d ago", secs / 86_400),
    }
}

/// The merge request named, or else the one for the checked out branch.
async fn or_current(wanted: Option<String>) -> Result<String, Failure> {
    match wanted {
        Some(wanted) => Ok(wanted),
        None => git_output(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await
            .ok_or_else(|| say("not on a branch; name a merge request by branch or ID")),
    }
}

/// `name (role, key abcdefgh)`: names are chosen freely, keys are not.
fn who(author: &Value) -> String {
    format!(
        "{} ({}, {})",
        clean(text(author, "name")),
        text(author, "role"),
        short(text(author, "key")),
    )
}

/// Run git in the user's repository, showing its output.
async fn git(args: &[&str]) -> Result<(), Failure> {
    let status = Command::new("git").args(args).status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::Code(status.code().unwrap_or(1)))
    }
}

/// Run git in the user's repository and take what it prints.
async fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().await.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The swarm remotes of the current git repository, `swarm` and `origin` first.
async fn swarm_remotes() -> Vec<(String, RepoId, Vec<String>)> {
    let mut names: Vec<String> = git_output(&["remote"])
        .await
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    names.sort_by_key(|name| match name.as_str() {
        "swarm" => 0,
        "origin" => 1,
        _ => 2,
    });
    let mut found = Vec::new();
    for name in names {
        let Some(url) = git_output(&["remote", "get-url", &name]).await else {
            continue;
        };
        if Link::parse(&url).is_err() {
            continue;
        }
        if let Ok((repo, hints)) = parse_link(&url) {
            found.push((name, repo, hints));
        }
    }
    found
}

struct Context {
    client: Client,
    repo: RepoId,
    /// Name of the git remote for this repo, when run inside a clone.
    remote: Option<String>,
}

impl Context {
    /// Find the repo and, unless `offline`, bring it up to date first.
    async fn new(wanted: Option<&str>, offline: bool) -> Result<Self, Failure> {
        let remotes = swarm_remotes().await;
        let (repo, mut hints) = match wanted {
            Some(value) => parse_link(value).map_err(say)?,
            None => {
                let (_, repo, _) = remotes.first().ok_or_else(|| {
                    say("no swarm remote here; name the repo with -R <link or ID>")
                })?;
                (*repo, Vec::new())
            }
        };
        let matching = remotes.iter().find(|(_, id, _)| *id == repo);
        if let Some((_, _, more)) = matching {
            hints.extend(more.iter().cloned());
        }
        let remote = matching.map(|(name, _, _)| name.clone());
        let home = Home::resolve().map_err(|err| say(err.to_string()))?;
        let mut client = Client::connect(home).await?;
        if !offline {
            let args = json!({ "repo": repo.to_base32(), "hints": hints });
            if let Err(err) = client.call("sync", args).await {
                eprintln!("swarm: showing what is here; sync failed: {err}");
            }
        }
        Ok(Self {
            client,
            repo,
            remote,
        })
    }

    async fn call(&mut self, command: &str, mut args: Value) -> Result<Value, Failure> {
        args["repo"] = json!(self.repo.to_base32());
        Ok(self.client.call(command, args).await?)
    }

    async fn show(&mut self, id: &str) -> Result<Value, Failure> {
        self.call("mr_show", json!({ "mr": id })).await
    }

    async fn event(&mut self, mut args: Value) -> Result<(), Failure> {
        args["name"] = json!(author_name().await);
        let kind = text(&args, "kind").to_string();
        let reply = self.call("mr_event", args).await?;
        let done = match kind.as_str() {
            "comment" => "commented on",
            "approve" => "approved",
            "request-changes" => "requested changes on",
            "close" => "closed",
            _ => "reopened",
        };
        println!(
            "{done} merge request {}{}",
            short(text(&reply, "mr")),
            delivery(&reply)
        );
        Ok(())
    }

    /// Bring the proposed commits into the current git repository.
    async fn fetch(&mut self, merge_request: &Value) -> Result<(), Failure> {
        if git_output(&["rev-parse", "--git-dir"]).await.is_none() {
            return Err(say("run this inside a clone of the repo"));
        }
        git(&[
            "fetch",
            "--quiet",
            "--no-tags",
            text(merge_request, "store_path"),
            text(merge_request, "ref"),
        ])
        .await
    }

    async fn store(&mut self) -> Result<PathBuf, Failure> {
        let info = self.call("repo_info", json!({})).await?;
        Ok(PathBuf::from(text(&info, "store_path")))
    }
}

/// Ask for text in the user's git editor.
async fn edit() -> Result<String, Failure> {
    let editor = git_output(&["var", "GIT_EDITOR"])
        .await
        .ok_or_else(|| say("no editor; give the text with -m"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let path = std::env::temp_dir().join(format!("swarm-mr-{}-{stamp}.md", std::process::id()));
    swarm_store::keyio::write_private_new(&path, b"").map_err(|err| say(err.to_string()))?;
    let result = run_editor(&editor, &path).await;
    let written = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    result?;
    Ok(written?)
}

async fn run_editor(editor: &str, path: &Path) -> Result<(), Failure> {
    // The editor setting is a shell command line, as git treats it.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(editor)
        .arg(path)
        .status()
        .await?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| say("the editor failed"))
}

fn print_list(reply: &Value, all: bool) {
    let requests = list(reply, "merge_requests");
    let mut shown = 0;
    for item in requests
        .iter()
        .rev()
        .filter(|item| all || text(item, "state") == "open")
    {
        shown += 1;
        let mut notes = Vec::new();
        for (key, label) in [
            ("approvals", "approved"),
            ("changes_requested", "changes requested"),
            ("comments", "comments"),
        ] {
            if number(item, key) > 0 {
                notes.push(format!("{label}: {}", number(item, key)));
            }
        }
        println!(
            "{}  {:<6}  {}",
            short(text(item, "id")),
            text(item, "state"),
            clean(text(item, "title")),
        );
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("; {}", notes.join(", "))
        };
        let from = item
            .get("topic")
            .and_then(Value::as_str)
            .map(|topic| format!("{} ", clean(topic)))
            .unwrap_or_default();
        println!(
            "          {from}into {} by {}, updated {}{notes}",
            clean(branch(text(item, "target"))),
            who(&item["author"]),
            age(number(item, "updated")),
        );
    }
    if shown == 0 {
        if requests.is_empty() {
            println!("no merge requests");
        } else {
            println!("no open merge requests (see --all)");
        }
    }
}

fn indented(body: &str) {
    for line in body.lines() {
        println!("    {}", clean(line));
    }
}

fn print_show(item: &Value) {
    println!(
        "merge request {}  [{}]",
        short(text(item, "id")),
        text(item, "state")
    );
    println!("{}", clean(text(item, "title")));
    let from = item
        .get("topic")
        .and_then(Value::as_str)
        .map_or_else(|| "commits".to_string(), clean);
    println!(
        "{} wants to merge {from} into {}, opened {}",
        who(&item["author"]),
        clean(branch(text(item, "target"))),
        age(number(item, "opened")),
    );
    println!(
        "approvals: {}, changes requested: {}",
        number(item, "approvals"),
        number(item, "changes_requested")
    );
    if !text(item, "body").is_empty() {
        println!();
        indented(text(item, "body"));
    }
    if !list(item, "commits").is_empty() {
        println!();
        println!("commits:");
        for commit in list(item, "commits") {
            println!(
                "    {} {}",
                short(text(commit, "id")),
                clean(text(commit, "subject"))
            );
        }
    }
    let head = text(item, "head");
    // The opening event is the heading above.
    for event in list(item, "events")
        .iter()
        .filter(|event| text(event, "kind") != "open")
    {
        let what = match text(event, "kind") {
            "update" => format!("added commits up to {}", short(text(event, "head"))),
            "comment" => "commented".to_string(),
            "approve" => "approved".to_string(),
            "request-changes" => "requested changes".to_string(),
            "close" => "closed this".to_string(),
            _ => "reopened this".to_string(),
        };
        let mut place = String::new();
        if let Some(file) = event.get("file").and_then(Value::as_str) {
            place = format!(" on {}", clean(file));
            if number(event, "line") > 0 {
                place.push_str(&format!(":{}", number(event, "line")));
            }
        }
        if let Some(parent) = event.get("reply_to").and_then(Value::as_str) {
            place.push_str(&format!(" in reply to {}", short(parent)));
        }
        let commit = text(event, "commit");
        let outdated = if commit.is_empty() || commit == head {
            ""
        } else {
            " (outdated)"
        };
        println!();
        println!(
            "[{}] {} {what}{place}{outdated}, {}",
            short(text(event, "id")),
            who(&event["author"]),
            age(number(event, "ts")),
        );
        indented(text(event, "text"));
    }
    if number(item, "ignored") > 0 {
        println!();
        println!(
            "{} event(s) ignored: their author may not do that",
            number(item, "ignored")
        );
    }
}

async fn create(
    context: &mut Context,
    from: Option<&str>,
    base: Option<&str>,
    title: Option<&str>,
    message: Option<&str>,
) -> Result<(), Failure> {
    let src = from.unwrap_or("HEAD");
    if git_output(&["rev-parse", "--verify", &format!("{src}^{{commit}}")])
        .await
        .is_none()
    {
        return Err(say(match from {
            Some(branch) => format!("there is no branch {branch} here"),
            None => "run this inside a clone, on the branch to propose".to_string(),
        }));
    }
    let store = context.store().await?;
    let reply = propose(
        &mut context.client,
        Proposal {
            repo: context.repo,
            store: &store,
            src,
            target: base,
            title,
            body: message,
            mr: None,
        },
    )
    .await?;
    println!("{}", describe(&reply));
    Ok(())
}

async fn checkout(context: &mut Context, id: &str) -> Result<(), Failure> {
    let item = context.show(id).await?;
    context.fetch(&item).await?;
    let name = format!("mr/{}", short(text(&item, "id")));
    let head = text(&item, "head");
    let exists = git_output(&["rev-parse", "--verify", &format!("refs/heads/{name}")])
        .await
        .is_some();
    if exists {
        git(&["checkout", &name]).await?;
        git(&["merge", "--ff-only", head]).await
    } else {
        git(&["checkout", "-b", &name, head]).await
    }
}

async fn diff(context: &mut Context, id: &str) -> Result<(), Failure> {
    let item = context.show(id).await?;
    context.fetch(&item).await?;
    let base = item
        .get("base")
        .and_then(Value::as_str)
        .ok_or_else(|| say("this merge request names no base commit"))?;
    git(&["diff", &format!("{base}...{}", text(&item, "head"))]).await
}

async fn merge(context: &mut Context, id: &str, push: bool) -> Result<(), Failure> {
    let item = context.show(id).await?;
    if text(&item, "state") != "open" {
        return Err(say(format!("it is {}", text(&item, "state"))));
    }
    let info = context.call("repo_info", json!({})).await?;
    if info.get("has_key").and_then(Value::as_bool) != Some(true) {
        return Err(say("merging needs the repo's root or a maintainer key"));
    }
    let remote = context
        .remote
        .clone()
        .ok_or_else(|| say("run this inside a clone of the repo"))?;
    let target = branch(text(&item, "target")).to_string();
    let current = git_output(&["symbolic-ref", "--short", "HEAD"]).await;
    if current.as_deref() != Some(target.as_str()) {
        return Err(say(format!("check out {target} first")));
    }
    let changes = git_output(&["status", "--porcelain", "--untracked-files=no"]).await;
    if changes.is_none_or(|changes| !changes.is_empty()) {
        return Err(say("commit or stash your changes first"));
    }
    context.fetch(&item).await?;
    let published = text(&item, "target_tip");
    if git_output(&["merge-base", "--is-ancestor", published, "HEAD"])
        .await
        .is_none()
    {
        return Err(say(format!("run `git pull {remote} {target}` first")));
    }
    let mr = short(text(&item, "id"));
    let message = format!("Merge merge request {mr}: {}", clean(text(&item, "title")));
    if let Err(err) = git(&["merge", "--no-ff", "-m", &message, text(&item, "head")]).await {
        eprintln!("swarm: resolve the conflicts, commit, then `git push {remote} {target}`");
        return Err(err);
    }
    if push {
        git(&["push", &remote, &format!("HEAD:refs/heads/{target}")]).await?;
        println!("merged merge request {mr} into {target}");
    } else {
        println!("merged here; publish with `git push {remote} {target}`");
    }
    Ok(())
}

async fn run_command(wanted: Option<&str>, offline: bool, command: MrCmd) -> Result<(), Failure> {
    let mut context = Context::new(wanted, offline).await?;
    match command {
        MrCmd::List { all, json } => {
            let reply = context.call("mr_list", json!({})).await?;
            if json {
                println!("{reply}");
            } else {
                print_list(&reply, all);
            }
        }
        MrCmd::Show { mr, json } => {
            let reply = context.show(&or_current(mr).await?).await?;
            if json {
                println!("{reply}");
            } else {
                print_show(&reply);
            }
        }
        MrCmd::Create {
            branch,
            base,
            title,
            message,
        } => {
            create(
                &mut context,
                branch.as_deref(),
                base.as_deref(),
                title.as_deref(),
                message.as_deref(),
            )
            .await?;
        }
        MrCmd::Comment {
            mr,
            message,
            file,
            line,
            reply_to,
        } => {
            let mr = or_current(mr).await?;
            let message = match message {
                Some(message) => message,
                None => edit().await?,
            };
            if message.trim().is_empty() {
                return Err(say("empty comment; nothing written"));
            }
            context
                .event(json!({
                    "mr": mr, "kind": "comment", "text": message,
                    "file": file, "line": line, "reply_to": reply_to,
                }))
                .await?;
        }
        MrCmd::Review {
            mr,
            approve,
            request_changes: _,
            message,
        } => {
            let kind = if approve {
                "approve"
            } else {
                "request-changes"
            };
            let mr = or_current(mr).await?;
            context
                .event(json!({ "mr": mr, "kind": kind, "text": message }))
                .await?;
        }
        MrCmd::Close { mr, message } => {
            context
                .event(json!({ "mr": mr, "kind": "close", "text": message }))
                .await?;
        }
        MrCmd::Reopen { mr } => {
            context.event(json!({ "mr": mr, "kind": "reopen" })).await?;
        }
        MrCmd::Checkout { mr } => checkout(&mut context, &mr).await?,
        MrCmd::Diff { mr } => diff(&mut context, &or_current(mr).await?).await?,
        MrCmd::Merge { mr, no_push } => merge(&mut context, &mr, !no_push).await?,
    }
    Ok(())
}

pub(super) async fn mr(wanted: Option<String>, offline: bool, command: MrCmd) -> i32 {
    match run_command(wanted.as_deref(), offline, command).await {
        Ok(()) => 0,
        Err(Failure::Message(message)) => {
            eprintln!("swarm: {message}");
            1
        }
        Err(Failure::Code(code)) => code,
        Err(Failure::Daemon(err)) => {
            eprintln!("swarm: {err}");
            client_code(&err)
        }
    }
}
