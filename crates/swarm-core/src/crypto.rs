//! Fixed cryptographic formulas from spec section 4.3.
//!
//! All values are derived here so the rest of the crate never invents a
//! domain separator or a byte order.

use blake3::Hasher;
use thiserror::Error;

use crate::consts::{
    CTX_INTEREST, CTX_KEYID, CTX_LAN, CTX_NODEAUTH, CTX_SESSION, CTX_SIG_MANIFEST, CTX_TOPIC,
};
use crate::ids::{KeyId, NodeId, RepoId};

/// Errors from cryptographic helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CryptoError {
    /// The operating system did not provide randomness.
    #[error("randomness unavailable")]
    Random,
    /// A key or signature has the wrong length or encoding.
    #[error("bad key or signature encoding")]
    BadKey,
    /// A signature did not verify.
    #[error("bad signature")]
    BadSignature,
    /// A caller supplied an invalid length.
    #[error("bad length")]
    BadLength,
}

/// Fill `N` bytes from the OS RNG.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|_| CryptoError::Random)?;
    Ok(out)
}

/// BLAKE3-256 of `data`.
#[must_use]
pub fn hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// BLAKE3 derive-key with a domain-separation context.
#[must_use]
pub fn derive_key(context: &str, material: &[u8]) -> [u8; 32] {
    blake3::derive_key(context, material)
}

/// BLAKE3 keyed hash.
#[must_use]
pub fn keyed_hash(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(key, data).as_bytes()
}

/// `key_id(bundle) = blake3_derive_key(CTX_KEYID, bundle)`.
#[must_use]
pub fn key_id(bundle: &[u8]) -> KeyId {
    KeyId::from_bytes(derive_key(CTX_KEYID, bundle))
}

/// The repo ID is the key ID of the root key bundle.
#[must_use]
pub fn repo_id(root_bundle: &[u8]) -> RepoId {
    RepoId::from_bytes(*key_id(root_bundle).as_bytes())
}

/// The node ID is the key ID of the node key bundle.
#[must_use]
pub fn node_id(node_bundle: &[u8]) -> NodeId {
    NodeId::from_bytes(*key_id(node_bundle).as_bytes())
}

/// Bytes signed by a manifest signature.
#[must_use]
pub fn sig_input(manifest: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CTX_SIG_MANIFEST.len() + manifest.len());
    out.extend_from_slice(CTX_SIG_MANIFEST);
    out.extend_from_slice(manifest);
    out
}

/// `manifest_id(envelope) = blake3(envelope)`.
#[must_use]
pub fn manifest_id(envelope: &[u8]) -> [u8; 32] {
    hash(envelope)
}

/// UTC day number.
#[must_use]
pub const fn day(unix_seconds: u64) -> u64 {
    unix_seconds / 86_400
}

/// UTC hour number.
#[must_use]
pub const fn hour(unix_seconds: u64) -> u64 {
    unix_seconds / 3_600
}

/// DHT topic: first 20 bytes of `derive_key(CTX_TOPIC, repo || u64_be(day))`.
#[must_use]
pub fn topic(repo: &RepoId, day_number: u64) -> [u8; 20] {
    let mut material = [0u8; 40];
    material[..32].copy_from_slice(repo.as_bytes());
    material[32..].copy_from_slice(&day_number.to_be_bytes());
    let derived = derive_key(CTX_TOPIC, &material);
    let mut out = [0u8; 20];
    out.copy_from_slice(&derived[..20]);
    out
}

/// Per-repo interest key.
#[must_use]
pub fn interest_key(repo: &RepoId) -> [u8; 32] {
    derive_key(CTX_INTEREST, repo.as_bytes())
}

/// Interest token bound to a Noise handshake hash.
#[must_use]
pub fn interest(repo: &RepoId, handshake_hash: &[u8; 32]) -> [u8; 16] {
    let digest = keyed_hash(&interest_key(repo), handshake_hash);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Per-repo LAN key.
#[must_use]
pub fn lan_key(repo: &RepoId) -> [u8; 32] {
    derive_key(CTX_LAN, repo.as_bytes())
}

/// LAN token for one hour number.
#[must_use]
pub fn lan_token(repo: &RepoId, hour_number: u64) -> [u8; 16] {
    let digest = keyed_hash(&lan_key(repo), &hour_number.to_be_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Session keys from NET-5.
///
/// Returns `(initiator_to_responder, responder_to_initiator)`.
#[must_use]
pub fn session_keys(
    handshake_hash: &[u8; 32],
    r_i: &[u8; 32],
    r_r: &[u8; 32],
    shared_secret: &[u8; 32],
) -> ([u8; 32], [u8; 32]) {
    let mut hasher = Hasher::new_derive_key(CTX_SESSION);
    hasher.update(handshake_hash);
    hasher.update(r_i);
    hasher.update(r_r);
    hasher.update(shared_secret);
    let mut okm = [0u8; 64];
    hasher.finalize_xof().fill(&mut okm);
    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];
    i2r.copy_from_slice(&okm[..32]);
    r2i.copy_from_slice(&okm[32..]);
    (i2r, r2i)
}

/// Transcript input signed by `PqAuth`.
#[must_use]
pub fn auth_input(
    role: u8,
    handshake_hash: &[u8; 32],
    ek: &[u8],
    ct: &[u8],
    r_i: &[u8; 32],
    r_r: &[u8; 32],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(CTX_NODEAUTH.len() + 32 + ek.len() + ct.len() + 32 + 32 + 1);
    out.extend_from_slice(CTX_NODEAUTH);
    out.extend_from_slice(handshake_hash);
    out.extend_from_slice(ek);
    out.extend_from_slice(ct);
    out.extend_from_slice(r_i);
    out.extend_from_slice(r_r);
    out.push(role);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_and_hour_are_floor_division() {
        assert_eq!(day(0), 0);
        assert_eq!(day(86_399), 0);
        assert_eq!(day(86_400), 1);
        assert_eq!(hour(3_599), 0);
        assert_eq!(hour(3_600), 1);
    }

    #[test]
    fn topic_is_twenty_bytes_and_domain_separated() {
        let repo = RepoId::from_bytes([3u8; 32]);
        assert_eq!(topic(&repo, 1), topic(&repo, 1));
        assert_ne!(topic(&repo, 1), topic(&repo, 2));
        assert_ne!(topic(&repo, 1), topic(&RepoId::from_bytes([4u8; 32]), 1));
    }

    #[test]
    fn session_keys_change_with_every_input() {
        let hh = [1u8; 32];
        let ri = [2u8; 32];
        let rr = [3u8; 32];
        let ss = [4u8; 32];
        let base = session_keys(&hh, &ri, &rr, &ss);
        assert_ne!(base, session_keys(&[9u8; 32], &ri, &rr, &ss));
        assert_ne!(base, session_keys(&hh, &[9u8; 32], &rr, &ss));
        assert_ne!(base, session_keys(&hh, &ri, &[9u8; 32], &ss));
        assert_ne!(base, session_keys(&hh, &ri, &rr, &[9u8; 32]));
        assert_ne!(base.0, base.1);
    }

    #[test]
    fn sec17_one_bit_in_any_input_changes_both_keys() {
        let hh = [1u8; 32];
        let ri = [2u8; 32];
        let rr = [3u8; 32];
        let ss = [4u8; 32];
        let base = session_keys(&hh, &ri, &rr, &ss);
        for byte in 0..32 {
            for bit in 0..8 {
                for which in 0..4 {
                    let mut hh2 = hh;
                    let mut ri2 = ri;
                    let mut rr2 = rr;
                    let mut ss2 = ss;
                    match which {
                        0 => hh2[byte] ^= 1 << bit,
                        1 => ri2[byte] ^= 1 << bit,
                        2 => rr2[byte] ^= 1 << bit,
                        _ => ss2[byte] ^= 1 << bit,
                    }
                    let changed = session_keys(&hh2, &ri2, &rr2, &ss2);
                    assert_ne!(changed.0, base.0, "input {which} byte {byte} bit {bit}");
                    assert_ne!(changed.1, base.1, "input {which} byte {byte} bit {bit}");
                }
            }
        }
    }
}
