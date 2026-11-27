//! `$SWARM_HOME/relays.db`: relays verified in earlier runs and their track
//! record (relay design spec REL-27, REL-31). It has its own schema version.

use std::path::Path;

use rusqlite::{params, Connection};

use crate::StoreError;

/// Schema version of `relays.db`.
const VERSION: i64 = 1;
/// Rows read back.
const MAX_ROWS: i64 = 256;

const SCHEMA: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS relays(
  addr TEXT PRIMARY KEY, relay_id BLOB NOT NULL,
  successes INTEGER NOT NULL, failures INTEGER NOT NULL, rtt_ms INTEGER NOT NULL);
PRAGMA user_version = 1;
COMMIT;
"#;

/// One saved relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayRow {
    /// `ip:port`.
    pub addr: String,
    /// The ID it reported.
    pub relay_id: [u8; 32],
    /// Probes and splices that worked.
    pub successes: u32,
    /// Failures since the last success.
    pub failures: u32,
    /// Last round-trip time.
    pub rtt_ms: u32,
}

fn open(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 => conn.execute_batch(SCHEMA)?,
        VERSION => {}
        other => {
            return Err(StoreError::Message(format!(
                "relays.db has schema version {other}, newer than this swarm"
            )))
        }
    }
    Ok(conn)
}

/// Read the saved relays. A missing file is an empty list.
pub fn load(path: &Path) -> Result<Vec<RelayRow>, StoreError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = open(path)?;
    let mut statement = conn.prepare(
        "SELECT addr, relay_id, successes, failures, rtt_ms FROM relays
         ORDER BY failures, successes DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![MAX_ROWS], |row| {
        Ok(RelayRow {
            addr: row.get(0)?,
            relay_id: row.get(1)?,
            successes: row.get(2)?,
            failures: row.get(3)?,
            rtt_ms: row.get(4)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Replace the saved relays with `rows`.
pub fn save(path: &Path, rows: &[RelayRow]) -> Result<(), StoreError> {
    let mut conn = open(path)?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM relays", [])?;
    for row in rows.iter().take(MAX_ROWS as usize) {
        tx.execute(
            "INSERT OR REPLACE INTO relays VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                row.addr,
                row.relay_id,
                row.successes,
                row.failures,
                row.rtt_ms
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_survive_and_a_newer_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relays.db");
        assert!(load(&path).unwrap().is_empty());
        let row = RelayRow {
            addr: "192.0.2.1:42425".into(),
            relay_id: [7; 32],
            successes: 3,
            failures: 0,
            rtt_ms: 12,
        };
        save(&path, std::slice::from_ref(&row)).unwrap();
        save(&path, std::slice::from_ref(&row)).unwrap();
        assert_eq!(load(&path).unwrap(), vec![row]);
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 9)
            .unwrap();
        assert!(load(&path).is_err());
    }
}
