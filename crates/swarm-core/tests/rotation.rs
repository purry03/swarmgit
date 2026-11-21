//! Recovery-key rotation (spec 11.9): the recovery key moves the root, the
//! old root is rejected afterwards, and nobody else can rotate.

use std::collections::BTreeMap;

use swarm_core::accept::AcceptState;
use swarm_core::crypto::{key_id, repo_id};
use swarm_core::manifest::{Manifest, VerifiedEnvelope};
use swarm_core::pq::{HybridSigner, RecoverySigner, Signer};
use swarm_core::{ErrorCode, KeyId};

struct Keys {
    root: HybridSigner,
    recovery: RecoverySigner,
    next: HybridSigner,
}

fn manifest(signer: &dyn Signer, repo: [u8; 32], seq: u64, prev: [u8; 32]) -> Manifest {
    Manifest {
        version: 1,
        repo_id: repo,
        signer: *key_id(&signer.bundle()).as_bytes(),
        seq,
        prev,
        ts: 1,
        refs: BTreeMap::new(),
        head: None,
        maintainers: None,
        object_format: None,
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: None,
        signer_keys: signer.bundle(),
        suite: u64::from(signer.suite()),
    }
}

fn root_with_recovery(keys: &Keys, repo: [u8; 32]) -> VerifiedEnvelope {
    let mut m = manifest(&keys.root, repo, 1, [0; 32]);
    m.object_format = Some("sha1".into());
    m.recovery = Some(*key_id(&keys.recovery.bundle()).as_bytes());
    m.sign(&keys.root).unwrap()
}

fn rotation(keys: &Keys, repo: [u8; 32]) -> VerifiedEnvelope {
    let mut m = manifest(&keys.recovery, repo, 1, [0; 32]);
    m.new_root = Some(*key_id(&keys.next.bundle()).as_bytes());
    m.sign(&keys.recovery).unwrap()
}

fn setup() -> (Keys, [u8; 32], AcceptState) {
    let keys = Keys {
        root: HybridSigner::generate().unwrap(),
        recovery: RecoverySigner::generate().unwrap(),
        next: HybridSigner::generate().unwrap(),
    };
    let repo = *repo_id(&keys.root.bundle()).as_bytes();
    let mut state = AcceptState::new(repo.into());
    state.accept(root_with_recovery(&keys, repo)).unwrap();
    (keys, repo, state)
}

#[test]
fn recovery_manifest_moves_the_root() {
    let (keys, repo, mut state) = setup();
    state.accept(rotation(&keys, repo)).unwrap();
    assert_eq!(state.root, key_id(&keys.next.bundle()));

    // The new root publishes; the old root is now rejected.
    let mut m = manifest(&keys.next, repo, 1, [0; 32]);
    m.object_format = Some("sha1".into());
    state.accept(m.sign(&keys.next).unwrap()).unwrap();
    let old = manifest(&keys.root, repo, 2, [0; 32]);
    let err = state.accept(old.sign(&keys.root).unwrap()).unwrap_err();
    assert_eq!(err, ErrorCode::Unauthorized);
}

#[test]
fn a_rotation_signed_by_anyone_else_is_rejected() {
    let (keys, repo, mut state) = setup();
    // The root key itself cannot rotate (it would have to be the recovery key).
    let mut by_root = manifest(&keys.root, repo, 2, [0; 32]);
    by_root.new_root = Some(*key_id(&keys.next.bundle()).as_bytes());
    // The signing layer already refuses it, and `accept` would too.
    match by_root.sign(&keys.root) {
        Err(code) => assert_eq!(code, ErrorCode::UnknownSuite),
        Ok(envelope) => assert_eq!(
            state.accept(envelope).unwrap_err(),
            ErrorCode::ForbiddenField
        ),
    }
    // A different suite-2 key is not the recovery key.
    let stranger = RecoverySigner::generate().unwrap();
    let mut m = manifest(&stranger, repo, 1, [0; 32]);
    m.new_root = Some(*key_id(&keys.next.bundle()).as_bytes());
    assert_eq!(
        state.accept(m.sign(&stranger).unwrap()).unwrap_err(),
        ErrorCode::Unauthorized
    );
    assert_eq!(state.root, KeyId::from_bytes(repo));
}

#[test]
fn rotation_needs_a_recovery_key_in_the_root_manifest() {
    let keys = Keys {
        root: HybridSigner::generate().unwrap(),
        recovery: RecoverySigner::generate().unwrap(),
        next: HybridSigner::generate().unwrap(),
    };
    let repo = *repo_id(&keys.root.bundle()).as_bytes();
    let mut state = AcceptState::new(repo.into());
    let mut m = manifest(&keys.root, repo, 1, [0; 32]);
    m.object_format = Some("sha1".into());
    state.accept(m.sign(&keys.root).unwrap()).unwrap();
    assert_eq!(
        state.accept(rotation(&keys, repo)).unwrap_err(),
        ErrorCode::Unauthorized
    );
}

#[test]
fn batch_order_does_not_matter() {
    let (keys, repo, _) = setup();
    let mut next = manifest(&keys.next, repo, 1, [0; 32]);
    next.object_format = Some("sha1".into());
    let mut state = AcceptState::new(repo.into());
    let outcomes = state.accept_batch(vec![
        next.sign(&keys.next).unwrap(),
        rotation(&keys, repo),
        root_with_recovery(&keys, repo),
    ]);
    assert!(outcomes.iter().all(Result::is_ok), "{outcomes:?}");
    assert_eq!(state.root, key_id(&keys.next.bundle()));
}
