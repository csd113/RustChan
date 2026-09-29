//! Database access, schema management, and persistence helpers.

use anyhow::{Context as _, Result};
use rusqlite::params;
use rusqlite::OptionalExtension as _;
use std::collections::HashSet;

/// Administrative users, sessions, moderation, and database health queries.
pub mod admin;
/// Banner asset persistence and ordering.
pub mod banners;
/// Board configuration, statistics, and deletion operations.
pub mod boards;
/// Durable filesystem-operation records.
mod fs_ops;
/// Database schema-version bookkeeping.
mod migrations;
/// `SQLite` connection-pool creation and startup checks.
mod pool;
/// Post persistence, search, deletion, and background-job state.
pub mod posts;
/// Baseline schema installation, repair, and verification.
mod schema;
/// First-run setup state and completion markers.
pub mod setup;
/// Built-in and custom theme persistence.
pub mod themes;
/// Thread creation, mutation, pruning, and archive queries.
pub mod threads;
/// Shared database input and output types.
mod types;
/// Anonymous per-thread display preferences.
mod user_thread_prefs;

pub use pool::{first_run_check, has_no_admin, init_pool};
pub use types::{CachedFile, DbPool, NewPost};

#[cfg(test)]
pub use pool::init_test_pool;

pub use admin::*;
pub use banners::*;
pub use boards::*;
pub use fs_ops::*;
pub use posts::*;
pub use setup::*;
pub use themes::*;
pub use threads::*;
pub use user_thread_prefs::*;

/// Commit a transaction opened with an explicit `BEGIN IMMEDIATE`.
///
/// A failed `COMMIT` can leave the transaction active on a pooled connection.
/// Rolling back before returning the error keeps the next pool borrower from
/// inheriting an open transaction or a stale write lock.
pub(crate) fn commit_transaction(conn: &rusqlite::Connection, context: &'static str) -> Result<()> {
    if let Err(error) = conn.execute_batch("COMMIT") {
        drop(conn.execute_batch("ROLLBACK"));
        return Err(error).context(context);
    }
    Ok(())
}

/// Cheap readiness probe for public health endpoints.
///
/// This deliberately avoids the full structural and integrity verification in
/// [`verify_database_schema`]: that scan walks the whole database file and is
/// reserved for startup, the detailed readiness response, and admin health.
///
/// # Errors
/// Returns an error if the connection cannot answer a query or the recorded
/// schema version is not the release baseline.
pub fn database_ready_probe(conn: &rusqlite::Connection) -> Result<()> {
    schema::verify_database_ready(conn)
}

/// Return the database schema version for the current release baseline.
#[must_use]
pub const fn baseline_schema_version() -> &'static str {
    schema::baseline_schema_version()
}

/// Verify that the open database exactly matches the current release baseline.
///
/// # Errors
/// Returns an error if integrity checks fail, schema objects differ from the
/// baseline, or the recorded schema version is not current.
pub fn verify_database_schema(conn: &rusqlite::Connection) -> Result<()> {
    schema::verify_database_schema(conn)
}

/// Verify the database baseline and stamp the current schema version when safe.
///
/// # Errors
/// Returns an error if the database does not structurally match the current
/// release baseline or cannot be stamped with the current schema version.
pub fn normalize_database_schema_version(conn: &rusqlite::Connection) -> Result<()> {
    schema::normalize_database_schema_version(conn)
}

/// Return a human-readable database schema status label for diagnostics.
#[must_use]
pub fn database_schema_status_label(conn: &rusqlite::Connection) -> String {
    schema::database_schema_status_label(conn)
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Filesystem paths made safe by a committed database deletion.
pub struct DeletePathsResult {
    /// Relative paths that are no longer referenced by database rows.
    pub paths: Vec<String>,
    /// Identifier of the durable cleanup operation, when one was queued.
    pub pending_fs_op_id: Option<String>,
}

/// Build a pending filesystem delete operation for paths collected during a DB delete.
///
/// # Errors
/// Returns an error if the delete-files payload cannot be serialized.
pub fn build_delete_files_pending_op(
    paths: &[String],
) -> Result<Option<crate::pending_fs::PendingFsOpInsert>> {
    build_delete_files_and_dirs_pending_op(paths, &[])
}

/// Build a pending filesystem delete operation for file and board-directory cleanup.
///
/// # Errors
/// Returns an error if the delete-files payload cannot be serialized.
pub fn build_delete_files_and_dirs_pending_op(
    paths: &[String],
    dirs: &[String],
) -> Result<Option<crate::pending_fs::PendingFsOpInsert>> {
    if paths.is_empty() && dirs.is_empty() {
        return Ok(None);
    }

    let payload = crate::pending_fs::DeleteFilesPayload {
        paths: paths.to_vec(),
        dirs: dirs.to_vec(),
    };
    Ok(Some(crate::pending_fs::PendingFsOpInsert {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: crate::pending_fs::DELETE_FILES_KIND,
        payload_json: serde_json::to_string(&payload)
            .context("Serialize delete_files payload failed")?,
    }))
}

/// Given a list of candidate file paths collected from posts about to be deleted,
/// return only those paths that are no longer referenced by any remaining post.
///
/// Callers must invoke this inside the same transaction as their DELETE so no
/// concurrent insert can slip in between the row removal and the reference check.
///
/// # Errors
/// Returns an error if the candidate lookup or stale deduplication-row cleanup
/// fails.
pub fn paths_safe_to_delete(
    conn: &rusqlite::Connection,
    candidates: Vec<String>,
) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let unique: Vec<String> = candidates
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut unique = unique;
    unique.sort();

    if unique.is_empty() {
        return Ok(Vec::new());
    }

    let mut ref_stmt = conn
        .prepare(
            "SELECT 1 FROM posts
             WHERE file_path = ?1 OR thumb_path = ?1 OR audio_file_path = ?1
             LIMIT 1",
        )
        .context("Prepare safe-delete reference query failed")?;

    let mut safe = Vec::new();
    for path in &unique {
        let still_referenced = ref_stmt
            .query_row(params![path], |_r| Ok(()))
            .optional()
            .context("Query safe-delete candidate failed")?
            .is_some();
        if !still_referenced {
            safe.push(path.clone());
        }
    }

    safe.sort();
    let safe_set: HashSet<&str> = safe.iter().map(String::as_str).collect();
    for path in &safe {
        let maybe_row: Option<(String, String)> = conn
            .query_row(
                "SELECT file_path, thumb_path FROM file_hashes
                 WHERE file_path = ?1 OR thumb_path = ?1
                 LIMIT 1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .context("Query file_hashes safe-delete candidate failed")?;

        if let Some((file_path, _thumb_path)) = maybe_row {
            if safe_set.contains(file_path.as_str()) {
                conn.execute(
                    "DELETE FROM file_hashes WHERE file_path = ?1",
                    params![file_path],
                )
                .context("Delete stale file_hashes row failed")?;
            }
        }
    }

    Ok(safe)
}

#[cfg(test)]
/// Transaction-helper regression tests.
mod tests {
    use super::commit_transaction;
    use anyhow::{ensure, Context as _, Result};

    #[test]
    fn commit_transaction_rolls_back_after_a_failed_commit() -> Result<()> {
        let pool = super::init_test_pool()?;
        let conn = pool.get().context("get test database connection")?;
        conn.execute_batch("PRAGMA defer_foreign_keys = ON; BEGIN IMMEDIATE")
            .context("begin deferred transaction")?;
        // Deferred foreign keys turn the commit into a failure while the
        // transaction stays active, which is the state the helper must clean up.
        conn.execute_batch(
            "INSERT INTO polls (thread_id, question, expires_at) VALUES (999999, 'q', 0)",
        )
        .context("insert deferred-violation row")?;

        ensure!(
            commit_transaction(&conn, "test commit failure").is_err(),
            "a deferred foreign-key violation must fail the commit"
        );
        // Starting a new transaction proves the failed one was rolled back.
        conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK")
            .context("connection must remain usable after a failed commit")?;
        Ok(())
    }

    #[test]
    fn readiness_probe_requires_the_release_schema_version() -> Result<()> {
        let empty = rusqlite::Connection::open_in_memory().context("open schema-less database")?;
        ensure!(
            super::database_ready_probe(&empty).is_err(),
            "a database without the release schema must not report ready"
        );

        let pool = super::init_test_pool()?;
        let conn = pool.get().context("get test database connection")?;
        ensure!(
            super::database_ready_probe(&conn).is_ok(),
            "a release-baseline database must report ready"
        );
        Ok(())
    }
}
