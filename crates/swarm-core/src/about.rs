//! A repo's name and description: a small record signed by the root key or a
//! listed maintainer, kept apart from the manifests so that peers which do
//! not know it are unaffected.

use crate::accept::AcceptState;
use crate::cbor::{self, Value};
use crate::consts::{
    CTX_SIG_ABOUT, MAX_ABOUT_BYTES, MAX_ABOUT_SKEW_SECS, MAX_DESCRIPTION_BYTES, MAX_NAME_BYTES,
    SUITE1_BUNDLE_LEN, SUITE1_SIG_LEN, SUITE_HYBRID,
};
use crate::crypto::{hash, key_id};
use crate::error::ErrorCode;
use crate::ids::{KeyId, RepoId};
use crate::pq::{self, Signer};

/// The signed fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct About {
    /// Repo the record belongs to.
    pub repo_id: [u8; 32],
    /// Unix seconds of the edit, raised above the replaced record if needed.
    /// The higher revision wins.
    pub revision: u64,
    /// Display name, empty when unset.
    pub name: String,
    /// One-line description, empty when unset.
    pub description: String,
    /// The signer's public key bundle.
    pub signer_keys: Vec<u8>,
}

/// A record whose signature and limits have been checked. Whether its signer
/// may name the repo depends on the root manifest: see
/// [`VerifiedAbout::is_valid_for`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAbout {
    /// Parsed fields.
    pub about: About,
    /// Key ID of the signer.
    pub signer: KeyId,
    /// Encoded record, as stored and sent.
    pub bytes: Vec<u8>,
}

/// Whether `text` fits a field of at most `max` bytes and is safe to print on
/// one line.
#[must_use]
pub fn is_valid_text(text: &str, max: usize) -> bool {
    text.len() <= max
        && !text.chars().any(|c| {
            // Line breaks, terminal escapes and bidirectional overrides.
            c.is_control()
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{200e}' | '\u{200f}')
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

impl About {
    fn check_limits(&self) -> Result<(), ErrorCode> {
        if is_valid_text(&self.name, MAX_NAME_BYTES)
            && is_valid_text(&self.description, MAX_DESCRIPTION_BYTES)
        {
            Ok(())
        } else {
            Err(ErrorCode::Malformed)
        }
    }

    fn encode_fields(&self) -> Result<Vec<u8>, ErrorCode> {
        cbor::encode(&Value::Array(vec![
            Value::Bytes(self.repo_id.to_vec()),
            Value::Uint(self.revision),
            Value::Text(self.name.clone()),
            Value::Text(self.description.clone()),
            Value::Bytes(self.signer_keys.clone()),
        ]))
        .map_err(|_| ErrorCode::Malformed)
    }

    /// Sign the record and return it verified.
    pub fn sign(&self, signer: &dyn Signer) -> Result<VerifiedAbout, ErrorCode> {
        self.check_limits()?;
        let fields = self.encode_fields()?;
        let signature = signer
            .try_sign(&sig_input(&fields))
            .map_err(|_| ErrorCode::BadSignature)?;
        let bytes = cbor::encode(&Value::Array(vec![
            Value::Bytes(fields),
            Value::Bytes(signature),
        ]))
        .map_err(|_| ErrorCode::Malformed)?;
        verify_about(&bytes)
    }
}

fn sig_input(fields: &[u8]) -> Vec<u8> {
    [CTX_SIG_ABOUT, fields].concat()
}

/// Decode a record and check its signature and limits.
pub fn verify_about(bytes: &[u8]) -> Result<VerifiedAbout, ErrorCode> {
    if bytes.len() > MAX_ABOUT_BYTES {
        return Err(ErrorCode::TooLarge);
    }
    let outer = cbor::decode(bytes).map_err(|_| ErrorCode::Malformed)?;
    let Some([Value::Bytes(fields), Value::Bytes(signature)]) = outer.as_array() else {
        return Err(ErrorCode::Malformed);
    };
    let inner = cbor::decode(fields).map_err(|_| ErrorCode::Malformed)?;
    let Some(
        [Value::Bytes(repo_id), Value::Uint(revision), Value::Text(name), Value::Text(description), Value::Bytes(signer_keys)],
    ) = inner.as_array()
    else {
        return Err(ErrorCode::Malformed);
    };
    let about = About {
        repo_id: repo_id
            .as_slice()
            .try_into()
            .map_err(|_| ErrorCode::Malformed)?,
        revision: *revision,
        name: name.clone(),
        description: description.clone(),
        signer_keys: signer_keys.clone(),
    };
    about.check_limits()?;
    if about.signer_keys.len() != SUITE1_BUNDLE_LEN {
        return Err(ErrorCode::BadKeys);
    }
    if signature.len() != SUITE1_SIG_LEN {
        return Err(ErrorCode::BadSignature);
    }
    pq::verify(
        SUITE_HYBRID,
        &about.signer_keys,
        &sig_input(fields),
        signature,
    )
    .map_err(|_| ErrorCode::BadSignature)?;
    Ok(VerifiedAbout {
        signer: key_id(&about.signer_keys),
        about,
        bytes: bytes.to_vec(),
    })
}

impl VerifiedAbout {
    /// True when the record is for this repo, its signer is the root or a
    /// listed maintainer, and it is not dated far past `now`.
    #[must_use]
    pub fn is_valid_for(&self, state: &AcceptState, now: u64) -> bool {
        self.about.repo_id == *state.repo_id.as_bytes()
            && state.is_authorized(self.signer)
            && self.about.revision <= now.saturating_add(MAX_ABOUT_SKEW_SECS)
    }

    /// True when this record should replace `other`. Equal revisions are
    /// settled by hash, so that every peer keeps the same one.
    #[must_use]
    pub fn supersedes(&self, other: &Self) -> bool {
        (self.about.revision, hash(&self.bytes)) > (other.about.revision, hash(&other.bytes))
    }

    /// Repo the record belongs to.
    #[must_use]
    pub fn repo_id(&self) -> RepoId {
        RepoId::from_bytes(self.about.repo_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pq::HybridSigner;

    fn record(signer: &HybridSigner, revision: u64, name: &str) -> About {
        About {
            repo_id: *crate::crypto::repo_id(&signer.bundle()).as_bytes(),
            revision,
            name: name.into(),
            description: "a description".into(),
            signer_keys: signer.bundle(),
        }
    }

    #[test]
    fn round_trip_and_tamper() {
        let signer = HybridSigner::generate().unwrap();
        let signed = record(&signer, 7, "demo").sign(&signer).unwrap();
        assert_eq!(verify_about(&signed.bytes).unwrap(), signed);
        let mut bytes = signed.bytes.clone();
        let at = bytes.windows(4).position(|w| w == b"demo").unwrap();
        bytes[at] = b'm';
        assert_eq!(verify_about(&bytes), Err(ErrorCode::BadSignature));
    }

    #[test]
    fn unprintable_or_long_text_is_refused() {
        let signer = HybridSigner::generate().unwrap();
        for name in ["a\nb", "\u{1b}[2J", "a\u{202e}b"] {
            assert!(record(&signer, 1, name).sign(&signer).is_err());
        }
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(record(&signer, 1, &long).sign(&signer).is_err());
    }

    #[test]
    fn only_authorized_and_current_records_count() {
        let root = HybridSigner::generate().unwrap();
        let other = HybridSigner::generate().unwrap();
        let repo = crate::crypto::repo_id(&root.bundle());
        let state = AcceptState::new(repo);
        let mine = record(&root, 10, "mine").sign(&root).unwrap();
        assert!(mine.is_valid_for(&state, 10));
        let future = record(&root, 10 + MAX_ABOUT_SKEW_SECS + 1, "future");
        assert!(!future.sign(&root).unwrap().is_valid_for(&state, 10));
        let mut theirs = record(&other, 11, "theirs");
        theirs.repo_id = *repo.as_bytes();
        assert!(!theirs.sign(&other).unwrap().is_valid_for(&state, 11));
    }

    #[test]
    fn higher_revision_supersedes() {
        let signer = HybridSigner::generate().unwrap();
        let old = record(&signer, 1, "old").sign(&signer).unwrap();
        let new = record(&signer, 2, "new").sign(&signer).unwrap();
        assert!(new.supersedes(&old));
        assert!(!old.supersedes(&new));
        assert!(!new.supersedes(&new));
    }
}
