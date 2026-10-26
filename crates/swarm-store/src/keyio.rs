//! Private file helpers: 0600 modes and atomic replacement (spec 4.5).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use swarm_core::keyfile::check_mode;

use crate::StoreError;

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()))
}

fn open_private(path: &Path) -> Result<File, StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !replace && path.exists() {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        )));
    }
    let tmp = temp_path(path);
    let mut file = open_private(&tmp)?;
    let result = (|| -> Result<(), StoreError> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Write a new private file, refusing to replace an existing one.
pub fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    atomic_write(path, bytes, false)
}

/// Atomically replace a private file, used by `swarm key backup`.
pub fn write_private_replace(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    atomic_write(path, bytes, true)
}

/// Read a key file after enforcing the Unix permission rule.
///
/// Returns `None` when the file does not exist.
pub fn read_private(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        check_mode(mode)?;
    }
    Ok(Some(fs::read(path)?))
}

/// Copy a key file to another path with mode 0600.
pub fn copy_private(src: &Path, dst: &Path) -> Result<(), StoreError> {
    let bytes = read_private(src)?
        .ok_or_else(|| StoreError::Message(format!("key file {} does not exist", src.display())))?;
    write_private_replace(dst, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swarm_core::keyfile::KeyFileError;

    #[test]
    fn new_files_are_private_and_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.key");
        write_private_new(&path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert!(write_private_new(&path, b"two").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn replace_is_atomic_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.key");
        write_private_new(&path, b"one").unwrap();
        write_private_replace(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn loose_permissions_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.key");
        write_private_new(&path, b"one").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(matches!(
                read_private(&path),
                Err(StoreError::Key(KeyFileError::LoosePermissions))
            ));
        }
    }
}
