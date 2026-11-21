//! Publishing (spec 5.6): sign the store's refs as the next manifest of this
//! node's key, which is the root key or a maintainer's key.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::consts::{MAX_REFS, SUITE_HYBRID};
use swarm_core::crypto::key_id;
use swarm_core::glob::any_glob_matches;
use swarm_core::ids::{Oid, RepoId};
use swarm_core::manifest::{Maintainer, Manifest};
use swarm_core::refname::is_valid_refname;
use swarm_store::RepoStore;

use super::{arg_str, db_repo, now, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// A change to the root manifest that is not a ref update.
pub(crate) enum RootEdit {
    /// Replace the whole maintainer list.
    Maintainers(Vec<Maintainer>),
    /// Name the recovery key (field 12).
    Recovery([u8; 32]),
    /// First manifest of a new root after a rotation: carry over the old
    /// root's fields (maintainers, head, tombstones, recovery key).
    Inherit(Box<StoredManifest>),
}

struct RootFields {
    head: Option<String>,
    maintainers: Option<Vec<Maintainer>>,
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

/// Non-root manifests carry no root-only field (5.4 step 3).
fn no_root_fields() -> RootFields {
    RootFields {
        head: None,
        maintainers: None,
        object_format: None,
        tombstones: None,
        forked_from: None,
        recovery: None,
    }
}

/// Restore store refs after a failed publish (ST-6).
async fn restore_refs(store: &RepoStore, refs: &BTreeMap<String, Oid>) {
    let _ = store.apply_canonical(refs).await;
}

/// `publish`: sign the store's current refs as this key's next manifest.
pub async fn publish(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    publish_repo(state, repo, None).await
}

pub(crate) async fn publish_repo(
    state: &DaemonState,
    repo: RepoId,
    edit: Option<RootEdit>,
) -> Result<Value, IpcError> {
    let row = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let mut accepted = store.load_state()?;
    let before = store.resolve(&accepted);
    let restore: BTreeMap<String, Oid> = before.canonical.clone();

    let outcome = publish_inner(
        state,
        &store,
        &mut accepted,
        &row.object_format,
        edit,
        &before,
    )
    .await;
    if outcome.is_err() {
        restore_refs(&store, &restore).await;
    }
    outcome
}

async fn publish_inner(
    state: &DaemonState,
    store: &RepoStore,
    accepted: &mut AcceptState,
    object_format: &str,
    edit: Option<RootEdit>,
    before: &swarm_core::Resolved,
) -> Result<Value, IpcError> {
    let repo = store.repo_id();
    let key = store
        .load_key()?
        .ok_or_else(|| IpcError::new("no_signing_key", "no signing key for this repo"))?;
    let signer = key
        .signer()
        .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
    let bundle = signer.bundle();
    let signer_id = key_id(&bundle);
    let is_root = signer_id == accepted.root;

    if !is_root && edit.is_some() {
        return Err(IpcError::new(
            "unauthorized",
            "only the root key can change the maintainers",
        ));
    }
    if !is_root && !accepted.is_authorized(signer_id) {
        let message = if signer_id.as_bytes() == repo.as_bytes() {
            "the root key was rotated; this key no longer publishes"
        } else {
            "this key is not the root key or a listed maintainer of this repo"
        };
        return Err(IpcError::new("unauthorized", message));
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

    let fields = if is_root {
        if matches!(edit, Some(RootEdit::Maintainers(_) | RootEdit::Recovery(_))) {
            // An edit republishes the root's own refs, so a maintainer's refs
            // are not absorbed into the root manifest.
            if let Some(stored) = &prev {
                refs = stored.manifest.refs.clone();
            }
        }
        let base = match (&edit, &prev) {
            (Some(RootEdit::Inherit(old)), None) => Some(&**old),
            _ => prev.as_ref(),
        };
        let mut fields = copy_root_fields(base, object_format);
        match edit {
            Some(RootEdit::Maintainers(list)) => {
                fields.maintainers = if list.is_empty() { None } else { Some(list) };
            }
            Some(RootEdit::Recovery(id)) => fields.recovery = Some(id),
            _ => {}
        }
        fields
    } else {
        let globs = accepted.scope_for(signer_id);
        // A push that changes a ref outside the granted globs is refused;
        // refs held by others are simply not repeated in this manifest.
        for (name, oid) in &refs {
            if before.canonical.get(name) != Some(oid) && !any_glob_matches(&globs, name) {
                return Err(IpcError::new(
                    "out_of_scope",
                    format!("{name} is outside the refs this key may publish"),
                ));
            }
        }
        refs.retain(|name, _| any_glob_matches(&globs, name));
        no_root_fields()
    };
    if refs.len() > MAX_REFS {
        return Err(IpcError::new("too_many_refs", "too many refs to publish"));
    }

    if let Some(stored) = &prev {
        let same = stored.manifest.refs == refs
            && stored.manifest.head == fields.head
            && stored.manifest.maintainers == fields.maintainers
            && stored.manifest.object_format == fields.object_format
            && stored.manifest.tombstones == fields.tombstones
            && stored.manifest.forked_from == fields.forked_from
            && stored.manifest.recovery == fields.recovery
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
        refs,
        head: fields.head,
        maintainers: fields.maintainers,
        object_format: fields.object_format,
        tombstones: fields.tombstones,
        forked_from: fields.forked_from,
        recovery: fields.recovery,
        new_root: None,
        signer_keys: bundle,
        suite: u64::from(SUITE_HYBRID),
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
        db.upsert_signer(repo, signer_id, seq, &stored.manifest_id)?;
        db.set_last_sync(repo, now())?;
    }
    let resolved = store.resolve(accepted);
    store.apply_canonical(&resolved.canonical).await?;
    if let Some(net) = state.net() {
        net.announce(repo, stored.envelope.clone()).await;
        // The head pointer is bound to the original root key (DHT-5).
        if signer_id.as_bytes() == repo.as_bytes() {
            if let Some(seed) = key.ed_seed() {
                net.put_head(repo, seed, stored.manifest_id, seq);
            }
        }
        net.announce_dht(repo);
    }

    Ok(json!({
        "seq": seq,
        "manifest_id": hex::encode(stored.manifest_id),
        "noop": false,
        "conflicts": resolved.conflicts.keys().cloned().collect::<Vec<_>>(),
    }))
}
