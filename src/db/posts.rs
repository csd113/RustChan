use crate::models::Post;
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use std::collections::HashMap;
use std::fmt;

/// Shared retry budget for job claiming and terminal-failure transitions.
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
/// Result of transactionally persisting one board-level prune evaluation.
pub struct ThreadPruneSchedule {
    /// Durable background-job identifier representing the pending evaluation.
    pub job_id: i64,
    /// Whether this call inserted a new pending row instead of coalescing.
    pub inserted: bool,
    /// Number of duplicate pending legacy rows resolved by this call.
    pub duplicates_resolved: usize,
    /// Number of duplicate running legacy rows observed for operator diagnostics.
    pub duplicate_running: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result of atomically scheduling media work for a post.
pub struct MediaJobSchedule {
    /// Durable background-job identifier representing the work.
    pub job_id: i64,
    /// Whether this call inserted a new row instead of coalescing an active job.
    pub inserted: bool,
    /// Legacy pending rows resolved while coalescing this schedule.
    pub pending_jobs_resolved: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Result of atomically resolving a successful media job.
pub struct MediaJobFinish {
    /// Whether the output was attached to the post.
    pub applied: bool,
    /// Durable, post-commit cleanup created by the transaction.
    pub cleanup: super::DeletePathsResult,
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

// Row mapper
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
///                                           24 `media_processing_error`.
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

// Post queries
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

/// Load at most 200 replies and the opening post, with a cursor for older replies.
/// Numeric cursors use the thread/id index, without deep OFFSET scans.
///
/// # Errors
/// Returns an error if the database query fails.
pub fn get_thread_post_window(
    conn: &rusqlite::Connection,
    thread_id: i64,
    before: Option<i64>,
) -> Result<(Vec<Post>, Option<i64>)> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {POST_SELECT_COLUMNS} FROM posts
         WHERE thread_id = ?1 AND is_op = 0 AND id <= ?2
         ORDER BY id DESC LIMIT 201"
    ))?;
    let mut posts = stmt
        .query_map(
            params![
                thread_id,
                before.map_or(i64::MAX, |id| id.saturating_sub(1))
            ],
            map_post,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_older = posts.len() > 200;
    posts.truncate(200);
    let older_before = has_older
        .then(|| posts.last().map(|post| post.id))
        .flatten();
    let op = conn
        .prepare_cached(&format!(
            "SELECT {POST_SELECT_COLUMNS} FROM posts WHERE thread_id = ?1 AND is_op = 1 LIMIT 1"
        ))?
        .query_row(params![thread_id], map_post)
        .optional()?;
    if let Some(op) = op {
        posts.push(op);
    }
    posts.reverse();
    Ok((posts, older_before))
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

    let placeholders = std::iter::repeat_n("?", post_ids.len())
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

    let placeholders = std::iter::repeat_n("?", thread_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
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
         WHERE preview_rank <= ?
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
             FROM (
                 SELECT * FROM post_submissions
                 UNION ALL SELECT * FROM post_submission_tombstones
             )
             WHERE submission_token = ?1
               AND ip_hash = ?2
               AND board_id = ?3
             LIMIT 1",
            params![submission_token, ip_hash, board_id],
            |row| {
                Ok(PostSubmissionRecord {
                    thread_id: row.get(0)?,
                    post_id: row.get(1)?,
                    is_thread: row.get::<_, i32>(2)? != 0_i32,
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

    anyhow::ensure!(
        !conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM post_submission_tombstones WHERE submission_token = ?1)",
            [submission_token],
            |row| row.get::<_, bool>(0),
        )?,
        "submission token was already accepted and deleted"
    );
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
    .context("Failed to record unique post submission token")
    .map(|_affected_rows| ())?;

    conn.execute(
        "DELETE FROM post_submissions WHERE created_at < unixepoch() - 604800",
        [],
    )
    .context("Failed to prune expired post submission tokens")
    .map(|_affected_rows| ())?;

    conn.execute(
        "DELETE FROM post_submission_tombstones WHERE created_at < unixepoch() - 604800",
        [],
    )
    .context("Failed to prune expired deleted submission receipts")
    .map(|_affected_rows| ())?;
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
            .context("Failed to insert poll option")
            .map(|_affected_rows| ())?;
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
/// Selection, deletion, and `paths_safe_to_delete` share a transaction so a
/// concurrent delete cannot invalidate the returned cleanup paths.
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
            super::commit_transaction(conn, "Failed to commit delete_post transaction")?;
            Ok(safe)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

/// Delete a non-opening post inside the caller's transaction.
pub(crate) fn delete_post_reply_in_tx(
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
                    r.get::<_, i32>(1)? != 0_i32,
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
            |row| {
                Ok((
                    row.get::<_, i32>(0)? != 0_i32,
                    row.get::<_, i32>(1)? != 0_i32,
                ))
            },
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
    self_delete_post_with_validation(conn, post_id, token, delete_window_secs, |_| Ok(()))
}

/// Recheck mutable request policy under the same write lock as deletion.
pub(crate) fn self_delete_post_with_validation(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
    delete_window_secs: i64,
    validate: impl FnOnce(&rusqlite::Connection) -> crate::error::Result<()>,
) -> crate::error::Result<(SelfDeleteOutcome, Option<crate::db::DeletePathsResult>)> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin self_delete_post transaction")?;

    let result: crate::error::Result<(SelfDeleteOutcome, Option<crate::db::DeletePathsResult>)> =
        (|| {
            validate(conn)?;
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
                            r.get::<_, i32>(1)? != 0_i32,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i32>(4)? != 0_i32,
                            r.get::<_, i32>(5)? != 0_i32,
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
                let has_replies: bool = conn.query_row(
                    "SELECT EXISTS (
                         SELECT 1 FROM posts
                         WHERE thread_id = ?1 AND is_op = 0
                     )",
                    params![thread_id],
                    |r| r.get(0),
                )?;
                if has_replies {
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
            super::commit_transaction(conn, "Failed to commit self_delete_post transaction")?;
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
/// An immediate transaction acquires the write lock before validation, and one
/// query reads both the token and timestamp so deletion cannot race either check.
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
    edit_post_with_validation(
        conn,
        post_id,
        token,
        new_body,
        new_body_html,
        edit_window_secs,
        |_| Ok(()),
    )
}

/// Recheck mutable request policy under the same write lock as editing.
pub(crate) fn edit_post_with_validation(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
    new_body: &str,
    new_body_html: &str,
    edit_window_secs: i64,
    validate: impl FnOnce(&rusqlite::Connection) -> crate::error::Result<()>,
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
        validate(conn)?;
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
                        r.get::<_, i32>(2)? != 0_i32,
                        r.get::<_, i32>(3)? != 0_i32,
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
        )
        .map(|_affected_rows| ())?;

        // Belt-and-suspenders: confirm the row was actually written.
        Ok(conn.changes() > 0)
    })();

    match result {
        Ok(updated) => {
            super::commit_transaction(conn, "Failed to commit edit_post transaction")?;
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
/// Length mismatch is folded into the accumulator while every byte from the
/// longer slice is processed.
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

// LIKE escape helper
/// Extract conservative FTS-safe tokens from free-form user input.
///
/// `SQLite` FTS5 treats punctuation-heavy input as query syntax, so raw tokens like
/// `'`, `"`, or `>>1` can raise syntax errors when passed through directly.
/// Normalizing to alphanumeric search terms preserves ordinary text search while
/// degrading punctuation-only input into a harmless "no results" query.
fn search_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();

    for ch in query.chars().take(crate::models::SEARCH_QUERY_MAX_CHARS) {
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

// Search
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
    let _timing = super::diagnostics::QueryTiming::start("search_posts");
    let limit = limit.clamp(1, 100);
    let offset = offset.clamp(0, 9_999);
    let Some(fts_query) = to_fts_query(query) else {
        return Ok(Vec::new());
    };
    // Prefix MATCH queries drive from FTS so each matching row is looked up
    // once, instead of running an FTS prefix probe for every board post.
    let sql = "SELECT posts.id, posts.thread_id, posts.board_id, posts.name, posts.tripcode,
                posts.subject, posts.body, posts.body_html, posts.ip_hash,
                posts.file_path, posts.file_name, posts.file_size, posts.thumb_path,
                posts.mime_type, posts.created_at, posts.deletion_token, posts.is_op,
                posts.media_type, posts.audio_file_path, posts.audio_file_name,
                posts.audio_file_size, posts.audio_mime_type, posts.edited_at,
                posts.media_processing_state, posts.media_processing_error
         FROM posts_fts CROSS JOIN posts ON posts.id = posts_fts.rowid
         WHERE posts.board_id = ?1 AND posts_fts MATCH ?2
         ORDER BY posts.created_at DESC, posts.id DESC
         LIMIT ?3 OFFSET ?4";
    super::search_budget::with_search_budget(conn, || {
        let mut stmt = conn.prepare_cached(sql)?;
        let posts = stmt
            .query_map(params![board_id, fts_query, limit, offset], map_post)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(posts)
    })
    .map_err(anyhow::Error::from)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn count_search_results(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
) -> Result<i64> {
    let _timing = super::diagnostics::QueryTiming::start("count_search_results");
    let Some(fts_query) = to_fts_query(query) else {
        return Ok(0);
    };
    super::search_budget::with_search_budget(conn, || {
        Ok(conn
            .prepare_cached(
                "SELECT COUNT(*) FROM (
                 SELECT 1 FROM posts_fts CROSS JOIN posts ON posts.id = posts_fts.rowid
                 WHERE posts.board_id = ?1 AND posts_fts MATCH ?2 LIMIT 10000
             )",
            )?
            .query_row(params![board_id, fts_query], |r| r.get(0))?)
    })
    .map_err(anyhow::Error::from)
}

// File deduplication
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
    let cached = stmt
        .query_row(params![sha256], |r| {
            Ok(super::CachedFile {
                file_path: r.get(0)?,
                thumb_path: r.get(1)?,
                mime_type: r.get(2)?,
            })
        })
        .optional()?;
    let Some(cached) = cached else {
        return Ok(None);
    };
    let posts_table_exists = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'posts'
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !posts_table_exists {
        return Ok(Some(cached));
    }
    let prune_pending = conn
        .query_row(
            "SELECT 1 FROM posts
             WHERE (file_path = ?1 OR audio_file_path = ?1)
               AND media_processing_state = ?2
             LIMIT 1",
            params![cached.file_path, MEDIA_ORIGINAL_PRUNE_PENDING],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    Ok((!prune_pending).then_some(cached))
}

/// Record a newly saved upload in the deduplication table.
///
/// `INSERT OR REPLACE` refreshes stale metadata when the same content hash is
/// re-uploaded after conversion policy changes.
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
    )
    .map(|_affected_rows| ())?;
    Ok(())
}

// Poll queries
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
                (
                    SELECT COUNT(*)
                    FROM poll_votes AS pv
                    WHERE pv.option_id = po.id AND pv.poll_id = po.poll_id
                ) AS vote_count
         FROM poll_options po
         WHERE po.poll_id = ?1
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
/// Validates that `option_id` belongs to an unexpired `poll_id` inside the same
/// INSERT statement. A mismatched pair or a poll that closes before this write
/// inserts nothing and returns false.
///
/// This returns false for two distinct cases:
///   1. The voter has already voted (UNIQUE constraint fires INSERT OR IGNORE)
///   2. The option does not belong to the poll, or the poll has expired/archived
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
         FROM poll_options AS po
         JOIN polls AS p ON p.id = po.poll_id
         JOIN threads AS t ON t.id = p.thread_id
         WHERE t.archived = 0
           AND po.id = ?2
           AND po.poll_id = ?1
           AND p.expires_at > unixepoch()",
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

// Poll maintenance
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

// Background job queue
// Jobs flow through: pending → running → done | failed
// claim_next_job uses UPDATE … RETURNING for atomic claim with no TOCTOU race.

/// Site-setting key containing the highest acknowledged failed-job identifier.
const FAILED_BACKGROUND_JOBS_ACK_ID_KEY: &str = "failed_background_jobs_acknowledged_through_id";

/// Persist a new job in the pending state. Returns the new row id.
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

/// Atomically validate a post, transition it to `pending`, and persist its
/// durable media job.
///
/// The serialized payload's source path is the generation guard: completion is
/// legal only while the post still references that path.
///
/// Existing active jobs for the same post and source are coalesced. Active jobs
/// for an older source are resolved as stale before the new row is inserted.
///
/// # Errors
/// Returns an error if the post is missing, belongs to another board, no longer
/// references `expected_source`, is in an original-prune state, or any part of
/// the immediate transaction fails.
#[expect(
    clippy::too_many_lines,
    reason = "validation, duplicate coalescing, state transition, insertion, and rollback form one auditable transaction"
)]
pub fn persist_media_job(
    conn: &rusqlite::Connection,
    job_type: &str,
    payload: &str,
    post_id: i64,
    expected_source: &str,
    board_short: &str,
) -> Result<MediaJobSchedule> {
    anyhow::ensure!(
        matches!(job_type, "video_transcode" | "audio_waveform"),
        "unsupported media job type"
    );
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin atomic media-job creation")?;

    let result = (|| {
        let post = conn
            .query_row(
                "SELECT p.file_path, p.media_processing_state, b.short_name
                 FROM posts p
                 JOIN boards b ON b.id = p.board_id
                 WHERE p.id = ?1",
                params![post_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("media-job target post {post_id} no longer exists"))?;
        anyhow::ensure!(
            post.0.as_deref() == Some(expected_source),
            "media-job target source changed before scheduling"
        );
        anyhow::ensure!(
            post.2 == board_short,
            "media-job target board does not match its payload"
        );
        anyhow::ensure!(
            !matches!(
                post.1.as_str(),
                MEDIA_ORIGINAL_PRUNE_PENDING | MEDIA_ORIGINAL_PRUNED
            ),
            "media-job target is incompatible with original pruning"
        );

        let mut stmt = conn.prepare_cached(
            "SELECT id, job_type, payload, status
             FROM background_jobs
             WHERE status IN ('pending', 'running')
               AND job_type IN ('video_transcode', 'audio_waveform')
               AND json_valid(payload)
               AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
             ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END, id ASC",
        )?;
        let active = stmt
            .query_map(params![post_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let mut matching_id = None;
        let mut pending_jobs_resolved = 0_usize;
        for (id, active_type, active_payload, status) in active {
            let parsed = serde_json::from_str::<serde_json::Value>(&active_payload);
            let active_source = match parsed {
                Ok(value) => value
                    .pointer("/d/file_path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                Err(error) => {
                    // Leave an unreadable job pending instead of resolving it as
                    // a stale duplicate, so queued work is never discarded.
                    tracing::warn!(
                        target: "db",
                        job_id = id,
                        %error,
                        "unreadable active media-job payload skipped during media scheduling"
                    );
                    continue;
                }
            };
            if matching_id.is_none()
                && active_type == job_type
                && active_source.as_deref() == Some(expected_source)
            {
                matching_id = Some(id);
            } else {
                conn.execute(
                    "UPDATE background_jobs
                     SET status = 'done',
                         last_error = 'resolved stale duplicate during media scheduling',
                         updated_at = unixepoch()
                     WHERE id = ?1 AND status IN ('pending', 'running')",
                    params![id],
                )
                .map(|_affected_rows| ())?;
                if status == "pending" {
                    pending_jobs_resolved = pending_jobs_resolved.saturating_add(1);
                }
            }
        }

        if let Some(job_id) = matching_id {
            // Never overwrite a terminal post state merely because a legacy job
            // was still active. Pending/running markers may be refreshed safely.
            if matches!(
                post.1.as_str(),
                MEDIA_PROCESSING_PENDING | "running" | MEDIA_PROCESSING_FAILED
            ) {
                conn.execute(
                    "UPDATE posts
                     SET media_processing_state = ?1, media_processing_error = NULL
                     WHERE id = ?2 AND file_path = ?3
                       AND media_processing_state IN (?1, 'running', ?4)",
                    params![
                        MEDIA_PROCESSING_PENDING,
                        post_id,
                        expected_source,
                        MEDIA_PROCESSING_FAILED
                    ],
                )
                .map(|_affected_rows| ())?;
            }
            return Ok(MediaJobSchedule {
                job_id,
                inserted: false,
                pending_jobs_resolved,
            });
        }

        let updated = conn.execute(
            "UPDATE posts
             SET media_processing_state = ?1, media_processing_error = NULL
             WHERE id = ?2 AND file_path = ?3
               AND media_processing_state IN ('', ?4)",
            params![
                MEDIA_PROCESSING_PENDING,
                post_id,
                expected_source,
                MEDIA_PROCESSING_FAILED
            ],
        )?;
        anyhow::ensure!(
            updated == 1,
            "illegal media-processing transition to pending"
        );
        let job_id = enqueue_job(conn, job_type, payload)?;
        Ok(MediaJobSchedule {
            job_id,
            inserted: true,
            pending_jobs_resolved,
        })
    })();

    match result {
        Ok(schedule) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit atomic media-job creation");
            }
            Ok(schedule)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Record an explicit capacity failure without creating a pending marker.
/// A pre-existing durable job wins over the rejection and is left untouched.
///
/// # Errors
/// Returns an error if the guarded transaction cannot be completed.
pub fn reject_media_job_at_capacity(
    conn: &rusqlite::Connection,
    post_id: i64,
    expected_source: &str,
    detail: &str,
) -> Result<Option<i64>> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin media capacity-rejection transaction")?;
    let result = (|| {
        let active = conn
            .query_row(
                "SELECT id FROM background_jobs
                 WHERE status IN ('pending', 'running')
                   AND job_type IN ('video_transcode', 'audio_waveform')
                   AND json_valid(payload)
                   AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
                   AND json_extract(payload, '$.d.file_path') = ?2
                 ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END, id ASC
                 LIMIT 1",
                params![post_id, expected_source],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if active.is_some() {
            return Ok(active);
        }
        let error: String = detail.trim().chars().take(512).collect();
        conn.execute(
            "UPDATE posts
             SET media_processing_state = ?1, media_processing_error = ?2
             WHERE id = ?3 AND file_path = ?4
               AND media_processing_state IN ('', ?5)",
            params![
                MEDIA_PROCESSING_FAILED,
                error,
                post_id,
                expected_source,
                MEDIA_PROCESSING_FAILED
            ],
        )
        .map(|_affected_rows| ())?;
        Ok(None)
    })();
    match result {
        Ok(active) => {
            super::commit_transaction(conn, "Failed to commit media capacity rejection")?;
            Ok(active)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Return the canonical policy-free payload for a board prune evaluation.
fn thread_prune_payload(board_id: i64) -> Result<String> {
    serde_json::to_string(&serde_json::json!({
        "t": "ThreadPrune",
        "d": { "board_id": board_id }
    }))
    .context("Failed to serialize board prune intent")
}

/// Persist or coalesce one board-level thread-prune intent inside the caller's
/// write transaction.
///
/// A board can have one running evaluation plus one pending dirty evaluation.
/// `SQLite`'s write transaction serializes concurrent producers, so the
/// read/coalesce/insert decision cannot race another producer.
///
/// # Errors
/// Returns an error if the payload cannot be serialized or the durable job
/// cannot be queried, updated, or inserted.
pub(crate) fn persist_thread_prune_intent_in_tx(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<ThreadPruneSchedule> {
    let payload = thread_prune_payload(board_id)?;
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, status
             FROM background_jobs
             WHERE job_type = 'thread_prune'
               AND status IN ('pending', 'running')
               AND CAST(json_extract(
                     CASE WHEN json_valid(payload) THEN payload ELSE NULL END,
                     '$.d.board_id'
                   ) AS INTEGER) = ?1
             ORDER BY CASE status WHEN 'running' THEN 0 ELSE 1 END, id ASC",
        )
        .context("Prepare active board prune intent lookup failed")?;
    let active = stmt
        .query_map(params![board_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .context("Query active board prune intents failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read active board prune intents failed")?;
    drop(stmt);

    let running_count = active
        .iter()
        .filter(|(_, status)| status == "running")
        .count();
    let pending_ids = active
        .iter()
        .filter_map(|(id, status)| (status == "pending").then_some(*id))
        .collect::<Vec<_>>();

    if let Some((&job_id, duplicate_ids)) = pending_ids.split_first() {
        conn.execute(
            "UPDATE background_jobs
             SET payload = ?2, priority = 10, attempts = 0, last_error = NULL,
                 updated_at = unixepoch()
             WHERE id = ?1 AND status = 'pending'",
            params![job_id, payload],
        )
        .context("Refresh coalesced board prune intent failed")
        .map(|_affected_rows| ())?;

        for duplicate_id in duplicate_ids {
            conn.execute(
                "UPDATE background_jobs
                 SET status = 'done',
                     last_error = 'coalesced duplicate legacy thread-prune intent',
                     updated_at = unixepoch()
                 WHERE id = ?1 AND status = 'pending'",
                params![duplicate_id],
            )
            .context("Resolve duplicate legacy board prune intent failed")
            .map(|_affected_rows| ())?;
        }

        return Ok(ThreadPruneSchedule {
            job_id,
            inserted: false,
            duplicates_resolved: duplicate_ids.len(),
            duplicate_running: running_count.saturating_sub(1),
        });
    }

    let job_id: i64 = conn
        .query_row(
            "INSERT INTO background_jobs
                 (job_type, payload, status, priority, updated_at)
             VALUES ('thread_prune', ?1, 'pending', 10, unixepoch())
             RETURNING id",
            params![payload],
            |row| row.get(0),
        )
        .context("Persist board prune intent failed")?;
    Ok(ThreadPruneSchedule {
        job_id,
        inserted: true,
        duplicates_resolved: 0,
        duplicate_running: running_count.saturating_sub(1),
    })
}

/// Persist or coalesce one board-level prune intent atomically.
///
/// # Errors
/// Returns an error if the immediate transaction or intent persistence fails.
pub fn persist_thread_prune_intent(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<ThreadPruneSchedule> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin board prune intent transaction")?;
    match persist_thread_prune_intent_in_tx(conn, board_id) {
        Ok(schedule) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit board prune intent");
            }
            Ok(schedule)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
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
             SELECT candidate.id FROM background_jobs AS candidate
             WHERE candidate.status = 'pending' AND candidate.attempts < ?1
               AND (candidate.job_type NOT IN ('video_transcode', 'audio_waveform')
                    OR NOT EXISTS (
                        SELECT 1 FROM pending_fs_ops fs,
                            json_each(CASE WHEN json_valid(fs.payload_json)
                                           THEN fs.payload_json ELSE '{}' END, '$.relative_paths') artifact
                        WHERE fs.kind = 'upload_finalize'
                          AND artifact.value = json_extract(
                              CASE WHEN json_valid(candidate.payload)
                                   THEN candidate.payload ELSE '{}' END, '$.d.file_path')
                    ))
               AND (
                   candidate.job_type != 'thread_prune'
                   OR NOT EXISTS (
                       SELECT 1
                       FROM background_jobs AS active
                       WHERE active.job_type = 'thread_prune'
                         AND active.status = 'running'
                         AND CAST(json_extract(
                               CASE WHEN json_valid(active.payload)
                                    THEN active.payload ELSE NULL END,
                               '$.d.board_id'
                             ) AS INTEGER)
                             = CAST(json_extract(
                                   CASE WHEN json_valid(candidate.payload)
                                        THEN candidate.payload ELSE NULL END,
                                   '$.d.board_id'
                               ) AS INTEGER)
                   )
               )
             ORDER BY candidate.priority DESC, candidate.created_at ASC
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

/// Mark a running job as successfully completed.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn complete_job(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE background_jobs
         SET status = 'done', last_error = NULL, updated_at = unixepoch()
         WHERE id = ?1 AND status = 'running'",
        params![id],
    )?;
    if n == 0 {
        anyhow::bail!("Job {id} not found or not in 'running' state");
    }
    Ok(())
}

/// Record a job failure. After `MAX_JOB_ATTEMPTS` the job stays failed permanently.
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
                             WHERE id = ?2
                               AND media_processing_state IN ('', ?1, 'running')",
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
                        conn.execute(
                            "UPDATE posts
                             SET media_processing_state = '', media_processing_error = NULL
                             WHERE id = ?1
                               AND media_processing_state IN (?2, 'running')",
                            params![post_id, MEDIA_PROCESSING_PENDING],
                        )
                        .map(|_affected_rows| ())?;
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
         WHERE status = 'running'
         ORDER BY id ASC
         LIMIT 256",
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
    let target_post_id = match &target {
        InterruptedMediaTarget::Video {
            post_id,
            source_path: _,
            expected_output_path: _,
        }
        | InterruptedMediaTarget::Audio {
            post_id,
            source_path: _,
            expected_thumb_path: _,
        } => *post_id,
    };
    let post = conn
        .query_row(
            "SELECT file_path, thumb_path, media_processing_state
             FROM posts
             WHERE id = ?1",
            rusqlite::params![target_post_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((current_path, current_thumb_path, processing_state)) = post else {
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
            if matches!(
                processing_state.as_str(),
                MEDIA_ORIGINAL_PRUNE_PENDING | MEDIA_ORIGINAL_PRUNED | MEDIA_PROCESSING_FAILED
            ) {
                return Ok(InterruptedJobDisposition::Resolve {
                    clear_media_post_id: None,
                });
            }
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
            if matches!(
                processing_state.as_str(),
                MEDIA_ORIGINAL_PRUNE_PENDING | MEDIA_ORIGINAL_PRUNED | MEDIA_PROCESSING_FAILED
            ) {
                return Ok(InterruptedJobDisposition::Resolve {
                    clear_media_post_id: None,
                });
            }
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

// Legal asynchronous media state machine:
//
//   normal ("") / failed
//       -> pending + durable job                 (one creation transaction)
//   pending
//       -> pending + durable retry               (one failure transaction)
//       -> normal + attached output + done job   (one completion transaction)
//       -> normal + safe original + done job     (one no-op transaction)
//       -> failed + terminal job                 (one failure transaction)
//   pending/running + missing or changed target
//       -> done stale job, without post mutation
//   prune_pending / pruned
//       -> never accepts media-job completion
//
// The job payload's post id, operation kind, board, and expected source path are
// its generation identity. No worker transition updates a post by id alone.
/// Media-processing state assigned while work is queued or running.
pub const MEDIA_PROCESSING_PENDING: &str = "pending";
/// Media-processing state assigned after terminal worker failure.
pub const MEDIA_PROCESSING_FAILED: &str = "failed";
/// Media-processing state assigned after a durable original-prune intent commits.
pub const MEDIA_ORIGINAL_PRUNE_PENDING: &str = "prune_pending";
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
    )
    .map(|_affected_rows| ())?;
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

// Post update helpers (used by background workers)
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

/// Verify that a running job row still carries the expected media identity.
fn media_job_matches(
    conn: &rusqlite::Connection,
    job_id: i64,
    job_type: &str,
    post_id: i64,
    expected_source: &str,
) -> Result<bool> {
    let row = conn
        .query_row(
            "SELECT job_type, payload, status FROM background_jobs WHERE id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_type, payload, status)) = row else {
        return Ok(false);
    };
    if stored_type != job_type || status != "running" {
        return Ok(false);
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return Ok(false);
    };
    Ok(payload
        .pointer("/d/post_id")
        .and_then(serde_json::Value::as_i64)
        == Some(post_id)
        && payload
            .pointer("/d/file_path")
            .and_then(serde_json::Value::as_str)
            == Some(expected_source))
}

/// Create durable cleanup for paths proven unreferenced in the current write
/// transaction.
fn journal_unreferenced_media_paths(
    conn: &rusqlite::Connection,
    candidates: Vec<String>,
) -> Result<super::DeletePathsResult> {
    let paths = super::paths_safe_to_delete(conn, candidates)?;
    let pending = super::build_delete_files_pending_op(&paths)?;
    if let Some(op) = pending.as_ref() {
        super::insert_pending_fs_op(conn, op)?;
    }
    Ok(super::DeletePathsResult {
        paths,
        pending_fs_op_id: pending.map(|op| op.id),
    })
}

/// Complete a media job that intentionally kept the safe original. This must
/// be called inside the worker's completion transaction.
///
/// # Errors
/// Returns an error if the running job identity or database mutation is invalid.
pub fn complete_media_job_without_output_in_tx(
    conn: &rusqlite::Connection,
    job_id: i64,
    job_type: &str,
    post_id: i64,
    expected_source: &str,
) -> Result<()> {
    anyhow::ensure!(
        media_job_matches(conn, job_id, job_type, post_id, expected_source)?,
        "media completion job identity is stale"
    );
    conn.execute(
        "UPDATE posts
         SET media_processing_state = '', media_processing_error = NULL
         WHERE id = ?1 AND file_path = ?2
           AND media_processing_state IN (?3, 'running')",
        params![post_id, expected_source, MEDIA_PROCESSING_PENDING],
    )
    .map(|_affected_rows| ())?;
    complete_job(conn, job_id)
}

/// Resolve a stale or deleted media target without mutating post state. This
/// must be called inside the worker's completion transaction.
///
/// # Errors
/// Returns an error if the job is no longer running or cannot be updated.
pub fn complete_stale_media_job_in_tx(conn: &rusqlite::Connection, job_id: i64) -> Result<()> {
    complete_job(conn, job_id)
}

/// Apply the bounded retry policy while preserving the post/job invariant.
/// Missing, changed, pruned, or otherwise terminal targets resolve as stale and
/// do not consume further queue capacity.
///
/// This must be called inside the worker's completion transaction.
///
/// # Errors
/// Returns an error if the job state cannot be inspected or updated.
pub fn fail_media_job_in_tx(
    conn: &rusqlite::Connection,
    job_id: i64,
    job_type: &str,
    post_id: i64,
    expected_source: &str,
    error: &str,
) -> Result<Option<JobFailureState>> {
    anyhow::ensure!(
        media_job_matches(conn, job_id, job_type, post_id, expected_source)?,
        "media failure job identity is stale"
    );
    let applicable = conn
        .query_row(
            "SELECT 1 FROM posts
             WHERE id = ?1 AND file_path = ?2
               AND media_processing_state IN (?3, 'running')",
            params![post_id, expected_source, MEDIA_PROCESSING_PENDING],
            |_row| Ok(()),
        )
        .optional()?
        .is_some();
    if !applicable {
        complete_job(conn, job_id)?;
        return Ok(None);
    }

    let failure_state = fail_job(conn, job_id, error)?;
    match failure_state {
        JobFailureState::Retrying => {
            conn.execute(
                "UPDATE posts
                 SET media_processing_state = ?1, media_processing_error = NULL
                 WHERE id = ?2 AND file_path = ?3
                   AND media_processing_state IN (?1, 'running')",
                params![MEDIA_PROCESSING_PENDING, post_id, expected_source],
            )
            .map(|_affected_rows| ())?;
        }
        JobFailureState::PermanentlyFailed => {
            let error: String = error.trim().chars().take(512).collect();
            conn.execute(
                "UPDATE posts
                 SET media_processing_state = ?1, media_processing_error = ?2
                 WHERE id = ?3 AND file_path = ?4
                   AND media_processing_state IN (?5, 'running')",
                params![
                    MEDIA_PROCESSING_FAILED,
                    error,
                    post_id,
                    expected_source,
                    MEDIA_PROCESSING_PENDING
                ],
            )
            .map(|_affected_rows| ())?;
        }
    }
    Ok(Some(failure_state))
}

/// Atomically attach a validated video output, update deduplication metadata,
/// journal unreferenced source cleanup, and mark the owning job done.
///
/// The caller must already hold an immediate transaction and must only perform
/// filesystem cleanup after it commits.
///
/// # Errors
/// Returns an error if the job identity, post update, hash update, cleanup
/// journal, or terminal job update fails.
#[expect(
    clippy::too_many_arguments,
    reason = "the atomic completion boundary explicitly carries job, source, and validated output identity"
)]
pub fn finish_video_media_job_in_tx(
    conn: &rusqlite::Connection,
    job_id: i64,
    post_id: i64,
    expected_source: &str,
    output_path: &str,
    output_mime: &str,
    output_sha256: &str,
    output_size: i64,
) -> Result<MediaJobFinish> {
    if !media_job_matches(conn, job_id, "video_transcode", post_id, expected_source)? {
        return Err(anyhow::Error::new(StaleMediaTargetError {
            post_id,
            expected_path: expected_source.to_owned(),
        }));
    }
    let thumb_path = conn
        .query_row(
            "SELECT thumb_path FROM posts
             WHERE id = ?1 AND file_path = ?2
               AND media_processing_state IN (?3, 'running')",
            params![post_id, expected_source, MEDIA_PROCESSING_PENDING],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let applicable = conn.query_row(
        "SELECT EXISTS(
                 SELECT 1 FROM posts
                 WHERE id = ?1 AND file_path = ?2
                   AND media_processing_state IN (?3, 'running')
             )",
        params![post_id, expected_source, MEDIA_PROCESSING_PENDING],
        |row| row.get::<_, bool>(0),
    )?;
    if !applicable {
        complete_job(conn, job_id)?;
        let cleanup = journal_unreferenced_media_paths(conn, vec![output_path.to_owned()])?;
        return Ok(MediaJobFinish {
            applied: false,
            cleanup,
        });
    }

    let updated = conn.execute(
        "UPDATE posts
         SET file_path = ?1, mime_type = ?2, file_size = ?3,
             media_processing_state = '', media_processing_error = NULL
         WHERE id = ?4 AND file_path = ?5
           AND media_processing_state IN (?6, 'running')",
        params![
            output_path,
            output_mime,
            output_size,
            post_id,
            expected_source,
            MEDIA_PROCESSING_PENDING
        ],
    )?;
    anyhow::ensure!(updated == 1, "video target changed during completion");
    let cleanup = journal_unreferenced_media_paths(conn, vec![expected_source.to_owned()])?;
    record_file_hash(
        conn,
        output_sha256,
        output_path,
        thumb_path.as_deref().unwrap_or(""),
        output_mime,
    )?;
    complete_job(conn, job_id)?;
    Ok(MediaJobFinish {
        applied: true,
        cleanup,
    })
}

/// Atomically attach a waveform output, update its hash metadata, journal a
/// superseded placeholder, and mark the job done.
///
/// The caller must already hold an immediate transaction.
///
/// # Errors
/// Returns an error if any guarded database mutation fails.
pub fn finish_waveform_media_job_in_tx(
    conn: &rusqlite::Connection,
    job_id: i64,
    post_id: i64,
    expected_source: &str,
    output_path: &str,
    source_sha256: &str,
) -> Result<MediaJobFinish> {
    if !media_job_matches(conn, job_id, "audio_waveform", post_id, expected_source)? {
        return Err(anyhow::Error::new(StaleMediaTargetError {
            post_id,
            expected_path: expected_source.to_owned(),
        }));
    }
    let previous_thumb = conn
        .query_row(
            "SELECT thumb_path FROM posts
             WHERE id = ?1 AND file_path = ?2
               AND media_processing_state IN (?3, 'running')",
            params![post_id, expected_source, MEDIA_PROCESSING_PENDING],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    let Some(previous_thumb) = previous_thumb else {
        complete_job(conn, job_id)?;
        let cleanup = journal_unreferenced_media_paths(conn, vec![output_path.to_owned()])?;
        return Ok(MediaJobFinish {
            applied: false,
            cleanup,
        });
    };
    let updated = conn.execute(
        "UPDATE posts
         SET thumb_path = ?1, media_processing_state = '', media_processing_error = NULL
         WHERE id = ?2 AND file_path = ?3
           AND media_processing_state IN (?4, 'running')",
        params![
            output_path,
            post_id,
            expected_source,
            MEDIA_PROCESSING_PENDING
        ],
    )?;
    anyhow::ensure!(updated == 1, "waveform target changed during completion");
    conn.execute(
        "UPDATE file_hashes SET thumb_path = ?1
         WHERE sha256 = ?2 AND file_path = ?3",
        params![output_path, source_sha256, expected_source],
    )
    .map(|_affected_rows| ())?;
    let cleanup = if previous_thumb
        .as_deref()
        .is_some_and(|path| path != output_path)
    {
        journal_unreferenced_media_paths(conn, previous_thumb.into_iter().collect())?
    } else {
        super::DeletePathsResult {
            paths: Vec::new(),
            pending_fs_op_id: None,
        }
    };
    complete_job(conn, job_id)?;
    Ok(MediaJobFinish {
        applied: true,
        cleanup,
    })
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
    new_size: i64,
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
            "UPDATE posts
             SET file_path = ?1, mime_type = ?2, file_size = ?3
             WHERE file_path = ?4",
            params![new_path, new_mime, new_size, old_path],
        )?;
        debug_assert!(updated > 0, "target_exists guarantees at least one update");
        set_post_media_processing_state(conn, post_id, None, None)?;

        let thumb_path = get_post_thumb_path(conn, post_id)?.unwrap_or_default();
        conn.execute(
            "DELETE FROM file_hashes WHERE file_path = ?1",
            params![old_path],
        )
        .map(|_affected_rows| ())?;
        record_file_hash(conn, new_sha256, new_path, &thumb_path, new_mime)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            super::commit_transaction(
                conn,
                "Failed to commit transcode media replacement transaction",
            )?;
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
    )
    .map(|_affected_rows| ())?;
    Ok(())
}
