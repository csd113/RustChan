// Jobs persist in SQLite and are claimed atomically with UPDATE … RETURNING.
// Cancellation tokens let graceful shutdown drain in-progress work; failures
// retain their last error and retry only within the shared budget.

mod media_jobs;
mod recovery;
mod thread_prune;
mod thumb_cache;

pub use media_jobs::cleanup_recovered_waveform_placeholders;
pub(crate) use recovery::MEDIA_RECONCILE_BATCH;
pub use recovery::{reconcile_media_job_states, MediaReconciliation};
pub(crate) use thread_prune::{
    normalize_legacy_thread_prune_jobs, reconcile_thread_prune_intents, ThreadPruneReconciliation,
    THREAD_PRUNE_RECONCILE_BATCH,
};
pub use thumb_cache::{evict_thumb_cache, ThumbCacheEvictionReport};

use media_jobs::{generate_waveform, transcode_video};
use thread_prune::prune_threads;

use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::{Context as _, Result};
use dashmap::DashMap;
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use std::num::NonZero;
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

/// Original module target retained by relocated media-execution log events.
const WORKER_LOG_TARGET: &str = module_path!();

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
    /// Coordinate complete jobs with backup and restore.
    pub(crate) data_access: crate::middleware::data_access::DataAccess,
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
    pub(crate) fn new(
        pool: DbPool,
        data_access: crate::middleware::data_access::DataAccess,
    ) -> Self {
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
            data_access,
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
    let recovery_result = crate::middleware::data_access::spawn_blocking(move || {
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
        let result = crate::middleware::data_access::spawn_blocking(move || {
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

        let Some(access) = queue.data_access.try_read() else {
            tokio::select! {
                () = sleep(Duration::from_millis(100)) => {},
                () = queue.cancel.cancelled() => return,
            }
            continue;
        };
        let outcome = crate::middleware::data_access::scope(access, async {
            // Atomically claim the next pending job (UPDATE … RETURNING).
            let pool_claim = queue.pool.clone();
            let claim = crate::middleware::data_access::spawn_blocking(move || {
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
                                    return std::ops::ControlFlow::Break(());
                                }
                            }
                            return std::ops::ControlFlow::Continue(());
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
                        Ok(JobExecution::Persisted) => return std::ops::ControlFlow::Continue(()),
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
                            return std::ops::ControlFlow::Break(());
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
                            return std::ops::ControlFlow::Break(());
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!("Worker {id}: DB error while claiming job: {e}");
                    let delay = backoff_duration(consecutive_errors);
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    tokio::select! {
                        () = sleep(delay) => {}
                        () = queue.cancel.cancelled() => { return std::ops::ControlFlow::Break(()); }
                    }
                }
                Err(e) => {
                    error!("Worker {id}: panic in spawn_blocking: {e}");
                    let delay = backoff_duration(consecutive_errors);
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    tokio::select! {
                        () = sleep(delay) => {}
                        () = queue.cancel.cancelled() => { return std::ops::ControlFlow::Break(()); }
                    }
                }
            }
            std::ops::ControlFlow::Continue(())
        })
        .await;
        if outcome.is_break() {
            return;
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

/// Builds a collision-free `WebM` filename for the transcoded output.
fn transcoded_webm_name(stem: &str, source_ext: &str) -> String {
    if source_ext.eq_ignore_ascii_case("webm") {
        format!("{stem}.vp9.webm")
    } else {
        format!("{stem}.webm")
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
