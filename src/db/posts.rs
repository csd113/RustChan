// db/posts.rs — Post queries, file deduplication, polls, and the background
//               job queue (including worker-side update helpers).
//
// Dependency notes:
//   create_post_inner  is pub(super) — threads.rs calls it inside
//                      create_thread_with_op's manual transaction.
//   delete_post        calls super::paths_safe_to_delete.
//
use crate::models::Post;
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use std::collections::HashMap;
use std::fmt;

// ─── Retry budget constant ────────────────────────────────────────────────────

/// Single source of truth for the job retry budget.
/// Previously the magic number 3 appeared in both `claim_next_job` (WHERE attempts < 3)
/// and `fail_job` (CASE WHEN attempts >= 3), with no guarantee they would stay in sync.
const MAX_JOB_ATTEMPTS: i64 = 3;
/// Shared projection used to decode a complete post.
const POST_SELECT_COLUMNS: &str = "id, thread_id, board_id, name, tripcode, subject, body, \
    body_html, ip_hash, file_path, file_name, file_size, thumb_path, mime_type, created_at, \
    deletion_token, is_op, media_type, audio_file_path, audio_file_name, audio_file_size, \
    audio_mime_type, edited_at, media_processing_state, media_processing_error";
/// Shared projection used to decode a complete post selected with alias `p`.
const POST_SELECT_COLUMNS_WITH_P_ALIAS: &str =
    "p.id, p.thread_id, p.board_id, p.name, p.tripcode, p.subject, p.body, p.body_html, \
    p.ip_hash, p.file_path, p.file_name, p.file_size, p.thumb_path, p.mime_type, p.created_at, \
    p.deletion_token, p.is_op, p.media_type, p.audio_file_path, p.audio_file_name, \
    p.audio_file_size, p.audio_mime_type, p.edited_at, p.media_processing_state, \
    p.media_processing_error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// State assigned after recording a background-job failure.
pub enum JobFailureState {
    /// The job remains eligible for another attempt.
    Retrying,
    /// The job exhausted its retry budget.
    PermanentlyFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Counts returned after recovering jobs interrupted by shutdown.
pub struct InterruptedJobRecovery {
    /// Interrupted jobs returned to the pending queue.
    pub jobs_reset: i64,
    /// Interrupted jobs already reflected in persisted media state.
    pub jobs_resolved: i64,
    /// Media posts whose processing state was cleared.
    pub media_posts_reset: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Aggregate status counts for the background-job dashboard.
pub struct BackgroundJobSummary {
    /// Jobs currently claimed by workers.
    pub running: i64,
    /// Jobs waiting to be claimed.
    pub queued: i64,
    /// Jobs completed during the recent reporting window.
    pub recent_completed: i64,
    /// Unacknowledged permanently failed jobs.
    pub failed: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One recent background-job record for operator diagnostics.
pub struct RecentBackgroundJob {
    /// Database row identifier.
    pub id: i64,
    /// Worker job-kind discriminator.
    pub job_type: String,
    /// Serialized worker payload.
    pub payload: String,
    /// Current lifecycle status.
    pub status: String,
    /// Number of claim attempts.
    pub attempts: i64,
    /// Most recent worker error, when present.
    pub last_error: Option<String>,
    /// Unix timestamp of the latest state transition.
    pub updated_at: i64,
}

// ─── Row mapper ───────────────────────────────────────────────────────────────

/// Map a full post row (25 columns, selected in the canonical order used
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
///   7  `body_html`     15 `deletion_token` 23 `media_processing_state`
///                                           24 `media_processing_error`
///
/// # Errors
/// Returns an error if the database operation fails.
pub(super) fn map_post(row: &rusqlite::Row<'_>) -> rusqlite::Result<Post> {
    let media_type_str: Option<String> = row.get(17)?;
    let media_type = media_type_str
        .as_deref()
        .and_then(crate::models::MediaType::from_db_str);
    let media_processing_state = row
        .get::<_, Option<String>>(23)?
        .filter(|state| !state.trim().is_empty());
    let media_processing_error = row
        .get::<_, Option<String>>(24)?
        .filter(|error| !error.trim().is_empty());

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
        media_processing_state,
        media_processing_error,
    })
}

// ─── Post queries ─────────────────────────────────────────────────────────────

/// # Errors
/// Returns an error if the database operation fails.
pub fn get_posts_for_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {POST_SELECT_COLUMNS}
         FROM posts WHERE thread_id = ?1 ORDER BY created_at ASC, id ASC"
    ))?;
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
    board_id: i64,
    thread_id: i64,
    since_id: i64,
    max_results: i64,
) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {POST_SELECT_COLUMNS}
         FROM posts WHERE board_id = ?1 AND thread_id = ?2 AND id > ?3
         ORDER BY id ASC
         LIMIT ?4"
    ))?;
    let posts = stmt
        .query_map(
            params![board_id, thread_id, since_id, max_results],
            map_post,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Fetch specific posts in a thread by id, ordered oldest-first.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_posts_by_ids_in_thread(
    conn: &rusqlite::Connection,
    board_id: i64,
    thread_id: i64,
    post_ids: &[i64],
) -> Result<Vec<Post>> {
    if post_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = post_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("?{}", index + 3))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {POST_SELECT_COLUMNS}
         FROM posts
         WHERE board_id = ?1 AND thread_id = ?2 AND id IN ({placeholders})
         ORDER BY created_at ASC, id ASC"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let params = rusqlite::params_from_iter(
        [board_id, thread_id]
            .into_iter()
            .chain(post_ids.iter().copied()),
    );
    let posts = stmt
        .query_map(params, map_post)?
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
        "SELECT {POST_SELECT_COLUMNS}
         FROM (
             SELECT {POST_SELECT_COLUMNS},
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
/// Idempotency record for one accepted post submission.
pub struct PostSubmissionRecord {
    /// Thread associated with the accepted submission.
    pub thread_id: i64,
    /// Post created by the accepted submission.
    pub post_id: i64,
    /// Whether the submission created a new thread.
    pub is_thread: bool,
}

/// Look up an existing post submission token.
///
/// # Errors
/// Returns an error if the database query fails.
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

/// Store a post submission token and prune expired rows.
///
/// # Errors
/// Returns an error if the token already exists or either database write fails.
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
        "INSERT INTO post_submissions
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
    .context("Failed to record unique post submission token")?;

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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {POST_SELECT_COLUMNS}
         FROM posts WHERE id = ?1"
    ))?;
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {POST_SELECT_COLUMNS_WITH_P_ALIAS}
         FROM posts p
         JOIN boards b ON b.id = p.board_id
         WHERE p.id = ?1 AND b.short_name = ?2
         LIMIT 1"
    ))?;
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
pub fn delete_post(
    conn: &rusqlite::Connection,
    post_id: i64,
) -> crate::error::Result<crate::db::DeletePathsResult> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_post transaction")?;

    let result: crate::error::Result<crate::db::DeletePathsResult> =
        delete_post_reply_in_tx(conn, post_id);

    match result {
        Ok(safe) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_post transaction")?;
            Ok(safe)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

/// Delete a non-opening post inside the caller's transaction.
fn delete_post_reply_in_tx(
    conn: &rusqlite::Connection,
    post_id: i64,
) -> crate::error::Result<crate::db::DeletePathsResult> {
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
            return Err(crate::error::AppError::NotFound(format!(
                "Post id {post_id} not found"
            )));
        };

        if is_op {
            return Err(crate::error::AppError::BadRequest(format!(
                "Post id {post_id} is the OP for thread {thread_id}; delete the thread instead"
            )));
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
        return Err(crate::error::AppError::NotFound(format!(
            "Post id {post_id} not found"
        )));
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
        return Err(crate::error::AppError::NotFound(format!(
            "Thread id {thread_id} not found while updating reply count"
        )));
    }

    // Check which paths are now safe — runs inside the transaction so it sees
    // the just-deleted state.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Outcome of a self-service post deletion request.
pub enum SelfDeleteOutcome {
    /// A reply was deleted.
    DeletedReply,
    /// An opening post and its thread were deleted.
    DeletedThread,
    /// The requested post does not exist.
    NotFound,
    /// The deletion token did not match.
    WrongToken,
    /// The board's self-delete window has elapsed.
    WindowClosed,
    /// The parent thread is locked or archived.
    ThreadClosed,
    /// The opening post cannot be deleted after replies exist.
    ThreadHasReplies,
}

/// Return whether a post's parent thread is currently open for self-actions.
///
/// # Errors
/// Returns an error if the database query fails.
pub fn post_thread_allows_self_actions(
    conn: &rusqlite::Connection,
    post_id: i64,
) -> crate::error::Result<bool> {
    let flags: Option<(bool, bool)> = conn
        .query_row(
            "SELECT t.locked, t.archived
             FROM posts p
             JOIN threads t ON t.id = p.thread_id
             WHERE p.id = ?1",
            params![post_id],
            |row| Ok((row.get::<_, i32>(0)? != 0, row.get::<_, i32>(1)? != 0)),
        )
        .optional()?;
    Ok(flags.is_some_and(|(locked, archived)| !locked && !archived))
}

/// Delete a post owned by the caller during the short self-delete grace window.
///
/// Returns the outcome plus any filesystem paths that should be cleaned up by
/// the caller when the delete succeeds.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn self_delete_post(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
    delete_window_secs: i64,
) -> crate::error::Result<(SelfDeleteOutcome, Option<crate::db::DeletePathsResult>)> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin self_delete_post transaction")?;

    let result: crate::error::Result<(SelfDeleteOutcome, Option<crate::db::DeletePathsResult>)> =
        (|| {
            let row: Option<(i64, bool, String, i64, bool, bool)> = conn
                .query_row(
                    "SELECT p.thread_id, p.is_op, p.deletion_token, p.created_at,
                            t.locked, t.archived
                     FROM posts p
                     JOIN threads t ON t.id = p.thread_id
                     WHERE p.id = ?1",
                    params![post_id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i32>(1)? != 0,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i32>(4)? != 0,
                            r.get::<_, i32>(5)? != 0,
                        ))
                    },
                )
                .optional()?;

            let Some((thread_id, is_op, stored_token, created_at, locked, archived)) = row else {
                return Ok((SelfDeleteOutcome::NotFound, None));
            };

            if !constant_time_eq(stored_token.as_bytes(), token.as_bytes()) {
                return Ok((SelfDeleteOutcome::WrongToken, None));
            }

            let now = chrono::Utc::now().timestamp();
            if now.saturating_sub(created_at) > delete_window_secs {
                return Ok((SelfDeleteOutcome::WindowClosed, None));
            }

            if locked || archived {
                return Ok((SelfDeleteOutcome::ThreadClosed, None));
            }

            if is_op {
                let reply_count: i64 = conn.query_row(
                    "SELECT reply_count FROM threads WHERE id = ?1",
                    params![thread_id],
                    |r| r.get(0),
                )?;
                if reply_count > 0 {
                    return Ok((SelfDeleteOutcome::ThreadHasReplies, None));
                }

                let deleted = crate::db::threads::delete_thread_verified(conn, thread_id)?;
                return Ok((SelfDeleteOutcome::DeletedThread, Some(deleted)));
            }

            let deleted = delete_post_reply_in_tx(conn, post_id)?;
            Ok((SelfDeleteOutcome::DeletedReply, Some(deleted)))
        })();

    match result {
        Ok(outcome) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit self_delete_post transaction")?;
            Ok(outcome)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Edit a post's body, verified against the deletion token and a per-board edit window.
///
/// `edit_window_secs` comes from the caller (0 means use the default 60s window).
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
        60
    } else {
        edit_window_secs
    };

    // BEGIN IMMEDIATE acquires the write lock now, preventing any concurrent
    // writer from modifying the post between our SELECT and UPDATE.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin IMMEDIATE transaction for edit_post")?;

    let result: Result<bool> = (|| {
        // Fetch token and created_at in a single round-trip.
        let row: Option<(String, i64, bool, bool)> = conn
            .query_row(
                "SELECT p.deletion_token, p.created_at, t.locked, t.archived
                 FROM posts p
                 JOIN threads t ON t.id = p.thread_id
                 WHERE p.id = ?1",
                params![post_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get::<_, i32>(2)? != 0,
                        r.get::<_, i32>(3)? != 0,
                    ))
                },
            )
            .optional()?;

        let Some((stored_token, created_at, locked, archived)) = row else {
            return Ok(false); // post does not exist
        };

        if !constant_time_eq(stored_token.as_bytes(), token.as_bytes()) {
            return Ok(false);
        }

        let now = chrono::Utc::now().timestamp();
        if now.saturating_sub(created_at) > window {
            return Ok(false);
        }

        if locked || archived {
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
            drop(conn.execute_batch("ROLLBACK"));
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
                posts.audio_file_size, posts.audio_mime_type, posts.edited_at,
                posts.media_processing_state, posts.media_processing_error
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
/// This returns false for two distinct cases:
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
/// This parameter is an expiry cutoff: polls that expired before it have
/// their vote rows pruned.
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

/// Site-setting key containing the highest acknowledged failed-job identifier.
const FAILED_BACKGROUND_JOBS_ACK_ID_KEY: &str = "failed_background_jobs_acknowledged_through_id";

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
pub fn fail_job(conn: &rusqlite::Connection, id: i64, error: &str) -> Result<JobFailureState> {
    let err_trunc: String = error.chars().take(512).collect();
    let failure_state = conn.query_row(
        "SELECT CASE WHEN attempts >= ?2 THEN 'failed' ELSE 'pending' END
         FROM background_jobs
         WHERE id = ?1 AND status = 'running'",
        params![id, MAX_JOB_ATTEMPTS],
        |r| {
            let state: String = r.get(0)?;
            Ok(if state == "failed" {
                JobFailureState::PermanentlyFailed
            } else {
                JobFailureState::Retrying
            })
        },
    )?;
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
    Ok(failure_state)
}

/// Count jobs currently in the 'pending' state (used for monitoring).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn pending_job_count(conn: &rusqlite::Connection) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM background_jobs WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Return a compact background job summary for admin status displays.
///
/// # Errors
/// Returns an error if the database queries fail.
pub fn background_job_summary(conn: &rusqlite::Connection) -> Result<BackgroundJobSummary> {
    let acknowledged_failed_id = acknowledged_failed_background_job_id(conn)?;
    let (running, queued, recent_completed, failed) = conn.query_row(
        "SELECT
             SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END),
             SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END),
             SUM(CASE WHEN status = 'done' AND updated_at >= unixepoch() - 86400 THEN 1 ELSE 0 END),
             SUM(CASE WHEN status = 'failed' AND id > ?1 THEN 1 ELSE 0 END)
         FROM background_jobs",
        params![acknowledged_failed_id],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            ))
        },
    )?;
    Ok(BackgroundJobSummary {
        running,
        queued,
        recent_completed,
        failed,
    })
}

/// Read the highest background-job failure acknowledged by an administrator.
fn acknowledged_failed_background_job_id(conn: &rusqlite::Connection) -> Result<i64> {
    let value = super::get_site_setting(conn, FAILED_BACKGROUND_JOBS_ACK_ID_KEY)?;
    Ok(value
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0))
}

/// Mark all currently failed background jobs as acknowledged for admin counters.
///
/// This intentionally preserves the `background_jobs` rows, payloads, and
/// errors. Only the admin attention counter is reset until a newer failure
/// appears.
///
/// # Errors
/// Returns an error if the database queries or site-setting update fail.
pub fn acknowledge_failed_background_jobs(conn: &rusqlite::Connection) -> Result<i64> {
    let max_failed_id = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM background_jobs WHERE status = 'failed'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    super::set_site_setting(
        conn,
        FAILED_BACKGROUND_JOBS_ACK_ID_KEY,
        &max_failed_id.to_string(),
    )?;
    Ok(max_failed_id)
}

/// Return recent terminal background jobs for bounded admin diagnostics.
///
/// # Errors
/// Returns an error if the database query fails.
pub fn recent_background_jobs(
    conn: &rusqlite::Connection,
    status: &str,
    limit: u32,
) -> Result<Vec<RecentBackgroundJob>> {
    anyhow::ensure!(
        matches!(status, "done" | "failed"),
        "unsupported job status"
    );
    let mut stmt = conn.prepare_cached(
        "SELECT id, job_type, payload, status, attempts, last_error, updated_at
         FROM background_jobs
         WHERE status = ?1
         ORDER BY updated_at DESC, id DESC
         LIMIT ?2",
    )?;
    let jobs = stmt
        .query_map(params![status, limit.min(25)], |row| {
            Ok(RecentBackgroundJob {
                id: row.get(0)?,
                job_type: row.get(1)?,
                payload: row.get(2)?,
                status: row.get(3)?,
                attempts: row.get(4)?,
                last_error: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(jobs)
}

/// Recover jobs that were interrupted after being claimed but before completion.
///
/// This is intended for startup before workers begin claiming jobs. Media jobs
/// whose database mutation is observably complete or stale are resolved without
/// replaying their external work. Other jobs return to the bounded retry queue.
///
/// # Errors
/// Returns an error if the recovery queries fail.
pub fn recover_interrupted_background_jobs(
    conn: &rusqlite::Connection,
) -> Result<InterruptedJobRecovery> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin interrupted-job recovery transaction")?;
    let result = (|| {
        let interrupted_jobs = interrupted_background_jobs(conn)?;
        let mut jobs_reset = 0_i64;
        let mut jobs_resolved = 0_i64;
        let mut media_posts_reset = 0_i64;

        for job in interrupted_jobs {
            match interrupted_media_job_disposition(conn, &job)? {
                InterruptedJobDisposition::Requeue { media_post_id } => {
                    let updated = conn.execute(
                        "UPDATE background_jobs
                         SET status = 'pending',
                             attempts = CASE WHEN attempts > 0 THEN attempts - 1 ELSE 0 END,
                             last_error = NULL,
                             updated_at = unixepoch()
                         WHERE id = ?1 AND status = 'running'",
                        params![job.id],
                    )?;
                    jobs_reset =
                        jobs_reset.saturating_add(i64::try_from(updated).unwrap_or(i64::MAX));
                    if let Some(post_id) = media_post_id {
                        let reset = conn.execute(
                            "UPDATE posts
                             SET media_processing_state = ?1,
                                 media_processing_error = NULL
                             WHERE id = ?2",
                            params![MEDIA_PROCESSING_PENDING, post_id],
                        )?;
                        media_posts_reset = media_posts_reset
                            .saturating_add(i64::try_from(reset).unwrap_or(i64::MAX));
                    }
                }
                InterruptedJobDisposition::Resolve {
                    clear_media_post_id,
                } => {
                    let updated = conn.execute(
                        "UPDATE background_jobs
                         SET status = 'done',
                             last_error = NULL,
                             updated_at = unixepoch()
                         WHERE id = ?1 AND status = 'running'",
                        params![job.id],
                    )?;
                    jobs_resolved =
                        jobs_resolved.saturating_add(i64::try_from(updated).unwrap_or(i64::MAX));
                    if let Some(post_id) = clear_media_post_id {
                        set_post_media_processing_state(conn, post_id, None, None)?;
                    }
                }
            }
        }

        Ok(InterruptedJobRecovery {
            jobs_reset,
            jobs_resolved,
            media_posts_reset,
        })
    })();

    match result {
        Ok(recovery) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit interrupted-job recovery");
            }
            Ok(recovery)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Persisted fields required to classify an interrupted background job.
struct InterruptedBackgroundJob {
    /// Background-job row identifier.
    id: i64,
    /// Worker job-kind discriminator.
    job_type: String,
    /// Serialized worker payload.
    payload: String,
}

/// Load every job left in the running state.
fn interrupted_background_jobs(
    conn: &rusqlite::Connection,
) -> Result<Vec<InterruptedBackgroundJob>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, job_type, payload
         FROM background_jobs
         WHERE status = 'running'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(InterruptedBackgroundJob {
            id: row.get(0)?,
            job_type: row.get(1)?,
            payload: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// Recovery action selected for an interrupted background job.
enum InterruptedJobDisposition {
    /// Return the job to the queue and optionally restore its media marker.
    Requeue {
        /// Post whose media-processing marker should remain pending.
        media_post_id: Option<i64>,
    },
    /// Mark the job complete because its side effect already happened.
    Resolve {
        /// Post whose now-complete media marker should be cleared.
        clear_media_post_id: Option<i64>,
    },
}

/// Media target parsed from an interrupted worker payload.
enum InterruptedMediaTarget {
    /// Video-transcode target.
    Video {
        /// Target post identifier.
        post_id: i64,
        /// Source media path captured by the job.
        source_path: String,
        /// Deterministic transcoded output path.
        expected_output_path: Option<String>,
    },
    /// Audio-waveform target.
    Audio {
        /// Target post identifier.
        post_id: i64,
        /// Source media path captured by the job.
        source_path: String,
        /// Deterministic waveform thumbnail path.
        expected_thumb_path: Option<String>,
    },
}

/// Determine whether an interrupted job must be replayed or was already applied.
fn interrupted_media_job_disposition(
    conn: &rusqlite::Connection,
    job: &InterruptedBackgroundJob,
) -> Result<InterruptedJobDisposition> {
    let Some(target) = interrupted_media_target(&job.job_type, &job.payload) else {
        return Ok(InterruptedJobDisposition::Requeue {
            media_post_id: None,
        });
    };
    let post_id = match &target {
        InterruptedMediaTarget::Video { post_id, .. }
        | InterruptedMediaTarget::Audio { post_id, .. } => *post_id,
    };
    let post = conn
        .query_row(
            "SELECT file_path, thumb_path
             FROM posts
             WHERE id = ?1",
            rusqlite::params![post_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()?;
    let Some((current_path, current_thumb_path)) = post else {
        // The target was deleted while work was in flight. Replaying cannot
        // attach the output and would only repeat an obsolete side effect.
        return Ok(InterruptedJobDisposition::Resolve {
            clear_media_post_id: None,
        });
    };

    match target {
        InterruptedMediaTarget::Video {
            post_id,
            source_path,
            expected_output_path,
        } => {
            if current_path.as_deref() == Some(source_path.as_str()) {
                return Ok(InterruptedJobDisposition::Requeue {
                    media_post_id: Some(post_id),
                });
            }

            // A changed path proves that this payload is no longer applicable.
            // Clear state only when it changed to this job's deterministic
            // output; otherwise a newer media job owns the post state.
            let clear_media_post_id =
                (current_path.as_deref() == expected_output_path.as_deref()).then_some(post_id);
            Ok(InterruptedJobDisposition::Resolve {
                clear_media_post_id,
            })
        }
        InterruptedMediaTarget::Audio {
            post_id,
            source_path,
            expected_thumb_path,
        } => {
            if current_path.as_deref() != Some(source_path.as_str()) {
                return Ok(InterruptedJobDisposition::Resolve {
                    clear_media_post_id: None,
                });
            }
            if current_thumb_path.as_deref() == expected_thumb_path.as_deref()
                && expected_thumb_path.is_some()
            {
                return Ok(InterruptedJobDisposition::Resolve {
                    clear_media_post_id: Some(post_id),
                });
            }
            Ok(InterruptedJobDisposition::Requeue {
                media_post_id: Some(post_id),
            })
        }
    }
}

/// Parse a supported media target from a worker job payload.
fn interrupted_media_target(job_type: &str, payload: &str) -> Option<InterruptedMediaTarget> {
    if !matches!(job_type, "video_transcode" | "audio_waveform") {
        return None;
    }

    let payload: serde_json::Value = serde_json::from_str(payload).ok()?;
    let tag = payload.get("t")?.as_str()?;
    let data = payload.get("d")?;
    let post_id = data.get("post_id")?.as_i64()?;
    let source_path = data.get("file_path")?.as_str()?.to_owned();
    let board_short = data.get("board_short")?.as_str()?;
    match (job_type, tag) {
        ("video_transcode", "VideoTranscode") => Some(InterruptedMediaTarget::Video {
            post_id,
            expected_output_path: expected_transcoded_path(&source_path, board_short),
            source_path,
        }),
        ("audio_waveform", "AudioWaveform") => Some(InterruptedMediaTarget::Audio {
            post_id,
            expected_thumb_path: expected_waveform_thumb_path(&source_path, board_short),
            source_path,
        }),
        _ => None,
    }
}

/// Derive the deterministic transcode output path for a source video.
fn expected_transcoded_path(source_path: &str, board_short: &str) -> Option<String> {
    let source = std::path::Path::new(source_path);
    let stem = source.file_stem()?.to_str()?;
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    let output_name = match extension.as_str() {
        "webm" => format!("{stem}.vp9.webm"),
        "mp4" | "mkv" => format!("{stem}.webm"),
        _ => return None,
    };
    Some(format!("{board_short}/{output_name}"))
}

/// Derive the deterministic waveform thumbnail path for a source audio file.
fn expected_waveform_thumb_path(source_path: &str, board_short: &str) -> Option<String> {
    let stem = std::path::Path::new(source_path).file_stem()?.to_str()?;
    if stem.is_empty() {
        return None;
    }
    Some(format!("{board_short}/thumbs/{stem}.png"))
}

/// Media-processing state assigned while work is queued or running.
pub const MEDIA_PROCESSING_PENDING: &str = "pending";
/// Media-processing state assigned after terminal worker failure.
pub const MEDIA_PROCESSING_FAILED: &str = "failed";
/// Media-processing state indicating that the original upload was pruned.
pub const MEDIA_ORIGINAL_PRUNED: &str = "pruned";

/// Update a post's async media-processing state.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn set_post_media_processing_state(
    conn: &rusqlite::Connection,
    post_id: i64,
    state: Option<&str>,
    error: Option<&str>,
) -> Result<()> {
    let state = state.unwrap_or("").trim();
    let normalized_state = if state.is_empty() { "" } else { state };
    let normalized_error = error.and_then(|detail| {
        let trimmed = detail.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.chars().take(512).collect::<String>())
        }
    });

    conn.execute(
        "UPDATE posts
         SET media_processing_state = ?1,
             media_processing_error = ?2
         WHERE id = ?3",
        params![normalized_state, normalized_error, post_id],
    )?;
    Ok(())
}

/// Count posts currently in a given async media-processing state.
///
/// # Errors
/// Returns an error if the database query fails.
pub fn count_posts_by_media_processing_state(
    conn: &rusqlite::Connection,
    state: &str,
) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_processing_state = ?1",
        params![state],
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
    expected_file_path: &str,
    thumb_path: &str,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE posts
         SET thumb_path = ?1,
             media_processing_state = '',
             media_processing_error = NULL
         WHERE id = ?2 AND file_path = ?3",
        params![thumb_path, post_id, expected_file_path],
    )?;
    if updated == 0 {
        return Err(anyhow::Error::new(StaleMediaTargetError {
            post_id,
            expected_path: expected_file_path.to_owned(),
        }));
    }
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

    let result: Result<()> = (|| {
        let target_exists = conn
            .query_row(
                "SELECT 1 FROM posts WHERE id = ?1 AND file_path = ?2 LIMIT 1",
                params![post_id, old_path],
                |_row| Ok(()),
            )
            .optional()?
            .is_some();
        if !target_exists {
            return Err(anyhow::Error::new(StaleMediaTargetError {
                post_id,
                expected_path: old_path.to_owned(),
            }));
        }

        let updated = conn.execute(
            "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE file_path = ?3",
            params![new_path, new_mime, old_path],
        )?;
        debug_assert!(updated > 0, "target_exists guarantees at least one update");
        set_post_media_processing_state(conn, post_id, None, None)?;

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
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Error raised when a media worker attempts to replace a stale post target.
pub struct StaleMediaTargetError {
    /// Post whose media target changed or disappeared.
    pub post_id: i64,
    /// Source path the worker expected to replace.
    pub expected_path: String,
}

impl fmt::Display for StaleMediaTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "media target for post {} is stale or deleted; expected path {}",
            self.post_id, self.expected_path
        )
    }
}

impl std::error::Error for StaleMediaTargetError {}

#[must_use]
/// Return whether an application error represents a stale media target.
pub fn is_stale_media_target_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<StaleMediaTargetError>().is_some()
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
        acknowledge_failed_background_jobs, background_job_summary, claim_next_job,
        count_posts_by_media_processing_state, count_search_results, get_post, get_post_submission,
        get_posts_for_thread, is_stale_media_target_error, recent_background_jobs,
        record_post_submission, recover_interrupted_background_jobs, replace_transcoded_media,
        search_posts, search_terms, self_delete_post, set_post_media_processing_state,
        to_fts_query, update_post_thumb_path, SelfDeleteOutcome, MEDIA_PROCESSING_FAILED,
        MEDIA_PROCESSING_PENDING,
    };
    use crate::db::{
        create_board, create_reply_with_thread_update, create_thread_with_optional_poll,
        get_board_by_short, get_thread, NewPost,
    };
    use crate::error::AppError;
    use anyhow::{Context as _, Result};
    use rusqlite::Connection;

    fn test_conn() -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
        super::super::schema::install_or_migrate_schema(&conn)?;
        Ok(conn)
    }

    fn seed_search_post(conn: &Connection, board_short: &str, body: &str) -> Result<i64> {
        create_board(conn, board_short, board_short, "", false)?;
        let board =
            get_board_by_short(conn, board_short)?.context("seeded search board should exist")?;
        let post = NewPost {
            thread_id: 0,
            board_id: board.id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some(format!("{board_short} subject")),
            body: body.to_owned(),
            body_html: body.to_owned(),
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
        let (thread_id, post_id, _) =
            create_thread_with_optional_poll(conn, board.id, None, &post, "", None, None)?;
        anyhow::ensure!(thread_id > 0, "seeded thread should have a positive ID");
        Ok(post_id)
    }

    fn seed_media_post(conn: &Connection, board_short: &str, file_path: &str) -> Result<i64> {
        create_board(conn, board_short, board_short, "", false)?;
        let board =
            get_board_by_short(conn, board_short)?.context("seeded media board should exist")?;
        let post = NewPost {
            thread_id: 0,
            board_id: board.id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some(format!("{board_short} subject")),
            body: "media body".to_owned(),
            body_html: "media body".to_owned(),
            ip_hash: None,
            file_path: Some(file_path.to_owned()),
            file_name: Some("media".to_owned()),
            file_size: Some(10),
            thumb_path: None,
            mime_type: Some("video/mp4".to_owned()),
            media_type: Some("video".to_owned()),
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: true,
        };
        let (_thread_id, post_id, _) =
            create_thread_with_optional_poll(conn, board.id, None, &post, "", None, None)?;
        Ok(post_id)
    }

    fn insert_background_job(
        conn: &Connection,
        job_type: &str,
        payload: &str,
        status: &str,
        attempts: i64,
        last_error: Option<&str>,
    ) -> Result<i64> {
        Ok(conn.query_row(
            "INSERT INTO background_jobs
             (job_type, payload, status, attempts, last_error, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())
             RETURNING id",
            rusqlite::params![job_type, payload, status, attempts, last_error],
            |row| row.get(0),
        )?)
    }

    fn background_job_status(conn: &Connection, id: i64) -> Result<(String, i64, Option<String>)> {
        Ok(conn.query_row(
            "SELECT status, attempts, last_error FROM background_jobs WHERE id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn recent_background_jobs_are_bounded_and_terminal_only() -> Result<()> {
        let conn = test_conn()?;
        let payload = r#"{"t":"SpamCheck","d":{"post_id":1,"ip_hash":"hash","body_len":5}}"#;
        let older =
            insert_background_job(&conn, "spam_check", payload, "failed", 3, Some("older"))?;
        let newer =
            insert_background_job(&conn, "thread_prune", payload, "failed", 2, Some("newer"))?;
        insert_background_job(&conn, "spam_check", payload, "pending", 0, None)?;

        let jobs = recent_background_jobs(&conn, "failed", 1)?;

        assert_eq!(
            jobs.len(),
            1,
            "the requested result limit should be honored"
        );
        let job = jobs.first().context("one recent job should be returned")?;
        assert_eq!(job.id, newer, "newest terminal job should be returned");
        assert_eq!(job.job_type, "thread_prune", "job type should be decoded");
        assert_eq!(job.status, "failed", "status should be decoded");
        assert_eq!(job.attempts, 2, "attempt count should be decoded");
        assert_eq!(
            job.last_error.as_deref(),
            Some("newer"),
            "last error should be decoded"
        );
        assert!(
            older < newer,
            "database IDs should preserve insertion order"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn failed_background_job_acknowledgement_preserves_history() -> Result<()> {
        let conn = test_conn()?;
        let payload = r#"{"t":"SpamCheck","d":{"post_id":1,"ip_hash":"hash","body_len":5}}"#;
        insert_background_job(&conn, "spam_check", payload, "failed", 3, Some("older"))?;
        let acknowledged =
            insert_background_job(&conn, "thread_prune", payload, "failed", 3, Some("newer"))?;

        assert_eq!(
            background_job_summary(&conn)?.failed,
            2,
            "both failures should initially require attention"
        );
        assert_eq!(
            acknowledge_failed_background_jobs(&conn)?,
            acknowledged,
            "acknowledgement should advance through the latest failure"
        );
        assert_eq!(
            background_job_summary(&conn)?.failed,
            0,
            "acknowledged failures should leave the attention count"
        );
        assert_eq!(
            recent_background_jobs(&conn, "failed", 10)?.len(),
            2,
            "acknowledgement should preserve failure history"
        );

        insert_background_job(
            &conn,
            "spam_check",
            payload,
            "failed",
            3,
            Some("new failure"),
        )?;
        assert_eq!(
            background_job_summary(&conn)?.failed,
            1,
            "a newer failure should require attention"
        );
        Ok(())
    }

    #[test]
    fn search_query_ignores_punctuation_only_input() {
        assert_eq!(to_fts_query("'"), None, "apostrophe alone has no term");
        assert_eq!(to_fts_query("\""), None, "quotation mark alone has no term");
        assert_eq!(
            to_fts_query("... !!!"),
            None,
            "punctuation-only input has no term"
        );
    }

    #[test]
    fn search_query_strips_chan_punctuation_without_crashing() {
        assert_eq!(
            search_terms(">>1"),
            vec!["1"],
            "quote marker should be stripped"
        );
        assert_eq!(
            search_terms("💥💥💥   >>1 ' \" %"),
            vec!["1"],
            "unsupported punctuation should be stripped"
        );
        assert_eq!(
            to_fts_query(">>1"),
            Some("\"1\"*".to_owned()),
            "remaining numeric term should produce a prefix query"
        );
    }

    #[test]
    fn search_query_keeps_text_terms_usable() {
        assert_eq!(
            search_terms("rock'n'roll C++ anime"),
            vec!["rock", "n", "roll", "c", "anime"],
            "text separated by punctuation should remain searchable"
        );
        assert_eq!(
            to_fts_query("hello world"),
            Some("\"hello\"* AND \"world\"*".to_owned()),
            "multiple terms should be combined with AND"
        );
    }

    #[test]
    fn search_query_lowercases_even_when_token_cap_is_hit() {
        assert_eq!(
            search_terms("A B C D E F G H I J K L M"),
            vec!["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"],
            "all retained terms should be normalized before the cap"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn search_posts_reads_joined_fts_rows_without_ambiguous_columns() -> Result<()> {
        let conn = test_conn()?;
        seed_search_post(&conn, "tech", "rust search body")?;
        let board = get_board_by_short(&conn, "tech")?.context("tech board should exist")?;

        let posts = search_posts(&conn, board.id, "rust", 20, 0)?;

        assert_eq!(posts.len(), 1, "one matching post should be returned");
        assert_eq!(
            posts.first().map(|post| post.body.as_str()),
            Some("rust search body"),
            "the joined row should decode the post body"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn search_posts_stays_scoped_to_board() -> Result<()> {
        let conn = test_conn()?;
        seed_search_post(&conn, "tech", "shared rust term")?;
        seed_search_post(&conn, "meta", "shared rust term")?;
        let tech = get_board_by_short(&conn, "tech")?.context("tech board should exist")?;

        let posts = search_posts(&conn, tech.id, "rust", 20, 0)?;
        let total = count_search_results(&conn, tech.id, "rust")?;

        assert_eq!(
            posts.len(),
            1,
            "only the requested board's post should be returned"
        );
        assert_eq!(total, 1, "count should use the same board scope");
        assert_eq!(
            posts.first().map(|post| post.board_id),
            Some(tech.id),
            "the result should belong to the requested board"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn search_posts_matches_case_insensitively() -> Result<()> {
        let conn = test_conn()?;
        seed_search_post(&conn, "tech", "AI will find this")?;
        let board = get_board_by_short(&conn, "tech")?.context("tech board should exist")?;

        let posts = search_posts(&conn, board.id, "ai", 20, 0)?;
        let total = count_search_results(&conn, board.id, "ai")?;

        assert_eq!(
            posts.len(),
            1,
            "case-insensitive search should find the post"
        );
        assert_eq!(total, 1, "case-insensitive count should find the post");
        assert_eq!(
            posts.first().map(|post| post.body.as_str()),
            Some("AI will find this"),
            "the original body casing should be preserved"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn search_posts_ignores_punctuation_only_queries_without_error() -> Result<()> {
        let conn = test_conn()?;
        let total = count_search_results(&conn, 1, ">>1 ' \" %")?;
        let posts = search_posts(&conn, 1, ">>1 ' \" %", 20, 0)?;

        assert_eq!(total, 0, "punctuation-only count should be zero");
        assert!(
            posts.is_empty(),
            "punctuation-only search should return no rows"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn post_submission_token_resolves_existing_post() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_search_post(&conn, "dup", "hello")?;
        let board = get_board_by_short(&conn, "dup")?.context("dup board should exist")?;

        record_post_submission(&conn, "token-1", "iphash", board.id, 1, post_id, true)?;

        let record = get_post_submission(&conn, "token-1", "iphash", board.id)?
            .context("submission record should exist")?;
        assert_eq!(
            record.thread_id, 1,
            "submission should retain the thread ID"
        );
        assert_eq!(
            record.post_id, post_id,
            "submission should retain the post ID"
        );
        assert!(
            record.is_thread,
            "submission should retain its new-thread classification"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn media_processing_state_round_trips_and_counts() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_search_post(&conn, "media", "hello")?;

        set_post_media_processing_state(
            &conn,
            post_id,
            Some(MEDIA_PROCESSING_FAILED),
            Some("ffmpeg timed out"),
        )?;

        let posts = get_posts_for_thread(&conn, 1)?;
        let post = posts
            .into_iter()
            .find(|post| post.id == post_id)
            .context("media post should exist")?;
        assert_eq!(
            post.media_processing_state.as_deref(),
            Some(MEDIA_PROCESSING_FAILED),
            "processing state should round-trip"
        );
        assert_eq!(
            post.media_processing_error.as_deref(),
            Some("ffmpeg timed out"),
            "processing error should round-trip"
        );
        assert_eq!(
            count_posts_by_media_processing_state(&conn, MEDIA_PROCESSING_FAILED)?,
            1,
            "failed-state count should include the post"
        );

        set_post_media_processing_state(&conn, post_id, None, None)?;
        assert_eq!(
            count_posts_by_media_processing_state(&conn, MEDIA_PROCESSING_FAILED)?,
            0,
            "cleared state should leave the failed count"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_resets_running_background_job_to_pending() -> Result<()> {
        let conn = test_conn()?;
        let payload = r#"{"t":"SpamCheck","d":{"post_id":1,"ip_hash":"hash","body_len":5}}"#;
        let job_id = insert_background_job(
            &conn,
            "spam_check",
            payload,
            "running",
            1,
            Some("worker interrupted"),
        )?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(recovery.jobs_reset, 1, "running job should be requeued");
        assert_eq!(
            recovery.jobs_resolved, 0,
            "unapplied job should not be resolved"
        );
        assert_eq!(
            recovery.media_posts_reset, 0,
            "non-media job should not affect post state"
        );
        assert_eq!(
            background_job_status(&conn, job_id)?,
            ("pending".to_owned(), 0, None),
            "requeued job should reset status, attempts, and error"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_leaves_non_running_jobs_unchanged() -> Result<()> {
        let conn = test_conn()?;
        let payload = r#"{"t":"SpamCheck","d":{"post_id":1,"ip_hash":"hash","body_len":5}}"#;
        let pending_id = insert_background_job(&conn, "spam_check", payload, "pending", 0, None)?;
        let done_id = insert_background_job(&conn, "spam_check", payload, "done", 1, None)?;
        let failed_id =
            insert_background_job(&conn, "spam_check", payload, "failed", 3, Some("bad input"))?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(
            recovery.jobs_reset, 0,
            "no non-running job should be requeued"
        );
        assert_eq!(
            recovery.jobs_resolved, 0,
            "no non-running job should be resolved"
        );
        assert_eq!(
            background_job_status(&conn, pending_id)?.0,
            "pending",
            "pending job should remain pending"
        );
        assert_eq!(
            background_job_status(&conn, done_id)?.0,
            "done",
            "completed job should remain done"
        );
        assert_eq!(
            background_job_status(&conn, failed_id)?,
            ("failed".to_owned(), 3, Some("bad input".to_owned())),
            "failed job should remain unchanged"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_restores_media_post_processing_state_to_pending() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "recover", "recover/video.mp4")?;
        set_post_media_processing_state(&conn, post_id, Some("running"), Some("old error"))?;
        let payload = format!(
            r#"{{"t":"VideoTranscode","d":{{"post_id":{post_id},"file_path":"recover/video.mp4","board_short":"recover"}}}}"#
        );
        insert_background_job(
            &conn,
            "video_transcode",
            &payload,
            "running",
            1,
            Some("old error"),
        )?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(recovery.jobs_reset, 1, "media job should be requeued");
        assert_eq!(
            recovery.jobs_resolved, 0,
            "unapplied media job should not be resolved"
        );
        assert_eq!(
            recovery.media_posts_reset, 1,
            "media post should return to pending"
        );
        let post = get_post(&conn, post_id)?.context("media post should exist")?;
        assert_eq!(
            post.media_processing_state.as_deref(),
            Some(MEDIA_PROCESSING_PENDING),
            "processing state should be pending"
        );
        assert_eq!(
            post.media_processing_error, None,
            "stale worker error should be cleared"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_resolves_applied_transcode_without_replay() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "applied", "applied/video.mp4")?;
        set_post_media_processing_state(&conn, post_id, Some("running"), Some("old error"))?;
        conn.execute(
            "UPDATE posts SET file_path = 'applied/video.webm' WHERE id = ?1",
            rusqlite::params![post_id],
        )?;
        let payload = format!(
            r#"{{"t":"VideoTranscode","d":{{"post_id":{post_id},"file_path":"applied/video.mp4","board_short":"applied"}}}}"#
        );
        let job_id = insert_background_job(
            &conn,
            "video_transcode",
            &payload,
            "running",
            1,
            Some("worker interrupted"),
        )?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(
            recovery.jobs_reset, 0,
            "applied transcode should not be requeued"
        );
        assert_eq!(
            recovery.jobs_resolved, 1,
            "applied transcode should be resolved"
        );
        assert_eq!(
            recovery.media_posts_reset, 0,
            "applied transcode should clear rather than reset state"
        );
        assert_eq!(
            background_job_status(&conn, job_id)?,
            ("done".to_owned(), 1, None),
            "applied job should be marked done"
        );
        let post = get_post(&conn, post_id)?.context("media post should exist")?;
        assert_eq!(
            post.file_path.as_deref(),
            Some("applied/video.webm"),
            "transcoded path should remain installed"
        );
        assert_eq!(
            post.media_processing_state, None,
            "completed media state should be cleared"
        );
        assert!(
            claim_next_job(&conn)?.is_none(),
            "already-applied transcode must not be replayed"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_resolves_stale_media_without_clearing_newer_state() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "newer", "newer/old.mp4")?;
        conn.execute(
            "UPDATE posts
             SET file_path = 'newer/replacement.mp4',
                 media_processing_state = ?2
             WHERE id = ?1",
            rusqlite::params![post_id, MEDIA_PROCESSING_PENDING],
        )?;
        let payload = format!(
            r#"{{"t":"VideoTranscode","d":{{"post_id":{post_id},"file_path":"newer/old.mp4","board_short":"newer"}}}}"#
        );
        let job_id = insert_background_job(
            &conn,
            "video_transcode",
            &payload,
            "running",
            1,
            Some("worker interrupted"),
        )?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(recovery.jobs_reset, 0, "stale job should not be requeued");
        assert_eq!(recovery.jobs_resolved, 1, "stale job should be resolved");
        let post = get_post(&conn, post_id)?.context("media post should exist")?;
        assert_eq!(
            post.file_path.as_deref(),
            Some("newer/replacement.mp4"),
            "newer media path should be preserved"
        );
        assert_eq!(
            post.media_processing_state.as_deref(),
            Some(MEDIA_PROCESSING_PENDING),
            "newer media-processing state should be preserved"
        );
        assert_eq!(
            background_job_status(&conn, job_id)?.0,
            "done",
            "stale job should be marked done"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn startup_recovery_resolves_applied_waveform_without_replay() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "audio", "audio/track.mp3")?;
        conn.execute(
            "UPDATE posts
             SET thumb_path = 'audio/thumbs/track.png',
                 media_processing_state = 'running'
             WHERE id = ?1",
            rusqlite::params![post_id],
        )?;
        let payload = format!(
            r#"{{"t":"AudioWaveform","d":{{"post_id":{post_id},"file_path":"audio/track.mp3","board_short":"audio"}}}}"#
        );
        let job_id = insert_background_job(
            &conn,
            "audio_waveform",
            &payload,
            "running",
            1,
            Some("worker interrupted"),
        )?;

        let recovery = recover_interrupted_background_jobs(&conn)?;

        assert_eq!(
            recovery.jobs_reset, 0,
            "applied waveform should not be requeued"
        );
        assert_eq!(
            recovery.jobs_resolved, 1,
            "applied waveform should be resolved"
        );
        assert_eq!(
            background_job_status(&conn, job_id)?.0,
            "done",
            "applied waveform job should be marked done"
        );
        let post = get_post(&conn, post_id)?.context("audio post should exist")?;
        assert_eq!(
            post.thumb_path.as_deref(),
            Some("audio/thumbs/track.png"),
            "waveform thumbnail should remain installed"
        );
        assert_eq!(
            post.media_processing_state, None,
            "completed waveform state should be cleared"
        );
        assert!(
            claim_next_job(&conn)?.is_none(),
            "already-applied waveform must not be replayed"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn recovered_background_job_can_be_claimed_by_worker() -> Result<()> {
        let conn = test_conn()?;
        let payload = r#"{"t":"SpamCheck","d":{"post_id":42,"ip_hash":"hash","body_len":5}}"#;
        let job_id = insert_background_job(&conn, "spam_check", payload, "running", 1, None)?;

        recover_interrupted_background_jobs(&conn)?;
        let claimed = claim_next_job(&conn)?.context("recovered job should be claimable")?;

        assert_eq!(
            claimed,
            (job_id, payload.to_owned()),
            "worker should claim the recovered job"
        );
        assert_eq!(
            background_job_status(&conn, job_id)?.0,
            "running",
            "claim should return the job to running state"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn update_post_thumb_path_requires_matching_post_and_file_path() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "thumbz", "thumbz/audio.mp3")?;

        let error = update_post_thumb_path(
            &conn,
            post_id,
            "thumbz/deleted-or-replaced.mp3",
            "thumbz/thumbs/audio.png",
        )
        .err()
        .context("stale thumbnail update should be rejected")?;

        assert!(
            is_stale_media_target_error(&error),
            "rejection should use the typed stale-target error"
        );
        let post = get_post(&conn, post_id)?.context("media post should exist")?;
        assert!(
            post.thumb_path.is_none(),
            "stale thumbnail update must not mutate the post"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn replace_transcoded_media_requires_matching_post_and_file_path() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_media_post(&conn, "trans", "trans/video.mp4")?;

        let error = replace_transcoded_media(
            &conn,
            post_id,
            "trans/deleted-or-replaced.mp4",
            "trans/video.webm",
            "video/webm",
            "deadbeef",
        )
        .err()
        .context("stale transcode update should be rejected")?;

        assert!(
            is_stale_media_target_error(&error),
            "rejection should use the typed stale-target error"
        );
        let post = get_post(&conn, post_id)?.context("media post should exist")?;
        assert_eq!(
            post.file_path.as_deref(),
            Some("trans/video.mp4"),
            "stale transcode must not replace the source path"
        );
        let hash_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE file_path = ?1",
            rusqlite::params!["trans/video.webm"],
            |row| row.get(0),
        )?;
        assert_eq!(
            hash_count, 0,
            "stale transcode must not insert a deduplication row"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_post_returns_not_found_on_retry() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "delp", "Del Post", "", false)?;
        let op = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "body".to_owned(),
            body_html: "body".to_owned(),
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
        let (thread_id, _post_id, _) =
            create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)?;
        assert_eq!(thread_id, 1, "first seeded thread should receive ID 1");

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
        let reply_id = create_reply_with_thread_update(&conn, &reply, "", false, None)?;

        let deleted = super::delete_post(&conn, reply_id)?;
        assert!(
            deleted.paths.is_empty(),
            "post without media should produce no cleanup paths"
        );
        let retry = super::delete_post(&conn, reply_id);
        assert!(
            matches!(retry, Err(AppError::NotFound(message)) if message.contains("Post id")),
            "a repeated post deletion should return not found"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_post_decrements_reply_count_for_the_thread() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "count", "Count", "", false)?;
        let op = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "body".to_owned(),
            body_html: "body".to_owned(),
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
        let (thread_id, _post_id, _) =
            create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)?;

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
        let reply_id = create_reply_with_thread_update(&conn, &reply, "", false, None)?;

        let before_count: i64 = conn.query_row(
            "SELECT reply_count FROM threads WHERE id = ?1",
            rusqlite::params![thread_id],
            |row| row.get(0),
        )?;
        assert_eq!(
            before_count, 1,
            "reply creation should increment the thread count"
        );

        super::delete_post(&conn, reply_id)?;

        let after_count: i64 = conn.query_row(
            "SELECT reply_count FROM threads WHERE id = ?1",
            rusqlite::params![thread_id],
            |row| row.get(0),
        )?;
        assert_eq!(
            after_count, 0,
            "reply deletion should decrement the thread count"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn self_delete_post_deletes_reply_with_matching_token_inside_window() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "selfdel", "Self Delete", "", false)?;
        let op = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "body".to_owned(),
            body_html: "body".to_owned(),
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
            deletion_token: "op-token".to_owned(),
            is_op: true,
        };
        let (thread_id, _post_id, _) =
            create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)?;

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
            deletion_token: "reply-token".to_owned(),
            is_op: false,
        };
        let reply_id = create_reply_with_thread_update(&conn, &reply, "", false, None)?;

        let (outcome, deleted) = self_delete_post(&conn, reply_id, "reply-token", 60)?;

        assert_eq!(
            outcome,
            SelfDeleteOutcome::DeletedReply,
            "matching token should delete the reply"
        );
        assert!(
            deleted.is_some(),
            "successful deletion should return cleanup information"
        );
        assert!(
            get_post(&conn, reply_id)?.is_none(),
            "deleted reply should no longer exist"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn self_delete_post_rejects_wrong_token() -> Result<()> {
        let conn = test_conn()?;
        let post_id = seed_search_post(&conn, "wrongtok", "hello")?;

        let (outcome, deleted) = self_delete_post(&conn, post_id, "nope", 60)?;

        assert_eq!(
            outcome,
            SelfDeleteOutcome::WrongToken,
            "wrong token should be rejected"
        );
        assert!(
            deleted.is_none(),
            "rejected deletion should have no cleanup information"
        );
        assert!(
            get_post(&conn, post_id)?.is_some(),
            "rejected deletion should preserve the post"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn self_delete_post_refuses_op_when_thread_has_replies() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "selfop", "Self OP", "", false)?;
        let op = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "body".to_owned(),
            body_html: "body".to_owned(),
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
            deletion_token: "op-token".to_owned(),
            is_op: true,
        };
        let (thread_id, op_id, _) =
            create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)?;

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
            deletion_token: "reply-token".to_owned(),
            is_op: false,
        };
        create_reply_with_thread_update(&conn, &reply, "", false, None)?;

        let (outcome, deleted) = self_delete_post(&conn, op_id, "op-token", 60)?;

        assert_eq!(
            outcome,
            SelfDeleteOutcome::ThreadHasReplies,
            "opening post with replies should not self-delete"
        );
        assert!(
            deleted.is_none(),
            "rejected opening-post deletion should have no cleanup information"
        );
        assert!(
            get_thread(&conn, thread_id)?.is_some(),
            "rejected deletion should preserve the thread"
        );
        Ok(())
    }
}
