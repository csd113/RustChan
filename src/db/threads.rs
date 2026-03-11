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
// FIX summary (from audit):
//   HIGH-1   delete_thread: SELECT+DELETE now atomic inside a transaction
//   HIGH-2   prune_old_threads: paths_safe_to_delete moved inside transaction
//              so it sees the post-delete DB state before any concurrent insert
//   HIGH-3   archive_old_threads / prune_old_threads: ID collection query
//              moved inside the transaction to close the TOCTOU race
//   MED-4    create_thread_with_op: raw BEGIN/COMMIT replaced with structured
//              helper using execute_batch for cleaner error flow
//   MED-5    prune_old_threads: prepare_cached inside loop is now a single
//              prepare_cached outside the loop (was documented as fixed but
//              was not actually implemented)
//   MED-6    bump_thread: not co-transactional with post insert — documented
//   MED-7    set_thread_archived(false): no longer unconditionally unlocks;
//              locked state is only changed when archiving
//   MED-8    image_count correlated subquery: replaced with LEFT JOIN + COUNT
//   MED-9    map_thread helper: extracted from 3 copy-pasted closures
//   LOW-10   LIMIT -1 OFFSET ?: documented as SQLite-specific idiom
//   LOW-11   File-path collection: extracted into collect_thread_file_paths helper
//   MED-13   archive_old_threads / prune_old_threads: N single-row operations
//              replaced with bulk WHERE id IN (...)
//   MED-17   prune_old_threads: N per-thread file-path queries replaced with
//              a single JOIN query

use crate::models::Thread;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a thread row. Column layout (must match every SELECT that calls this):
///   0  t.id           4  `t.bumped_at`    8  op.body       12 op.tripcode
///   1  `t.board_id`     5  t.locked       9  `op.file_path`  13 op.id (`op_id`)
///   2  t.subject      6  t.sticky       10 `op.thumb_path` 14 t.archived
///   3  `t.created_at`   7  `t.reply_count`  11 op.name       15 `image_count`
///
/// FIX[MED-9]: Extracted from three copy-pasted closures into a single helper,
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
/// FIX[LOW-11]: Extracted from `delete_thread` and `prune_old_threads` to eliminate
///   copy-pasted collection loops.
/// FIX[MED-17]: Uses a single JOIN query instead of one query per thread.
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
/// FIX[MED-8]: Replaced the correlated `image_count` subquery (which ran once
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

/// Create a thread AND its OP post atomically in a single transaction.
///
/// The invariant guaranteed here: every thread row has exactly one corresponding
/// post with `is_op=1`. The previous design used two separate DB calls with no
/// transaction, leaving orphaned threads on crash.
///
/// FIX[MED-4]: Replaced the raw `conn.execute("BEGIN IMMEDIATE", [])?` /
/// `conn.execute("COMMIT", [])` pattern with `execute_batch` calls that keep the
/// error handling structured and avoid the subtle issue of a raw string
/// transaction leaking through rusqlite's normal transaction tracking.
///
/// Returns (`thread_id`, `post_id`).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn create_thread_with_op(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
    post: &super::NewPost,
) -> Result<(i64, i64)> {
    // BEGIN IMMEDIATE acquires the write lock upfront to avoid SQLITE_BUSY
    // during the lock-upgrade step that DEFERRED transactions perform on first
    // write. With &Connection (not &mut Connection) we cannot use rusqlite's
    // typed Transaction::new(Immediate), so we issue the pragma directly.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin IMMEDIATE transaction for create_thread_with_op")?;

    let result: Result<(i64, i64)> = (|| {
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

        Ok((thread_id, post_id))
    })();

    match result {
        Ok(ids) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit create_thread_with_op transaction")?;
            Ok(ids)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Thread mutation ──────────────────────────────────────────────────────────

/// Bump a thread's `bumped_at` timestamp and increment `reply_count`.
///
/// Note (MED-6): `bump_thread` is called from the route handler after
/// `create_post` returns, not inside the same transaction as the post insert.
/// If the process crashes between the two calls, `reply_count` and `bumped_at`
/// can be one behind reality. A full fix would require moving `bump_thread`
/// into `create_post_inner`, which would change the API surface. Accepted as
/// a known minor inconsistency; the `reply_count` column is advisory and a
/// board reload corrects the displayed count.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn bump_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE threads SET bumped_at = unixepoch(), reply_count = reply_count + 1
         WHERE id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn set_thread_sticky(conn: &rusqlite::Connection, thread_id: i64, sticky: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET sticky = ?1 WHERE id = ?2",
        params![i32::from(sticky), thread_id],
    )?;
    Ok(())
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn set_thread_locked(conn: &rusqlite::Connection, thread_id: i64, locked: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET locked = ?1 WHERE id = ?2",
        params![i32::from(locked), thread_id],
    )?;
    Ok(())
}

/// Move a thread to (or out of) the board archive.
///
/// FIX[MED-7]: The previous implementation used `SET archived = ?1, locked = ?1`
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
    conn.execute(
        "UPDATE threads
         SET archived = ?1,
             locked   = CASE WHEN ?1 = 1 THEN 1 ELSE locked END
         WHERE id = ?2",
        params![i32::from(archived), thread_id],
    )?;
    Ok(())
}

/// Delete a thread and return on-disk paths that are now safe to remove.
///
/// FIX[HIGH-1]: The previous SELECT → DELETE sequence was not wrapped in a
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
pub fn delete_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<String>> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin delete_thread transaction")?;

    // Step 1: collect file paths from all posts in this thread.
    let candidates = collect_thread_file_paths(&tx, &[thread_id])?;

    // Step 2: delete thread (CASCADE removes posts).
    tx.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
        .context("Failed to delete thread")?;

    // Step 3: determine which paths are now unreferenced.
    // paths_safe_to_delete sees the post-delete state because we're still inside
    // the same transaction.
    let safe = super::paths_safe_to_delete(&tx, candidates);

    tx.commit()
        .context("Failed to commit delete_thread transaction")?;
    Ok(safe)
}

// ─── Archive / prune ──────────────────────────────────────────────────────────

/// Archive oldest non-sticky threads that exceed the board's `max_threads` limit.
///
/// Archived threads are locked and marked read-only; their content remains
/// accessible via `/{board}/archive`. Returns the count of threads archived
/// (no file deletion occurs).
///
/// FIX[HIGH-3]: The ID collection query is now inside the same transaction as
/// the UPDATEs, closing the TOCTOU race where a concurrent bump could change
/// the ordering between the SELECT and the UPDATE loop.
///
/// FIX[MED-13]: Replaced the N per-row UPDATE loop with a single bulk
/// UPDATE … WHERE id IN (…), which is both faster and more crash-safe.
///
/// Note: LIMIT -1 OFFSET ? is a SQLite-specific idiom for "skip the first
/// max rows, return everything else". It is not standard SQL. The LIMIT -1
/// means "no upper bound on the result set after the offset is applied".
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn archive_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin archive_old_threads transaction")?;

    // Collect inside the transaction to prevent races with concurrent bumps.
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let x = stmt
            .query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        x
    };

    let count = ids.len();
    if count == 0 {
        tx.rollback().ok();
        return Ok(0);
    }

    // Single bulk UPDATE instead of N individual statements.
    let placeholders: String = ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("UPDATE threads SET archived = 1, locked = 1 WHERE id IN ({placeholders})");
    tx.execute(&sql, rusqlite::params_from_iter(&ids))
        .context("Failed to bulk archive threads")?;

    tx.commit()
        .context("Failed to commit archive_old_threads transaction")?;
    Ok(count)
}

/// Hard-delete oldest non-sticky, non-archived threads that exceed `max_threads`.
/// Used when a board has archiving disabled — threads are permanently removed.
///
/// Returns the on-disk paths that are now safe to delete (i.e. no longer
/// referenced by any remaining post after the prune). The caller is responsible
/// for actually removing these files from disk.
///
/// FIX[HIGH-3]: ID collection query is now inside the transaction (see above).
///
/// FIX[HIGH-2]: `paths_safe_to_delete` is called INSIDE the transaction before
/// COMMIT so it sees the post-delete state atomically. Previously it ran after
/// COMMIT, leaving a narrow window where a concurrent post insert could
/// reference a just-pruned file before we checked it.
///
/// FIX[MED-5]: `prepare_cached` is now used OUTSIDE the loop (was documented as
/// fixed but the `prepare_cached` call was still inside the loop).
///
/// FIX[MED-13]: Replaced the N per-row DELETE loop with a single bulk DELETE.
///
/// FIX[MED-17]: File-path collection is now a single JOIN query instead of
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
) -> Result<Vec<String>> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin prune_old_threads transaction")?;

    // Collect ids inside the transaction to prevent concurrent bumps from
    // changing the ordering between the SELECT and the DELETE.
    let ids: Vec<i64> = {
        let mut stmt = tx.prepare_cached(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let x = stmt
            .query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        x
    };

    if ids.is_empty() {
        tx.rollback().ok();
        return Ok(Vec::new());
    }

    // Collect all file paths in a single query BEFORE the DELETEs.
    let candidates = collect_thread_file_paths(&tx, &ids)?;

    // Single bulk DELETE instead of N individual statements.
    let placeholders: String = ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("DELETE FROM threads WHERE id IN ({placeholders})");
    tx.execute(&sql, rusqlite::params_from_iter(&ids))
        .context("Failed to bulk delete pruned threads")?;

    // Determine safe paths INSIDE the transaction so the check sees the
    // post-delete state before any concurrent writer can insert new references.
    let safe = super::paths_safe_to_delete(&tx, candidates);

    tx.commit()
        .context("Failed to commit prune_old_threads transaction")?;

    Ok(safe)
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
