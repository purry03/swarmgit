//! Key file formats from spec section 4.5.
//!
//! Core only parses and serializes bytes. Writing files, `O_EXCL` and atomic
//! renames belong to `swarm-store`.

use thiserror::Error;

use crate::consts::{MAGIC_KEY, SUITE_HYBRID, SUITE_RECOVERY};
use crate::crypto::CryptoError;
use crate::pq::{HybridSigner, RecoverySigner, Signer};

/// Size of a repo key file.
pub const REPO_KEY_FILE_LEN: usize = 69;

/// Size of a node key file.
pub const NODE_KEY_FILE_LEN: usize = 101;

/// Errors while parsing a key file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KeyFileError {
    /// Wrong total length.
    #[error("key file has the wrong length")]
    BadLength,
    /// Wrong magic bytes.
    #[error("key file has the wrong magic")]
    BadMagic,
    /// Unknown suite byte.
    #[error("key file has an unknown suite")]
    BadSuite,
    /// Group or other permission bits are set.
    #[error("key file is readable by group or others; run chmod 600")]
    LoosePermissions,
}

fn check_magic(bytes: &[u8]) -> Result<(), KeyFileError> {
    if bytes.len() < MAGIC_KEY.len() || &bytes[..MAGIC_KEY.len()] != MAGIC_KEY {
        return Err(KeyFileError::BadMagic);
    }
    Ok(())
}

/// A repo root or recovery signing key.
#[derive(Clone)]
pub struct RepoKey {
    suite: u8,
    secret: Vec<u8>,
}

impl RepoKey {
    /// Generate a fresh root key (suite 1) or recovery key (suite 2).
    pub fn generate(suite: u8) -> Result<Self, CryptoError> {
        match suite {
            SUITE_HYBRID => {
                let signer = HybridSigner::generate()?;
                let (ed, ml) = signer.seeds();
                let mut secret = Vec::with_capacity(64);
                secret.extend_from_slice(&ed);
                secret.extend_from_slice(&ml);
                Ok(Self { suite, secret })
            }
            SUITE_RECOVERY => {
                let signer = RecoverySigner::generate()?;
                Ok(Self {
                    suite,
                    secret: signer.secret(),
                })
            }
            _ => Err(CryptoError::BadKey),
        }
    }

    /// Suite number.
    #[must_use]
    pub fn suite(&self) -> u8 {
        self.suite
    }

    /// Serialize to the 69-byte key file format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(REPO_KEY_FILE_LEN);
        out.extend_from_slice(MAGIC_KEY);
        out.push(self.suite);
        out.extend_from_slice(&self.secret);
        out
    }

    /// Parse the 69-byte key file format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyFileError> {
        if bytes.len() != REPO_KEY_FILE_LEN {
            return Err(KeyFileError::BadLength);
        }
        check_magic(bytes)?;
        let suite = bytes[MAGIC_KEY.len()];
        if !matches!(suite, SUITE_HYBRID | SUITE_RECOVERY) {
            return Err(KeyFileError::BadSuite);
        }
        Ok(Self {
            suite,
            secret: bytes[MAGIC_KEY.len() + 1..].to_vec(),
        })
    }

    /// Build a signer for this key.
    pub fn signer(&self) -> Result<Box<dyn Signer>, CryptoError> {
        match self.suite {
            SUITE_HYBRID => {
                let ed: [u8; 32] = self.secret[..32]
                    .try_into()
                    .map_err(|_| CryptoError::BadLength)?;
                let ml: [u8; 32] = self.secret[32..]
                    .try_into()
                    .map_err(|_| CryptoError::BadLength)?;
                Ok(Box::new(HybridSigner::from_seeds(&ed, &ml)?))
            }
            SUITE_RECOVERY => Ok(Box::new(RecoverySigner::from_secret(&self.secret)?)),
            _ => Err(CryptoError::BadKey),
        }
    }
}

impl Drop for RepoKey {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.secret.zeroize();
    }
}

/// A per-install node key. Identifies this peer on the wire only.
#[derive(Clone)]
pub struct NodeKey {
    ed_seed: [u8; 32],
    ml_seed: [u8; 32],
    x25519_secret: [u8; 32],
}

impl NodeKey {
    /// Generate a fresh node key.
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(Self {
            ed_seed: crate::crypto::random_bytes()?,
            ml_seed: crate::crypto::random_bytes()?,
            x25519_secret: crate::crypto::random_bytes()?,
        })
    }

    /// Parse the 101-byte node key format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyFileError> {
        if bytes.len() != NODE_KEY_FILE_LEN {
            return Err(KeyFileError::BadLength);
        }
        check_magic(bytes)?;
        if bytes[MAGIC_KEY.len()] != SUITE_HYBRID {
            return Err(KeyFileError::BadSuite);
        }
        let mut offset = MAGIC_KEY.len() + 1;
        let mut take = || -> Result<[u8; 32], KeyFileError> {
            let end = offset + 32;
            let slice = bytes.get(offset..end).ok_or(KeyFileError::BadLength)?;
            offset = end;
            slice.try_into().map_err(|_| KeyFileError::BadLength)
        };
        Ok(Self {
            ed_seed: take()?,
            ml_seed: take()?,
            x25519_secret: take()?,
        })
    }

    /// Serialize to the 101-byte node key format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(NODE_KEY_FILE_LEN);
        out.extend_from_slice(MAGIC_KEY);
        out.push(SUITE_HYBRID);
        out.extend_from_slice(&self.ed_seed);
        out.extend_from_slice(&self.ml_seed);
        out.extend_from_slice(&self.x25519_secret);
        out
    }

    /// The node's hybrid public key bundle.
    pub fn bundle(&self) -> Result<Vec<u8>, CryptoError> {
        Ok(HybridSigner::from_seeds(&self.ed_seed, &self.ml_seed)?.bundle())
    }

    /// The node ID.
    pub fn node_id(&self) -> Result<crate::ids::NodeId, CryptoError> {
        Ok(crate::crypto::node_id(&self.bundle()?))
    }

    /// Build a suite 1 signer for this node.
    pub fn signer(&self) -> Result<HybridSigner, CryptoError> {
        HybridSigner::from_seeds(&self.ed_seed, &self.ml_seed)
    }

    /// The X25519 static secret used by the Noise phase A handshake.
    #[must_use]
    pub fn x25519_secret(&self) -> &[u8; 32] {
        &self.x25519_secret
    }
}

impl Drop for NodeKey {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.ed_seed.zeroize();
        self.ml_seed.zeroize();
        self.x25519_secret.zeroize();
    }
}

/// Check Unix permission bits: group and other must have no access.
pub fn check_mode(mode: u32) -> Result<(), KeyFileError> {
    if mode & 0o077 != 0 {
        return Err(KeyFileError::LoosePermissions);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{SUITE1_BUNDLE_LEN, SUITE2_BUNDLE_LEN};

    #[test]
    fn repo_key_round_trip_suite1() {
        let key = RepoKey::generate(SUITE_HYBRID).unwrap();
        let bytes = key.to_bytes();
        assert_eq!(bytes.len(), REPO_KEY_FILE_LEN);
        let parsed = RepoKey::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.suite(), SUITE_HYBRID);
        let signer = parsed.signer().unwrap();
        assert_eq!(signer.bundle().len(), SUITE1_BUNDLE_LEN);
    }

    #[test]
    fn repo_key_round_trip_suite2() {
        let key = RepoKey::generate(SUITE_RECOVERY).unwrap();
        let parsed = RepoKey::from_bytes(&key.to_bytes()).unwrap();
        let signer = parsed.signer().unwrap();
        assert_eq!(signer.bundle().len(), SUITE2_BUNDLE_LEN);
        assert_eq!(signer.suite(), SUITE_RECOVERY);
    }

    #[test]
    fn node_key_round_trip() {
        let key = NodeKey::generate().unwrap();
        let bytes = key.to_bytes();
        assert_eq!(bytes.len(), NODE_KEY_FILE_LEN);
        let parsed = NodeKey::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.bundle().unwrap(), key.bundle().unwrap());
        assert_eq!(parsed.node_id().unwrap(), key.node_id().unwrap());
    }

    #[test]
    fn rejects_loose_permissions() {
        assert!(check_mode(0o600).is_ok());
        assert_eq!(check_mode(0o640), Err(KeyFileError::LoosePermissions));
        assert_eq!(check_mode(0o604), Err(KeyFileError::LoosePermissions));
    }

    #[test]
    fn rejects_bad_magic_and_suite() {
        let key = RepoKey::generate(SUITE_HYBRID).unwrap();
        let mut bytes = key.to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            RepoKey::from_bytes(&bytes),
            Err(KeyFileError::BadMagic)
        ));
        let mut bytes = key.to_bytes();
        bytes[4] = 9;
        assert!(matches!(
            RepoKey::from_bytes(&bytes),
            Err(KeyFileError::BadSuite)
        ));
    }
}
