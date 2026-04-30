// src/db/migrations.rs

use anyhow::{Context, Result};

/// Post-squash schema version for the canonical baseline.
///
/// Earlier development builds used a long numbered ladder up to v41. Fresh
/// installs now create the complete current schema directly and stamp this
/// clean baseline as v1; pre-squash databases use one legacy bridge before
/// joining the same version line.
pub(super) const POST_SQUASH_SCHEMA_VERSION: i64 = 1;

pub(super) fn read_schema_version(conn: &rusqlite::Connection) -> Result<i64> {
    if !schema_version_table_exists(conn)? {
        return Ok(0);
    }

    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )
    .context("Failed to read schema_version")
}

pub(super) fn stamp_schema_version(conn: &rusqlite::Connection, version: i64) -> Result<()> {
    ensure_schema_version_table_has_row(conn)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .with_context(|| format!("Failed to begin schema_version stamp to v{version}"))?;

    let result = (|| {
        conn.execute("DELETE FROM schema_version", [])
            .context("Failed to clear schema_version table")?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![version],
        )
        .with_context(|| format!("Failed to set schema_version to v{version}"))?;
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .with_context(|| format!("Failed to commit schema_version stamp to v{version}")),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ensure_schema_version_table_has_row(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(version)
         );
         INSERT INTO schema_version (version)
         SELECT 0
         WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
    )
    .context("Failed to create schema_version table")
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
    use super::{read_schema_version, stamp_schema_version, POST_SQUASH_SCHEMA_VERSION};

    #[test]
    fn missing_schema_version_reads_as_legacy_zero() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");

        assert_eq!(read_schema_version(&conn).expect("read schema version"), 0);
    }

    #[test]
    fn stamp_replaces_existing_version_with_post_squash_baseline() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);",
        )
        .expect("create legacy schema_version");

        stamp_schema_version(&conn, POST_SQUASH_SCHEMA_VERSION).expect("stamp schema version");

        assert_eq!(
            read_schema_version(&conn).expect("read stamped schema version"),
            POST_SQUASH_SCHEMA_VERSION
        );
    }
}
