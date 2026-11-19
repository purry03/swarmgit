//! Maintainers and local conflict resolution (spec 5.4, 5.5, 8.4).

use serde_json::{json, Value};
use swarm_core::consts::{MAX_GLOBS, MAX_MAINTAINERS, SUITE_HYBRID};
use swarm_core::crypto::key_id;
use swarm_core::ids::KeyId;
use swarm_core::keyfile::RepoKey;
use swarm_core::manifest::Maintainer;
use swarm_store::RepoStore;

use super::publish::{publish_repo, RootEdit};
use super::{arg_str, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// Parse a key ID given as 52-character base32 or 64 hex digits.
fn parse_key(value: &str) -> Result<KeyId, IpcError> {
    let parsed = if value.len() == 64 {
        KeyId::from_hex(value)
    } else {
        KeyId::from_base32(value)
    };
    parsed.map_err(|err| IpcError::new("malformed", format!("bad key ID: {err}")))
}

/// `key_new`: create this machine's own signing key for a repo it does not own.
pub async fn key_new(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let store = RepoStore::new(state.home.clone(), repo);
    if store.has_key() {
        return Err(IpcError::new(
            "malformed",
            "a signing key for this repo already exists on this machine",
        ));
    }
    let key = RepoKey::generate(SUITE_HYBRID)
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    store.write_key(&key)?;
    let id = key_id(&signer.bundle());
    Ok(json!({ "key": id.to_base32(), "key_hex": id.to_hex() }))
}

/// `key_id`: the key ID of this machine's signing key and its role.
pub async fn key_id_of(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let key = store
        .load_key()?
        .ok_or_else(|| IpcError::new("no_signing_key", "no signing key for this repo"))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let id = key_id(&signer.bundle());
    let accepted = store.load_state()?;
    let role = if id == accepted.root {
        "root"
    } else if accepted.is_authorized(id) {
        "maintainer"
    } else {
        "unlisted"
    };
    Ok(json!({ "key": id.to_base32(), "key_hex": id.to_hex(), "role": role }))
}

/// `maintainers`: add, remove or list maintainers (root key only).
pub async fn maintainers(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let op = arg_str(args, "op")?;
    let store = RepoStore::new(state.home.clone(), repo);
    let accepted = store.load_state()?;
    let mut list: Vec<Maintainer> = accepted
        .root_manifest()
        .and_then(|stored| stored.manifest.maintainers.clone())
        .unwrap_or_default();
    let view = |list: &[Maintainer]| -> Vec<Value> {
        list.iter()
            .map(|m| json!({ "key": m.key.to_base32(), "globs": m.globs }))
            .collect()
    };
    match op {
        "list" => return Ok(json!({ "maintainers": view(&list) })),
        "add" => {
            let key = parse_key(arg_str(args, "key")?)?;
            if key == accepted.root {
                return Err(IpcError::new("malformed", "the root key needs no entry"));
            }
            let mut globs: Vec<String> = args
                .get("globs")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if globs.is_empty() {
                globs.push("refs/heads/*".to_string());
            }
            if globs.len() > MAX_GLOBS || globs.iter().any(|g| g.is_empty() || g.len() > 256) {
                return Err(IpcError::new("malformed", "bad glob list"));
            }
            list.retain(|m| m.key != key);
            list.push(Maintainer { key, globs });
            if list.len() > MAX_MAINTAINERS {
                return Err(IpcError::new("limit_exceeded", "too many maintainers"));
            }
        }
        "remove" => {
            let key = parse_key(arg_str(args, "key")?)?;
            let before = list.len();
            list.retain(|m| m.key != key);
            if list.len() == before {
                return Err(IpcError::new("malformed", "that key is not a maintainer"));
            }
        }
        other => {
            return Err(IpcError::new(
                "malformed",
                format!("unknown operation {other}"),
            ));
        }
    }
    list.sort_by_key(|m| m.key);
    let result = publish_repo(state, repo, Some(RootEdit::Maintainers(list.clone()))).await?;
    Ok(json!({ "maintainers": view(&list), "publish": result }))
}

/// `accept_ref`: settle a ref conflict locally by taking one signer's value.
pub async fn accept_ref(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let name = arg_str(args, "ref")?.to_string();
    let signer = parse_key(arg_str(args, "signer")?)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let accepted = store.load_state()?;
    let resolved = store.resolve(&accepted);
    if !resolved.conflicts.contains_key(&name) {
        return Err(IpcError::new(
            "malformed",
            format!("{name} has no conflict"),
        ));
    }
    let oid = accepted
        .signers
        .get(&signer)
        .filter(|_| accepted.is_authorized(signer))
        .and_then(|stored| stored.manifest.refs.get(&name))
        .cloned()
        .ok_or_else(|| IpcError::new("malformed", "that signer does not list this ref"))?;
    let git = store.git();
    if !git.history_is_complete(&oid).await {
        return Err(IpcError::new(
            "malformed",
            "that value's objects are not in the store yet; sync first",
        ));
    }
    git.update_ref(&name, &oid).await?;
    Ok(json!({ "ref": name, "oid": oid.to_hex(), "signer": signer.to_hex() }))
}
