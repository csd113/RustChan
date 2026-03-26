// db/threads.rs — Thread-level queries.
//
// Covers: thread listing, creation (atomically with its OP post via a
// transaction), sticky/lock toggles, deletion, archive/prune logic.
//
// Dependency notes:
//   create_thread_with_op  → super::posts::create_post_inner  (OP insert)
//   delete_thread          → super::paths_safe_to_delete       (file safety)
//   prune_old_threads      → super::paths_safe_to_delete       (file safety)

use crate::models::Thread;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

/// Maximum number of IDs per SQL `IN`-clause batch, staying well below
/// `SQLite`'s default `SQLITE_MAX_VARIABLE_NUMBER` (`999`).
const SQL_BATCH_SIZE: usize = 900;

// ─── Transaction guard ────────────────────────────────────────────────────────

/// Drop-based guard that issues `ROLLBACK` if the transaction was not explicitly
/// committed or rolled back. This protects against panics inside transaction
/// closures leaving the connection in an open-transaction state.
struct TxGuard<'a> {
    conn: &'a rusqlite::Connection,
    finished: bool,
}

impl TxGuard<'_> {
    fn begin_immediate(conn: &rusqlite::Connection) -> Result<TxGuard<'_>> {
        conn.execute_batch("BEGIN IMMEDIATE")
            .context("Failed to BEGIN IMMEDIATE")?;
        Ok(TxGuard {
            conn,
            finished: false,
        })
    }

    fn commit(mut self) -> Result<()> {
        self.finished = true;
        self.conn
            .execute_batch("COMMIT")
            .context("Failed to COMMIT")?;
        Ok(())
    }

    fn rollback(mut self) {
        self.finished = true;
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a thread row. Column layout (must match every `SELECT` that calls this):
///
/// | Index | Column          | Index | Column          |
/// |-------|-----------------|-------|-----------------|
/// | 0     | `t.id`          | 8     | `op.body`       |
/// | 1     | `t.board_id`    | 9     | `op.file_path`  |
/// | 2     | `t.subject`     | 10    | `op.thumb_path`  |
/// | 3     | `t.created_at`  | 11    | `op.name`       |
/// | 4     | `t.bumped_at`   | 12    | `op.tripcode`   |
/// | 5     | `t.locked`      | 13    | `op.id` (`op_id`) |
/// | 6     | `t.sticky`      | 14    | `t.archived`    |
/// | 7     | `t.reply_count` | 15    | `image_count`   |
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

// ─── Placeholder helpers ──────────────────────────────────────────────────────

/// Build a comma-separated string of positional placeholders `?1, ?2, ...`
/// for use in SQL `IN` clauses. `count` must be greater than zero.
fn build_placeholders(count: usize) -> String {
    (1..=count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ─── File-path collection helper ──────────────────────────────────────────────

/// Collect all file paths (`file_path`, `thumb_path`, `audio_file_path`) for every
/// post in the given set of thread ids. Returns a flat `Vec` of non-null paths.
///
/// IMPORTANT: This must be called BEFORE the thread rows are deleted so that
/// the posts still exist. The `CASCADE` on threads→posts removes them atomically
/// with the thread row when you later execute `DELETE FROM threads`.
///
/// Large ID lists are automatically chunked to stay within the `SQLite`
/// parameter limit.
fn collect_thread_file_paths(
    conn: &rusqlite::Connection,
    thread_ids: &[i64],
) -> Result<Vec<String>> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();

    for chunk in thread_ids.chunks(SQL_BATCH_SIZE) {
        let placeholders = build_placeholders(chunk.len());
        let sql = format!(
            "SELECT file_path, thumb_path, audio_file_path
             FROM posts WHERE thread_id IN ({placeholders})"
        );

        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
            .query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;

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
    }

    Ok(paths)
}

/// Execute a bulk `UPDATE` or `DELETE` with `WHERE id IN (...)` across batches.
/// The `sql_template` must contain the literal `{PLACEHOLDERS}` which will be
/// replaced with `?1, ?2, ...` for each batch.
/// Returns the total number of rows affected.
fn bulk_execute_by_id(
    conn: &rusqlite::Connection,
    sql_template: &str,
    ids: &[i64],
) -> Result<usize> {
    let mut total = 0usize;
    for chunk in ids.chunks(SQL_BATCH_SIZE) {
        let placeholders = build_placeholders(chunk.len());
        let sql = sql_template.replace("{PLACEHOLDERS}", &placeholders);
        let affected = conn
            .execute(&sql, rusqlite::params_from_iter(chunk))
            .context("bulk_execute_by_id failed")?;
        total = total.saturating_add(affected);
    }
    Ok(total)
}

// ─── Board-index thread listing ───────────────────────────────────────────────

/// The canonical thread `SELECT` fragment shared by all listing queries.
///
/// `image_count` counts non-OP posts with a file attachment. The OP's image
/// (if any) is excluded so the count reflects reply images only.
const THREAD_SELECT: &str = "
    SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
           t.locked, t.sticky, t.reply_count,
           op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
           t.archived,
           (SELECT COUNT(*) FROM posts img
            WHERE img.thread_id = t.id AND img.is_op = 0 AND img.file_path IS NOT NULL
           ) AS image_count
    FROM threads t
    JOIN posts op ON op.thread_id = t.id AND op.is_op = 1";

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
         WHERE t.id = ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt.query_row(params![thread_id], map_thread).optional()?)
}

// ─── Thread creation (atomic with OP post) ────────────────────────────────────

/// Create a thread AND its OP post atomically in a single transaction.
///
/// The invariant guaranteed here: every thread row has exactly one corresponding
/// post with `is_op=1`.
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
    let tx = TxGuard::begin_immediate(conn)
        .context("Failed to begin transaction for create_thread_with_op")?;

    let thread_id: i64 = conn.query_row(
        "INSERT INTO threads (board_id, subject) VALUES (?1, ?2) RETURNING id",
        params![board_id, subject],
        |r| r.get(0),
    )?;

    let post_with_thread = super::NewPost {
        thread_id,
        is_op: true,
        ..post.clone()
    };
    let post_id = super::posts::create_post_inner(conn, &post_with_thread)?;

    tx.commit()
        .context("Failed to commit create_thread_with_op")?;
    Ok((thread_id, post_id))
}

// ─── Thread mutation ──────────────────────────────────────────────────────────

/// Bump a thread's `bumped_at` timestamp and increment `reply_count`.
///
/// Only bumps threads that are not locked and not archived. If the thread is
/// locked or archived the `UPDATE` is a no-op.
///
/// Note: `bump_thread` is called from the route handler after `create_post`
/// returns, not inside the same transaction as the post insert. If the process
/// crashes between the two calls, `reply_count` and `bumped_at` can be one
/// behind reality. The `reply_count` column is advisory and a board reload
/// corrects the displayed count.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn bump_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE threads SET bumped_at = unixepoch(), reply_count = reply_count + 1
         WHERE id = ?1 AND locked = 0 AND archived = 0",
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
/// Archiving always locks the thread; unarchiving only restores the archived
/// flag and leaves the locked state untouched. Callers that want to unlock a
/// thread after unarchiving should call `set_thread_locked` separately.
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
/// The full sequence is atomic:
///   1. Collect file paths (while posts still exist)
///   2. `DELETE` the thread (`CASCADE` removes posts)
///   3. `paths_safe_to_delete` inside the transaction sees the post-delete state
///   4. `COMMIT`
///
/// Returns an empty `Vec` if the thread did not exist.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<String>> {
    let tx = TxGuard::begin_immediate(conn).context("Failed to begin delete_thread transaction")?;

    // Step 1: collect file paths from all posts in this thread.
    let candidates = collect_thread_file_paths(conn, &[thread_id])?;

    // Step 2: delete thread (CASCADE removes posts).
    let affected = conn
        .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
        .context("Failed to delete thread")?;

    if affected == 0 {
        tx.rollback();
        return Ok(Vec::new());
    }

    // Step 3: determine which paths are now unreferenced.
    let safe = super::paths_safe_to_delete(conn, candidates)?;

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
/// Note: `LIMIT -1 OFFSET ?` is a `SQLite`-specific idiom for "skip the first
/// max rows, return everything else". `LIMIT -1` means "no upper bound on the
/// result set after the offset is applied". This is not standard SQL.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn archive_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    let tx = TxGuard::begin_immediate(conn)
        .context("Failed to begin archive_old_threads transaction")?;

    // Collect inside the transaction to prevent races with concurrent bumps.
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
        tx.rollback();
        return Ok(0);
    }

    let count = bulk_execute_by_id(
        conn,
        "UPDATE threads SET archived = 1, locked = 1 WHERE id IN ({PLACEHOLDERS})",
        &ids,
    )
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
/// Note: `LIMIT -1 OFFSET ?` is a `SQLite`-specific idiom — see
/// `archive_old_threads`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn prune_old_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<Vec<String>> {
    let tx =
        TxGuard::begin_immediate(conn).context("Failed to begin prune_old_threads transaction")?;

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
        tx.rollback();
        return Ok(Vec::new());
    }

    // Collect all file paths in a single query BEFORE the DELETEs.
    let candidates = collect_thread_file_paths(conn, &ids)?;

    // Bulk DELETE.
    bulk_execute_by_id(
        conn,
        "DELETE FROM threads WHERE id IN ({PLACEHOLDERS})",
        &ids,
    )
    .context("Failed to bulk delete pruned threads")?;

    // Determine safe paths INSIDE the transaction so the check sees the
    // post-delete state before any concurrent writer can insert new references.
    let safe = super::paths_safe_to_delete(conn, candidates)?;

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
