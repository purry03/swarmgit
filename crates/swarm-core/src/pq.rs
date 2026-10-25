//! Hybrid post-quantum primitives (spec section 4.7).
//!
//! Suite 1 is Ed25519 plus ML-DSA-65. Suite 2 is SLH-DSA-SHAKE-128s and is
//! reserved for recovery keys. The `verify` function requires both halves of a
//! suite 1 signature; there is no classical-only path.

use std::fmt;

use ed25519_dalek::{
    Signature as EdSignature, Signer as _, SigningKey as EdSigningKey,
    VerifyingKey as EdVerifyingKey,
};
use ml_dsa::{
    EncodedVerifyingKey as MlDsaEncodedVerifyingKey, Keypair as _, MlDsa65,
    Signature as MlDsaSignature, SigningKey as MlDsaSigningKey, Verifier as _,
    VerifyingKey as MlDsaVerifyingKey,
};
use ml_kem::kem::{Decapsulate as _, Encapsulate as _, KeyExport as _};
use ml_kem::{MlKem768, Seed as MlKemSeed};
use rand_core::{TryCryptoRng, TryRng};
use slh_dsa::{
    Shake128s, Signature as SlhSignature, SigningKey as SlhSigningKey,
    VerifyingKey as SlhVerifyingKey,
};
use thiserror::Error;

use crate::consts::{
    SUITE1_BUNDLE_LEN, SUITE1_SIG_LEN, SUITE2_BUNDLE_LEN, SUITE2_SIG_LEN, SUITE_HYBRID,
    SUITE_RECOVERY,
};
use crate::crypto::CryptoError;

/// Length of a public key bundle for a suite.
#[must_use]
pub const fn bundle_len(suite: u8) -> Option<usize> {
    match suite {
        SUITE_HYBRID => Some(SUITE1_BUNDLE_LEN),
        SUITE_RECOVERY => Some(SUITE2_BUNDLE_LEN),
        _ => None,
    }
}

/// Length of a signature for a suite.
#[must_use]
pub const fn signature_len(suite: u8) -> Option<usize> {
    match suite {
        SUITE_HYBRID => Some(SUITE1_SIG_LEN),
        SUITE_RECOVERY => Some(SUITE2_SIG_LEN),
        _ => None,
    }
}

/// A signer that produces a full hybrid signature.
pub trait Signer: Send + Sync {
    /// Suite number.
    fn suite(&self) -> u8;
    /// Public key bundle in the fixed suite layout.
    fn bundle(&self) -> Vec<u8>;
    /// Sign `msg`. Returns an empty vector only if the OS RNG failed, which
    /// makes the resulting signature invalid rather than weak.
    fn sign(&self, msg: &[u8]) -> Vec<u8>;
    /// Fallible signing used by manifest creation.
    fn try_sign(&self, msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Ok(self.sign(msg))
    }
}

/// Errors from the random source adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("OS randomness unavailable")]
pub struct OsRngError;

struct OsRng;

impl TryRng for OsRng {
    type Error = OsRngError;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| OsRngError)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|_| OsRngError)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        getrandom::fill(dst).map_err(|_| OsRngError)
    }
}

impl TryCryptoRng for OsRng {}

/// Suite 1 signer: Ed25519 and ML-DSA-65 over the same message.
pub struct HybridSigner {
    ed: EdSigningKey,
    ml: MlDsaSigningKey<MlDsa65>,
}

impl HybridSigner {
    /// Build from the two 32-byte seeds stored in a key file.
    pub fn from_seeds(ed_seed: &[u8; 32], ml_seed: &[u8; 32]) -> Result<Self, CryptoError> {
        let ml_seed = ml_dsa::Seed::from(*ml_seed);
        Ok(Self {
            ed: EdSigningKey::from_bytes(ed_seed),
            ml: MlDsaSigningKey::from_seed(&ml_seed),
        })
    }

    /// Generate fresh seeds from the OS RNG.
    pub fn generate() -> Result<Self, CryptoError> {
        let ed_seed: [u8; 32] = crate::crypto::random_bytes()?;
        let ml_seed: [u8; 32] = crate::crypto::random_bytes()?;
        Self::from_seeds(&ed_seed, &ml_seed)
    }

    /// The two secret seeds, in key-file order.
    #[must_use]
    pub fn seeds(&self) -> ([u8; 32], [u8; 32]) {
        let mut ml_seed = [0u8; 32];
        ml_seed.copy_from_slice(self.ml.to_seed().as_slice());
        (self.ed.to_bytes(), ml_seed)
    }

    fn bundle(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SUITE1_BUNDLE_LEN);
        out.extend_from_slice(self.ed.verifying_key().as_bytes());
        out.extend_from_slice(self.ml.verifying_key().encode().as_slice());
        out
    }
}

impl Signer for HybridSigner {
    fn suite(&self) -> u8 {
        SUITE_HYBRID
    }

    fn bundle(&self) -> Vec<u8> {
        Self::bundle(self)
    }

    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.try_sign(msg).unwrap_or_default()
    }

    fn try_sign(&self, msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let ed_sig = self.ed.sign(msg).to_bytes();
        let ml_sig = self
            .ml
            .expanded_key()
            .sign_randomized(msg, &[], &mut OsRng)
            .map_err(|_| CryptoError::Random)?;
        let mut out = Vec::with_capacity(SUITE1_SIG_LEN);
        out.extend_from_slice(&ed_sig);
        out.extend_from_slice(ml_sig.encode().as_slice());
        Ok(out)
    }
}

/// Suite 2 signer: SLH-DSA-SHAKE-128s, recovery use only.
pub struct RecoverySigner {
    key: SlhSigningKey<Shake128s>,
}

impl RecoverySigner {
    /// Load a recovery secret key from its serialized form.
    pub fn from_secret(bytes: &[u8]) -> Result<Self, CryptoError> {
        let key = SlhSigningKey::<Shake128s>::try_from(bytes).map_err(|_| CryptoError::BadKey)?;
        Ok(Self { key })
    }

    /// Generate a fresh recovery key.
    pub fn generate() -> Result<Self, CryptoError> {
        let sk_seed: [u8; 16] = crate::crypto::random_bytes()?;
        let sk_prf: [u8; 16] = crate::crypto::random_bytes()?;
        let pk_seed: [u8; 16] = crate::crypto::random_bytes()?;
        let key = SlhSigningKey::<Shake128s>::slh_keygen_internal(&sk_seed, &sk_prf, &pk_seed);
        Ok(Self { key })
    }

    /// Serialized secret key bytes for a key file.
    #[must_use]
    pub fn secret(&self) -> Vec<u8> {
        self.key.to_vec()
    }
}

impl Signer for RecoverySigner {
    fn suite(&self) -> u8 {
        SUITE_RECOVERY
    }

    fn bundle(&self) -> Vec<u8> {
        self.key.as_ref().to_bytes().to_vec()
    }

    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.try_sign(msg).unwrap_or_default()
    }

    fn try_sign(&self, msg: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let randomizer: [u8; 16] = crate::crypto::random_bytes()?;
        let sig = self
            .key
            .try_sign_with_context(msg, &[], Some(&randomizer))
            .map_err(|_| CryptoError::BadSignature)?;
        Ok(sig.to_vec())
    }
}

/// Verify a hybrid signature. Both halves must verify for suite 1.
///
/// The caller must also check that `key_id(bundle)` equals the manifest's
/// `signer` field.
pub fn verify(suite: u8, bundle: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), CryptoError> {
    match suite {
        SUITE_HYBRID => verify_suite1(bundle, msg, sig),
        SUITE_RECOVERY => verify_suite2(bundle, msg, sig),
        _ => Err(CryptoError::BadKey),
    }
}

fn verify_suite1(bundle: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), CryptoError> {
    if bundle.len() != SUITE1_BUNDLE_LEN || sig.len() != SUITE1_SIG_LEN {
        return Err(CryptoError::BadLength);
    }
    let ed_key_bytes: [u8; 32] = bundle[..32].try_into().map_err(|_| CryptoError::BadKey)?;
    let ed_key = EdVerifyingKey::from_bytes(&ed_key_bytes).map_err(|_| CryptoError::BadKey)?;
    let ed_sig = EdSignature::from_slice(&sig[..64]).map_err(|_| CryptoError::BadSignature)?;
    ed_key
        .verify_strict(msg, &ed_sig)
        .map_err(|_| CryptoError::BadSignature)?;

    let encoded = MlDsaEncodedVerifyingKey::<MlDsa65>::try_from(&bundle[32..])
        .map_err(|_| CryptoError::BadKey)?;
    let ml_key = MlDsaVerifyingKey::<MlDsa65>::decode(&encoded);
    let ml_sig =
        MlDsaSignature::<MlDsa65>::try_from(&sig[64..]).map_err(|_| CryptoError::BadSignature)?;
    ml_key
        .verify(msg, &ml_sig)
        .map_err(|_| CryptoError::BadSignature)
}

fn verify_suite2(bundle: &[u8], msg: &[u8], sig: &[u8]) -> Result<(), CryptoError> {
    if bundle.len() != SUITE2_BUNDLE_LEN || sig.len() != SUITE2_SIG_LEN {
        return Err(CryptoError::BadLength);
    }
    let key = SlhVerifyingKey::<Shake128s>::try_from(bundle).map_err(|_| CryptoError::BadKey)?;
    let sig = SlhSignature::<Shake128s>::try_from(sig).map_err(|_| CryptoError::BadSignature)?;
    key.try_verify_with_context(msg, &[], &sig)
        .map_err(|_| CryptoError::BadSignature)
}

/// A KEM encapsulation key.
#[derive(Clone, PartialEq, Eq)]
pub struct EncapKey(pub Vec<u8>);

impl fmt::Debug for EncapKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EncapKey").field(&self.0.len()).finish()
    }
}

/// A KEM decapsulation key (the 64-byte seed form).
#[derive(Clone, PartialEq, Eq)]
pub struct DecapKey(pub Vec<u8>);

impl fmt::Debug for DecapKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DecapKey").field(&self.0.len()).finish()
    }
}

/// A KEM ciphertext.
#[derive(Clone, PartialEq, Eq)]
pub struct Ciphertext(pub Vec<u8>);

impl fmt::Debug for Ciphertext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Ciphertext").field(&self.0.len()).finish()
    }
}

/// KEM interface from spec 4.7.
pub trait Kem {
    /// Generate a key pair.
    fn keygen(&self) -> Result<(EncapKey, DecapKey), CryptoError>;
    /// Encapsulate to an encapsulation key.
    fn encaps(&self, ek: &EncapKey) -> Result<(Ciphertext, [u8; 32]), CryptoError>;
    /// Decapsulate a ciphertext.
    fn decaps(&self, dk: &DecapKey, ct: &Ciphertext) -> Result<[u8; 32], CryptoError>;
}

/// ML-KEM-768 implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct MlKem768Kem;

impl Kem for MlKem768Kem {
    fn keygen(&self) -> Result<(EncapKey, DecapKey), CryptoError> {
        use ml_kem::kem::Kem as _;
        let (dk, ek) = MlKem768::generate_keypair();
        Ok((
            EncapKey(ek.to_bytes().to_vec()),
            DecapKey(dk.to_bytes().to_vec()),
        ))
    }

    fn encaps(&self, ek: &EncapKey) -> Result<(Ciphertext, [u8; 32]), CryptoError> {
        let key = ml_kem::kem::Key::<ml_kem::EncapsulationKey<MlKem768>>::try_from(ek.0.as_slice())
            .map_err(|_| CryptoError::BadKey)?;
        let key =
            ml_kem::EncapsulationKey::<MlKem768>::new(&key).map_err(|_| CryptoError::BadKey)?;
        let (ct, shared) = key.encapsulate();
        Ok((Ciphertext(ct.to_vec()), shared.into()))
    }

    fn decaps(&self, dk: &DecapKey, ct: &Ciphertext) -> Result<[u8; 32], CryptoError> {
        let key = match dk.0.len() {
            64 => {
                let seed = MlKemSeed::try_from(dk.0.as_slice()).map_err(|_| CryptoError::BadKey)?;
                ml_kem::DecapsulationKey::<MlKem768>::from_seed(seed)
            }
            2400 => {
                #[allow(deprecated)]
                let expanded =
                    ml_kem::ExpandedDecapsulationKey::<MlKem768>::try_from(dk.0.as_slice())
                        .map_err(|_| CryptoError::BadKey)?;
                #[allow(deprecated)]
                ml_kem::DecapsulationKey::<MlKem768>::from_expanded(&expanded)
                    .map_err(|_| CryptoError::BadKey)?
            }
            _ => return Err(CryptoError::BadKey),
        };
        let ct = ml_kem::kem::Ciphertext::<MlKem768>::try_from(ct.0.as_slice())
            .map_err(|_| CryptoError::BadKey)?;
        Ok(key.decapsulate(&ct).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::key_id;

    #[test]
    fn suite1_bundle_and_signature_lengths() {
        let signer = HybridSigner::from_seeds(&[1u8; 32], &[2u8; 32]).unwrap();
        assert_eq!(signer.bundle().len(), SUITE1_BUNDLE_LEN);
        let sig = signer.sign(b"hello");
        assert_eq!(sig.len(), SUITE1_SIG_LEN);
        assert!(verify(SUITE_HYBRID, &signer.bundle(), b"hello", &sig).is_ok());
        assert!(verify(SUITE_HYBRID, &signer.bundle(), b"hell0", &sig).is_err());
        assert_eq!(key_id(&signer.bundle()), key_id(&signer.bundle()));
    }

    #[test]
    fn suite1_rejects_either_half_flipped() {
        let signer = HybridSigner::from_seeds(&[3u8; 32], &[4u8; 32]).unwrap();
        let bundle = signer.bundle();
        let sig = signer.sign(b"msg");
        let mut ed_flipped = sig.clone();
        ed_flipped[0] ^= 1;
        assert!(verify(SUITE_HYBRID, &bundle, b"msg", &ed_flipped).is_err());
        let mut ml_flipped = sig.clone();
        ml_flipped[64] ^= 1;
        assert!(verify(SUITE_HYBRID, &bundle, b"msg", &ml_flipped).is_err());
        let mut zero_ml = sig.clone();
        zero_ml[64..].fill(0);
        assert!(verify(SUITE_HYBRID, &bundle, b"msg", &zero_ml).is_err());
    }

    #[test]
    fn ml_kem_round_trip() {
        let kem = MlKem768Kem;
        let (ek, dk) = kem.keygen().unwrap();
        assert_eq!(ek.0.len(), 1184);
        assert_eq!(dk.0.len(), 64);
        let (ct, sent) = kem.encaps(&ek).unwrap();
        assert_eq!(ct.0.len(), 1088);
        let received = kem.decaps(&dk, &ct).unwrap();
        assert_eq!(sent, received);
    }
}
