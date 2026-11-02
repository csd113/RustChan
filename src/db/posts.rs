use crate::models::Post;
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use std::collections::HashMap;
use std::fmt;

pub(crate) use super::jobs::persist_thread_prune_intent_in_tx;
pub use super::jobs::{
    acknowledge_failed_background_jobs, background_job_summary, claim_next_job, complete_job,
    complete_media_job_without_output_in_tx, complete_stale_media_job_in_tx, enqueue_job, fail_job,
    fail_media_job_in_tx, finish_video_media_job_in_tx, finish_waveform_media_job_in_tx,
    pending_job_count, persist_media_job, persist_thread_prune_intent, recent_background_jobs,
    recover_interrupted_background_jobs, reject_media_job_at_capacity, BackgroundJobSummary,
    InterruptedJobRecovery, JobFailureState, MediaJobFinish, MediaJobSchedule, RecentBackgroundJob,
    ThreadPruneSchedule,
};

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

// Row mapper
/// Map a full post row (25 columns, selected in the canonical order used
/// throughout this module) into a Post struct.
///
/// Callers must keep their SELECT lists in this order. Dynamic `row.get` calls
/// check column indices and types at runtime, not at compile time.
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
