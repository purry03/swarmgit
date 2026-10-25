//! Security and manifest tests from spec section 12 (SEC-1, SEC-14, SEC-15).

use std::collections::BTreeMap;

use swarm_core::accept::{AcceptOutcome, AcceptState};
use swarm_core::crypto::key_id;
use swarm_core::manifest::{encode_envelope, verify_envelope, Manifest};
use swarm_core::pq::{HybridSigner, Signer};
use swarm_core::{ErrorCode, Oid, RepoId};

fn signer() -> HybridSigner {
    HybridSigner::from_seeds(&[11u8; 32], &[12u8; 32]).unwrap()
}

fn root_manifest(signer: &HybridSigner) -> Manifest {
    let mut refs = BTreeMap::new();
    refs.insert("refs/heads/main".to_string(), Oid::new(vec![7u8; 32]));
    Manifest {
        version: 1,
        repo_id: *key_id(&signer.bundle()).as_bytes(),
        signer: *key_id(&signer.bundle()).as_bytes(),
        seq: 1,
        prev: [0u8; 32],
        ts: 1,
        refs,
        head: Some("refs/heads/main".to_string()),
        maintainers: None,
        object_format: Some("sha256".to_string()),
        tombstones: None,
        forked_from: None,
        recovery: None,
        new_root: None,
        signer_keys: signer.bundle(),
        suite: 1,
    }
}

#[test]
fn encode_decode_round_trip() {
    let signer = signer();
    let manifest = root_manifest(&signer);
    let bytes = manifest.encode().unwrap();
    assert_eq!(Manifest::decode(&bytes).unwrap(), manifest);
    let envelope = manifest.sign(&signer).unwrap();
    let verified = verify_envelope(&envelope.envelope).unwrap();
    assert_eq!(verified.manifest, manifest);
}

#[test]
fn sec1_flipping_any_envelope_byte_fails() {
    let signer = signer();
    let manifest = root_manifest(&signer);
    let envelope = manifest.sign(&signer).unwrap();
    for offset in 0..envelope.envelope.len() {
        let mut bytes = envelope.envelope.clone();
        bytes[offset] ^= 1;
        assert!(
            verify_envelope(&bytes).is_err(),
            "byte {offset} was accepted"
        );
    }
}

#[test]
fn sec14_both_halves_are_required() {
    let signer = signer();
    let manifest = root_manifest(&signer);
    let envelope = manifest.sign(&signer).unwrap();
    let mut ed_flipped = envelope.signature.clone();
    ed_flipped[0] ^= 1;
    let bytes = encode_envelope(&envelope.manifest_bytes, &ed_flipped).unwrap();
    assert_eq!(
        verify_envelope(&bytes).unwrap_err(),
        ErrorCode::BadSignature
    );
    let mut ml_zeroed = envelope.signature.clone();
    ml_zeroed[64..].fill(0);
    let bytes = encode_envelope(&envelope.manifest_bytes, &ml_zeroed).unwrap();
    assert_eq!(
        verify_envelope(&bytes).unwrap_err(),
        ErrorCode::BadSignature
    );

    let mut ml_flipped = envelope.signature.clone();
    ml_flipped[64] ^= 1;
    let bytes = encode_envelope(&envelope.manifest_bytes, &ml_flipped).unwrap();
    assert_eq!(
        verify_envelope(&bytes).unwrap_err(),
        ErrorCode::BadSignature
    );

    let mut ed_zeroed = envelope.signature.clone();
    ed_zeroed[..64].fill(0);
    let bytes = encode_envelope(&envelope.manifest_bytes, &ed_zeroed).unwrap();
    assert_eq!(
        verify_envelope(&bytes).unwrap_err(),
        ErrorCode::BadSignature
    );
}

#[test]
fn sec15_bad_keys_and_unknown_suite() {
    let signer = signer();
    let mut manifest = root_manifest(&signer);
    manifest.signer_keys[100] ^= 1;
    let bytes = manifest.encode().unwrap();
    assert_eq!(
        verify_envelope(&encode_envelope(&bytes, &[0; 3373]).unwrap()).unwrap_err(),
        ErrorCode::BadKeys
    );

    let mut manifest = root_manifest(&signer);
    manifest.signer_keys.pop();
    let bytes = manifest.encode().unwrap();
    assert_eq!(
        verify_envelope(&encode_envelope(&bytes, &[0; 3373]).unwrap()).unwrap_err(),
        ErrorCode::BadKeys
    );

    let mut manifest = root_manifest(&signer);
    manifest.suite = 9;
    let bytes = manifest.encode().unwrap();
    assert_eq!(
        verify_envelope(&encode_envelope(&bytes, &[0; 3373]).unwrap()).unwrap_err(),
        ErrorCode::UnknownSuite
    );
}

#[test]
fn reject_non_canonical_manifest_bytes() {
    let signer = signer();
    let manifest = root_manifest(&signer);
    let mut bytes = manifest.encode().unwrap();
    // Re-encode the leading map with a non-shortest length encoding.
    bytes[0] = 0xb8;
    bytes.insert(1, 9);
    assert_eq!(
        Manifest::decode(&bytes).unwrap_err(),
        ErrorCode::NonCanonical
    );
}

#[test]
fn accept_is_monotonic_and_records_equivocation() {
    let signer = signer();
    let manifest = root_manifest(&signer);
    let envelope = manifest.sign(&signer).unwrap();
    let mut state = AcceptState::new(RepoId::from_bytes(manifest.repo_id));
    assert!(matches!(
        state.accept(envelope.clone()).unwrap(),
        AcceptOutcome::Accepted { seq: 1, .. }
    ));
    assert!(matches!(
        state.accept(envelope.clone()).unwrap(),
        AcceptOutcome::Duplicate
    ));

    let mut next = manifest.clone();
    next.seq = 2;
    next.prev = envelope.manifest_id;
    next.refs.clear();
    let next = next.sign(&signer).unwrap();
    assert!(matches!(
        state.accept(next.clone()).unwrap(),
        AcceptOutcome::Accepted { seq: 2, .. }
    ));
    assert_eq!(
        state.accept(envelope.clone()).unwrap_err(),
        ErrorCode::Stale
    );

    let mut conflicting = manifest.clone();
    conflicting.seq = 2;
    conflicting.prev = envelope.manifest_id;
    conflicting.refs.clear();
    conflicting.ts = 99;
    let conflicting = conflicting.sign(&signer).unwrap();
    assert_eq!(
        state.accept(conflicting).unwrap(),
        AcceptOutcome::Equivocation
    );
    assert_eq!(state.equivocations.len(), 1);
}
