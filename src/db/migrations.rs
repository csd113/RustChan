// src/db/migrations.rs

use anyhow::{Context as _, Result};
use rusqlite::OptionalExtension as _;

/// Database schema version for the `RustChan` 1.3.0 release baseline.
pub(super) const BASELINE_SCHEMA_VERSION: &str = "1.3.0";

pub(super) fn read_schema_version(conn: &rusqlite::Connection) -> Result<Option<String>> {
    if !schema_version_table_exists(conn)? {
        return Ok(None);
    }

    conn.query_row(
        "SELECT CAST(version AS TEXT) FROM schema_version LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
    .context("Failed to read schema_version")
}

pub(super) fn stamp_schema_version(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin schema_version stamp to 1.3.0")?;

    let result = (|| {
        conn.execute_batch(
            "DROP TABLE IF EXISTS schema_version;
             CREATE TABLE schema_version (
                 version TEXT NOT NULL PRIMARY KEY
             );",
        )
        .context("Failed to recreate schema_version table")?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![BASELINE_SCHEMA_VERSION],
        )
        .context("Failed to set schema_version to 1.3.0")?;
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .context("Failed to commit schema_version stamp to 1.3.0"),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn schema_version_table_exists(conn: &rusqlite::Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM sqlite_master
            WHERE type = 'table' AND name = 'schema_version'
        )",
        [],
        |row| row.get(0),
    )
    .context("Failed to inspect schema_version table")
}

#[cfg(test)]
mod tests {
    use super::{read_schema_version, stamp_schema_version, BASELINE_SCHEMA_VERSION};

    #[test]
    fn missing_schema_version_reads_as_unversioned() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");

        assert_eq!(
            read_schema_version(&conn).expect("read schema version"),
            None
        );
    }

    #[test]
    fn stamp_replaces_existing_version_with_release_baseline() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);",
        )
        .expect("create legacy schema_version");

        stamp_schema_version(&conn).expect("stamp schema version");

        assert_eq!(
            read_schema_version(&conn).expect("read stamped schema version"),
            Some(BASELINE_SCHEMA_VERSION.to_owned())
        );
    }

    #[test]
    fn baseline_schema_version_matches_package_release() {
        assert_eq!(BASELINE_SCHEMA_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
