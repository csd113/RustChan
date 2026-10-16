use crate::models::Thread;
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

/// Authoritative thread-state rejection for a reply or federated import.
///
/// Callers match on this type instead of parsing error text so that a locked or
/// archived thread keeps mapping to its exact HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadClosed {
    /// The thread is locked against new replies.
    Locked,
    /// The thread has been archived.
    Archived,
}

impl ThreadClosed {
    /// Return the operator-facing message for this rejection.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Locked => "This thread is locked.",
            Self::Archived => "This thread is archived.",
        }
    }
}

impl std::fmt::Display for ThreadClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for ThreadClosed {}

#[derive(Debug)]
/// Optional poll values inserted alongside a new thread.
pub struct PollInsert<'a> {
    /// Poll question text.
    pub question: &'a str,
    /// Ordered poll answer choices.
    pub options: &'a [String],
    /// Unix expiration timestamp, or the caller's non-expiring sentinel.
    pub expires_at: i64,
}

/// Map a thread row. Column layout (must match every SELECT that calls this):
///   0  t.id           4  `t.bumped_at`    8  op.body       12 op.tripcode
///   1  `t.board_id`     5  t.locked       9  `op.file_path`  13 op.id (`op_id`)
///   2  t.subject      6  t.sticky       10 `op.thumb_path` 14 t.archived
///   3  `t.created_at`   7  `t.reply_count`  11 op.name       15 `image_count`
///   16 `last_post_at`.
fn map_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: row.get(0)?,
        board_id: row.get(1)?,
        subject: row.get(2)?,
        created_at: row.get(3)?,
        bumped_at: row.get(4)?,
        last_post_at: row.get(16)?,
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

// File-path collection helper
/// Collect all file paths (`file_path`, `thumb_path`, `audio_file_path`) for every
/// post in the given set of thread ids. Returns a flat Vec of non-null paths.
///
/// Uses bounded batches instead of one query per thread.
///
/// Call before deleting the thread rows; cascading deletion removes the posts
/// that supply these paths.
fn collect_thread_file_paths(
    conn: &rusqlite::Connection,
    thread_ids: &[i64],
) -> Result<Vec<String>> {
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut paths = Vec::new();
    for batch in thread_ids.chunks(500) {
        // Bound parameters so restored or backlogged boards cannot exceed SQLite limits.
        let placeholders: String = batch
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
            .query_map(rusqlite::params_from_iter(batch), |r| {
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

// Board-index thread listing
/// The canonical thread SELECT fragment shared by all listing queries.
///
/// A left-join aggregation computes image counts in one pass. Callers must
/// group by thread to preserve one output row per thread.
pub(super) const THREAD_SELECT: &str = "
    SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
           t.locked, t.sticky, t.reply_count,
           op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
           t.archived,
           COUNT(DISTINCT CASE WHEN fp.file_path IS NOT NULL THEN fp.id END) AS image_count,
           COALESCE(MAX(fp.created_at), t.created_at) AS last_post_at
    FROM threads t
    JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
    LEFT JOIN posts fp ON fp.thread_id = t.id";

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
    load_thread_page(conn, board_id, limit, offset, false)
}

/// Paginate threads before computing reply aggregates; exclude missing OPs first.
pub(super) fn thread_page_sql(archived: bool) -> String {
    thread_listing_sql(archived, false)
}

/// Build the shared page query, applying personal visibility before pagination.
fn thread_listing_sql(archived: bool, personalized: bool) -> String {
    // Preserve the legacy aggregate's tie order for each listing, including
    // deterministic boundaries when many replies bump in the same second.
    let base_order = if archived {
        "t.bumped_at DESC, t.id ASC"
    } else {
        "t.sticky DESC, t.bumped_at DESC, t.id DESC"
    };
    let order = if personalized && !archived {
        format!("t.viewer_pinned DESC, {base_order}")
    } else {
        base_order.to_owned()
    };
    let page_order = if personalized && !archived {
        format!("COALESCE(pref.pinned, 0) DESC, {base_order}")
    } else {
        base_order.to_owned()
    };
    let (columns, join, visibility) = if personalized {
        (
            ", COALESCE(pref.pinned, 0) AS viewer_pinned",
            "LEFT JOIN user_thread_preferences pref ON pref.thread_id = t.id AND pref.user_hash = ?5",
            "AND COALESCE(pref.hidden, 0) = ?6",
        )
    } else {
        ("", "", "")
    };
    format!(
        "WITH page AS MATERIALIZED (
             SELECT t.*{columns} FROM threads t {join}
             WHERE t.board_id = ?1 AND t.archived = ?4
               {visibility}
               AND EXISTS (SELECT 1 FROM posts op WHERE op.thread_id=t.id AND op.is_op=1)
             ORDER BY {page_order} LIMIT ?2 OFFSET ?3
         )
         SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts fp WHERE fp.thread_id=t.id AND fp.file_path IS NOT NULL),
                COALESCE((SELECT MAX(fp.created_at) FROM posts fp WHERE fp.thread_id=t.id), t.created_at)
         FROM page t JOIN posts op ON op.thread_id=t.id AND op.is_op=1
         ORDER BY {order}"
    )
}

/// List a visitor's visible or hidden active threads in canonical personal order.
///
/// Personal pins precede sticky threads, then bump time and descending thread ID.
/// Filtering and sorting occur before LIMIT/OFFSET, so pages never repeat pins.
///
/// # Errors
/// Returns an error if the indexed listing cannot be read.
pub fn get_threads_for_viewer(
    conn: &rusqlite::Connection,
    board_id: i64,
    user_hash: &str,
    hidden: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    let sql = thread_listing_sql(false, true);
    let mut stmt = conn.prepare_cached(&sql)?;
    let threads = stmt
        .query_map(
            params![board_id, limit, offset, 0_i32, user_hash, i32::from(hidden)],
            map_thread,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// Count active, renderable threads with the same visibility filter as listing.
///
/// # Errors
/// Returns an error if the count cannot be read.
pub fn count_threads_for_viewer(
    conn: &rusqlite::Connection,
    board_id: i64,
    user_hash: &str,
    hidden: bool,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads t
         LEFT JOIN user_thread_preferences pref ON pref.thread_id = t.id AND pref.user_hash = ?2
         WHERE t.board_id = ?1 AND t.archived = 0 AND COALESCE(pref.hidden, 0) = ?3
           AND EXISTS (SELECT 1 FROM posts op WHERE op.thread_id = t.id AND op.is_op = 1)",
        params![board_id, user_hash, i32::from(hidden)],
        |row| row.get(0),
    )?)
}

/// Execute an indexed page and its bounded per-thread aggregate probes.
fn load_thread_page(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
    archived: bool,
) -> Result<Vec<Thread>> {
    let _timing = super::diagnostics::QueryTiming::start("thread_page");
    let sql = thread_page_sql(archived);
    let mut stmt = conn.prepare_cached(&sql)?;
    let threads = stmt
        .query_map(
            params![board_id, limit, offset, i64::from(archived)],
            map_thread,
        )?
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

// Thread creation (atomic with OP post)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result of atomically deciding whether a public post submission is new.
pub(crate) enum PostCreationOutcome<T> {
    /// The transaction created and committed a new post bundle.
    Created(T, i64),
    /// The token already names a canonical committed post.
    Replayed(super::PostSubmissionRecord),
}

#[derive(Debug, Clone, Copy)]
/// Filesystem lifecycle context validated in a public post transaction.
pub(crate) struct PostFilesystemCommit<'a, F = fn(&rusqlite::Connection) -> Result<()>> {
    /// Durable publication operation for newly staged files.
    pending_fs_op: Option<&'a crate::pending_fs::PendingFsOpInsert>,
    /// Prior dedup cache hits that must still be valid under the write lock.
    deduplicated_paths: &'a [&'a str],
    /// Whether a new thread must atomically persist board-prune work.
    schedule_thread_prune: bool,
    /// Posting policy checked after token replay and while holding the write lock.
    validate: F,
    /// Board name for media work that must persist alongside the new post.
    media_job_board: Option<&'a str>,
}

impl<'a> PostFilesystemCommit<'a> {
    /// Build filesystem context for one post-creation transaction.
    #[must_use]
    pub(crate) const fn new(
        pending_fs_op: Option<&'a crate::pending_fs::PendingFsOpInsert>,
        deduplicated_paths: &'a [&'a str],
        schedule_thread_prune: bool,
    ) -> Self {
        Self::new_with_validation(
            pending_fs_op,
            deduplicated_paths,
            schedule_thread_prune,
            |_| Ok(()),
        )
    }
}

impl<'a, F> PostFilesystemCommit<'a, F> {
    /// Build creation context with request validation under the write lock.
    pub(crate) const fn new_with_validation(
        pending_fs_op: Option<&'a crate::pending_fs::PendingFsOpInsert>,
        deduplicated_paths: &'a [&'a str],
        schedule_thread_prune: bool,
        validate: F,
    ) -> Self {
        Self::new_with_media_validation(
            pending_fs_op,
            deduplicated_paths,
            schedule_thread_prune,
            None,
            validate,
        )
    }

    /// Include required media work in the atomic creation bundle.
    pub(crate) const fn new_with_media_validation(
        pending_fs_op: Option<&'a crate::pending_fs::PendingFsOpInsert>,
        deduplicated_paths: &'a [&'a str],
        schedule_thread_prune: bool,
        media_job_board: Option<&'a str>,
        validate: F,
    ) -> Self {
        Self {
            pending_fs_op,
            deduplicated_paths,
            schedule_thread_prune,
            validate,
            media_job_board,
        }
    }
}

/// Persist media intent before commit; a full queue leaves an explicit failure.
fn persist_new_post_metadata(
    conn: &rusqlite::Connection,
    post: &super::NewPost,
    post_id: i64,
    media_board: Option<&str>,
) -> Result<i64> {
    if let Some(board_short) = media_board {
        let source = post
            .file_path
            .as_ref()
            .context("pending media requires a source")?;
        let (job_type, job_tag) = match post.media_type.as_deref() {
            Some("video") => ("video_transcode", "VideoTranscode"),
            Some("audio") => ("audio_waveform", "AudioWaveform"),
            _ => anyhow::bail!("pending processing requires audio or video"),
        };
        let capacity = crate::config::CONFIG.job_queue_capacity;
        let full = capacity > 0 && u64::try_from(super::pending_job_count(conn)?)? >= capacity;
        if full {
            super::set_post_media_processing_state(
                conn,
                post_id,
                Some(super::MEDIA_PROCESSING_FAILED),
                Some("Background media queue is full; deferred processing was skipped."),
            )?;
        } else {
            let payload = serde_json::json!({
                "t": job_tag, "d": { "post_id": post_id, "file_path": source, "board_short": board_short }
            }).to_string();
            super::enqueue_job(conn, job_type, &payload).map(|_job_id| ())?;
            super::set_post_media_processing_state(
                conn,
                post_id,
                Some(super::MEDIA_PROCESSING_PENDING),
                None,
            )?;
        }
    }
    Ok(conn.query_row(
        "SELECT created_at FROM posts WHERE id = ?1",
        [post_id],
        |row| row.get(0),
    )?)
}

/// Reject reuse of a receipt for another operation or reply target.
pub(crate) fn validate_submission_target(
    record: super::PostSubmissionRecord,
    thread_id: Option<i64>,
) -> Result<()> {
    if record.is_thread != thread_id.is_none()
        || thread_id.is_some_and(|target| target != record.thread_id)
    {
        return Err(crate::error::AppError::Conflict(
            "This submission token belongs to a different posting form. Reload the page.".into(),
        )
        .into());
    }
    Ok(())
}

/// A token accepted for another actor/board must never create new content.
fn reject_conflicting_submission(conn: &rusqlite::Connection, token: &str) -> Result<()> {
    if !token.is_empty()
        && conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM post_submissions WHERE submission_token=?1)
            OR EXISTS(SELECT 1 FROM post_submission_tombstones WHERE submission_token=?1)",
            [token],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(crate::error::AppError::Conflict(
            "This submission token has already been used. Reload the posting form.".into(),
        )
        .into());
    }
    Ok(())
}

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
    match create_thread_submission(
        conn,
        board_id,
        subject,
        post,
        submission_token,
        poll,
        PostFilesystemCommit::new(pending_fs_op, &[], false),
    )? {
        PostCreationOutcome::Created(ids, _) => Ok(ids),
        PostCreationOutcome::Replayed(existing) => Ok((existing.thread_id, existing.post_id, None)),
    }
}

/// Create a public thread submission or return its canonical replay target.
///
/// The submission-token lookup is performed after `BEGIN IMMEDIATE`, before
/// any content mutation, so concurrent requests serialize on one durable
/// database decision.
///
/// # Errors
/// Returns an error if the transaction cannot be started or committed, or if
/// any insert in a new submission fails.
pub(crate) fn create_thread_submission(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
    post: &super::NewPost,
    submission_token: &str,
    poll: Option<&PollInsert<'_>>,
    filesystem: PostFilesystemCommit<'_, impl FnOnce(&rusqlite::Connection) -> Result<()>>,
) -> Result<PostCreationOutcome<(i64, i64, Option<i64>)>> {
    // BEGIN IMMEDIATE acquires the write lock upfront to avoid SQLITE_BUSY
    // during the lock-upgrade step that DEFERRED transactions perform on first
    // write. With &Connection (not &mut Connection) we cannot use rusqlite's
    // typed Transaction::new(Immediate), so we issue the pragma directly.
    let _timing = super::diagnostics::WriteTiming::begin(conn, "create_thread")?;

    let result: Result<PostCreationOutcome<(i64, i64, Option<i64>)>> = (|| {
        if let Some(ip_hash) = post.ip_hash.as_deref() {
            if let Some(existing) =
                super::posts::get_post_submission(conn, submission_token, ip_hash, board_id)?
            {
                validate_submission_target(existing, None)?;
                return Ok(PostCreationOutcome::Replayed(existing));
            }
        }

        reject_conflicting_submission(conn, submission_token)?;
        (filesystem.validate)(conn)?;
        validate_deduplicated_paths(conn, filesystem.deduplicated_paths)?;

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

        if let Some(op) = filesystem.pending_fs_op {
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
        if filesystem.schedule_thread_prune {
            super::posts::persist_thread_prune_intent_in_tx(conn, board_id)
                .context("Persist required board prune intent failed")
                .map(|_operation_summary| ())?;
        }

        let created_at = persist_new_post_metadata(
            conn,
            &post_with_thread,
            post_id,
            filesystem.media_job_board,
        )?;
        Ok(PostCreationOutcome::Created(
            (thread_id, post_id, poll_id),
            created_at,
        ))
    })();

    match result {
        Ok(ids) => {
            super::commit_transaction(
                conn,
                "Failed to commit create_thread_with_optional_poll transaction",
            )?;
            Ok(ids)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

// Thread mutation
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
    match create_reply_submission(
        conn,
        post,
        submission_token,
        should_bump,
        PostFilesystemCommit::new(pending_fs_op, &[], false),
    )? {
        PostCreationOutcome::Created(post_id, _) => Ok(post_id),
        PostCreationOutcome::Replayed(existing) => Ok(existing.post_id),
    }
}

/// Create a public reply submission or return its canonical replay target.
///
/// # Errors
/// Returns an error if the transaction cannot be started or committed, if the
/// thread is not writable, or if any mutation in a new submission fails.
pub(crate) fn create_reply_submission(
    conn: &rusqlite::Connection,
    post: &super::NewPost,
    submission_token: &str,
    should_bump: bool,
    filesystem: PostFilesystemCommit<'_, impl FnOnce(&rusqlite::Connection) -> Result<()>>,
) -> Result<PostCreationOutcome<i64>> {
    // Serialize token replay, policy/thread validation, count and bump decisions.
    let _timing = super::diagnostics::WriteTiming::begin(conn, "create_reply")?;

    let result: Result<PostCreationOutcome<i64>> = (|| {
        if let Some(ip_hash) = post.ip_hash.as_deref() {
            if let Some(existing) =
                super::posts::get_post_submission(conn, submission_token, ip_hash, post.board_id)?
            {
                validate_submission_target(existing, Some(post.thread_id))?;
                return Ok(PostCreationOutcome::Replayed(existing));
            }
        }

        if post.is_op {
            return Err(crate::error::AppError::BadRequest(
                "A reply cannot be a thread starter.".into(),
            )
            .into());
        }
        reject_conflicting_submission(conn, submission_token)?;
        (filesystem.validate)(conn)?;
        validate_deduplicated_paths(conn, filesystem.deduplicated_paths)?;

        ensure_replyable(conn, post.board_id, post.thread_id)?;

        let post_id = super::posts::create_post_inner(conn, post)?;
        let updated = if should_bump {
            conn.execute(
                "UPDATE threads
                 SET bumped_at = CASE
                         WHEN reply_count < (SELECT bump_limit FROM boards WHERE id = ?2)
                         THEN unixepoch() ELSE bumped_at END,
                     reply_count = reply_count + 1
                 WHERE id = ?1 AND board_id = ?2 AND locked = 0 AND archived = 0",
                params![post.thread_id, post.board_id],
            )?
        } else {
            conn.execute(
                "UPDATE threads
                 SET reply_count = reply_count + 1
                 WHERE id = ?1 AND board_id = ?2 AND locked = 0 AND archived = 0",
                params![post.thread_id, post.board_id],
            )?
        };
        if updated == 0 {
            anyhow::bail!(
                "Thread id {} not found while updating reply metadata",
                post.thread_id
            );
        }
        if let Some(op) = filesystem.pending_fs_op {
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
        let created_at =
            persist_new_post_metadata(conn, post, post_id, filesystem.media_job_board)?;
        Ok(PostCreationOutcome::Created(post_id, created_at))
    })();

    match result {
        Ok(outcome) => {
            super::commit_transaction(
                conn,
                "Failed to commit create_reply_with_thread_update transaction",
            )?;
            Ok(outcome)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Authoritative current parent-state check, performed under the posting write lock.
fn ensure_replyable(conn: &rusqlite::Connection, board_id: i64, thread_id: i64) -> Result<()> {
    let flags = conn
        .query_row(
            "SELECT locked, archived
                 FROM threads
                 WHERE id = ?1 AND board_id = ?2",
            params![thread_id, board_id],
            |row| {
                Ok((
                    row.get::<_, i32>(0)? != 0_i32,
                    row.get::<_, i32>(1)? != 0_i32,
                ))
            },
        )
        .optional()?;
    let Some((locked, archived)) = flags else {
        return Err(crate::error::AppError::NotFound(
            "Thread not found. It may have been deleted or pruned.".into(),
        )
        .into());
    };
    if archived {
        return Err(anyhow::Error::new(ThreadClosed::Archived));
    }
    if locked {
        return Err(anyhow::Error::new(ThreadClosed::Locked));
    }

    Ok(())
}

/// Revalidate a prior dedup cache hit after the post transaction owns the write lock.
fn validate_deduplicated_paths(conn: &rusqlite::Connection, paths: &[&str]) -> Result<()> {
    for path in paths {
        let valid = conn.query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM file_hashes fh WHERE fh.file_path = ?1
             ) AND NOT EXISTS (
                 SELECT 1 FROM posts p
                 WHERE (p.file_path = ?1 OR p.audio_file_path = ?1)
                   AND p.media_processing_state = ?2
             )",
            params![path, super::MEDIA_ORIGINAL_PRUNE_PENDING],
            |row| row.get::<_, bool>(0),
        )?;
        if !valid {
            anyhow::bail!("deduplicated media changed before post creation");
        }
    }
    Ok(())
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
/// Archiving locks the thread. Unarchiving preserves its lock state; callers
/// must invoke `set_thread_locked` separately when they intend to unlock it.
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
/// The transaction keeps the following sequence atomic with concurrent admin
/// and pruning operations:
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
    let candidates = collect_thread_file_paths(conn, &[thread_id])?;

    let deleted = conn
        .execute("DELETE FROM threads WHERE id = ?1", params![thread_id])
        .context("Failed to delete thread")?;
    if deleted == 0 {
        return Err(crate::error::AppError::NotFound(format!(
            "Thread id {thread_id} not found"
        )));
    }

    // The reference check sees the post-delete state inside this transaction.
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
            super::commit_transaction(conn, "Failed to commit delete_thread transaction")?;
            Ok(safe)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

// Archive / prune
/// Archive overflow non-sticky active threads, keeping the newest bump/ID pairs.
///
/// Archiving locks threads and preserves all posts and media references.
/// Sticky active threads do not count against the limit; locked threads do.
///
/// # Errors
/// Returns an error for invalid limits or a failed transaction.
pub fn archive_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let count = archive_old_threads_in_tx(&tx, board_id, max)?;
    tx.commit()?;
    Ok(count)
}

/// Archive overflow while the caller holds the SQLite write transaction.
pub(crate) fn archive_old_threads_in_tx(
    conn: &rusqlite::Transaction<'_>,
    board_id: i64,
    max: i64,
) -> Result<usize> {
    validate_retention_limit(max, false)?;
    // A subquery avoids building an unbounded parameter list for backlog recovery.
    conn.execute(
        "UPDATE threads SET archived = 1, locked = 1
         WHERE id IN (
             SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC, id DESC LIMIT -1 OFFSET ?2
         )",
        params![board_id, max],
    )
    .context("Failed to archive overflow threads")
}

/// Hard-delete overflow non-sticky active threads when archiving is disabled.
///
/// Returns safe file paths and a durable cleanup intent for filesystem replay.
///
/// # Errors
/// Returns an error for invalid limits or a failed transaction.
pub fn prune_old_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<crate::db::DeletePathsResult> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let result = prune_old_threads_in_tx(&tx, board_id, max, false)?;
    tx.commit()?;
    Ok(result)
}

/// Hard-delete archived overflow, keeping the first entries in archive order.
///
/// Retention uses bump time, not time of archival. Archived sticky threads are
/// subject to this cap too. File cleanup is durably scheduled before commit.
///
/// # Errors
/// Returns an error for invalid limits or a failed transaction.
pub fn prune_old_archived_threads(
    conn: &rusqlite::Connection,
    board_id: i64,
    max: i64,
) -> Result<crate::db::DeletePathsResult> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let result = prune_old_threads_in_tx(&tx, board_id, max, true)?;
    tx.commit()?;
    Ok(result)
}

/// Reject invalid destructive limits even when called outside the worker.
fn validate_retention_limit(max: i64, archived: bool) -> Result<()> {
    let upper = if archived { 10_000 } else { 1_000 };
    anyhow::ensure!(
        (1..=upper).contains(&max),
        "invalid retention limit {max}; expected 1..={upper}; preserving content"
    );
    Ok(())
}

/// Select, delete, reference-check and journal files in the caller's transaction.
pub(crate) fn prune_old_threads_in_tx(
    conn: &rusqlite::Transaction<'_>,
    board_id: i64,
    max: i64,
    archived: bool,
) -> Result<crate::db::DeletePathsResult> {
    validate_retention_limit(max, archived)?;
    // Archive ties historically ascend by ID. Active ties descend by ID, exactly
    // matching the board index. Never let query planner choice decide retention.
    let sql = if archived {
        "SELECT id FROM threads WHERE board_id = ?1 AND archived = 1
         ORDER BY bumped_at DESC, id ASC LIMIT -1 OFFSET ?2"
    } else {
        "SELECT id FROM threads WHERE board_id = ?1 AND sticky = 0 AND archived = 0
         ORDER BY bumped_at DESC, id DESC LIMIT -1 OFFSET ?2"
    };
    let ids = conn
        .prepare_cached(sql)?
        .query_map(params![board_id, max], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let candidates = collect_thread_file_paths(conn, &ids)?;
    for batch in ids.chunks(500) {
        let placeholders = vec!["?"; batch.len()].join(", ");
        conn.execute(
            &format!("DELETE FROM threads WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(batch),
        )
        .context("Failed to delete overflow threads")
        .map(|_affected_rows| ())?;
    }
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

// Archive listing
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
    load_thread_page(conn, board_id, limit, offset, true)
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
