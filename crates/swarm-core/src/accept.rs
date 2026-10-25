//! Acceptance state for verified manifests (spec section 5.4).

use std::collections::BTreeMap;

use crate::consts::SUITE_RECOVERY;
use crate::error::ErrorCode;
use crate::glob::any_glob_matches;
use crate::ids::{KeyId, Oid, RepoId};
use crate::manifest::{Manifest, VerifiedEnvelope};

/// One accepted manifest.
#[derive(Clone, Debug)]
pub struct StoredManifest {
    /// Parsed manifest.
    pub manifest: Manifest,
    /// Canonical manifest bytes.
    pub manifest_bytes: Vec<u8>,
    /// Signature bytes.
    pub signature: Vec<u8>,
    /// Envelope bytes.
    pub envelope: Vec<u8>,
    /// Envelope hash.
    pub manifest_id: [u8; 32],
}

impl From<VerifiedEnvelope> for StoredManifest {
    fn from(value: VerifiedEnvelope) -> Self {
        Self {
            manifest: value.manifest,
            manifest_bytes: value.manifest_bytes,
            signature: value.signature,
            envelope: value.envelope,
            manifest_id: value.manifest_id,
        }
    }
}

/// A recorded equivocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Equivocation {
    /// Signer key ID.
    pub signer: KeyId,
    /// Conflicting sequence number.
    pub seq: u64,
    /// First manifest ID.
    pub id_a: [u8; 32],
    /// Second manifest ID.
    pub id_b: [u8; 32],
}

/// Result of accepting one envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcceptOutcome {
    /// The manifest was stored.
    Accepted {
        /// New sequence number.
        seq: u64,
        /// Manifest ID.
        manifest_id: [u8; 32],
    },
    /// Same sequence and same manifest ID as the stored one.
    Duplicate,
    /// Same sequence but a different manifest ID; the first is kept.
    Equivocation,
}

/// Mutable accepted state for one repo.
#[derive(Clone, Debug)]
pub struct AcceptState {
    /// Repo being synced.
    pub repo_id: RepoId,
    /// Object format, fixed at seq 1.
    pub object_format: Option<String>,
    /// Current root key ID, initially the repo ID.
    pub root: KeyId,
    /// Recovery key ID, if the root manifest set one.
    pub recovery: Option<KeyId>,
    /// Latest accepted manifest per signer.
    pub signers: BTreeMap<KeyId, StoredManifest>,
    /// Recorded equivocations.
    pub equivocations: Vec<Equivocation>,
    /// Set when state changes and the sync loop must re-resolve.
    pub dirty: bool,
    /// Last canonical refs, used to keep a value on conflict.
    pub canonical: BTreeMap<String, Oid>,
}

impl AcceptState {
    /// New empty state for a repo.
    #[must_use]
    pub fn new(repo_id: RepoId) -> Self {
        Self {
            repo_id,
            object_format: None,
            root: KeyId::from_bytes(*repo_id.as_bytes()),
            recovery: None,
            signers: BTreeMap::new(),
            equivocations: Vec::new(),
            dirty: false,
            canonical: BTreeMap::new(),
        }
    }

    /// The latest root manifest, if any.
    #[must_use]
    pub fn root_manifest(&self) -> Option<&StoredManifest> {
        self.signers.get(&self.root)
    }

    /// Globs granted to `signer` by the current root manifest.
    #[must_use]
    pub fn scope_for(&self, signer: KeyId) -> Vec<String> {
        self.root_manifest()
            .and_then(|stored| stored.manifest.maintainers.as_ref())
            .map(|maintainers| {
                maintainers
                    .iter()
                    .filter(|maintainer| maintainer.key == signer)
                    .flat_map(|maintainer| maintainer.globs.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True when `signer` is root or a listed maintainer.
    #[must_use]
    pub fn is_authorized(&self, signer: KeyId) -> bool {
        if signer == self.root {
            return true;
        }
        self.root_manifest()
            .and_then(|stored| stored.manifest.maintainers.as_ref())
            .is_some_and(|maintainers| maintainers.iter().any(|m| m.key == signer))
    }

    /// Accept one verified envelope.
    pub fn accept(&mut self, envelope: VerifiedEnvelope) -> Result<AcceptOutcome, ErrorCode> {
        let manifest = &envelope.manifest;
        if manifest.repo_id != *self.repo_id.as_bytes() {
            return Err(ErrorCode::WrongRepo);
        }
        let signer = KeyId::from_bytes(manifest.signer);
        let is_root = signer == self.root;
        let is_recovery = self.recovery == Some(signer);

        if manifest.suite == u64::from(SUITE_RECOVERY) {
            if !is_recovery {
                return Err(ErrorCode::Unauthorized);
            }
        } else if !is_root && !self.is_authorized(signer) {
            return Err(ErrorCode::Unauthorized);
        }

        let forbidden_for_non_root = manifest.head.is_some()
            || manifest.maintainers.is_some()
            || manifest.object_format.is_some()
            || manifest.tombstones.is_some()
            || manifest.forked_from.is_some()
            || manifest.recovery.is_some()
            || (manifest.new_root.is_some() && !is_recovery);
        if !is_root && forbidden_for_non_root {
            return Err(ErrorCode::ForbiddenField);
        }

        if let (Some(incoming), Some(stored)) = (&manifest.object_format, &self.object_format) {
            if incoming != stored {
                return Err(ErrorCode::ObjectFormatChanged);
            }
        }

        if !is_root {
            let globs = self.scope_for(signer);
            if manifest
                .refs
                .keys()
                .any(|name| !any_glob_matches(&globs, name))
            {
                return Err(ErrorCode::OutOfScope);
            }
        }

        if let Some(stored) = self.signers.get(&signer) {
            if manifest.seq < stored.manifest.seq {
                return Err(ErrorCode::Stale);
            }
            if manifest.seq == stored.manifest.seq {
                if envelope.manifest_id == stored.manifest_id {
                    return Ok(AcceptOutcome::Duplicate);
                }
                self.equivocations.push(Equivocation {
                    signer,
                    seq: manifest.seq,
                    id_a: stored.manifest_id,
                    id_b: envelope.manifest_id,
                });
                return Ok(AcceptOutcome::Equivocation);
            }
            if manifest.seq == stored.manifest.seq + 1 && manifest.prev != stored.manifest_id {
                return Err(ErrorCode::BadPrev);
            }
        }

        let seq = manifest.seq;
        let id = envelope.manifest_id;
        if is_root {
            if let Some(format) = &manifest.object_format {
                self.object_format = Some(format.clone());
            }
            if let Some(recovery) = manifest.recovery {
                self.recovery = Some(KeyId::from_bytes(recovery));
            }
        }
        if manifest.suite == u64::from(SUITE_RECOVERY) {
            if let Some(new_root) = manifest.new_root {
                self.root = KeyId::from_bytes(new_root);
            }
        }
        self.signers.insert(signer, StoredManifest::from(envelope));
        self.dirty = true;
        Ok(AcceptOutcome::Accepted {
            seq,
            manifest_id: id,
        })
    }

    /// Accept a batch, processing the initial root manifest first.
    pub fn accept_batch(
        &mut self,
        envelopes: Vec<VerifiedEnvelope>,
    ) -> Vec<Result<AcceptOutcome, ErrorCode>> {
        let repo_id = *self.repo_id.as_bytes();
        let root_id = *self.root.as_bytes();
        let mut indexed: Vec<(usize, VerifiedEnvelope)> =
            envelopes.into_iter().enumerate().collect();
        indexed.sort_by_key(|(index, env)| {
            let rank = if env.manifest.signer == root_id {
                0
            } else if env.manifest.suite == u64::from(SUITE_RECOVERY) {
                1
            } else if env.manifest.signer == repo_id {
                0
            } else {
                2
            };
            (rank, env.manifest.seq, *index)
        });
        indexed
            .into_iter()
            .map(|(_, env)| self.accept(env))
            .collect()
    }
}
