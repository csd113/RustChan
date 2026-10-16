// Jobs persist in SQLite and are claimed atomically with UPDATE … RETURNING.
// Cancellation tokens let graceful shutdown drain in-progress work; failures
// retain their last error and retry only within the shared budget.

use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::{Context as _, Result};
use dashmap::DashMap;
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use std::num::NonZero;
use std::path::PathBuf;
use std::process::Output;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command as TokioCommand;
use tokio::sync::Notify;
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Builds an `FFmpeg` command using the configured executable path.
fn ffmpeg_command() -> TokioCommand {
    let mut command = TokioCommand::new(&CONFIG.ffmpeg_path);
    let _stdin_configuration = command.stdin(Stdio::null());
    #[cfg(unix)]
    {
        let _process_group_configuration = command.process_group(0);
    }
    command
}

/// Result of waiting for an asynchronous `FFmpeg` subprocess.
pub(crate) enum AsyncWaitOutcome {
    /// The process exited and its captured output is available.
    Exited(Output),
    /// The configured execution deadline elapsed.
    TimedOut,
    /// Application shutdown or explicit cancellation was requested.
    Cancelled,
}

/// Exit state observed while racing a media subprocess against its deadline.
enum MediaChildWaitOutcome {
    /// The direct child exited with this status.
    Exited(std::process::ExitStatus),
    /// The configured execution deadline elapsed.
    TimedOut,
    /// Application shutdown or explicit cancellation was requested.
    Cancelled,
}

/// Waits for an `FFmpeg` child while racing its deadline and cancellation token.
pub(crate) async fn wait_for_ffmpeg_output(
    mut child: tokio::process::Child,
    timeout: Duration,
    cancel: CancellationToken,
) -> Result<AsyncWaitOutcome> {
    let mut process_group = crate::media::process::ProcessGroupGuard::new(child.id());
    let stdout_reader = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(read_bounded_async_pipe(stdout)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_bounded_async_pipe(stderr)));

    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => MediaChildWaitOutcome::Cancelled,
        result = child.wait() => MediaChildWaitOutcome::Exited(
            result.context("media subprocess wait failed")?
        ),
        () = sleep(timeout) => MediaChildWaitOutcome::TimedOut,
    };

    match outcome {
        MediaChildWaitOutcome::Exited(status) => {
            process_group.terminate_remaining();
            let (stdout, stderr) = tokio::join!(
                join_async_pipe_reader(stdout_reader, "stdout"),
                join_async_pipe_reader(stderr_reader, "stderr")
            );
            Ok(AsyncWaitOutcome::Exited(Output {
                status,
                stdout: stdout?,
                stderr: stderr?,
            }))
        }
        terminal @ (MediaChildWaitOutcome::TimedOut | MediaChildWaitOutcome::Cancelled) => {
            process_group.terminate_remaining();
            // Kill the direct child as a non-Unix fallback and protection
            // against a process-group setup race, then reap it before return.
            drop(child.start_kill());
            drop(child.wait().await);
            let (stdout_result, stderr_result) = tokio::join!(
                join_async_pipe_reader(stdout_reader, "stdout"),
                join_async_pipe_reader(stderr_reader, "stderr")
            );
            drop(stdout_result);
            drop(stderr_result);

            Ok(match terminal {
                MediaChildWaitOutcome::TimedOut => AsyncWaitOutcome::TimedOut,
                MediaChildWaitOutcome::Cancelled => AsyncWaitOutcome::Cancelled,
                MediaChildWaitOutcome::Exited(_) => {
                    return Err(anyhow::anyhow!(
                        "media subprocess exit state changed unexpectedly"
                    ));
                }
            })
        }
    }
}

/// Drains an asynchronous child pipe while retaining a bounded prefix.
async fn read_bounded_async_pipe(
    mut pipe: impl tokio::io::AsyncRead + Unpin,
) -> std::io::Result<Vec<u8>> {
    const READ_BUFFER_BYTES: usize = 8 * 1024;

    let mut bytes = Vec::with_capacity(READ_BUFFER_BYTES);
    let mut chunk = [0_u8; READ_BUFFER_BYTES];
    loop {
        let read = pipe.read(&mut chunk).await?;
        if read == 0 {
            break;
        }

        let remaining =
            crate::media::process::MEDIA_SUBPROCESS_OUTPUT_LIMIT_BYTES.saturating_sub(bytes.len());
        let retain = remaining.min(read);
        let retained = chunk.get(..retain).ok_or_else(|| {
            std::io::Error::other("media output retention exceeded its read buffer")
        })?;
        bytes.extend_from_slice(retained);
    }
    Ok(bytes)
}

/// Resolves an optional asynchronous pipe reader with contextual errors.
async fn join_async_pipe_reader(
    reader: Option<tokio::task::JoinHandle<std::io::Result<Vec<u8>>>>,
    pipe_name: &str,
) -> Result<Vec<u8>> {
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    reader
        .await
        .with_context(|| format!("media subprocess {pipe_name} reader task failed"))?
        .with_context(|| format!("media subprocess {pipe_name} read failed"))
}

/// How long a worker sleeps when the queue is empty.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

// Job definitions
/// All job variants the worker pool can process.
/// Serialised to JSON and stored in `background_jobs.payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "d")]
pub enum Job {
    /// Transcode an uploaded MP4/MKV to `WebM` (VP9 + Opus) via ffmpeg.
    VideoTranscode {
        /// Database identifier of the post owning the upload.
        post_id: i64,
        /// Path relative to `CONFIG.upload_dir`, e.g. "b/abc123.mp4".
        file_path: String,
        /// Short name of the board containing the post.
        board_short: String,
    },
    /// Generate a waveform PNG thumbnail with streaming Rust audio decoders.
    AudioWaveform {
        /// Database identifier of the post owning the upload.
        post_id: i64,
        /// Path relative to `CONFIG.upload_dir`.
        file_path: String,
        /// Short name of the board containing the post.
        board_short: String,
    },
    /// Delete overflow threads from a board (runs after a new thread is created).
    ThreadPrune {
        /// Database identifier of the board to prune.
        board_id: i64,
    },
    /// Lightweight spam / abuse signal logging.
    SpamCheck {
        /// Database identifier of the post to inspect.
        post_id: i64,
        /// Privacy-preserving hash of the posting address.
        ip_hash: String,
        /// Length of the submitted post body in bytes.
        body_len: usize,
    },
}

impl Job {
    /// Short identifier stored in `background_jobs.job_type` for diagnostics.
    #[must_use]
    pub const fn type_str(&self) -> &'static str {
        match self {
            Self::VideoTranscode {
                post_id: _,
                file_path: _,
                board_short: _,
            } => "video_transcode",
            Self::AudioWaveform {
                post_id: _,
                file_path: _,
                board_short: _,
            } => "audio_waveform",
            Self::ThreadPrune { board_id: _ } => "thread_prune",
            Self::SpamCheck {
                post_id: _,
                ip_hash: _,
                body_len: _,
            } => "spam_check",
        }
    }

    /// Return the durable post/source identity for asynchronous media work.
    const fn media_target(&self) -> Option<(i64, &str, &str)> {
        match self {
            Self::VideoTranscode {
                post_id,
                file_path,
                board_short,
            }
            | Self::AudioWaveform {
                post_id,
                file_path,
                board_short,
            } => Some((*post_id, file_path.as_str(), board_short.as_str())),
            Self::ThreadPrune { board_id: _ }
            | Self::SpamCheck {
                post_id: _,
                ip_hash: _,
                body_len: _,
            } => None,
        }
    }
}

/// Serialize a durable payload before any queue-state transaction begins.
fn serialize_job_payload<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).context("Serialize background job failed")
}

/// Result of attempting to add a background job to the bounded queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// The job was persisted with the contained database identifier.
    Enqueued(i64),
    /// Capacity back-pressure rejected the job before persistence.
    DroppedAtCapacity,
}

/// Emit diagnostics only for legacy duplicate states; ordinary coalescing is
/// expected and intentionally remains quiet.
fn log_thread_prune_schedule(board_id: i64, schedule: crate::db::ThreadPruneSchedule) {
    if schedule.duplicates_resolved > 0 || schedule.duplicate_running > 0 {
        warn!(
            target: "workers",
            board_id,
            job_id = schedule.job_id,
            duplicates_resolved = schedule.duplicates_resolved,
            duplicate_running = schedule.duplicate_running,
            "coalesced duplicate legacy thread-prune jobs"
        );
    }
}

// Job queue
/// Cheaply-cloneable handle to the shared job queue.
/// Clone this into every handler that needs to enqueue work.
#[derive(Clone, Debug)]
#[expect(
    clippy::partial_pub_fields,
    reason = "public operational handles preserve the queue API; accounting counters stay private to preserve invariants"
)]
pub struct JobQueue {
    /// Database pool used to persist and claim background jobs.
    pub pool: DbPool,
    /// Wakes an idle worker after a producer enqueues work.
    pub notify: Arc<Notify>,
    /// Token cancelled at shutdown; workers observe this to exit cleanly.
    pub cancel: CancellationToken,
    /// 2.2: Set of `file_path` strings for media jobs currently being processed.
    /// Workers check this before starting a `VideoTranscode` or `AudioWaveform` job
    /// and skip if the same path is already in flight, preventing redundant
    /// `FFmpeg` invocations on client retries or server-restart re-queues.
    pub in_progress: Arc<DashMap<String, bool>>,
    /// In-memory count of persisted jobs that have not yet been claimed.
    pending_jobs: Arc<AtomicU64>,
    /// Number of jobs rejected by capacity back-pressure.
    dropped_jobs: Arc<AtomicU64>,
    /// Number of video transcodes currently executing.
    active_video_jobs: Arc<AtomicU64>,
}

impl JobQueue {
    /// Creates a queue backed by `pool`, initializing its pending-job gauge.
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        let pending_jobs = pool
            .get()
            .ok()
            .and_then(|conn| crate::db::pending_job_count(&conn).ok())
            .and_then(|count| u64::try_from(count).ok())
            .unwrap_or_else(|| {
                warn!(
                    "JobQueue: pending-job count unavailable at startup; capacity back-pressure may under-count until the first poll"
                );
                0
            });
        Self {
            pool,
            notify: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
            in_progress: Arc::new(DashMap::new()),
            pending_jobs: Arc::new(AtomicU64::new(pending_jobs)),
            dropped_jobs: Arc::new(AtomicU64::new(0)),
            active_video_jobs: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Persist a job and wake a sleeping worker immediately.
    /// Safe to call from any thread, including inside `tokio::task::spawn_blocking`.
    ///
    /// 2.1: If the number of pending jobs already equals or exceeds
    /// `CONFIG.job_queue_capacity`, the job is dropped with a warning log
    /// rather than accepted. This prevents the queue table growing without
    /// bound under a post flood. Required `ThreadPrune` work is the exception:
    /// it is coalesced durably per board and never subjected to this capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when serializing the job, acquiring a database
    /// connection, or persisting the job fails.
    pub fn enqueue(&self, job: &Job) -> Result<EnqueueOutcome> {
        if job.media_target().is_some() {
            let conn = self
                .pool
                .get()
                .context("Get DB connection for atomic media-job creation failed")?;
            return self.enqueue_media(&conn, job);
        }
        if let Job::ThreadPrune { board_id } = job {
            let conn = self
                .pool
                .get()
                .context("Get DB connection for required board prune intent failed")?;
            let schedule = crate::db::persist_thread_prune_intent(&conn, *board_id)?;
            if schedule.inserted {
                let _previous_count = self.pending_jobs.fetch_add(1, Ordering::Relaxed);
            }
            log_thread_prune_schedule(*board_id, schedule);
            self.notify.notify_one();
            return Ok(EnqueueOutcome::Enqueued(schedule.job_id));
        }

        let payload = serialize_job_payload(job)?;

        // Back-pressure: check pending count before inserting.
        if self.reserve_pending_slot(job.type_str()) {
            let conn = match self.pool.get() {
                Ok(conn) => conn,
                Err(error) => {
                    let _previous_count = self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
                    return Err(error.into());
                }
            };
            let enqueue_result = crate::db::enqueue_job(&conn, job.type_str(), &payload);
            match enqueue_result {
                Ok(id) => {
                    self.notify.notify_one();
                    Ok(EnqueueOutcome::Enqueued(id))
                }
                Err(error) => {
                    let _previous_count = self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
                    Err(error)
                }
            }
        } else {
            let _previous_count = self.dropped_jobs.fetch_add(1, Ordering::Relaxed);
            Ok(EnqueueOutcome::DroppedAtCapacity)
        }
    }

    /// Atomically mark a post pending and persist its durable media job, then
    /// wake workers only after commit. A failed wakeup cannot lose work because
    /// workers also poll the database.
    ///
    /// Capacity rejection is recorded as an explicit guarded failure and never
    /// leaves a pending marker without a durable row.
    ///
    /// # Errors
    /// Returns an error when serialization, target validation, or the atomic
    /// database transaction fails.
    pub(crate) fn enqueue_media(
        &self,
        conn: &rusqlite::Connection,
        job: &Job,
    ) -> Result<EnqueueOutcome> {
        let (post_id, expected_source, board_short) = job
            .media_target()
            .context("enqueue_media requires a media job")?;
        let payload = serialize_job_payload(job)?;
        if !self.reserve_pending_slot(job.type_str()) {
            let detail = "Background media queue is full; upload kept original file but deferred processing was skipped.";
            let active =
                crate::db::reject_media_job_at_capacity(conn, post_id, expected_source, detail)
                    .context("Persist media queue-capacity failure")?;
            if let Some(job_id) = active {
                return Ok(EnqueueOutcome::Enqueued(job_id));
            }
            let _previous_count = self.dropped_jobs.fetch_add(1, Ordering::Relaxed);
            return Ok(EnqueueOutcome::DroppedAtCapacity);
        }

        let schedule = crate::db::persist_media_job(
            conn,
            job.type_str(),
            &payload,
            post_id,
            expected_source,
            board_short,
        );
        match schedule {
            Ok(schedule) => {
                let reservation = usize::from(!schedule.inserted);
                self.decrement_pending_by(
                    reservation.saturating_add(schedule.pending_jobs_resolved),
                );
                self.notify.notify_one();
                Ok(EnqueueOutcome::Enqueued(schedule.job_id))
            }
            Err(error) => {
                let _previous_count = self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
                error!(
                    target: "workers",
                    post_id,
                    job_type = job.type_str(),
                    error = %error,
                    "atomic media-job creation failed"
                );
                Err(error)
            }
        }
    }

    /// Number of jobs currently in "pending" state (not yet started).
    /// Used by the terminal stats display.
    #[must_use]
    pub fn pending_count(&self) -> i64 {
        i64::try_from(self.pending_jobs.load(Ordering::Relaxed)).unwrap_or(i64::MAX)
    }

    /// Returns the number of jobs rejected since this queue was created.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_jobs.load(Ordering::Relaxed)
    }

    /// Returns the number of video transcodes currently executing.
    #[must_use]
    pub fn active_video_count(&self) -> u64 {
        self.active_video_jobs.load(Ordering::Relaxed)
    }

    /// Refresh queue accounting and wake a worker after a thread transaction
    /// has already committed its required prune intent.
    ///
    /// The durable row is authoritative: even if this diagnostic refresh
    /// fails, workers discover the intent through their periodic database poll.
    ///
    /// # Errors
    /// Returns an error if the persisted pending-job count cannot be read.
    pub fn notify_persisted_thread_prune(&self, conn: &rusqlite::Connection) -> Result<()> {
        let pending = crate::db::pending_job_count(conn)?;
        self.pending_jobs.store(
            u64::try_from(pending).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.notify.notify_one();
        Ok(())
    }

    /// Atomically reserves capacity for a job before it is persisted.
    fn reserve_pending_slot(&self, job_type: &str) -> bool {
        if CONFIG.job_queue_capacity == 0 {
            let _previous_count = self.pending_jobs.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        loop {
            let pending = self.pending_jobs.load(Ordering::Relaxed);
            if pending >= CONFIG.job_queue_capacity {
                warn!(
                    "Job queue at capacity ({}/{}) — dropping {} job",
                    pending, CONFIG.job_queue_capacity, job_type,
                );
                return false;
            }
            if self
                .pending_jobs
                .compare_exchange(
                    pending,
                    pending.saturating_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Decrements the pending-job gauge after a worker claims a job.
    fn mark_job_claimed(&self) {
        self.decrement_pending_by(1);
    }

    /// Saturating queue-accounting decrement used after claims and coalescing.
    fn decrement_pending_by(&self, count: usize) {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        let _previous_count =
            self.pending_jobs
                .update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                    pending.saturating_sub(count)
                });
    }

    /// Restores the pending-job gauge when a failed job is scheduled to retry.
    fn mark_job_failure_state(&self, failure_state: crate::db::JobFailureState) {
        if failure_state == crate::db::JobFailureState::Retrying {
            let _previous_count = self.pending_jobs.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// Worker pool startup
/// Spawn the background worker pool. Call exactly once at server startup.
///
/// Returns a vec of `JoinHandles`, one per worker, so the caller can await all
/// of them during graceful shutdown after cancelling `queue.cancel`.  Without
/// holding these handles the caller has no way to know when in-progress jobs
/// have actually finished — the process could exit mid-transcode, leaving DB
/// rows permanently stuck in `"running"` state and partially-written files on
/// disk.
///
/// Typical shutdown sequence:
///   `queue.cancel.cancel()`;
///   `for h in handles { h.await.ok(); }`.
///
/// Workers are pure async Tokio tasks — they do not consume OS threads at rest.
pub fn start_worker_pool(
    queue: &Arc<JobQueue>,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
) -> Vec<tokio::task::JoinHandle<()>> {
    let n = std::thread::available_parallelism()
        .map_or(2, NonZero::get)
        .min(4); // cap at 4 to avoid overwhelming SQLite's write lock

    tracing::info!(target: "workers", count = n, "Background worker pool online");

    (0..n)
        .map(|idx| {
            let q = Arc::clone(queue);
            tokio::spawn(async move {
                worker_loop(idx, q, ffmpeg_available, ffmpeg_vp9_available).await;
            })
        })
        .collect()
}

// Worker loop
/// Outcome of executing a claimed job before its status is persisted.
#[derive(Clone)]
enum JobCompletion {
    /// The job completed successfully.
    Done,
    /// The job no longer targets the post's current media.
    Stale,
    /// The job failed with the contained diagnostic message.
    Failed(String),
}

/// Outcome of executing external work before the generic completion path.
#[derive(Debug)]
enum JobExecution {
    /// The worker still needs to commit the job's terminal state.
    NeedsCompletion,
    /// Media attachment and job completion already committed atomically.
    Persisted,
    /// The job was a duplicate or otherwise stale before external mutation.
    Stale,
}

#[derive(Clone)]
/// Durable identity used to guard media completion and failure transitions.
struct MediaJobIdentity {
    /// Post targeted by the durable job.
    post_id: i64,
    /// Stable background-job type discriminator.
    job_type: &'static str,
    /// Source path captured when this generation was scheduled.
    expected_source: String,
}

/// Resolution selected after completion persistence itself fails.
enum CompletionRecovery {
    /// The job reached a known database state.
    Resolved(Option<crate::db::JobFailureState>),
    /// A transient recovery failure permits another persistence attempt.
    Retry,
}

/// Maximum time spent persisting completion after production shutdown begins.
const COMPLETION_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Persists one job-completion attempt in an immediate transaction.
fn persist_job_completion_once(
    pool: &DbPool,
    job_id: i64,
    media_identity: Option<&MediaJobIdentity>,
    completion: &JobCompletion,
) -> Result<Option<crate::db::JobFailureState>> {
    let conn = pool.get().map_err(anyhow::Error::from)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin background-job completion transaction")?;

    let result: Result<Option<crate::db::JobFailureState>> = (|| match completion {
        JobCompletion::Done => {
            if let Some(identity) = media_identity {
                crate::db::complete_media_job_without_output_in_tx(
                    &conn,
                    job_id,
                    identity.job_type,
                    identity.post_id,
                    &identity.expected_source,
                )?;
            } else {
                crate::db::complete_job(&conn, job_id)?;
            }
            Ok(None)
        }
        JobCompletion::Stale => {
            // A stale media job can refer to an older file while a newer job is
            // pending for the same post. Completing the obsolete job must not
            // clear the newer job's processing state.
            crate::db::complete_stale_media_job_in_tx(&conn, job_id)?;
            Ok(None)
        }
        JobCompletion::Failed(error) => {
            if let Some(identity) = media_identity {
                crate::db::fail_media_job_in_tx(
                    &conn,
                    job_id,
                    identity.job_type,
                    identity.post_id,
                    &identity.expected_source,
                    error,
                )
            } else {
                Ok(Some(crate::db::fail_job(&conn, job_id, error)?))
            }
        }
    })();

    match result {
        Ok(failure_state) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit background-job completion");
            }
            Ok(failure_state)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Returns whether a completion error is safe to retry without recovery.
fn completion_error_is_transient(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|sqlite_error| {
                matches!(
                    sqlite_error,
                    rusqlite::Error::SqliteFailure(code, _)
                        if matches!(
                            code.code,
                            rusqlite::ErrorCode::DatabaseBusy
                                | rusqlite::ErrorCode::DatabaseLocked
                        )
                )
            })
            || cause
                .downcast_ref::<r2d2::Error>()
                .is_some_and(|pool_error| {
                    let message = pool_error.to_string();
                    message.contains("timed out") || message.contains("Timeout")
                })
    })
}

/// Attempts once to place a job in a safe state after completion persistence fails.
fn recover_job_after_completion_error_once(
    pool: &DbPool,
    job_id: i64,
    media_identity: Option<&MediaJobIdentity>,
    completion: &JobCompletion,
    completion_error: &str,
) -> Result<Option<crate::db::JobFailureState>> {
    let conn = pool.get().map_err(anyhow::Error::from)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin background-job recovery transaction")?;
    let recovery_message: String = match completion {
        JobCompletion::Done => format!(
            "Work completed, but completion persistence failed; refusing to replay: \
             {completion_error}"
        ),
        JobCompletion::Stale => format!(
            "Stale work was discarded, but completion persistence failed; refusing to replay: \
             {completion_error}"
        ),
        JobCompletion::Failed(job_error) => {
            format!("{job_error}; completion persistence failed: {completion_error}")
        }
    }
    .chars()
    .take(512)
    .collect();
    let result: Result<Option<crate::db::JobFailureState>> = (|| {
        let status = conn
            .query_row(
                "SELECT status FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if status.as_deref() != Some("running") {
            return Ok(None);
        }

        let failure_state = match completion {
            JobCompletion::Done | JobCompletion::Stale => {
                // The external work has already succeeded (or was proven stale).
                // Re-queueing it could repeat a non-idempotent mutation, so retain
                // the attempt count and force a safe terminal state.
                let _failed_jobs = conn.execute(
                    "UPDATE background_jobs
                     SET status = 'failed',
                         last_error = ?2,
                         updated_at = unixepoch()
                     WHERE id = ?1 AND status = 'running'",
                    rusqlite::params![job_id, recovery_message],
                )?;
                if matches!(completion, JobCompletion::Done) {
                    if let Some(identity) = media_identity {
                        let _completed_posts = conn.execute(
                            "UPDATE posts
                             SET media_processing_state = '', media_processing_error = NULL
                             WHERE id = ?1 AND file_path = ?2
                               AND media_processing_state IN (?3, 'running')",
                            rusqlite::params![
                                identity.post_id,
                                identity.expected_source,
                                crate::db::MEDIA_PROCESSING_PENDING
                            ],
                        )?;
                    }
                }
                crate::db::JobFailureState::PermanentlyFailed
            }
            JobCompletion::Failed(_) => {
                // The work itself failed, so the normal bounded retry policy is
                // still safe. fail_job intentionally preserves the attempt count.
                if let Some(identity) = media_identity {
                    crate::db::fail_media_job_in_tx(
                        &conn,
                        job_id,
                        identity.job_type,
                        identity.post_id,
                        &identity.expected_source,
                        &recovery_message,
                    )?
                    .unwrap_or(crate::db::JobFailureState::PermanentlyFailed)
                } else {
                    crate::db::fail_job(&conn, job_id, &recovery_message)?
                }
            }
        };
        Ok(Some(failure_state))
    })();

    match result {
        Ok(failure_state) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit background-job recovery");
            }
            Ok(failure_state)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Computes the bounded delay between completion-persistence attempts.
fn completion_retry_delay(consecutive_errors: u32) -> Duration {
    let exponent = consecutive_errors.min(5);
    Duration::from_millis(50_u64.saturating_mul(1_u64 << exponent).min(1_000))
}

/// Maps a successful recovery query to the worker's next action.
fn resolved_completion_recovery(
    worker_id: usize,
    job_id: i64,
    failure_state: Option<crate::db::JobFailureState>,
) -> CompletionRecovery {
    match failure_state {
        Some(crate::db::JobFailureState::Retrying) => {
            warn!("Worker {worker_id}: retained job #{job_id} for a bounded retry after its work failed");
            CompletionRecovery::Resolved(Some(crate::db::JobFailureState::Retrying))
        }
        Some(crate::db::JobFailureState::PermanentlyFailed) => {
            warn!(
                "Worker {worker_id}: moved job #{job_id} to a safe terminal state after a non-transient completion error"
            );
            CompletionRecovery::Resolved(Some(crate::db::JobFailureState::PermanentlyFailed))
        }
        None => {
            warn!("Worker {worker_id}: job #{job_id} was already resolved while persisting completion");
            CompletionRecovery::Resolved(None)
        }
    }
}

/// Classifies an unsuccessful recovery attempt as retryable or terminal.
fn failed_completion_recovery(
    worker_id: usize,
    job_id: i64,
    original_error: &str,
    error: anyhow::Error,
) -> Result<CompletionRecovery> {
    if completion_error_is_transient(&error) {
        error!("Worker {worker_id}: temporary error recovering job #{job_id}: {error}");
        return Ok(CompletionRecovery::Retry);
    }
    Err(error).with_context(|| {
        format!("Could not return job {job_id} to a recoverable state after: {original_error}")
    })
}

/// Recovers a job asynchronously after a non-transient completion error.
async fn recover_non_transient_completion_error(
    worker_id: usize,
    pool: &DbPool,
    job_id: i64,
    media_identity: Option<MediaJobIdentity>,
    completion: &JobCompletion,
    completion_error: anyhow::Error,
) -> Result<CompletionRecovery> {
    let recovery_pool = pool.clone();
    let recovery_completion = completion.clone();
    let recovery_identity = media_identity.clone();
    let original_error = completion_error.to_string();
    let recovery_error = original_error.clone();
    let recovery_result = tokio::task::spawn_blocking(move || {
        recover_job_after_completion_error_once(
            &recovery_pool,
            job_id,
            recovery_identity.as_ref(),
            &recovery_completion,
            &recovery_error,
        )
    })
    .await
    .map_err(|join_error| {
        anyhow::anyhow!("Worker {worker_id}: recovery task for job #{job_id} failed: {join_error}")
    })
    .and_then(std::convert::identity);

    match recovery_result {
        Ok(failure_state) => Ok(resolved_completion_recovery(
            worker_id,
            job_id,
            failure_state,
        )),
        Err(error) => failed_completion_recovery(worker_id, job_id, &original_error, error),
    }
}

/// Persists a job completion, retrying transient database failures until resolved.
async fn persist_job_completion(
    worker_id: usize,
    pool: DbPool,
    job_id: i64,
    media_identity: Option<MediaJobIdentity>,
    completion: JobCompletion,
    cancel: CancellationToken,
) -> Result<Option<crate::db::JobFailureState>> {
    let mut consecutive_errors = 0_u32;
    let mut shutdown_deadline = cancel
        .is_cancelled()
        .then(|| {
            tokio::time::Instant::now()
                .checked_add(COMPLETION_SHUTDOWN_GRACE)
                .context("completion shutdown deadline overflow")
        })
        .transpose()?;
    loop {
        let attempt_pool = pool.clone();
        let attempt_completion = completion.clone();
        let attempt_identity = media_identity.clone();
        let result = tokio::task::spawn_blocking(move || {
            persist_job_completion_once(
                &attempt_pool,
                job_id,
                attempt_identity.as_ref(),
                &attempt_completion,
            )
        })
        .await;

        let error = match result {
            Ok(Ok(failure_state)) => return Ok(failure_state),
            Ok(Err(error)) => error,
            Err(join_error) => anyhow::anyhow!(
                "Worker {worker_id}: completion task for job #{job_id} failed: {join_error}"
            ),
        };
        error!("Worker {worker_id}: failed to persist completion for job #{job_id}: {error}");

        if !completion_error_is_transient(&error) {
            match recover_non_transient_completion_error(
                worker_id,
                &pool,
                job_id,
                media_identity.clone(),
                &completion,
                error,
            )
            .await?
            {
                CompletionRecovery::Resolved(failure_state) => return Ok(failure_state),
                CompletionRecovery::Retry => {}
            }
        }

        if shutdown_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            anyhow::bail!(
                "Timed out persisting completion for job {job_id} during graceful shutdown"
            );
        }

        let delay = completion_retry_delay(consecutive_errors);
        consecutive_errors = consecutive_errors.saturating_add(1);
        if shutdown_deadline.is_some() {
            sleep(delay).await;
        } else {
            tokio::select! {
                () = sleep(delay) => {}
                () = cancel.cancelled() => {
                    shutdown_deadline =
                        Some(tokio::time::Instant::now().checked_add(COMPLETION_SHUTDOWN_GRACE).context("completion shutdown deadline overflow")?);
                }
            }
        }
    }
}

#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "claim, execution, completion persistence, retry, and shutdown form one worker state machine"
)]
/// Claims and executes jobs until the queue's cancellation token is set.
async fn worker_loop(
    id: usize,
    queue: Arc<JobQueue>,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
) {
    debug!("Worker {id} started");
    // Track consecutive errors for exponential back-off.
    // Reset to 0 whenever a job is successfully claimed or the queue is empty.
    let mut consecutive_errors: u32 = 0;
    loop {
        // Check for shutdown before trying to claim a job.
        if queue.cancel.is_cancelled() {
            debug!("Worker {id} exiting: shutdown requested");
            return;
        }

        // Atomically claim the next pending job (UPDATE … RETURNING).
        let pool_claim = queue.pool.clone();
        let claim = tokio::task::spawn_blocking(move || {
            let c = pool_claim.get().map_err(anyhow::Error::from)?;
            crate::db::claim_next_job(&c)
        })
        .await;

        match claim {
            Ok(Ok(Some((job_id, payload)))) => {
                queue.mark_job_claimed();
                consecutive_errors = 0; // reset back-off on any successful claim
                debug!("Worker {id}: picked up job #{job_id}");
                let job: Job = match serde_json::from_str(&payload) {
                    Ok(job) => job,
                    Err(error) => {
                        error!("Worker {id}: cannot deserialize job #{job_id}: {error}");
                        let err_msg = format!("Cannot deserialize job payload: {error}");
                        match persist_job_completion(
                            id,
                            queue.pool.clone(),
                            job_id,
                            None,
                            JobCompletion::Failed(err_msg),
                            queue.cancel.clone(),
                        )
                        .await
                        {
                            Ok(Some(failure_state)) => {
                                queue.mark_job_failure_state(failure_state);
                            }
                            Ok(None) => {}
                            Err(completion_error) => {
                                error!(
                                    "Worker {id}: could not resolve broken job #{job_id}: {completion_error}"
                                );
                                return;
                            }
                        }
                        continue;
                    }
                };
                let media_identity = media_job_identity(&job);
                let result = handle_job(
                    job_id,
                    job,
                    ffmpeg_available,
                    ffmpeg_vp9_available,
                    queue.pool.clone(),
                    Arc::clone(&queue.in_progress),
                    Arc::clone(&queue.active_video_jobs),
                    queue.cancel.clone(),
                )
                .await;
                let completion = match result {
                    Ok(JobExecution::Persisted) => continue,
                    Ok(JobExecution::NeedsCompletion) => JobCompletion::Done,
                    Ok(JobExecution::Stale) => JobCompletion::Stale,
                    Err(error) if crate::db::is_stale_media_target_error(&error) => {
                        warn!("Worker {id}: job #{job_id} target is stale — {error}");
                        JobCompletion::Stale
                    }
                    Err(error) => {
                        warn!("Worker {id}: job #{job_id} failed — {error}");
                        JobCompletion::Failed(error.to_string())
                    }
                };
                let completion_result = persist_job_completion(
                    id,
                    queue.pool.clone(),
                    job_id,
                    media_identity,
                    completion,
                    queue.cancel.clone(),
                )
                .await;
                match completion_result {
                    Ok(Some(failure_state)) => {
                        queue.mark_job_failure_state(failure_state);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        error!(
                            "Worker {id}: could not resolve completion for job #{job_id}: {error}"
                        );
                        return;
                    }
                }
            }
            Ok(Ok(None)) => {
                consecutive_errors = 0; // queue empty is not an error
                                        // Queue is empty — sleep until notified, POLL_INTERVAL elapses,
                                        // or a shutdown is requested (#7).
                tokio::select! {
                    () = queue.notify.notified() => {}
                    () = sleep(POLL_INTERVAL) => {}
                    () = queue.cancel.cancelled() => {
                        debug!("Worker {id} exiting: shutdown requested while idle");
                        return;
                    }
                }
            }
            Ok(Err(e)) => {
                error!("Worker {id}: DB error while claiming job: {e}");
                let delay = backoff_duration(consecutive_errors);
                consecutive_errors = consecutive_errors.saturating_add(1);
                tokio::select! {
                    () = sleep(delay) => {}
                    () = queue.cancel.cancelled() => { return; }
                }
            }
            Err(e) => {
                error!("Worker {id}: panic in spawn_blocking: {e}");
                let delay = backoff_duration(consecutive_errors);
                consecutive_errors = consecutive_errors.saturating_add(1);
                tokio::select! {
                    () = sleep(delay) => {}
                    () = queue.cancel.cancelled() => { return; }
                }
            }
        }
    }
}

/// Compute exponential back-off with random jitter.
///
/// Base: 500 ms × 2^n, capped at 60 s.
/// Jitter: uniform random 0–500 ms added to spread simultaneous retries
/// across all workers so they do not storm the DB at the same instant.
fn backoff_duration(consecutive_errors: u32) -> Duration {
    const BASE_MS: u64 = 500;
    const MAX_MS: u64 = 60_000;
    const JITTER_MAX_MS: u64 = 500;

    let exp = consecutive_errors.min(7); // 2^7 = 128 → 64 s before cap
    let base = BASE_MS.saturating_mul(1u64 << exp).min(MAX_MS);
    let jitter = u64::from(crate::utils::crypto::os_random_u32_or_exit(
        "computing worker retry jitter",
    )) % JITTER_MAX_MS;
    Duration::from_millis(base.saturating_add(jitter))
}

// Job dispatch
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_arguments,
    reason = "the dispatcher mirrors the closed set of background job variants"
)]
/// Dispatches a decoded job to the corresponding worker operation.
async fn handle_job(
    job_id: i64,
    job: Job,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
    pool: DbPool,
    in_progress: Arc<DashMap<String, bool>>,
    active_video_jobs: Arc<AtomicU64>,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    debug!("Dispatching {} job", job.type_str());

    match job {
        Job::VideoTranscode {
            post_id,
            file_path,
            board_short,
        } => {
            // Avoid scheduling the same source path twice. The insert result
            // makes the claim atomic against a concurrently dispatched job.
            if in_progress.insert(file_path.clone(), true).is_some() {
                warn!(
                    "VideoTranscode: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(JobExecution::Stale);
            }
            let _active_before_start = active_video_jobs.fetch_add(1, Ordering::Relaxed);
            let result = transcode_video(
                job_id,
                post_id,
                file_path.clone(),
                board_short,
                ffmpeg_available,
                ffmpeg_vp9_available,
                pool,
                cancel,
            )
            .await;
            let _active_before_completion = active_video_jobs.fetch_sub(1, Ordering::Relaxed);
            let _removed_entry = in_progress.remove(&file_path);
            result
        }

        Job::AudioWaveform {
            post_id,
            file_path,
            board_short,
        } => {
            // Avoid scheduling the same source path twice. The insert result
            // makes the claim atomic against a concurrently dispatched job.
            if in_progress.insert(file_path.clone(), true).is_some() {
                warn!(
                    "AudioWaveform: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(JobExecution::Stale);
            }
            let result = generate_waveform(
                job_id,
                post_id,
                file_path.clone(),
                board_short,
                ffmpeg_available,
                pool,
                cancel,
            )
            .await;
            let _removed_entry = in_progress.remove(&file_path);
            result
        }

        Job::ThreadPrune { board_id } => {
            let result = prune_threads(board_id, pool).await;
            if let Err(error) = result.as_ref() {
                warn!(
                    target: "workers",
                    board_id,
                    error = %error,
                    "board thread-prune evaluation failed and will follow durable retry policy"
                );
            }
            result.map(|()| JobExecution::NeedsCompletion)
        }

        Job::SpamCheck {
            post_id,
            ip_hash,
            body_len,
        } => {
            run_spam_check(post_id, &ip_hash, body_len);
            Ok(JobExecution::NeedsCompletion)
        }
    }
}

/// Returns the post identifier whose media state follows this job, if any.
fn media_job_identity(job: &Job) -> Option<MediaJobIdentity> {
    match job {
        Job::VideoTranscode {
            post_id,
            file_path,
            board_short: _,
        } => Some(MediaJobIdentity {
            post_id: *post_id,
            job_type: "video_transcode",
            expected_source: file_path.clone(),
        }),
        Job::AudioWaveform {
            post_id,
            file_path,
            board_short: _,
        } => Some(MediaJobIdentity {
            post_id: *post_id,
            job_type: "audio_waveform",
            expected_source: file_path.clone(),
        }),
        Job::ThreadPrune { board_id: _ }
        | Job::SpamCheck {
            post_id: _,
            ip_hash: _,
            body_len: _,
        } => None,
    }
}

// VideoTranscode
/// Convert video to VP9/Opus `WebM`, then atomically update media metadata.
/// Compatible VP8/VP9 `WebM` is retained; source cleanup is journaled on success.
///
/// Requires both `ffmpeg_available` (binary present) and `ffmpeg_vp9_available`
/// (libvpx-vp9 + libopus and the `WebM` muxer compiled in). If either is false,
/// the job is skipped gracefully — no error is returned and the file remains as-is.
///
/// The command runs in its own process group. Timeout, cancellation, and scope
/// cleanup terminate that whole group so descendants cannot outlive the job.
///
/// A hard timeout of the live `ffmpeg_timeout_secs` setting is applied.
#[expect(
    clippy::too_many_arguments,
    reason = "transcode process control and fail-closed cleanup share one lifecycle"
)]
async fn transcode_video(
    job_id: i64,
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
    pool: DbPool,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    if !ffmpeg_available {
        debug!("VideoTranscode skipped for post {post_id}: ffmpeg not available");
        return Ok(JobExecution::NeedsCompletion);
    }

    if !ffmpeg_vp9_available {
        warn!(
            "VideoTranscode skipped for post {post_id}: libvpx-vp9, libopus or the WebM muxer not available. \
             Install ffmpeg with VP9 + Opus support to enable MP4/MKV→WebM transcoding."
        );
        return Ok(JobExecution::NeedsCompletion);
    }

    let timeout_secs = crate::config::ffmpeg_timeout_secs();
    // File checks, codec probing, and temporary-file creation are blocking.
    let prepare_result = {
        let file_path2 = file_path.clone();
        let board_short2 = board_short.clone();
        tokio::task::spawn_blocking(move || {
            transcode_video_prepare(post_id, &file_path2, &board_short2)
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in prepare: {e}"))?
    }?;

    let Some((args, src_path, webm_abs, webm_rel, _webm_name, tmp)) = prepare_result else {
        return Ok(JobExecution::NeedsCompletion);
    };

    let mut command = ffmpeg_command();
    let _configured_command = command.args(&args);
    run_video_transcode(command, post_id, timeout_secs, cancel).await?;

    // File persistence and database updates are blocking.
    let finalise_result = tokio::task::spawn_blocking(move || {
        transcode_video_finalise(
            job_id, post_id, &file_path, &src_path, &webm_abs, &webm_rel, tmp, &pool,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in finalise: {e}"))?;
    finalise_result
}

/// Run the bounded video process; only a successful exit permits finalisation.
async fn run_video_transcode(
    mut command: TokioCommand,
    post_id: i64,
    timeout_secs: u64,
    cancel: CancellationToken,
) -> Result<()> {
    let _configured_command = command
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg '{}': {e}", CONFIG.ffmpeg_path))?;
    drop(command);

    let ffmpeg_timeout = Duration::from_secs(timeout_secs);
    match wait_for_ffmpeg_output(child, ffmpeg_timeout, cancel).await? {
        AsyncWaitOutcome::Exited(output) => {
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "ffmpeg exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        AsyncWaitOutcome::TimedOut => {
            warn!("{}", video_reencode_timeout_warning(post_id, timeout_secs));
            return Err(anyhow::anyhow!(
                "ffmpeg transcode timed out after {timeout_secs}s"
            ));
        }
        AsyncWaitOutcome::Cancelled => {
            return Err(anyhow::anyhow!(
                "ffmpeg transcode cancelled during shutdown"
            ));
        }
    }

    Ok(())
}

/// Prepare a video transcode: validate the source file, inspect container and
/// stream metadata, create a temp output file, and return the ffmpeg
/// argument list along with the relevant paths.
///
/// Returns `Ok(None)` for compatible VP8/VP9 `WebM`. Returns `Ok(Some(...))` when
/// ffmpeg should be invoked. `Err` for genuine failures.
///
/// This is a pure synchronous function; call it from `spawn_blocking` or at
/// startup where blocking is acceptable.
/// Parts returned by [`transcode_video_prepare`] when a transcode should proceed.
type TranscodePrepareParts = (
    Vec<String>,
    PathBuf,
    PathBuf,
    String,
    String,
    tempfile::NamedTempFile,
);

/// Validates and prepares all paths and arguments needed for a video transcode.
fn transcode_video_prepare(
    post_id: i64,
    file_path: &str,
    board_short: &str,
) -> Result<Option<TranscodePrepareParts>> {
    let upload_root = PathBuf::from(&CONFIG.upload_dir);
    let board_dir = validated_board_media_dir(&upload_root, board_short)?;
    let src = validated_board_media_file(&upload_root, file_path, board_short)?;

    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let info = crate::media::probe::inspect_video(&src)
        .context("VideoTranscode: failed to inspect source container/streams")?;
    if !info.needs_webm_conversion() {
        debug!("VideoTranscode: preserving compatible VP8/VP9 WebM for post {post_id}");
        return Ok(None);
    }
    if info.codecs.first() == Some(&"av1")
        && crate::media::ffmpeg::video_capabilities()
            .av1
            .decoders
            .is_empty()
    {
        anyhow::bail!("VideoTranscode: installed FFmpeg has no AV1 decoder");
    }

    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Malformed filename: {}", src.display()))?
        .to_owned();

    tracing::info!(target: "workers", post_id = post_id, file = %file_path, "VideoTranscode: starting");

    let webm_name = transcoded_webm_name(&stem, &ext);
    let webm_abs = board_dir.join(&webm_name);
    let webm_rel = format!("{board_short}/{webm_name}");
    let _validated_path =
        crate::utils::fs_security::canonical_parent_for_new_child(&upload_root, &webm_abs)
            .context("Transcode destination failed safety validation")?;

    // temp file in the same directory for POSIX-atomic rename.
    let tmp = tempfile::Builder::new()
        .suffix(".webm")
        .tempfile_in(&board_dir)
        .map_err(|e| anyhow::anyhow!("Failed to create temp file for WebM output: {e}"))?;

    let tmp_path_str = tmp
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Temp file path is non-UTF-8"))?
        .to_owned();
    let src_path_str = src
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Source path is non-UTF-8"))?
        .to_owned();

    // Build the ffmpeg argument list as owned strings so it can cross the
    // async boundary without lifetime issues.
    let args = crate::media::ffmpeg::build_vp9_transcode_args(&src_path_str, &tmp_path_str);

    Ok(Some((args, src, webm_abs, webm_rel, webm_name, tmp)))
}

/// Resolves a board's media directory and rejects symlinks or traversal.
fn validated_board_media_dir(upload_root: &std::path::Path, board_short: &str) -> Result<PathBuf> {
    let board_component = single_normal_component(board_short)
        .ok_or_else(|| anyhow::anyhow!("Transcode board name contains unsafe path components"))?;
    let board_dir = upload_root.join(board_component);
    let _validated_path = crate::utils::fs_security::canonical_child_of(upload_root, &board_dir)
        .context("Transcode board directory failed safety validation")?;
    crate::utils::fs_security::assert_dir_no_symlink(&board_dir)
        .context("Transcode board directory is not safe")?;
    Ok(board_dir)
}

/// Resolves an immediate board media file and verifies it is a safe regular file.
fn validated_board_media_file(
    upload_root: &std::path::Path,
    file_path: &str,
    board_short: &str,
) -> Result<PathBuf> {
    let relative = std::path::Path::new(file_path);
    if relative.is_absolute() {
        anyhow::bail!("Transcode source path must be relative");
    }

    let mut components = relative.components();
    let board_component = components
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            std::path::Component::CurDir
            | std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => None,
        })
        .ok_or_else(|| anyhow::anyhow!("Transcode source path is missing a board component"))?;
    if board_component != board_short {
        anyhow::bail!("Transcode source path does not belong to its board");
    }
    let filename = components
        .next()
        .and_then(|component| match component {
            std::path::Component::Normal(value) => Some(value),
            std::path::Component::CurDir
            | std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => None,
        })
        .ok_or_else(|| anyhow::anyhow!("Transcode source path is missing a file name"))?;
    if components.next().is_some() {
        anyhow::bail!("Transcode source path must be an immediate board media file");
    }

    let src = upload_root.join(board_short).join(filename);
    let src = crate::utils::fs_security::canonical_child_of(upload_root, &src)
        .context("Transcode source failed safety validation")?;
    crate::utils::fs_security::assert_regular_file_no_symlink(&src)
        .context("Transcode source is not a safe regular file")?;
    Ok(src)
}

/// Returns `value` as one normal path component, rejecting all other forms.
fn single_normal_component(value: &str) -> Option<&std::ffi::OsStr> {
    let mut components = std::path::Path::new(value).components();
    let component = match components.next()? {
        std::path::Component::Normal(component) => component,
        std::path::Component::CurDir
        | std::path::Component::ParentDir
        | std::path::Component::RootDir
        | std::path::Component::Prefix(_) => return None,
    };
    components.next().is_none().then_some(component)
}

/// Builds a collision-free `WebM` filename for the transcoded output.
fn transcoded_webm_name(stem: &str, source_ext: &str) -> String {
    if source_ext.eq_ignore_ascii_case("webm") {
        format!("{stem}.vp9.webm")
    } else {
        format!("{stem}.webm")
    }
}

/// Finalise a completed video transcode: persist the temp file atomically,
/// read the result for dedup, and update the database.
///
/// On any DB error the temp-turned-final `WebM` is removed to prevent leaks.
#[expect(
    clippy::too_many_arguments,
    reason = "the finalisation boundary carries the durable job and all validated filesystem identities"
)]
fn transcode_video_finalise(
    job_id: i64,
    post_id: i64,
    file_path: &str,
    src: &std::path::Path,
    webm_abs: &std::path::Path,
    webm_rel: &str,
    tmp: tempfile::NamedTempFile,
    pool: &DbPool,
) -> Result<JobExecution> {
    match validate_transcoded_webm_output(src, tmp.path()) {
        Ok(TranscodeOutputDecision::Accept) => {}
        Ok(TranscodeOutputDecision::Skip { reason }) => {
            tracing::warn!(
                target: "workers",
                post_id,
                reason,
                "VideoTranscode: keeping original media"
            );
            return Ok(JobExecution::NeedsCompletion);
        }
        Err(error) => {
            return Err(error)
                .context("VideoTranscode: output validation failed; original retained")
        }
    }

    persist_transcoded_webm(
        job_id, post_id, file_path, src, webm_abs, webm_rel, tmp, pool,
    )
}

/// Atomically installs a validated transcode and replaces the post media record.
#[expect(
    clippy::too_many_arguments,
    reason = "output installation and atomic DB completion require the full validated transcode identity"
)]
fn persist_transcoded_webm(
    job_id: i64,
    post_id: i64,
    file_path: &str,
    src: &std::path::Path,
    webm_abs: &std::path::Path,
    webm_rel: &str,
    tmp: tempfile::NamedTempFile,
    pool: &DbPool,
) -> Result<JobExecution> {
    let installed_new = install_deterministic_output(tmp, webm_abs)?;
    let persist_result: Result<u64> = (|| {
        let webm_sha256 = sha256_file_hex(webm_abs)?;
        let webm_bytes = std::fs::metadata(webm_abs)
            .with_context(|| format!("Failed to stat transcoded WebM {}", webm_abs.display()))?
            .len();
        let persisted_size =
            i64::try_from(webm_bytes).context("Transcoded WebM size exceeds database range")?;
        let conn = pool.get()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .context("Failed to begin atomic video-job completion")?;
        let finish = crate::db::finish_video_media_job_in_tx(
            &conn,
            job_id,
            post_id,
            file_path,
            webm_rel,
            "video/webm",
            &webm_sha256,
            persisted_size,
        );
        let finish = match finish {
            Ok(finish) => {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    drop(conn.execute_batch("ROLLBACK"));
                    return Err(error).context("Failed to commit atomic video-job completion");
                }
                finish
            }
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error);
            }
        };
        finalize_media_cleanup(
            &conn,
            &CONFIG.upload_dir,
            &finish.cleanup,
            post_id,
            job_id,
            "video_transcode",
        );
        Ok(webm_bytes)
    })();

    let webm_bytes = match persist_result {
        Ok(webm_bytes) => webm_bytes,
        Err(error) => {
            if installed_new {
                if let Err(cleanup_error) = std::fs::remove_file(webm_abs) {
                    warn!(
                        output = %webm_abs.display(),
                        error = %cleanup_error,
                        "VideoTranscode: failed to remove unattached WebM output"
                    );
                }
            }
            return Err(error);
        }
    };

    tracing::info!(target: "workers", post_id = post_id, source = %src.display(), output = %webm_rel, bytes = webm_bytes, "VideoTranscode done");
    Ok(JobExecution::Persisted)
}

/// Install a deterministic output without overwriting an existing managed file.
/// An existing byte-identical regular file is reused; mismatches, symlinks, and
/// hard links fail closed.
fn install_deterministic_output(
    tmp: tempfile::NamedTempFile,
    destination: &std::path::Path,
) -> Result<bool> {
    if destination.exists() {
        crate::utils::fs_security::assert_regular_file_no_symlink(destination)
            .context("Existing deterministic media output is unsafe")?;
        let existing_digest = sha256_file_hex(destination)?;
        let candidate_digest = sha256_file_hex(tmp.path())?;
        anyhow::ensure!(
            existing_digest == candidate_digest,
            "existing deterministic media output does not match this job"
        );
        return Ok(false);
    }
    drop(
        tmp.persist_noclobber(destination).map_err(|error| {
            anyhow::anyhow!("Failed to atomically install media output: {error}")
        })?,
    );
    crate::utils::fs_security::assert_regular_file_no_symlink(destination)
        .context("Installed deterministic media output is unsafe")?;
    Ok(true)
}

/// Replay cleanup journaled by an atomic media completion. Failures leave the
/// durable row for startup/periodic filesystem reconciliation.
fn finalize_media_cleanup(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    cleanup: &crate::db::DeletePathsResult,
    post_id: i64,
    job_id: i64,
    job_type: &str,
) {
    if cleanup.paths.is_empty() {
        return;
    }
    if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
        conn,
        upload_dir,
        cleanup.pending_fs_op_id.as_deref(),
        &cleanup.paths,
    ) {
        warn!(
            target: "workers",
            post_id,
            job_id,
            job_type,
            error = %error,
            "media completion cleanup replay failed; durable intent retained"
        );
    }
}

/// Decision produced by post-transcode validation.
enum TranscodeOutputDecision {
    /// The output is valid and should replace the original media.
    Accept,
    /// The output is validly inspectable but should not replace the original.
    Skip {
        /// Stable diagnostic explaining why the original media is retained.
        reason: &'static str,
    },
}

/// Checks a transcode's size, stream kind, and codec before persistence.
fn validate_transcoded_webm_output(
    source_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<TranscodeOutputDecision> {
    let source_size = std::fs::metadata(source_path)
        .with_context(|| format!("Failed to stat source video {}", source_path.display()))?
        .len();
    let output_size = std::fs::metadata(output_path)
        .with_context(|| format!("Failed to stat transcoded WebM {}", output_path.display()))?
        .len();
    if output_size == 0 {
        return Ok(TranscodeOutputDecision::Skip {
            reason: "transcoded output is empty",
        });
    }
    if output_size >= source_size {
        return Ok(TranscodeOutputDecision::Skip {
            reason: "transcoded output is not smaller than the original",
        });
    }

    crate::media::probe::validate_webm_output_file(output_path)?;

    Ok(TranscodeOutputDecision::Accept)
}

// AudioWaveform
/// Parts prepared for atomic waveform persistence after in-process decoding.
type WaveformPrepareParts = (PathBuf, String, PathBuf, String, tempfile::NamedTempFile);

/// Generate a Rust waveform, retaining the authorized uncovered-codec fallback.
async fn generate_waveform(
    job_id: i64,
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    let (png_abs, png_rel, src_path, expected_file_path, tmp_png) =
        tokio::task::spawn_blocking(move || waveform_prepare(&file_path, &board_short))
            .await
            .map_err(|error| anyhow::anyhow!("waveform preparation failed: {error}"))??;
    if !render_waveform_with_compatibility(&src_path, tmp_png.path(), ffmpeg_available, &cancel)
        .await?
    {
        return Ok(JobExecution::NeedsCompletion);
    }
    if cancel.is_cancelled() {
        anyhow::bail!("audio waveform cancelled before persistence");
    }
    let finalised = tokio::task::spawn_blocking(move || {
        waveform_finalise(
            job_id,
            post_id,
            &png_abs,
            &png_rel,
            &src_path,
            &expected_file_path,
            tmp_png,
            std::path::Path::new(&CONFIG.upload_dir),
            &pool,
        )
    })
    .await
    .map_err(|error| anyhow::anyhow!("waveform persistence failed: {error}"))?;
    finalised
}

/// Render common codecs internally; only unavailable codecs may invoke `FFmpeg`.
/// Both paths share the existing deadline, shutdown handling and temp output.
async fn render_waveform_with_compatibility(
    src_path: &std::path::Path,
    output_path: &std::path::Path,
    ffmpeg_available: bool,
    cancel: &CancellationToken,
) -> Result<bool> {
    let started = std::time::Instant::now();
    let source = src_path.to_path_buf();
    let decoder_output_path = output_path.to_path_buf();
    let decoder_cancel = cancel.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        crate::media::audio::render_waveform(
            &source,
            &decoder_output_path,
            CONFIG.thumb_size,
            (CONFIG.thumb_size / 2).max(1),
            &decoder_cancel,
        )
    })
    .await
    .map_err(|error| anyhow::anyhow!("waveform decoder failed: {error}"))?;
    if let Err(error) = rendered {
        if !crate::media::audio::is_unsupported(&error) {
            return Err(error);
        }
        if !ffmpeg_available {
            return Ok(false);
        }
        if cancel.is_cancelled() {
            anyhow::bail!("audio decoding cancelled before compatibility fallback");
        }
        let remaining = Duration::from_secs(crate::config::ffmpeg_timeout_secs())
            .checked_sub(started.elapsed())
            .ok_or_else(|| anyhow::anyhow!("waveform deadline exceeded"))?;
        let mut command = ffmpeg_command();
        // Debian FFmpeg's Speex decoder leaves frame layouts unspecified. Its
        // validated header permits only mono/stereo; negotiate either without
        // downmixing or changing the channel count before showwavespic.
        let layout = if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<symphonia::core::errors::Error>(),
                Some(symphonia::core::errors::Error::Unsupported(
                    "Speex requires the compatibility decoder"
                ))
            )
        }) {
            "aformat=channel_layouts=mono|stereo,"
        } else {
            ""
        };
        let filter = format!(
            "{layout}showwavespic=s={}x{}:colors=0x888888",
            CONFIG.thumb_size,
            CONFIG.thumb_size / 2
        );
        let _configured_command = command
            .args(["-loglevel", "error", "-i"])
            .arg(src_path)
            .args(["-filter_complex", &filter, "-frames:v", "1", "-y"])
            .arg(output_path)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|spawn_error| {
            anyhow::anyhow!(
                "uncovered audio codec requires FFmpeg '{}': {spawn_error}",
                CONFIG.ffmpeg_path
            )
        })?;
        drop(command);
        match wait_for_ffmpeg_output(child, remaining, cancel.clone()).await? {
            AsyncWaitOutcome::Exited(output) if output.status.success() => {}
            AsyncWaitOutcome::Exited(output) => anyhow::bail!(
                "FFmpeg audio compatibility decoder failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            AsyncWaitOutcome::TimedOut => {
                anyhow::bail!("FFmpeg audio compatibility decoder timed out")
            }
            AsyncWaitOutcome::Cancelled => {
                anyhow::bail!("audio compatibility decoding cancelled during shutdown")
            }
        }
    }
    Ok(true)
}

/// Formats the actionable warning emitted when video re-encoding times out.
fn video_reencode_timeout_warning(post_id: i64, timeout_secs: u64) -> String {
    let human_timeout = crate::config::describe_timeout_secs(timeout_secs);
    format!(
        "VideoTranscode: ffmpeg video re-encoding/conversion timed out for post {post_id} after {human_timeout} ({timeout_secs}s); slow systems such as Raspberry Pi devices may need a higher timeout. Increase the video re-encoding timeout in the admin panel or settings.toml."
    )
}

/// Blocking prepare phase for [`generate_waveform`]: validate the source,
/// and create a temp output file.
fn waveform_prepare(file_path: &str, board_short: &str) -> Result<WaveformPrepareParts> {
    use anyhow::Context as _;
    let upload_dir = &CONFIG.upload_dir;
    let upload_root = std::path::Path::new(upload_dir);
    let board_dir = validated_board_media_dir(upload_root, board_short)?;
    let src = validated_board_media_file(upload_root, file_path, board_short)?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Malformed audio filename: {}", src.display()))?
        .to_owned();
    let thumbs_dir = board_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)?;
    let png_name = format!("{stem}.png");
    let png_abs = thumbs_dir.join(&png_name);
    let png_rel = format!("{board_short}/thumbs/{png_name}");
    let tmp_png = tempfile::Builder::new()
        .prefix("chan_wav_")
        .suffix(".png")
        .tempfile_in(&thumbs_dir)
        .context("Failed to create temp file for waveform PNG")?;
    Ok((png_abs, png_rel, src, file_path.to_owned(), tmp_png))
}

/// Blocking finalise phase for [`generate_waveform`]: atomically persist the
/// temp PNG and update the database with the new thumb path.
#[expect(
    clippy::too_many_arguments,
    reason = "the finalisation boundary explicitly carries all validated paths and DB identity"
)]
fn waveform_finalise(
    job_id: i64,
    post_id: i64,
    png_abs: &std::path::Path,
    png_rel: &str,
    src_path: &std::path::Path,
    expected_file_path: &str,
    tmp_png: tempfile::NamedTempFile,
    upload_root: &std::path::Path,
    pool: &DbPool,
) -> Result<JobExecution> {
    use anyhow::Context as _;
    let installed_new = install_deterministic_output(tmp_png, png_abs)?;
    let result: Result<()> = (|| {
        let audio_sha256 = sha256_file_hex(src_path)?;
        let conn = pool.get()?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .context("Failed to begin waveform persistence transaction")?;
        let db_result = crate::db::finish_waveform_media_job_in_tx(
            &conn,
            job_id,
            post_id,
            expected_file_path,
            png_rel,
            &audio_sha256,
        );
        match db_result {
            Ok(finish) => {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    drop(conn.execute_batch("ROLLBACK"));
                    return Err(error).context("Failed to commit waveform persistence");
                }
                finalize_media_cleanup(
                    &conn,
                    upload_root.to_string_lossy().as_ref(),
                    &finish.cleanup,
                    post_id,
                    job_id,
                    "audio_waveform",
                );
                Ok(())
            }
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                Err(error)
            }
        }
    })();
    if let Err(error) = result {
        if installed_new {
            if let Err(cleanup_error) = std::fs::remove_file(png_abs) {
                warn!(
                    output = %png_abs.display(),
                    error = %cleanup_error,
                    "AudioWaveform: failed to remove unattached waveform output"
                );
            }
        }
        return Err(error);
    }

    tracing::info!(target: "workers", post_id = post_id, thumb = %png_rel, "AudioWaveform done");
    Ok(JobExecution::Persisted)
}

/// Removes one superseded SVG waveform placeholder after verifying that no
/// post or deduplication record still references it.
fn cleanup_waveform_placeholder_candidate(
    conn: &rusqlite::Connection,
    upload_root: &std::path::Path,
    placeholder_rel: &str,
    current_png_rel: &str,
) {
    let placeholder = std::path::Path::new(placeholder_rel);
    if placeholder.extension().and_then(|ext| ext.to_str()) != Some("svg")
        || placeholder.with_extension("png") != std::path::Path::new(current_png_rel)
    {
        return;
    }

    let referenced = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM posts WHERE thumb_path = ?1
                 UNION ALL
                 SELECT 1 FROM file_hashes WHERE thumb_path = ?1
             )",
            rusqlite::params![placeholder_rel],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(true);
    if referenced {
        return;
    }

    let placeholder_abs = match crate::utils::fs_security::existing_regular_file_child(
        upload_root,
        placeholder_rel,
    ) {
        Ok(path) => path,
        Err(error)
            if error
                .chain()
                .find_map(|source| source.downcast_ref::<std::io::Error>())
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return;
        }
        Err(error) => {
            warn!(
                placeholder = placeholder_rel,
                error = %error,
                "AudioWaveform: rejected unsafe obsolete placeholder path"
            );
            return;
        }
    };
    if let Err(error) = std::fs::remove_file(&placeholder_abs) {
        warn!(
            placeholder = %placeholder_abs.display(),
            error = %error,
            "AudioWaveform: failed to remove obsolete SVG placeholder"
        );
    }
}

/// Removes unreferenced SVG placeholders left by a completed waveform update
/// that was interrupted before its post-commit filesystem cleanup.
///
/// # Errors
/// Returns an error when candidate discovery cannot be queried.
pub fn cleanup_recovered_waveform_placeholders(
    conn: &rusqlite::Connection,
    upload_root: &std::path::Path,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT thumb_path
         FROM posts
         WHERE mime_type LIKE 'audio/%' AND thumb_path LIKE '%.png'",
    )?;
    let png_paths = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for png_rel in png_paths {
        let placeholder_rel = std::path::Path::new(&png_rel)
            .with_extension("svg")
            .to_string_lossy()
            .into_owned();
        cleanup_waveform_placeholder_candidate(conn, upload_root, &placeholder_rel, &png_rel);
    }
    Ok(())
}

/// Streams a file into SHA-256 and returns its lowercase hexadecimal digest.
fn sha256_file_hex(path: &std::path::Path) -> Result<String> {
    use anyhow::Context as _;
    use sha2::Digest as _;
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("failed to open {} for hashing", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf)
            .with_context(|| format!("failed to read {} for hashing", path.display()))?;
        if read == 0 {
            break;
        }
        if let Some(bytes) = buf.get(..read) {
            hasher.update(bytes);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

// ThreadPrune
/// Applies one board's live and archived thread-retention limits.
async fn prune_threads(board_id: i64, pool: DbPool) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        // Policy and both retention transitions share one write lock. A concurrent
        // board-settings edit cannot change the policy between read and deletion.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let Some(policy) = load_board_retention_policy(&tx, board_id)? else {
            warn!(target: "workers", board_id,
                "thread-prune intent targeted a missing board; resolving safely");
            return Ok(());
        };
        let do_archive = policy.allow_archive || CONFIG.archive_before_prune;
        let (count, deleted) = if do_archive {
            (
                crate::db::threads::archive_old_threads_in_tx(&tx, board_id, policy.max_threads)?,
                None,
            )
        } else {
            (
                0,
                Some(crate::db::threads::prune_old_threads_in_tx(
                    &tx,
                    board_id,
                    policy.max_threads,
                    false,
                )?),
            )
        };
        let archived = crate::db::threads::prune_old_threads_in_tx(
            &tx,
            board_id,
            policy.max_archived_threads,
            true,
        )?;
        tx.commit()?;
        if count > 0 {
            tracing::info!(target: "workers", count, board = %policy.board_short,
                board_id, "ThreadArchive: threads archived");
        }
        if let Some(deleted) = deleted {
            finalize_thread_prune_cleanup(
                &conn,
                &policy,
                &deleted,
                "thread prune cleanup did not fully complete",
            );
            if !deleted.paths.is_empty() {
                tracing::info!(target: "workers", board = %policy.board_short, board_id,
                    files_removed = deleted.paths.len(), "ThreadPrune: threads deleted");
            }
        }
        finalize_thread_prune_cleanup(
            &conn,
            &policy,
            &archived,
            "archived prune cleanup did not fully complete",
        );
        if !archived.paths.is_empty() {
            tracing::info!(target: "workers", archived_cap = policy.max_archived_threads,
                board = %policy.board_short, board_id, files_removed = archived.paths.len(),
                "ThreadArchivePrune: archived threads deleted");
        }

        let (live_eligible, archived_count) = board_retention_counts(&conn, board_id)?;
        if live_eligible > policy.max_threads || archived_count > policy.max_archived_threads {
            warn!(
                target: "workers",
                board = %policy.board_short,
                board_id,
                live_eligible,
                live_limit = policy.max_threads,
                archived_count,
                archived_limit = policy.max_archived_threads,
                "board remains over its current retention limit after prune attempt"
            );
        }
        Ok(())
    })
    .await?
}

/// Current validated policy loaded at execution time, never from job payloads.
struct BoardRetentionPolicy {
    /// Database identifier used for mutation and structured diagnostics.
    board_id: i64,
    /// Current board route name used only for structured diagnostics.
    board_short: String,
    /// Current maximum retained non-sticky live threads.
    max_threads: i64,
    /// Current maximum retained archived threads.
    max_archived_threads: i64,
    /// Current board preference for archiving live overflow.
    allow_archive: bool,
}

/// Load and validate all mutable values that can decide content retention.
fn load_board_retention_policy(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<Option<BoardRetentionPolicy>> {
    let raw = conn
        .query_row(
            "SELECT short_name, max_threads, max_archived_threads, allow_archive
             FROM boards WHERE id = ?1",
            rusqlite::params![board_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .context("Load current board retention policy failed")?;
    let Some((board_short, max_threads, max_archived_threads, allow_archive)) = raw else {
        return Ok(None);
    };
    anyhow::ensure!(
        (1..=1_000).contains(&max_threads),
        "invalid current live-thread limit for board {board_id}: {max_threads}; preserving content"
    );
    anyhow::ensure!(
        (1..=10_000).contains(&max_archived_threads),
        "invalid current archived-thread limit for board {board_id}: {max_archived_threads}; preserving content"
    );
    anyhow::ensure!(
        matches!(allow_archive, 0 | 1),
        "invalid current archive policy for board {board_id}: {allow_archive}; preserving content"
    );
    Ok(Some(BoardRetentionPolicy {
        board_id,
        board_short,
        max_threads,
        max_archived_threads,
        allow_archive: allow_archive == 1,
    }))
}

/// Count only live threads eligible for the existing sticky-preserving prune,
/// plus every archived thread subject to the archive cap.
fn board_retention_counts(conn: &rusqlite::Connection, board_id: i64) -> Result<(i64, i64)> {
    conn.query_row(
        "SELECT
             SUM(CASE WHEN archived = 0 AND sticky = 0 THEN 1 ELSE 0 END),
             SUM(CASE WHEN archived = 1 THEN 1 ELSE 0 END)
         FROM threads WHERE board_id = ?1",
        rusqlite::params![board_id],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            ))
        },
    )
    .context("Count board retention state failed")
}

/// Replay durable filesystem cleanup after the deletion transaction commits.
fn finalize_thread_prune_cleanup(
    conn: &rusqlite::Connection,
    policy: &BoardRetentionPolicy,
    deleted: &crate::db::DeletePathsResult,
    message: &'static str,
) {
    if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
        conn,
        &CONFIG.upload_dir,
        deleted.pending_fs_op_id.as_deref(),
        &deleted.paths,
    ) {
        warn!(
            target: "workers",
            board = %policy.board_short,
            board_id = policy.board_id,
            error = %error,
            "{message}"
        );
    }
}

/// Maximum boards inspected by one startup or periodic retention pass.
pub(crate) const THREAD_PRUNE_RECONCILE_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Summary of one bounded board-retention reconciliation page.
pub(crate) struct ThreadPruneReconciliation {
    /// Boards whose current retention state was inspected.
    pub scanned: usize,
    /// Over-limit boards for which durable work was newly inserted.
    pub inserted: usize,
    /// Over-limit boards already represented by pending work.
    pub coalesced: usize,
    /// Boards that could not be evaluated or scheduled.
    pub failures: usize,
    /// Cursor for the next page, or zero after reaching the end.
    pub next_board_id: i64,
}

/// Inspect a bounded page of boards and durably schedule each current policy
/// violation. Errors are isolated per board so one bad row does not block the
/// remainder of the page.
///
/// # Errors
/// Returns an error only when the database connection or page query itself
/// cannot be completed.
pub(crate) fn reconcile_thread_prune_intents(
    pool: &DbPool,
    after_board_id: i64,
    limit: usize,
) -> Result<ThreadPruneReconciliation> {
    let conn = pool
        .get()
        .context("Get DB connection for thread-prune reconciliation failed")?;
    let bounded_limit = limit.clamp(1, THREAD_PRUNE_RECONCILE_BATCH);
    let mut stmt = conn
        .prepare(
            "SELECT b.id, b.short_name, b.max_threads, b.max_archived_threads,
                    b.allow_archive,
                    (SELECT COUNT(*) FROM threads AS live
                     WHERE live.board_id = b.id AND live.archived = 0 AND live.sticky = 0),
                    (SELECT COUNT(*) FROM threads AS archived
                     WHERE archived.board_id = b.id AND archived.archived = 1)
             FROM boards AS b
             WHERE b.id > ?1
             ORDER BY b.id ASC
             LIMIT ?2",
        )
        .context("Prepare thread-prune reconciliation page failed")?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                after_board_id.max(0),
                i64::try_from(bounded_limit).unwrap_or(i64::MAX)
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .context("Query thread-prune reconciliation page failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read thread-prune reconciliation page failed")?;
    drop(stmt);

    let mut report = ThreadPruneReconciliation {
        scanned: rows.len(),
        next_board_id: rows.last().map_or(0, |row| row.0),
        ..ThreadPruneReconciliation::default()
    };
    if rows.len() < bounded_limit {
        report.next_board_id = 0;
    }

    for (
        board_id,
        board_short,
        max_threads,
        max_archived_threads,
        allow_archive,
        live_count,
        archived_count,
    ) in rows
    {
        let policy_valid = (1..=1_000).contains(&max_threads)
            && (1..=10_000).contains(&max_archived_threads)
            && matches!(allow_archive, 0 | 1);
        if !policy_valid {
            report.failures = report.failures.saturating_add(1);
            warn!(
                target: "workers",
                board = %board_short,
                board_id,
                max_threads,
                max_archived_threads,
                allow_archive,
                "thread-prune reconciliation found invalid policy; preserving content"
            );
            continue;
        }
        if live_count <= max_threads && archived_count <= max_archived_threads {
            continue;
        }

        let schedule_result = crate::db::persist_thread_prune_intent(&conn, board_id);
        match schedule_result {
            Ok(schedule) => {
                log_thread_prune_schedule(board_id, schedule);
                if schedule.inserted {
                    report.inserted = report.inserted.saturating_add(1);
                } else {
                    report.coalesced = report.coalesced.saturating_add(1);
                }
            }
            Err(error) => {
                report.failures = report.failures.saturating_add(1);
                error!(
                    target: "workers",
                    board = %board_short,
                    board_id,
                    error = %error,
                    "failed to persist or coalesce reconciled board prune intent"
                );
            }
        }
    }
    Ok(report)
}

/// Normalize a bounded set of active legacy prune payloads before workers
/// start. Policy fields in those payloads are ignored by `Job` deserialization.
/// Missing-board jobs are resolved, and duplicate pending jobs are coalesced.
///
/// # Errors
/// Returns an error if the legacy job page cannot be queried.
pub(crate) fn normalize_legacy_thread_prune_jobs(pool: &DbPool, limit: usize) -> Result<()> {
    let conn = pool
        .get()
        .context("Get DB connection for legacy thread-prune recovery failed")?;
    let mut stmt = conn
        .prepare(
            "SELECT id, payload
             FROM background_jobs
             WHERE job_type = 'thread_prune' AND status = 'pending'
             ORDER BY id ASC LIMIT ?1",
        )
        .context("Prepare legacy thread-prune recovery failed")?;
    let jobs = stmt
        .query_map(
            rusqlite::params![i64::try_from(limit.max(1)).unwrap_or(i64::MAX)],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .context("Query legacy thread-prune jobs failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read legacy thread-prune jobs failed")?;
    drop(stmt);

    let mut board_ids = std::collections::BTreeSet::new();
    for (job_id, payload) in jobs {
        match serde_json::from_str::<Job>(&payload) {
            Ok(Job::ThreadPrune { board_id }) => {
                let _previous_entry = board_ids.insert(board_id);
            }
            Ok(_) => {
                error!(target: "workers", job_id, "thread_prune row contained another job type");
            }
            Err(error) => {
                error!(target: "workers", job_id, error = %error, "invalid legacy thread-prune payload remains retryable");
            }
        }
    }

    for board_id in board_ids {
        let exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM boards WHERE id = ?1)",
                rusqlite::params![board_id],
                |row| row.get::<_, bool>(0),
            )
            .context("Check legacy thread-prune board failed")?;
        if exists {
            let schedule_result = crate::db::persist_thread_prune_intent(&conn, board_id);
            match schedule_result {
                Ok(schedule) => log_thread_prune_schedule(board_id, schedule),
                Err(error) => error!(
                    target: "workers",
                    board_id,
                    error = %error,
                    "failed to normalize legacy thread-prune jobs"
                ),
            }
        } else {
            let resolved = conn
                .execute(
                    "UPDATE background_jobs
                     SET status = 'done',
                         last_error = 'resolved missing-board legacy thread-prune job',
                         updated_at = unixepoch()
                     WHERE job_type = 'thread_prune' AND status = 'pending'
                       AND CAST(json_extract(
                             CASE WHEN json_valid(payload) THEN payload ELSE NULL END,
                             '$.d.board_id'
                           ) AS INTEGER) = ?1",
                    rusqlite::params![board_id],
                )
                .context("Resolve missing-board legacy thread-prune jobs failed")?;
            warn!(
                target: "workers",
                board_id,
                jobs_resolved = resolved,
                "resolved legacy thread-prune work for missing board"
            );
        }
    }
    Ok(())
}

/// Maximum media jobs and pending posts inspected by one reconciliation pass.
pub(crate) const MEDIA_RECONCILE_BATCH: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Summary of one bounded media-state reconciliation page.
pub struct MediaReconciliation {
    /// Active media-job rows inspected.
    pub jobs_scanned: usize,
    /// Pending post rows inspected.
    pub posts_scanned: usize,
    /// Job rows moved to a safe terminal state.
    pub jobs_resolved: usize,
    /// Post media states repaired.
    pub posts_repaired: usize,
    /// Malformed rows isolated without blocking later work.
    pub malformed_jobs: usize,
    /// Cursor for the next active-job page, or zero after wrapping.
    pub next_job_id: i64,
    /// Cursor for the next pending-post page, or zero after wrapping.
    pub next_post_id: i64,
}

/// Derive the deterministic output named by a video job payload.
fn deterministic_video_output_path(source: &str, board: &str) -> Option<String> {
    let source = std::path::Path::new(source);
    let stem = source.file_stem()?.to_str()?;
    let extension = source.extension()?.to_str()?.to_ascii_lowercase();
    matches!(extension.as_str(), "mp4" | "mkv" | "webm")
        .then(|| format!("{board}/{}", transcoded_webm_name(stem, &extension)))
}

/// Derive the deterministic output named by an audio-waveform payload.
fn deterministic_waveform_output_path(source: &str, board: &str) -> Option<String> {
    let stem = std::path::Path::new(source).file_stem()?.to_str()?;
    (!stem.is_empty()).then(|| format!("{board}/thumbs/{stem}.png"))
}

/// Reconcile one bounded page of legacy media job/post inconsistencies.
///
/// Repairs use only the job payload's source identity and current durable post
/// state. Deterministic files are never assumed to belong to a post merely from
/// their name.
///
/// # Errors
/// Returns an error if the immediate transaction or bounded queries fail.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "the bounded legacy-state matrix is intentionally explicit"
)]
pub fn reconcile_media_job_states(
    pool: &DbPool,
    after_job_id: i64,
    after_post_id: i64,
    limit: usize,
) -> Result<MediaReconciliation> {
    let conn = pool
        .get()
        .context("Get DB connection for media reconciliation failed")?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin media reconciliation transaction failed")?;
    let result = (|| {
        let bounded_limit = limit.clamp(1, MEDIA_RECONCILE_BATCH);
        let sql_limit = i64::try_from(bounded_limit).unwrap_or(i64::MAX);
        let mut job_stmt = conn.prepare(
            "SELECT id, job_type, payload
             FROM background_jobs
             WHERE id > ?1 AND status IN ('pending', 'running')
               AND job_type IN ('video_transcode', 'audio_waveform')
             ORDER BY id ASC LIMIT ?2",
        )?;
        let jobs = job_stmt
            .query_map(rusqlite::params![after_job_id.max(0), sql_limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(job_stmt);

        let mut report = MediaReconciliation {
            jobs_scanned: jobs.len(),
            next_job_id: jobs.last().map_or(0, |job| job.0),
            ..MediaReconciliation::default()
        };
        if jobs.len() < bounded_limit {
            report.next_job_id = 0;
        }

        for (job_id, stored_type, payload) in jobs {
            let parsed = serde_json::from_str::<Job>(&payload);
            let job = match parsed {
                Ok(job) if job.type_str() == stored_type => job,
                Ok(_) | Err(_) => {
                    let _rows_affected = conn.execute(
                        "UPDATE background_jobs
                         SET status = 'failed',
                             last_error = 'malformed legacy media job payload',
                             updated_at = unixepoch()
                         WHERE id = ?1 AND status IN ('pending', 'running')",
                        rusqlite::params![job_id],
                    )?;
                    report.jobs_resolved = report.jobs_resolved.saturating_add(1);
                    report.malformed_jobs = report.malformed_jobs.saturating_add(1);
                    warn!(
                        target: "workers",
                        job_id,
                        job_type = %stored_type,
                        "isolated malformed legacy media job"
                    );
                    continue;
                }
            };
            let Some(identity) = media_job_identity(&job) else {
                continue;
            };
            let post = conn
                .query_row(
                    "SELECT file_path, media_processing_state
                     FROM posts WHERE id = ?1",
                    rusqlite::params![identity.post_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let compatible = post.as_ref().is_some_and(|(path, state)| {
                path.as_deref() == Some(identity.expected_source.as_str())
                    && matches!(
                        state.as_str(),
                        crate::db::MEDIA_PROCESSING_PENDING | "running"
                    )
            });
            let older_duplicate: bool = conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM background_jobs
                     WHERE id < ?1 AND status IN ('pending', 'running')
                       AND job_type = ?2 AND json_valid(payload)
                       AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?3
                       AND json_extract(payload, '$.d.file_path') = ?4
                 )",
                rusqlite::params![
                    job_id,
                    identity.job_type,
                    identity.post_id,
                    identity.expected_source
                ],
                |row| row.get(0),
            )?;
            if compatible && !older_duplicate {
                continue;
            }

            let _rows_affected = conn.execute(
                "UPDATE background_jobs
                 SET status = 'done',
                     last_error = ?2,
                     updated_at = unixepoch()
                 WHERE id = ?1 AND status IN ('pending', 'running')",
                rusqlite::params![
                    job_id,
                    if post.is_none() {
                        "resolved missing media target"
                    } else if older_duplicate {
                        "resolved duplicate media job"
                    } else {
                        "resolved stale or terminal media target"
                    }
                ],
            )?;
            report.jobs_resolved = report.jobs_resolved.saturating_add(1);
            debug!(
                target: "workers",
                post_id = identity.post_id,
                job_id,
                job_type = identity.job_type,
                "resolved stale, duplicate, or missing-target media job"
            );
        }

        let mut post_stmt = conn.prepare(
            "SELECT p.id, p.file_path, p.thumb_path
             FROM posts p
             WHERE p.id > ?1 AND p.media_processing_state = ?2
             ORDER BY p.id ASC LIMIT ?3",
        )?;
        let posts = post_stmt
            .query_map(
                rusqlite::params![
                    after_post_id.max(0),
                    crate::db::MEDIA_PROCESSING_PENDING,
                    sql_limit
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(post_stmt);
        report.posts_scanned = posts.len();
        report.next_post_id = posts.last().map_or(0, |post| post.0);
        if posts.len() < bounded_limit {
            report.next_post_id = 0;
        }

        for (post_id, source, current_thumb) in posts {
            let Some(source) = source else {
                let _rows_affected = conn.execute(
                    "UPDATE posts SET media_processing_state = ?1,
                         media_processing_error = 'pending media post has no source'
                     WHERE id = ?2 AND media_processing_state = ?3",
                    rusqlite::params![
                        crate::db::MEDIA_PROCESSING_FAILED,
                        post_id,
                        crate::db::MEDIA_PROCESSING_PENDING
                    ],
                )?;
                report.posts_repaired = report.posts_repaired.saturating_add(1);
                continue;
            };
            let mut active_stmt = conn.prepare(
                "SELECT job_type, payload FROM background_jobs
                 WHERE status IN ('pending', 'running')
                   AND job_type IN ('video_transcode', 'audio_waveform')
                   AND json_valid(payload)
                   AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
                 ORDER BY id ASC",
            )?;
            let active_payloads = active_stmt
                .query_map(rusqlite::params![post_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(active_stmt);
            let has_compatible = active_payloads.iter().any(|(stored_type, payload)| {
                serde_json::from_str::<Job>(payload).is_ok_and(|job| {
                    job.type_str() == stored_type
                        && media_job_identity(&job).is_some_and(|identity| {
                            identity.post_id == post_id && identity.expected_source == source
                        })
                })
            });
            if has_compatible {
                continue;
            }

            let mut terminal_stmt = conn.prepare(
                "SELECT job_type, payload, status, last_error FROM background_jobs
                     WHERE status IN ('done', 'failed')
                       AND job_type IN ('video_transcode', 'audio_waveform')
                       AND json_valid(payload)
                       AND CAST(json_extract(payload, '$.d.post_id') AS INTEGER) = ?1
                     ORDER BY id DESC LIMIT 16",
            )?;
            let terminal_rows = terminal_stmt
                .query_map(rusqlite::params![post_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(terminal_stmt);
            let mut terminal = None;
            for (job_type, payload, status, error) in terminal_rows {
                let Ok(job) = serde_json::from_str::<Job>(&payload) else {
                    continue;
                };
                if job.type_str() != job_type {
                    continue;
                }
                let disposition = match job {
                    Job::VideoTranscode {
                        file_path,
                        board_short,
                        post_id: _,
                    } => {
                        let expected_output_path =
                            deterministic_video_output_path(&file_path, &board_short);
                        let installed_output_matches = status == "done"
                            && expected_output_path.as_deref() == Some(source.as_str())
                            && crate::utils::fs_security::existing_regular_file_child(
                                std::path::Path::new(&CONFIG.upload_dir),
                                &source,
                            )
                            .is_ok();
                        if file_path == source || installed_output_matches {
                            Some((status.as_str(), error))
                        } else {
                            None
                        }
                    }
                    Job::AudioWaveform {
                        file_path,
                        board_short,
                        post_id: _,
                    } => {
                        let expected_thumb_path =
                            deterministic_waveform_output_path(&file_path, &board_short);
                        if file_path == source
                            && (status != "done"
                                || expected_thumb_path.is_none()
                                || current_thumb.as_deref() == expected_thumb_path.as_deref())
                        {
                            Some((status.as_str(), error))
                        } else {
                            None
                        }
                    }
                    Job::ThreadPrune { board_id: _ }
                    | Job::SpamCheck {
                        post_id: _,
                        ip_hash: _,
                        body_len: _,
                    } => None,
                };
                if let Some((terminal_status, terminal_error)) = disposition {
                    terminal = Some((terminal_status.to_owned(), terminal_error));
                    break;
                }
            }
            let (state, detail) = match terminal {
                Some((status, _error)) if status == "done" => ("", None),
                Some((_status, error)) => (
                    crate::db::MEDIA_PROCESSING_FAILED,
                    error.or_else(|| Some("media job permanently failed".to_owned())),
                ),
                None => (
                    crate::db::MEDIA_PROCESSING_FAILED,
                    Some("pending media state has no durable compatible job".to_owned()),
                ),
            };
            let _rows_affected = conn.execute(
                "UPDATE posts SET media_processing_state = ?1, media_processing_error = ?2
                 WHERE id = ?3 AND file_path = ?4 AND media_processing_state = ?5",
                rusqlite::params![
                    state,
                    detail,
                    post_id,
                    source,
                    crate::db::MEDIA_PROCESSING_PENDING
                ],
            )?;
            report.posts_repaired = report.posts_repaired.saturating_add(1);
            warn!(
                target: "workers",
                post_id,
                "repaired pending post without compatible durable media work"
            );
        }
        Ok(report)
    })();

    match result {
        Ok(report) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Commit media reconciliation failed");
            }
            Ok(report)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

// SpamCheck
/// Records lightweight abuse signals for later operational review.
fn run_spam_check(post_id: i64, _ip_hash: &str, body_len: usize) {
    if body_len > 3500 {
        debug!(
            "SpamCheck: post {} body_len={} exceeds 3500 chars (flagged for review)",
            post_id, body_len
        );
    }
}

// Thumbnail / waveform cache eviction
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Result of one database-aware thumbnail-cache eviction pass.
pub struct ThumbCacheEvictionReport {
    /// Bytes in all inspected thumbnail files before eviction.
    pub total_before_bytes: u64,
    /// Bytes remaining after all safe candidates were considered.
    pub total_after_bytes: u64,
    /// Unreferenced files removed.
    pub removed_files: u64,
    /// Bytes freed by removed files.
    pub removed_bytes: u64,
}

/// Sum cache bytes before filesystem mutation, rejecting unrepresentable totals.
fn thumbnail_cache_bytes(mut sizes: impl Iterator<Item = u64>) -> Result<u64> {
    sizes.try_fold(0_u64, |total, size| {
        total
            .checked_add(size)
            .context("thumbnail cache size overflow")
    })
}

/// Load every live thumbnail reference before considering cache eviction.
fn referenced_thumbnail_paths(
    conn: &rusqlite::Connection,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT thumb_path
         FROM posts
         WHERE thumb_path IS NOT NULL AND TRIM(thumb_path) != ''",
    )?;
    let referenced = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(referenced)
}

/// Walk every board's `thumbs/` subdirectory and evict unreferenced files.
///
/// Only files inside `{upload_dir}/{board}/thumbs/` are considered — original
/// uploads are never touched.  Deletion is best-effort: individual failures
/// are logged and skipped rather than aborting the whole pass.
///
/// # Errors
/// Returns an error if current thumbnail references cannot be loaded. In that
/// fail-closed case no filesystem deletion is attempted.
pub fn evict_thumb_cache(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    max_bytes: u64,
) -> Result<ThumbCacheEvictionReport> {
    let referenced = referenced_thumbnail_paths(conn)?;

    // Collect (mtime_secs, path, size) for every file inside any thumbs/ dir.
    let mut files: Vec<(u64, PathBuf, u64, bool)> = Vec::new();
    let Ok(upload_root) = std::path::Path::new(upload_dir).canonicalize() else {
        return Ok(ThumbCacheEvictionReport::default());
    };
    let Ok(boards_iter) = std::fs::read_dir(upload_dir) else {
        return Ok(ThumbCacheEvictionReport::default());
    };
    for board_entry in boards_iter.flatten() {
        let Ok(board_metadata) = board_entry.path().symlink_metadata() else {
            continue;
        };
        if !board_metadata.is_dir() || board_metadata.file_type().is_symlink() {
            continue;
        }
        let thumbs_dir = board_entry.path().join("thumbs");
        let Ok(thumbs_metadata) = std::fs::symlink_metadata(&thumbs_dir) else {
            continue;
        };
        if !thumbs_metadata.is_dir() || thumbs_metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(thumbs_iter) = std::fs::read_dir(&thumbs_dir) else {
            continue;
        };
        for entry in thumbs_iter.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.file_type().is_symlink() {
                continue;
            }
            let Ok(canonical_path) = path.canonicalize() else {
                continue;
            };
            if !canonical_path.starts_with(&upload_root) {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let is_referenced = canonical_path
                .strip_prefix(&upload_root)
                .ok()
                .and_then(std::path::Path::to_str)
                .is_some_and(|relative| referenced.contains(relative));
            files.push((mtime, canonical_path, meta.len(), is_referenced));
        }
    }

    // Validate the entire byte total before removing any cache file.
    let total = thumbnail_cache_bytes(files.iter().map(|(_, _, size, _)| *size))?;
    let mut report = ThumbCacheEvictionReport {
        total_before_bytes: total,
        total_after_bytes: total,
        ..ThumbCacheEvictionReport::default()
    };
    if total <= max_bytes {
        return Ok(report);
    }

    // Sort oldest-first so we delete the least-recently-used files first.
    files.sort_unstable_by_key(|(mtime, _, _, _)| *mtime);

    let mut remaining = total;
    let mut deleted = 0u64;
    let mut deleted_bytes = 0u64;
    for (_, path, size, is_referenced) in &files {
        if remaining <= max_bytes {
            break;
        }
        if *is_referenced {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                remaining = remaining.saturating_sub(*size);
                deleted = deleted
                    .checked_add(1)
                    .context("thumbnail eviction count overflow")?;
                // Dereference `size` for clarity.
                deleted_bytes = deleted_bytes
                    .checked_add(*size)
                    .context("thumbnail eviction size overflow")?;
            }
            Err(e) => {
                warn!("evict_thumb_cache: failed to delete {:?}: {}", path, e);
            }
        }
    }
    if deleted > 0 {
        tracing::info!(target: "workers", files_removed = deleted, freed_kib = deleted_bytes / 1024, remaining_kib = remaining / 1024, limit_kib = max_bytes / 1024, "Thumbnail cache eviction complete");
    }
    report.total_after_bytes = remaining;
    report.removed_files = deleted;
    report.removed_bytes = deleted_bytes;
    Ok(report)
}
