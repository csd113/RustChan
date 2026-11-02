//! Durable background-job scheduling, recovery, diagnostics, and completion persistence.

use super::posts::{
    record_file_hash, StaleMediaTargetError, MEDIA_ORIGINAL_PRUNED, MEDIA_ORIGINAL_PRUNE_PENDING,
    MEDIA_PROCESSING_FAILED, MEDIA_PROCESSING_PENDING,
};
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

/// Shared retry budget for job claiming and terminal-failure transitions.
const MAX_JOB_ATTEMPTS: i64 = 3;

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
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private owner preserves the crate-visible posts re-export without making this transaction helper public"
)]
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
