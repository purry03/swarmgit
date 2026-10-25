//! Manifests, envelopes, verification and acceptance (spec section 5).

use std::collections::BTreeMap;

use crate::cbor::{self, CborError, Value};
use crate::consts::{
    MANIFEST_VERSION, MAX_ENVELOPE_BYTES, MAX_GLOBS, MAX_MAINTAINERS, MAX_REFS, SUITE1_BUNDLE_LEN,
    SUITE2_BUNDLE_LEN, SUITE_HYBRID, SUITE_RECOVERY,
};
use crate::crypto::{key_id, manifest_id, sig_input};
use crate::error::ErrorCode;
use crate::ids::{KeyId, Oid};
use crate::pq::{self, Signer};
use crate::refname::is_valid_refname;

/// A maintainer entry in a root manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Maintainer {
    /// Key ID of the maintainer.
    pub key: KeyId,
    /// Ref globs the maintainer may publish.
    pub globs: Vec<String>,
}

/// One signer's signed snapshot of refs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Manifest version, always 1 in v1.
    pub version: u64,
    /// Repo this manifest belongs to.
    pub repo_id: [u8; 32],
    /// Key ID of the signing key bundle.
    pub signer: [u8; 32],
    /// Strictly increasing per signer, starts at 1.
    pub seq: u64,
    /// Manifest ID of this signer's previous manifest, or 32 zero bytes.
    pub prev: [u8; 32],
    /// Unix seconds, informational only.
    pub ts: u64,
    /// Ref name to object ID.
    pub refs: BTreeMap<String, Oid>,
    /// Default branch, root only.
    pub head: Option<String>,
    /// Maintainer list, root only.
    pub maintainers: Option<Vec<Maintainer>>,
    /// `sha1` or `sha256`, required on a root manifest with seq 1.
    pub object_format: Option<String>,
    /// Deleted refs, root only.
    pub tombstones: Option<Vec<String>>,
    /// Source repo ID for a fork, root and seq 1 only.
    pub forked_from: Option<[u8; 32]>,
    /// Recovery key ID, root only.
    pub recovery: Option<[u8; 32]>,
    /// New root key ID on a recovery manifest.
    pub new_root: Option<[u8; 32]>,
    /// The signer's public key bundle.
    pub signer_keys: Vec<u8>,
    /// Signature suite.
    pub suite: u64,
}

/// The wire envelope: a CBOR array of manifest bytes and signature bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// Canonical manifest bytes, exactly as signed.
    pub manifest_bytes: Vec<u8>,
    /// Signature bytes for the suite.
    pub signature: Vec<u8>,
}

impl Envelope {
    /// Build an envelope from its two byte strings.
    #[must_use]
    pub fn new(manifest_bytes: Vec<u8>, signature: Vec<u8>) -> Self {
        Self {
            manifest_bytes,
            signature,
        }
    }

    /// Encode the envelope canonically.
    pub fn encode(&self) -> Result<Vec<u8>, ErrorCode> {
        cbor::encode(&Value::Array(vec![
            Value::Bytes(self.manifest_bytes.clone()),
            Value::Bytes(self.signature.clone()),
        ]))
        .map_err(cbor_code)
    }

    /// Decode an envelope without verifying the signature.
    pub fn decode(bytes: &[u8]) -> Result<Self, ErrorCode> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(ErrorCode::TooLarge);
        }
        let value = cbor::decode(bytes).map_err(cbor_code)?;
        let items = as_array(&value)?;
        if items.len() != 2 {
            return Err(ErrorCode::Malformed);
        }
        Ok(Self {
            manifest_bytes: as_bytes(&items[0])?.to_vec(),
            signature: as_bytes(&items[1])?.to_vec(),
        })
    }
}

/// A verified envelope, carrying the exact bytes that were signed.
#[derive(Clone, Debug)]
pub struct VerifiedEnvelope {
    /// Parsed manifest.
    pub manifest: Manifest,
    /// Canonical manifest bytes.
    pub manifest_bytes: Vec<u8>,
    /// Signature bytes.
    pub signature: Vec<u8>,
    /// Canonical envelope bytes.
    pub envelope: Vec<u8>,
    /// `blake3(envelope)`.
    pub manifest_id: [u8; 32],
}

fn cbor_code(err: CborError) -> ErrorCode {
    match err {
        CborError::NonCanonical => ErrorCode::NonCanonical,
        CborError::Malformed | CborError::DuplicateKey => ErrorCode::Malformed,
        CborError::TooLarge => ErrorCode::TooLarge,
    }
}

fn as_uint(value: &Value) -> Result<u64, ErrorCode> {
    value.as_uint().ok_or(ErrorCode::Malformed)
}

fn as_bytes(value: &Value) -> Result<&[u8], ErrorCode> {
    value.as_bytes().ok_or(ErrorCode::Malformed)
}

fn as_text(value: &Value) -> Result<&str, ErrorCode> {
    value.as_text().ok_or(ErrorCode::Malformed)
}

fn as_array(value: &Value) -> Result<&[Value], ErrorCode> {
    value.as_array().ok_or(ErrorCode::Malformed)
}

fn as_map(value: &Value) -> Result<&[(Value, Value)], ErrorCode> {
    value.as_map().ok_or(ErrorCode::Malformed)
}

fn as_bytes32(value: &Value) -> Result<[u8; 32], ErrorCode> {
    as_bytes(value)?
        .try_into()
        .map_err(|_| ErrorCode::Malformed)
}

fn required(fields: &BTreeMap<u64, Value>, key: u64) -> Result<&Value, ErrorCode> {
    fields.get(&key).ok_or(ErrorCode::Malformed)
}

impl Manifest {
    /// Encode canonically.
    pub fn encode(&self) -> Result<Vec<u8>, ErrorCode> {
        let mut map = vec![
            (Value::Uint(0), Value::Uint(self.version)),
            (Value::Uint(1), Value::Bytes(self.repo_id.to_vec())),
            (Value::Uint(2), Value::Bytes(self.signer.to_vec())),
            (Value::Uint(3), Value::Uint(self.seq)),
            (Value::Uint(4), Value::Bytes(self.prev.to_vec())),
            (Value::Uint(5), Value::Uint(self.ts)),
        ];
        let refs = self
            .refs
            .iter()
            .map(|(name, oid)| {
                (
                    Value::Text(name.clone()),
                    Value::Bytes(oid.as_slice().to_vec()),
                )
            })
            .collect();
        map.push((Value::Uint(6), Value::Map(refs)));
        if let Some(head) = &self.head {
            map.push((Value::Uint(7), Value::Text(head.clone())));
        }
        if let Some(maintainers) = &self.maintainers {
            let entries = maintainers
                .iter()
                .map(|maintainer| {
                    Value::Map(vec![
                        (
                            Value::Uint(0),
                            Value::Bytes(maintainer.key.as_bytes().to_vec()),
                        ),
                        (
                            Value::Uint(1),
                            Value::Array(
                                maintainer
                                    .globs
                                    .iter()
                                    .map(|glob| Value::Text(glob.clone()))
                                    .collect(),
                            ),
                        ),
                    ])
                })
                .collect();
            map.push((Value::Uint(8), Value::Array(entries)));
        }
        if let Some(format) = &self.object_format {
            map.push((Value::Uint(9), Value::Text(format.clone())));
        }
        if let Some(tombstones) = &self.tombstones {
            map.push((
                Value::Uint(10),
                Value::Array(
                    tombstones
                        .iter()
                        .map(|name| Value::Text(name.clone()))
                        .collect(),
                ),
            ));
        }
        if let Some(forked_from) = self.forked_from {
            map.push((Value::Uint(11), Value::Bytes(forked_from.to_vec())));
        }
        if let Some(recovery) = self.recovery {
            map.push((Value::Uint(12), Value::Bytes(recovery.to_vec())));
        }
        if let Some(new_root) = self.new_root {
            map.push((Value::Uint(13), Value::Bytes(new_root.to_vec())));
        }
        map.push((Value::Uint(14), Value::Bytes(self.signer_keys.clone())));
        map.push((Value::Uint(15), Value::Uint(self.suite)));
        cbor::encode(&Value::Map(map)).map_err(cbor_code)
    }

    /// Decode canonical bytes into a manifest, without verifying a signature.
    pub fn decode(bytes: &[u8]) -> Result<Self, ErrorCode> {
        let value = cbor::decode(bytes).map_err(cbor_code)?;
        let manifest = Self::from_value(&value)?;
        if manifest.encode()? != bytes {
            return Err(ErrorCode::NonCanonical);
        }
        Ok(manifest)
    }

    fn from_value(value: &Value) -> Result<Self, ErrorCode> {
        let entries = as_map(value)?;
        let mut fields = BTreeMap::new();
        for (key, value) in entries {
            let key = as_uint(key)?;
            if key > 15 {
                return Err(ErrorCode::Malformed);
            }
            if fields.insert(key, value.clone()).is_some() {
                return Err(ErrorCode::Malformed);
            }
        }

        let refs_value = required(&fields, 6)?;
        let mut refs = BTreeMap::new();
        for (name, oid) in as_map(refs_value)? {
            let name = as_text(name)?.to_string();
            let oid = Oid::new(as_bytes(oid)?.to_vec());
            if refs.insert(name, oid).is_some() {
                return Err(ErrorCode::Malformed);
            }
        }

        let maintainers = match fields.get(&8) {
            Some(value) => {
                let mut list = Vec::new();
                for entry in as_array(value)? {
                    let mut key = None;
                    let mut globs = None;
                    for (k, v) in as_map(entry)? {
                        match as_uint(k)? {
                            0 => {
                                let bytes = as_bytes(v)?;
                                key = Some(
                                    KeyId::from_slice(bytes).map_err(|_| ErrorCode::Malformed)?,
                                );
                            }
                            1 => {
                                let mut parsed = Vec::new();
                                for glob in as_array(v)? {
                                    parsed.push(as_text(glob)?.to_string());
                                }
                                globs = Some(parsed);
                            }
                            _ => return Err(ErrorCode::Malformed),
                        }
                    }
                    list.push(Maintainer {
                        key: key.ok_or(ErrorCode::Malformed)?,
                        globs: globs.ok_or(ErrorCode::Malformed)?,
                    });
                }
                Some(list)
            }
            None => None,
        };

        let tombstones = match fields.get(&10) {
            Some(value) => {
                let mut list = Vec::new();
                for item in as_array(value)? {
                    list.push(as_text(item)?.to_string());
                }
                Some(list)
            }
            None => None,
        };

        let seq = as_uint(required(&fields, 3)?)?;
        if seq == 0 {
            return Err(ErrorCode::Malformed);
        }

        Ok(Self {
            version: as_uint(required(&fields, 0)?)?,
            repo_id: as_bytes32(required(&fields, 1)?)?,
            signer: as_bytes32(required(&fields, 2)?)?,
            seq,
            prev: as_bytes32(required(&fields, 4)?)?,
            ts: as_uint(required(&fields, 5)?)?,
            refs,
            head: fields
                .get(&7)
                .map(|v| as_text(v).map(str::to_string))
                .transpose()?,
            maintainers,
            object_format: fields
                .get(&9)
                .map(|v| as_text(v).map(str::to_string))
                .transpose()?,
            tombstones,
            forked_from: fields.get(&11).map(as_bytes32).transpose()?,
            recovery: fields.get(&12).map(as_bytes32).transpose()?,
            new_root: fields.get(&13).map(as_bytes32).transpose()?,
            signer_keys: as_bytes(required(&fields, 14)?)?.to_vec(),
            suite: as_uint(required(&fields, 15)?)?,
        })
    }

    fn validate_header(&self) -> Result<u8, ErrorCode> {
        if self.version != MANIFEST_VERSION {
            return Err(ErrorCode::BadVersion);
        }
        if self.seq == 1 && self.prev != [0u8; 32] {
            return Err(ErrorCode::BadPrev);
        }
        let suite = u8::try_from(self.suite).map_err(|_| ErrorCode::UnknownSuite)?;
        let expected_bundle = match suite {
            SUITE_HYBRID => {
                if self.new_root.is_some() {
                    return Err(ErrorCode::UnknownSuite);
                }
                SUITE1_BUNDLE_LEN
            }
            SUITE_RECOVERY => {
                if self.new_root.is_none() {
                    return Err(ErrorCode::UnknownSuite);
                }
                SUITE2_BUNDLE_LEN
            }
            _ => return Err(ErrorCode::UnknownSuite),
        };
        if self.signer_keys.len() != expected_bundle {
            return Err(ErrorCode::BadKeys);
        }
        if key_id(&self.signer_keys) != KeyId::from_bytes(self.signer) {
            return Err(ErrorCode::BadKeys);
        }
        Ok(suite)
    }

    fn validate_refs(&self) -> Result<(), ErrorCode> {
        if self.refs.len() > MAX_REFS {
            return Err(ErrorCode::TooManyRefs);
        }
        for name in self.refs.keys() {
            if !is_valid_refname(name) {
                return Err(ErrorCode::BadRefname);
            }
        }
        if let Some(head) = &self.head {
            if !is_valid_refname(head) {
                return Err(ErrorCode::BadRefname);
            }
        }
        if let Some(tombstones) = &self.tombstones {
            for name in tombstones {
                if !is_valid_refname(name) {
                    return Err(ErrorCode::BadRefname);
                }
            }
        }
        if let Some(maintainers) = &self.maintainers {
            if maintainers.len() > MAX_MAINTAINERS {
                return Err(ErrorCode::TooManyRefs);
            }
            for maintainer in maintainers {
                if maintainer.globs.len() > MAX_GLOBS {
                    return Err(ErrorCode::TooManyRefs);
                }
            }
        }

        let expected_oid_len = match self.object_format.as_deref() {
            Some("sha1") => Some(20),
            Some("sha256") => Some(32),
            Some(_) => return Err(ErrorCode::BadOidLen),
            None => None,
        };
        if self.seq == 1 && self.signer == self.repo_id && self.object_format.is_none() {
            return Err(ErrorCode::BadOidLen);
        }
        let mut inferred = None;
        for oid in self.refs.values() {
            if let Some(expected) = expected_oid_len {
                if oid.len() != expected {
                    return Err(ErrorCode::BadOidLen);
                }
            } else {
                if oid.len() != 20 && oid.len() != 32 {
                    return Err(ErrorCode::BadOidLen);
                }
                match inferred {
                    None => inferred = Some(oid.len()),
                    Some(len) if len != oid.len() => return Err(ErrorCode::BadOidLen),
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// Sign this manifest and return a verified envelope.
    pub fn sign(&self, signer: &dyn Signer) -> Result<VerifiedEnvelope, ErrorCode> {
        let manifest_bytes = self.encode()?;
        let signature = signer
            .try_sign(&sig_input(&manifest_bytes))
            .map_err(|_| ErrorCode::BadSignature)?;
        let envelope = encode_envelope(&manifest_bytes, &signature)?;
        verify_envelope(&envelope)
    }
}

/// Encode an envelope: a CBOR array of the manifest bytes and signature bytes.
pub fn encode_envelope(manifest_bytes: &[u8], signature: &[u8]) -> Result<Vec<u8>, ErrorCode> {
    Envelope::new(manifest_bytes.to_vec(), signature.to_vec()).encode()
}

/// Verify an envelope and return its manifest and exact signed bytes.
///
/// Error order follows spec 5.4: size, canonical form, version, suite and keys,
/// signature, then refs and object IDs.
pub fn verify_envelope(bytes: &[u8]) -> Result<VerifiedEnvelope, ErrorCode> {
    let envelope = Envelope::decode(bytes)?;
    let manifest = Manifest::decode(&envelope.manifest_bytes)?;
    let suite = manifest.validate_header()?;
    let expected_sig_len = pq::signature_len(suite).ok_or(ErrorCode::UnknownSuite)?;
    if envelope.signature.len() != expected_sig_len {
        return Err(ErrorCode::BadSignature);
    }
    pq::verify(
        suite,
        &manifest.signer_keys,
        &sig_input(&envelope.manifest_bytes),
        &envelope.signature,
    )
    .map_err(|_| ErrorCode::BadSignature)?;
    manifest.validate_refs()?;
    Ok(VerifiedEnvelope {
        manifest,
        manifest_bytes: envelope.manifest_bytes,
        signature: envelope.signature,
        envelope: bytes.to_vec(),
        manifest_id: manifest_id(bytes),
    })
}
