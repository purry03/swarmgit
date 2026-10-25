//! Pure canonical-ref resolution (spec section 5.5).
//!
//! The caller supplies ancestry queries, so tests can use an in-memory DAG and
//! production can shell out to `git merge-base --is-ancestor` without this
//! module doing any I/O.

use std::collections::{BTreeMap, BTreeSet};

use crate::accept::AcceptState;
use crate::glob::any_glob_matches;
use crate::ids::{KeyId, Oid};

/// Git ancestry queries used by `resolve`.
pub trait Ancestry {
    /// `Some(true)` when `a` is an ancestor of `b`, `Some(false)` when it is
    /// not, and `None` when an object needed for the answer is missing.
    fn is_ancestor(&self, a: &Oid, b: &Oid) -> Option<bool>;
}

/// The result of resolving all authorized manifests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Ref names that have one agreed value.
    pub canonical: BTreeMap<String, Oid>,
    /// Ref names with competing tips and no fast-forward winner.
    pub conflicts: BTreeMap<String, Vec<Oid>>,
    /// Ref names whose ancestor test cannot run yet because objects are missing.
    pub pending: BTreeSet<String>,
    /// `HEAD` according to 5.5.
    pub head: Option<String>,
    /// Whether the canonical table differs from the previous one.
    pub dirty: bool,
}

fn scope_allows(state: &AcceptState, signer: KeyId, name: &str) -> bool {
    if signer == state.root {
        return true;
    }
    let globs = state.scope_for(signer);
    any_glob_matches(&globs, name)
}

fn pick_fast_forward(git: &dyn Ancestry, values: &[Oid]) -> Result<Option<Oid>, ()> {
    for candidate in values {
        let mut all_ancestors = true;
        let mut missing = false;
        for other in values {
            if other == candidate {
                continue;
            }
            match git.is_ancestor(other, candidate) {
                Some(true) => {}
                Some(false) => {
                    all_ancestors = false;
                    break;
                }
                None => {
                    missing = true;
                    break;
                }
            }
        }
        if missing {
            return Err(());
        }
        if all_ancestors {
            return Ok(Some(candidate.clone()));
        }
    }
    Ok(None)
}

/// Resolve the canonical ref table for the current accepted state.
#[must_use]
pub fn resolve(state: &AcceptState, git: &dyn Ancestry) -> Resolved {
    let mut out = Resolved::default();

    // The root manifest's tombstone list removes names for everyone.
    let tombstones: BTreeSet<String> = state
        .root_manifest()
        .and_then(|stored| stored.manifest.tombstones.as_ref())
        .map(|list| list.iter().cloned().collect())
        .unwrap_or_default();

    // Union of ref names across the latest authorized manifests.
    let mut names = BTreeSet::new();
    for (signer, stored) in &state.signers {
        if !state.is_authorized(*signer) && *signer != state.root {
            continue;
        }
        for name in stored.manifest.refs.keys() {
            names.insert(name.clone());
        }
    }

    for name in names {
        if tombstones.contains(&name) {
            continue;
        }
        let mut candidates: Vec<(KeyId, Oid)> = Vec::new();
        for (signer, stored) in &state.signers {
            if *signer != state.root && !state.is_authorized(*signer) {
                continue;
            }
            if !scope_allows(state, *signer, &name) {
                continue;
            }
            if let Some(oid) = stored.manifest.refs.get(&name) {
                candidates.push((*signer, oid.clone()));
            }
        }
        if candidates.is_empty() {
            continue;
        }
        candidates.sort_by(|a, b| a.1.as_slice().cmp(b.1.as_slice()));

        let mut values: Vec<Oid> = Vec::new();
        for (_, oid) in &candidates {
            if !values.contains(oid) {
                values.push(oid.clone());
            }
        }

        if values.len() == 1 {
            if let Some(oid) = values.first() {
                out.canonical.insert(name, oid.clone());
            }
            continue;
        }

        // Root wins when its own manifest lists this ref.
        if candidates.iter().any(|(signer, _)| *signer == state.root) {
            if let Some((_, oid)) = candidates.iter().find(|(signer, _)| *signer == state.root) {
                out.canonical.insert(name, oid.clone());
                continue;
            }
        }

        match pick_fast_forward(git, &values) {
            Ok(Some(oid)) => {
                out.canonical.insert(name, oid);
            }
            Ok(None) => {
                out.conflicts.insert(name.clone(), values);
                if let Some(previous) = state.canonical.get(&name) {
                    out.canonical.insert(name, previous.clone());
                }
            }
            Err(()) => {
                out.pending.insert(name);
            }
        }
    }

    let root_head = state
        .root_manifest()
        .and_then(|stored| stored.manifest.head.clone());
    out.head = root_head
        .filter(|head| out.canonical.contains_key(head))
        .or_else(|| {
            let main = "refs/heads/main".to_string();
            out.canonical.contains_key(&main).then_some(main)
        })
        .or_else(|| {
            out.canonical
                .keys()
                .find(|name| name.starts_with("refs/heads/"))
                .cloned()
        });

    out.dirty = out.canonical != state.canonical;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accept::{AcceptState, StoredManifest};
    use crate::crypto::key_id;
    use crate::ids::RepoId;
    use crate::manifest::Manifest;
    use crate::pq::{HybridSigner, Signer};
    use std::collections::BTreeMap;

    struct Dag(BTreeMap<(Vec<u8>, Vec<u8>), bool>);

    impl Dag {
        fn new() -> Self {
            Self(BTreeMap::new())
        }
        fn edge(&mut self, a: u8, b: u8) {
            // a is ancestor of b
            self.0.insert((vec![a], vec![b]), true);
            self.0.insert((vec![b], vec![a]), false);
        }
        fn unrelated(&mut self, a: u8, b: u8) {
            self.0.insert((vec![a], vec![b]), false);
            self.0.insert((vec![b], vec![a]), false);
        }
    }

    impl Ancestry for Dag {
        fn is_ancestor(&self, a: &Oid, b: &Oid) -> Option<bool> {
            self.0
                .get(&(a.as_slice().to_vec(), b.as_slice().to_vec()))
                .copied()
        }
    }

    fn stored(signer: &HybridSigner, seq: u64, refs: &[(&str, u8)]) -> StoredManifest {
        let mut map = BTreeMap::new();
        for (name, byte) in refs {
            map.insert((*name).to_string(), Oid::new(vec![*byte]));
        }
        let manifest = Manifest {
            version: 1,
            repo_id: *key_id(&signer.bundle()).as_bytes(),
            signer: *key_id(&signer.bundle()).as_bytes(),
            seq,
            prev: [0u8; 32],
            ts: 1,
            refs: map,
            head: None,
            maintainers: None,
            object_format: Some("sha256".to_string()),
            tombstones: None,
            forked_from: None,
            recovery: None,
            new_root: None,
            signer_keys: signer.bundle(),
            suite: 1,
        };
        StoredManifest {
            manifest,
            manifest_bytes: Vec::new(),
            signature: Vec::new(),
            envelope: Vec::new(),
            manifest_id: [0u8; 32],
        }
    }

    #[test]
    fn single_signer_copies_refs() {
        let signer = HybridSigner::from_seeds(&[1u8; 32], &[2u8; 32]).unwrap();
        let mut state = AcceptState::new(RepoId::from_bytes(*key_id(&signer.bundle()).as_bytes()));
        state.signers.insert(
            KeyId::from_bytes(*key_id(&signer.bundle()).as_bytes()),
            stored(&signer, 1, &[("refs/heads/main", 1), ("refs/tags/v1", 2)]),
        );
        let resolved = resolve(&state, &Dag::new());
        assert_eq!(resolved.canonical.len(), 2);
        assert_eq!(resolved.head.as_deref(), Some("refs/heads/main"));
        assert!(resolved.conflicts.is_empty());
        assert!(resolved.pending.is_empty());
    }

    #[test]
    fn fast_forward_chain_picks_descendant() {
        let root = HybridSigner::from_seeds(&[3u8; 32], &[4u8; 32]).unwrap();
        let maint_a = HybridSigner::from_seeds(&[5u8; 32], &[6u8; 32]).unwrap();
        let maint_b = HybridSigner::from_seeds(&[19u8; 32], &[20u8; 32]).unwrap();
        let repo = RepoId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let mut state = AcceptState::new(repo);
        let root_id = KeyId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let maint_a_id = KeyId::from_bytes(*key_id(&maint_a.bundle()).as_bytes());
        let maint_b_id = KeyId::from_bytes(*key_id(&maint_b.bundle()).as_bytes());
        let mut root_stored = stored(&root, 1, &[]);
        root_stored.manifest.maintainers = Some(vec![
            crate::manifest::Maintainer {
                key: maint_a_id,
                globs: vec!["refs/heads/**".to_string()],
            },
            crate::manifest::Maintainer {
                key: maint_b_id,
                globs: vec!["refs/heads/**".to_string()],
            },
        ]);
        state.signers.insert(root_id, root_stored);
        state
            .signers
            .insert(maint_a_id, stored(&maint_a, 1, &[("refs/heads/main", 1)]));
        state
            .signers
            .insert(maint_b_id, stored(&maint_b, 1, &[("refs/heads/main", 2)]));
        let mut dag = Dag::new();
        dag.edge(1, 2);
        let resolved = resolve(&state, &dag);
        assert_eq!(
            resolved.canonical.get("refs/heads/main"),
            Some(&Oid::new(vec![2]))
        );
        assert!(resolved.conflicts.is_empty());
    }

    #[test]
    fn missing_objects_mark_pending() {
        let root = HybridSigner::from_seeds(&[7u8; 32], &[8u8; 32]).unwrap();
        let maint_a = HybridSigner::from_seeds(&[9u8; 32], &[10u8; 32]).unwrap();
        let maint_b = HybridSigner::from_seeds(&[21u8; 32], &[22u8; 32]).unwrap();
        let repo = RepoId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let mut state = AcceptState::new(repo);
        let root_id = KeyId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let maint_a_id = KeyId::from_bytes(*key_id(&maint_a.bundle()).as_bytes());
        let maint_b_id = KeyId::from_bytes(*key_id(&maint_b.bundle()).as_bytes());
        let mut root_stored = stored(&root, 1, &[]);
        root_stored.manifest.maintainers = Some(vec![
            crate::manifest::Maintainer {
                key: maint_a_id,
                globs: vec!["refs/heads/**".to_string()],
            },
            crate::manifest::Maintainer {
                key: maint_b_id,
                globs: vec!["refs/heads/**".to_string()],
            },
        ]);
        state.signers.insert(root_id, root_stored);
        state
            .signers
            .insert(maint_a_id, stored(&maint_a, 1, &[("refs/heads/main", 1)]));
        state
            .signers
            .insert(maint_b_id, stored(&maint_b, 1, &[("refs/heads/main", 2)]));
        let resolved = resolve(&state, &Dag::new());
        assert!(resolved.pending.contains("refs/heads/main"));
        assert!(!resolved.canonical.contains_key("refs/heads/main"));
    }

    #[test]
    fn divergence_keeps_previous_value() {
        let root = HybridSigner::from_seeds(&[11u8; 32], &[12u8; 32]).unwrap();
        let maint_a = HybridSigner::from_seeds(&[13u8; 32], &[14u8; 32]).unwrap();
        let maint_b = HybridSigner::from_seeds(&[17u8; 32], &[18u8; 32]).unwrap();
        let repo = RepoId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let mut state = AcceptState::new(repo);
        let root_id = KeyId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let maint_a_id = KeyId::from_bytes(*key_id(&maint_a.bundle()).as_bytes());
        let maint_b_id = KeyId::from_bytes(*key_id(&maint_b.bundle()).as_bytes());
        let mut root_stored = stored(&root, 1, &[]);
        root_stored.manifest.maintainers = Some(vec![
            crate::manifest::Maintainer {
                key: maint_a_id,
                globs: vec!["refs/heads/**".to_string()],
            },
            crate::manifest::Maintainer {
                key: maint_b_id,
                globs: vec!["refs/heads/**".to_string()],
            },
        ]);
        state.signers.insert(root_id, root_stored);
        state
            .signers
            .insert(maint_a_id, stored(&maint_a, 1, &[("refs/heads/main", 1)]));
        state
            .signers
            .insert(maint_b_id, stored(&maint_b, 1, &[("refs/heads/main", 2)]));
        state
            .canonical
            .insert("refs/heads/main".to_string(), Oid::new(vec![9]));
        let mut dag = Dag::new();
        dag.unrelated(1, 2);
        let resolved = resolve(&state, &dag);
        assert_eq!(
            resolved.canonical.get("refs/heads/main"),
            Some(&Oid::new(vec![9]))
        );
        assert_eq!(
            resolved.conflicts.get("refs/heads/main").map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn tombstone_removes_ref() {
        let root = HybridSigner::from_seeds(&[15u8; 32], &[16u8; 32]).unwrap();
        let repo = RepoId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let mut state = AcceptState::new(repo);
        let root_id = KeyId::from_bytes(*key_id(&root.bundle()).as_bytes());
        let mut root_stored = stored(&root, 2, &[("refs/heads/main", 1), ("refs/heads/old", 2)]);
        root_stored.manifest.tombstones = Some(vec!["refs/heads/old".to_string()]);
        state.signers.insert(root_id, root_stored);
        let resolved = resolve(&state, &Dag::new());
        assert!(resolved.canonical.contains_key("refs/heads/main"));
        assert!(!resolved.canonical.contains_key("refs/heads/old"));
    }
}
