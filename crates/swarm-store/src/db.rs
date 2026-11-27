//! Sqlite metadata (spec section 9.4). Version 1 is below; later versions are
//! forward migrations on top of it.

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

/// One row of the `peers` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRow {
    /// TCP address text.
    pub addr: String,
    /// Last authenticated node ID.
    pub node_id: Option<[u8; 32]>,
    /// Last successful contact.
    pub last_ok: Option<i64>,
    /// Consecutive failures.
    pub fails: u32,
}

/// A sqlite handle. Callers serialize access with a mutex.
pub struct Db {
    pub(crate) conn: Connection,
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
        if !(0..=crate::bans::SCHEMA_VERSION).contains(&version) {
            return Err(StoreError::Message(format!(
                "unsupported database schema version {version}"
            )));
        }
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
        }
        if version < crate::bans::SCHEMA_VERSION {
            conn.execute_batch(crate::bans::MIGRATION_2)?;
        }
        // Added without a version bump: older binaries ignore it.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workdirs(
               repo BLOB NOT NULL, path TEXT NOT NULL, PRIMARY KEY(repo, path));",
        )?;
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

    /// Delete everything recorded about a repo (SEC-13).
    pub fn forget_repo(&self, id: RepoId) -> Result<(), StoreError> {
        for statement in [
            "DELETE FROM signers WHERE repo = ?1",
            "DELETE FROM equivocations WHERE repo = ?1",
            "DELETE FROM peers WHERE repo = ?1",
            "DELETE FROM workdirs WHERE repo = ?1",
            "DELETE FROM repos WHERE id = ?1",
        ] {
            self.conn.execute(statement, params![&id.as_bytes()[..]])?;
        }
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

    /// Remember a git directory on this machine that uses the repo.
    pub fn record_workdir(&self, repo: RepoId, path: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO workdirs(repo, path) VALUES(?1, ?2)",
            params![&repo.as_bytes()[..], path],
        )?;
        Ok(())
    }

    /// Git directories recorded for a repo.
    pub fn workdirs(&self, repo: RepoId) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT path FROM workdirs WHERE repo = ?1 ORDER BY path")?;
        let rows = statement.query_map(params![&repo.as_bytes()[..]], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
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

    /// Cached peers for a repo, best first.
    pub fn peers(&self, repo: RepoId) -> Result<Vec<PeerRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT addr, node_id, last_ok, fails FROM peers WHERE repo = ?1
             ORDER BY fails, last_ok DESC",
        )?;
        let rows = statement.query_map(params![&repo.as_bytes()[..]], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (addr, node_id, last_ok, fails) = row?;
            out.push(PeerRow {
                addr,
                node_id: match node_id {
                    Some(bytes) => Some(blob32(&bytes)?),
                    None => None,
                },
                last_ok,
                fails: fails as u32,
            });
        }
        Ok(out)
    }

    /// Stored node ID for one address, used as a pin (NET-11).
    pub fn peer_node(&self, repo: RepoId, addr: &str) -> Result<Option<[u8; 32]>, StoreError> {
        let value = self
            .conn
            .query_row(
                "SELECT node_id FROM peers WHERE repo = ?1 AND addr = ?2",
                params![&repo.as_bytes()[..], addr],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?;
        match value {
            Some(Some(bytes)) => Ok(Some(blob32(&bytes)?)),
            _ => Ok(None),
        }
    }

    /// Record a successful contact and keep the node ID as a pin.
    pub fn record_peer_ok(
        &self,
        repo: RepoId,
        addr: &str,
        node_id: &[u8; 32],
        when: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO peers(repo, addr, node_id, last_ok, fails) VALUES(?1, ?2, ?3, ?4, 0)
             ON CONFLICT(repo, addr) DO UPDATE SET
               node_id = excluded.node_id, last_ok = excluded.last_ok, fails = 0",
            params![&repo.as_bytes()[..], addr, &node_id[..], when],
        )?;
        Ok(())
    }

    /// Record a failure, deleting the peer after eight in a row (DM-4).
    pub fn record_peer_fail(&self, repo: RepoId, addr: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO peers(repo, addr, node_id, last_ok, fails) VALUES(?1, ?2, NULL, NULL, 1)
             ON CONFLICT(repo, addr) DO UPDATE SET fails = fails + 1",
            params![&repo.as_bytes()[..], addr],
        )?;
        self.conn.execute(
            "DELETE FROM peers WHERE repo = ?1 AND addr = ?2 AND fails >= 8",
            params![&repo.as_bytes()[..], addr],
        )?;
        Ok(())
    }

    /// Remember a PEX candidate without touching an existing row (6.5). A
    /// node ID, when given, becomes the pin for a new address.
    pub fn record_peer_hint(
        &self,
        repo: RepoId,
        addr: &str,
        node_id: Option<&[u8; 32]>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO peers(repo, addr, node_id, last_ok, fails)
             VALUES(?1, ?2, ?3, NULL, 0)",
            params![&repo.as_bytes()[..], addr, node_id.map(|id| id.to_vec())],
        )?;
        Ok(())
    }

    /// DHT nodes that answered before, newest first (DHT-3).
    pub fn dht_nodes(&self, limit: usize) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT addr FROM dht_nodes ORDER BY last_ok DESC LIMIT ?1")?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = statement.query_map(params![limit], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Replace the persisted DHT nodes, keeping at most `keep` of them.
    pub fn save_dht_nodes(
        &self,
        nodes: &[String],
        when: i64,
        keep: usize,
    ) -> Result<(), StoreError> {
        for addr in nodes {
            self.conn.execute(
                "INSERT INTO dht_nodes(addr, last_ok) VALUES(?1, ?2)
                 ON CONFLICT(addr) DO UPDATE SET last_ok = excluded.last_ok",
                params![addr, when],
            )?;
        }
        let keep = i64::try_from(keep).unwrap_or(i64::MAX);
        self.conn.execute(
            "DELETE FROM dht_nodes WHERE addr NOT IN
               (SELECT addr FROM dht_nodes ORDER BY last_ok DESC LIMIT ?1)",
            params![keep],
        )?;
        Ok(())
    }

    /// Create a repo row when it does not exist yet.
    pub fn ensure_repo(
        &self,
        id: RepoId,
        object_format: &str,
        created: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO repos(id, object_format, seeding, created, last_sync)
             VALUES(?1, ?2, 0, ?3, NULL)",
            params![&id.as_bytes()[..], object_format, created],
        )?;
        Ok(())
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
    fn dht_nodes_and_hints() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("swarm.db")).unwrap();
        db.save_dht_nodes(&["1.2.3.4:5".into(), "6.7.8.9:1".into()], 10, 1)
            .unwrap();
        assert_eq!(db.dht_nodes(10).unwrap().len(), 1);
        db.save_dht_nodes(&["9.9.9.9:9".into()], 20, 8).unwrap();
        assert_eq!(db.dht_nodes(10).unwrap()[0], "9.9.9.9:9");
        let repo = RepoId::from_bytes([1u8; 32]);
        db.record_peer_ok(repo, "a:1", &[3; 32], 5).unwrap();
        db.record_peer_hint(repo, "a:1", Some(&[4; 32])).unwrap();
        db.record_peer_hint(repo, "b:2", None).unwrap();
        assert_eq!(db.peer_node(repo, "a:1").unwrap(), Some([3; 32]));
        assert_eq!(db.peer_count(repo).unwrap(), 2);
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
