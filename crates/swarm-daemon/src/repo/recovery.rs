//! The recovery key (spec 11.9): a suite-2 backup key that can move the root.

use serde_json::{json, Value};
use swarm_core::accept::StoredManifest;
use swarm_core::consts::{SUITE_HYBRID, SUITE_RECOVERY};
use swarm_core::crypto::key_id;
use swarm_core::keyfile::{RepoKey, REPO_KEY_FILE_LEN};
use swarm_core::manifest::Manifest;
use swarm_store::{keyio, RepoStore};

use super::publish::{publish_repo, RootEdit};
use super::{arg_str, now, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// `recovery_set`: create a recovery key, write it to `path` and name it in a
/// new root manifest. Keep the file offline; it is the way back from a stolen
/// or lost root key.
pub async fn recovery_set(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let path = std::path::PathBuf::from(arg_str(args, "path")?);
    let store = RepoStore::new(state.home.clone(), repo);
    let accepted = store.load_state()?;
    let root_key = store
        .load_key()?
        .ok_or_else(|| IpcError::new("no_signing_key", "no signing key for this repo"))?;
    let root_signer = root_key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    if key_id(&root_signer.bundle()) != accepted.root {
        return Err(IpcError::new(
            "unauthorized",
            "only the root key can set the recovery key",
        ));
    }
    let key = RepoKey::generate(SUITE_RECOVERY)
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let id = key_id(&signer.bundle());
    keyio::write_private_new(&path, &key.to_bytes())?;
    if let Err(err) = publish_repo(state, repo, Some(RootEdit::Recovery(*id.as_bytes()))).await {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(json!({
        "recovery": id.to_base32(),
        "path": path.to_string_lossy(),
    }))
}

/// `rotate`: move the root to a fresh key, authorized by the recovery key.
pub async fn rotate(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let path = std::path::PathBuf::from(arg_str(args, "recovery")?);
    let store = RepoStore::new(state.home.clone(), repo);
    let mut accepted = store.load_state()?;
    let old_root = accepted
        .root_manifest()
        .cloned()
        .ok_or_else(|| IpcError::new("unknown_repo", "this machine has no root manifest yet"))?;

    let bytes = keyio::read_private(&path)?
        .ok_or_else(|| IpcError::new("malformed", "recovery key file not found"))?;
    if bytes.len() != REPO_KEY_FILE_LEN {
        return Err(IpcError::new(
            "bad_keys",
            "recovery key file has the wrong length",
        ));
    }
    let recovery =
        RepoKey::from_bytes(&bytes).map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    if recovery.suite() != SUITE_RECOVERY {
        return Err(IpcError::new("bad_keys", "that is not a recovery key"));
    }
    let signer = recovery
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let recovery_id = key_id(&signer.bundle());
    if accepted.recovery != Some(recovery_id) {
        return Err(IpcError::new(
            "unauthorized",
            "the repo's root manifest does not name this recovery key",
        ));
    }

    let fresh = RepoKey::generate(SUITE_HYBRID)
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let fresh_bundle = fresh
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?
        .bundle();
    let new_root = key_id(&fresh_bundle);

    let prev = accepted.signers.get(&recovery_id).cloned();
    let (seq, prev_id) = match &prev {
        Some(stored) => (stored.manifest.seq + 1, stored.manifest_id),
        None => (1, [0u8; 32]),
    };
    let manifest = Manifest {
        version: 1,
        repo_id: *repo.as_bytes(),
        signer: *recovery_id.as_bytes(),
        seq,
        prev: prev_id,
        ts: now() as u64,
        refs: std::collections::BTreeMap::new(),
        head: None,
        maintainers: None,
        object_format: None,
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: Some(*new_root.as_bytes()),
        signer_keys: signer.bundle(),
        suite: u64::from(SUITE_RECOVERY),
    };
    let envelope = manifest
        .sign(&*signer)
        .map_err(|err| IpcError::new(err.to_string(), err.to_string()))?;
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
        db.upsert_signer(repo, recovery_id, seq, &stored.manifest_id)?;
    }

    // The old root key (if this machine has it) is kept aside, never reused.
    let key_path = store.key_path();
    if key_path.exists() {
        let aside = key_path.with_extension("key.old");
        std::fs::rename(&key_path, &aside)
            .map_err(|err| IpcError::new("malformed", err.to_string()))?;
    }
    store.write_key(&fresh)?;

    if let Some(net) = state.net() {
        net.announce(repo, stored.envelope.clone()).await;
    }
    let published = publish_repo(state, repo, Some(RootEdit::Inherit(Box::new(old_root)))).await?;
    Ok(json!({
        "new_root": new_root.to_base32(),
        "rotation_seq": seq,
        "publish": published,
    }))
}
