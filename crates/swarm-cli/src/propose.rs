//! Offering a commit from the current git repository as a merge request. Used
//! by `git push <remote> HEAD:refs/for/<branch>` and `swarm mr create`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use swarm_core::ids::RepoId;
use swarm_daemon::client::{Client, ClientError};
use swarm_store::git;
use tokio::process::Command;

/// What to propose.
pub(crate) struct Proposal<'a> {
    pub(crate) repo: RepoId,
    /// The daemon's store for the repo.
    pub(crate) store: &'a Path,
    /// Commit to propose, as the local repository names it.
    pub(crate) src: &'a str,
    /// Branch to merge into; the repo's default when absent.
    pub(crate) target: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
    pub(crate) body: Option<&'a str>,
    /// Merge request to update, when the caller names one.
    pub(crate) mr: Option<&'a str>,
}

/// One line of output from git run in the caller's repository, if it succeeds
/// and says something.
async fn local(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().await.ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

/// The name to sign events with. The e-mail address is never published.
pub(crate) async fn author_name() -> String {
    local(&["config", "user.name"]).await.unwrap_or_default()
}

/// Copy the commit into the store and have the daemon sign the request.
pub(crate) async fn propose(
    client: &mut Client,
    proposal: Proposal<'_>,
) -> Result<Value, ClientError> {
    // The user's branch name lets a second push update the same request.
    let topic = local(&["rev-parse", "--symbolic-full-name", proposal.src])
        .await
        .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_string));
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let temp = format!("refs/swarm/mr-tmp/{}-{stamp}", std::process::id());
    git::push_ref_inherited(proposal.store, &format!("{}:{temp}", proposal.src), false)
        .await
        .map_err(|err| ClientError::Protocol(err.to_string()))?;
    let reply = client
        .call(
            "mr_open",
            json!({
                "repo": proposal.repo.to_base32(),
                "head": temp,
                "target": proposal.target,
                "title": proposal.title,
                "body": proposal.body,
                "mr": proposal.mr,
                "topic": topic,
                "name": author_name().await,
            }),
        )
        .await;
    let _ = git::Git::bare(proposal.store)
        .output(&["update-ref", "-d", &temp])
        .await;
    reply
}

/// What happened, for a person.
pub(crate) fn describe(reply: &Value) -> String {
    let text = |key: &str| reply.get(key).and_then(Value::as_str).unwrap_or("?");
    let id = short(text("mr"));
    let branch = text("target").trim_start_matches("refs/heads/");
    match text("action") {
        "unchanged" => format!("merge request {id} already proposes that commit"),
        action => format!(
            "{action} merge request {id} into {branch}{}",
            delivery(reply)
        ),
    }
}

/// Where a new event has got to.
pub(crate) fn delivery(reply: &Value) -> String {
    match reply.get("peers").and_then(Value::as_u64).unwrap_or(0) {
        0 => " (saved here; it is sent when a peer is reachable)".to_string(),
        1 => " (1 peer has it)".to_string(),
        count => format!(" ({count} peers have it)"),
    }
}

/// The short form of a merge request or event ID.
pub(crate) fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
