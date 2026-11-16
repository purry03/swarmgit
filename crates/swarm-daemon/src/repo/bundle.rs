//! Bundles (spec 6.6): one file that carries a repo and its signed state.
//!
//! ```text
//! SWGB1\n | u32 n | n x (u32 len, envelope) | u64 git_len | git bundle
//! ```
//!
//! Import never trusts the file: only signed manifests decide which refs
//! exist (BUN-3), and nothing is stored until everything has been checked.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use swarm_core::accept::{AcceptOutcome, AcceptState};
use swarm_core::consts::MAGIC_BUNDLE;
use swarm_core::ids::RepoId;
use swarm_core::manifest::{verify_envelope, VerifiedEnvelope};
use swarm_store::RepoStore;

use super::{arg_str, db_repo, now, parse_repo};
use crate::protocol::IpcError;
use crate::state::DaemonState;

/// Most envelopes a bundle may carry (one per signer).
const MAX_ENVELOPES: u32 = 4096;
/// Size cap of one envelope (5.4 step 1).
const MAX_ENVELOPE: u32 = 1 << 20;

fn io_error(err: std::io::Error) -> IpcError {
    IpcError::new("malformed", err.to_string())
}

/// `export`: write a bundle for a repo held in the store (BUN-1).
pub async fn export(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let repo = parse_repo(arg_str(args, "repo")?)?;
    let path = PathBuf::from(arg_str(args, "path")?);
    let _ = db_repo(state, repo)?;
    let store = RepoStore::new(state.home.clone(), repo);
    if !store.exists() {
        return Err(IpcError::new("unknown_repo", "store does not exist"));
    }
    let written = write_bundle(&store, &path).await?;
    Ok(json!({
        "path": path.to_string_lossy(),
        "bytes": written.bytes,
        "manifests": written.manifests,
        "git": written.git,
    }))
}

/// What [`write_bundle`] produced.
pub(crate) struct Written {
    pub bytes: u64,
    pub manifests: usize,
    pub git: bool,
}

/// Write the bundle file for `store` to `path` (atomically).
pub(crate) async fn write_bundle(store: &RepoStore, path: &Path) -> Result<Written, IpcError> {
    let accepted = store.load_state()?;
    if accepted.signers.is_empty() {
        return Err(IpcError::new("unknown_repo", "repo has no manifests"));
    }
    let git_tmp = store.dir().join("export.tmp.bundle");
    let _ = fs::remove_file(&git_tmp);
    let git = store.git();
    let has_refs = !git.refs().await?.is_empty();
    if has_refs {
        let tmp = git_tmp.to_string_lossy().into_owned();
        git.output(&["bundle", "create", &tmp, "--all"]).await?;
    }
    let result = assemble(&accepted, has_refs.then_some(git_tmp.as_path()), path);
    let _ = fs::remove_file(&git_tmp);
    result
}

fn assemble(
    accepted: &AcceptState,
    git_file: Option<&Path>,
    path: &Path,
) -> Result<Written, IpcError> {
    let part = path.with_extension("swarmbundle.part");
    let manifests = accepted.signers.len();
    let outcome = (|| -> std::io::Result<u64> {
        let mut out = BufWriter::new(File::create(&part)?);
        out.write_all(MAGIC_BUNDLE)?;
        out.write_all(&(manifests as u32).to_be_bytes())?;
        for stored in accepted.signers.values() {
            out.write_all(&(stored.envelope.len() as u32).to_be_bytes())?;
            out.write_all(&stored.envelope)?;
        }
        let git_len = match git_file {
            Some(file) => fs::metadata(file)?.len(),
            None => 0,
        };
        out.write_all(&git_len.to_be_bytes())?;
        if let Some(file) = git_file {
            std::io::copy(&mut File::open(file)?, &mut out)?;
        }
        out.flush()?;
        out.into_inner()?.sync_all()?;
        fs::rename(&part, path)?;
        Ok(fs::metadata(path)?.len())
    })();
    match outcome {
        Ok(bytes) => Ok(Written {
            bytes,
            manifests,
            git: git_file.is_some(),
        }),
        Err(err) => {
            let _ = fs::remove_file(&part);
            Err(io_error(err))
        }
    }
}

/// A bundle file after parsing and signature checks.
struct Parsed {
    envelopes: Vec<VerifiedEnvelope>,
    git_offset: u64,
    git_len: u64,
}

fn read_u32(reader: &mut impl Read) -> Result<u32, IpcError> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| IpcError::new("malformed", "truncated bundle"))?;
    Ok(u32::from_be_bytes(bytes))
}

fn parse(path: &Path) -> Result<Parsed, IpcError> {
    let file = File::open(path).map_err(io_error)?;
    let total = file.metadata().map_err(io_error)?.len();
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; MAGIC_BUNDLE.len()];
    reader
        .read_exact(&mut magic)
        .map_err(|_| IpcError::new("malformed", "not a swarm bundle"))?;
    if magic != MAGIC_BUNDLE {
        return Err(IpcError::new("malformed", "not a swarm bundle"));
    }
    let count = read_u32(&mut reader)?;
    if count == 0 || count > MAX_ENVELOPES {
        return Err(IpcError::new("malformed", "bad manifest count"));
    }
    let mut offset = (MAGIC_BUNDLE.len() + 4) as u64;
    let mut envelopes = Vec::new();
    for _ in 0..count {
        let len = read_u32(&mut reader)?;
        if len == 0 || len > MAX_ENVELOPE {
            return Err(IpcError::new("too_large", "bad envelope length"));
        }
        offset += 4 + u64::from(len);
        if offset > total {
            return Err(IpcError::new("malformed", "truncated bundle"));
        }
        let mut bytes = vec![0u8; len as usize];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| IpcError::new("malformed", "truncated bundle"))?;
        let envelope = verify_envelope(&bytes)
            .map_err(|err| IpcError::new(err.to_string(), err.to_string()))?;
        envelopes.push(envelope);
    }
    let mut len_bytes = [0u8; 8];
    reader
        .read_exact(&mut len_bytes)
        .map_err(|_| IpcError::new("malformed", "truncated bundle"))?;
    let git_len = u64::from_be_bytes(len_bytes);
    offset += 8;
    if total.checked_sub(offset) != Some(git_len) {
        return Err(IpcError::new("malformed", "bundle length mismatch"));
    }
    let first = envelopes[0].manifest.repo_id;
    if envelopes.iter().any(|env| env.manifest.repo_id != first) {
        return Err(IpcError::new("wrong_repo", "bundle mixes several repos"));
    }
    Ok(Parsed {
        envelopes,
        git_offset: offset,
        git_len,
    })
}

/// `import`: read a bundle file (BUN-2).
pub async fn import(state: &DaemonState, args: &Value) -> Result<Value, IpcError> {
    let path = PathBuf::from(arg_str(args, "path")?);
    import_file(state, &path).await
}

/// Import a bundle file: check every signature and the git data, then store
/// the manifests and move the canonical refs. A rejected bundle leaves the
/// store as it was.
pub(crate) async fn import_file(state: &DaemonState, path: &Path) -> Result<Value, IpcError> {
    let parsed = parse(path)?;
    let repo = RepoId::from_bytes(parsed.envelopes[0].manifest.repo_id);
    let store = RepoStore::new(state.home.clone(), repo);
    let lock = state.net().map(|net| net.sync_lock(repo));
    let _guard = match &lock {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };

    let is_new = !store.exists();
    let mut accepted = if is_new {
        AcceptState::new(repo)
    } else {
        store.load_state()?
    };
    let total = parsed.envelopes.len();
    let (mut fresh, mut stale, mut rejected) = (0usize, 0usize, 0usize);
    let mut first_error = None;
    for outcome in accepted.accept_batch(parsed.envelopes) {
        match outcome {
            Ok(AcceptOutcome::Accepted { .. }) => fresh += 1,
            Ok(_) => stale += 1,
            Err(swarm_core::ErrorCode::Stale) => stale += 1,
            Err(code) => {
                rejected += 1;
                first_error.get_or_insert(code);
            }
        }
    }
    if accepted.signers.is_empty() {
        let code = first_error.map_or_else(|| "unauthorized".to_string(), |c| c.to_string());
        return Err(IpcError::new(
            code.clone(),
            format!("bundle rejected: {code}"),
        ));
    }
    let object_format = match (&accepted.object_format, is_new) {
        (Some(format), _) => format.clone(),
        (None, false) => db_repo(state, repo)?.object_format,
        (None, true) => {
            return Err(IpcError::new("malformed", "bundle has no root manifest"));
        }
    };

    if is_new {
        store.create(&object_format).await?;
    }
    if parsed.git_len > 0 {
        if let Err(err) = import_git(&store, path, parsed.git_offset, parsed.git_len).await {
            if is_new {
                let _ = fs::remove_dir_all(store.dir());
            } else {
                let _ = store.remove_temp_refs().await;
            }
            return Err(err);
        }
    }

    store.persist_state(&accepted)?;
    {
        let db = state
            .db
            .lock()
            .map_err(|_| IpcError::new("malformed", "database lock poisoned"))?;
        db.ensure_repo(repo, &object_format, now())?;
        for (signer, stored) in &accepted.signers {
            db.upsert_signer(repo, *signer, stored.manifest.seq, &stored.manifest_id)?;
        }
        if is_new {
            db.set_seeding(repo, true)?;
        }
        db.set_last_sync(repo, now())?;
    }
    let resolved = store.resolve(&accepted);
    store.apply_canonical(&resolved.canonical).await?;
    store.remove_temp_refs().await?;
    super::admin::announce_routes(state, repo);
    let seq = super::root_seq(&accepted, repo);
    let base32 = repo.to_base32();
    Ok(json!({
        "repo_id": base32,
        "link": format!("swarm://{base32}"),
        "manifests": total,
        "accepted": fresh,
        "stale": stale,
        "rejected": rejected,
        "refs": resolved.canonical.len(),
        "seq": seq,
    }))
}

/// Verify the embedded git bundle and fetch it into `refs/swarm/in/*`.
async fn import_git(store: &RepoStore, path: &Path, offset: u64, len: u64) -> Result<(), IpcError> {
    use std::io::Seek;
    let tmp = store.dir().join("import.tmp.bundle");
    let copy = (|| -> std::io::Result<()> {
        let mut input = File::open(path)?;
        input.seek(std::io::SeekFrom::Start(offset))?;
        let mut out = File::create(&tmp)?;
        std::io::copy(&mut input.take(len), &mut out)?;
        out.sync_all()
    })();
    if let Err(err) = copy {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(err));
    }
    let file = tmp.to_string_lossy().into_owned();
    let git = store.git();
    let result = async {
        git.output(&["bundle", "verify", &file]).await?;
        git.output(&["fetch", "--quiet", &file, "+refs/*:refs/swarm/in/*"])
            .await?;
        Ok::<(), swarm_store::StoreError>(())
    }
    .await;
    let _ = fs::remove_file(&tmp);
    result.map_err(|err| IpcError::new("pack_invalid", format!("bundle git data: {err}")))
}
