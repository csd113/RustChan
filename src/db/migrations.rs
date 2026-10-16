use anyhow::{Context as _, Result};
use rusqlite::OptionalExtension as _;

/// Database schema version for the current package release baseline.
pub(super) const BASELINE_SCHEMA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Read the recorded schema version, if the version table exists.
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

/// Atomically replace the version table with the release baseline version.
pub(super) fn stamp_schema_version(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE").with_context(|| {
        format!("Failed to begin schema_version stamp to {BASELINE_SCHEMA_VERSION}")
    })?;

    let result = stamp_schema_version_in_transaction(conn);

    match result {
        Ok(()) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                Err(error).with_context(|| {
                    format!("Failed to commit schema_version stamp to {BASELINE_SCHEMA_VERSION}")
                })
            }
        },
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Replace the version table while the caller holds a schema transaction.
pub(super) fn stamp_schema_version_in_transaction(conn: &rusqlite::Connection) -> Result<()> {
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
    .with_context(|| format!("Failed to set schema_version to {BASELINE_SCHEMA_VERSION}"))
    .map(|_affected_rows| ())?;
    Ok(())
}

/// Return whether the schema-version table exists.
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
