//! The persisted ban list (spec 7.4 and 11.10): schema version 2.

use rusqlite::{params, OptionalExtension};

use crate::{Db, StoreError};

/// Schema version after every migration.
pub(crate) const SCHEMA_VERSION: i64 = 2;

/// Version 1 to version 2: add the ban list. Version 1 itself never changes.
pub(crate) const MIGRATION_2: &str = r#"
BEGIN;
CREATE TABLE IF NOT EXISTS bans(ip TEXT PRIMARY KEY, until INTEGER NOT NULL);
PRAGMA user_version = 2;
COMMIT;
"#;

impl Db {
    /// Ignore `ip` until `until`, Unix seconds.
    pub fn ban(&self, ip: &str, until: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO bans(ip, until) VALUES(?1, ?2)
             ON CONFLICT(ip) DO UPDATE SET until = max(until, excluded.until)",
            params![ip, until],
        )?;
        Ok(())
    }

    /// True while a ban on `ip` has not run out at `now`.
    pub fn is_banned(&self, ip: &str, now: i64) -> Result<bool, StoreError> {
        let until: Option<i64> = self
            .conn
            .query_row("SELECT until FROM bans WHERE ip = ?1", params![ip], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(until.is_some_and(|until| until > now))
    }

    /// Forget bans that ran out before `now`.
    pub fn prune_bans(&self, now: i64) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM bans WHERE until <= ?1", params![now])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ban_survives_reopening_and_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swarm.db");
        let db = Db::open(&path).unwrap();
        db.ban("198.51.100.7", 100).unwrap();
        // A shorter ban does not cut a longer one.
        db.ban("198.51.100.7", 50).unwrap();
        drop(db);
        let db = Db::open(&path).unwrap();
        assert!(db.is_banned("198.51.100.7", 99).unwrap());
        assert!(!db.is_banned("198.51.100.7", 100).unwrap());
        assert!(!db.is_banned("198.51.100.8", 0).unwrap());
        db.prune_bans(100).unwrap();
        assert!(!db.is_banned("198.51.100.7", 0).unwrap());
    }

    #[test]
    fn a_version_1_database_migrates_and_keeps_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swarm.db");
        let old = rusqlite::Connection::open(&path).unwrap();
        old.execute_batch(
            "PRAGMA user_version = 1;
             CREATE TABLE dht_nodes(addr TEXT PRIMARY KEY, last_ok INTEGER NOT NULL);
             INSERT INTO dht_nodes VALUES('192.0.2.1:6881', 1);",
        )
        .unwrap();
        drop(old);
        let db = Db::open(&path).unwrap();
        db.ban("192.0.2.9", 10).unwrap();
        assert_eq!(db.dht_nodes(8).unwrap(), ["192.0.2.1:6881"]);
    }
}
