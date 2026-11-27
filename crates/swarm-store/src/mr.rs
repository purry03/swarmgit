//! Merge request refs in a store: `refs/mr/<merge request>/<author>`, each
//! the tip of one author's chain of signed commits.

use std::collections::BTreeMap;
use std::process::Stdio;

use swarm_core::ids::{KeyId, Oid};
use swarm_core::mr::{parse_mr_ref, verify_event, VerifiedEvent, MR_REF_PREFIX};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::store::RepoStore;
use crate::StoreError;

/// The tips of every author's chain, keyed by merge request and author.
pub type MrTips = BTreeMap<(String, KeyId), Oid>;

fn git_error(what: &str) -> StoreError {
    StoreError::Git(what.to_string())
}

/// One `git cat-file --batch` process, for reading chains commit by commit.
pub struct Objects {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Objects {
    /// The bytes of the commit `oid`, or none when it is missing, is not a
    /// commit, or is over `max` bytes.
    pub async fn commit(&mut self, oid: &str, max: usize) -> Result<Option<Vec<u8>>, StoreError> {
        if !swarm_core::mr::is_oid(oid) {
            return Ok(None);
        }
        self.stdin.write_all(format!("{oid}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        let mut header = String::new();
        if self.stdout.read_line(&mut header).await? == 0 {
            return Err(git_error("git cat-file stopped"));
        }
        let mut words = header.split_whitespace().skip(1);
        let (Some(kind), Some(size)) = (words.next(), words.next()) else {
            return Ok(None);
        };
        let size: usize = size.parse().map_err(|_| git_error("bad cat-file reply"))?;
        let wanted = kind == "commit" && size <= max;
        // The object and its newline must be read either way.
        let mut left = size + 1;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        while left > 0 {
            let take = left.min(buffer.len());
            self.stdout.read_exact(&mut buffer[..take]).await?;
            if wanted {
                bytes.extend_from_slice(&buffer[..take]);
            }
            left -= take;
        }
        bytes.pop();
        Ok(wanted.then_some(bytes))
    }

    /// The event in commit `oid`, when it is one and its signature holds.
    pub async fn event(&mut self, oid: &str) -> Result<Option<VerifiedEvent>, StoreError> {
        let bytes = self
            .commit(oid, swarm_core::consts::MAX_MR_EVENT_BYTES)
            .await?;
        Ok(bytes.and_then(|bytes| verify_event(oid, &bytes).ok()))
    }
}

impl RepoStore {
    /// The `refs/mr` refs. Ref names of any other shape are ignored.
    pub async fn mr_tips(&self) -> Result<MrTips, StoreError> {
        let text = self
            .git()
            .output(&[
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                MR_REF_PREFIX,
            ])
            .await?;
        let mut tips = MrTips::new();
        for line in text.lines() {
            let Some((oid, name)) = line.split_once(' ') else {
                continue;
            };
            let (Some(key), Ok(bytes)) = (parse_mr_ref(name), hex::decode(oid)) else {
                continue;
            };
            tips.insert(key, Oid::new(bytes));
        }
        Ok(tips)
    }

    /// Start reading objects from the store.
    pub fn objects(&self) -> Result<Objects, StoreError> {
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(self.store_path())
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| git_error("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| git_error("no stdout"))?;
        Ok(Objects {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// Every event reachable along the authors' chains that verifies and sits
    /// under the ref of the key that signed it.
    pub async fn mr_events(&self) -> Result<Vec<VerifiedEvent>, StoreError> {
        let tips = self.mr_tips().await?;
        let mut events = Vec::new();
        if tips.is_empty() {
            return Ok(events);
        }
        let mut objects = self.objects()?;
        for ((mr, author), tip) in tips {
            let mut next = Some(tip.to_hex());
            for _ in 0..swarm_core::consts::MAX_MR_CHAIN {
                let Some(oid) = next else { break };
                let Some(event) = objects.event(&oid).await? else {
                    break;
                };
                let belongs = event.event.mr.as_deref().unwrap_or(&event.id) == mr;
                if event.signer != author || !belongs {
                    break;
                }
                next = event.event.prev.clone();
                events.push(event);
            }
        }
        Ok(events)
    }

    /// Write an object that events point at: the empty tree.
    pub async fn ensure_empty_tree(&self) -> Result<(), StoreError> {
        self.hash_object("tree", b"").await.map(|_| ())
    }

    /// Write one object, returning its ID.
    pub async fn hash_object(&self, kind: &str, bytes: &[u8]) -> Result<String, StoreError> {
        let mut child = Command::new("git")
            .arg("--git-dir")
            .arg(self.store_path())
            .args(["hash-object", "-w", "--stdin", "-t", kind])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| git_error("no stdin"))?;
        stdin.write_all(bytes).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        if !output.status.success() {
            return Err(StoreError::Git(format!(
                "git hash-object failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Point an author's ref at `new`, only if it still holds `old`.
    pub async fn set_mr_ref(
        &self,
        mr: &str,
        author: KeyId,
        new: &str,
        old: Option<&str>,
    ) -> Result<(), StoreError> {
        let name = swarm_core::mr::mr_ref(mr, author);
        // All zeros: the ref must not exist yet.
        let zero = "0".repeat(new.len());
        self.git()
            .output(&["update-ref", &name, new, old.unwrap_or(&zero)])
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use swarm_core::crypto::key_id;
    use swarm_core::ids::RepoId;
    use swarm_core::mr::{Event, Kind};
    use swarm_core::pq::{HybridSigner, Signer as _};

    use crate::{git, Home, RepoStore};

    /// Events must be commits git itself accepts, in both object formats.
    #[tokio::test]
    async fn an_event_is_a_sound_commit() {
        for format in ["sha1", "sha256"] {
            let dir = tempfile::tempdir().unwrap();
            let repo = RepoId::from_bytes([3; 32]);
            let store = RepoStore::new(Home::from_root(dir.path()), repo);
            git::init_bare(&store.store_path(), format).await.unwrap();
            store.ensure_empty_tree().await.unwrap();
            let tree = store.hash_object("tree", b"").await.unwrap();
            let head =
                format!("tree {tree}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nwork\n");
            let head = store.hash_object("commit", head.as_bytes()).await.unwrap();

            let signer = HybridSigner::generate().unwrap();
            let mut event = Event::new(repo, Kind::Open, "Ada", 5);
            event.head = Some(head);
            event.target = Some("refs/heads/main".into());
            event.text = "Title".into();
            let bytes = event.sign(&signer).unwrap();
            let id = store.hash_object("commit", &bytes).await.unwrap();
            let author = key_id(&signer.bundle());
            store.set_mr_ref(&id, author, &id, None).await.unwrap();

            let walk = ["rev-list", "--objects", &id];
            store.git().output(&walk).await.unwrap();
            store.git().output(&["fsck", "--strict"]).await.unwrap();
            let events = store.mr_events().await.unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event, event);
        }
    }
}
