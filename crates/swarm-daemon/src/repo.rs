//! Repo operations behind the IPC commands: init, sync, publish and status.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::consts::{MAX_REFS, SUITE_HYBRID};
use swarm_core::crypto::{key_id, repo_id};
use swarm_core::ids::{KeyId, Link, Oid, RepoId};
use swarm_core::keyfile::RepoKey;
use swarm_core::manifest::Manifest;
use swarm_core::refname::is_valid_refname;
use swarm_store::git::{self, Git};
use swarm_store::RepoStore;

use crate::protocol::IpcError;
use crate::state::DaemonState;

fn now() -> i64 {
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

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, IpcError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| IpcError::new("malformed", format!("missing argument {key}")))
}

fn arg_hints(args: &Value) -> Vec<String> {
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

fn db_repo(state: &DaemonState, repo: RepoId) -> Result<swarm_store::RepoRow, IpcError> {
    let db = state
        .db
        .lock()
        .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
    db.repo(repo)?
        .ok_or_else(|| IpcError::new("unknown_repo", "unknown repo"))
}

fn root_seq(state: &AcceptState, repo: RepoId) -> u64 {
    state
        .signers
        .get(&KeyId::from_bytes(*repo.as_bytes()))
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
    if !git::is_git_worktree(&dir).await {
        return Err(IpcError::new(
            "malformed",
            format!("{} is not a git repository", dir.display()),
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

    announce_lan(state);
    let base32 = repo.to_base32();
    let link = match state.net() {
        Some(net) => crate::net::link_with_hint(repo, net.listen, net.node_id, None).to_string(),
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
pub async fn sync(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let hints = arg_hints(args);
    let net = state
        .net()
        .ok_or_else(|| IpcError::new("malformed", "network is not ready"))?;
    let outcome = crate::sync::sync_repo(net, repo, hints)
        .await
        .map_err(net_error)?;
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

fn net_error(err: swarm_net::NetError) -> IpcError {
    IpcError::new(err.code(), err.to_string())
}

struct RootFields {
    head: Option<String>,
    maintainers: Option<Vec<swarm_core::manifest::Maintainer>>,
    object_format: Option<String>,
    tombstones: Option<Vec<String>>,
    forked_from: Option<[u8; 32]>,
    recovery: Option<[u8; 32]>,
}

fn copy_root_fields(prev: Option<&StoredManifest>, format: &str) -> RootFields {
    match prev {
        Some(stored) => RootFields {
            head: stored.manifest.head.clone(),
            maintainers: stored.manifest.maintainers.clone(),
            object_format: stored
                .manifest
                .object_format
                .clone()
                .or_else(|| Some(format.to_string())),
            tombstones: stored.manifest.tombstones.clone(),
            forked_from: stored.manifest.forked_from,
            recovery: stored.manifest.recovery,
        },
        None => RootFields {
            head: None,
            maintainers: None,
            object_format: Some(format.to_string()),
            tombstones: None,
            forked_from: None,
            recovery: None,
        },
    }
}

/// Restore store refs after a failed publish (ST-6).
async fn restore_refs(store: &RepoStore, refs: &BTreeMap<String, Oid>) {
    let _ = store.apply_canonical(refs).await;
}

/// `publish`: sign the store's current refs as the next root manifest.
pub async fn publish(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let row = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let mut accepted = store.load_state()?;
    let restore: BTreeMap<String, Oid> = accepted
        .root_manifest()
        .map(|stored| stored.manifest.refs.clone())
        .unwrap_or_default();

    let key = match store.load_key() {
        Ok(Some(key)) => key,
        Ok(None) => {
            restore_refs(&store, &restore).await;
            return Err(IpcError::new(
                "no_signing_key",
                "no signing key for this repo",
            ));
        }
        Err(err) => {
            restore_refs(&store, &restore).await;
            return Err(err.into());
        }
    };
    let signer = match key.signer() {
        Ok(signer) => signer,
        Err(err) => {
            restore_refs(&store, &restore).await;
            return Err(IpcError::new("bad_keys", err.to_string()));
        }
    };
    let bundle = signer.bundle();
    let signer_id = key_id(&bundle);
    if signer_id.as_bytes() != repo.as_bytes() {
        restore_refs(&store, &restore).await;
        return Err(IpcError::new(
            "unauthorized",
            "key file is not the root key for this repo",
        ));
    }

    let prev = accepted.signers.get(&signer_id).cloned();
    let current = store.read_refs().await?;
    let mut refs = BTreeMap::new();
    for (name, oid) in &current {
        let allowed = (name.starts_with("refs/heads/") || name.starts_with("refs/tags/"))
            && is_valid_refname(name);
        if allowed {
            refs.insert(name.clone(), oid.clone());
        }
    }
    if refs.len() > MAX_REFS {
        restore_refs(&store, &restore).await;
        return Err(IpcError::new("too_many_refs", "too many refs to publish"));
    }

    let root = copy_root_fields(prev.as_ref(), &row.object_format);
    let (head, maintainers, object_format, tombstones, forked_from, recovery) = (
        root.head,
        root.maintainers,
        root.object_format,
        root.tombstones,
        root.forked_from,
        root.recovery,
    );

    if let Some(stored) = &prev {
        let same = stored.manifest.refs == refs
            && stored.manifest.head == head
            && stored.manifest.maintainers == maintainers
            && stored.manifest.object_format == object_format
            && stored.manifest.tombstones == tombstones
            && stored.manifest.forked_from == forked_from
            && stored.manifest.recovery == recovery
            && stored.manifest.new_root.is_none();
        if same {
            return Ok(json!({
                "seq": stored.manifest.seq,
                "manifest_id": hex::encode(stored.manifest_id),
                "noop": true,
            }));
        }
    }

    let (seq, prev_id) = match &prev {
        Some(stored) => (stored.manifest.seq + 1, stored.manifest_id),
        None => (1, [0u8; 32]),
    };
    let manifest = Manifest {
        version: 1,
        repo_id: *repo.as_bytes(),
        signer: *signer_id.as_bytes(),
        seq,
        prev: prev_id,
        ts: now() as u64,
        refs: refs.clone(),
        head,
        maintainers,
        object_format,
        tombstones,
        forked_from,
        recovery,
        new_root: None,
        signer_keys: bundle,
        suite: u64::from(SUITE_HYBRID),
    };
    let envelope = match manifest.sign(&*signer) {
        Ok(envelope) => envelope,
        Err(err) => {
            restore_refs(&store, &restore).await;
            return Err(IpcError::new(err.to_string(), err.to_string()));
        }
    };
    if let Err(err) = accepted.accept(envelope.clone()) {
        restore_refs(&store, &restore).await;
        return Err(IpcError::new(err.to_string(), err.to_string()));
    }
    let stored = StoredManifest::from(envelope);
    if let Err(err) = store.write_manifest(&stored) {
        restore_refs(&store, &restore).await;
        return Err(err.into());
    }
    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        db.upsert_signer(repo, signer_id, seq, &stored.manifest_id)?;
        db.set_last_sync(repo, now())?;
    }
    let resolved = store.resolve(&accepted);
    store.apply_canonical(&resolved.canonical).await?;
    if let Some(net) = state.net() {
        net.announce(repo, stored.envelope.clone()).await;
    }

    Ok(json!({
        "seq": seq,
        "manifest_id": hex::encode(stored.manifest_id),
        "noop": false,
    }))
}

/// `repo_info`: what the remote helper needs to know.
pub async fn repo_info(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let row = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let accepted = store.load_state()?;
    let resolved = store.resolve(&accepted);
    Ok(json!({
        "repo": repo.to_base32(),
        "store_path": store.store_path().to_string_lossy(),
        "seq": root_seq(&accepted, repo),
        "has_key": store.has_key(),
        "seeding": row.seeding,
        "conflicts": resolved.conflicts.keys().cloned().collect::<Vec<_>>(),
        "object_format": row.object_format,
    }))
}

fn summary(state: &DaemonState, row: &swarm_store::RepoRow) -> Result<Value, IpcError> {
    let store = RepoStore::new(state.home.clone(), row.id);
    let accepted = store.load_state()?;
    let resolved = store.resolve(&accepted);
    let equivocations = {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        let list = db.equivocations(row.id)?;
        let peers = db.peer_count(row.id)?;
        (list, peers)
    };
    let (equivocations, peers) = equivocations;
    let equivocations: Vec<Value> = equivocations
        .into_iter()
        .map(|item| {
            json!({
                "signer": item.signer.to_hex(),
                "seq": item.seq,
                "id_a": hex::encode(item.id_a),
                "id_b": hex::encode(item.id_b),
                "seen": item.seen,
            })
        })
        .collect();
    Ok(json!({
        "repo": row.id.to_base32(),
        "link": format!("swarm://{}", row.id.to_base32()),
        "seq": root_seq(&accepted, row.id),
        "refs": resolved.canonical.len(),
        "peers": peers,
        "seeding": row.seeding,
        "object_format": row.object_format,
        "conflicts": resolved.conflicts.keys().cloned().collect::<Vec<_>>(),
        "equivocations": equivocations,
    }))
}

fn repo_rows(state: &DaemonState) -> Result<Vec<swarm_store::RepoRow>, IpcError> {
    let db = state
        .db
        .lock()
        .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
    Ok(db.repos()?)
}

/// `status`: summaries for one or all repos.
pub async fn status(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let rows = match args.get("repo").and_then(Value::as_str) {
        Some(value) => vec![db_repo(state, parse_repo(value)?)?],
        None => repo_rows(state)?,
    };
    let mut repos = Vec::with_capacity(rows.len());
    for row in &rows {
        repos.push(summary(state, row)?);
    }
    let lan = match state.net().map(|net| net.lan()) {
        Some(Some(lan)) => json!({ "enabled": true, "port": lan.port() }),
        _ => json!({ "enabled": false }),
    };
    Ok(json!({ "repos": repos, "lan": lan }))
}

/// Multicast a LAN announce now, so a new seed is found without waiting for
/// the 60 s timer (6.3).
fn announce_lan(state: &DaemonState) {
    if let Some(lan) = state.net().and_then(|net| net.lan()) {
        tokio::spawn(async move { lan.announce_now().await });
    }
}

/// `list_repos`: the array form used by some clients.
pub async fn list_repos(state: &DaemonState, _args: &Value) -> Result<Value, IpcError> {
    let rows = repo_rows(state)?;
    let mut repos = Vec::with_capacity(rows.len());
    for row in &rows {
        repos.push(summary(state, row)?);
    }
    Ok(Value::Array(repos))
}

/// `seed` and `unseed` share this implementation.
pub async fn set_seeding(
    state: &DaemonState,
    args: &Value,
    seeding: bool,
) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let _ = db_repo(state, repo)?;
    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        db.set_seeding(repo, seeding)?;
    }
    if seeding {
        announce_lan(state);
    }
    Ok(json!({ "seeding": seeding }))
}

/// `fork`: create an independent repo from another one (section 8.4).
pub async fn fork(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let raw = arg_str(args, "repo")?;
    let hints = arg_hints(args);
    let source = parse_repo(raw)?;
    let net = state
        .net()
        .ok_or_else(|| IpcError::new("malformed", "network is not ready"))?;
    let outcome = crate::sync::sync_repo(net, source, hints)
        .await
        .map_err(net_error)?;
    let source_store = RepoStore::new(state.home.clone(), source);
    if !source_store.exists() {
        return Err(IpcError::new("unknown_repo", "source store does not exist"));
    }

    let key = RepoKey::generate(SUITE_HYBRID)
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let bundle = signer.bundle();
    let repo = repo_id(&bundle);
    let signer_id = key_id(&bundle);
    let store = RepoStore::new(state.home.clone(), repo);
    if store.exists() {
        return Err(IpcError::new(
            "malformed",
            "the derived fork store already exists",
        ));
    }
    git::clone_bare(&source_store.store_path(), &store.store_path())
        .await
        .map_err(|err| IpcError::new("malformed", err.to_string()))?;
    store
        .configure()
        .await
        .map_err(|err| IpcError::new("malformed", err.to_string()))?;
    store.write_key(&key)?;

    let manifest = Manifest {
        version: 1,
        repo_id: *repo.as_bytes(),
        signer: *signer_id.as_bytes(),
        seq: 1,
        prev: [0u8; 32],
        ts: now() as u64,
        refs: outcome.ref_list.clone(),
        head: outcome.head.clone(),
        maintainers: None,
        object_format: Some(outcome.object_format.clone()),
        tombstones: None,
        forked_from: Some(*source.as_bytes()),
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
    store.persist_state(&accepted)?;
    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        db.insert_repo(repo, &outcome.object_format, now())?;
        db.upsert_signer(repo, signer_id, 1, &stored.manifest_id)?;
        db.set_seeding(repo, true)?;
        db.set_last_sync(repo, now())?;
    }
    let resolved = store.resolve(&accepted);
    store.apply_canonical(&resolved.canonical).await?;
    announce_lan(state);
    let link = crate::net::link_with_hint(repo, net.listen, net.node_id, None).to_string();
    Ok(json!({
        "repo_id": repo.to_base32(),
        "link": link,
        "forked_from": source.to_base32(),
        "object_format": outcome.object_format,
        "seq": 1,
    }))
}
