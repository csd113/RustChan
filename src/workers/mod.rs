// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#![allow(clippy::too_many_lines)]

// workers/mod.rs — Background job queue and worker pool.
//
// Architecture:
//   • SQLite-backed persistent job queue (table: background_jobs).
//     Jobs survive a server restart — they are picked up again on next boot.
//   • Worker pool: N Tokio tasks (min(available_cpus, 4)).
//   • Jobs are claimed atomically via UPDATE … RETURNING so multiple workers
//     never process the same job even under concurrent access in WAL mode.
//   • Workers sleep until a Notify fires or a 5-second poll timeout elapses.
//   • Failed jobs are retried up to the shared job retry budget; then marked "failed"
//     with the last error message recorded for inspection.
//   • A CancellationToken is threaded through every worker so that a graceful
//     shutdown drains in-progress jobs before exiting (#7).
//
// Job types:
//   VideoTranscode — MP4 → WebM (VP9 + Opus) via ffmpeg (off the hot path)
//   AudioWaveform  — waveform PNG from audio via ffmpeg (off the hot path)
//   ThreadPrune    — delete overflow threads from a board asynchronously
//   SpamCheck      — lightweight abuse signal logging
//
// Integration (handlers):
//   1. save_upload() saves the raw file and returns processing_pending=true
//      when async post-processing is needed.
//   2. After db::create_post / db::create_thread_with_op, the handler calls
//      job_queue.enqueue(…) with the now-known post_id.
//   3. Workers update posts.file_path / posts.thumb_path on completion.

use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::Result;
use dashmap::DashMap;
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use std::num::NonZero;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

fn ffmpeg_command() -> TokioCommand {
    TokioCommand::new(&CONFIG.ffmpeg_path)
}

// How long a worker sleeps when the queue is empty.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

// ─── Job definitions ──────────────────────────────────────────────────────────

/// All job variants the worker pool can process.
/// Serialised to JSON and stored in `background_jobs.payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "d")]
pub enum Job {
    /// Transcode an uploaded MP4 to `WebM` (VP9 + Opus) via ffmpeg.
    VideoTranscode {
        post_id: i64,
        /// Path relative to `CONFIG.upload_dir`, e.g. "b/abc123.mp4"
        file_path: String,
        board_short: String,
    },
    /// Generate a waveform PNG thumbnail for an audio upload via ffmpeg.
    AudioWaveform {
        post_id: i64,
        /// Path relative to `CONFIG.upload_dir`
        file_path: String,
        board_short: String,
    },
    /// Delete overflow threads from a board (runs after a new thread is created).
    ThreadPrune {
        board_id: i64,
        board_short: String,
        max_threads: i64,
        max_archived_threads: i64,
        allow_archive: bool,
    },
    /// Lightweight spam / abuse signal logging.
    SpamCheck {
        post_id: i64,
        ip_hash: String,
        body_len: usize,
    },
}

impl Job {
    /// Short identifier stored in `background_jobs.job_type` for diagnostics.
    pub const fn type_str(&self) -> &'static str {
        match self {
            Self::VideoTranscode { .. } => "video_transcode",
            Self::AudioWaveform { .. } => "audio_waveform",
            Self::ThreadPrune { .. } => "thread_prune",
            Self::SpamCheck { .. } => "spam_check",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Enqueued(i64),
    DroppedAtCapacity,
}

// ─── Job queue ────────────────────────────────────────────────────────────────

/// Cheaply-cloneable handle to the shared job queue.
/// Clone this into every handler that needs to enqueue work.
#[derive(Clone)]
pub struct JobQueue {
    pub pool: DbPool,
    pub notify: Arc<Notify>,
    /// Token cancelled at shutdown; workers observe this to exit cleanly.
    pub cancel: CancellationToken,
    /// 2.2: Set of `file_path` strings for media jobs currently being processed.
    /// Workers check this before starting a `VideoTranscode` or `AudioWaveform` job
    /// and skip if the same path is already in flight, preventing redundant
    /// `FFmpeg` invocations on client retries or server-restart re-queues.
    pub in_progress: Arc<DashMap<String, bool>>,
    pending_jobs: Arc<AtomicU64>,
    dropped_jobs: Arc<AtomicU64>,
    active_video_jobs: Arc<AtomicU64>,
}

impl JobQueue {
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
    /// bound under a post flood.
    pub fn enqueue(&self, job: &Job) -> Result<EnqueueOutcome> {
        let payload = serde_json::to_string(job)?;

        // Back-pressure: check pending count before inserting.
        if self.reserve_pending_slot(job.type_str()) {
            let conn = match self.pool.get() {
                Ok(conn) => conn,
                Err(error) => {
                    self.pending_jobs.fetch_sub(1, Ordering::Relaxed);
                    return Err(error.into());
                }
            };
            match crate::db::enqueue_job(&conn, job.type_str(), &payload) {
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

    /// Number of jobs currently in "pending" state (not yet started).
    /// Used by the terminal stats display.
    pub fn pending_count(&self) -> i64 {
        i64::try_from(self.pending_jobs.load(Ordering::Relaxed)).unwrap_or(i64::MAX)
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped_jobs.load(Ordering::Relaxed)
    }

    pub fn active_video_count(&self) -> u64 {
        self.active_video_jobs.load(Ordering::Relaxed)
    }

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

    fn mark_job_claimed(&self) {
        self.pending_jobs
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                Some(pending.saturating_sub(1))
            })
            .ok();
    }
}

// ─── Worker pool startup ──────────────────────────────────────────────────────

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
    ffmpeg_vp9_available: bool,
) -> Vec<tokio::task::JoinHandle<()>> {
    let n = std::thread::available_parallelism()
        .map_or(2, NonZero::get)
        .min(4); // cap at 4 to avoid overwhelming SQLite's write lock

    tracing::info!(target: "workers", count = n, "Background worker pool online");

    (0..n)
        .map(|idx| {
            let q = std::sync::Arc::clone(queue);
            tokio::spawn(async move {
                worker_loop(idx, q, ffmpeg_available, ffmpeg_vp9_available).await;
            })
        })
        .collect()
}

// ─── Worker loop ─────────────────────────────────────────────────────────────

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
                        let pool_done = queue.pool.clone();
                        let err_msg = format!("Cannot deserialize job payload: {error}");
                        let db_result = tokio::task::spawn_blocking(move || -> Result<()> {
                            let c = pool_done.get().map_err(anyhow::Error::from)?;
                            let _ = crate::db::fail_job(&c, job_id, &err_msg)?;
                            Ok(())
                        })
                        .await;
                        if let Ok(Err(db_error)) = db_result {
                            error!("Worker {id}: failed to mark broken job #{job_id} as failed: {db_error}");
                        }
                        continue;
                    }
                };
                let pool_done = queue.pool.clone();
                let media_post_id = media_job_post_id(&job);
                let result = handle_job(
                    job,
                    ffmpeg_available,
                    ffmpeg_vp9_available,
                    queue.pool.clone(),
                    std::sync::Arc::clone(&queue.in_progress),
                    std::sync::Arc::clone(&queue.active_video_jobs),
                )
                .await;
                // Previously pool_done.get() failures were
                // silently ignored (if let Ok(c) = ...), leaving the job row
                // permanently stuck in "running" — claim_next_job only claims
                // "pending" rows, so it would never be retried or cleaned up.
                // We now propagate the error into the back-off path so the
                // worker retries acquiring a connection, and we log explicitly
                // so operators can see pool exhaustion events.
                let db_result = tokio::task::spawn_blocking(move || -> Result<()> {
                    let c = pool_done.get().map_err(anyhow::Error::from)?;
                    match result {
                        Ok(()) => {
                            crate::db::complete_job(&c, job_id)?;
                            if let Some(post_id) = media_post_id {
                                crate::db::set_post_media_processing_state(
                                    &c, post_id, None, None,
                                )?;
                            }
                        }
                        Err(e) if crate::db::is_stale_media_target_error(&e) => {
                            warn!("Worker {id}: job #{job_id} target is stale — {e}");
                            crate::db::complete_job(&c, job_id)?;
                        }
                        Err(e) => {
                            warn!("Worker {id}: job #{job_id} failed — {e}");
                            let failure_state = crate::db::fail_job(&c, job_id, &e.to_string())?;
                            if let Some(post_id) = media_post_id {
                                match failure_state {
                                    crate::db::JobFailureState::Retrying => {
                                        crate::db::set_post_media_processing_state(
                                            &c,
                                            post_id,
                                            Some(crate::db::MEDIA_PROCESSING_PENDING),
                                            None,
                                        )?;
                                    }
                                    crate::db::JobFailureState::PermanentlyFailed => {
                                        crate::db::set_post_media_processing_state(
                                            &c,
                                            post_id,
                                            Some(crate::db::MEDIA_PROCESSING_FAILED),
                                            Some(&e.to_string()),
                                        )?;
                                    }
                                }
                            }
                        }
                    }
                    Ok(())
                })
                .await;
                match db_result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        error!("Worker {id}: failed to update completion status for job #{job_id}: {e}");
                        let delay = backoff_duration(consecutive_errors);
                        consecutive_errors = consecutive_errors.saturating_add(1);
                        tokio::select! {
                            () = sleep(delay) => {}
                            () = queue.cancel.cancelled() => { return; }
                        }
                    }
                    Err(join_err) => {
                        error!("Worker {id}: spawn_blocking panicked during job #{job_id} completion: {join_err}");
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
    // Use OsRng (already a dependency) for jitter — no new deps required.
    let jitter = u64::from(OsRng.next_u32()) % JITTER_MAX_MS;
    Duration::from_millis(base + jitter)
}

// ─── Job dispatch ─────────────────────────────────────────────────────────────

async fn handle_job(
    job: Job,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
    pool: DbPool,
    in_progress: Arc<DashMap<String, bool>>,
    active_video_jobs: Arc<AtomicU64>,
) -> Result<()> {
    debug!("Dispatching {} job", job.type_str());

    match job {
        Job::VideoTranscode {
            post_id,
            file_path,
            board_short,
        } => {
            // 2.2: Skip if this file_path is already being processed.
            if in_progress.contains_key(&file_path) {
                warn!(
                    "VideoTranscode: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(());
            }
            in_progress.insert(file_path.clone(), true);
            active_video_jobs.fetch_add(1, Ordering::Relaxed);
            let result = transcode_video(
                post_id,
                file_path.clone(),
                board_short,
                ffmpeg_available,
                ffmpeg_vp9_available,
                pool,
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
            // 2.2: Skip if this file_path is already being processed.
            if in_progress.contains_key(&file_path) {
                warn!(
                    "AudioWaveform: skipping duplicate job for post {} ({}): already in flight",
                    post_id, file_path
                );
                return Ok(());
            }
            in_progress.insert(file_path.clone(), true);
            let result = generate_waveform(
                post_id,
                file_path.clone(),
                board_short,
                ffmpeg_available,
                pool,
            )
            .await;
            in_progress.remove(&file_path);
            result
        }

        Job::ThreadPrune {
            board_id,
            board_short,
            max_threads,
            max_archived_threads,
            allow_archive,
        } => {
            prune_threads(
                board_id,
                board_short,
                max_threads,
                max_archived_threads,
                allow_archive,
                pool,
            )
            .await
        }

        Job::SpamCheck {
            post_id,
            ip_hash,
            body_len,
        } => {
            run_spam_check(post_id, &ip_hash, body_len);
            Ok(())
        }
    }
}

const fn media_job_post_id(job: &Job) -> Option<i64> {
    match job {
        Job::VideoTranscode { post_id, .. } | Job::AudioWaveform { post_id, .. } => Some(*post_id),
        Job::ThreadPrune { .. } | Job::SpamCheck { .. } => None,
    }
}

// ─── VideoTranscode ───────────────────────────────────────────────────────────

/// Transcode an MP4 upload to `WebM` (VP9 + Opus), then update the post's
/// `file_path` and `mime_type`. The original MP4 is deleted on success.
///
/// Requires both `ffmpeg_available` (binary present) and `ffmpeg_vp9_available`
/// (libvpx-vp9 + libopus compiled in).  If either flag is false the job is
/// skipped gracefully — no error is returned and the file remains as-is.
///
/// The previous implementation wrapped `std::process::Command`
/// in `spawn_blocking` and applied `tokio::time::timeout`. When the timeout
/// fired, Tokio stopped polling the future but the OS process kept running,
/// occupying a blocking thread until it finished. The log message "ffmpeg killed"
/// was factually incorrect.
///
/// The fix switches to `tokio::process::Command` with `kill_on_drop(true)`.
/// The `Child` handle is driven by `child.wait_with_output()` directly on the
/// async executor. When `timeout` fires, the future (and the `Child` inside it)
/// is dropped, and `kill_on_drop` ensures the OS process receives SIGKILL
/// immediately. No blocking thread is held during the wait.
///
/// A hard timeout of the live `ffmpeg_timeout_secs` setting is applied.
async fn transcode_video(
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    ffmpeg_vp9_available: bool,
    pool: DbPool,
) -> Result<()> {
    if !ffmpeg_available {
        debug!("VideoTranscode skipped for post {post_id}: ffmpeg not available");
        return Ok(());
    }

    if !ffmpeg_vp9_available {
        warn!(
            "VideoTranscode skipped for post {post_id}: libvpx-vp9 or libopus not available. \
             Install ffmpeg with VP9 + Opus support to enable MP4→WebM transcoding."
        );
        return Ok(());
    }

    let timeout_secs = crate::config::ffmpeg_timeout_secs();
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

    // Phase 1: prepare (file checks, codec probe, temp file creation) — blocking.
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
        return Ok(()); // skip gracefully
    };

    // Phase 2: run ffmpeg via tokio::process::Command with kill_on_drop(true).
    // When the timeout future is dropped, the Child is dropped, and kill_on_drop
    // ensures the OS process receives SIGKILL immediately — unlike the previous
    // spawn_blocking approach where the OS process kept running after timeout.
    let child = ffmpeg_command()
        .args(&args)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg '{}': {e}", CONFIG.ffmpeg_path))?;

    match timeout(ffmpeg_timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "ffmpeg exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        Ok(Err(e)) => return Err(anyhow::anyhow!("ffmpeg I/O error: {e}")),
        Err(_elapsed) => {
            // Child is dropped here; kill_on_drop fires SIGKILL on the OS process.
            warn!("{}", video_reencode_timeout_warning(post_id, timeout_secs));
            return Err(anyhow::anyhow!(
                "ffmpeg transcode timed out after {timeout_secs}s"
            ));
        }
    }

    // Phase 3: persist temp file + DB updates — blocking.
    tokio::task::spawn_blocking(move || {
        transcode_video_finalise(
            post_id, &file_path, &src_path, &webm_abs, &webm_rel, tmp, &pool,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in finalise: {e}"))?
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

fn transcode_video_prepare(
    post_id: i64,
    file_path: &str,
    board_short: &str,
) -> Result<Option<TranscodePrepareParts>> {
    let upload_dir = &CONFIG.upload_dir;
    let src = PathBuf::from(upload_dir).join(file_path);

    if !src.exists() {
        return Err(anyhow::anyhow!(
            "Source file not found for transcode: {}",
            src.display()
        ));
    }

    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "mp4" && ext != "webm" {
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

    let board_dir = PathBuf::from(upload_dir).join(board_short);
    let webm_name = format!("{stem}.webm");
    let webm_abs = board_dir.join(&webm_name);
    let webm_rel = format!("{board_short}/{webm_name}");

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

/// Finalise a completed video transcode: persist the temp file atomically,
/// read the result for dedup, and update the database.
///
/// On any DB error the temp-turned-final `WebM` is removed to prevent leaks.
fn transcode_video_finalise(
    post_id: i64,
    file_path: &str,
    src: &PathBuf,
    webm_abs: &PathBuf,
    webm_rel: &str,
    tmp: tempfile::NamedTempFile,
    pool: &DbPool,
) -> Result<()> {
    use anyhow::Context as _;

    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    tmp.persist(webm_abs)
        .map_err(|e| anyhow::anyhow!("Failed to atomically rename WebM output: {e}"))?;

    let webm_bytes =
        std::fs::read(webm_abs).context("Failed to read transcoded WebM for dedup hash")?;

    let conn = pool.get()?;

    // clean up on DB failure.
    let db_result = {
        let webm_sha256 = crate::utils::crypto::sha256_hex(&webm_bytes);
        crate::db::replace_transcoded_media(
            &conn,
            post_id,
            file_path,
            webm_rel,
            "video/webm",
            &webm_sha256,
        )
    };

    if let Err(e) = db_result {
        if let Err(cleanup_error) = std::fs::remove_file(webm_abs) {
            warn!(
                output = %webm_abs.display(),
                error = %cleanup_error,
                "VideoTranscode: failed to remove unattached WebM output"
            );
        }
        return Err(e);
    }

    if ext != "webm" {
        let _ = std::fs::remove_file(src);
    }

    tracing::info!(target: "workers", post_id = post_id, output = %webm_rel, bytes = webm_bytes.len(), "VideoTranscode done");
    Ok(())
}

// ─── AudioWaveform ────────────────────────────────────────────────────────────

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

async fn generate_waveform(
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
) -> Result<()> {
    if !ffmpeg_available {
        return Ok(());
    }

    let timeout_secs = crate::config::ffmpeg_timeout_secs();
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

    // Phase 1: prepare (file I/O, temp file creation) — blocking.
    let (args, png_abs, png_rel, src_path, expected_file_path, tmp_png) = {
        let file_path2 = file_path.clone();
        let board_short2 = board_short.clone();
        tokio::task::spawn_blocking(move || waveform_prepare(&file_path2, &board_short2))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in waveform prepare: {e}"))??
    };

    // Phase 2: run ffmpeg with kill_on_drop.
    let child = ffmpeg_command()
        .args(&args)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn ffmpeg '{}' for waveform: {e}",
                CONFIG.ffmpeg_path
            )
        })?;

    match timeout(ffmpeg_timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "ffmpeg waveform exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        Ok(Err(e)) => return Err(anyhow::anyhow!("ffmpeg waveform I/O error: {e}")),
        Err(_elapsed) => {
            warn!("AudioWaveform: job for post {post_id} timed out after {timeout_secs}s — ffmpeg process killed");
            return Err(anyhow::anyhow!(
                "ffmpeg waveform timed out after {timeout_secs}s"
            ));
        }
    }

    // Phase 3: persist + DB update — blocking.
    tokio::task::spawn_blocking(move || {
        waveform_finalise(
            post_id,
            &png_abs,
            &png_rel,
            &src_path,
            &expected_file_path,
            tmp_png,
            &pool,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("spawn_blocking panicked in waveform finalise: {e}"))?
}

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
fn waveform_finalise(
    post_id: i64,
    png_abs: &std::path::Path,
    png_rel: &str,
    src_path: &std::path::Path,
    expected_file_path: &str,
    tmp_png: tempfile::NamedTempFile,
    pool: &DbPool,
) -> Result<()> {
    use anyhow::Context as _;
    tmp_png
        .persist(png_abs)
        .context("Failed to atomically rename waveform PNG into place")?;
    let conn = pool.get()?;
    if let Err(error) =
        crate::db::update_post_thumb_path(&conn, post_id, expected_file_path, png_rel)
    {
        if let Err(cleanup_error) = std::fs::remove_file(png_abs) {
            warn!(
                output = %png_abs.display(),
                error = %cleanup_error,
                "AudioWaveform: failed to remove unattached waveform output"
            );
        }
        return Err(error);
    }
    // update dedup record with final thumb path.
    let audio_sha256 = sha256_file_hex(src_path)?;
    let _ = conn.execute(
        "UPDATE file_hashes SET thumb_path = ?1 WHERE sha256 = ?2",
        rusqlite::params![png_rel, audio_sha256],
    );

    tracing::info!(target: "workers", post_id = post_id, thumb = %png_rel, "AudioWaveform done");
    Ok(())
}

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

// ─── ThreadPrune ─────────────────────────────────────────────────────────────

async fn prune_threads(
    board_id: i64,
    board_short: String,
    max_threads: i64,
    max_archived_threads: i64,
    allow_archive: bool,
    pool: DbPool,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        // 2.5: archive_before_prune acts as a global safety net — when true,
        // overflow threads are always archived rather than hard-deleted, even
        // on boards where allow_archive = false.  This closes the silent data
        // loss gap where a thread could disappear simply because a board hit
        // its thread limit while the admin had not opted into archiving.
        let do_archive = allow_archive || CONFIG.archive_before_prune;
        if do_archive {
            let count = crate::db::archive_old_threads(&conn, board_id, max_threads)?;
            let deleted =
                crate::db::prune_old_archived_threads(&conn, board_id, max_archived_threads)?;
            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn,
                &CONFIG.upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
            ) {
                tracing::warn!(
                    target: "workers",
                    board = %board_short,
                    board_id = board_id,
                    error = %error,
                    "archived prune cleanup did not fully complete"
                );
            }
            if count > 0 {
                tracing::info!(target: "workers", count = count, board = %board_short, board_id = board_id, "ThreadArchive: threads archived");
            }
            if !deleted.paths.is_empty() {
                tracing::info!(target: "workers", archived_cap = max_archived_threads, board = %board_short, board_id = board_id, files_removed = deleted.paths.len(), "ThreadArchivePrune: archived threads deleted");
            }
        } else {
            let deleted = crate::db::prune_old_threads(&conn, board_id, max_threads)?;
            let count = deleted.paths.len();
            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn,
                &CONFIG.upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
            ) {
                tracing::warn!(
                    target: "workers",
                    board = %board_short,
                    board_id = board_id,
                    error = %error,
                    "thread prune cleanup did not fully complete"
                );
            }
            if count > 0 {
                tracing::info!(target: "workers", count = count, board = %board_short, board_id = board_id, files_removed = deleted.paths.len(), "ThreadPrune: threads deleted");
            }
        }
        Ok(())
    })
    .await?
}

// ─── SpamCheck ────────────────────────────────────────────────────────────────

fn run_spam_check(post_id: i64, ip_hash: &str, body_len: usize) {
    if body_len > 3500 {
        debug!(
            "SpamCheck: post {} body_len={} exceeds 3500 chars (flagged for review)",
            post_id, body_len
        );
    }
    let _ = ip_hash;
}

// ─── Thumbnail / waveform cache eviction ─────────────────────────────────────

/// Walk every board's `thumbs/` subdirectory, collect all files with their
/// modification times, and delete the oldest ones until the total size of
/// the remaining set is under `max_bytes`.
///
/// Only files inside `{upload_dir}/{board}/thumbs/` are considered — original
/// uploads are never touched.  Deletion is best-effort: individual failures
/// are logged and skipped rather than aborting the whole pass.
pub fn evict_thumb_cache(upload_dir: &str, max_bytes: u64) {
    // Collect (mtime_secs, path, size) for every file inside any thumbs/ dir.
    let mut files: Vec<(u64, std::path::PathBuf, u64)> = Vec::new();
    let Ok(boards_iter) = std::fs::read_dir(upload_dir) else {
        return;
    };
    for board_entry in boards_iter.flatten() {
        let thumbs_dir = board_entry.path().join("thumbs");
        if !thumbs_dir.is_dir() {
            continue;
        }
        let Ok(thumbs_iter) = std::fs::read_dir(&thumbs_dir) else {
            continue;
        };
        for entry in thumbs_iter.flatten() {
            let path = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_secs());
                    files.push((mtime, path, meta.len()));
                }
            }
        }
    }

    // Dereference `sz` explicitly and annotate the sum type for
    // clarity.  `Iterator<Item = &u64>` implements `Sum<&u64>` in std so this
    // compiled before, but the explicit form is more readable and avoids the
    // implicit coercion.
    let total: u64 = files.iter().map(|(_, _, sz)| *sz).sum::<u64>();
    if total <= max_bytes {
        return; // already within budget
    }

    // Sort oldest-first so we delete the least-recently-used files first.
    files.sort_unstable_by_key(|(mtime, _, _)| *mtime);

    let mut remaining = total;
    let mut deleted = 0u64;
    let mut deleted_bytes = 0u64;
    for (_, path, size) in &files {
        if remaining <= max_bytes {
            break;
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
}

#[cfg(test)]
mod tests {
    use super::{transcode_video_finalise, video_reencode_timeout_warning, waveform_finalise};
    use std::io::Write as _;

    fn file_hash_count(conn: &rusqlite::Connection, file_path: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE file_path = ?1",
            rusqlite::params![file_path],
            |row| row.get(0),
        )
        .expect("file hash count")
    }

    #[test]
    fn stale_transcode_finalise_removes_unattached_webm_and_writes_no_hash() {
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

        let error = transcode_video_finalise(
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
            999,
            &png_abs,
            "b/thumbs/audio.png",
            &src,
            "b/audio.mp3",
            tmp,
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
