//! `about`: show or set a repo's name and description.

use serde_json::{json, Value};
use swarm_core::about::{is_valid_text, About};
use swarm_core::consts::{MAX_DESCRIPTION_BYTES, MAX_NAME_BYTES};
use swarm_core::crypto::key_id;
use swarm_store::RepoStore;

use super::{arg_str, db_repo, now, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

fn field<'a>(args: &'a Value, key: &str, max: usize) -> Result<Option<&'a str>, IpcError> {
    let Some(text) = args.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    let text = text.trim();
    if !is_valid_text(text, max) {
        return Err(IpcError::new(
            "malformed",
            format!("the {key} must be one line of at most {max} bytes"),
        ));
    }
    Ok(Some(text))
}

/// With neither `name` nor `description`, report the stored values. Otherwise
/// sign them with this machine's root or maintainer key; a field not given
/// keeps its value.
pub async fn about(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let name = field(args, "name", MAX_NAME_BYTES)?;
    let description = field(args, "description", MAX_DESCRIPTION_BYTES)?;
    let _ = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    let accepted = store.load_state()?;
    let clock = now() as u64;
    let old = store.load_about(&accepted, clock).map(|old| old.about);
    let mut new_name = old.as_ref().map(|o| o.name.clone()).unwrap_or_default();
    let mut new_description = old
        .as_ref()
        .map(|o| o.description.clone())
        .unwrap_or_default();

    if name.is_some() || description.is_some() {
        let key = store
            .load_key()?
            .ok_or_else(|| IpcError::new("no_signing_key", "no signing key for this repo"))?;
        let signer = key
            .signer()
            .map_err(|err| IpcError::new("bad_keys", err.to_string()))?;
        let bundle = signer.bundle();
        if !accepted.is_authorized(key_id(&bundle)) {
            return Err(IpcError::new(
                "unauthorized",
                "this key is not the root key or a listed maintainer of this repo",
            ));
        }
        new_name = name.map_or(new_name, str::to_string);
        new_description = description.map_or(new_description, str::to_string);
        // Past the record being replaced, even if its clock ran ahead.
        let revision = clock.max(old.map_or(0, |o| o.revision + 1));
        let record = About {
            repo_id: *repo.as_bytes(),
            revision,
            name: new_name.clone(),
            description: new_description.clone(),
            signer_keys: bundle,
        }
        .sign(&*signer)
        .map_err(|err| IpcError::new(err.to_string(), err.to_string()))?;
        if !store.write_about(&record, &accepted, clock)? {
            return Err(IpcError::new(
                "stale",
                "a newer name or description is already stored; try again",
            ));
        }
        if let Some(net) = state.net() {
            net.announce_about(repo, record.bytes).await;
        }
    }

    let shown = |text: String| Some(text).filter(|text| !text.is_empty());
    Ok(json!({
        "repo_id": repo.to_base32(),
        "name": shown(new_name),
        "description": shown(new_description),
    }))
}
