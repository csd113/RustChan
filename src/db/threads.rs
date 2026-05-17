// db/threads.rs — Thread-level queries.
//
// Covers: thread listing, creation (atomically with its OP post via a
// transaction), sticky/lock toggles, deletion, archive/prune logic.
//
// Dependency notes:
//   create_thread_with_op  → super::posts::create_post_inner  (OP insert)
//   delete_thread          → super::paths_safe_to_delete       (file safety)
//   prune_old_threads      → super::paths_safe_to_delete       (file safety)
//
use crate::models::Thread;
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

pub struct PollInsert<'a> {
    pub question: &'a str,
    pub options: &'a [String],
    pub expires_at: i64,
}

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a thread row. Column layout (must match every SELECT that calls this):
///   0  t.id           4  `t.bumped_at`    8  op.body       12 op.tripcode
///   1  `t.board_id`     5  t.locked       9  `op.file_path`  13 op.id (`op_id`)
///   2  t.subject      6  t.sticky       10 `op.thumb_path` 14 t.archived
///   3  `t.created_at`   7  `t.reply_count`  11 op.name       15 `image_count`
///
/// Extracted from three copy-pasted closures into a single helper,
/// eliminating the risk of the three copies diverging.
fn map_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: row.get(0)?,
        board_id: row.get(1)?,
        subject: row.get(2)?,
        created_at: row.get(3)?,
        bumped_at: row.get(4)?,
        locked: row.get::<_, i32>(5)? != 0,
        sticky: row.get::<_, i32>(6)? != 0,
        reply_count: row.get(7)?,
        op_body: row.get(8)?,
        op_file: row.get(9)?,
        op_thumb: row.get(10)?,
        op_name: row.get(11)?,
        op_tripcode: row.get(12)?,
        op_id: row.get(13)?,
        archived: row.get::<_, i32>(14)? != 0,
        image_count: row.get(15)?,
    })
}

// ─── File-path collection helper ──────────────────────────────────────────────

/// Collect all file paths (`file_path`, `thumb_path`, `audio_file_path`) for every
/// post in the given set of thread ids. Returns a flat Vec of non-null paths.
///
/// Extracted from `delete_thread` and `prune_old_threads` to eliminate
///   copy-pasted collection loops.
/// Uses a single JOIN query instead of one query per thread.
///
/// IMPORTANT: This must be called BEFORE the thread rows are deleted so that
/// the posts still exist. The CASCADE on threads→posts removes them atomically
/// with the thread row when you later execute DELETE FROM threads.
fn collect_thread_file_paths(
    conn: &rusqlite::Connection,
    thread_ids: &[i64],
) -> Result<Vec<String>> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }

    // Build WHERE thread_id IN (?, ?, ...) dynamically.
    let placeholders: String = thread_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT file_path, thumb_path, audio_file_path
         FROM posts WHERE thread_id IN ({placeholders})"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
        .query_map(rusqlite::params_from_iter(thread_ids), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut paths = Vec::new();
    for (f, t, a) in rows {
        if let Some(p) = f {
            paths.push(p);
        }
        if let Some(p) = t {
            paths.push(p);
        }
        if let Some(p) = a {
            paths.push(p);
        }
    }
    Ok(paths)
}

// ─── Board-index thread listing ───────────────────────────────────────────────

/// The canonical thread SELECT fragment shared by all listing queries.
///
/// Replaced the correlated `image_count` subquery (which ran once
/// per thread row) with a LEFT JOIN aggregation so the count is computed in a
/// single pass. The GROUP BY ensures one output row per thread.
const THREAD_SELECT: &str = "
    SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
           t.locked, t.sticky, t.reply_count,
           op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
           t.archived,
           COUNT(DISTINCT fp.id) AS image_count
    FROM threads t
    JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
    LEFT JOIN posts fp ON fp.thread_id = t.id AND fp.file_path IS NOT NULL";

/// Get paginated threads for a board with OP preview data.
/// Sticky threads float to the top, then sorted by most recent bump.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    let sql = format!(
        "{THREAD_SELECT}
         WHERE t.board_id = ?1 AND t.archived = 0
         GROUP BY t.id, op.id
         ORDER BY t.sticky DESC, t.bumped_at DESC
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let threads = stmt
        .query_map(params![board_id, limit, offset], map_thread)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn count_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 0",
        params![board_id],
        |r| r.get(0),
    )?)
}

/// Latest currently visible thread on a board, ordered by `(created_at, id)`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_latest_visible_thread_marker(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<Option<(i64, i64)>> {
    conn.query_row(
        "SELECT created_at, id
         FROM threads
         WHERE board_id = ?1 AND archived = 0
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
        params![board_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn get_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Option<Thread>> {
    let sql = format!(
        "{THREAD_SELECT}
         WHERE t.id = ?1
         GROUP BY t.id, op.id"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_row(params![thread_id], map_thread).optional()?)
}

// ─── Thread creation (atomic with OP post) ────────────────────────────────────

/// Create a thread, its OP post, and an optional poll atomically.
///
/// # Errors
/// Returns an error if any insert in the bundle fails.
pub fn create_thread_with_optional_poll(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
    post: &super::NewPost,
    submission_token: &str,
    poll: Option<&PollInsert<'_>>,
    pending_fs_op: Option<&crate::pending_fs::PendingFsOpInsert>,
) -> Result<(i64, i64, Option<i64>)> {
    // BEGIN IMMEDIATE acquires the write lock upfront to avoid SQLITE_BUSY
    // during the lock-upgrade step that DEFERRED transactions perform on first
    // write. With &Connection (not &mut Connection) we cannot use rusqlite's
    // typed Transaction::new(Immediate), so we issue the pragma directly.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin IMMEDIATE transaction for create_thread_with_optional_poll")?;

    let result: Result<(i64, i64, Option<i64>)> = (|| {
        let thread_id: i64 = conn.query_row(
            "INSERT INTO threads (board_id, subject) VALUES (?1, ?2) RETURNING id",
            params![board_id, subject],
            |r| r.get(0),
        )?;

        // Bind thread_id and is_op into the post struct. We avoid a Clone of
        // the entire struct by building a minimal wrapper with references.
        let post_with_thread = super::NewPost {
            thread_id,
            is_op: true,
            ..post.clone()
        };
        let post_id = super::posts::create_post_inner(conn, &post_with_thread)?;
        let poll_id = poll
            .map(|poll_insert| {
                super::posts::create_poll_inner(
                    conn,
                    thread_id,
                    poll_insert.question,
                    poll_insert.options,
                    poll_insert.expires_at,
                )
            })
            .transpose()?;

        if let Some(op) = pending_fs_op {
            super::insert_pending_fs_op(conn, op)?;
        }
        if let Some(ip_hash) = post.ip_hash.as_deref() {
            super::posts::record_post_submission(
                conn,
                submission_token,
                ip_hash,
                board_id,
                thread_id,
                post_id,
                true,
            )?;
        }

        Ok((thread_id, post_id, poll_id))
    })();

    match result {
        Ok(ids) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit create_thread_with_optional_poll transaction")?;
            Ok(ids)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Thread mutation ──────────────────────────────────────────────────────────

/// Insert a reply and update thread counters in one transaction.
///
/// # Errors
/// Returns an error if the reply insert or thread metadata update fails.
pub fn create_reply_with_thread_update(
    conn: &rusqlite::Connection,
    post: &super::NewPost,
    submission_token: &str,
    should_bump: bool,
    pending_fs_op: Option<&crate::pending_fs::PendingFsOpInsert>,
) -> Result<i64> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin create_reply_with_thread_update transaction")?;

    let result: Result<i64> = (|| {
        let post_id = super::posts::create_post_inner(conn, post)?;
        let updated = if should_bump {
            conn.execute(
                "UPDATE threads
                 SET bumped_at = unixepoch(),
                     reply_count = reply_count + 1
                 WHERE id = ?1",
                params![post.thread_id],
            )?
        } else {
            conn.execute(
                "UPDATE threads SET reply_count = reply_count + 1 WHERE id = ?1",
                params![post.thread_id],
            )?
        };
        if updated == 0 {
            anyhow::bail!(
                "Thread id {} not found while updating reply metadata",
                post.thread_id
            );
        }
        if let Some(op) = pending_fs_op {
            super::insert_pending_fs_op(conn, op)?;
        }
        if let Some(ip_hash) = post.ip_hash.as_deref() {
            super::posts::record_post_submission(
                conn,
                submission_token,
                ip_hash,
                post.board_id,
                post.thread_id,
                post_id,
                false,
            )?;
        }
        Ok(post_id)
    })();

    match result {
        Ok(post_id) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit create_reply_with_thread_update transaction")?;
            Ok(post_id)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn set_thread_sticky(conn: &rusqlite::Connection, thread_id: i64, sticky: bool) -> Result<()> {
    let updated = conn.execute(
        "UPDATE threads SET sticky = ?1 WHERE id = ?2",
        params![i32::from(sticky), thread_id],
    )?;
    if updated == 0 {
        anyhow::bail!("Thread id {thread_id} not found");
    }
    Ok(())
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn set_thread_locked(conn: &rusqlite::Connection, thread_id: i64, locked: bool) -> Result<()> {
    let updated = conn.execute(
        "UPDATE threads SET locked = ?1 WHERE id = ?2",
        params![i32::from(locked), thread_id],
    )?;
    if updated == 0 {
        anyhow::bail!("Thread id {thread_id} not found");
    }
    Ok(())
}

/// Move a thread to (or out of) the board archive.
///
/// The previous implementation used `SET archived = ?1, locked = ?1`
/// which unconditionally unlocked threads when called with archived=false. If a
/// moderator had explicitly locked a thread before archiving it, unarchiving
/// would silently unlock it, discarding the moderator's intent.
///
/// The new logic: archiving always locks the thread; unarchiving only restores
/// the archived flag and leaves locked untouched. Callers that want to unlock
/// a thread after unarchiving should call `set_thread_locked` separately.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn set_thread_archived(
    conn: &rusqlite::Connection,
    thread_id: i64,
    archived: bool,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE threads
         SET archived = ?1,
             locked   = CASE WHEN ?1 = 1 THEN 1 ELSE locked END
         WHERE id = ?2",
        params![i32::from(archived), thread_id],
    )?;
    if updated == 0 {
        anyhow::bail!("Thread id {thread_id} not found");
    }
    Ok(())
}

/// Delete a thread and return on-disk paths that are now safe to remove.
///
/// The previous SELECT → DELETE sequence was not wrapped in a
/// transaction. A concurrent delete (e.g. admin panel + prune running together)
/// could delete the posts between our SELECT and DELETE, causing the returned
/// path list to include paths that had already been cleaned up by the other
/// operation — producing spurious filesystem errors.
///
/// The full sequence is now atomic:
///   1. Collect file paths (while posts still exist)
///   2. DELETE the thread (CASCADE removes posts)
///   3. `paths_safe_to_delete` inside the transaction sees the post-delete state
///   4. COMMIT
///
/// # Errors
/// Returns an error if the database operation fails.
fn delete_thread_in_tx(
    conn: &rusqlite::Connection,
    thread_id: i64,
) -> crate::error::Result<crate::db::DeletePathsResult> {
    // Step 1: collect file paths from all posts in this thread.
    let candidates = collect_thread_file_paths(conn, &[thread_id])?;

    // Step 2: delete thread (CASCADE removes posts).
    let deleted = conn
        .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
        .context("Failed to delete thread")?;
    if deleted == 0 {
        return Err(crate::error::AppError::NotFound(format!(
            "Thread id {thread_id} not found"
        )));
    }

    // Step 3: determine which paths are now unreferenced.
    // paths_safe_to_delete sees the post-delete state because we're still
    // inside the same transaction.
    let safe = super::paths_safe_to_delete(conn, candidates)?;
    let pending_fs_op = super::build_delete_files_pending_op(&safe)?;
    if let Some(op) = pending_fs_op.as_ref() {
        super::insert_pending_fs_op(conn, op)?;
    }
    Ok(crate::db::DeletePathsResult {
        paths: safe,
        pending_fs_op_id: pending_fs_op.map(|op| op.id),
    })
}

/// Delete a thread within an already-open transaction after authorization has
/// been checked by the caller.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_thread_verified(
    conn: &rusqlite::Connection,
    thread_id: i64,
) -> crate::error::Result<crate::db::DeletePathsResult> {
    delete_thread_in_tx(conn, thread_id)
}

/// Delete a thread and return on-disk paths that are now safe to remove.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_thread(
    conn: &rusqlite::Connection,
    thread_id: i64,
) -> crate::error::Result<crate::db::DeletePathsResult> {
    // BEGIN IMMEDIATE acquires the write lock up-front, preventing SQLITE_BUSY
    // on the lock upgrade that DEFERRED (unchecked_transaction) suffers under WAL.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_thread transaction")?;

    let result: crate::error::Result<crate::db::DeletePathsResult> =
        delete_thread_in_tx(conn, thread_id);

    match result {
        Ok(safe) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_thread transaction")?;
            Ok(safe)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Archive / prune ──────────────────────────────────────────────────────────

/// Archive oldest non-sticky threads that exceed the board's `max_threads` limit.
///
/// Archived threads are locked and marked read-only; their content remains
/// accessible via `/{board}/archive`. Returns the count of threads archived
/// (no file deletion occurs).
///
/// The ID collection query is now inside the same transaction as
/// the UPDATEs, closing the TOCTOU race where a concurrent bump could change
/// the ordering between the SELECT and the UPDATE loop.
///
/// Replaced the N per-row UPDATE loop with a single bulk
/// UPDATE … WHERE id IN (…), which is both faster and more crash-safe.
///
/// Note: LIMIT -1 OFFSET ? is a SQLite-specific idiom for "skip the first
/// max rows, return everything else". It is not standard SQL. The LIMIT -1
/// means "no upper bound on the result set after the offset is applied".
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn archive_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin archive_old_threads transaction")?;

    let result: anyhow::Result<usize> = (|| {
        // Collect inside the transaction to prevent races with concurrent bumps.
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id FROM threads
                 WHERE board_id = ?1 AND sticky = 0 AND archived = 0
                 ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
            )?;
            // Bind `collected` explicitly so `stmt` is dropped before the
            // block ends — the MappedRows iterator borrows `stmt`, and the
            // compiler requires the borrow to end before the binding goes out
            // of scope at the closing `}`.
            let collected = stmt
                .query_map(params![board_id, max], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };

        let count = ids.len();
        if count == 0 {
            return Ok(0);
        }

        // Single bulk UPDATE instead of N individual statements.
        let placeholders: String = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i.saturating_add(1)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql =
            format!("UPDATE threads SET archived = 1, locked = 1 WHERE id IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(&ids))
            .context("Failed to bulk archive threads")?;

        Ok(count)
    })();

    match result {
        Ok(0) => {
            // Nothing to archive — roll back the (empty) transaction cleanly.
            let _ = conn.execute_batch("ROLLBACK");
            Ok(0)
        }
        Ok(count) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit archive_old_threads transaction")?;
            Ok(count)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Hard-delete oldest non-sticky, non-archived threads that exceed `max_threads`.
/// Used when a board has archiving disabled — threads are permanently removed.
///
/// Returns the on-disk paths that are now safe to delete (i.e. no longer
/// referenced by any remaining post after the prune). The caller is responsible
/// for actually removing these files from disk.
///
/// ID collection query is now inside the transaction (see above).
///
/// `paths_safe_to_delete` is called INSIDE the transaction before
/// COMMIT so it sees the post-delete state atomically. Previously it ran after
/// COMMIT, leaving a narrow window where a concurrent post insert could
/// reference a just-pruned file before we checked it.
///
/// `prepare_cached` is now used OUTSIDE the loop (was documented as
/// fixed but the `prepare_cached` call was still inside the loop).
///
/// Replaced the N per-row DELETE loop with a single bulk DELETE.
///
/// File-path collection is now a single JOIN query instead of
/// one query per thread id.
///
/// Note: LIMIT -1 OFFSET ? is a SQLite-specific idiom — see `archive_old_threads`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn prune_old_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<crate::db::DeletePathsResult> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin prune_old_threads transaction")?;

    let result: anyhow::Result<crate::db::DeletePathsResult> = (|| {
        // Collect ids inside the transaction to prevent concurrent bumps from
        // changing the ordering between the SELECT and the DELETE.
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id FROM threads
                 WHERE board_id = ?1 AND sticky = 0 AND archived = 0
                 ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
            )?;
            let collected = stmt
                .query_map(params![board_id, max], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };

        if ids.is_empty() {
            return Ok(crate::db::DeletePathsResult {
                paths: Vec::new(),
                pending_fs_op_id: None,
            });
        }

        // Collect all file paths in a single query BEFORE the DELETEs.
        let candidates = collect_thread_file_paths(conn, &ids)?;

        // Single bulk DELETE instead of N individual statements.
        let placeholders: String = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i.saturating_add(1)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("DELETE FROM threads WHERE id IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(&ids))
            .context("Failed to bulk delete pruned threads")?;

        // Determine safe paths INSIDE the transaction so the check sees the
        // post-delete state before any concurrent writer can insert new references.
        let safe = super::paths_safe_to_delete(conn, candidates)?;
        let pending_fs_op = super::build_delete_files_pending_op(&safe)?;
        if let Some(op) = pending_fs_op.as_ref() {
            super::insert_pending_fs_op(conn, op)?;
        }
        Ok(crate::db::DeletePathsResult {
            paths: safe,
            pending_fs_op_id: pending_fs_op.map(|op| op.id),
        })
    })();

    match result {
        Ok(result) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit prune_old_threads transaction")?;
            Ok(result)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Hard-delete oldest archived threads that exceed the archive retention cap.
///
/// Returns the on-disk paths that are now safe to remove. As with live-thread
/// pruning, the caller is responsible for deleting those files from disk.
///
/// The ordering uses `bumped_at DESC`, matching the archive page and ensuring
/// we keep the most recently-active archived threads.
///
/// # Errors
/// Returns an error if the transaction cannot be opened or committed, if the
/// candidate threads cannot be queried, or if the bulk delete/safe-path
/// calculation fails.
pub fn prune_old_archived_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<crate::db::DeletePathsResult> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin prune_old_archived_threads transaction")?;

    let result: anyhow::Result<crate::db::DeletePathsResult> = (|| {
        let ids: Vec<i64> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id FROM threads
                 WHERE board_id = ?1 AND archived = 1
                 ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
            )?;
            let collected = stmt
                .query_map(params![board_id, max], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            collected
        };

        if ids.is_empty() {
            return Ok(crate::db::DeletePathsResult {
                paths: Vec::new(),
                pending_fs_op_id: None,
            });
        }

        let candidates = collect_thread_file_paths(conn, &ids)?;

        let placeholders: String = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i.saturating_add(1)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("DELETE FROM threads WHERE id IN ({placeholders})");
        conn.execute(&sql, rusqlite::params_from_iter(&ids))
            .context("Failed to bulk delete archived threads")?;

        let safe = super::paths_safe_to_delete(conn, candidates)?;
        let pending_fs_op = super::build_delete_files_pending_op(&safe)?;
        if let Some(op) = pending_fs_op.as_ref() {
            super::insert_pending_fs_op(conn, op)?;
        }
        Ok(crate::db::DeletePathsResult {
            paths: safe,
            pending_fs_op_id: pending_fs_op.map(|op| op.id),
        })
    })();

    match result {
        Ok(result) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit prune_old_archived_threads transaction")?;
            Ok(result)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Archive listing ──────────────────────────────────────────────────────────

/// Get paginated archived threads for a board.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_archived_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    let sql = format!(
        "{THREAD_SELECT}
         WHERE t.board_id = ?1 AND t.archived = 1
         GROUP BY t.id, op.id
         ORDER BY t.bumped_at DESC
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let threads = stmt
        .query_map(params![board_id, limit, offset], map_thread)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// Count archived threads for a board (used for archive pagination).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn count_archived_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 1",
        params![board_id],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::{
        count_threads_for_board, create_reply_with_thread_update, delete_thread,
        prune_old_archived_threads, prune_old_threads,
    };
    use crate::db::{create_board, create_thread_with_optional_poll, get_board_by_short, NewPost};
    use crate::error::AppError;
    use crate::models::MediaType;
    use crate::pending_fs::finalize_delete_files_payload;
    use rusqlite::{params, Connection};

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        super::super::schema::install_or_migrate_schema(&conn).expect("install schema");
        conn
    }

    fn create_plain_thread(conn: &Connection, board_id: i64, title: &str) -> i64 {
        let post = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some(title.to_owned()),
            body: title.to_owned(),
            body_html: title.to_owned(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: true,
        };
        let (thread_id, _, _) =
            create_thread_with_optional_poll(conn, board_id, Some(title), &post, "", None, None)
                .expect("create thread");
        thread_id
    }

    #[test]
    fn prune_old_threads_commits_even_when_no_files_are_safe() {
        let conn = test_conn();
        let board_id = create_board(&conn, "prune", "Prune", "", false).expect("create board");
        create_plain_thread(&conn, board_id, "old thread");
        create_plain_thread(&conn, board_id, "new thread");
        let board = get_board_by_short(&conn, "prune")
            .expect("load board")
            .expect("board exists");
        assert_eq!(
            count_threads_for_board(&conn, board.id).expect("count before"),
            2
        );

        let deleted = prune_old_threads(&conn, board.id, 1).expect("prune");
        assert!(deleted.paths.is_empty());
        assert!(deleted.pending_fs_op_id.is_none());
        assert_eq!(
            count_threads_for_board(&conn, board.id).expect("count after"),
            1
        );
    }

    #[test]
    fn prune_old_archived_threads_commits_even_when_no_files_are_safe() {
        let conn = test_conn();
        let board_id = create_board(&conn, "aprune", "Archive", "", false).expect("create board");
        let first = create_plain_thread(&conn, board_id, "old archived thread");
        let second = create_plain_thread(&conn, board_id, "new archived thread");
        conn.execute(
            "UPDATE threads SET archived = 1 WHERE id IN (?1, ?2)",
            params![first, second],
        )
        .expect("archive threads");

        let deleted = prune_old_archived_threads(&conn, board_id, 1).expect("prune archived");
        assert!(deleted.paths.is_empty());
        assert!(deleted.pending_fs_op_id.is_none());
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 1",
                params![board_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("archived count"),
            1
        );
    }

    #[test]
    fn delete_thread_returns_pending_cleanup_for_media_reply() {
        let conn = test_conn();
        let board_id = create_board(&conn, "media", "Media", "", false).expect("create board");
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let board_dir = upload_dir.join("media");
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).expect("create upload dirs");
        std::fs::write(board_dir.join("reply.webp"), b"reply").expect("write reply");
        std::fs::write(thumb_dir.join("reply.webp"), b"thumb").expect("write thumb");

        let thread_id = create_plain_thread(&conn, board_id, "thread with media reply");
        let reply = NewPost {
            thread_id,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: None,
            body: "reply".to_owned(),
            body_html: "reply".to_owned(),
            ip_hash: None,
            file_path: Some("media/reply.webp".to_owned()),
            file_name: Some("reply.webp".to_owned()),
            file_size: Some(5),
            thumb_path: Some("media/thumbs/reply.webp".to_owned()),
            mime_type: Some("image/webp".to_owned()),
            media_type: Some(MediaType::Image.as_str().to_owned()),
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: false,
        };
        create_reply_with_thread_update(&conn, &reply, "", false, None).expect("create reply");

        let deleted = delete_thread(&conn, thread_id).expect("delete thread");
        assert!(deleted.pending_fs_op_id.is_some());
        assert!(deleted.paths.iter().any(|path| path == "media/reply.webp"));
        assert!(deleted
            .paths
            .iter()
            .any(|path| path == "media/thumbs/reply.webp"));
        assert_eq!(
            count_threads_for_board(&conn, board_id).expect("count after"),
            0
        );

        finalize_delete_files_payload(
            &conn,
            upload_dir.to_str().expect("utf8 upload dir"),
            deleted.pending_fs_op_id.as_deref(),
            &deleted.paths,
        )
        .expect("cleanup reply files");

        assert!(!board_dir.join("reply.webp").exists());
        assert!(!thumb_dir.join("reply.webp").exists());
    }

    #[test]
    fn delete_thread_returns_not_found_on_retry() {
        let conn = test_conn();
        let board_id = create_board(&conn, "delth", "Del Thread", "", false).expect("create board");
        let thread_id = create_plain_thread(&conn, board_id, "thread to delete");

        let deleted = delete_thread(&conn, thread_id).expect("delete thread");
        assert!(deleted.paths.is_empty());
        match delete_thread(&conn, thread_id) {
            Err(AppError::NotFound(msg)) => assert!(msg.contains("Thread id")),
            other => panic!("expected not found on retry, got {other:?}"),
        }
    }

    #[test]
    fn delete_thread_removes_replies_and_retries_cleanly() {
        let conn = test_conn();
        let board_id =
            create_board(&conn, "delthr", "Del Thread Replies", "", false).expect("create board");
        let thread_id = create_plain_thread(&conn, board_id, "thread with reply");
        let reply = NewPost {
            thread_id,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: None,
            body: "reply".to_owned(),
            body_html: "reply".to_owned(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: false,
        };
        create_reply_with_thread_update(&conn, &reply, "", false, None).expect("create reply");

        let deleted = delete_thread(&conn, thread_id).expect("delete thread");
        assert!(deleted.paths.is_empty());
        assert!(
            conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get::<_, i64>(0),
            )
            .expect("post count after delete")
                == 0
        );
        match delete_thread(&conn, thread_id) {
            Err(AppError::NotFound(msg)) => assert!(msg.contains("Thread id")),
            other => panic!("expected not found on retry, got {other:?}"),
        }
    }
}
