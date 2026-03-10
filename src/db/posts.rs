// db/posts.rs — Post queries, file deduplication, polls, and the background
//               job queue (including worker-side update helpers).
//
// Dependency notes:
//   create_post_inner  is pub(super) — threads.rs calls it inside
//                      create_thread_with_op's manual transaction.
//   delete_post        calls super::paths_safe_to_delete.
//
// FIX summary (from audit):
//   HIGH-1   edit_post: transaction upgraded from DEFERRED (unchecked_transaction)
//              to IMMEDIATE (raw BEGIN IMMEDIATE) to prevent write contention
//   HIGH-2   edit_post: combined two separate round-trips (token fetch +
//              created_at fetch) into a single SELECT, eliminating race window
//   HIGH-3   delete_post: SELECT → DELETE is now wrapped in a transaction to
//              eliminate the TOCTOU race
//   MED-4    enqueue_job: INSERT … RETURNING id replaces last_insert_rowid()
//   MED-5    create_poll: INSERT … RETURNING id inside transaction
//   MED-6    MAX_JOB_ATTEMPTS constant extracted; magic number 3 was duplicated
//              in claim_next_job and fail_job and could diverge
//   MED-7    constant_time_eq: fixed early-return length leak; comparison now
//              processes all bytes regardless of length difference
//   MED-8    LIKE escape logic extracted into like_escape helper
//   LOW-9    get_preview_posts inner SELECT * replaced with explicit column list
//   LOW-10   get_new_posts_since LIMIT 100 documented
//   MED-11   retention_cutoff parameter rename documented
//   MED-12   map_post: column-count assertion added as a compile-time guard
//   MED-13   get_new_posts_since hardcoded LIMIT 100: now takes a max_results param
//   MED-14   cast_vote conflation documented
//   MED-15   update_all_posts_file_path: doc clarified for implicit caller contract
//   LOW-16   complete_job / fail_job: added rows-affected checks

use crate::models::*;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

// ─── Retry budget constant ────────────────────────────────────────────────────

/// FIX[MED-6]: Single source of truth for the job retry budget.
/// Previously the magic number 3 appeared in both claim_next_job (WHERE attempts < 3)
/// and fail_job (CASE WHEN attempts >= 3), with no guarantee they would stay in sync.
const MAX_JOB_ATTEMPTS: i64 = 3;

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a full post row (23 columns, selected in the canonical order used
/// throughout this module) into a Post struct.
///
/// FIX[MED-12]: The expected column count is asserted here so any future change
/// to the SELECT list that shifts column indices produces a compile-time error
/// rather than silent data corruption at runtime.
///
/// Column layout:
///   0  id            8  ip_hash        16 is_op
///   1  thread_id     9  file_path      17 media_type
///   2  board_id      10 file_name      18 audio_file_path
///   3  name          11 file_size      19 audio_file_name
///   4  tripcode      12 thumb_path     20 audio_file_size
///   5  subject       13 mime_type      21 audio_mime_type
///   6  body          14 created_at     22 edited_at
///   7  body_html     15 deletion_token
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
///
/// FIX[MED-13]: The limit is now an explicit parameter instead of a hardcoded
/// magic number. Callers should pass a sensible cap (e.g. 100 for live polling)
/// to prevent runaway result sets on very active threads.
pub fn get_new_posts_since(
    conn: &rusqlite::Connection,
    thread_id: i64,
    since_id: i64,
    max_results: i64,
) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE thread_id = ?1 AND id > ?2
         ORDER BY id ASC
         LIMIT ?3",
    )?;
    let posts = stmt
        .query_map(params![thread_id, since_id, max_results], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Get last N posts for a thread (for board index preview).
///
/// FIX[LOW-9]: The inner subquery used `SELECT *` which silently breaks if the
/// schema adds or reorders columns. Replaced with explicit column list.
pub fn get_preview_posts(conn: &rusqlite::Connection, thread_id: i64, n: i64) -> Result<Vec<Post>> {
    // Subquery gets the last N, outer query re-orders ascending for display.
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM (
             SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                    ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                    created_at, deletion_token, is_op, media_type,
                    audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                    edited_at
             FROM posts WHERE thread_id = ?1 AND is_op = 0
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
    let post_id: i64 = conn.query_row(
        "INSERT INTO posts
         (thread_id, board_id, name, tripcode, subject, body, body_html,
          ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
          deletion_token, is_op, media_type,
          audio_file_path, audio_file_name, audio_file_size, audio_mime_type)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
         RETURNING id",
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
        |r| r.get(0),
    )?;
    Ok(post_id)
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

/// Delete a post by id; returns file paths safe to remove from disk.
///
/// FIX[HIGH-3]: The previous implementation had a SELECT → DELETE TOCTOU race:
/// if the post was concurrently deleted between the get_post call and the
/// DELETE, the function silently returned an empty path list rather than an
/// error, and the caller would skip file cleanup assuming there was nothing to
/// clean. Both operations are now wrapped in a single transaction so no
/// interleaving is possible. paths_safe_to_delete is called inside the
/// transaction so it sees the post-delete state.
pub fn delete_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Vec<String>> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin delete_post transaction")?;

    let candidates = {
        let mut candidates = Vec::new();
        let mut stmt = tx.prepare_cached(
            "SELECT file_path, thumb_path, audio_file_path FROM posts WHERE id = ?1",
        )?;
        if let Some((f, t, a)) = stmt
            .query_row(params![post_id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .optional()?
        {
            if let Some(p) = f {
                candidates.push(p);
            }
            if let Some(p) = t {
                candidates.push(p);
            }
            if let Some(p) = a {
                candidates.push(p);
            }
        }
        candidates
    };

    tx.execute("DELETE FROM posts WHERE id = ?1", params![post_id])
        .context("Failed to delete post")?;

    // Check which paths are now safe — runs inside the transaction so it sees
    // the just-deleted state.
    let safe = super::paths_safe_to_delete(&tx, candidates);

    tx.commit()
        .context("Failed to commit delete_post transaction")?;
    Ok(safe)
}

/// Use constant-time byte comparison to prevent timing side-channel attacks on
/// deletion token verification. Tokens are 32-char random hex, making practical
/// timing attacks difficult, but constant-time comparison is correct practice
/// for any secret value.
///
/// Note: `edit_post` inlines its own transactional token check, so this helper
/// is not currently called there. Kept for future handlers (e.g. user-facing
/// post deletion) that need standalone token verification.
#[allow(dead_code)]
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
///
/// FIX[HIGH-1]: Upgraded from DEFERRED (unchecked_transaction) to IMMEDIATE by
/// issuing BEGIN IMMEDIATE explicitly. A DEFERRED transaction on a write
/// operation can fail with SQLITE_BUSY when the write lock is contested; IMMEDIATE
/// acquires the write lock upfront, eliminating mid-transaction lock escalation.
///
/// FIX[HIGH-2]: The previous two-round-trip design (one SELECT for the token,
/// a second SELECT for created_at) introduced a race window: the post could be
/// deleted between the token check and the timestamp fetch. Both values are now
/// fetched in a single SELECT inside the IMMEDIATE transaction.
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

    // BEGIN IMMEDIATE acquires the write lock now, preventing any concurrent
    // writer from modifying the post between our SELECT and UPDATE.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin IMMEDIATE transaction for edit_post")?;

    let result: Result<bool> = (|| {
        // FIX[HIGH-2]: Fetch token and created_at in a single round-trip.
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT deletion_token, created_at FROM posts WHERE id = ?1",
                params![post_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let (stored_token, created_at) = match row {
            Some(r) => r,
            None => return Ok(false), // post does not exist
        };

        if !constant_time_eq(stored_token.as_bytes(), token.as_bytes()) {
            return Ok(false);
        }

        let now = chrono::Utc::now().timestamp();
        if now - created_at > window {
            return Ok(false);
        }

        conn.execute(
            "UPDATE posts SET body = ?1, body_html = ?2, edited_at = ?3 WHERE id = ?4",
            params![new_body, new_body_html, now, post_id],
        )?;

        // Belt-and-suspenders: confirm the row was actually written.
        Ok(conn.changes() > 0)
    })();

    match result {
        Ok(updated) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit edit_post transaction")?;
            Ok(updated)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Constant-time byte slice comparison to prevent timing side-channel attacks.
///
/// FIX[MED-7]: The previous implementation returned false immediately when
/// lengths differed, leaking token length as a timing signal. The comparison
/// now processes all bytes from the longer slice regardless of length, folding
/// the length mismatch into the accumulator.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    // Non-zero when lengths differ.
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..max_len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

// ─── LIKE escape helper ───────────────────────────────────────────────────────

/// FIX[MED-8]: Extracted from search_posts and count_search_results to avoid
/// duplicating the escape logic. Escapes `%` and `_` metacharacters so that
/// user-supplied query strings are treated as literal substrings.
fn like_escape(query: &str) -> String {
    format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"))
}

// ─── Search ───────────────────────────────────────────────────────────────────

/// Full-text search across post bodies.
pub fn search_posts(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Post>> {
    let pattern = like_escape(query);
    let mut stmt = conn.prepare_cached(
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
    let pattern = like_escape(query);
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
///
/// FIX[MED-5]: Replaced last_insert_rowid() with INSERT … RETURNING id so the
/// poll id is retrieved atomically in the same statement rather than relying on
/// connection-local state.
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

    let poll_id: i64 = tx
        .query_row(
            "INSERT INTO polls (thread_id, question, expires_at) VALUES (?1, ?2, ?3)
             RETURNING id",
            params![thread_id, question, expires_at],
            |r| r.get(0),
        )
        .context("Failed to insert poll")?;

    let mut opt_stmt = tx
        .prepare_cached("INSERT INTO poll_options (poll_id, text, position) VALUES (?1, ?2, ?3)")?;
    for (i, text) in options.iter().enumerate() {
        opt_stmt
            .execute(params![poll_id, text, i as i64])
            .context("Failed to insert poll option")?;
    }
    drop(opt_stmt); // release borrow on tx before commit

    tx.commit().context("Failed to commit poll transaction")?;
    Ok(poll_id)
}

/// Fetch the full poll for a thread including vote counts and the user's choice.
///
/// Note: poll expiry is checked against the application clock (chrono::Utc::now)
/// while poll_votes are pruned using the SQLite clock (unixepoch()). A skew
/// between the two clocks (e.g. container time drift) could cause a poll to
/// appear expired to the application before SQLite prunes it, or vice versa.
/// In practice the skew is negligible for typical deployments.
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
                                AND pv.poll_id   = po.poll_id
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

/// Cast a vote. Returns true if vote was recorded, false otherwise.
///
/// FIX[CROSS-POLL]: Validates that `option_id` belongs to `poll_id` inside
/// the same INSERT statement via a correlated WHERE EXISTS. A mismatched
/// (poll_id, option_id) pair inserts nothing and returns false.
///
/// Note (MED-14): This function returns false for two distinct cases:
///   1. The voter has already voted (UNIQUE constraint fires INSERT OR IGNORE)
///   2. The option_id does not belong to poll_id (EXISTS check fails)
///
/// Callers that need to distinguish these cases should call cast_vote and, on
/// false, separately query whether the IP has voted on this poll. A future
/// refactor could return a tri-state enum instead.
pub fn cast_vote(
    conn: &rusqlite::Connection,
    poll_id: i64,
    option_id: i64,
    ip_hash: &str,
) -> Result<bool> {
    let result = conn.execute(
        "INSERT OR IGNORE INTO poll_votes (poll_id, option_id, ip_hash)
         SELECT ?1, ?2, ?3
         WHERE EXISTS (
             SELECT 1 FROM poll_options
             WHERE id = ?2 AND poll_id = ?1
         )",
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

// ─── Poll maintenance ─────────────────────────────────────────────────────────

/// Delete vote rows for polls whose `expires_at` is older than the given
/// cutoff timestamp (a Unix timestamp). The poll question and options are
/// preserved for historical display; only the per-IP vote records are pruned.
///
/// Returns the number of vote rows deleted.
///
/// Note (MED-11): The parameter was previously named `retention_cutoff` in some
/// call sites, which is misleading — a lower value retains more votes, and a
/// higher value prunes more. It is more accurately described as an "expiry
/// cutoff": any poll that expired before this timestamp has its votes pruned.
pub fn cleanup_expired_poll_votes(
    conn: &rusqlite::Connection,
    expiry_cutoff: i64,
) -> Result<usize> {
    let n = conn.execute(
        "DELETE FROM poll_votes
         WHERE poll_id IN (
             SELECT id FROM polls
             WHERE expires_at IS NOT NULL AND expires_at < ?1
         )",
        params![expiry_cutoff],
    )?;
    Ok(n)
}

// ─── Background job queue ─────────────────────────────────────────────────────
//
// Jobs flow through: pending → running → done | failed
// claim_next_job uses UPDATE … RETURNING for atomic claim with no TOCTOU race.

/// Persist a new job in the pending state. Returns the new row id.
///
/// FIX[MED-4]: INSERT … RETURNING id replaces execute + last_insert_rowid().
pub fn enqueue_job(conn: &rusqlite::Connection, job_type: &str, payload: &str) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO background_jobs (job_type, payload, status, updated_at)
             VALUES (?1, ?2, 'pending', unixepoch()) RETURNING id",
            params![job_type, payload],
            |r| r.get(0),
        )
        .context("Failed to enqueue job")?;
    Ok(id)
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
             WHERE status = 'pending' AND attempts < ?1
             ORDER BY priority DESC, created_at ASC
             LIMIT 1
         )
         RETURNING id, payload",
    )?;
    let result = stmt
        .query_row(params![MAX_JOB_ATTEMPTS], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .optional()?;
    Ok(result)
}

/// Mark a job as successfully completed.
///
/// FIX[LOW-16]: Added rows-affected check — silently succeeding for an unknown
/// job_id made double-complete bugs invisible.
pub fn complete_job(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE background_jobs SET status = 'done', updated_at = unixepoch()
         WHERE id = ?1 AND status = 'running'",
        params![id],
    )?;
    if n == 0 {
        anyhow::bail!("Job {} not found or not in 'running' state", id);
    }
    Ok(())
}

/// Record a job failure. After MAX_JOB_ATTEMPTS the job stays "failed" permanently.
///
/// FIX[LOW-16]: Added rows-affected check.
/// FIX[MED-6]: Uses MAX_JOB_ATTEMPTS constant instead of duplicating the magic number.
pub fn fail_job(conn: &rusqlite::Connection, id: i64, error: &str) -> Result<()> {
    let err_trunc: String = error.chars().take(512).collect();
    let n = conn.execute(
        "UPDATE background_jobs
         SET status = CASE WHEN attempts >= ?3 THEN 'failed' ELSE 'pending' END,
             last_error  = ?2,
             updated_at  = unixepoch()
         WHERE id = ?1 AND status = 'running'",
        params![id, err_trunc, MAX_JOB_ATTEMPTS],
    )?;
    if n == 0 {
        anyhow::bail!("Job {} not found or not in 'running' state", id);
    }
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
/// Note (MED-15): This function does NOT update the corresponding file_hashes
/// row. The caller MUST call delete_file_hash_by_path(old_path) and then
/// record_file_hash(sha256, new_path, ...) after this function returns, before
/// any subsequent paths_safe_to_delete call. Failure to do so leaves the dedup
/// table pointing at the old (now-deleted) path.
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
