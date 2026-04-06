// db/posts.rs — Post queries, file deduplication, polls, and the background
//               job queue (including worker-side update helpers).
//
// Dependency notes:
//   create_post_inner  is pub(super) — threads.rs calls it inside
//                      create_thread_with_op's manual transaction.
//   delete_post        calls super::paths_safe_to_delete.
//
// FIX summary (from audit):
//              to IMMEDIATE (raw BEGIN IMMEDIATE) to prevent write contention
//              created_at fetch) into a single SELECT, eliminating race window
//              eliminate the TOCTOU race
//              in claim_next_job and fail_job and could diverge
//              processes all bytes regardless of length difference

use crate::models::Post;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::HashMap;

// ─── Retry budget constant ────────────────────────────────────────────────────

/// Single source of truth for the job retry budget.
/// Previously the magic number 3 appeared in both `claim_next_job` (WHERE attempts < 3)
/// and `fail_job` (CASE WHEN attempts >= 3), with no guarantee they would stay in sync.
const MAX_JOB_ATTEMPTS: i64 = 3;

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a full post row (23 columns, selected in the canonical order used
/// throughout this module) into a Post struct.
///
/// The expected column count is asserted here so any future change
/// to the SELECT list that shifts column indices produces a compile-time error
/// rather than silent data corruption at runtime.
///
/// Column layout:
///   0  id            8  `ip_hash`        16 `is_op`
///   1  `thread_id`     9  `file_path`      17 `media_type`
///   2  `board_id`      10 `file_name`      18 `audio_file_path`
///   3  name          11 `file_size`      19 `audio_file_name`
///   4  tripcode      12 `thumb_path`     20 `audio_file_size`
///   5  subject       13 `mime_type`      21 `audio_mime_type`
///   6  body          14 `created_at`     22 `edited_at`
///   7  `body_html`     15 `deletion_token`
///
/// # Errors
/// Returns an error if the database operation fails.
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
        ip_hash: row.get::<_, Option<String>>(8)?,
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

/// # Errors
/// Returns an error if the database operation fails.
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
/// The limit is now an explicit parameter instead of a hardcoded
/// magic number. Callers should pass a sensible cap (e.g. 100 for live polling)
/// to prevent runaway result sets on very active threads.
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Fetch the latest `n` non-OP posts for every thread in `thread_ids`.
///
/// The result is grouped by thread id and each thread's preview posts are
/// ordered oldest-first for direct display on the board index.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_preview_posts_for_threads(
    conn: &rusqlite::Connection,
    thread_ids: &[i64],
    n: i64,
) -> Result<HashMap<i64, Vec<Post>>> {
    if thread_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders = thread_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("?{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let limit_param = thread_ids.len() + 1;
    let sql = format!(
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
                    edited_at,
                    ROW_NUMBER() OVER (
                        PARTITION BY thread_id
                        ORDER BY created_at DESC, id DESC
                    ) AS preview_rank
             FROM posts
             WHERE is_op = 0 AND thread_id IN ({placeholders})
         )
         WHERE preview_rank <= ?{limit_param}
         ORDER BY thread_id ASC, created_at ASC, id ASC"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let params = rusqlite::params_from_iter(thread_ids.iter().copied().chain(std::iter::once(n)));
    let posts = stmt
        .query_map(params, map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut grouped = HashMap::with_capacity(thread_ids.len());
    for post in posts {
        grouped
            .entry(post.thread_id)
            .or_insert_with(Vec::new)
            .push(post);
    }
    Ok(grouped)
}

/// Internal post insertion. Called directly by `threads::create_thread_with_op`
/// inside its manual BEGIN IMMEDIATE transaction, and wrapped by `create_post`.
///
/// `pub(super)` so sibling modules can call it without exposing it externally.
///
/// # Errors
/// Returns an error if the database operation fails.
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
            i32::from(p.is_op),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostSubmissionRecord {
    pub thread_id: i64,
    pub post_id: i64,
    pub is_thread: bool,
}

pub fn get_post_submission(
    conn: &rusqlite::Connection,
    submission_token: &str,
    ip_hash: &str,
    board_id: i64,
) -> Result<Option<PostSubmissionRecord>> {
    if submission_token.trim().is_empty() {
        return Ok(None);
    }

    Ok(conn
        .query_row(
            "SELECT thread_id, post_id, is_thread
             FROM post_submissions
             WHERE submission_token = ?1
               AND ip_hash = ?2
               AND board_id = ?3
             LIMIT 1",
            params![submission_token, ip_hash, board_id],
            |row| {
                Ok(PostSubmissionRecord {
                    thread_id: row.get(0)?,
                    post_id: row.get(1)?,
                    is_thread: row.get::<_, i32>(2)? != 0,
                })
            },
        )
        .optional()?)
}

pub fn record_post_submission(
    conn: &rusqlite::Connection,
    submission_token: &str,
    ip_hash: &str,
    board_id: i64,
    thread_id: i64,
    post_id: i64,
    is_thread: bool,
) -> Result<()> {
    if submission_token.trim().is_empty() {
        return Ok(());
    }

    conn.execute(
        "INSERT OR IGNORE INTO post_submissions
         (submission_token, ip_hash, board_id, thread_id, post_id, is_thread)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            submission_token,
            ip_hash,
            board_id,
            thread_id,
            post_id,
            i32::from(is_thread)
        ],
    )
    .context("Failed to record post submission token")?;

    conn.execute(
        "DELETE FROM post_submissions WHERE created_at < unixepoch() - 604800",
        [],
    )
    .context("Failed to prune expired post submission tokens")?;

    Ok(())
}

/// Insert a poll row and its options using the caller's existing transaction.
///
/// # Errors
/// Returns an error if the poll row or any option row cannot be inserted.
pub(super) fn create_poll_inner(
    conn: &rusqlite::Connection,
    thread_id: i64,
    question: &str,
    options: &[String],
    expires_at: i64,
) -> Result<i64> {
    let poll_id: i64 = conn
        .query_row(
            "INSERT INTO polls (thread_id, question, expires_at) VALUES (?1, ?2, ?3)
             RETURNING id",
            params![thread_id, question, expires_at],
            |r| r.get(0),
        )
        .context("Failed to insert poll")?;

    let mut opt_stmt = conn
        .prepare_cached("INSERT INTO poll_options (poll_id, text, position) VALUES (?1, ?2, ?3)")?;
    for (i, text) in options.iter().enumerate() {
        opt_stmt
            .execute(params![
                poll_id,
                text,
                i64::try_from(i).context("poll option index overflow")?
            ])
            .context("Failed to insert poll option")?;
    }

    Ok(poll_id)
}

/// # Errors
/// Returns an error if the database operation fails.
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
/// # Errors
/// Returns an error if the database operation fails.
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
/// The previous implementation had a SELECT → DELETE TOCTOU race:
/// if the post was concurrently deleted between the `get_post` call and the
/// DELETE, the function silently returned an empty path list rather than an
/// error, and the caller would skip file cleanup assuming there was nothing to
/// clean. Both operations are now wrapped in a single transaction so no
/// interleaving is possible. `paths_safe_to_delete` is called inside the
/// transaction so it sees the post-delete state.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Vec<String>> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_post transaction")?;

    let result: anyhow::Result<Vec<String>> = (|| {
        let (thread_id, is_op, candidates) = {
            let mut candidates = Vec::new();
            let mut stmt = conn.prepare_cached(
                "SELECT thread_id, is_op, file_path, thumb_path, audio_file_path
                 FROM posts WHERE id = ?1",
            )?;
            let row = stmt
                .query_row(params![post_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i32>(1)? != 0,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .optional()?;

            let Some((thread_id, is_op, f, t, a)) = row else {
                anyhow::bail!("Post id {post_id} not found");
            };

            if is_op {
                anyhow::bail!(
                    "Post id {post_id} is the OP for thread {thread_id}; delete the thread instead"
                );
            }

            if let Some(p) = f {
                candidates.push(p);
            }
            if let Some(p) = t {
                candidates.push(p);
            }
            if let Some(p) = a {
                candidates.push(p);
            }

            (thread_id, is_op, candidates)
        };

        debug_assert!(!is_op, "OP posts must be deleted through delete_thread");

        let deleted = conn
            .execute("DELETE FROM posts WHERE id = ?1", params![post_id])
            .context("Failed to delete post")?;
        if deleted == 0 {
            anyhow::bail!("Post id {post_id} not found");
        }

        let updated = conn.execute(
            "UPDATE threads
             SET reply_count = CASE
                 WHEN reply_count > 0 THEN reply_count - 1
                 ELSE 0
             END
             WHERE id = ?1",
            params![thread_id],
        )?;
        if updated == 0 {
            anyhow::bail!("Thread id {thread_id} not found while updating reply count");
        }

        // Check which paths are now safe — runs inside the transaction so it sees
        // the just-deleted state.
        let safe = super::paths_safe_to_delete(conn, candidates)?;
        Ok(safe)
    })();

    match result {
        Ok(safe) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_post transaction")?;
            Ok(safe)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Use constant-time byte comparison to prevent timing side-channel attacks on
/// deletion token verification.
///
/// Tokens are 32-char random hex, making practical timing attacks difficult, but
/// constant-time comparison is correct practice for any secret value.
///
/// Note: `edit_post` inlines its own transactional token check, so this helper
/// is not currently called there. Kept for future handlers (e.g. user-facing
/// post deletion) that need standalone token verification.
///
/// # Errors
/// Returns an error if the database operation fails.
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

    Ok(stored.is_some_and(|s| constant_time_eq(s.as_bytes(), token.as_bytes())))
}

/// Edit a post's body, verified against the deletion token and a per-board edit window.
///
/// `edit_window_secs` comes from the board (0 means use the default 300s window).
/// The caller is responsible for checking `board.allow_editing` before calling this.
/// Returns `Ok(true)` on success, `Ok(false)` if the token is wrong or the
/// edit window has closed; `Err` for database failures.
///
/// Upgraded from DEFERRED (`unchecked_transaction`) to IMMEDIATE by
/// issuing BEGIN IMMEDIATE explicitly. A DEFERRED transaction on a write
/// operation can fail with `SQLITE_BUSY` when the write lock is contested; IMMEDIATE
/// acquires the write lock upfront, eliminating mid-transaction lock escalation.
///
/// The previous two-round-trip design (one SELECT for the token,
/// a second SELECT for `created_at`) introduced a race window: the post could be
/// deleted between the token check and the timestamp fetch. Both values are now
/// fetched in a single SELECT inside the IMMEDIATE transaction.
///
/// # Errors
/// Returns an error if the database operation fails.
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
        // Fetch token and created_at in a single round-trip.
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT deletion_token, created_at FROM posts WHERE id = ?1",
                params![post_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((stored_token, created_at)) = row else {
            return Ok(false); // post does not exist
        };

        if !constant_time_eq(stored_token.as_bytes(), token.as_bytes()) {
            return Ok(false);
        }

        let now = chrono::Utc::now().timestamp();
        if now.saturating_sub(created_at) > window {
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
/// The previous implementation returned false immediately when
/// lengths differed, leaking token length as a timing signal. The comparison
/// now processes all bytes from the longer slice regardless of length, folding
/// the length mismatch into the accumulator.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    // Non-zero when lengths differ.
    let mut diff = u8::try_from(a.len() ^ b.len()).unwrap_or(u8::MAX);
    for i in 0..max_len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

// ─── LIKE escape helper ───────────────────────────────────────────────────────

/// Extract conservative FTS-safe tokens from free-form user input.
///
/// `SQLite` FTS5 treats punctuation-heavy input as query syntax, so raw tokens like
/// `'`, `"`, or `>>1` can raise syntax errors when passed through directly.
/// Normalizing to alphanumeric search terms preserves ordinary text search while
/// degrading punctuation-only input into a harmless "no results" query.
fn search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();

    for ch in query.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
            continue;
        }

        if !current.is_empty() {
            terms.push(std::mem::take(&mut current));
            if terms.len() >= 12 {
                return terms;
            }
        }
    }

    if !current.is_empty() && terms.len() < 12 {
        terms.push(current);
    }

    terms
}

/// Build a conservative FTS5 query from free-form user input.
///
/// Each token becomes an `AND`-joined prefix term so searches remain fast on the FTS
/// index without exposing raw FTS syntax to the user.
fn to_fts_query(query: &str) -> Option<String> {
    let terms = search_terms(query)
        .into_iter()
        .map(|term| format!(r#""{}"*"#, term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

// ─── Search ───────────────────────────────────────────────────────────────────

/// Full-text search across post bodies.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn search_posts(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Post>> {
    let Some(fts_query) = to_fts_query(query) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare_cached(
        "SELECT posts.id, posts.thread_id, posts.board_id, posts.name, posts.tripcode,
                posts.subject, posts.body, posts.body_html, posts.ip_hash,
                posts.file_path, posts.file_name, posts.file_size, posts.thumb_path,
                posts.mime_type, posts.created_at, posts.deletion_token, posts.is_op,
                posts.media_type, posts.audio_file_path, posts.audio_file_name,
                posts.audio_file_size, posts.audio_mime_type, posts.edited_at
         FROM posts
         JOIN posts_fts ON posts_fts.rowid = posts.id
         WHERE posts.board_id = ?1 AND posts_fts MATCH ?2
         ORDER BY posts.created_at DESC, posts.id DESC
         LIMIT ?3 OFFSET ?4",
    )?;
    let posts = stmt
        .query_map(params![board_id, fts_query, limit, offset], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn count_search_results(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
) -> Result<i64> {
    let Some(fts_query) = to_fts_query(query) else {
        return Ok(0);
    };
    Ok(conn.query_row(
        "SELECT COUNT(*)
         FROM posts
         JOIN posts_fts ON posts_fts.rowid = posts.id
         WHERE posts.board_id = ?1 AND posts_fts MATCH ?2",
        params![board_id, fts_query],
        |r| r.get(0),
    )?)
}

// ─── File deduplication ───────────────────────────────────────────────────────

/// Look up an existing upload by its SHA-256 hash.
///
/// # Errors
/// Returns an error if the database operation fails.
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
///
/// Uses INSERT OR REPLACE so that if the same SHA-256 was previously stored
/// with an unconverted format (e.g. image/jpeg stored before WebP conversion
/// was enabled), re-uploading the same bytes will update the cache to point
/// at the converted file and mime type. Without OR REPLACE, the stale
/// cache entry would be returned on every subsequent upload of that image,
/// silently skipping conversion forever.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn record_file_hash(
    conn: &rusqlite::Connection,
    sha256: &str,
    file_path: &str,
    thumb_path: &str,
    mime_type: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO file_hashes (sha256, file_path, thumb_path, mime_type)
         VALUES (?1, ?2, ?3, ?4)",
        params![sha256, file_path, thumb_path, mime_type],
    )?;
    Ok(())
}

// ─── Poll queries ─────────────────────────────────────────────────────────────

/// Fetch the full poll for a thread including vote counts and the user's choice.
///
/// Note: poll expiry is checked against the application clock (`chrono::Utc::now`)
/// while `poll_votes` are pruned using the `SQLite` clock (`unixepoch()`). A skew
/// between the two clocks (e.g. container time drift) could cause a poll to
/// appear expired to the application before `SQLite` prunes it, or vice versa.
/// In practice the skew is negligible for typical deployments.
///
/// # Errors
/// Returns an error if the database operation fails.
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

    let Some(poll) = poll_row else {
        return Ok(None);
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
/// Validates that `option_id` belongs to `poll_id` inside
/// the same INSERT statement via a correlated WHERE EXISTS. A mismatched
/// (`poll_id`, `option_id`) pair inserts nothing and returns false.
///
/// Note (): This function returns false for two distinct cases:
///   1. The voter has already voted (UNIQUE constraint fires INSERT OR IGNORE)
///   2. The `option_id` does not belong to `poll_id` (EXISTS check fails)
///
/// Callers that need to distinguish these cases should call `cast_vote` and, on
/// false, separately query whether the IP has voted on this poll. A future
/// refactor could return a tri-state enum instead.
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Resolve (`poll_id`, `thread_id`, `board_short`) from an `option_id`.
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Delete vote rows for polls whose `expires_at` is older than the given cutoff timestamp.
///
/// The poll question and options are preserved for historical display; only
/// the per-IP vote records are pruned.
///
/// Returns the number of vote rows deleted.
///
/// Note (): The parameter was previously named `retention_cutoff` in some
/// call sites, which is misleading — a lower value retains more votes, and a
/// higher value prunes more. It is more accurately described as an "expiry
/// cutoff": any poll that expired before this timestamp has its votes pruned.
///
/// # Errors
/// Returns an error if the database operation fails.
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
/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
///
/// # Errors
/// Returns an error if the database operation fails.
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
/// its retry budget. Returns (`job_id`, payload) or None when the queue is empty.
///
/// The UPDATE … RETURNING subquery is a single atomic operation in `SQLite`'s
/// WAL mode, so no two workers can claim the same job.
///
/// # Errors
/// Returns an error if the database operation fails.
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
/// Added rows-affected check — silently succeeding for an unknown
/// `job_id` made double-complete bugs invisible.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn complete_job(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE background_jobs SET status = 'done', updated_at = unixepoch()
         WHERE id = ?1 AND status = 'running'",
        params![id],
    )?;
    if n == 0 {
        anyhow::bail!("Job {id} not found or not in 'running' state");
    }
    Ok(())
}

/// Record a job failure. After `MAX_JOB_ATTEMPTS` the job stays "failed" permanently.
///
/// Added rows-affected check.
/// Uses `MAX_JOB_ATTEMPTS` constant instead of duplicating the magic number.
///
/// # Errors
/// Returns an error if the database operation fails.
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
        anyhow::bail!("Job {id} not found or not in 'running' state");
    }
    Ok(())
}

/// Count jobs currently in the 'pending' state (used for monitoring).
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Update a post's `thumb_path` after background waveform / thumbnail generation.
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Retrieve just the `thumb_path` for a post (used by `VideoTranscode` worker to
/// preserve the existing thumbnail when refreshing the file-hash record).
///
/// # Errors
/// Returns an error if the database operation fails.
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

/// Atomically replace a transcoded media path everywhere it is referenced.
///
/// # Errors
/// Returns an error if any post update or file-hash rewrite fails.
pub fn replace_transcoded_media(
    conn: &rusqlite::Connection,
    post_id: i64,
    old_path: &str,
    new_path: &str,
    new_mime: &str,
    new_sha256: &str,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin transcode media replacement transaction")?;

    let result: anyhow::Result<()> = (|| {
        let updated = conn.execute(
            "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE file_path = ?3",
            params![new_path, new_mime, old_path],
        )?;
        if updated == 0 {
            conn.execute(
                "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE id = ?3",
                params![new_path, new_mime, post_id],
            )?;
        }

        let thumb_path = get_post_thumb_path(conn, post_id)?.unwrap_or_default();
        conn.execute(
            "DELETE FROM file_hashes WHERE file_path = ?1",
            params![old_path],
        )?;
        record_file_hash(conn, new_sha256, new_path, &thumb_path, new_mime)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit transcode media replacement transaction")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Remove a file-hash record for a path that is being rolled back.
///
/// # Errors
/// Returns an error if the deduplication row cannot be deleted.
pub fn delete_file_hash_by_path(conn: &rusqlite::Connection, file_path: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM file_hashes WHERE file_path = ?1",
        params![file_path],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        count_search_results, get_post_submission, record_post_submission, search_posts,
        search_terms, to_fts_query,
    };
    use crate::db::{create_board, create_thread_with_optional_poll, get_board_by_short, NewPost};
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        super::super::schema::create_schema(&conn).expect("create schema");
        conn
    }

    fn seed_search_post(conn: &Connection, board_short: &str, body: &str) -> i64 {
        create_board(conn, board_short, board_short, "", false).expect("create board");
        let board = get_board_by_short(conn, board_short)
            .expect("load board")
            .expect("board exists");
        let post = NewPost {
            thread_id: 0,
            board_id: board.id,
            name: "anon".to_string(),
            tripcode: None,
            subject: Some(format!("{board_short} subject")),
            body: body.to_string(),
            body_html: body.to_string(),
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
            deletion_token: "token".to_string(),
            is_op: true,
        };
        let (thread_id, post_id, _) =
            create_thread_with_optional_poll(conn, board.id, None, &post, "", None, None)
                .expect("create thread");
        assert!(thread_id > 0);
        post_id
    }

    #[test]
    fn search_query_ignores_punctuation_only_input() {
        assert_eq!(to_fts_query("'"), None);
        assert_eq!(to_fts_query("\""), None);
        assert_eq!(to_fts_query("... !!!"), None);
    }

    #[test]
    fn search_query_strips_chan_punctuation_without_crashing() {
        assert_eq!(search_terms(">>1"), vec!["1"]);
        assert_eq!(search_terms("💥💥💥   >>1 ' \" %"), vec!["1"]);
        assert_eq!(to_fts_query(">>1"), Some("\"1\"*".to_string()));
    }

    #[test]
    fn search_query_keeps_text_terms_usable() {
        assert_eq!(
            search_terms("rock'n'roll C++ anime"),
            vec!["rock", "n", "roll", "c", "anime"]
        );
        assert_eq!(
            to_fts_query("hello world"),
            Some("\"hello\"* AND \"world\"*".to_string())
        );
    }

    #[test]
    fn search_query_lowercases_even_when_token_cap_is_hit() {
        assert_eq!(
            search_terms("A B C D E F G H I J K L M"),
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"]
        );
    }

    #[test]
    fn search_posts_reads_joined_fts_rows_without_ambiguous_columns() {
        let conn = test_conn();
        seed_search_post(&conn, "tech", "rust search body");
        let board = get_board_by_short(&conn, "tech")
            .expect("load board")
            .expect("board exists");

        let posts = search_posts(&conn, board.id, "rust", 20, 0).expect("search posts");

        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].body, "rust search body");
    }

    #[test]
    fn search_posts_stays_scoped_to_board() {
        let conn = test_conn();
        seed_search_post(&conn, "tech", "shared rust term");
        seed_search_post(&conn, "meta", "shared rust term");
        let tech = get_board_by_short(&conn, "tech")
            .expect("load tech board")
            .expect("tech board exists");

        let posts = search_posts(&conn, tech.id, "rust", 20, 0).expect("search posts");
        let total = count_search_results(&conn, tech.id, "rust").expect("count search results");

        assert_eq!(posts.len(), 1);
        assert_eq!(total, 1);
        assert_eq!(posts[0].board_id, tech.id);
    }

    #[test]
    fn search_posts_matches_case_insensitively() {
        let conn = test_conn();
        seed_search_post(&conn, "tech", "AI will find this");
        let board = get_board_by_short(&conn, "tech")
            .expect("load board")
            .expect("board exists");

        let posts = search_posts(&conn, board.id, "ai", 20, 0).expect("search posts");
        let total = count_search_results(&conn, board.id, "ai").expect("count search results");

        assert_eq!(posts.len(), 1);
        assert_eq!(total, 1);
        assert_eq!(posts[0].body, "AI will find this");
    }

    #[test]
    fn search_posts_ignores_punctuation_only_queries_without_error() {
        let conn = test_conn();
        let total = count_search_results(&conn, 1, ">>1 ' \" %").expect("count search results");
        let posts = search_posts(&conn, 1, ">>1 ' \" %", 20, 0).expect("search posts");

        assert_eq!(total, 0);
        assert!(posts.is_empty());
    }

    #[test]
    fn post_submission_token_resolves_existing_post() {
        let conn = test_conn();
        let post_id = seed_search_post(&conn, "dup", "hello");
        let board = get_board_by_short(&conn, "dup")
            .expect("load board")
            .expect("board exists");

        record_post_submission(&conn, "token-1", "iphash", board.id, 1, post_id, true)
            .expect("record submission token");

        let record = get_post_submission(&conn, "token-1", "iphash", board.id)
            .expect("lookup token")
            .expect("record should exist");
        assert_eq!(record.thread_id, 1);
        assert_eq!(record.post_id, post_id);
        assert!(record.is_thread);
    }
}
