//! Thin wrapper around the `git` binary (spec section 8.1).
//!
//! Nothing here reimplements git internals. Store operations use
//! `git --git-dir=<store>`; worktree operations use `git -C <dir>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use swarm_core::ids::Oid;
use tokio::process::Command;

use crate::StoreError;

fn command(prefix: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.args(prefix);
    command.stdin(Stdio::null());
    command
}

async fn finish(output: std::process::Output, what: &str) -> Result<String, StoreError> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(StoreError::Git(format!("{what} failed: {}", stderr.trim())))
    }
}

/// A git repository addressed either as a worktree or a bare store.
#[derive(Clone, Debug)]
pub struct Git {
    dir: PathBuf,
    bare: bool,
}

impl Git {
    /// Address a normal repository through `git -C <dir>`.
    #[must_use]
    pub fn worktree(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            bare: false,
        }
    }

    /// Address a bare repository through `git --git-dir=<dir>`.
    #[must_use]
    pub fn bare(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            bare: true,
        }
    }

    fn prefix(&self) -> Vec<String> {
        let dir = self.dir.to_string_lossy().into_owned();
        if self.bare {
            vec!["--git-dir".to_string(), dir]
        } else {
            vec!["-C".to_string(), dir]
        }
    }

    async fn run(&self, args: &[&str]) -> Result<String, StoreError> {
        let prefix = self.prefix();
        let mut parts: Vec<&str> = prefix.iter().map(String::as_str).collect();
        parts.extend_from_slice(args);
        let output = command(&parts).output().await?;
        finish(output, &format!("git {}", args.join(" "))).await
    }

    /// Run a git command, returning its trimmed stdout.
    pub async fn output(&self, args: &[&str]) -> Result<String, StoreError> {
        self.run(args).await
    }

    /// Set a config key.
    pub async fn config(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.run(&["config", key, value]).await.map(|_| ())
    }

    /// Refs under `refs/heads` and `refs/tags`, sorted by name.
    pub async fn refs(&self) -> Result<BTreeMap<String, Oid>, StoreError> {
        let text = self
            .run(&[
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                "refs/heads",
                "refs/tags",
            ])
            .await?;
        let mut out = BTreeMap::new();
        for line in text.lines() {
            let Some((oid, name)) = line.split_once(' ') else {
                continue;
            };
            let bytes = hex::decode(oid)
                .map_err(|_| StoreError::Git(format!("git returned a bad object ID {oid}")))?;
            out.insert(name.to_string(), Oid::new(bytes));
        }
        Ok(out)
    }

    /// Update one ref.
    pub async fn update_ref(&self, name: &str, oid: &Oid) -> Result<(), StoreError> {
        self.run(&["update-ref", name, &oid.to_hex()]).await?;
        Ok(())
    }

    /// Delete one ref.
    pub async fn delete_ref(&self, name: &str) -> Result<(), StoreError> {
        self.run(&["update-ref", "-d", name]).await?;
        Ok(())
    }

    /// True when `git rev-list --objects --missing=error <oid>` succeeds.
    pub async fn history_is_complete(&self, oid: &Oid) -> bool {
        let oid = oid.to_hex();
        self.run(&["rev-list", "--objects", "--missing=error", &oid])
            .await
            .is_ok()
    }

    /// True when the object exists in this repository.
    pub async fn has_object(&self, oid: &Oid) -> bool {
        self.run(&["cat-file", "-e", &oid.to_hex()]).await.is_ok()
    }

    /// Return only the object IDs that exist here (WANT-2).
    pub async fn filter_existing(&self, oids: &[Oid]) -> Result<Vec<Oid>, StoreError> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = self.prefix();
        let mut command = Command::new("git");
        command.args(&prefix);
        command.args(["cat-file", "--batch-check=%(objectname)"]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::null());
        let mut child = command.spawn()?;
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| StoreError::Git("cat-file has no stdin".into()))?;
            for oid in oids {
                stdin.write_all(oid.to_hex().as_bytes()).await?;
                stdin.write_all(b"\n").await?;
            }
            stdin.shutdown().await?;
        }
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(StoreError::Git("git cat-file failed".into()));
        }
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            // Missing objects print "<oid> missing"; existing ones print only
            // the object name because of the custom format.
            let text = line.trim();
            if text.len() == 40 || text.len() == 64 {
                if let Ok(bytes) = hex::decode(text) {
                    out.push(Oid::new(bytes));
                }
            }
        }
        Ok(out)
    }

    /// Fetch refs from another local repository.
    pub async fn fetch_from(&self, source: &Path, refspecs: &[String]) -> Result<(), StoreError> {
        let source = source.to_string_lossy().into_owned();
        let mut args = vec![
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "fetch",
            "--no-tags",
            source.as_str(),
        ];
        for refspec in refspecs {
            args.push(refspec.as_str());
        }
        self.run(&args).await.map(|_| ())
    }

    /// Build a pack for the given tips and return it on stdout (spec 8.2).
    pub async fn pack_objects(&self, wants: &[Oid], haves: &[Oid]) -> Result<Vec<u8>, StoreError> {
        self.pack_objects_limited(wants, haves, usize::MAX).await
    }

    /// Build a pack, rejecting output above `max_bytes`.
    pub async fn pack_objects_limited(
        &self,
        wants: &[Oid],
        haves: &[Oid],
        max_bytes: usize,
    ) -> Result<Vec<u8>, StoreError> {
        let prefix = self.prefix();
        let mut command = Command::new("git");
        command.args(&prefix);
        command.args([
            "pack-objects",
            "--stdout",
            "--revs",
            "--thin",
            "--delta-base-offset",
        ]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command.spawn()?;
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| StoreError::Git("pack-objects has no stdin".into()))?;
            for oid in wants {
                stdin.write_all(oid.to_hex().as_bytes()).await?;
                stdin.write_all(b"\n").await?;
            }
            for oid in haves {
                stdin.write_all(b"^").await?;
                stdin.write_all(oid.to_hex().as_bytes()).await?;
                stdin.write_all(b"\n").await?;
            }
            stdin.shutdown().await?;
        }
        let output = child.wait_with_output().await?;
        if output.status.success() {
            if output.stdout.len() > max_bytes {
                return Err(StoreError::Core(swarm_core::ErrorCode::LimitExceeded));
            }
            Ok(output.stdout)
        } else {
            Err(StoreError::Git(format!(
                "git pack-objects failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    /// Index a pack from bytes with `--stdin --fix-thin --strict` (SEC-6).
    pub async fn index_pack(&self, pack: &[u8]) -> Result<String, StoreError> {
        self.index_pack_limited(pack, usize::MAX).await
    }

    /// Index a pack with an explicit input cap.
    pub async fn index_pack_limited(
        &self,
        pack: &[u8],
        max_bytes: usize,
    ) -> Result<String, StoreError> {
        // `--max-input-size` exists in newer git; older releases reject it.
        // We already cap the bytes in memory, so retry without the flag when
        // the installed git does not know it (SEC-6).
        let limit = format!("--max-input-size={max_bytes}");
        for attempt in 0..2 {
            let mut args = vec!["index-pack", "--stdin", "--fix-thin", "--strict"];
            if attempt == 0 && max_bytes != usize::MAX {
                args.push(limit.as_str());
            }
            let prefix = self.prefix();
            let mut command = Command::new("git");
            command.args(&prefix);
            command.args(&args);
            command.stdin(Stdio::piped());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            let mut child = command.spawn()?;
            {
                use tokio::io::AsyncWriteExt;
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| StoreError::Git("index-pack has no stdin".into()))?;
                stdin.write_all(pack).await?;
                stdin.shutdown().await?;
            }
            let output = child.wait_with_output().await?;
            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if attempt == 0 && stderr.contains("max-input-size") {
                continue;
            }
            return Err(StoreError::Git(format!(
                "git index-pack failed: {}",
                stderr.trim()
            )));
        }
        Err(StoreError::Git("git index-pack failed".into()))
    }
}

/// Create a bare store with no template (SEC-7) and the requested object format.
pub async fn init_bare(path: &Path, object_format: &str) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = path.to_string_lossy().into_owned();
    let output = command(&[
        "init",
        "--bare",
        "--template=",
        &format!("--object-format={object_format}"),
        &path,
    ])
    .output()
    .await?;
    finish(output, "git init").await.map(|_| ())
}

/// Clone a bare store's objects into a new bare store (section 8.4).
pub async fn clone_bare(source: &Path, dest: &Path) -> Result<(), StoreError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let source = source.to_string_lossy().into_owned();
    let dest = dest.to_string_lossy().into_owned();
    let output = command(&["clone", "--bare", "--no-hardlinks", &source, &dest])
        .output()
        .await?;
    finish(output, "git clone --bare").await.map(|_| ())
}

/// Object format of a worktree (`sha1` or `sha256`).
pub async fn worktree_object_format(dir: &Path) -> Result<String, StoreError> {
    let git = Git::worktree(dir);
    let format = git.output(&["rev-parse", "--show-object-format"]).await?;
    if format == "sha1" || format == "sha256" {
        Ok(format)
    } else {
        Err(StoreError::Message(format!(
            "unsupported git object format {format}"
        )))
    }
}

/// True when `dir` is inside a git worktree.
pub async fn is_git_worktree(dir: &Path) -> bool {
    Git::worktree(dir)
        .output(&["rev-parse", "--git-dir"])
        .await
        .is_ok()
}

/// The worktree's symbolic `HEAD`, such as `refs/heads/main`.
pub async fn worktree_head(dir: &Path) -> Option<String> {
    Git::worktree(dir)
        .output(&["symbolic-ref", "-q", "HEAD"])
        .await
        .ok()
        .filter(|value| !value.is_empty())
}

/// Initialize a worktree, trying sha256 first when no format is forced.
pub async fn init_worktree(dir: &Path, format: Option<&str>) -> Result<String, StoreError> {
    let requested = format.unwrap_or("sha256");
    let mut args = vec!["init", "-b", "main"];
    if requested == "sha256" {
        args.push("--object-format=sha256");
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir);
    cmd.args(&args);
    cmd.stdin(Stdio::null());
    let output = cmd.output().await?;
    if output.status.success() {
        return worktree_object_format(dir).await;
    }
    if format.is_none() {
        // Older git: fall back to sha1 and let the caller print the warning.
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-b", "main"])
            .output()
            .await?;
        finish(output, "git init").await?;
        return Ok("sha1".to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(StoreError::Git(format!(
        "git init failed: {}",
        stderr.trim()
    )))
}

/// Fetch one advertised object from a local store into a worktree (SEC-8).
pub async fn fetch_oid_into_worktree(
    worktree: &Path,
    store: &Path,
    oid: &Oid,
) -> Result<(), StoreError> {
    let store = store.to_string_lossy().into_owned();
    let oid = oid.to_hex();
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "fetch",
            "--quiet",
            "--no-tags",
            &store,
            &oid,
        ])
        .output()
        .await?;
    finish(output, "git fetch").await.map(|_| ())
}

/// Push one ref from a worktree into the store.
pub async fn push_ref(
    worktree: &Path,
    store: &Path,
    src: &str,
    dst: &str,
    force: bool,
) -> Result<(), StoreError> {
    let store = store.to_string_lossy().into_owned();
    let refspec = format!("{src}:{dst}");
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(worktree).arg("push");
    if force {
        cmd.arg("--force");
    }
    cmd.arg(&store).arg(&refspec);
    let output = cmd.output().await?;
    finish(output, "git push").await.map(|_| ())
}

/// Fetch advertised objects into the caller's repository (remote helper).
///
/// The helper process inherits `GIT_DIR` from git, so this deliberately runs
/// without `-C` and lets the environment select the destination repository.
pub async fn fetch_oids_inherited(store: &Path, oids: &[String]) -> Result<(), StoreError> {
    if oids.is_empty() {
        return Ok(());
    }
    let store = store.to_string_lossy().into_owned();
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "fetch.fsckObjects=true",
        "-c",
        "transfer.fsckObjects=true",
        "fetch",
        "--quiet",
        "--no-tags",
        store.as_str(),
    ]);
    for oid in oids {
        cmd.arg(oid);
    }
    let output = cmd.output().await?;
    finish(output, "git fetch").await.map(|_| ())
}

/// Push one ref into a store from the caller's repository (remote helper).
///
/// Like `fetch_oids_inherited`, this relies on the inherited `GIT_DIR`.
pub async fn push_ref_inherited(
    store: &Path,
    refspec: &str,
    force: bool,
) -> Result<(), StoreError> {
    let store = store.to_string_lossy().into_owned();
    let mut cmd = Command::new("git");
    cmd.arg("push");
    if force {
        cmd.arg("--force");
    }
    cmd.arg(&store).arg(refspec);
    let output = cmd.output().await?;
    finish(output, "git push").await.map(|_| ())
}
