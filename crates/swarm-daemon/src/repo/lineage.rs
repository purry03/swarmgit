//! Where a repo was forked from, and which repos here were forked from it.
//! Only a fork records the link, so a source learns of its forks from the
//! copies this machine holds; the network does not list them.

use serde_json::{json, Value};
use swarm_core::ids::RepoId;
use swarm_store::RepoStore;

use super::{about_text, local_about};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// The upstream of `repo` (given what its root states) and the forks of it
/// held on this machine.
pub(super) fn of(
    state: &DaemonState,
    repo: RepoId,
    forked_from: Option<RepoId>,
) -> Result<(Option<Value>, Vec<Value>), IpcError> {
    let rows = state
        .db
        .lock()
        .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?
        .repos()
        .map_err(|err| IpcError::new("malformed", err.to_string()))?;
    let mut forks = Vec::new();
    for row in rows {
        let store = RepoStore::new(state.home.clone(), row.id);
        if row.id == repo || !store.exists() {
            continue;
        }
        // A repo too damaged to load is left out, not an error for this one.
        let source = store.load_state().ok().and_then(|accepted| {
            accepted
                .root_manifest()
                .and_then(|root| root.manifest.forked_from)
        });
        if source == Some(*repo.as_bytes()) {
            forks.push(describe(state, row.id));
        }
    }
    Ok((forked_from.map(|source| describe(state, source)), forks))
}

/// A related repo, with its name if this machine knows it.
fn describe(state: &DaemonState, repo: RepoId) -> Value {
    let about = local_about(state, repo);
    json!({
        "repoId": repo.to_base32(),
        "link": format!("swarm://{}", repo.to_base32()),
        "name": about_text(&about, |about| &about.name),
    })
}
