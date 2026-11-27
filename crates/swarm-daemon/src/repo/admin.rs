//! Status, seeding, fork and gc commands.

use serde_json::{json, Value};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::consts::SUITE_HYBRID;
use swarm_core::crypto::{key_id, repo_id};
use swarm_core::ids::{NodeId, RepoId};
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

/// `workdir`: a git directory on this machine fetched from or pushed to a repo.
pub async fn record_workdir(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let _ = db_repo(state, repo)?;
    let path = arg_str(args, "path")?;
    if !std::path::Path::new(path).is_absolute() {
        return Err(IpcError::new("malformed", "path must be absolute"));
    }
    state
        .db
        .lock()
        .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?
        .record_workdir(repo, path)?;
    Ok(json!({ "recorded": true }))
}

/// The recorded project folders, when there are some and none is left.
pub(crate) fn deleted_workdirs(paths: &[String]) -> Option<Vec<&str>> {
    // Only a definite "not found" counts, not an unreadable or odd path.
    let gone = |path: &String| matches!(std::path::Path::new(path).try_exists(), Ok(false));
    (!paths.is_empty() && paths.iter().all(gone)).then(|| {
        paths
            .iter()
            .map(|path| path.strip_suffix("/.git").unwrap_or(path))
            .collect()
    })
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
        (db.equivocations(row.id)?, db.workdirs(row.id)?)
    };
    let (equivocations, workdirs) = equivocations;
    let seeders = state.net().and_then(|net| net.seeders(row.id));
    // Only a repo whose files are here can be served.
    let seeding = row.seeding && store.exists();
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
        "root": accepted.root.to_base32(),
        "recovery": accepted.recovery.is_some(),
        "refs": resolved.canonical.len(),
        // Every seeder known, this machine included.
        "seeders": seeders.map(|(others, _)| others + usize::from(seeding)),
        "checked_secs_ago": seeders.map(|(_, age)| age.as_secs()),
        "checking": state.net().is_some_and(|net| net.is_checking(row.id)),
        "on_disk": store.exists(),
        "error": (!store.exists()).then_some("seeding failed: files missing"),
        "deleted_folders": deleted_workdirs(&workdirs),
        "seeding": seeding,
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
                "address": relay.addr.as_ref().map(ToString::to_string),
                "registered": relay.registered,
                "observed": relay.observed,
                "error": relay.error,
            })
        })
        .collect();
    let network = state.net().map(|net| {
        let onion = net.onion();
        json!({
            "proxy": net.settings.proxy.as_ref().map(|proxy| proxy.server.clone()),
            "obfs": net.settings.obfs,
            "experimental_udp": net.settings.experimental_udp,
            "onion": onion.host,
            "onion_error": onion.error,
        })
    });
    Ok(json!({ "repos": repos, "lan": lan, "relays": relays, "network": network }))
}

/// `peers`: for one or all repos, the cached peer addresses and the open
/// sessions.
pub async fn peers(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let rows = match args.get("repo").and_then(Value::as_str) {
        Some(value) => vec![db_repo(state, parse_repo(value)?)?],
        None => repo_rows(state)?,
    };
    let net = state.net();
    let mut peers = Vec::new();
    for row in &rows {
        let repo = row.id.to_base32();
        let cached = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?
            .peers(row.id)?;
        let mut connected: Vec<NodeId> = net
            .map(|net| net.sessions_for(row.id))
            .unwrap_or_default()
            .iter()
            .map(|session| session.info().node_id)
            .collect();
        connected.sort_by_key(|node| *node.as_bytes());
        connected.dedup();
        let relay_of = |node: NodeId| net.and_then(|net| net.relay_of(node));
        let mut listed = Vec::new();
        for peer in cached {
            let node = peer.node_id.map(NodeId::from_bytes);
            let open = node.filter(|node| connected.contains(node));
            listed.extend(open);
            peers.push(json!({
                "repo": repo,
                "addr": peer.addr,
                "node": node.map(|node| node.to_base32()),
                "connected": open.is_some(),
                "relay": open.and_then(relay_of),
                "last_ok_secs_ago": peer.last_ok.map(|when| (now() - when).max(0)),
                "fails": peer.fails,
            }));
        }
        // Sessions the cache has no address for, such as inbound ones.
        for &node in connected.iter().filter(|node| !listed.contains(node)) {
            peers.push(json!({
                "repo": repo,
                "addr": null,
                "node": node.to_base32(),
                "connected": true,
                "relay": relay_of(node),
                "last_ok_secs_ago": null,
                "fails": 0,
            }));
        }
    }
    Ok(json!({ "peers": peers }))
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

/// `purge`: stop serving a repo and delete its store, manifests and
/// database rows (SEC-13). The CLI asks for confirmation first.
pub async fn purge(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let _ = db_repo(state, repo)?;
    let net = state.net();
    // Wait for a running sync, which would write the store back.
    let lock = net.map(|net| net.sync_lock(repo));
    let _held = match &lock {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };
    let had_key = state.home.repo_key_path(repo).is_file();
    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        // Rows first: from here on nothing serves or announces it.
        db.forget_repo(repo)?;
    }
    if let Some(net) = net {
        net.close_sessions(repo);
    }
    let remove = |result: std::io::Result<()>| match result {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(IpcError::new(
            "malformed",
            format!("could not delete everything: {err}"),
        )),
        _ => Ok(()),
    };
    // The key cannot be recreated, so it goes only when asked for by name.
    let delete_key = args.get("delete_key").and_then(Value::as_bool) == Some(true);
    if delete_key {
        remove(std::fs::remove_file(state.home.repo_key_path(repo)))?;
    }
    remove(std::fs::remove_dir_all(state.home.repo_dir(repo)))?;
    Ok(json!({ "purged": true, "had_key": had_key, "key_deleted": had_key && delete_key }))
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
