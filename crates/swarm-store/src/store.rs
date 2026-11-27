//! Per-repo store: bare git repository, manifests and the signing key.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use swarm_core::about::{verify_about, VerifiedAbout};
use swarm_core::accept::{AcceptState, StoredManifest};
use swarm_core::ids::{Oid, RepoId};
use swarm_core::keyfile::{RepoKey, REPO_KEY_FILE_LEN};
use swarm_core::manifest::verify_envelope;
use swarm_core::resolve::{resolve, Ancestry, Resolved};

use crate::git::{self, Git};
use crate::home::Home;
use crate::keyio;
use crate::StoreError;

/// One swarm repo inside `$SWARM_HOME/repos/<repo-id>`.
#[derive(Clone, Debug)]
pub struct RepoStore {
    home: Home,
    repo_id: RepoId,
}

impl RepoStore {
    /// Wrap a repo under a home directory.
    #[must_use]
    pub fn new(home: Home, repo_id: RepoId) -> Self {
        Self { home, repo_id }
    }

    /// Repo ID.
    #[must_use]
    pub fn repo_id(&self) -> RepoId {
        self.repo_id
    }

    /// `$SWARM_HOME/repos/<repo-id>`.
    #[must_use]
    pub fn dir(&self) -> PathBuf {
        self.home.repo_dir(self.repo_id)
    }

    /// Bare store path.
    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.home.store_path(self.repo_id)
    }

    /// Manifests directory.
    #[must_use]
    pub fn manifests_dir(&self) -> PathBuf {
        self.home.manifests_dir(self.repo_id)
    }

    /// Signing key path.
    #[must_use]
    pub fn key_path(&self) -> PathBuf {
        self.home.repo_key_path(self.repo_id)
    }

    /// Git handle for the bare store.
    #[must_use]
    pub fn git(&self) -> Git {
        Git::bare(self.store_path())
    }

    /// Create the bare store and set the required config (ST-1, ST-2, SEC-7).
    pub async fn create(&self, object_format: &str) -> Result<(), StoreError> {
        fs::create_dir_all(self.manifests_dir())?;
        git::init_bare(&self.store_path(), object_format).await?;
        self.configure().await
    }

    /// Apply the required git configuration to an existing store.
    pub async fn configure(&self) -> Result<(), StoreError> {
        let store = self.git();
        for (key, value) in [
            ("uploadpack.allowAnySHA1InWant", "true"),
            ("receive.denyCurrentBranch", "ignore"),
            ("gc.auto", "0"),
            ("core.logAllRefUpdates", "false"),
            ("transfer.fsckObjects", "true"),
        ] {
            store.config(key, value).await?;
        }
        Ok(())
    }

    /// True when the store directory already exists.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.store_path().is_dir()
    }

    /// Write the latest envelope for a signer atomically. A newer one on
    /// disk stays: a sync holds its state across the network, and a push
    /// may publish in the meantime.
    pub fn write_manifest(&self, stored: &StoredManifest) -> Result<(), StoreError> {
        static WRITING: Mutex<()> = Mutex::new(());
        let signer = hex::encode(stored.manifest.signer);
        let path = self.manifests_dir().join(format!("{signer}.cbor"));
        let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(on_disk) = fs::read(&path) {
            if on_disk == stored.envelope
                || verify_envelope(&on_disk)
                    .is_ok_and(|on_disk| on_disk.manifest.seq > stored.manifest.seq)
            {
                return Ok(());
            }
        }
        keyio::write_private_replace(&path, &stored.envelope)
    }

    /// Path of the signed name and description record.
    #[must_use]
    pub fn about_path(&self) -> PathBuf {
        self.dir().join("about.cbor")
    }

    /// The stored name and description, when their signer may still set them
    /// according to `state`.
    #[must_use]
    pub fn load_about(&self, state: &AcceptState, now: u64) -> Option<VerifiedAbout> {
        let bytes = fs::read(self.about_path()).ok()?;
        verify_about(&bytes)
            .ok()
            .filter(|about| about.is_valid_for(state, now))
    }

    /// Store `about` if its signer may set it and it supersedes the stored
    /// record. Returns true when it was written.
    pub fn write_about(
        &self,
        about: &VerifiedAbout,
        state: &AcceptState,
        now: u64,
    ) -> Result<bool, StoreError> {
        static WRITING: Mutex<()> = Mutex::new(());
        let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let newer = self
            .load_about(state, now)
            .is_none_or(|old| about.supersedes(&old));
        if !newer || !about.is_valid_for(state, now) {
            return Ok(false);
        }
        keyio::write_private_replace(&self.about_path(), &about.bytes)?;
        Ok(true)
    }

    /// Take into `state` every manifest on disk that is newer than its own.
    pub fn adopt_newer(&self, state: &mut AcceptState) -> Result<(), StoreError> {
        for (signer, on_disk) in self.load_state()?.signers {
            let older = state
                .signers
                .get(&signer)
                .is_none_or(|mine| mine.manifest.seq < on_disk.manifest.seq);
            if older {
                state.signers.insert(signer, on_disk);
            }
        }
        Ok(())
    }

    /// Write every accepted manifest to disk (5.4 step 8).
    pub fn persist_state(&self, state: &AcceptState) -> Result<(), StoreError> {
        fs::create_dir_all(self.manifests_dir())?;
        for stored in state.signers.values() {
            self.write_manifest(stored)?;
        }
        Ok(())
    }

    /// Remove temporary manifests left by an interrupted write.
    pub fn clean_temp_manifests(&self) -> Result<(), StoreError> {
        let dir = self.manifests_dir();
        if !dir.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') && name.ends_with(".tmp") {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    /// Load and accept every stored envelope.
    pub fn load_state(&self) -> Result<AcceptState, StoreError> {
        let mut state = AcceptState::new(self.repo_id);
        let dir = self.manifests_dir();
        let mut envelopes = Vec::new();
        if dir.is_dir() {
            let mut paths: Vec<PathBuf> = fs::read_dir(&dir)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "cbor"))
                .collect();
            paths.sort();
            for path in paths {
                let bytes = fs::read(&path)?;
                match verify_envelope(&bytes) {
                    Ok(envelope) => envelopes.push(envelope),
                    Err(err) => {
                        eprintln!("swarm: ignoring invalid manifest {}: {err}", path.display());
                    }
                }
            }
        }
        for outcome in state.accept_batch(envelopes) {
            if let Err(err) = outcome {
                eprintln!("swarm: ignoring unaccepted manifest: {err}");
            }
        }
        Ok(state)
    }

    /// Refs currently in the bare store.
    pub async fn read_refs(&self) -> Result<BTreeMap<String, Oid>, StoreError> {
        self.git().refs().await
    }

    /// Move the store's refs to the canonical table (ST-3, ST-4).
    pub async fn apply_canonical(
        &self,
        canonical: &BTreeMap<String, Oid>,
    ) -> Result<(), StoreError> {
        let store = self.git();
        let current = store.refs().await?;
        for (name, oid) in canonical {
            if current.get(name) == Some(oid) {
                continue;
            }
            if !store.history_is_complete(oid).await {
                continue;
            }
            store.update_ref(name, oid).await?;
        }
        for name in current.keys() {
            if !canonical.contains_key(name) {
                store.delete_ref(name).await?;
            }
        }
        Ok(())
    }

    /// Resolve canonical refs using the store's object database.
    #[must_use]
    pub fn resolve(&self, state: &AcceptState) -> Resolved {
        // A conflicting ref keeps the value it has now (5.5), so hand the
        // resolver what the store holds.
        let mut current = state.clone();
        current.canonical = self.refs_now();
        resolve(&current, &GitAncestry::new(self.store_path()))
    }

    /// `refs/heads` and `refs/tags` as they are in the store right now.
    fn refs_now(&self) -> BTreeMap<String, Oid> {
        let output = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(self.store_path())
            .args([
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                "refs/heads",
                "refs/tags",
            ])
            .output();
        let mut refs = BTreeMap::new();
        let Ok(output) = output else { return refs };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some((oid, name)) = line.split_once(' ') {
                if let Ok(bytes) = hex::decode(oid) {
                    refs.insert(name.to_string(), Oid::new(bytes));
                }
            }
        }
        refs
    }

    /// Remove `refs/swarm/*` at startup (ST-5).
    pub async fn remove_temp_refs(&self) -> Result<(), StoreError> {
        let store = self.git();
        let text = store
            .output(&["for-each-ref", "--format=%(refname)", "refs/swarm"])
            .await?;
        for name in text.lines().filter(|line| !line.is_empty()) {
            store.delete_ref(name).await?;
        }
        Ok(())
    }

    /// True when a signing key file exists.
    #[must_use]
    pub fn has_key(&self) -> bool {
        self.key_path().is_file()
    }

    /// Load the root signing key, enforcing file permissions.
    pub fn load_key(&self) -> Result<Option<RepoKey>, StoreError> {
        let Some(bytes) = keyio::read_private(&self.key_path())? else {
            return Ok(None);
        };
        if bytes.len() != REPO_KEY_FILE_LEN {
            return Err(StoreError::Message("repo key has the wrong length".into()));
        }
        Ok(Some(RepoKey::from_bytes(&bytes)?))
    }

    /// Write a new signing key with mode 0600.
    pub fn write_key(&self, key: &RepoKey) -> Result<(), StoreError> {
        keyio::write_private_new(&self.key_path(), &key.to_bytes())
    }
}

/// Synchronous ancestry queries backed by `git merge-base --is-ancestor`.
#[derive(Clone, Debug)]
pub struct GitAncestry {
    store: PathBuf,
}

impl GitAncestry {
    /// Query a bare store.
    #[must_use]
    pub fn new(store: impl Into<PathBuf>) -> Self {
        Self {
            store: store.into(),
        }
    }
}

impl Ancestry for GitAncestry {
    fn is_ancestor(&self, a: &Oid, b: &Oid) -> Option<bool> {
        let output = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&self.store)
            .args(["merge-base", "--is-ancestor", &a.to_hex(), &b.to_hex()])
            .output()
            .ok()?;
        match output.status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }
}

/// The canonical refs for a store, without applying them.
pub fn canonical_refs(store: &RepoStore, state: &AcceptState) -> Resolved {
    resolve(state, &GitAncestry::new(store.store_path()))
}

/// Convenience: the bare path of a store under a home.
#[must_use]
pub fn store_path(home: &Home, repo: RepoId) -> PathBuf {
    home.store_path(repo)
}

/// Convenience: the manifests path of a store under a home.
#[must_use]
pub fn manifests_path(home: &Home, repo: RepoId) -> PathBuf {
    home.manifests_dir(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trip_in_a_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::from_root(dir.path());
        home.ensure().unwrap();
        let repo = RepoId::from_bytes([5u8; 32]);
        let store = RepoStore::new(home, repo);
        let key = RepoKey::generate(1).unwrap();
        store.write_key(&key).unwrap();
        assert!(store.has_key());
        let loaded = store.load_key().unwrap().unwrap();
        assert_eq!(loaded.suite(), 1);
    }

    #[test]
    fn an_older_state_never_replaces_a_newer_manifest() {
        use swarm_core::pq::{HybridSigner, Signer};

        let signer = HybridSigner::generate().unwrap();
        let repo = swarm_core::crypto::repo_id(&signer.bundle());
        let key = *swarm_core::crypto::key_id(&signer.bundle()).as_bytes();
        let signed = |seq: u64, prev: [u8; 32]| {
            let manifest = swarm_core::manifest::Manifest {
                version: 1,
                repo_id: *repo.as_bytes(),
                signer: key,
                seq,
                prev,
                ts: 1,
                refs: BTreeMap::new(),
                head: None,
                maintainers: None,
                object_format: Some("sha1".into()),
                tombstones: None,
                forked_from: None,
                recovery: None,
                new_root: None,
                signer_keys: signer.bundle(),
                suite: u64::from(signer.suite()),
            };
            StoredManifest::from(manifest.sign(&signer).unwrap())
        };
        let old = signed(1, [0; 32]);
        let new = signed(2, old.manifest_id);

        let dir = tempfile::tempdir().unwrap();
        let store = RepoStore::new(Home::from_root(dir.path()), repo);
        fs::create_dir_all(store.manifests_dir()).unwrap();
        store.write_manifest(&old).unwrap();
        let mut stale = store.load_state().unwrap();
        store.write_manifest(&new).unwrap();
        store.persist_state(&stale).unwrap();
        store.adopt_newer(&mut stale).unwrap();
        for state in [store.load_state().unwrap(), stale] {
            let seqs: Vec<u64> = state.signers.values().map(|s| s.manifest.seq).collect();
            assert_eq!(seqs, [2]);
        }
    }
}
