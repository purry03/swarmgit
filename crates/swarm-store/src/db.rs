//! Sqlite metadata, schema version 1 (spec section 9.4).

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use swarm_core::ids::{KeyId, RepoId};

use crate::StoreError;

const SCHEMA: &str = r#"
PRAGMA user_version = 1;
CREATE TABLE IF NOT EXISTS repos(
  id BLOB PRIMARY KEY, object_format TEXT NOT NULL,
  seeding INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL, last_sync INTEGER);
CREATE TABLE IF NOT EXISTS signers(
  repo BLOB NOT NULL, signer BLOB NOT NULL, seq INTEGER NOT NULL, manifest_id BLOB NOT NULL,
  PRIMARY KEY(repo, signer));
CREATE TABLE IF NOT EXISTS equivocations(
  repo BLOB NOT NULL, signer BLOB NOT NULL, seq INTEGER NOT NULL,
  id_a BLOB NOT NULL, id_b BLOB NOT NULL, seen INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS peers(
  repo BLOB NOT NULL, addr TEXT NOT NULL, node_id BLOB, last_ok INTEGER, fails INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(repo, addr));
CREATE TABLE IF NOT EXISTS dht_nodes(addr TEXT PRIMARY KEY, last_ok INTEGER NOT NULL);
"#;

/// One row of the `repos` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRow {
    /// Repo key ID.
    pub id: RepoId,
    /// `sha1` or `sha256`.
    pub object_format: String,
    /// Whether the daemon keeps serving it.
    pub seeding: bool,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// Last successful sync, Unix seconds.
    pub last_sync: Option<i64>,
}

/// One row of the `signers` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerRow {
    /// Signer key ID.
    pub signer: KeyId,
    /// Latest accepted sequence number.
    pub seq: u64,
    /// Latest manifest ID.
    pub manifest_id: [u8; 32],
}

/// One row of the `equivocations` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquivocationRow {
    /// Signer key ID.
    pub signer: KeyId,
    /// Conflicting sequence number.
    pub seq: u64,
    /// First manifest ID.
    pub id_a: [u8; 32],
    /// Second manifest ID.
    pub id_b: [u8; 32],
    /// When it was seen, Unix seconds.
    pub seen: i64,
}

/// A sqlite handle. Callers serialize access with a mutex.
pub struct Db {
    conn: Connection,
}

fn blob32(bytes: &[u8]) -> Result<[u8; 32], StoreError> {
    bytes
        .try_into()
        .map_err(|_| StoreError::Message("database blob is not 32 bytes".into()))
}

impl Db {
    /// Open and migrate the database.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
        } else if version != 1 {
            return Err(StoreError::Message(format!(
                "unsupported database schema version {version}"
            )));
        }
        Ok(Self { conn })
    }

    /// Insert a repo row.
    pub fn insert_repo(
        &self,
        id: RepoId,
        object_format: &str,
        created: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO repos(id, object_format, seeding, created, last_sync) VALUES(?1, ?2, 0, ?3, NULL)",
            params![&id.as_bytes()[..], object_format, created],
        )?;
        Ok(())
    }

    /// Fetch one repo.
    pub fn repo(&self, id: RepoId) -> Result<Option<RepoRow>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, object_format, seeding, created, last_sync FROM repos WHERE id = ?1",
                params![&id.as_bytes()[..]],
                |row| {
                    let id: Vec<u8> = row.get(0)?;
                    Ok((
                        id,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, object_format, seeding, created, last_sync)) = row else {
            return Ok(None);
        };
        Ok(Some(RepoRow {
            id: RepoId::from_bytes(blob32(&id)?),
            object_format,
            seeding: seeding != 0,
            created,
            last_sync,
        }))
    }

    /// Every repo, oldest first.
    pub fn repos(&self) -> Result<Vec<RepoRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, object_format, seeding, created, last_sync FROM repos ORDER BY created, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, object_format, seeding, created, last_sync) = row?;
            out.push(RepoRow {
                id: RepoId::from_bytes(blob32(&id)?),
                object_format,
                seeding: seeding != 0,
                created,
                last_sync,
            });
        }
        Ok(out)
    }

    /// Update the seeding flag.
    pub fn set_seeding(&self, id: RepoId, seeding: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE repos SET seeding = ?2 WHERE id = ?1",
            params![&id.as_bytes()[..], i64::from(seeding)],
        )?;
        Ok(())
    }

    /// Record a successful sync time.
    pub fn set_last_sync(&self, id: RepoId, when: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE repos SET last_sync = ?2 WHERE id = ?1",
            params![&id.as_bytes()[..], when],
        )?;
        Ok(())
    }

    /// Insert or replace a signer row.
    pub fn upsert_signer(
        &self,
        repo: RepoId,
        signer: KeyId,
        seq: u64,
        manifest_id: &[u8; 32],
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO signers(repo, signer, seq, manifest_id) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(repo, signer) DO UPDATE SET seq = excluded.seq, manifest_id = excluded.manifest_id",
            params![&repo.as_bytes()[..], &signer.as_bytes()[..], seq as i64, &manifest_id[..]],
        )?;
        Ok(())
    }

    /// Latest sequence for one signer.
    pub fn signer_seq(&self, repo: RepoId, signer: KeyId) -> Result<Option<u64>, StoreError> {
        let value = self
            .conn
            .query_row(
                "SELECT seq FROM signers WHERE repo = ?1 AND signer = ?2",
                params![&repo.as_bytes()[..], &signer.as_bytes()[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(value.map(|v| v as u64))
    }

    /// All signer rows for a repo.
    pub fn signers(&self, repo: RepoId) -> Result<Vec<SignerRow>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT signer, seq, manifest_id FROM signers WHERE repo = ?1")?;
        let rows = statement.query_map(params![&repo.as_bytes()[..]], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (signer, seq, manifest_id) = row?;
            out.push(SignerRow {
                signer: KeyId::from_bytes(blob32(&signer)?),
                seq: seq as u64,
                manifest_id: blob32(&manifest_id)?,
            });
        }
        Ok(out)
    }

    /// Record an equivocation.
    pub fn insert_equivocation(
        &self,
        repo: RepoId,
        signer: KeyId,
        seq: u64,
        id_a: &[u8; 32],
        id_b: &[u8; 32],
        seen: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO equivocations(repo, signer, seq, id_a, id_b, seen) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &repo.as_bytes()[..],
                &signer.as_bytes()[..],
                seq as i64,
                &id_a[..],
                &id_b[..],
                seen
            ],
        )?;
        Ok(())
    }

    /// Equivocations recorded for a repo.
    pub fn equivocations(&self, repo: RepoId) -> Result<Vec<EquivocationRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT signer, seq, id_a, id_b, seen FROM equivocations WHERE repo = ?1 ORDER BY seen",
        )?;
        let rows = statement.query_map(params![&repo.as_bytes()[..]], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (signer, seq, id_a, id_b, seen) = row?;
            out.push(EquivocationRow {
                signer: KeyId::from_bytes(blob32(&signer)?),
                seq: seq as u64,
                id_a: blob32(&id_a)?,
                id_b: blob32(&id_b)?,
                seen,
            });
        }
        Ok(out)
    }

    /// Number of cached peers for a repo.
    pub fn peer_count(&self, repo: RepoId) -> Result<usize, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM peers WHERE repo = ?1",
            params![&repo.as_bytes()[..]],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trips_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("swarm.db")).unwrap();
        let repo = RepoId::from_bytes([1u8; 32]);
        let signer = KeyId::from_bytes([2u8; 32]);
        db.insert_repo(repo, "sha256", 10).unwrap();
        db.upsert_signer(repo, signer, 3, &[9u8; 32]).unwrap();
        db.insert_equivocation(repo, signer, 3, &[9u8; 32], &[8u8; 32], 11)
            .unwrap();
        let row = db.repo(repo).unwrap().unwrap();
        assert_eq!(row.object_format, "sha256");
        assert_eq!(db.signer_seq(repo, signer).unwrap(), Some(3));
        assert_eq!(db.equivocations(repo).unwrap().len(), 1);
        assert_eq!(db.repos().unwrap().len(), 1);
    }

    #[test]
    fn version_is_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swarm.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
        }
        assert!(Db::open(&path).is_err());
    }
}
