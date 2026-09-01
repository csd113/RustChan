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
        .with_context(|| format!("Failed to set schema_version to {BASELINE_SCHEMA_VERSION}"))?;
        Ok(())
    })();

    match result {
        Ok(()) => conn.execute_batch("COMMIT").with_context(|| {
            format!("Failed to commit schema_version stamp to {BASELINE_SCHEMA_VERSION}")
        }),
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{read_schema_version, stamp_schema_version, BASELINE_SCHEMA_VERSION};
    use anyhow::Result;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn missing_schema_version_reads_as_unversioned() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;

        assert_eq!(
            read_schema_version(&conn)?,
            None,
            "a database without schema_version should be unversioned"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn stamp_replaces_existing_version_with_release_baseline() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);",
        )?;

        stamp_schema_version(&conn)?;

        assert_eq!(
            read_schema_version(&conn)?,
            Some(BASELINE_SCHEMA_VERSION.to_owned()),
            "stamping should replace the legacy version"
        );
        Ok(())
    }

    #[test]
    fn baseline_schema_version_matches_package_release() {
        assert_eq!(
            BASELINE_SCHEMA_VERSION,
            env!("CARGO_PKG_VERSION"),
            "the database baseline should match the package release"
        );
    }
}
