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
    // Preserve the legacy aggregate's tie order for each listing, including
    // deterministic boundaries when many replies bump in the same second.
    let order = if archived {
        "t.bumped_at DESC, t.id ASC"
    } else {
        "t.sticky DESC, t.bumped_at DESC, t.id DESC"
    };
    format!(
        "WITH page AS MATERIALIZED (
             SELECT t.* FROM threads t
             WHERE t.board_id = ?1 AND t.archived = ?4
               AND EXISTS (SELECT 1 FROM posts op WHERE op.thread_id=t.id AND op.is_op=1)
             ORDER BY {order} LIMIT ?2 OFFSET ?3
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
    Created(T),
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
        Self {
            pending_fs_op,
            deduplicated_paths,
            schedule_thread_prune,
            validate,
        }
    }
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
        PostCreationOutcome::Created(ids) => Ok(ids),
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
                return Ok(PostCreationOutcome::Replayed(existing));
            }
        }

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

        Ok(PostCreationOutcome::Created((thread_id, post_id, poll_id)))
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
        PostCreationOutcome::Created(post_id) => Ok(post_id),
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
                return Ok(PostCreationOutcome::Replayed(existing));
            }
        }

        (filesystem.validate)(conn)?;
        validate_deduplicated_paths(conn, filesystem.deduplicated_paths)?;

        let flags = conn
            .query_row(
                "SELECT locked, archived
                 FROM threads
                 WHERE id = ?1 AND board_id = ?2",
                params![post.thread_id, post.board_id],
                |row| {
                    Ok((
                        row.get::<_, i32>(0)? != 0_i32,
                        row.get::<_, i32>(1)? != 0_i32,
                    ))
                },
            )
            .optional()?;
        let Some((locked, archived)) = flags else {
            anyhow::bail!(
                "Thread id {} not found while creating reply",
                post.thread_id
            );
        };
        if archived {
            return Err(anyhow::Error::new(ThreadClosed::Archived));
        }
        if locked {
            return Err(anyhow::Error::new(ThreadClosed::Locked));
        }

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
        Ok(PostCreationOutcome::Created(post_id))
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

#[cfg(test)]
mod tests {
    use super::{
        count_threads_for_board, create_reply_with_thread_update, create_thread_submission,
        delete_thread, prune_old_archived_threads, prune_old_threads, validate_deduplicated_paths,
        PostFilesystemCommit, ThreadClosed,
    };
    use crate::db::{create_board, create_thread_with_optional_poll, get_board_by_short, NewPost};
    use crate::error::AppError;
    use crate::models::MediaType;
    use crate::pending_fs::finalize_delete_files_payload;
    use anyhow::{Context as _, Result};
    use rusqlite::{params, Connection};

    fn test_conn() -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
        super::super::schema::install_or_migrate_schema(&conn)?;
        Ok(conn)
    }

    fn create_plain_thread(conn: &Connection, board_id: i64, title: &str) -> Result<i64> {
        let post = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some(title.to_owned()),
            body: title.to_owned(),
            body_html: title.to_owned(),
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
        let (thread_id, _, _) =
            create_thread_with_optional_poll(conn, board_id, Some(title), &post, "", None, None)?;
        Ok(thread_id)
    }

    #[test]
    fn required_prune_insert_failure_rolls_back_new_thread() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "durable", "Durable", "", false)?;
        conn.execute_batch(
            "CREATE TRIGGER fail_required_thread_prune
             BEFORE INSERT ON background_jobs
             WHEN NEW.job_type = 'thread_prune'
             BEGIN
                 SELECT RAISE(ABORT, 'injected durable prune failure');
             END;",
        )?;
        let post = NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("atomic".to_owned()),
            body: "atomic".to_owned(),
            body_html: "atomic".to_owned(),
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

        let result = create_thread_submission(
            &conn,
            board_id,
            Some("atomic"),
            &post,
            "submission",
            None,
            PostFilesystemCommit::new(None, &[], true),
        );

        anyhow::ensure!(result.is_err());
        anyhow::ensure!(count_threads_for_board(&conn, board_id)? == 0);
        let jobs: i64 =
            conn.query_row("SELECT COUNT(*) FROM background_jobs", [], |row| row.get(0))?;
        anyhow::ensure!(jobs == 0);
        Ok(())
    }

    fn plain_reply(board_id: i64, thread_id: i64) -> NewPost {
        NewPost {
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
        }
    }

    #[test]
    fn listing_last_post_includes_non_bumping_text_and_audio_without_counting_them_as_images(
    ) -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "read", "Reading", "", false)?;
        let thread_id = create_plain_thread(&conn, board_id, "opening post only")?;
        conn.execute(
            "UPDATE threads SET created_at = 100, bumped_at = 100 WHERE id = ?1",
            params![thread_id],
        )
        .map(|_affected_rows| ())?;
        conn.execute(
            "UPDATE posts SET created_at = 100 WHERE thread_id = ?1",
            params![thread_id],
        )
        .map(|_affected_rows| ())?;
        let thread = super::get_thread(&conn, thread_id)?.context("opening thread should exist")?;
        anyhow::ensure!(
            thread.last_post_at == 100 && thread.image_count == 0,
            "a thread without replies uses the opening-post time and has no images"
        );

        let mut image = plain_reply(board_id, thread_id);
        image.file_path = Some("read/image.png".to_owned());
        let image_id = create_reply_with_thread_update(&conn, &image, "", false, None)?;
        let mut audio = plain_reply(board_id, thread_id);
        audio.audio_file_path = Some("read/audio.wav".to_owned());
        let audio_id = create_reply_with_thread_update(&conn, &audio, "", false, None)?;
        let text_id = create_reply_with_thread_update(
            &conn,
            &plain_reply(board_id, thread_id),
            "",
            false,
            None,
        )?;
        for (post_id, timestamp) in [(image_id, 200_i32), (audio_id, 300_i32), (text_id, 400_i32)] {
            conn.execute(
                "UPDATE posts SET created_at = ?1 WHERE id = ?2",
                params![timestamp, post_id],
            )
            .map(|_affected_rows| ())?;
        }
        let replied_thread =
            super::get_thread(&conn, thread_id)?.context("thread should exist after replies")?;
        anyhow::ensure!(
            replied_thread.last_post_at == 400 && replied_thread.bumped_at == 100 && replied_thread.image_count == 1,
            "sage text replies update last-post time while bump time and image counts remain correct"
        );
        let active = super::get_threads_for_board(&conn, board_id, 20, 0)?;
        anyhow::ensure!(active.len() == 1, "the active listing contains one thread");
        let active = active.first().context("active thread should exist")?;
        anyhow::ensure!(
            active.last_post_at == 400 && active.image_count == 1,
            "catalog/index listing must retain the same post and image aggregates"
        );
        conn.execute(
            "UPDATE threads SET archived = 1 WHERE id = ?1",
            params![thread_id],
        )
        .map(|_affected_rows| ())?;
        let archived = super::get_archived_threads_for_board(&conn, board_id, 20, 0)?;
        anyhow::ensure!(
            archived.len() == 1,
            "the archive listing contains one thread"
        );
        let archived = archived.first().context("archived thread should exist")?;
        anyhow::ensure!(
            archived.last_post_at == 400 && archived.image_count == 1,
            "archive listing must retain the same post and image aggregates"
        );
        Ok(())
    }

    fn pending_upload_op(id: &str) -> crate::pending_fs::PendingFsOpInsert {
        crate::pending_fs::PendingFsOpInsert {
            id: id.to_owned(),
            kind: crate::pending_fs::UPLOAD_FINALIZE_KIND,
            payload_json: "{}".to_owned(),
        }
    }

    fn thread_reply_count(conn: &Connection, thread_id: i64) -> Result<i64> {
        Ok(conn.query_row(
            "SELECT reply_count FROM threads WHERE id = ?1",
            params![thread_id],
            |row| row.get(0),
        )?)
    }

    fn post_count(conn: &Connection, thread_id: i64) -> Result<i64> {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )?)
    }

    fn pending_fs_op_count(conn: &Connection) -> Result<i64> {
        Ok(conn.query_row("SELECT COUNT(*) FROM pending_fs_ops", [], |row| row.get(0))?)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn dedup_path_revalidation_rejects_a_concurrent_prune_transition() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "b", "Random", "", false)?;
        crate::db::record_file_hash(
            &conn,
            "hash",
            "b/file.webp",
            "b/thumbs/file.webp",
            "image/webp",
        )?;
        validate_deduplicated_paths(&conn, &["b/file.webp"])?;

        let thread_id = create_plain_thread(&conn, board_id, "thread")?;
        let post_id = create_reply_with_thread_update(
            &conn,
            &plain_reply(board_id, thread_id),
            "submission",
            true,
            None,
        )?;
        conn.execute(
            "UPDATE posts
             SET file_path = 'b/file.webp', media_processing_state = ?1
             WHERE id = ?2",
            params![crate::db::MEDIA_ORIGINAL_PRUNE_PENDING, post_id],
        )
        .map(|_affected_rows| ())?;

        assert!(validate_deduplicated_paths(&conn, &["b/file.webp"]).is_err());
        assert!(crate::db::find_file_by_hash(&conn, "hash")?.is_none());
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn prune_old_threads_commits_even_when_no_files_are_safe() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "prune", "Prune", "", false)?;
        create_plain_thread(&conn, board_id, "old thread").map(|_created_id| ())?;
        create_plain_thread(&conn, board_id, "new thread").map(|_created_id| ())?;
        let board = get_board_by_short(&conn, "prune")?.context("prune board should exist")?;
        assert_eq!(
            count_threads_for_board(&conn, board.id)?,
            2,
            "the test should begin with two threads"
        );

        let deleted = prune_old_threads(&conn, board.id, 1)?;
        assert!(
            deleted.paths.is_empty(),
            "posts without media should produce no cleanup paths"
        );
        assert!(
            deleted.pending_fs_op_id.is_none(),
            "no cleanup operation should be queued without paths"
        );
        assert_eq!(
            count_threads_for_board(&conn, board.id)?,
            1,
            "pruning should commit the oldest thread deletion"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn prune_old_archived_threads_commits_even_when_no_files_are_safe() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "aprune", "Archive", "", false)?;
        let first = create_plain_thread(&conn, board_id, "old archived thread")?;
        let second = create_plain_thread(&conn, board_id, "new archived thread")?;
        conn.execute(
            "UPDATE threads SET archived = 1 WHERE id IN (?1, ?2)",
            params![first, second],
        )
        .map(|_affected_rows| ())?;

        let deleted = prune_old_archived_threads(&conn, board_id, 1)?;
        assert!(
            deleted.paths.is_empty(),
            "posts without media should produce no cleanup paths"
        );
        assert!(
            deleted.pending_fs_op_id.is_none(),
            "no cleanup operation should be queued without paths"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 1",
                params![board_id],
                |row| row.get::<_, i64>(0),
            )?,
            1,
            "archived pruning should commit the oldest deletion"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_thread_returns_pending_cleanup_for_media_reply() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "media", "Media", "", false)?;
        let temp_dir = tempfile::tempdir()?;
        let upload_dir = temp_dir.path().join("uploads");
        let board_dir = upload_dir.join("media");
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir)?;
        std::fs::write(board_dir.join("reply.webp"), b"reply")?;
        std::fs::write(thumb_dir.join("reply.webp"), b"thumb")?;

        let thread_id = create_plain_thread(&conn, board_id, "thread with media reply")?;
        let reply = NewPost {
            thread_id,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: None,
            body: "reply".to_owned(),
            body_html: "reply".to_owned(),
            ip_hash: None,
            file_path: Some("media/reply.webp".to_owned()),
            file_name: Some("reply.webp".to_owned()),
            file_size: Some(5),
            thumb_path: Some("media/thumbs/reply.webp".to_owned()),
            mime_type: Some("image/webp".to_owned()),
            media_type: Some(MediaType::Image.as_str().to_owned()),
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: false,
        };
        create_reply_with_thread_update(&conn, &reply, "", false, None).map(|_created_id| ())?;

        let deleted = delete_thread(&conn, thread_id)?;
        assert!(
            deleted.pending_fs_op_id.is_some(),
            "deleting media should enqueue durable cleanup"
        );
        assert!(
            deleted.paths.iter().any(|path| path == "media/reply.webp"),
            "primary media path should be returned"
        );
        assert!(
            deleted
                .paths
                .iter()
                .any(|path| path == "media/thumbs/reply.webp"),
            "thumbnail path should be returned"
        );
        assert_eq!(
            count_threads_for_board(&conn, board_id)?,
            0,
            "thread deletion should commit"
        );

        finalize_delete_files_payload(
            &conn,
            upload_dir
                .to_str()
                .context("temporary path should be UTF-8")?,
            deleted.pending_fs_op_id.as_deref(),
            &deleted.paths,
        )?;

        assert!(
            !board_dir.join("reply.webp").exists(),
            "primary media file should be removed"
        );
        assert!(
            !thumb_dir.join("reply.webp").exists(),
            "thumbnail file should be removed"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn create_reply_rejects_locked_thread_without_mutations() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "lock", "Lock", "", false)?;
        let thread_id = create_plain_thread(&conn, board_id, "locked thread")?;
        conn.execute(
            "UPDATE threads SET locked = 1 WHERE id = ?1",
            params![thread_id],
        )
        .map(|_affected_rows| ())?;
        let reply = plain_reply(board_id, thread_id);
        let pending_op = pending_upload_op("locked-reply-upload");

        let error = create_reply_with_thread_update(&conn, &reply, "", true, Some(&pending_op))
            .err()
            .context("locked thread should reject reply")?;

        assert_eq!(
            error.downcast_ref::<ThreadClosed>(),
            Some(&ThreadClosed::Locked),
            "the rejection must stay downcastable for the reply status mapping"
        );
        assert!(
            error.to_string().contains("This thread is locked."),
            "the rejection should identify the locked state"
        );
        assert_eq!(
            post_count(&conn, thread_id)?,
            1,
            "no reply row should be inserted"
        );
        assert_eq!(
            thread_reply_count(&conn, thread_id)?,
            0,
            "reply count should remain unchanged"
        );
        assert_eq!(
            pending_fs_op_count(&conn)?,
            0,
            "pending upload operation should not be inserted"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn create_reply_rejects_archived_thread_without_mutations() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "arch", "Archive", "", false)?;
        let thread_id = create_plain_thread(&conn, board_id, "archived thread")?;
        conn.execute(
            "UPDATE threads SET archived = 1 WHERE id = ?1",
            params![thread_id],
        )
        .map(|_affected_rows| ())?;
        let reply = plain_reply(board_id, thread_id);
        let pending_op = pending_upload_op("archived-reply-upload");

        let error = create_reply_with_thread_update(&conn, &reply, "", true, Some(&pending_op))
            .err()
            .context("archived thread should reject reply")?;

        assert_eq!(
            error.downcast_ref::<ThreadClosed>(),
            Some(&ThreadClosed::Archived),
            "the rejection must stay downcastable for the reply status mapping"
        );
        assert!(
            error.to_string().contains("This thread is archived."),
            "the rejection should identify the archived state"
        );
        assert_eq!(
            post_count(&conn, thread_id)?,
            1,
            "no reply row should be inserted"
        );
        assert_eq!(
            thread_reply_count(&conn, thread_id)?,
            0,
            "reply count should remain unchanged"
        );
        assert_eq!(
            pending_fs_op_count(&conn)?,
            0,
            "pending upload operation should not be inserted"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_thread_returns_not_found_on_retry() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "delth", "Del Thread", "", false)?;
        let thread_id = create_plain_thread(&conn, board_id, "thread to delete")?;

        let deleted = delete_thread(&conn, thread_id)?;
        assert!(
            deleted.paths.is_empty(),
            "thread without media should have no cleanup paths"
        );
        let retry = delete_thread(&conn, thread_id);
        assert!(
            matches!(retry, Err(AppError::NotFound(message)) if message.contains("Thread id")),
            "a repeated delete should return not found"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_thread_removes_replies_and_retries_cleanly() -> Result<()> {
        let conn = test_conn()?;
        let board_id = create_board(&conn, "delthr", "Del Thread Replies", "", false)?;
        let thread_id = create_plain_thread(&conn, board_id, "thread with reply")?;
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
        create_reply_with_thread_update(&conn, &reply, "", false, None).map(|_created_id| ())?;

        let deleted = delete_thread(&conn, thread_id)?;
        assert!(
            deleted.paths.is_empty(),
            "posts without media should have no cleanup paths"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get::<_, i64>(0),
            )?,
            0,
            "thread deletion should remove all replies"
        );
        let retry = delete_thread(&conn, thread_id);
        assert!(
            matches!(retry, Err(AppError::NotFound(message)) if message.contains("Thread id")),
            "a repeated delete should return not found"
        );
        Ok(())
    }

    #[test]
    fn paged_thread_aggregates_match_the_unbounded_baseline() -> Result<()> {
        let conn = test_conn()?;
        let board = create_board(&conn, "page", "Page", "", false)?;
        for number in 0_i32..35_i32 {
            let thread = create_plain_thread(&conn, board, &format!("thread {number}"))?;
            conn.execute(
                "UPDATE threads SET bumped_at=?1, sticky=?2, archived=?3 WHERE id=?4",
                params![
                    1_700_000_000_i32 + number / 7_i32,
                    i64::from(number % 11_i32 == 0_i32),
                    i64::from(number % 5_i32 == 0_i32),
                    thread
                ],
            )
            .map(|_affected_rows| ())?;
            for reply in 0_i32..number % 7_i32 {
                conn.execute("INSERT INTO posts(thread_id,board_id,body,body_html,deletion_token,file_path,created_at)
                              VALUES(?1,?2,'reply','reply','delete',?3,?4)",
                    params![thread,board,(reply%2_i32==0_i32).then(|| format!("page/file-{number}-{reply}")),1_700_000_100_i32+reply]).map(|_affected_rows| ())?;
            }
        }
        // A missing OP must be excluded before pagination, matching the old JOIN.
        conn.execute(
            "INSERT INTO threads(board_id,bumped_at,sticky) VALUES(?1,1900000000,1)",
            [board],
        )
        .map(|_affected_rows| ())?;
        for archived in [false, true] {
            let order = if archived {
                "t.bumped_at DESC"
            } else {
                "t.sticky DESC,t.bumped_at DESC"
            };
            let old = format!(
                "{} WHERE t.board_id=?1 AND t.archived=?4 GROUP BY t.id,op.id ORDER BY {order} LIMIT ?2 OFFSET ?3",
                super::THREAD_SELECT
            );
            for (limit, offset) in [
                (1, 0),
                (10, 0),
                (10, 10),
                (100, 0),
                (5, 99),
                (0, 0),
                (-1, 2),
            ] {
                let baseline = conn
                    .prepare(&old)?
                    .query_map(
                        params![board, limit, offset, i64::from(archived)],
                        super::map_thread,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let paged = super::load_thread_page(&conn, board, limit, offset, archived)?;
                let baseline_ids: Vec<_> = baseline.iter().map(|thread| thread.id).collect();
                let paged_ids: Vec<_> = paged.iter().map(|thread| thread.id).collect();
                anyhow::ensure!(
                    serde_json::to_value(baseline)? == serde_json::to_value(paged)?,
                    "page aggregates changed: archived={archived}, limit={limit}, offset={offset}, baseline={baseline_ids:?}, paged={paged_ids:?}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn concurrent_bump_requests_use_the_current_count_and_limit() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let board = create_board(&conn, "bump", "Bump", "", false)?;
        conn.execute("UPDATE boards SET bump_limit=1 WHERE id=?1", [board])
            .map(|_affected_rows| ())?;
        let thread = create_plain_thread(&conn, board, "target")?;
        let op = crate::db::get_posts_for_thread(&conn, thread)?
            .into_iter()
            .next()
            .context("OP")?;
        let mut post = NewPost {
            thread_id: thread,
            board_id: board,
            name: "anon".into(),
            tripcode: None,
            subject: None,
            body: "reply".into(),
            body_html: "reply".into(),
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
            deletion_token: op.deletion_token,
            is_op: false,
        };
        post.ip_hash = Some("bump-actor".into());
        drop(conn);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| -> Result<()> {
            let handles:Vec<_>=(0_i32..2_i32).map(|number| {
                let pool=&pool; let post=&post; let barrier=&barrier;
                scope.spawn(move || -> Result<()> {
                    let _barrier_state = barrier.wait();
                    let worker_conn=pool.get()?;
                    super::create_reply_submission(&worker_conn,post,&format!("bump-{number}"),true,
                        PostFilesystemCommit::new_with_validation(None,&[],false,|validation_conn:&Connection| {
                            // Give the second serialized reply a distinguishable previous
                            // timestamp; no sleep or wall-clock assumption is required.
                            validation_conn.execute("UPDATE threads SET bumped_at=42 WHERE id=?1 AND reply_count=1",[thread]).map(|_affected_rows| ())?;
                            Ok(())
                        })).map(|_operation_summary| ())?;
                    Ok(())
                })
            }).collect();
            for handle in handles {
                handle.join().map_err(|panic_payload| {
                    anyhow::anyhow!(
                        "reply worker panicked: {}",
                        crate::media::process::panic_message(panic_payload.as_ref())
                    )
                })??;
            }
            Ok(())
        })?;
        let recovered_conn = pool.get()?;
        let state = crate::db::get_thread(&recovered_conn, thread)?.context("thread")?;
        anyhow::ensure!(
            state.reply_count == 2 && state.bumped_at == 42,
            "concurrent replies exceeded the bump limit"
        );
        crate::db::verify_database_schema(&recovered_conn)?;
        Ok(())
    }
}

#[cfg(test)]
mod archive_tests;
