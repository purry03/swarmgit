//! Property tests for canonical round trips, parser robustness and resolve
//! order independence.

use std::collections::BTreeMap;

use proptest::prelude::*;
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::crypto::key_id;
use swarm_core::glob::glob_match;
use swarm_core::ids::{KeyId, Oid, RepoId};
use swarm_core::manifest::{Maintainer, Manifest};
use swarm_core::refname::is_valid_refname;
use swarm_core::resolve::{resolve, Ancestry};
use swarm_core::{verify_envelope, Link};

fn refname() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["refs/heads/", "refs/tags/"]),
        prop::collection::vec("[a-z0-9]{1,8}", 1..4),
    )
        .prop_map(|(prefix, parts)| format!("{prefix}{}", parts.join("/")))
}

fn oid32() -> impl Strategy<Value = Oid> {
    prop::array::uniform32(any::<u8>()).prop_map(|bytes| Oid::new(bytes.to_vec()))
}

fn key_bytes() -> impl Strategy<Value = [u8; 32]> {
    prop::array::uniform32(any::<u8>())
}

fn manifest() -> impl Strategy<Value = Manifest> {
    (
        key_bytes(),
        key_bytes(),
        1u64..10_000,
        key_bytes(),
        0u64..4_000_000_000,
        prop::collection::btree_map(refname(), oid32(), 0..8),
        proptest::option::of(refname()),
        proptest::option::of(prop::collection::vec(any::<bool>(), 0..4)),
        proptest::option::of(refname()),
        proptest::option::of(key_bytes()),
        prop::collection::vec(any::<u8>(), 1984..=1984),
    )
        .prop_map(
            |(
                repo_id,
                signer,
                seq,
                prev,
                ts,
                refs,
                head,
                flags,
                object_format,
                forked_from,
                signer_keys,
            )| {
                let maintainers = flags.map(|flags| {
                    flags
                        .into_iter()
                        .map(|flag| Maintainer {
                            key: KeyId::from_bytes([u8::from(flag); 32]),
                            globs: vec!["refs/heads/**".to_string()],
                        })
                        .collect()
                });
                Manifest {
                    version: 1,
                    repo_id,
                    signer,
                    seq,
                    prev,
                    ts,
                    refs,
                    head,
                    maintainers,
                    object_format: object_format
                        .map(|_| "sha256".to_string())
                        .or_else(|| Some("sha256".to_string())),
                    tombstones: None,
                    forked_from,
                    recovery: None,
                    new_root: None,
                    signer_keys,
                    suite: 1,
                }
            },
        )
}

proptest! {
    #[test]
    fn manifest_round_trip(manifest in manifest()) {
        let bytes = manifest.encode().expect("encode");
        let decoded = Manifest::decode(&bytes).expect("decode");
        prop_assert_eq!(&decoded, &manifest);
        prop_assert_eq!(decoded.encode().expect("re-encode"), bytes);
    }

    #[test]
    fn random_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = verify_envelope(&data);
        let _ = Manifest::decode(&data);
        if let Ok(text) = std::str::from_utf8(&data) {
            let _ = Link::parse(text);
            let _ = is_valid_refname(text);
            let _ = glob_match(text, text);
        }
    }

    #[test]
    fn accepted_bytes_reencode_identically(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(manifest) = Manifest::decode(&data) {
            prop_assert_eq!(manifest.encode().expect("encode"), data);
        }
    }
}

struct TotalOrder;

impl Ancestry for TotalOrder {
    fn is_ancestor(&self, a: &Oid, b: &Oid) -> Option<bool> {
        Some(a.as_slice() < b.as_slice())
    }
}

fn stored(manifest: Manifest) -> StoredManifest {
    StoredManifest {
        manifest,
        manifest_bytes: Vec::new(),
        signature: Vec::new(),
        envelope: Vec::new(),
        manifest_id: [0u8; 32],
    }
}

fn root_manifest(repo: [u8; 32], maintainers: Vec<KeyId>) -> Manifest {
    Manifest {
        version: 1,
        repo_id: repo,
        signer: repo,
        seq: 1,
        prev: [0u8; 32],
        ts: 1,
        refs: BTreeMap::new(),
        head: Some("refs/heads/main".to_string()),
        maintainers: Some(
            maintainers
                .into_iter()
                .map(|key| Maintainer {
                    key,
                    globs: vec!["refs/heads/**".to_string()],
                })
                .collect(),
        ),
        object_format: Some("sha256".to_string()),
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: None,
        signer_keys: vec![0x11; 1984],
        suite: 1,
    }
}

fn maint_manifest(repo: [u8; 32], signer: [u8; 32], oid: Oid) -> Manifest {
    let mut refs = BTreeMap::new();
    refs.insert("refs/heads/main".to_string(), oid);
    Manifest {
        version: 1,
        repo_id: repo,
        signer,
        seq: 1,
        prev: [0u8; 32],
        ts: 1,
        refs,
        head: None,
        maintainers: None,
        object_format: None,
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: None,
        signer_keys: vec![0x22; 1984],
        suite: 1,
    }
}

proptest! {
    #[test]
    fn resolve_is_independent_of_insertion_order(
        repo in key_bytes(),
        maint_a in key_bytes(),
        maint_b in key_bytes(),
        oid_a in oid32(),
        oid_b in oid32(),
        reverse in any::<bool>(),
    ) {
        prop_assume!(oid_a != oid_b);
        let expected = if oid_a.as_slice() > oid_b.as_slice() {
            oid_a.clone()
        } else {
            oid_b.clone()
        };
        let root_id = KeyId::from_bytes(repo);
        let a_id = KeyId::from_bytes(maint_a);
        let b_id = KeyId::from_bytes(maint_b);
        let entries = vec![
            (root_id, root_manifest(repo, vec![a_id, b_id])),
            (a_id, maint_manifest(repo, maint_a, oid_a)),
            (b_id, maint_manifest(repo, maint_b, oid_b)),
        ];
        let mut state = AcceptState::new(RepoId::from_bytes(repo));
        if reverse {
            for (id, manifest) in entries.into_iter().rev() {
                state.signers.insert(id, stored(manifest));
            }
        } else {
            for (id, manifest) in entries {
                state.signers.insert(id, stored(manifest));
            }
        }
        let resolved = resolve(&state, &TotalOrder);
        prop_assert_eq!(resolved.canonical.len(), 1);
        prop_assert_eq!(
            resolved.canonical.get("refs/heads/main"),
            Some(&expected)
        );
        prop_assert!(resolved.conflicts.is_empty());
        prop_assert!(resolved.pending.is_empty());
    }
}

#[test]
fn key_id_is_stable_for_a_bundle() {
    let bundle = vec![7u8; 1984];
    assert_eq!(key_id(&bundle), key_id(&bundle));
}
