// src/db/mod.rs

use anyhow::{Context as _, Result};
use rusqlite::params;
use rusqlite::OptionalExtension as _;
use std::collections::HashSet;

pub mod admin;
pub mod banners;
pub mod boards;
pub mod chan_net;
mod fs_ops;
mod migrations;
mod pool;
pub mod posts;
mod schema;
pub mod setup;
pub mod themes;
pub mod threads;
mod types;
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
pub struct DeletePathsResult {
    pub paths: Vec<String>,
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
