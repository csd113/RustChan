// db/threads.rs — Thread-level queries.
//
// Covers: thread listing, creation (atomically with its OP post via a
// transaction), sticky/lock toggles, deletion, archive/prune logic.
//
// Dependency notes:
//   create_thread_with_op  → super::posts::create_post_inner  (OP insert)
//   delete_thread          → super::paths_safe_to_delete       (file safety)
//   prune_old_threads      → super::paths_safe_to_delete       (file safety)

use crate::models::*;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

// ─── Board-index thread listing ───────────────────────────────────────────────

/// Get paginated threads for a board with OP preview data.
pub fn get_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    // Sticky threads float to top, then sorted by most recent bump.
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 ) AS image_count
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.board_id = ?1 AND t.archived = 0
         ORDER BY t.sticky DESC, t.bumped_at DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let threads = stmt
        .query_map(params![board_id, limit, offset], |row| {
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

pub fn count_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 0",
        params![board_id],
        |r| r.get(0),
    )?)
}

pub fn get_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Option<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 ) AS image_count
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.id = ?1",
    )?;
    Ok(stmt
        .query_row(params![thread_id], |row| {
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
        })
        .optional()?)
}

// ─── Thread creation (atomic with OP post) ────────────────────────────────────

/// Create a thread AND its OP post atomically in a single transaction.
///
/// FIX[MEDIUM-3]: The previous design had two separate DB calls — create_thread
/// followed by create_post — with no transaction. A crash between the two calls
/// left an orphaned thread with no OP post, causing all board-listing queries
/// (which JOIN on is_op=1) to silently skip the thread forever.
///
/// This function is the single entry point for thread creation and wraps both
/// operations in a transaction, guaranteeing the invariant that every thread
/// row has exactly one corresponding post with is_op=1.
///
/// Returns (thread_id, post_id).
pub fn create_thread_with_op(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
    post: &super::NewPost,
) -> Result<(i64, i64)> {
    conn.execute("BEGIN IMMEDIATE", [])?;

    let result = (|| -> Result<(i64, i64)> {
        conn.execute(
            "INSERT INTO threads (board_id, subject) VALUES (?1, ?2)",
            params![board_id, subject],
        )?;
        let thread_id = conn.last_insert_rowid();

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
            conn.execute("COMMIT", [])?;
            Ok(ids)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
}

// ─── Thread mutation ──────────────────────────────────────────────────────────

pub fn bump_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE threads SET bumped_at = unixepoch(), reply_count = reply_count + 1
         WHERE id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

pub fn set_thread_sticky(conn: &rusqlite::Connection, thread_id: i64, sticky: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET sticky = ?1 WHERE id = ?2",
        params![sticky as i32, thread_id],
    )?;
    Ok(())
}

pub fn set_thread_locked(conn: &rusqlite::Connection, thread_id: i64, locked: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET locked = ?1 WHERE id = ?2",
        params![locked as i32, thread_id],
    )?;
    Ok(())
}

pub fn delete_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT file_path, thumb_path, audio_file_path FROM posts WHERE thread_id = ?1")?;
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
        .query_map(params![thread_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut candidates = Vec::new();
    for (f, t, a) in rows {
        if let Some(p) = f { candidates.push(p); }
        if let Some(p) = t { candidates.push(p); }
        if let Some(p) = a { candidates.push(p); }
    }

    conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
    Ok(super::paths_safe_to_delete(conn, candidates))
}

// ─── Archive / prune ──────────────────────────────────────────────────────────

/// Archive oldest non-sticky threads that exceed the board's max_threads limit.
/// Archived threads are locked and marked read-only instead of deleted, so their
/// content remains accessible via /{board}/archive.
/// Returns the count of threads archived (no file deletion occurs).
pub fn archive_old_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let ids = stmt.query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let count = ids.len();
    for id in ids {
        conn.execute(
            "UPDATE threads SET archived = 1, locked = 1 WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(count)
}

/// Hard-delete oldest non-sticky, non-archived threads that exceed max_threads.
/// Used when a board has archiving disabled — threads are permanently removed.
///
/// Returns the on-disk paths that are now safe to delete (i.e. no longer
/// referenced by any remaining post after the prune). The caller is responsible
/// for actually removing these files from disk.
pub fn prune_old_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<Vec<String>> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let ids = stmt.query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };

    let mut candidates: Vec<String> = Vec::new();
    for id in &ids {
        let mut stmt = conn.prepare(
            "SELECT file_path, thumb_path, audio_file_path FROM posts WHERE thread_id = ?1",
        )?;
        let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (f, t, a) in rows {
            if let Some(p) = f { candidates.push(p); }
            if let Some(p) = t { candidates.push(p); }
            if let Some(p) = a { candidates.push(p); }
        }
        conn.execute("DELETE FROM threads WHERE id = ?1", params![id])?;
    }

    Ok(super::paths_safe_to_delete(conn, candidates))
}

// ─── Archive listing ──────────────────────────────────────────────────────────

pub fn get_archived_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 ) AS image_count
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.board_id = ?1 AND t.archived = 1
         ORDER BY t.bumped_at DESC
         LIMIT ?2 OFFSET ?3",
    )?;
    let threads = stmt
        .query_map(params![board_id, limit, offset], |row| {
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// Count archived threads for a board (used for archive pagination).
pub fn count_archived_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 1",
        params![board_id],
        |r| r.get(0),
    )?)
}
