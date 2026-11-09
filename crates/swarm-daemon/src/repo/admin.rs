//! Status, seeding, fork and gc commands.

use serde_json::{json, Value};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::{key_id, repo_id};
use swarm_core::ids::RepoId;
use swarm_core::keyfile::RepoKey;
use swarm_core::manifest::Manifest;
use swarm_store::git;
use swarm_store::RepoStore;

use super::{arg_hints, arg_str, db_repo, net_error, now, parse_repo, root_seq};
use crate::protocol::IpcError;
use crate::state::DaemonState;
use crate::sync::progress::Progress;

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
    let relays: Vec<Value> = state
        .net()
        .map(|net| net.relay_statuses())
        .unwrap_or_default()
        .iter()
        .map(|relay| {
            json!({
                "relay": relay.name,
                "registered": relay.registered,
                "observed": relay.observed,
                "error": relay.error,
            })
        })
        .collect();
    Ok(json!({ "repos": repos, "lan": lan, "relays": relays }))
}

/// Announce a newly served repo now on LAN (6.3) and the DHT (DHT-2),
/// instead of waiting for the periodic timers.
pub(crate) fn announce_routes(state: &DaemonState, repo: RepoId) {
    let Some(net) = state.net() else {
        return;
    };
    if let Some(lan) = net.lan() {
        tokio::spawn(async move { lan.announce_now().await });
    }
    net.announce_dht(repo);
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
        announce_routes(state, repo);
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
    let outcome = crate::sync::sync_repo(net, source, hints, &Progress::none())
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
    announce_routes(state, repo);
    let link = net.link(repo).to_string();
    Ok(json!({
        "repo_id": repo.to_base32(),
        "link": link,
        "forked_from": source.to_base32(),
        "object_format": outcome.object_format,
        "seq": 1,
    }))
}

/// `gc`: repack and prune the bare store (ST-7). It refuses with `busy`
/// while a sync of the repo or any upload is running, so a pack in flight
/// never loses its objects.
pub async fn gc(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let _ = db_repo(state, repo)?;
    let net = state
        .net()
        .ok_or_else(|| IpcError::new("malformed", "network is not ready"))?;
    let store = RepoStore::new(state.home.clone(), repo);
    if !store.exists() {
        return Err(IpcError::new("unknown_repo", "store does not exist"));
    }
    let lock = net.sync_lock(repo);
    let Ok(_sync) = lock.try_lock() else {
        return Err(IpcError::new("busy", "a sync of this repo is running"));
    };
    let Ok(_uploads) = net.uploads.try_acquire_many(crate::net::UPLOAD_SLOTS) else {
        return Err(IpcError::new("busy", "an upload is running"));
    };
    let output = tokio::process::Command::new("git")
        .arg("--git-dir")
        .arg(store.store_path())
        .args(["gc", "--prune=now", "--quiet"])
        .output()
        .await
        .map_err(|err| IpcError::new("malformed", err.to_string()))?;
    if !output.status.success() {
        return Err(IpcError::new(
            "malformed",
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(json!({ "gc": true }))
}
