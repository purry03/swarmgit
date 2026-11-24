//! Repo operations behind the IPC commands: init, sync and publish.

mod admin;
mod bundle;
mod maintainers;
mod mirror;
mod publish;
mod recovery;

use admin::announce_routes;
pub use admin::{fork, gc, list_repos, repo_info, set_seeding, status};
pub use bundle::{export, import};
pub use maintainers::{accept_ref, key_id_of, key_new, maintainers};
pub use mirror::mirror_export;
pub use publish::publish;
pub use recovery::{recovery_set, rotate};

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::{key_id, repo_id};
use swarm_core::ids::{Link, RepoId};
use swarm_core::keyfile::RepoKey;
use swarm_core::manifest::Manifest;
use swarm_store::git::{self, Git};
use swarm_store::RepoStore;

use crate::protocol::IpcError;
use crate::state::DaemonState;
use crate::sync::progress::Progress;

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse a repo argument: base32, hex or a full `swarm://` link.
pub fn parse_repo(value: &str) -> Result<RepoId, IpcError> {
    if value.starts_with("swarm://") {
        return Link::parse(value)
            .map(|link| link.repo)
            .map_err(|err| IpcError::new("malformed", err.to_string()));
    }
    if value.len() == 52 {
        return RepoId::from_base32(value)
            .map_err(|err| IpcError::new("malformed", err.to_string()));
    }
    RepoId::from_hex(value).map_err(|err| IpcError::new("malformed", err.to_string()))
}

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, IpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| IpcError::new("malformed", format!("missing argument {key}")))
}

pub(crate) fn arg_hints(args: &Value) -> Vec<String> {
    args.get("hints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn db_repo(state: &DaemonState, repo: RepoId) -> Result<swarm_store::RepoRow, IpcError> {
    let db = state
        .db
        .lock()
        .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
    db.repo(repo)?
        .ok_or_else(|| IpcError::new("unknown_repo", "unknown repo"))
}

pub(crate) fn root_seq(state: &AcceptState, repo: RepoId) -> u64 {
    let _ = repo;
    state
        .root_manifest()
        .map(|stored| stored.manifest.seq)
        .unwrap_or(0)
}

/// `init_repo`: create the key, store and first manifest from a user repo.
pub async fn init_repo(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let raw = arg_str(args, "git_dir")?;
    if raw.contains('\0') {
        return Err(IpcError::new("malformed", "git_dir contains NUL"));
    }
    let dir = std::fs::canonicalize(raw)
        .map_err(|err| IpcError::new("malformed", format!("cannot open git_dir: {err}")))?;
    let worktree = Git::worktree(&dir);
    if let Err(err) = worktree.output(&["rev-parse", "--git-dir"]).await {
        return Err(IpcError::new(
            "malformed",
            format!("{} is not a usable git repository: {err}", dir.display()),
        ));
    }
    if worktree
        .output(&["remote", "get-url", "swarm"])
        .await
        .is_ok()
    {
        return Err(IpcError::new(
            "malformed",
            "a git remote named swarm already exists",
        ));
    }

    let object_format = git::worktree_object_format(&dir).await?;
    let head = git::worktree_head(&dir).await;
    let user_refs = worktree.refs().await?;

    let key = RepoKey::generate(SUITE_HYBRID)
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let bundle = signer.bundle();
    let repo = repo_id(&bundle);
    let signer_id = key_id(&bundle);

    let store = RepoStore::new(state.home.clone(), repo);
    store.create(&object_format).await?;
    store.write_key(&key)?;

    if !user_refs.is_empty() {
        let refspecs = vec![
            "+refs/heads/*:refs/heads/*".to_string(),
            "+refs/tags/*:refs/tags/*".to_string(),
        ];
        store.git().fetch_from(&dir, &refspecs).await?;
    }
    let refs = store.read_refs().await?;

    let manifest = Manifest {
        version: 1,
        repo_id: *repo.as_bytes(),
        signer: *signer_id.as_bytes(),
        seq: 1,
        prev: [0u8; 32],
        ts: now() as u64,
        refs,
        head,
        maintainers: None,
        object_format: Some(object_format.clone()),
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: None,
        signer_keys: bundle,
        suite: u64::from(SUITE_HYBRID),
    };
    let envelope = manifest
        .sign(&*signer)
        .map_err(|err| IpcError::new(err.to_string(), err.to_string()))?;
    let mut accepted = AcceptState::new(repo);
    accepted
        .accept(envelope.clone())
        .map_err(|err| IpcError::new(err.to_string(), err.to_string()))?;
    let stored = StoredManifest::from(envelope);
    store.write_manifest(&stored)?;

    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        db.insert_repo(repo, &object_format, now())?;
        db.upsert_signer(repo, signer_id, 1, &stored.manifest_id)?;
        db.set_seeding(repo, true)?;
        db.set_last_sync(repo, now())?;
    }

    let resolved = store.resolve(&accepted);
    store.apply_canonical(&resolved.canonical).await?;

    announce_routes(state, repo);
    if let (Some(net), Some(seed)) = (state.net(), key.ed_seed()) {
        net.put_head(repo, seed, stored.manifest_id, 1);
    }
    let base32 = repo.to_base32();
    let link = match state.net() {
        Some(net) => net.link(repo).to_string(),
        None => format!("swarm://{base32}"),
    };
    Ok(json!({
        "repo_id": base32,
        "link": link,
        "object_format": object_format,
        "seq": 1,
    }))
}

/// `sync`: link-hint sync plus cached peers, then a local resolve fallback.
pub async fn sync(
    state: &DaemonState,
    args: &Value,
    progress: &Progress,
) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let hints = arg_hints(args);
    let net = state
        .net()
        .ok_or_else(|| IpcError::new("malformed", "network is not ready"))?;
    let mirrors = mirror::bases(&hints);
    let outcome = match crate::sync::sync_repo(net, repo, hints, progress).await {
        Ok(outcome) => outcome,
        Err(err) => {
            // MIR-2: peers first; mirrors only when no peer could help.
            match from_mirrors(state, net, repo, &mirrors, progress).await {
                Some(outcome) => outcome,
                None => return Err(net_error(err)),
            }
        }
    };
    let ref_list: serde_json::Map<String, Value> = outcome
        .ref_list
        .iter()
        .map(|(name, oid)| (name.clone(), Value::String(oid.to_hex())))
        .collect();
    Ok(json!({
        "state": outcome.state,
        "refs": outcome.refs(),
        "ref_list": ref_list,
        "head": outcome.head,
        "seq": outcome.seq,
        "store_path": outcome.store_path.to_string_lossy(),
        "object_format": outcome.object_format,
        "peers": outcome.peers,
        "accepted": outcome.accepted,
        "rejected": outcome.rejected,
    }))
}

/// Try each mirror in turn; the first bundle that imports wins.
async fn from_mirrors(
    state: &DaemonState,
    net: &std::sync::Arc<crate::net::NetState>,
    repo: RepoId,
    bases: &[String],
    progress: &Progress,
) -> Option<crate::sync::SyncResult> {
    for base in bases {
        progress.say("mirror", format!("trying mirror {base}"));
        let tmp = state
            .home
            .root()
            .join(format!("mirror-{}.tmp", repo.to_base32()));
        let got = mirror::download(base, repo, &tmp).await;
        let imported = match got {
            Ok(_) => bundle::import_file(state, &tmp, Some(repo))
                .await
                .map_err(|e| e.msg),
            Err(err) => Err(err),
        };
        let _ = std::fs::remove_file(&tmp);
        match imported {
            Ok(_) => {
                progress.say("mirror", "imported from the mirror");
                return crate::sync::local_result(net, repo, "mirror");
            }
            Err(err) => progress.say("mirror", format!("mirror failed: {err}")),
        }
    }
    None
}

pub(crate) fn net_error(err: swarm_net::NetError) -> IpcError {
    IpcError::new(err.code(), err.to_string())
}
