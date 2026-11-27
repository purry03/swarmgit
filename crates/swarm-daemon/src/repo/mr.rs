//! Merge requests: writing signed events into the store and reading the
//! threads they make. See `swarm_core::mr` for the format.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use swarm_core::accept::AcceptState;
use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::key_id;
use swarm_core::ids::{KeyId, RepoId};
use swarm_core::keyfile::RepoKey;
use swarm_core::mr::{clean_text, is_oid, mr_ref, Event, Kind, Thread, VerifiedEvent};
use swarm_core::pq::Signer;
use swarm_net::message::MrTip;
use swarm_store::{keyio, RepoStore};

use super::{arg_str, db_repo, now, parse_repo};
use crate::net::NetState;
use crate::protocol::IpcError;
use crate::state::DaemonState;
use crate::sync::mrs;
use crate::sync::progress::Progress;

/// Shortest merge request or event ID prefix accepted.
const MIN_PREFIX: usize = 4;
/// Longest description made from commit messages.
const MAX_MADE_BODY: usize = 8 * 1024;
/// How long to look for a peer to take a new event.
const DELIVER_SYNC: Duration = Duration::from_secs(20);
/// How long to wait for peers to confirm they took it.
const DELIVER_CONFIRM: Duration = Duration::from_secs(5);

fn bad(message: impl Into<String>) -> IpcError {
    IpcError::new("malformed", message)
}

fn optional<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// The keys this machine may sign events with.
struct Keys {
    /// The repo key, when it is the root's or a maintainer's.
    manager: Option<Box<dyn Signer>>,
    /// This machine's own key, once it has written anything.
    identity: Option<Box<dyn Signer>>,
}

impl Keys {
    fn id(signer: &dyn Signer) -> KeyId {
        key_id(&signer.bundle())
    }

    fn mine(&self, key: KeyId) -> bool {
        self.manager
            .iter()
            .chain(&self.identity)
            .any(|signer| Self::id(&**signer) == key)
    }

    fn signing(&self, key: KeyId) -> Option<&dyn Signer> {
        self.manager
            .iter()
            .chain(&self.identity)
            .map(|signer| &**signer)
            .find(|signer| Self::id(*signer) == key)
    }
}

/// Everything needed to read or write one repo's merge requests.
struct Repo {
    id: RepoId,
    store: RepoStore,
    accepted: AcceptState,
    canonical: BTreeMap<String, String>,
    head: Option<String>,
    events: Vec<VerifiedEvent>,
    threads: Vec<Thread>,
}

impl Repo {
    async fn open(state: &DaemonState, args: &Value) -> Result<Self, IpcError> {
        let id = parse_repo(arg_str(args, "repo")?)?;
        db_repo(state, id)?;
        let store = RepoStore::new(state.home.clone(), id);
        let accepted = store.load_state()?;
        let resolved = store.resolve(&accepted);
        let canonical = resolved
            .canonical
            .iter()
            .map(|(name, oid)| (name.clone(), oid.to_hex()))
            .collect();
        let events = store.mr_events().await?;
        let mut by_mr: BTreeMap<&str, Vec<VerifiedEvent>> = BTreeMap::new();
        for item in &events {
            let mr = item.event.mr.as_deref().unwrap_or(&item.id);
            by_mr.entry(mr).or_default().push(item.clone());
        }
        let mut threads: Vec<Thread> = by_mr
            .into_iter()
            .filter_map(|(mr, list)| Thread::build(mr, list, |key| accepted.is_authorized(key)))
            .collect();
        threads.sort_by(|a, b| (a.opened, &a.id).cmp(&(b.opened, &b.id)));
        Ok(Self {
            id,
            store,
            head: resolved.head,
            accepted,
            canonical,
            events,
            threads,
        })
    }

    fn keys(&self, state: &DaemonState) -> Result<Keys, IpcError> {
        let signer = |key: RepoKey| {
            key.signer()
                .map_err(|err| IpcError::new("bad_keys", err.to_string()))
        };
        let manager = match self.store.load_key()? {
            Some(key) => Some(signer(key)?).filter(|s| self.accepted.is_authorized(Keys::id(&**s))),
            None => None,
        };
        let identity = match keyio::read_private(&state.home.identity_key_path())? {
            Some(bytes) => {
                Some(signer(RepoKey::from_bytes(&bytes).map_err(|err| {
                    IpcError::new("bad_keys", err.to_string())
                })?)?)
            }
            None => None,
        };
        Ok(Keys { manager, identity })
    }

    /// The keys, creating this machine's own one if it will be needed to sign.
    fn signing_keys(&self, state: &DaemonState) -> Result<Keys, IpcError> {
        let keys = self.keys(state)?;
        if keys.manager.is_some() || keys.identity.is_some() {
            return Ok(keys);
        }
        let key = RepoKey::generate(SUITE_HYBRID)
            .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
        keyio::write_private_new(&state.home.identity_key_path(), &key.to_bytes())?;
        self.keys(state)
    }

    fn role(&self, key: KeyId) -> &'static str {
        if key == self.accepted.root {
            "owner"
        } else if self.accepted.is_authorized(key) {
            "maintainer"
        } else {
            "contributor"
        }
    }

    fn author(&self, key: KeyId, name: &str) -> Value {
        json!({ "key": key.to_base32(), "name": name, "role": self.role(key) })
    }

    async fn is_ancestor(&self, older: &str, newer: &str) -> bool {
        self.store
            .git()
            .output(&["merge-base", "--is-ancestor", older, newer])
            .await
            .is_ok()
    }

    /// `merged`, `closed` or `open`. Merged is not recorded anywhere: it is
    /// true once the target branch holds the proposed commit.
    async fn status(&self, thread: &Thread) -> &'static str {
        if let Some(tip) = self.canonical.get(&thread.target) {
            if self.is_ancestor(&thread.head, tip).await {
                return "merged";
            }
        }
        if thread.closed {
            "closed"
        } else {
            "open"
        }
    }

    async fn summary(&self, thread: &Thread, keys: &Keys) -> Value {
        let reviews = thread.reviews();
        // A review of an older head is shown but no longer counts.
        let counted = |kind: Kind| {
            reviews
                .iter()
                .filter(|r| r.event.kind == kind && r.event.commit.as_ref() == Some(&thread.head))
                .count()
        };
        json!({
            "id": thread.id,
            "title": thread.title,
            "author": self.author(thread.author, &thread.author_name),
            "target": thread.target,
            "topic": thread.topic,
            "state": self.status(thread).await,
            "head": thread.head,
            "base": thread.base,
            // The author's ref, whose history holds `head` and `base`.
            "ref": mr_ref(&thread.id, thread.author),
            "opened": thread.opened,
            "updated": thread.updated,
            "comments": thread.events.iter().filter(|e| e.event.kind == Kind::Comment).count(),
            "approvals": counted(Kind::Approve),
            "changes_requested": counted(Kind::RequestChanges),
            "mine": keys.mine(thread.author),
        })
    }

    /// The merge request a person means: by the branch it proposes (`mr/<id>`
    /// for a checked out one), else by the start of its ID. Among several for
    /// one branch name, one's own and then the open one is meant.
    async fn find(&self, wanted: &str, keys: &Keys) -> Result<&Thread, IpcError> {
        let mut found: Vec<&Thread> = self
            .threads
            .iter()
            .filter(|t| t.topic.as_deref() == Some(wanted))
            .collect();
        if found.iter().any(|t| keys.mine(t.author)) {
            found.retain(|t| keys.mine(t.author));
        }
        if found.len() > 1 {
            let mut open = Vec::new();
            for thread in &found {
                if self.status(thread).await == "open" {
                    open.push(*thread);
                }
            }
            if !open.is_empty() {
                found = open;
            }
        }
        if found.is_empty() {
            let prefix = wanted
                .strip_prefix("mr/")
                .unwrap_or(wanted)
                .to_ascii_lowercase();
            if prefix.len() >= MIN_PREFIX {
                found = self
                    .threads
                    .iter()
                    .filter(|t| t.id.starts_with(&prefix))
                    .collect();
            }
        }
        match found.as_slice() {
            [thread] => Ok(thread),
            [] => Err(IpcError::new(
                "unknown_mr",
                format!("no merge request for {wanted}; `swarm mr list --all` shows them"),
            )),
            several => Err(bad(format!(
                "{wanted} matches several merge requests; give the ID of one: {}",
                several
                    .iter()
                    .map(|t| &t.id[..8])
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// The commits a merge request proposes, oldest first.
    async fn commits(&self, thread: &Thread) -> Vec<Value> {
        let Some(base) = &thread.base else {
            return Vec::new();
        };
        let range = format!("{base}..{}", thread.head);
        let log = self
            .store
            .git()
            .output(&["log", "--max-count=100", "--format=%H%x09%s", &range])
            .await
            .unwrap_or_default();
        log.lines()
            .rev()
            .filter_map(|line| line.split_once('\t'))
            .map(|(id, subject)| json!({ "id": id, "subject": subject }))
            .collect()
    }

    /// Sign `event` and make it the next one in its author's chain.
    async fn write(&self, mut event: Event, signer: &dyn Signer) -> Result<MrTip, IpcError> {
        let author = Keys::id(signer);
        let tips = self.store.mr_tips().await?;
        if let Some(mr) = &event.mr {
            let key = (mr.clone(), author);
            event.prev = tips.get(&key).map(|oid| oid.to_hex());
        }
        let old = event.prev.clone();
        let bytes = event.sign(signer).map_err(|err| match err {
            swarm_core::ErrorCode::TooLarge => bad("the text is too long"),
            _ => bad("the event cannot be written; check the title, branch and file names"),
        })?;
        self.store.ensure_empty_tree().await?;
        let id = self.store.hash_object("commit", &bytes).await?;
        let mr = event.mr.clone().unwrap_or_else(|| id.clone());
        self.store
            .set_mr_ref(&mr, author, &id, old.as_deref())
            .await?;
        let decode = |hex_id: &str| {
            hex::decode(hex_id)
                .map(swarm_core::ids::Oid::new)
                .map_err(|_| bad("git returned a bad object ID"))
        };
        Ok(MrTip {
            mr: decode(&mr)?,
            author: *author.as_bytes(),
            tip: decode(&id)?,
        })
    }

    /// A new event, dated after what it follows even if a clock runs behind.
    fn event(&self, kind: Kind, name: &str, thread: Option<&Thread>) -> Event {
        let clock = now().max(0) as u64;
        let ts = thread.map_or(clock, |t| clock.max(t.updated.saturating_add(1)));
        let mut event = Event::new(self.id, kind, name, ts);
        event.mr = thread.map(|t| t.id.clone());
        event
    }

    async fn commit_id(&self, rev: &str) -> Result<String, IpcError> {
        self.store
            .git()
            .output(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
            .await
            .map_err(|_| bad("the commit to propose is not in the store"))
    }

    fn target_branch(&self, wanted: Option<&str>) -> Result<(String, String), IpcError> {
        let name = match wanted {
            Some(name) if name.starts_with("refs/") => name.to_string(),
            Some(name) => format!("refs/heads/{name}"),
            None => self
                .head
                .clone()
                .ok_or_else(|| bad("name the branch to merge into"))?,
        };
        let tip = self
            .canonical
            .get(&name)
            .filter(|_| name.starts_with("refs/heads/"))
            .ok_or_else(|| bad(format!("the repo has no branch {name}")))?;
        Ok((name.clone(), tip.clone()))
    }

    /// Title and description taken from the proposed commits.
    async fn describe(&self, base: &str, head: &str, topic: Option<&str>) -> (String, String) {
        let range = format!("{base}..{head}");
        let log = |format: &'static str| {
            let range = range.clone();
            let git = self.store.git();
            async move {
                git.output(&["log", "--reverse", "--max-count=100", format, &range])
                    .await
                    .unwrap_or_default()
            }
        };
        let subjects: Vec<String> = log("--format=%s").await.lines().map(String::from).collect();
        let (title, mut body) = match subjects.as_slice() {
            [only] => (only.clone(), log("--format=%b").await),
            _ => (
                topic
                    .map(String::from)
                    .or_else(|| subjects.last().cloned())
                    .unwrap_or_default(),
                subjects
                    .iter()
                    .map(|subject| format!("- {subject}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        };
        if body.len() > MAX_MADE_BODY {
            let mut end = MAX_MADE_BODY;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
        }
        (title, body)
    }
}

/// Send new events to the repo's peers. Returns how many say they have them;
/// the rest are sent by a later sync.
async fn deliver(state: &DaemonState, repo: &Repo, tip: &MrTip) -> usize {
    let Some(net) = state.net() else {
        return 0;
    };
    let new = std::slice::from_ref(tip);
    mrs::announce(net, repo.id, new).await;
    if net.sessions_for(repo.id).is_empty() {
        // The sync dials the peers we know and exchanges with each.
        let progress = Progress::none();
        let sync = crate::sync::sync_repo(net, repo.id, Vec::new(), &progress);
        match tokio::time::timeout(DELIVER_SYNC, sync).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => tracing::debug!(error = %err, "no peer for the merge request yet"),
            Err(_) => tracing::debug!("looking for a peer took too long"),
        }
    }
    confirm(net, repo.id, tip).await
}

async fn confirm(net: &NetState, repo: RepoId, tip: &MrTip) -> usize {
    let deadline = tokio::time::Instant::now() + DELIVER_CONFIRM;
    loop {
        let sessions = net.sessions_for(repo);
        let mut have = std::collections::BTreeSet::new();
        for handle in &sessions {
            if mrs::ask(handle, repo)
                .await
                .is_some_and(|tips| tips.contains(tip))
            {
                have.insert(*handle.info().node_id.as_bytes());
            }
        }
        if !have.is_empty() || sessions.is_empty() || tokio::time::Instant::now() >= deadline {
            return have.len();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `mr_list`: every merge request, oldest first.
pub async fn mr_list(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = Repo::open(state, args).await?;
    let keys = repo.keys(state)?;
    let mut list = Vec::new();
    for thread in &repo.threads {
        list.push(repo.summary(thread, &keys).await);
    }
    Ok(json!({ "merge_requests": list }))
}

/// `mr_show`: one merge request with its whole conversation.
pub async fn mr_show(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = Repo::open(state, args).await?;
    let keys = repo.keys(state)?;
    let thread = repo.find(arg_str(args, "mr")?, &keys).await?;
    let events: Vec<Value> = thread
        .events
        .iter()
        .map(|item| {
            let event = &item.event;
            json!({
                "id": item.id,
                "kind": event.kind.as_str(),
                "author": repo.author(item.signer, &event.name),
                "ts": event.ts,
                "text": event.text,
                "head": event.head,
                "file": event.file,
                "line": event.line,
                "commit": event.commit,
                "reply_to": event.reply_to,
            })
        })
        .collect();
    let mut summary = repo.summary(thread, &keys).await;
    let mine = repo
        .events
        .iter()
        .filter(|e| e.event.mr.as_deref().unwrap_or(&e.id) == thread.id);
    summary["body"] = json!(thread.body);
    summary["events"] = Value::Array(events);
    summary["commits"] = Value::Array(repo.commits(thread).await);
    summary["ignored"] = json!(mine.count() - thread.events.len());
    summary["store_path"] = json!(repo.store.store_path().to_string_lossy());
    summary["target_tip"] = json!(repo.canonical.get(&thread.target));
    Ok(summary)
}

/// `mr_open`: propose the commit `head`, already in the store. A request this
/// machine has open for the same branches gets the new commit instead.
pub async fn mr_open(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = Repo::open(state, args).await?;
    let keys = repo.signing_keys(state)?;
    let head = repo.commit_id(arg_str(args, "head")?).await?;
    let topic = optional(args, "topic");
    let name = optional(args, "name").unwrap_or_default();

    let existing = match optional(args, "mr") {
        Some(wanted) => Some(repo.find(wanted, &keys).await?),
        None => {
            let (target, _) = repo.target_branch(optional(args, "target"))?;
            let mut found = None;
            for thread in repo.threads.iter().rev() {
                if keys.mine(thread.author)
                    && thread.target == target
                    && topic.is_some()
                    && thread.topic.as_deref() == topic
                    && repo.status(thread).await == "open"
                {
                    found = Some(thread);
                    break;
                }
            }
            found
        }
    };
    let wanted_target = existing.map(|thread| thread.target.as_str());
    let (target, target_tip) = repo.target_branch(wanted_target.or(optional(args, "target")))?;

    if let Some(thread) = existing {
        let state_now = repo.status(thread).await;
        if state_now != "open" {
            return Err(bad(format!("that merge request is {state_now}")));
        }
        if thread.head == head {
            return Ok(json!({ "mr": thread.id, "action": "unchanged", "target": target }));
        }
    }
    if repo.is_ancestor(&head, &target_tip).await {
        return Err(bad(format!(
            "nothing to merge: {target} already holds that commit"
        )));
    }
    let base = repo
        .store
        .git()
        .output(&["merge-base", &target_tip, &head])
        .await
        .ok()
        .filter(|base| is_oid(base))
        .ok_or_else(|| bad(format!("that commit shares no history with {target}")))?;

    let given_title = clean_text(optional(args, "title").unwrap_or_default());
    let given_body = clean_text(optional(args, "body").unwrap_or_default());
    let (event, signer) = match existing {
        Some(thread) => {
            let signer = keys.signing(thread.author).ok_or_else(|| {
                IpcError::new("unauthorized", "only its author can change a merge request")
            })?;
            let mut event = repo.event(Kind::Update, name, Some(thread));
            event.text = clean_text(&format!("{given_title}\n\n{given_body}"));
            (event, signer)
        }
        None => {
            let signer = keys
                .manager
                .as_deref()
                .or(keys.identity.as_deref())
                .ok_or_else(|| IpcError::new("no_signing_key", "no key to sign with"))?;
            let (made_title, made_body) = repo.describe(&base, &head, topic).await;
            let title = if given_title.is_empty() {
                clean_text(&made_title)
            } else {
                given_title.replace('\n', " ")
            };
            let body = if given_body.is_empty() && given_title.is_empty() {
                clean_text(&made_body)
            } else {
                given_body
            };
            let mut event = repo.event(Kind::Open, name, None);
            let title = if title.is_empty() {
                "(untitled)".to_string()
            } else {
                title
            };
            event.text = clean_text(&format!("{title}\n\n{body}"));
            event.target = Some(target.clone());
            event.topic = topic.map(String::from);
            (event, signer)
        }
    };
    let mut event = event;
    event.head = Some(head);
    event.base = Some(base);
    let action = if existing.is_some() {
        "updated"
    } else {
        "opened"
    };
    let tip = repo.write(event, signer).await?;
    let delivered = deliver(state, &repo, &tip).await;
    Ok(json!({
        "mr": tip.mr.to_hex(),
        "action": action,
        "target": target,
        "peers": delivered,
    }))
}

/// `mr_event`: comment on, review, close or reopen a merge request.
pub async fn mr_event(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = Repo::open(state, args).await?;
    let keys = repo.signing_keys(state)?;
    let thread = repo.find(arg_str(args, "mr")?, &keys).await?;
    let kind = Kind::parse(arg_str(args, "kind")?)
        .filter(|kind| !kind.proposes_code())
        .ok_or_else(|| bad("unknown kind of event"))?;
    let name = optional(args, "name").unwrap_or_default();
    let anyone = keys.manager.as_deref().or(keys.identity.as_deref());

    let signer = match kind {
        Kind::Close | Kind::Reopen => {
            let status = repo.status(thread).await;
            let wanted = if kind == Kind::Close {
                "open"
            } else {
                "closed"
            };
            if status != wanted {
                return Err(bad(format!("that merge request is {status}")));
            }
            keys.manager
                .as_deref()
                .or_else(|| keys.signing(thread.author))
                .ok_or_else(|| {
                    IpcError::new(
                        "unauthorized",
                        "only its author or a maintainer can do that",
                    )
                })?
        }
        Kind::Approve | Kind::RequestChanges => {
            let signer = anyone.ok_or_else(|| bad("no key to sign with"))?;
            if keys.mine(thread.author) {
                return Err(bad("you cannot review your own merge request"));
            }
            signer
        }
        _ => anyone.ok_or_else(|| bad("no key to sign with"))?,
    };

    let mut event = repo.event(kind, name, Some(thread));
    event.text = clean_text(optional(args, "text").unwrap_or_default());
    if kind == Kind::Comment && event.text.is_empty() {
        return Err(bad("a comment needs some text"));
    }
    if matches!(kind, Kind::Approve | Kind::RequestChanges) {
        event.commit = Some(thread.head.clone());
    }
    if let Some(file) = optional(args, "file") {
        if kind != Kind::Comment {
            return Err(bad("only a comment can point at a file"));
        }
        let spec = format!("{}:{file}", thread.head);
        let text = repo
            .store
            .git()
            .output(&["cat-file", "blob", &spec])
            .await
            .map_err(|_| bad(format!("the proposed commit has no file {file}")))?;
        event.line = match args.get("line").and_then(Value::as_u64) {
            Some(line) if line == 0 || line > text.lines().count() as u64 => {
                return Err(bad(format!("{file} has no line {line}")));
            }
            Some(line) => u32::try_from(line).ok(),
            None => None,
        };
        event.file = Some(file.to_string());
        event.commit = Some(thread.head.clone());
    }
    if let Some(prefix) = optional(args, "reply_to") {
        let mut found = thread
            .events
            .iter()
            .filter(|e| prefix.len() >= MIN_PREFIX && e.id.starts_with(prefix));
        let (Some(parent), None) = (found.next(), found.next()) else {
            return Err(bad(format!(
                "no single event {prefix} in this merge request"
            )));
        };
        event.reply_to = Some(parent.id.clone());
    }

    let id = thread.id.clone();
    let tip = repo.write(event, signer).await?;
    let delivered = deliver(state, &repo, &tip).await;
    Ok(json!({ "mr": id, "event": tip.tip.to_hex(), "peers": delivered }))
}

/// How many merge requests are open, for `status`.
pub(crate) async fn open_count(state: &DaemonState, repo: RepoId) -> usize {
    let has_any = RepoStore::new(state.home.clone(), repo)
        .mr_tips()
        .await
        .is_ok_and(|tips| !tips.is_empty());
    if !has_any {
        return 0;
    }
    let Ok(repo) = Repo::open(state, &json!({ "repo": repo.to_base32() })).await else {
        return 0;
    };
    let mut count = 0;
    for thread in &repo.threads {
        count += usize::from(repo.status(thread).await == "open");
    }
    count
}
