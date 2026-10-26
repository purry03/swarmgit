//! Data-directory paths from spec section 9.4.

use std::fs;
use std::path::{Path, PathBuf};

use swarm_core::RepoId;

use crate::StoreError;

/// The resolved `$SWARM_HOME` and every path below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Resolve `SWARM_HOME`, else the platform data directory.
    pub fn resolve() -> Result<Self, StoreError> {
        if let Some(value) = std::env::var_os("SWARM_HOME") {
            if !value.is_empty() {
                return Ok(Self::from_root(PathBuf::from(value)));
            }
        }
        #[cfg(target_os = "macos")]
        let base = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support"));
        #[cfg(windows)]
        let base = std::env::var_os("APPDATA").map(PathBuf::from);
        #[cfg(not(any(target_os = "macos", windows)))]
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            });

        let base =
            base.ok_or_else(|| StoreError::Message("cannot determine data directory".into()))?;
        Ok(Self::from_root(base.join("swarmgit")))
    }

    /// Wrap an explicit root, used by tests and the daemon client.
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the directories the daemon owns.
    pub fn ensure(&self) -> Result<(), StoreError> {
        fs::create_dir_all(self.keys_dir())?;
        fs::create_dir_all(self.repos_dir())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// `$SWARM_HOME/node.key`.
    #[must_use]
    pub fn node_key_path(&self) -> PathBuf {
        self.root.join("node.key")
    }

    /// `$SWARM_HOME/config.toml` (parsed from M2).
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// `$SWARM_HOME/daemon.lock`.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }

    /// `$SWARM_HOME/daemon.log`.
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.root.join("daemon.log")
    }

    /// `$SWARM_HOME/daemon.json`.
    #[must_use]
    pub fn daemon_json_path(&self) -> PathBuf {
        self.root.join("daemon.json")
    }

    /// `$SWARM_HOME/swarm.db`.
    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.root.join("swarm.db")
    }

    /// `$SWARM_HOME/keys`.
    #[must_use]
    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// `$SWARM_HOME/repos`.
    #[must_use]
    pub fn repos_dir(&self) -> PathBuf {
        self.root.join("repos")
    }

    /// `$SWARM_HOME/keys/<repo-id>.key`.
    #[must_use]
    pub fn repo_key_path(&self, repo: RepoId) -> PathBuf {
        self.keys_dir().join(format!("{}.key", repo.to_base32()))
    }

    /// `$SWARM_HOME/repos/<repo-id>`.
    #[must_use]
    pub fn repo_dir(&self, repo: RepoId) -> PathBuf {
        self.repos_dir().join(repo.to_base32())
    }

    /// `$SWARM_HOME/repos/<repo-id>/store.git`.
    #[must_use]
    pub fn store_path(&self, repo: RepoId) -> PathBuf {
        self.repo_dir(repo).join("store.git")
    }

    /// `$SWARM_HOME/repos/<repo-id>/manifests`.
    #[must_use]
    pub fn manifests_dir(&self, repo: RepoId) -> PathBuf {
        self.repo_dir(repo).join("manifests")
    }

    /// IPC socket path, shortened when the default would exceed 100 bytes.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        let direct = self.root.join("ipc.sock");
        if direct.as_os_str().len() <= 100 {
            return direct;
        }
        let digest = blake3::hash(self.root.to_string_lossy().as_bytes());
        let short = hex::encode(&digest.as_bytes()[..4]);
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        runtime.join(format!("swarmgit-{short}.sock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_the_root() {
        let home = Home::from_root("/tmp/swarmgit-test");
        let repo = RepoId::from_bytes([7u8; 32]);
        assert_eq!(home.db_path(), Path::new("/tmp/swarmgit-test/swarm.db"));
        assert!(home.store_path(repo).ends_with("store.git"));
        assert!(home.repo_key_path(repo).to_string_lossy().ends_with(".key"));
    }

    #[test]
    fn long_roots_use_a_short_socket_name() {
        let home = Home::from_root(format!("/tmp/{}", "x".repeat(200)));
        let socket = home.socket_path();
        assert!(socket.as_os_str().len() < 100);
        assert!(socket.to_string_lossy().contains("swarmgit-"));
    }
}
