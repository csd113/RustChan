// src/db/mod.rs

use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::HashSet;

pub mod admin;
pub mod boards;
pub mod chan_net;
mod fs_ops;
mod migrations;
mod pool;
pub mod posts;
mod schema;
pub mod threads;
mod types;

pub use pool::{first_run_check, has_no_admin, init_pool};
pub use types::{CachedFile, DbPool, NewPost};

pub use admin::*;
pub use boards::*;
pub use fs_ops::*;
pub use posts::*;
pub use threads::*;

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

    if unique.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: String = unique
        .iter()
        .enumerate()
        .map(|(i, _)| format!("(?{})", i.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT v.p FROM (VALUES {placeholders}) AS v(p)
         WHERE NOT EXISTS (
             SELECT 1 FROM posts
             WHERE file_path = v.p OR thumb_path = v.p OR audio_file_path = v.p
         )"
    );

    let mut stmt = conn
        .prepare(&sql)
        .context("Prepare safe-delete candidate query failed")?;

    let safe: Vec<String> = stmt
        .query_map(rusqlite::params_from_iter(&unique), |r| r.get(0))
        .context("Query safe-delete candidates failed")?
        .collect::<rusqlite::Result<_>>()
        .context("Read safe-delete candidates failed")?;

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
            .ok();

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
