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
    command.stdin(Stdio::null());
    #[cfg(unix)]
    {
        command.process_group(0);
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
        /// Path relative to `CONFIG.upload_dir`, e.g. "b/abc123.mp4"
        file_path: String,
        /// Short name of the board containing the post.
        board_short: String,
    },
    /// Generate a waveform PNG thumbnail for an audio upload via ffmpeg.
    AudioWaveform {
        /// Database identifier of the post owning the upload.
        post_id: i64,
        /// Path relative to `CONFIG.upload_dir`
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
            Self::VideoTranscode { .. } => "video_transcode",
            Self::AudioWaveform { .. } => "audio_waveform",
            Self::ThreadPrune { .. } => "thread_prune",
            Self::SpamCheck { .. } => "spam_check",
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
            Self::ThreadPrune { .. } | Self::SpamCheck { .. } => None,
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
            .unwrap_or(0);
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
                self.pending_jobs.fetch_add(1, Ordering::Relaxed);
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
                    self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
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
                    self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
                    Err(error)
                }
            }
        } else {
            self.dropped_jobs.fetch_add(1, Ordering::Relaxed);
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
            self.dropped_jobs.fetch_add(1, Ordering::Relaxed);
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
                self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
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
            self.pending_jobs.fetch_add(1, Ordering::Relaxed);
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
        self.pending_jobs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                Some(pending.saturating_sub(count))
            })
            .ok();
    }

    /// Restores the pending-job gauge when a failed job is scheduled to retry.
    fn mark_job_failure_state(&self, failure_state: crate::db::JobFailureState) {
        if failure_state == crate::db::JobFailureState::Retrying {
            self.pending_jobs.fetch_add(1, Ordering::Relaxed);
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
///   `for h in handles { h.await.ok(); }`
///
/// Workers are pure async Tokio tasks — they do not consume OS threads at rest.
pub fn start_worker_pool(
    queue: &Arc<JobQueue>,
    ffmpeg_available: bool,
    ffprobe_available: bool,
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
                worker_loop(
                    idx,
                    q,
                    ffmpeg_available,
                    ffprobe_available,
                    ffmpeg_vp9_available,
                )
                .await;
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
#[cfg(not(test))]
const COMPLETION_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Short shutdown grace used to keep worker tests bounded.
#[cfg(test)]
const COMPLETION_SHUTDOWN_GRACE: Duration = Duration::from_millis(200);

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
                conn.execute(
                    "UPDATE background_jobs
                     SET status = 'failed',
                         last_error = ?2,
                         updated_at = unixepoch()
                     WHERE id = ?1 AND status = 'running'",
                    rusqlite::params![job_id, recovery_message],
                )?;
                if matches!(completion, JobCompletion::Done) {
                    if let Some(identity) = media_identity {
                        conn.execute(
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
        .then(|| tokio::time::Instant::now() + COMPLETION_SHUTDOWN_GRACE);
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
                        Some(tokio::time::Instant::now() + COMPLETION_SHUTDOWN_GRACE);
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
    ffprobe_available: bool,
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
                            Err(error) => {
                                error!(
                                    "Worker {id}: could not resolve broken job #{job_id}: {error}"
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
                    ffprobe_available,
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
    Duration::from_millis(base + jitter)
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
    ffprobe_available: bool,
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
            // Avoid scheduling the same source path twice.
            if in_progress.contains_key(&file_path) {
                warn!(
                    "VideoTranscode: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(JobExecution::Stale);
            }
            in_progress.insert(file_path.clone(), true);
            active_video_jobs.fetch_add(1, Ordering::Relaxed);
            let result = transcode_video(
                job_id,
                post_id,
                file_path.clone(),
                board_short,
                ffmpeg_available,
                ffprobe_available,
                ffmpeg_vp9_available,
                pool,
                cancel,
            )
            .await;
            active_video_jobs.fetch_sub(1, Ordering::Relaxed);
            in_progress.remove(&file_path);
            result
        }

        Job::AudioWaveform {
            post_id,
            file_path,
            board_short,
        } => {
            // Avoid scheduling the same source path twice.
            if in_progress.contains_key(&file_path) {
                warn!(
                    "AudioWaveform: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(JobExecution::Stale);
            }
            in_progress.insert(file_path.clone(), true);
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
            in_progress.remove(&file_path);
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
        Job::ThreadPrune { .. } | Job::SpamCheck { .. } => None,
    }
}

// VideoTranscode
/// Transcode an MP4 upload to `WebM` (VP9 + Opus), then update the post's
/// `file_path` and `mime_type`. The original MP4 is deleted on success.
///
/// Requires both `ffmpeg_available` (binary present) and `ffmpeg_vp9_available`
/// (libvpx-vp9 + libopus compiled in).  If either flag is false the job is
/// skipped gracefully — no error is returned and the file remains as-is.
///
/// The command runs in its own process group. Timeout, cancellation, and scope
/// cleanup terminate that whole group so descendants cannot outlive the job.
///
/// A hard timeout of the live `ffmpeg_timeout_secs` setting is applied.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_arguments,
    reason = "transcode process control and fail-closed cleanup share one lifecycle"
)]
async fn transcode_video(
    job_id: i64,
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_vp9_available: bool,
    pool: DbPool,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    if !ffmpeg_available {
        debug!("VideoTranscode skipped for post {post_id}: ffmpeg not available");
        return Ok(JobExecution::NeedsCompletion);
    }

    if !ffprobe_available {
        warn!(
            "VideoTranscode skipped for post {post_id}: ffprobe not available. \
             Install ffprobe alongside ffmpeg to enable safe video transcoding."
        );
        return Ok(JobExecution::NeedsCompletion);
    }

    if !ffmpeg_vp9_available {
        warn!(
            "VideoTranscode skipped for post {post_id}: libvpx-vp9 or libopus not available. \
             Install ffmpeg with VP9 + Opus support to enable MP4/MKV→WebM transcoding."
        );
        return Ok(JobExecution::NeedsCompletion);
    }

    let timeout_secs = crate::config::ffmpeg_timeout_secs();
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

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

    // `kill_on_drop` terminates ffmpeg when its timeout future is dropped.
    let mut command = ffmpeg_command();
    command
        .args(&args)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg '{}': {e}", CONFIG.ffmpeg_path))?;
    drop(command);

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

/// Prepare a video transcode: validate the source file, optionally probe the
/// codec for `WebM` inputs, create a temp output file, and return the ffmpeg
/// argument list along with the relevant paths.
///
/// Returns `Ok(None)` when the job should be skipped gracefully (unrecognised
/// extension, or `WebM` that is already VP8/VP9). Returns `Ok(Some(...))` when
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

#[expect(
    clippy::cognitive_complexity,
    reason = "source validation and codec-specific preparation share one preflight boundary"
)]
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
    if ext != "mp4" && ext != "webm" && ext != "mkv" {
        debug!("VideoTranscode: skipping unrecognised extension {file_path}");
        return Ok(None);
    }

    if ext == "webm" {
        let src_str = src
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Source path is non-UTF-8: {}", src.display()))?;
        match crate::media::ffmpeg::probe_video_codec(src_str) {
            Ok(codec) if codec == "av1" => {
                tracing::info!(target: "workers", post_id = post_id, codec = "av1", "VideoTranscode: re-encoding WebM/AV1 to VP9");
            }
            Ok(codec) => {
                debug!(
                    "VideoTranscode: skipping WebM with codec '{}' for post {} (already VP8/VP9)",
                    codec, post_id
                );
                return Ok(None);
            }
            Err(e) => {
                warn!(
                    "VideoTranscode: could not probe codec for post {} ({}); skipping",
                    post_id, e
                );
                return Ok(None);
            }
        }
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
    crate::utils::fs_security::canonical_child_of(upload_root, &board_dir)
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
            tracing::warn!(
                target: "workers",
                post_id,
                error = %error,
                "VideoTranscode: output validation failed; keeping original media"
            );
            return Ok(JobExecution::NeedsCompletion);
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
    tmp.persist_noclobber(destination)
        .map_err(|error| anyhow::anyhow!("Failed to atomically install media output: {error}"))?;
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

    if crate::media::ffmpeg::probe_stream_kind(output_path)?
        != crate::media::ffmpeg::StreamKind::Video
    {
        return Ok(TranscodeOutputDecision::Skip {
            reason: "transcoded output has no video stream",
        });
    }
    let output_str = output_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Transcoded output path is non-UTF-8"))?;
    let codec = crate::media::ffmpeg::probe_video_codec(output_str)?;
    if codec != "vp9" {
        return Ok(TranscodeOutputDecision::Skip {
            reason: "transcoded output is not VP9",
        });
    }

    Ok(TranscodeOutputDecision::Accept)
}

// AudioWaveform
/// Generate a waveform PNG thumbnail for an audio upload via ffmpeg.
///
/// Same `kill_on_drop` fix as `transcode_video`. Uses
/// `tokio::process::Command` so the OS process is actually killed when the
/// timeout fires, rather than continuing to run in an abandoned blocking thread.
/// Parts produced by [`waveform_prepare`] that are consumed by the ffmpeg
/// phase and [`waveform_finalise`].
type WaveformPrepareParts = (
    Vec<String>,
    PathBuf,
    String,
    PathBuf,
    String,
    tempfile::NamedTempFile,
);

/// Generates and persists an audio waveform thumbnail with bounded process time.
async fn generate_waveform(
    job_id: i64,
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    if !ffmpeg_available {
        return Ok(JobExecution::NeedsCompletion);
    }

    let timeout_secs = crate::config::ffmpeg_timeout_secs();
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

    // File I/O and temporary-file creation are blocking.
    let (args, png_abs, png_rel, src_path, expected_file_path, tmp_png) = {
        let file_path2 = file_path.clone();
        let board_short2 = board_short.clone();
        tokio::task::spawn_blocking(move || waveform_prepare(&file_path2, &board_short2))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in waveform prepare: {e}"))??
    };

    // `kill_on_drop` terminates ffmpeg when its timeout future is dropped.
    let mut command = ffmpeg_command();
    command
        .args(&args)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|e| {
        anyhow::anyhow!(
            "failed to spawn ffmpeg '{}' for waveform: {e}",
            CONFIG.ffmpeg_path
        )
    })?;
    drop(command);

    match wait_for_ffmpeg_output(child, ffmpeg_timeout, cancel).await? {
        AsyncWaitOutcome::Exited(output) => {
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "ffmpeg waveform exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        AsyncWaitOutcome::TimedOut => {
            warn!("AudioWaveform: job for post {post_id} timed out after {timeout_secs}s — ffmpeg process killed");
            return Err(anyhow::anyhow!(
                "ffmpeg waveform timed out after {timeout_secs}s"
            ));
        }
        AsyncWaitOutcome::Cancelled => {
            return Err(anyhow::anyhow!("ffmpeg waveform cancelled during shutdown"));
        }
    }

    // File persistence and the database update are blocking.
    let finalise_result = tokio::task::spawn_blocking(move || {
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
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in waveform finalise: {e}"))?;
    finalise_result
}

/// Formats the actionable warning emitted when video re-encoding times out.
fn video_reencode_timeout_warning(post_id: i64, timeout_secs: u64) -> String {
    let human_timeout = crate::config::describe_timeout_secs(timeout_secs);
    format!(
        "VideoTranscode: ffmpeg video re-encoding/conversion timed out for post {post_id} after {human_timeout} ({timeout_secs}s); slow systems such as Raspberry Pi devices may need a higher timeout. Increase the video re-encoding timeout in the admin panel or settings.toml."
    )
}

/// Blocking prepare phase for [`generate_waveform`]: validate the source,
/// create a temp output file, and build the ffmpeg arg list.
fn waveform_prepare(file_path: &str, board_short: &str) -> Result<WaveformPrepareParts> {
    use anyhow::Context as _;
    let upload_dir = &CONFIG.upload_dir;
    let src = PathBuf::from(upload_dir).join(file_path);
    if !src.exists() {
        return Err(anyhow::anyhow!(
            "Audio source not found for waveform: {}",
            src.display()
        ));
    }
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Malformed audio filename: {}", src.display()))?
        .to_owned();
    let thumb_size = CONFIG.thumb_size;
    let thumbs_dir = PathBuf::from(upload_dir).join(board_short).join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)?;
    let png_name = format!("{stem}.png");
    let png_abs = thumbs_dir.join(&png_name);
    let png_rel = format!("{board_short}/thumbs/{png_name}");
    let tmp_png = tempfile::Builder::new()
        .prefix("chan_wav_")
        .suffix(".png")
        .tempfile_in(&thumbs_dir)
        .context("Failed to create temp file for waveform PNG")?;
    let src_str = src
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Source path is non-UTF-8"))?
        .to_owned();
    let tmp_str = tmp_png
        .path()
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Temp path is non-UTF-8"))?
        .to_owned();
    let filter = format!(
        "showwavespic=s={thumb_size}x{}:colors=0x888888",
        thumb_size / 2
    );
    let args: Vec<String> = [
        "-loglevel",
        "error",
        "-i",
        &src_str,
        "-filter_complex",
        &filter,
        "-frames:v",
        "1",
        "-y",
        &tmp_str,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    Ok((args, png_abs, png_rel, src, file_path.to_owned(), tmp_png))
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
#[expect(
    clippy::cognitive_complexity,
    reason = "archive, prune, and filesystem finalization remain one consistency operation"
)]
/// Applies one board's live and archived thread-retention limits.
async fn prune_threads(board_id: i64, pool: DbPool) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let Some(policy) = load_board_retention_policy(&conn, board_id)? else {
            warn!(
                target: "workers",
                board_id,
                "thread-prune intent targeted a missing board; resolving safely"
            );
            return Ok(());
        };
        // `archive_before_prune` is a global safety net: when true,
        // overflow threads are always archived rather than hard-deleted, even
        // on boards where allow_archive = false.  This closes the silent data
        // loss gap where a thread could disappear simply because a board hit
        // its thread limit while the admin had not opted into archiving.
        let do_archive = policy.allow_archive || CONFIG.archive_before_prune;
        if do_archive {
            let count = crate::db::archive_old_threads(&conn, board_id, policy.max_threads)?;
            if count > 0 {
                tracing::info!(target: "workers", count, board = %policy.board_short, board_id, "ThreadArchive: threads archived");
            }
        } else {
            let deleted = crate::db::prune_old_threads(&conn, board_id, policy.max_threads)?;
            let count = deleted.paths.len();
            finalize_thread_prune_cleanup(
                &conn,
                &policy,
                &deleted,
                "thread prune cleanup did not fully complete",
            );
            if count > 0 {
                tracing::info!(target: "workers", count, board = %policy.board_short, board_id, files_removed = deleted.paths.len(), "ThreadPrune: threads deleted");
            }
        }

        // Archived retention remains authoritative even if archiving has since
        // been disabled; disabling archive changes live overflow behavior but
        // does not make already-archived content exempt from its current cap.
        let archived = crate::db::prune_old_archived_threads(
            &conn,
            board_id,
            policy.max_archived_threads,
        )?;
        finalize_thread_prune_cleanup(
            &conn,
            &policy,
            &archived,
            "archived prune cleanup did not fully complete",
        );
        if !archived.paths.is_empty() {
            tracing::info!(target: "workers", archived_cap = policy.max_archived_threads, board = %policy.board_short, board_id, files_removed = archived.paths.len(), "ThreadArchivePrune: archived threads deleted");
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
                board_ids.insert(board_id);
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
                    conn.execute(
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

            conn.execute(
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
                conn.execute(
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
                        ..
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
                        ..
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
                    Job::ThreadPrune { .. } | Job::SpamCheck { .. } => None,
                };
                if let Some((status, error)) = disposition {
                    terminal = Some((status.to_owned(), error));
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
            conn.execute(
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
fn run_spam_check(post_id: i64, ip_hash: &str, body_len: usize) {
    if body_len > 3500 {
        debug!(
            "SpamCheck: post {} body_len={} exceeds 3500 chars (flagged for review)",
            post_id, body_len
        );
    }
    let _ = ip_hash;
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
    let mut reference_stmt = conn.prepare(
        "SELECT DISTINCT thumb_path
         FROM posts
         WHERE thumb_path IS NOT NULL AND TRIM(thumb_path) != ''",
    )?;
    let referenced: std::collections::HashSet<String> = reference_stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;

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

    // Dereference `sz` explicitly and annotate the sum type for
    // clarity.  `Iterator<Item = &u64>` implements `Sum<&u64>` in std so this
    // compiled before, but the explicit form is more readable and avoids the
    // implicit coercion.
    let total: u64 = files.iter().map(|(_, _, size, _)| *size).sum::<u64>();
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
                deleted += 1;
                // Dereference `size` for clarity.
                deleted_bytes += *size;
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

#[cfg(test)]
mod tests {
    use super::{
        media_job_identity, normalize_legacy_thread_prune_jobs, persist_job_completion,
        persist_job_completion_once, persist_transcoded_webm, prune_threads,
        reconcile_media_job_states, reconcile_thread_prune_intents, serialize_job_payload,
        transcode_video_finalise, transcoded_webm_name, validated_board_media_file,
        video_reencode_timeout_warning, waveform_finalise, EnqueueOutcome, Job, JobCompletion,
        JobQueue,
    };
    use anyhow::{bail, ensure, Context as _};
    use std::io::Write as _;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[expect(
        clippy::expect_used,
        reason = "test database queries stop locally with fixture-specific failure context"
    )]
    fn file_hash_count(conn: &rusqlite::Connection, file_path: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE file_path = ?1",
            rusqlite::params![file_path],
            |row| row.get(0),
        )
        .expect("file hash count")
    }

    fn seed_media_job_post(
        pool: &crate::db::DbPool,
        board: &str,
        source: &str,
        state: &str,
    ) -> anyhow::Result<i64> {
        let conn = pool.get().context("get media fixture connection")?;
        conn.execute(
            "INSERT INTO boards (short_name, name, description) VALUES (?1, ?1, '')",
            rusqlite::params![board],
        )?;
        let board_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'media')",
            rusqlite::params![board_id],
        )?;
        let thread_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO posts
             (thread_id, board_id, name, body, body_html, deletion_token, is_op,
              file_path, file_size, mime_type, media_type, media_processing_state)
             VALUES (?1, ?2, 'anon', '', '', 'token', 1, ?3, 10,
                     'video/mp4', 'video', ?4)",
            rusqlite::params![thread_id, board_id, source, state],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn enqueue_spam_job(queue: &JobQueue) -> anyhow::Result<i64> {
        let job = Job::SpamCheck {
            post_id: 42,
            ip_hash: "hash".to_owned(),
            body_len: 5,
        };
        match queue.enqueue(&job).context("enqueue spam-check job")? {
            EnqueueOutcome::Enqueued(id) => Ok(id),
            EnqueueOutcome::DroppedAtCapacity => bail!("test job was dropped at capacity"),
        }
    }

    fn seed_retention_board(
        pool: &crate::db::DbPool,
        short: &str,
        max_threads: i64,
        max_archived_threads: i64,
        allow_archive: bool,
    ) -> anyhow::Result<i64> {
        let conn = pool.get().context("get retention fixture connection")?;
        let board_id = crate::db::create_board(&conn, short, short, "", false)?;
        conn.execute(
            "UPDATE boards
             SET max_threads = ?2, max_archived_threads = ?3, allow_archive = ?4
             WHERE id = ?1",
            rusqlite::params![
                board_id,
                max_threads,
                max_archived_threads,
                i64::from(allow_archive)
            ],
        )?;
        Ok(board_id)
    }

    fn seed_threads(
        pool: &crate::db::DbPool,
        board_id: i64,
        live: usize,
        archived: usize,
    ) -> anyhow::Result<()> {
        let conn = pool.get().context("get thread fixture connection")?;
        for ordinal in 0..live {
            conn.execute(
                "INSERT INTO threads (board_id, subject, bumped_at)
                 VALUES (?1, 'live', ?2)",
                rusqlite::params![board_id, i64::try_from(ordinal).unwrap_or(i64::MAX)],
            )?;
        }
        for ordinal in 0..archived {
            conn.execute(
                "INSERT INTO threads (board_id, subject, bumped_at, archived, locked)
                 VALUES (?1, 'archived', ?2, 1, 1)",
                rusqlite::params![board_id, i64::try_from(ordinal).unwrap_or(i64::MAX)],
            )?;
        }
        Ok(())
    }

    fn active_prune_count(conn: &rusqlite::Connection, board_id: i64) -> anyhow::Result<i64> {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM background_jobs
             WHERE job_type = 'thread_prune' AND status IN ('pending', 'running')
               AND CAST(json_extract(payload, '$.d.board_id') AS INTEGER) = ?1",
            rusqlite::params![board_id],
            |row| row.get(0),
        )?)
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "fixture setup failures should stop this queue durability test"
    )]
    fn required_thread_prune_bypasses_capacity_and_survives_queue_recreation() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let board_id =
            seed_retention_board(&pool, "capacity", 1, 10, true).expect("seed retention board");
        let queue = JobQueue::new(pool.clone());
        queue.pending_jobs.store(
            crate::config::CONFIG.job_queue_capacity.max(1),
            Ordering::Relaxed,
        );

        let outcome = queue
            .enqueue(&Job::ThreadPrune { board_id })
            .expect("persist required prune intent");
        assert!(matches!(outcome, EnqueueOutcome::Enqueued(_)));
        let conn = pool.get().expect("verification connection");
        assert_eq!(
            active_prune_count(&conn, board_id).expect("count intents"),
            1
        );
        drop(conn);

        let restarted = JobQueue::new(pool);
        assert!(restarted.pending_count() >= 1);
    }

    #[test]
    fn media_capacity_rejection_is_terminal_without_a_durable_job() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "cap", "cap/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        queue.pending_jobs.store(
            crate::config::CONFIG.job_queue_capacity.max(1),
            Ordering::Relaxed,
        );
        let outcome = queue.enqueue(&Job::VideoTranscode {
            post_id,
            file_path: "cap/video.mp4".to_owned(),
            board_short: "cap".to_owned(),
        })?;
        ensure!(outcome == EnqueueOutcome::DroppedAtCapacity);
        let conn = pool.get()?;
        let state: (String, Option<String>) = conn.query_row(
            "SELECT media_processing_state, media_processing_error FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(state.0 == crate::db::MEDIA_PROCESSING_FAILED);
        ensure!(state.1.is_some());
        ensure!(crate::db::pending_job_count(&conn)? == 0);
        Ok(())
    }

    #[test]
    fn media_serialization_failure_precedes_any_state_transaction() -> anyhow::Result<()> {
        use std::result::Result as StdResult;

        struct FailingSerialization;
        impl serde::Serialize for FailingSerialization {
            fn serialize<S>(&self, _serializer: S) -> StdResult<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }

        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "ser", "ser/video.mp4", "")?;
        ensure!(serialize_job_payload(&FailingSerialization).is_err());
        let conn = pool.get()?;
        let state: String = conn.query_row(
            "SELECT media_processing_state FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| row.get(0),
        )?;
        ensure!(state.is_empty());
        ensure!(crate::db::pending_job_count(&conn)? == 0);
        Ok(())
    }

    #[test]
    fn fast_worker_completion_cannot_be_overwritten_back_to_pending() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "race", "race/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        let job = Job::VideoTranscode {
            post_id,
            file_path: "race/video.mp4".to_owned(),
            board_short: "race".to_owned(),
        };

        // This return is the old handler pause point. Both pending state and the
        // durable row have already committed, and there is no later state write.
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("race fixture dropped at capacity"),
        };
        {
            let conn = pool.get()?;
            claim_job(&queue, &conn, job_id);
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let finish = crate::db::finish_video_media_job_in_tx(
                &conn,
                job_id,
                post_id,
                "race/video.mp4",
                "race/video.webm",
                "video/webm",
                &"a".repeat(64),
                5,
            )?;
            ensure!(finish.applied);
            conn.execute_batch("COMMIT")?;
        }

        // Resume the handler: enqueue_media has no handler-side pending update.
        let conn = pool.get()?;
        let post: (String, String, i64) = conn.query_row(
            "SELECT file_path, media_processing_state,
                    (SELECT COUNT(*) FROM file_hashes WHERE file_path = 'race/video.webm')
             FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let status: String = conn.query_row(
            "SELECT status FROM background_jobs WHERE id = ?1",
            rusqlite::params![job_id],
            |row| row.get(0),
        )?;
        ensure!(post == ("race/video.webm".to_owned(), String::new(), 1));
        ensure!(status == "done");
        ensure!(crate::db::list_pending_fs_ops(&conn)?.len() == 1);
        Ok(())
    }

    #[test]
    fn deleted_media_target_resolves_without_retrying() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "gone", "gone/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        let job = Job::VideoTranscode {
            post_id,
            file_path: "gone/video.mp4".to_owned(),
            board_short: "gone".to_owned(),
        };
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("deleted-target fixture dropped"),
        };
        let identity = media_job_identity(&job).context("media identity")?;
        {
            let conn = pool.get()?;
            conn.execute(
                "DELETE FROM posts WHERE id = ?1",
                rusqlite::params![post_id],
            )?;
            claim_job(&queue, &conn, job_id);
        }
        let state = persist_job_completion_once(
            &pool,
            job_id,
            Some(&identity),
            &JobCompletion::Failed("source disappeared".to_owned()),
        )?;
        ensure!(state.is_none());
        let conn = pool.get()?;
        let status: String = conn.query_row(
            "SELECT status FROM background_jobs WHERE id = ?1",
            rusqlite::params![job_id],
            |row| row.get(0),
        )?;
        ensure!(status == "done");
        ensure!(crate::db::claim_next_job(&conn)?.is_none());
        Ok(())
    }

    #[test]
    fn committed_media_job_is_recovered_without_notification() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "wake", "wake/video.mp4", "")?;
        let job = Job::VideoTranscode {
            post_id,
            file_path: "wake/video.mp4".to_owned(),
            board_short: "wake".to_owned(),
        };
        let payload = serde_json::to_string(&job)?;
        let job_id = {
            let conn = pool.get()?;
            crate::db::persist_media_job(
                &conn,
                job.type_str(),
                &payload,
                post_id,
                "wake/video.mp4",
                "wake",
            )?
            .job_id
        };

        // Simulate interruption after commit and before Notify::notify_one.
        let restarted = JobQueue::new(pool.clone());
        ensure!(restarted.pending_count() == 1);
        let conn = pool.get()?;
        let claimed = crate::db::claim_next_job(&conn)?.context("poll recovered durable job")?;
        ensure!(claimed.0 == job_id);
        Ok(())
    }

    #[test]
    fn media_retry_and_permanent_failure_remain_atomic_and_reschedulable() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "retry", "retry/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        let job = Job::VideoTranscode {
            post_id,
            file_path: "retry/video.mp4".to_owned(),
            board_short: "retry".to_owned(),
        };
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("retry fixture dropped"),
        };
        let identity = media_job_identity(&job).context("media identity")?;
        for attempt in 1..=3 {
            {
                let conn = pool.get()?;
                claim_job(&queue, &conn, job_id);
            }
            let failure = persist_job_completion_once(
                &pool,
                job_id,
                Some(&identity),
                &JobCompletion::Failed(format!("failure {attempt}")),
            )?;
            let conn = pool.get()?;
            let (job_status, post_state, source): (String, String, String) = conn.query_row(
                "SELECT j.status, p.media_processing_state, p.file_path
                 FROM background_jobs j JOIN posts p ON p.id = ?2 WHERE j.id = ?1",
                rusqlite::params![job_id, post_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            ensure!(source == "retry/video.mp4");
            if attempt < 3 {
                ensure!(failure == Some(crate::db::JobFailureState::Retrying));
                ensure!(job_status == "pending");
                ensure!(post_state == crate::db::MEDIA_PROCESSING_PENDING);
                queue.mark_job_failure_state(crate::db::JobFailureState::Retrying);
            } else {
                ensure!(failure == Some(crate::db::JobFailureState::PermanentlyFailed));
                ensure!(job_status == "failed");
                ensure!(post_state == crate::db::MEDIA_PROCESSING_FAILED);
            }
        }

        let replacement_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("rescheduled fixture dropped"),
        };
        {
            let conn = pool.get()?;
            claim_job(&queue, &conn, replacement_id);
        }
        persist_job_completion_once(&pool, replacement_id, Some(&identity), &JobCompletion::Done)?;
        let conn = pool.get()?;
        let state: String = conn.query_row(
            "SELECT media_processing_state FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| row.get(0),
        )?;
        ensure!(state.is_empty());
        Ok(())
    }

    #[test]
    fn changed_source_and_original_prune_reject_stale_completion() -> anyhow::Result<()> {
        for (board, replacement_state) in [
            ("newsrc", crate::db::MEDIA_PROCESSING_PENDING),
            ("prune", crate::db::MEDIA_ORIGINAL_PRUNE_PENDING),
        ] {
            let pool = crate::db::init_test_pool()?;
            let old_source = format!("{board}/old.mp4");
            let post_id = seed_media_job_post(&pool, board, &old_source, "")?;
            let queue = JobQueue::new(pool.clone());
            let old_job = Job::VideoTranscode {
                post_id,
                file_path: old_source.clone(),
                board_short: board.to_owned(),
            };
            let old_job_id = match queue.enqueue(&old_job)? {
                EnqueueOutcome::Enqueued(job_id) => job_id,
                EnqueueOutcome::DroppedAtCapacity => bail!("stale fixture dropped"),
            };
            {
                let conn = pool.get()?;
                claim_job(&queue, &conn, old_job_id);
                if board == "newsrc" {
                    conn.execute_batch("BEGIN IMMEDIATE")?;
                }
                conn.execute(
                    "UPDATE posts SET file_path = ?2, media_processing_state = ?3 WHERE id = ?1",
                    rusqlite::params![post_id, format!("{board}/new.mp4"), replacement_state],
                )?;
                if board == "newsrc" {
                    let newer = Job::VideoTranscode {
                        post_id,
                        file_path: "newsrc/new.mp4".to_owned(),
                        board_short: "newsrc".to_owned(),
                    };
                    crate::db::enqueue_job(
                        &conn,
                        newer.type_str(),
                        &serde_json::to_string(&newer)?,
                    )?;
                    conn.execute_batch("COMMIT")?;
                }
                conn.execute_batch("BEGIN IMMEDIATE")?;
                let finish = crate::db::finish_video_media_job_in_tx(
                    &conn,
                    old_job_id,
                    post_id,
                    &old_source,
                    &format!("{board}/old.webm"),
                    "video/webm",
                    &"b".repeat(64),
                    5,
                )?;
                ensure!(!finish.applied);
                conn.execute_batch("COMMIT")?;
            }
            let conn = pool.get()?;
            let (path, state, job_status): (String, String, String) = conn.query_row(
                "SELECT p.file_path, p.media_processing_state, j.status
                 FROM posts p JOIN background_jobs j ON j.id = ?2 WHERE p.id = ?1",
                rusqlite::params![post_id, old_job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            ensure!(path == format!("{board}/new.mp4"));
            ensure!(state == replacement_state);
            ensure!(job_status == "done");
            ensure!(crate::db::list_pending_fs_ops(&conn)?.len() == 1);
        }
        Ok(())
    }

    #[test]
    fn video_completion_preserves_shared_deduplicated_source() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "share", "share/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        let shared_post_id = {
            let conn = pool.get()?;
            let (thread_id, board_id): (i64, i64) = conn.query_row(
                "SELECT thread_id, board_id FROM posts WHERE id = ?1",
                rusqlite::params![post_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token,
                  file_path, file_size, mime_type, media_type)
                 VALUES (?1, ?2, 'anon', '', '', 'token2', 'share/video.mp4', 10,
                         'video/mp4', 'video')",
                rusqlite::params![thread_id, board_id],
            )?;
            conn.last_insert_rowid()
        };
        let job = Job::VideoTranscode {
            post_id,
            file_path: "share/video.mp4".to_owned(),
            board_short: "share".to_owned(),
        };
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("shared-source fixture dropped"),
        };
        let conn = pool.get()?;
        claim_job(&queue, &conn, job_id);
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let finish = crate::db::finish_video_media_job_in_tx(
            &conn,
            job_id,
            post_id,
            "share/video.mp4",
            "share/video.webm",
            "video/webm",
            &"c".repeat(64),
            5,
        )?;
        ensure!(finish.applied);
        ensure!(finish.cleanup.paths.is_empty());
        conn.execute_batch("COMMIT")?;
        let shared_path: String = conn.query_row(
            "SELECT file_path FROM posts WHERE id = ?1",
            rusqlite::params![shared_post_id],
            |row| row.get(0),
        )?;
        ensure!(shared_path == "share/video.mp4");
        ensure!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        Ok(())
    }

    #[test]
    fn durable_video_source_cleanup_replays_after_interruption() -> anyhow::Result<()> {
        let upload_root = tempfile::tempdir()?;
        let board_dir = upload_root.path().join("clean");
        std::fs::create_dir_all(&board_dir)?;
        let source = board_dir.join("video.mp4");
        let output = board_dir.join("video.webm");
        std::fs::write(&source, b"original")?;
        std::fs::write(&output, b"derived")?;

        let pool = crate::db::init_test_pool()?;
        let post_id = seed_media_job_post(&pool, "clean", "clean/video.mp4", "")?;
        let queue = JobQueue::new(pool.clone());
        let job = Job::VideoTranscode {
            post_id,
            file_path: "clean/video.mp4".to_owned(),
            board_short: "clean".to_owned(),
        };
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("cleanup fixture dropped"),
        };
        {
            let conn = pool.get()?;
            claim_job(&queue, &conn, job_id);
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let finish = crate::db::finish_video_media_job_in_tx(
                &conn,
                job_id,
                post_id,
                "clean/video.mp4",
                "clean/video.webm",
                "video/webm",
                &"d".repeat(64),
                7,
            )?;
            ensure!(finish.cleanup.paths == ["clean/video.mp4"]);
            conn.execute_batch("COMMIT")?;
            ensure!(crate::db::list_pending_fs_ops(&conn)?.len() == 1);
        }
        ensure!(source.exists(), "simulated interruption precedes cleanup");

        let upload_dir = upload_root.path().to_str().context("UTF-8 upload root")?;
        crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_dir)?;
        ensure!(!source.exists());
        ensure!(output.exists());
        let conn = pool.get()?;
        ensure!(crate::db::list_pending_fs_ops(&conn)?.is_empty());
        drop(conn);
        crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_dir)?;
        ensure!(
            output.exists(),
            "idempotent replay must retain referenced output"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "fixture setup failures should stop this coalescing test"
    )]
    fn board_prune_coalesces_and_running_work_leaves_one_dirty_evaluation() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let board_id = seed_retention_board(&pool, "coalesce", 1, 10, true).expect("seed board");
        let conn = pool.get().expect("database connection");
        for _ in 0..100 {
            crate::db::persist_thread_prune_intent(&conn, board_id).expect("coalesce prune intent");
        }
        assert_eq!(
            active_prune_count(&conn, board_id).expect("count intents"),
            1
        );

        let (running_id, _) = crate::db::claim_next_job(&conn)
            .expect("claim intent")
            .expect("intent exists");
        let dirty = crate::db::persist_thread_prune_intent(&conn, board_id)
            .expect("persist dirty evaluation");
        assert!(dirty.inserted);
        assert_eq!(
            active_prune_count(&conn, board_id).expect("count intents"),
            2
        );
        assert!(crate::db::claim_next_job(&conn)
            .expect("check conflicting claim")
            .is_none());

        crate::db::complete_job(&conn, running_id).expect("complete running intent");
        assert!(crate::db::claim_next_job(&conn)
            .expect("claim dirty intent")
            .is_some());
    }

    #[tokio::test]
    async fn delayed_prune_uses_only_current_limits() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let board_id = seed_retention_board(&pool, "policy", 1, 10, true)?;
        seed_threads(&pool, board_id, 3, 0)?;
        {
            let conn = pool.get()?;
            crate::db::persist_thread_prune_intent(&conn, board_id)?;
            conn.execute(
                "UPDATE boards SET max_threads = 3, allow_archive = 0 WHERE id = ?1",
                rusqlite::params![board_id],
            )?;
        }

        prune_threads(board_id, pool.clone()).await?;
        {
            let conn = pool.get()?;
            let live = crate::db::count_threads_for_board(&conn, board_id)?;
            ensure!(
                live == 3,
                "increased current limit must preserve all threads"
            );
            conn.execute(
                "UPDATE boards SET max_threads = 1 WHERE id = ?1",
                rusqlite::params![board_id],
            )?;
        }
        prune_threads(board_id, pool.clone()).await?;
        let conn = pool.get()?;
        ensure!(crate::db::count_threads_for_board(&conn, board_id)? == 1);
        Ok(())
    }

    #[tokio::test]
    async fn delayed_prune_uses_current_archived_limit() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let board_id = seed_retention_board(&pool, "archive-policy", 10, 1, true)?;
        seed_threads(&pool, board_id, 0, 3)?;
        {
            let conn = pool.get()?;
            crate::db::persist_thread_prune_intent(&conn, board_id)?;
            conn.execute(
                "UPDATE boards SET max_archived_threads = 3 WHERE id = ?1",
                rusqlite::params![board_id],
            )?;
        }

        prune_threads(board_id, pool.clone()).await?;
        {
            let conn = pool.get()?;
            ensure!(crate::db::count_archived_threads_for_board(&conn, board_id)? == 3);
            conn.execute(
                "UPDATE boards SET max_archived_threads = 1 WHERE id = ?1",
                rusqlite::params![board_id],
            )?;
        }
        prune_threads(board_id, pool.clone()).await?;
        let conn = pool.get()?;
        ensure!(crate::db::count_archived_threads_for_board(&conn, board_id)? == 1);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_current_policy_fails_closed() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let board_id = seed_retention_board(&pool, "invalid", 1, 10, true)?;
        seed_threads(&pool, board_id, 3, 2)?;
        {
            let conn = pool.get()?;
            conn.execute(
                "UPDATE boards SET max_threads = 0 WHERE id = ?1",
                rusqlite::params![board_id],
            )?;
        }

        ensure!(prune_threads(board_id, pool.clone()).await.is_err());
        let conn = pool.get()?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
            rusqlite::params![board_id],
            |row| row.get(0),
        )?;
        ensure!(total == 5, "invalid policy must not remove content");
        Ok(())
    }

    #[test]
    fn reconciliation_is_bounded_and_reaches_a_clean_fixed_point() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let excess_live = seed_retention_board(&pool, "live", 1, 10, true)?;
        let excess_archive = seed_retention_board(&pool, "archive", 10, 1, true)?;
        let within_limit = seed_retention_board(&pool, "clean", 10, 10, true)?;
        seed_threads(&pool, excess_live, 2, 0)?;
        seed_threads(&pool, excess_archive, 0, 2)?;
        seed_threads(&pool, within_limit, 1, 1)?;

        let first = reconcile_thread_prune_intents(&pool, 0, 128)?;
        ensure!(first.scanned == 3);
        ensure!(first.inserted == 2);
        let second = reconcile_thread_prune_intents(&pool, 0, 128)?;
        ensure!(second.inserted == 0);
        ensure!(second.coalesced == 2);

        let conn = pool.get()?;
        ensure!(active_prune_count(&conn, excess_live)? == 1);
        ensure!(active_prune_count(&conn, excess_archive)? == 1);
        ensure!(active_prune_count(&conn, within_limit)? == 0);
        Ok(())
    }

    #[test]
    fn legacy_duplicates_coalesce_and_missing_board_work_resolves() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool()?;
        let board_id = seed_retention_board(&pool, "legacy", 1, 10, true)?;
        let conn = pool.get()?;
        let legacy_payload = serde_json::json!({
            "t": "ThreadPrune",
            "d": {
                "board_id": board_id,
                "board_short": "legacy",
                "max_threads": 1,
                "max_archived_threads": 10,
                "allow_archive": false
            }
        })
        .to_string();
        for _ in 0..3 {
            crate::db::enqueue_job(&conn, "thread_prune", &legacy_payload)?;
        }
        let missing_payload = serde_json::json!({
            "t": "ThreadPrune",
            "d": {
                "board_id": 999_999,
                "board_short": "gone",
                "max_threads": 1,
                "max_archived_threads": 1,
                "allow_archive": false
            }
        })
        .to_string();
        let missing_job_id = crate::db::enqueue_job(&conn, "thread_prune", &missing_payload)?;
        drop(conn);

        normalize_legacy_thread_prune_jobs(&pool, 128)?;

        let conn = pool.get()?;
        ensure!(active_prune_count(&conn, board_id)? == 1);
        let missing_status: String = conn.query_row(
            "SELECT status FROM background_jobs WHERE id = ?1",
            rusqlite::params![missing_job_id],
            |row| row.get(0),
        )?;
        ensure!(missing_status == "done");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the legacy reconciliation matrix remains explicit in one fixed-point test"
    )]
    fn media_reconciliation_repairs_legacy_matrix_and_reaches_fixed_point() -> anyhow::Result<()> {
        fn insert_job(conn: &rusqlite::Connection, job: &Job, status: &str) -> anyhow::Result<i64> {
            let id = crate::db::enqueue_job(conn, job.type_str(), &serde_json::to_string(job)?)?;
            conn.execute(
                "UPDATE background_jobs SET status = ?2 WHERE id = ?1",
                rusqlite::params![id, status],
            )?;
            Ok(id)
        }

        let pool = crate::db::init_test_pool()?;
        let no_job = seed_media_job_post(
            &pool,
            "noj",
            "noj/video.mp4",
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let completed = seed_media_job_post(
            &pool,
            "done",
            "done/video.mp4",
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let normal = seed_media_job_post(&pool, "norm", "norm/video.mp4", "")?;
        let failed = seed_media_job_post(
            &pool,
            "fail",
            "fail/video.mp4",
            crate::db::MEDIA_PROCESSING_FAILED,
        )?;
        let duplicate = seed_media_job_post(
            &pool,
            "dup",
            "dup/video.mp4",
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let changed = seed_media_job_post(
            &pool,
            "chg",
            "chg/new.mp4",
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let valid = seed_media_job_post(
            &pool,
            "valid",
            "valid/video.mp4",
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let conn = pool.get()?;
        insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: completed,
                file_path: "done/video.mp4".to_owned(),
                board_short: "done".to_owned(),
            },
            "done",
        )?;
        let normal_job = insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: normal,
                file_path: "norm/video.mp4".to_owned(),
                board_short: "norm".to_owned(),
            },
            "pending",
        )?;
        let missing_job = insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: 999_999,
                file_path: "gone/video.mp4".to_owned(),
                board_short: "gone".to_owned(),
            },
            "pending",
        )?;
        let failed_job = insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: failed,
                file_path: "fail/video.mp4".to_owned(),
                board_short: "fail".to_owned(),
            },
            "pending",
        )?;
        let duplicate_job = Job::VideoTranscode {
            post_id: duplicate,
            file_path: "dup/video.mp4".to_owned(),
            board_short: "dup".to_owned(),
        };
        let kept_duplicate = insert_job(&conn, &duplicate_job, "pending")?;
        let resolved_duplicate = insert_job(&conn, &duplicate_job, "pending")?;
        let changed_job = insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: changed,
                file_path: "chg/old.mp4".to_owned(),
                board_short: "chg".to_owned(),
            },
            "pending",
        )?;
        crate::db::enqueue_job(&conn, "video_transcode", "{malformed")?;
        let valid_job = insert_job(
            &conn,
            &Job::VideoTranscode {
                post_id: valid,
                file_path: "valid/video.mp4".to_owned(),
                board_short: "valid".to_owned(),
            },
            "pending",
        )?;
        drop(conn);

        let first = reconcile_media_job_states(&pool, 0, 0, 128)?;
        ensure!(first.jobs_resolved >= 6);
        ensure!(first.posts_repaired >= 3);
        ensure!(first.malformed_jobs == 1);
        let conn = pool.get()?;
        for job_id in [
            normal_job,
            missing_job,
            failed_job,
            resolved_duplicate,
            changed_job,
        ] {
            let status: String = conn.query_row(
                "SELECT status FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |row| row.get(0),
            )?;
            ensure!(status == "done");
        }
        for job_id in [kept_duplicate, valid_job] {
            let status: String = conn.query_row(
                "SELECT status FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |row| row.get(0),
            )?;
            ensure!(status == "pending");
        }
        let no_job_state: String = conn.query_row(
            "SELECT media_processing_state FROM posts WHERE id = ?1",
            rusqlite::params![no_job],
            |row| row.get(0),
        )?;
        let completed_state: String = conn.query_row(
            "SELECT media_processing_state FROM posts WHERE id = ?1",
            rusqlite::params![completed],
            |row| row.get(0),
        )?;
        ensure!(no_job_state == crate::db::MEDIA_PROCESSING_FAILED);
        ensure!(completed_state.is_empty());
        drop(conn);

        let second = reconcile_media_job_states(&pool, 0, 0, 128)?;
        ensure!(second.jobs_resolved == 0);
        ensure!(second.posts_repaired == 0);
        ensure!(second.malformed_jobs == 0);
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "constructing the constrained test pool is a prerequisite for completion-retry tests"
    )]
    fn single_connection_test_pool(connection_timeout: Duration) -> crate::db::DbPool {
        let initialized_pool = crate::db::init_test_pool().expect("initial test pool");
        let database_path: String = {
            let conn = initialized_pool.get().expect("initial connection");
            conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get(0),
            )
            .expect("test database path")
        };
        drop(initialized_pool);

        let manager = r2d2_sqlite::SqliteConnectionManager::file(database_path).with_init(|conn| {
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                     PRAGMA busy_timeout = 25;",
            )
        });
        r2d2::Pool::builder()
            .max_size(1)
            .connection_timeout(connection_timeout)
            .build(manager)
            .expect("single-connection test pool")
    }

    #[expect(
        clippy::expect_used,
        reason = "claiming the seeded job is a prerequisite for each queue-state assertion"
    )]
    fn claim_job(queue: &JobQueue, conn: &rusqlite::Connection, expected_id: i64) -> (i64, String) {
        let claimed = crate::db::claim_next_job(conn)
            .expect("claim job")
            .expect("job should be claimable");
        assert_eq!(claimed.0, expected_id);
        queue.mark_job_claimed();
        claimed
    }

    #[expect(
        clippy::expect_used,
        reason = "test fixture failures stop the individual queue accounting test with local context"
    )]
    #[test]
    fn retryable_failure_restores_queue_pending_count() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        let conn = pool.get().expect("db connection");

        claim_job(&queue, &conn, job_id);
        assert_eq!(
            crate::db::pending_job_count(&conn).expect("pending jobs"),
            0
        );
        assert_eq!(queue.pending_count(), 0);

        let failure_state =
            crate::db::fail_job(&conn, job_id, "temporary failure").expect("fail job");
        assert_eq!(failure_state, crate::db::JobFailureState::Retrying);
        queue.mark_job_failure_state(failure_state);

        assert_eq!(
            crate::db::pending_job_count(&conn).expect("pending jobs"),
            1
        );
        assert_eq!(queue.pending_count(), 1);
    }

    #[expect(
        clippy::expect_used,
        reason = "test fixture failures stop the individual queue accounting test with local context"
    )]
    #[test]
    fn permanent_failure_leaves_queue_pending_count_at_zero() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        let conn = pool.get().expect("db connection");
        let mut saw_permanent_failure = false;

        for _ in 0..8 {
            claim_job(&queue, &conn, job_id);
            let failure_state =
                crate::db::fail_job(&conn, job_id, "persistent failure").expect("fail job");
            queue.mark_job_failure_state(failure_state);
            if failure_state == crate::db::JobFailureState::PermanentlyFailed {
                saw_permanent_failure = true;
                break;
            }
            assert_eq!(
                crate::db::pending_job_count(&conn).expect("pending jobs"),
                1
            );
            assert_eq!(queue.pending_count(), 1);
        }

        assert!(saw_permanent_failure);
        assert_eq!(
            crate::db::pending_job_count(&conn).expect("pending jobs"),
            0
        );
        assert_eq!(queue.pending_count(), 0);
    }

    #[expect(
        clippy::expect_used,
        reason = "the async retry test requires every timed and database fixture step to succeed"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_retries_the_same_job_after_pool_exhaustion() {
        let pool = single_connection_test_pool(Duration::from_millis(25));
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        {
            let conn = pool.get().expect("claim connection");
            claim_job(&queue, &conn, job_id);
        }

        let held_connection = pool.get().expect("exhaust pool");
        let completion_pool = pool.clone();
        let completion = tokio::spawn(async move {
            persist_job_completion(
                0,
                completion_pool,
                job_id,
                None,
                JobCompletion::Done,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !completion.is_finished(),
            "completion must wait and retry while the pool is exhausted"
        );
        drop(held_connection);

        let failure_state = tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .expect("completion retry timed out")
            .expect("completion task panicked")
            .expect("completion persistence failed");
        assert_eq!(failure_state, None);

        let conn = pool.get().expect("verification connection");
        let status: String = conn
            .query_row(
                "SELECT status FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |row| row.get(0),
            )
            .expect("job status");
        assert_eq!(status, "done");
    }

    #[expect(
        clippy::expect_used,
        reason = "the shutdown recovery test requires every timed and database fixture step to succeed"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completion_stops_retrying_after_shutdown_grace_and_startup_can_recover() {
        let pool = single_connection_test_pool(Duration::from_millis(25));
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        {
            let conn = pool.get().expect("claim connection");
            claim_job(&queue, &conn, job_id);
        }

        let held_connection = pool.get().expect("exhaust pool");
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let completion = tokio::time::timeout(
            Duration::from_secs(1),
            persist_job_completion(0, pool.clone(), job_id, None, JobCompletion::Done, cancel),
        )
        .await
        .expect("shutdown-bounded completion timed out");
        assert!(completion.is_err());
        drop(held_connection);

        let conn = pool.get().expect("recovery connection");
        let recovery =
            crate::db::recover_interrupted_background_jobs(&conn).expect("recover interrupted job");
        assert_eq!(recovery.jobs_reset, 1);
        let status: String = conn
            .query_row(
                "SELECT status FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
                |row| row.get(0),
            )
            .expect("recovered job status");
        assert_eq!(status, "pending");
    }

    #[expect(
        clippy::expect_used,
        reason = "the completion recovery fixture must resolve within its explicit test timeout"
    )]
    #[tokio::test]
    async fn already_resolved_completion_error_does_not_wedge_worker() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        {
            let conn = pool.get().expect("claim connection");
            claim_job(&queue, &conn, job_id);
            conn.execute(
                "DELETE FROM background_jobs WHERE id = ?1",
                rusqlite::params![job_id],
            )
            .expect("delete claimed job");
        }

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            persist_job_completion(
                0,
                pool,
                job_id,
                None,
                JobCompletion::Done,
                tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .expect("permanent completion error must not loop")
        .expect("already-resolved job is not a persistence failure");
        assert_eq!(result, None);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the regression keeps job seeding, terminal persistence, and replay checks in one end-to-end scenario"
    )]
    #[tokio::test]
    async fn non_transient_done_completion_is_terminal_and_never_replayed() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool().context("create test pool")?;
        let queue = JobQueue::new(pool.clone());
        let post_id = {
            let conn = pool.get().context("get seed connection")?;
            conn.execute(
                "INSERT INTO boards (short_name, name, description)
                 VALUES ('finished', 'Finished', '')",
                [],
            )
            .context("insert terminal-state board")?;
            let board_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'finished thread')",
                rusqlite::params![board_id],
            )
            .context("insert terminal-state thread")?;
            let thread_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token,
                  is_op, file_path, media_processing_state)
                 VALUES (?1, ?2, 'anon', 'body', 'body', 'token', 1,
                         'finished/final.webm', ?3)",
                rusqlite::params![thread_id, board_id, ""],
            )
            .context("insert terminal-state media post")?;
            conn.last_insert_rowid()
        };
        let job = Job::VideoTranscode {
            post_id,
            file_path: "finished/final.webm".to_owned(),
            board_short: "finished".to_owned(),
        };
        let job_id = match queue.enqueue(&job).context("enqueue completed media job")? {
            EnqueueOutcome::Enqueued(id) => id,
            EnqueueOutcome::DroppedAtCapacity => bail!("completed media job was dropped"),
        };
        {
            let conn = pool.get().context("get claim connection")?;
            conn.execute(
                "UPDATE background_jobs SET attempts = 2 WHERE id = ?1",
                rusqlite::params![job_id],
            )
            .context("place job at final claimable attempt")?;
            claim_job(&queue, &conn, job_id);
            conn.execute_batch(
                "CREATE TRIGGER reject_done_completion
                 BEFORE UPDATE OF status ON background_jobs
                 WHEN NEW.status = 'done'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected completion failure');
                 END;",
            )
            .context("install completion failure trigger")?;
        }

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            persist_job_completion(
                0,
                pool.clone(),
                job_id,
                Some(media_job_identity(&job).context("media job identity")?),
                JobCompletion::Done,
                tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .context("completion recovery timed out")?
        .context("transition job to a safe terminal state")?;
        ensure!(result == Some(crate::db::JobFailureState::PermanentlyFailed));

        let conn = pool.get().context("get verification connection")?;
        let (status, attempts, error, file_path, media_state): (
            String,
            i64,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT j.status, j.attempts, j.last_error,
                        p.file_path, p.media_processing_state
                 FROM background_jobs j
                 JOIN posts p ON p.id = ?2
                 WHERE j.id = ?1",
                rusqlite::params![job_id, post_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .context("load terminal job state")?;
        ensure!(status == "failed");
        ensure!(attempts == 3);
        ensure!(error.contains("injected completion failure"));
        ensure!(file_path == "finished/final.webm");
        ensure!(media_state.is_empty());
        ensure!(
            crate::db::claim_next_job(&conn)
                .context("query pending jobs")?
                .is_none(),
            "successfully applied work must never be replayed"
        );
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "the injected retry-state fixture requires each database and timeout step to succeed"
    )]
    #[tokio::test]
    async fn non_transient_failed_completion_preserves_bounded_attempt_count() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let queue = JobQueue::new(pool.clone());
        let job_id = enqueue_spam_job(&queue).expect("enqueue spam-check fixture");
        {
            let conn = pool.get().expect("claim connection");
            claim_job(&queue, &conn, job_id);
            conn.execute_batch(
                "CREATE TRIGGER reject_original_job_failure
                 BEFORE UPDATE OF status ON background_jobs
                 WHEN NEW.last_error = 'work failed'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected failure persistence error');
                 END;",
            )
            .expect("install failure persistence trigger");
        }

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            persist_job_completion(
                0,
                pool.clone(),
                job_id,
                None,
                JobCompletion::Failed("work failed".to_owned()),
                tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .expect("recovery timed out")
        .expect("failed job should retain its normal retry policy");
        assert_eq!(result, Some(crate::db::JobFailureState::Retrying));

        let conn = pool.get().expect("verification connection");
        let (status, attempts, error): (String, i64, String) = conn
            .query_row(
                "SELECT status, attempts, last_error
                 FROM background_jobs
                 WHERE id = ?1",
                rusqlite::params![job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("recovered failed-job state");
        assert_eq!(status, "pending");
        assert_eq!(attempts, 1);
        assert!(error.contains("work failed"));
        assert!(error.contains("injected failure persistence error"));
    }

    #[test]
    fn stale_media_completion_preserves_newer_pending_state() -> anyhow::Result<()> {
        let pool = crate::db::init_test_pool().context("create test pool")?;
        let queue = JobQueue::new(pool.clone());
        let post_id = {
            let conn = pool.get().context("get seed connection")?;
            conn.execute(
                "INSERT INTO boards (short_name, name, description)
                 VALUES ('media', 'Media', '')",
                [],
            )
            .context("insert media board")?;
            let board_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'media thread')",
                rusqlite::params![board_id],
            )
            .context("insert media thread")?;
            let thread_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token,
                  is_op, file_path, media_processing_state)
                 VALUES (?1, ?2, 'anon', 'body', 'body', 'token', 1,
                         'media/new.mp4', ?3)",
                rusqlite::params![thread_id, board_id, crate::db::MEDIA_PROCESSING_PENDING],
            )
            .context("insert media post")?;
            conn.last_insert_rowid()
        };
        let job = Job::VideoTranscode {
            post_id,
            file_path: "media/old.mp4".to_owned(),
            board_short: "media".to_owned(),
        };
        let payload = serde_json::to_string(&job)?;
        let job_id = {
            let conn = pool.get().context("get legacy enqueue connection")?;
            crate::db::enqueue_job(&conn, job.type_str(), &payload)?
        };
        {
            let conn = pool.get().context("get claim connection")?;
            claim_job(&queue, &conn, job_id);
        }

        let identity = media_job_identity(&job).context("media job identity")?;
        persist_job_completion_once(&pool, job_id, Some(&identity), &JobCompletion::Stale)
            .context("persist stale completion")?;

        let conn = pool.get().context("get verification connection")?;
        let (job_status, media_state): (String, String) = conn
            .query_row(
                "SELECT j.status, p.media_processing_state
                 FROM background_jobs j
                 JOIN posts p ON p.id = ?2
                 WHERE j.id = ?1",
                rusqlite::params![job_id, post_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("load stale-completion state")?;
        ensure!(job_status == "done");
        ensure!(media_state == crate::db::MEDIA_PROCESSING_PENDING);
        Ok(())
    }

    #[expect(
        clippy::expect_used,
        reason = "temporary-file and database fixture setup must succeed before cleanup assertions"
    )]
    #[test]
    fn stale_transcode_persist_removes_unattached_webm_and_writes_no_hash() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let board_dir = temp_dir.path().join("b");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        let src = board_dir.join("video.mp4");
        std::fs::write(&src, b"source").expect("write source");
        let webm_abs = board_dir.join("video.webm");
        let mut tmp = tempfile::Builder::new()
            .suffix(".webm")
            .tempfile_in(&board_dir)
            .expect("temp webm");
        tmp.write_all(b"webm").expect("write temp webm");
        let pool = crate::db::init_test_pool().expect("test pool");

        let error = persist_transcoded_webm(
            998,
            999,
            "b/video.mp4",
            &src,
            &webm_abs,
            "b/video.webm",
            tmp,
            &pool,
        )
        .expect_err("deleted transcode target rejected");

        assert!(crate::db::is_stale_media_target_error(&error));
        assert!(!webm_abs.exists());
        let conn = pool.get().expect("db connection");
        assert_eq!(file_hash_count(&conn, "b/video.webm"), 0);
    }

    #[expect(
        clippy::expect_used,
        reason = "temporary-file fixture setup must succeed before transcode output assertions"
    )]
    #[test]
    fn transcode_finalise_skips_larger_output_and_cleans_temp_file() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let board_dir = temp_dir.path().join("b");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        let src = board_dir.join("video.mp4");
        std::fs::write(&src, b"small").expect("write source");
        let webm_abs = board_dir.join("video.webm");
        let mut tmp = tempfile::Builder::new()
            .suffix(".webm")
            .tempfile_in(&board_dir)
            .expect("temp webm");
        tmp.write_all(b"larger than source")
            .expect("write temp webm");
        let tmp_path = tmp.path().to_path_buf();
        let pool = crate::db::init_test_pool().expect("test pool");

        transcode_video_finalise(
            998,
            999,
            "b/video.mp4",
            &src,
            &webm_abs,
            "b/video.webm",
            tmp,
            &pool,
        )
        .expect("larger output should be skipped without failing the job");

        assert!(src.exists());
        assert!(!webm_abs.exists());
        assert!(!tmp_path.exists());
    }

    #[test]
    fn webm_reencode_uses_distinct_output_name() {
        assert_eq!(transcoded_webm_name("clip", "mp4"), "clip.webm");
        assert_eq!(transcoded_webm_name("clip", "mkv"), "clip.webm");
        assert_eq!(transcoded_webm_name("clip", "webm"), "clip.vp9.webm");
    }

    #[expect(
        clippy::expect_used,
        reason = "temporary upload fixture setup must succeed before path validation assertions"
    )]
    #[test]
    fn transcode_source_validation_rejects_escape_paths() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_root = temp_dir.path().join("uploads");
        let board_dir = upload_root.join("b");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        std::fs::write(board_dir.join("video.mp4"), b"source").expect("write source");

        assert!(validated_board_media_file(&upload_root, "../video.mp4", "b").is_err());
        assert!(validated_board_media_file(&upload_root, "other/video.mp4", "b").is_err());
        assert!(validated_board_media_file(&upload_root, "b/thumbs/video.mp4", "b").is_err());
        assert!(validated_board_media_file(&upload_root, "b/video.mp4", "b").is_ok());
    }

    #[cfg(unix)]
    #[expect(
        clippy::expect_used,
        reason = "Unix symlink fixture setup must succeed before containment assertions"
    )]
    #[test]
    fn thumb_cache_eviction_skips_symlinked_thumbs_dir() {
        use std::os::unix::fs as unix_fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_root = temp_dir.path().join("uploads");
        let board_dir = upload_root.join("b");
        let outside_dir = temp_dir.path().join("outside");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        std::fs::create_dir_all(&outside_dir).expect("create outside dir");
        let outside_file = outside_dir.join("thumb.webp");
        std::fs::write(&outside_file, b"outside").expect("write outside file");
        unix_fs::symlink(&outside_dir, board_dir.join("thumbs")).expect("symlink thumbs dir");

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("test connection");
        super::evict_thumb_cache(&conn, upload_root.to_str().expect("utf8 upload root"), 0)
            .expect("evict thumbnail cache");

        assert_eq!(
            std::fs::read(&outside_file).expect("read outside"),
            b"outside"
        );
    }

    #[cfg(unix)]
    #[expect(
        clippy::expect_used,
        reason = "Unix symlink fixture setup must succeed before containment assertions"
    )]
    #[test]
    fn thumb_cache_eviction_skips_symlinked_board_dir() {
        use std::os::unix::fs as unix_fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_root = temp_dir.path().join("uploads");
        let outside_board = temp_dir.path().join("outside_board");
        let outside_thumbs = outside_board.join("thumbs");
        std::fs::create_dir_all(&upload_root).expect("create upload root");
        std::fs::create_dir_all(&outside_thumbs).expect("create outside thumbs");
        let outside_file = outside_thumbs.join("thumb.webp");
        std::fs::write(&outside_file, b"outside").expect("write outside file");
        unix_fs::symlink(&outside_board, upload_root.join("b")).expect("symlink board dir");

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("test connection");
        super::evict_thumb_cache(&conn, upload_root.to_str().expect("utf8 upload root"), 0)
            .expect("evict thumbnail cache");

        assert_eq!(
            std::fs::read(&outside_file).expect("read outside"),
            b"outside"
        );
    }

    #[cfg(unix)]
    #[expect(
        clippy::expect_used,
        reason = "Unix symlink fixture setup must succeed before containment assertions"
    )]
    #[test]
    fn thumb_cache_eviction_skips_symlinked_thumb_file() {
        use std::os::unix::fs as unix_fs;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_root = temp_dir.path().join("uploads");
        let thumbs_dir = upload_root.join("b").join("thumbs");
        let outside_file = temp_dir.path().join("outside.webp");
        std::fs::create_dir_all(&thumbs_dir).expect("create thumbs dir");
        std::fs::write(&outside_file, b"outside").expect("write outside file");
        unix_fs::symlink(&outside_file, thumbs_dir.join("thumb.webp")).expect("symlink thumb file");

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("test connection");
        super::evict_thumb_cache(&conn, upload_root.to_str().expect("utf8 upload root"), 0)
            .expect("evict thumbnail cache");

        assert_eq!(
            std::fs::read(&outside_file).expect("read outside"),
            b"outside"
        );
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "fixture setup must succeed before eviction assertions"
    )]
    fn thumb_cache_evicts_only_unreferenced_files_across_all_media_states() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_root = temp_dir.path().join("uploads");
        let thumbs_dir = upload_root.join("b/thumbs");
        std::fs::create_dir_all(&thumbs_dir).expect("create thumbs");
        let referenced_paths = [
            "normal.webp",
            "pending.webp",
            "failed.webp",
            "pruned.webp",
            "shared.webp",
        ];
        for name in referenced_paths
            .iter()
            .chain(std::iter::once(&"orphan.webp"))
        {
            std::fs::write(thumbs_dir.join(name), [0_u8; 4]).expect("write thumbnail");
        }

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("test connection");
        let board_id =
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create board");
        let thread_id: i64 = conn
            .query_row(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'thread') RETURNING id",
                [board_id],
                |row| row.get(0),
            )
            .expect("create thread");
        let fixtures = [
            (101, "normal.webp", ""),
            (102, "pending.webp", crate::db::MEDIA_PROCESSING_PENDING),
            (103, "failed.webp", crate::db::MEDIA_PROCESSING_FAILED),
            (104, "pruned.webp", crate::db::MEDIA_ORIGINAL_PRUNED),
            (105, "shared.webp", ""),
            (106, "shared.webp", crate::db::MEDIA_ORIGINAL_PRUNED),
        ];
        for (post_id, thumb_name, state) in fixtures {
            conn.execute(
                "INSERT INTO posts (
                    id, thread_id, board_id, name, body, body_html, file_path,
                    file_name, file_size, thumb_path, mime_type, deletion_token,
                    is_op, media_type, media_processing_state, created_at
                 ) VALUES (
                    ?1, ?2, ?3, 'anon', 'body', 'body', ?4, 'file.webp', 4,
                    ?5, 'image/webp', ?6, 0, 'image', ?7, ?1
                 )",
                rusqlite::params![
                    post_id,
                    thread_id,
                    board_id,
                    format!("b/file-{post_id}.webp"),
                    format!("b/thumbs/{thumb_name}"),
                    format!("token-{post_id}"),
                    state,
                ],
            )
            .expect("insert media post");
        }

        let upload_dir = upload_root.to_str().expect("UTF-8 upload root");
        let first = super::evict_thumb_cache(&conn, upload_dir, 0).expect("first eviction");
        assert_eq!(first.total_before_bytes, 24);
        assert_eq!(first.total_after_bytes, 20);
        assert_eq!(first.removed_files, 1);
        assert!(!thumbs_dir.join("orphan.webp").exists());
        for name in referenced_paths {
            assert!(
                thumbs_dir.join(name).exists(),
                "referenced thumbnail {name} must remain"
            );
        }

        let second = super::evict_thumb_cache(&conn, upload_dir, 0).expect("second eviction");
        assert_eq!(second.total_before_bytes, 20);
        assert_eq!(second.total_after_bytes, 20);
        assert_eq!(second.removed_files, 0, "fixed point is idempotent");
    }

    #[expect(
        clippy::expect_used,
        reason = "temporary-file and database fixture setup must succeed before cleanup assertions"
    )]
    #[test]
    fn stale_waveform_finalise_removes_unattached_png_and_writes_no_hash() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let board_dir = temp_dir.path().join("b");
        let thumbs_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumbs_dir).expect("create thumbs dir");
        let src = board_dir.join("audio.mp3");
        std::fs::write(&src, b"source").expect("write source");
        let png_abs = thumbs_dir.join("audio.png");
        let mut tmp = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(&thumbs_dir)
            .expect("temp png");
        tmp.write_all(b"png").expect("write temp png");
        let pool = crate::db::init_test_pool().expect("test pool");

        let error = waveform_finalise(
            998,
            999,
            &png_abs,
            "b/thumbs/audio.png",
            &src,
            "b/audio.mp3",
            tmp,
            temp_dir.path(),
            &pool,
        )
        .expect_err("deleted waveform target rejected");

        assert!(crate::db::is_stale_media_target_error(&error));
        assert!(!png_abs.exists());
        let conn = pool.get().expect("db connection");
        assert_eq!(file_hash_count(&conn, "b/audio.mp3"), 0);
        assert_eq!(file_hash_count(&conn, "b/thumbs/audio.png"), 0);
    }

    #[test]
    fn successful_waveform_finalise_removes_obsolete_svg_placeholder() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir().context("create temp directory")?;
        let board_dir = temp_dir.path().join("b");
        let thumbs_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumbs_dir).context("create thumbnail directory")?;
        let src = board_dir.join("audio.mp3");
        std::fs::write(&src, b"source audio").context("write audio source")?;
        let svg_abs = thumbs_dir.join("audio.svg");
        std::fs::write(&svg_abs, b"<svg/>").context("write placeholder")?;
        let png_abs = thumbs_dir.join("audio.png");
        let mut tmp = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(&thumbs_dir)
            .context("create temporary waveform")?;
        tmp.write_all(b"png waveform")
            .context("write temporary waveform")?;
        let pool = crate::db::init_test_pool().context("create test pool")?;
        let post_id = {
            let conn = pool.get().context("get seed connection")?;
            conn.execute(
                "INSERT INTO boards (short_name, name, description)
                 VALUES ('b', 'B', '')",
                [],
            )?;
            let board_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'audio')",
                rusqlite::params![board_id],
            )?;
            let thread_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token, is_op,
                  file_path, file_size, thumb_path, mime_type, media_type)
                 VALUES (?1, ?2, 'anon', '', '', 'token', 1,
                         'b/audio.mp3', 12, 'b/thumbs/audio.svg', 'audio/mpeg', 'audio')",
                rusqlite::params![thread_id, board_id],
            )?;
            let post_id = conn.last_insert_rowid();
            let audio_hash = super::sha256_file_hex(&src)?;
            crate::db::record_file_hash(
                &conn,
                &audio_hash,
                "b/audio.mp3",
                "b/thumbs/audio.svg",
                "audio/mpeg",
            )?;
            post_id
        };

        let queue = JobQueue::new(pool.clone());
        let job = Job::AudioWaveform {
            post_id,
            file_path: "b/audio.mp3".to_owned(),
            board_short: "b".to_owned(),
        };
        let job_id = match queue.enqueue(&job)? {
            EnqueueOutcome::Enqueued(job_id) => job_id,
            EnqueueOutcome::DroppedAtCapacity => bail!("waveform fixture dropped at capacity"),
        };
        {
            let conn = pool.get()?;
            claim_job(&queue, &conn, job_id);
        }
        waveform_finalise(
            job_id,
            post_id,
            &png_abs,
            "b/thumbs/audio.png",
            &src,
            "b/audio.mp3",
            tmp,
            temp_dir.path(),
            &pool,
        )?;

        ensure!(png_abs.exists(), "waveform PNG was not installed");
        ensure!(
            !svg_abs.exists(),
            "obsolete SVG placeholder was not removed"
        );
        let conn = pool.get().context("get verification connection")?;
        let thumb_path: String = conn.query_row(
            "SELECT thumb_path FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| row.get(0),
        )?;
        ensure!(thumb_path == "b/thumbs/audio.png");
        Ok(())
    }

    #[test]
    fn video_reencode_timeout_warning_mentions_admin_guidance() {
        let warning = video_reencode_timeout_warning(42, 600);
        assert!(warning.contains("ffmpeg video re-encoding/conversion timed out"));
        assert!(warning.contains("10 minutes"));
        assert!(warning.contains("(600s)"));
        assert!(warning.contains("Raspberry Pi"));
        assert!(warning.contains("admin panel"));
        assert!(warning.contains("settings.toml"));
    }
}
