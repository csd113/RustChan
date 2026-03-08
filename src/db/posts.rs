// db/posts.rs — Post queries, file deduplication, polls, and the background
//               job queue (including worker-side update helpers).
//
// Dependency notes:
//   create_post_inner  is pub(super) — threads.rs calls it inside
//                      create_thread_with_op's manual transaction.
//   delete_post        calls super::paths_safe_to_delete.

use crate::models::*;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a full post row (23 columns, selected in the canonical order used
/// throughout this module) into a Post struct.
pub(super) fn map_post(row: &rusqlite::Row<'_>) -> rusqlite::Result<Post> {
    let media_type_str: Option<String> = row.get(17)?;
    let media_type = media_type_str
        .as_deref()
        .and_then(crate::models::MediaType::from_db_str);

    Ok(Post {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        board_id: row.get(2)?,
        name: row.get(3)?,
        tripcode: row.get(4)?,
        subject: row.get(5)?,
        body: row.get(6)?,
        body_html: row.get(7)?,
        ip_hash: row.get(8)?,
        file_path: row.get(9)?,
        file_name: row.get(10)?,
        file_size: row.get(11)?,
        thumb_path: row.get(12)?,
        mime_type: row.get(13)?,
        created_at: row.get(14)?,
        deletion_token: row.get(15)?,
        is_op: row.get::<_, i32>(16)? != 0,
        media_type,
        audio_file_path: row.get(18)?,
        audio_file_name: row.get(19)?,
        audio_file_size: row.get(20)?,
        audio_mime_type: row.get(21)?,
        edited_at: row.get(22)?,
    })
}

// ─── Post queries ─────────────────────────────────────────────────────────────

pub fn get_posts_for_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE thread_id = ?1 ORDER BY created_at ASC, id ASC",
    )?;
    let posts = stmt
        .query_map(params![thread_id], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Fetch posts in `thread_id` whose id is strictly greater than `since_id`.
/// Returns them oldest-first. Used by the thread auto-update polling endpoint.
pub fn get_new_posts_since(
    conn: &rusqlite::Connection,
    thread_id: i64,
    since_id: i64,
) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE thread_id = ?1 AND id > ?2
         ORDER BY id ASC
         LIMIT 100",
    )?;
    let posts = stmt
        .query_map(params![thread_id, since_id], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Get last N posts for a thread (for board index preview).
pub fn get_preview_posts(conn: &rusqlite::Connection, thread_id: i64, n: i64) -> Result<Vec<Post>> {
    // Subquery gets the last N, outer query re-orders ascending for display.
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM (
             SELECT * FROM posts WHERE thread_id = ?1 AND is_op = 0
             ORDER BY created_at DESC, id DESC LIMIT ?2
         ) ORDER BY created_at ASC, id ASC",
    )?;
    let posts = stmt
        .query_map(params![thread_id, n], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Internal post insertion. Called directly by `threads::create_thread_with_op`
/// inside its manual BEGIN IMMEDIATE transaction, and wrapped by `create_post`.
///
/// `pub(super)` so sibling modules can call it without exposing it externally.
pub(super) fn create_post_inner(conn: &rusqlite::Connection, p: &super::NewPost) -> Result<i64> {
    conn.execute(
        "INSERT INTO posts
         (thread_id, board_id, name, tripcode, subject, body, body_html,
          ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
          deletion_token, is_op, media_type,
          audio_file_path, audio_file_name, audio_file_size, audio_mime_type)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        params![
            p.thread_id,
            p.board_id,
            p.name,
            p.tripcode,
            p.subject,
            p.body,
            p.body_html,
            p.ip_hash,
            p.file_path,
            p.file_name,
            p.file_size,
            p.thumb_path,
            p.mime_type,
            p.deletion_token,
            p.is_op as i32,
            p.media_type,
            p.audio_file_path,
            p.audio_file_name,
            p.audio_file_size,
            p.audio_mime_type,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn create_post(conn: &rusqlite::Connection, p: &super::NewPost) -> Result<i64> {
    create_post_inner(conn, p)
}

pub fn get_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Option<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![post_id], map_post).optional()?)
}

/// Fetch a post by its global post ID, verifying it belongs to the given board.
///
/// Because the `posts` table uses a single SQLite AUTOINCREMENT sequence, every
/// post has a globally unique `id`. `>>>/board/N` links therefore unambiguously
/// identify one post; this function validates the board membership so a crafted
/// link cannot leak posts from a different board.
pub fn get_post_on_board(
    conn: &rusqlite::Connection,
    board_short: &str,
    post_id: i64,
) -> Result<Option<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, p.thread_id, p.board_id, p.name, p.tripcode, p.subject,
                p.body, p.body_html, p.ip_hash, p.file_path, p.file_name, p.file_size,
                p.thumb_path, p.mime_type, p.created_at, p.deletion_token, p.is_op,
                p.media_type, p.audio_file_path, p.audio_file_name, p.audio_file_size,
                p.audio_mime_type, p.edited_at
         FROM posts p
         JOIN boards b ON b.id = p.board_id
         WHERE p.id = ?1 AND b.short_name = ?2
         LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![post_id, board_short], map_post)
        .optional()?)
}

/// Delete a post by id; returns file paths for cleanup.
pub fn delete_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Vec<String>> {
    let mut candidates = Vec::new();
    if let Some(post) = get_post(conn, post_id)? {
        if let Some(p) = post.file_path {
            candidates.push(p);
        }
        if let Some(p) = post.thumb_path {
            candidates.push(p);
        }
        if let Some(p) = post.audio_file_path {
            candidates.push(p);
        }
    }
    conn.execute("DELETE FROM posts WHERE id = ?1", params![post_id])?;
    Ok(super::paths_safe_to_delete(conn, candidates))
}

/// FIX[LOW-3]: Use constant-time byte comparison to prevent timing attacks on
/// deletion token verification. Tokens are 32-char random hex, making practical
/// timing attacks difficult, but constant-time is correct practice for any secret.
pub fn verify_deletion_token(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
) -> Result<bool> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT deletion_token FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get(0),
        )
        .optional()?;

    Ok(stored
        .map(|s| constant_time_eq(s.as_bytes(), token.as_bytes()))
        .unwrap_or(false))
}

/// Edit a post's body, verified against the deletion token and a per-board edit window.
///
/// `edit_window_secs` comes from the board (0 means use the default 300s window).
/// The caller is responsible for checking `board.allow_editing` before calling this.
/// Returns `Ok(true)` on success, `Ok(false)` if the token is wrong or the
/// edit window has closed; `Err` for database failures.
pub fn edit_post(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
    new_body: &str,
    new_body_html: &str,
    edit_window_secs: i64,
) -> Result<bool> {
    let window = if edit_window_secs <= 0 {
        300
    } else {
        edit_window_secs
    };

    if !verify_deletion_token(conn, post_id, token)? {
        return Ok(false);
    }

    let created_at: Option<i64> = conn
        .query_row(
            "SELECT created_at FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get(0),
        )
        .optional()?;

    let created_at = match created_at {
        Some(t) => t,
        None => return Ok(false),
    };

    let now = chrono::Utc::now().timestamp();
    if now - created_at > window {
        return Ok(false);
    }

    conn.execute(
        "UPDATE posts SET body = ?1, body_html = ?2, edited_at = ?3 WHERE id = ?4",
        params![new_body, new_body_html, now, post_id],
    )?;

    Ok(true)
}

/// Constant-time byte slice comparison to prevent timing side-channel attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

/// Full-text search across post bodies.
pub fn search_posts(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Post>> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE board_id = ?1 AND body LIKE ?2 ESCAPE '\\'
         ORDER BY created_at DESC LIMIT ?3 OFFSET ?4",
    )?;
    let posts = stmt
        .query_map(params![board_id, pattern, limit, offset], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

pub fn count_search_results(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
) -> Result<i64> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE board_id = ?1 AND body LIKE ?2 ESCAPE '\\'",
        params![board_id, pattern],
        |r| r.get(0),
    )?)
}

// ─── File deduplication ───────────────────────────────────────────────────────

/// Look up an existing upload by its SHA-256 hash.
pub fn find_file_by_hash(
    conn: &rusqlite::Connection,
    sha256: &str,
) -> Result<Option<super::CachedFile>> {
    let mut stmt = conn.prepare_cached(
        "SELECT file_path, thumb_path, mime_type FROM file_hashes WHERE sha256 = ?1",
    )?;
    Ok(stmt
        .query_row(params![sha256], |r| {
            Ok(super::CachedFile {
                file_path: r.get(0)?,
                thumb_path: r.get(1)?,
                mime_type: r.get(2)?,
            })
        })
        .optional()?)
}

/// Record a newly saved upload in the deduplication table.
pub fn record_file_hash(
    conn: &rusqlite::Connection,
    sha256: &str,
    file_path: &str,
    thumb_path: &str,
    mime_type: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO file_hashes (sha256, file_path, thumb_path, mime_type)
         VALUES (?1, ?2, ?3, ?4)",
        params![sha256, file_path, thumb_path, mime_type],
    )?;
    Ok(())
}

// ─── Poll queries ─────────────────────────────────────────────────────────────

/// Create a poll with its options atomically.
pub fn create_poll(
    conn: &rusqlite::Connection,
    thread_id: i64,
    question: &str,
    options: &[String],
    expires_at: i64,
) -> Result<i64> {
    // Wrap poll row + all option rows in one transaction so a crash mid-loop
    // cannot leave a poll with zero options (which would cause divide-by-zero
    // in the vote-percentage display).
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin poll transaction")?;
    tx.execute(
        "INSERT INTO polls (thread_id, question, expires_at) VALUES (?1, ?2, ?3)",
        params![thread_id, question, expires_at],
    )?;
    let poll_id = tx.last_insert_rowid();
    for (i, text) in options.iter().enumerate() {
        tx.execute(
            "INSERT INTO poll_options (poll_id, text, position) VALUES (?1, ?2, ?3)",
            params![poll_id, text, i as i64],
        )?;
    }
    tx.commit().context("Failed to commit poll transaction")?;
    Ok(poll_id)
}

/// Fetch the full poll for a thread including vote counts and the user's choice.
pub fn get_poll_for_thread(
    conn: &rusqlite::Connection,
    thread_id: i64,
    ip_hash: &str,
) -> Result<Option<crate::models::PollData>> {
    let now = chrono::Utc::now().timestamp();

    let poll_row = conn
        .query_row(
            "SELECT id, thread_id, question, expires_at, created_at FROM polls WHERE thread_id = ?1",
            params![thread_id],
            |r| {
                Ok(crate::models::Poll {
                    id: r.get(0)?,
                    thread_id: r.get(1)?,
                    question: r.get(2)?,
                    expires_at: r.get(3)?,
                    created_at: r.get(4)?,
                })
            },
        )
        .optional()?;

    let poll = match poll_row {
        Some(p) => p,
        None => return Ok(None),
    };

    let mut stmt = conn.prepare_cached(
        "SELECT po.id, po.poll_id, po.text, po.position,
                COUNT(pv.id) as vote_count
         FROM poll_options po
         LEFT JOIN poll_votes pv ON pv.option_id = po.id
         WHERE po.poll_id = ?1
         GROUP BY po.id
         ORDER BY po.position ASC",
    )?;
    let options: Vec<crate::models::PollOption> = stmt
        .query_map(params![poll.id], |r| {
            Ok(crate::models::PollOption {
                id: r.get(0)?,
                poll_id: r.get(1)?,
                text: r.get(2)?,
                position: r.get(3)?,
                vote_count: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let total_votes: i64 = options.iter().map(|o| o.vote_count).sum();

    let user_voted_option: Option<i64> = conn
        .query_row(
            "SELECT option_id FROM poll_votes WHERE poll_id = ?1 AND ip_hash = ?2",
            params![poll.id, ip_hash],
            |r| r.get(0),
        )
        .optional()?;

    let is_expired = poll.expires_at <= now;

    Ok(Some(crate::models::PollData {
        poll,
        options,
        total_votes,
        user_voted_option,
        is_expired,
    }))
}

/// Cast a vote. Returns true if vote was recorded, false if already voted.
pub fn cast_vote(
    conn: &rusqlite::Connection,
    poll_id: i64,
    option_id: i64,
    ip_hash: &str,
) -> Result<bool> {
    let result = conn.execute(
        "INSERT OR IGNORE INTO poll_votes (poll_id, option_id, ip_hash)
         VALUES (?1, ?2, ?3)",
        params![poll_id, option_id, ip_hash],
    )?;
    Ok(result > 0)
}

/// Resolve (poll_id, thread_id, board_short) from an option_id.
pub fn get_poll_context(
    conn: &rusqlite::Connection,
    option_id: i64,
) -> Result<Option<(i64, i64, String)>> {
    Ok(conn
        .query_row(
            "SELECT p.id, p.thread_id, b.short_name
         FROM poll_options po
         JOIN polls p ON p.id = po.poll_id
         JOIN threads t ON t.id = p.thread_id
         JOIN boards b ON b.id = t.board_id
         WHERE po.id = ?1",
            params![option_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

// ─── Background job queue ─────────────────────────────────────────────────────
//
// Jobs flow through: pending → running → done | failed
// claim_next_job uses UPDATE … RETURNING for atomic claim with no TOCTOU race.

/// Persist a new job in the pending state. Returns the new row id.
pub fn enqueue_job(conn: &rusqlite::Connection, job_type: &str, payload: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO background_jobs (job_type, payload, status, updated_at)
         VALUES (?1, ?2, 'pending', unixepoch())",
        params![job_type, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Atomically claim the highest-priority pending job that has not exhausted
/// its retry budget. Returns (job_id, payload) or None when the queue is empty.
///
/// The UPDATE … RETURNING subquery is a single atomic operation in SQLite's
/// WAL mode, so no two workers can claim the same job.
pub fn claim_next_job(conn: &rusqlite::Connection) -> Result<Option<(i64, String)>> {
    let mut stmt = conn.prepare_cached(
        "UPDATE background_jobs
         SET status = 'running',
             attempts  = attempts + 1,
             updated_at = unixepoch()
         WHERE id = (
             SELECT id FROM background_jobs
             WHERE status = 'pending' AND attempts < 3
             ORDER BY priority DESC, created_at ASC
             LIMIT 1
         )
         RETURNING id, payload",
    )?;
    let result = stmt
        .query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .optional()?;
    Ok(result)
}

/// Mark a job as successfully completed.
pub fn complete_job(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE background_jobs SET status = 'done', updated_at = unixepoch()
         WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Record a job failure. After MAX_ATTEMPTS the job stays "failed" permanently.
pub fn fail_job(conn: &rusqlite::Connection, id: i64, error: &str) -> Result<()> {
    let err_trunc: String = error.chars().take(512).collect();
    conn.execute(
        "UPDATE background_jobs
         SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END,
             last_error  = ?2,
             updated_at  = unixepoch()
         WHERE id = ?1",
        params![id, err_trunc],
    )?;
    Ok(())
}

/// Count jobs currently in the 'pending' state (used for monitoring).
#[allow(dead_code)]
pub fn pending_job_count(conn: &rusqlite::Connection) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM background_jobs WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

// ─── Post update helpers (used by background workers) ────────────────────────

/// Update a post's file_path and mime_type after background transcoding.
pub fn update_post_file_info(
    conn: &rusqlite::Connection,
    post_id: i64,
    file_path: &str,
    mime_type: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE id = ?3",
        params![file_path, mime_type, post_id],
    )?;
    Ok(())
}

/// Update every post that currently stores `old_path` as its `file_path`.
///
/// Required after video transcoding: the deduplication system shares one
/// physical file across N posts. The VideoTranscode worker knows only the
/// post_id that triggered the job, but ALL posts that reference the same MP4
/// must be migrated to the new WebM path before the MP4 is removed from disk.
/// Without this, `paths_safe_to_delete` counts zero references to the old MP4
/// and marks it safe to delete — but the WebM it was replaced with would also
/// be considered orphaned the next time any of those stale posts is deleted.
///
/// Returns the number of posts updated.
pub fn update_all_posts_file_path(
    conn: &rusqlite::Connection,
    old_path: &str,
    new_path: &str,
    new_mime: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE file_path = ?3",
        params![new_path, new_mime, old_path],
    )?;
    Ok(n)
}

/// Update a post's thumb_path after background waveform / thumbnail generation.
pub fn update_post_thumb_path(
    conn: &rusqlite::Connection,
    post_id: i64,
    thumb_path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE posts SET thumb_path = ?1 WHERE id = ?2",
        params![thumb_path, post_id],
    )?;
    Ok(())
}

/// Retrieve just the thumb_path for a post (used by VideoTranscode worker to
/// preserve the existing thumbnail when refreshing the file-hash record).
pub fn get_post_thumb_path(conn: &rusqlite::Connection, post_id: i64) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT thumb_path FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(result)
}

/// Delete a file-hash record by its stored file_path (used when the worker
/// replaces an MP4 with the transcoded WebM and needs to refresh the index).
pub fn delete_file_hash_by_path(conn: &rusqlite::Connection, file_path: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM file_hashes WHERE file_path = ?1",
        params![file_path],
    )?;
    Ok(())
}
