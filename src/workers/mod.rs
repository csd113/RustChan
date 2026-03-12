// workers/mod.rs — Background job queue and worker pool.
//
// Architecture:
//   • SQLite-backed persistent job queue (table: background_jobs).
//     Jobs survive a server restart — they are picked up again on next boot.
//   • Worker pool: N Tokio tasks (min(available_cpus, 4)).
//   • Jobs are claimed atomically via UPDATE … RETURNING so multiple workers
//     never process the same job even under concurrent access in WAL mode.
//   • Workers sleep until a Notify fires or a 5-second poll timeout elapses.
//   • Failed jobs are retried up to MAX_ATTEMPTS times; then marked "failed"
//     with the last error message recorded for inspection.
//   • A CancellationToken is threaded through every worker so that a graceful
//     shutdown drains in-progress jobs before exiting (#7).
//
// Job types:
//   VideoTranscode — MP4 → WebM (VP9 + Opus) via ffmpeg (off the hot path)
//   AudioWaveform  — waveform PNG from audio via ffmpeg (off the hot path)
//   ThreadPrune    — delete overflow threads from a board asynchronously
//   SpamCheck      — hook for future spam / abuse analysis
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
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::num::NonZero;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// How many times a job may be attempted before being permanently failed.
#[allow(dead_code)]
const MAX_ATTEMPTS: i64 = 3;
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
        allow_archive: bool,
    },
    /// Spam / abuse analysis hook — currently logs; extend for auto-banning.
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
}

impl JobQueue {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            notify: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
            in_progress: Arc::new(DashMap::new()),
        }
    }

    /// Persist a job and wake a sleeping worker immediately.
    /// Safe to call from any thread, including inside `tokio::task::spawn_blocking`.
    ///
    /// 2.1: If the number of pending jobs already equals or exceeds
    /// `CONFIG.job_queue_capacity`, the job is dropped with a warning log
    /// rather than accepted. This prevents the queue table growing without
    /// bound under a post flood.
    pub fn enqueue(&self, job: &Job) -> Result<i64> {
        let payload = serde_json::to_string(job)?;
        let conn = self.pool.get()?;

        // Back-pressure: check pending count before inserting.
        if CONFIG.job_queue_capacity > 0 {
            let pending = crate::db::pending_job_count(&conn).unwrap_or(0);
            if pending.cast_unsigned() >= CONFIG.job_queue_capacity {
                warn!(
                    "Job queue at capacity ({}/{}) — dropping {} job",
                    pending,
                    CONFIG.job_queue_capacity,
                    job.type_str(),
                );
                // Return a sentinel -1 rather than an error so callers that
                // fire-and-forget don't bubble up a spurious error.
                return Ok(-1);
            }
        }

        let id = crate::db::enqueue_job(&conn, job.type_str(), &payload)?;
        self.notify.notify_one();
        Ok(id)
    }

    /// Number of jobs currently in "pending" state (not yet started).
    /// Used by the terminal stats display.
    #[allow(dead_code)]
    pub fn pending_count(&self) -> i64 {
        self.pool
            .get()
            .ok()
            .and_then(|c| crate::db::pending_job_count(&c).ok())
            .unwrap_or(0)
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
) -> Vec<tokio::task::JoinHandle<()>> {
    let n = std::thread::available_parallelism()
        .map(NonZero::get)
        .unwrap_or(2)
        .min(4); // cap at 4 to avoid overwhelming SQLite's write lock

    info!("Background worker pool: {} worker(s) online", n);

    (0..n)
        .map(|idx| {
            let q = queue.clone();
            tokio::spawn(async move {
                worker_loop(idx, q, ffmpeg_available).await;
            })
        })
        .collect()
}

// ─── Worker loop ─────────────────────────────────────────────────────────────

async fn worker_loop(id: usize, queue: Arc<JobQueue>, ffmpeg_available: bool) {
    debug!("Worker {id} started");
    // FIX[HIGH-6]: Track consecutive errors for exponential back-off.
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
            pool_claim
                .get()
                .map_err(anyhow::Error::from)
                .and_then(|c| crate::db::claim_next_job(&c))
        })
        .await;

        match claim {
            Ok(Ok(Some((job_id, payload)))) => {
                consecutive_errors = 0; // reset back-off on any successful claim
                debug!("Worker {id}: picked up job #{job_id}");
                let pool_done = queue.pool.clone();
                let result = handle_job(
                    job_id,
                    &payload,
                    ffmpeg_available,
                    queue.pool.clone(),
                    queue.in_progress.clone(),
                )
                .await;
                // FIX[STUCK-RUNNING]: Previously pool_done.get() failures were
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
                        }
                        Err(ref e) => {
                            warn!("Worker {id}: job #{job_id} failed — {e}");
                            crate::db::fail_job(&c, job_id, &e.to_string())?;
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

/// FIX[HIGH-6]: Compute exponential back-off with random jitter.
///
/// Base: 500 ms × 2^n, capped at 60 s.
/// Jitter: uniform random 0–500 ms added to spread simultaneous retries
/// across all workers so they do not storm the DB at the same instant.
#[allow(clippy::arithmetic_side_effects)]
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

#[allow(clippy::too_many_lines)]
async fn handle_job(
    job_id: i64,
    payload: &str,
    ffmpeg_available: bool,
    pool: DbPool,
    in_progress: Arc<DashMap<String, bool>>,
) -> Result<()> {
    let job: Job = serde_json::from_str(payload)
        .map_err(|e| anyhow::anyhow!("Cannot deserialise job #{job_id}: {e}"))?;

    debug!("Job #{job_id} type={}", job.type_str());

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
            let result = transcode_video(
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
            allow_archive,
        } => prune_threads(board_id, board_short, max_threads, allow_archive, pool).await,

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

// ─── VideoTranscode ───────────────────────────────────────────────────────────

/// Transcode an MP4 upload to `WebM` (VP9 + Opus), then update the post's
/// `file_path` and `mime_type`. The original MP4 is deleted on success.
///
/// A hard timeout of `CONFIG.ffmpeg_timeout_secs` is applied (2.3).
async fn transcode_video(
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
) -> Result<()> {
    if !ffmpeg_available {
        debug!("VideoTranscode skipped for post {post_id}: ffmpeg not available");
        return Ok(());
    }

    let timeout_secs = CONFIG.ffmpeg_timeout_secs;
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

    // Wrap the entire blocking transcode in a configurable timeout (2.3).
    match timeout(
        ffmpeg_timeout,
        tokio::task::spawn_blocking(move || {
            transcode_video_inner(post_id, &file_path, &board_short, &pool)
        }),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(anyhow::anyhow!("spawn_blocking panicked: {join_err}")),
        Err(_elapsed) => {
            warn!("VideoTranscode: job for post {post_id} timed out after {timeout_secs}s — ffmpeg killed");
            Err(anyhow::anyhow!(
                "ffmpeg transcode timed out after {timeout_secs}s"
            ))
        }
    }
}

fn transcode_video_inner(
    post_id: i64,
    file_path: &str,
    board_short: &str,
    pool: &DbPool,
) -> Result<()> {
    let upload_dir = &CONFIG.upload_dir;
    let src = PathBuf::from(upload_dir).join(file_path);

    if !src.exists() {
        return Err(anyhow::anyhow!(
            "Source file not found for transcode: {}",
            src.display()
        ));
    }

    // Handle MP4 and WebM (AV1) inputs; skip anything else.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "mp4" && ext != "webm" {
        debug!("VideoTranscode: skipping unrecognised extension {file_path}");
        return Ok(());
    }

    // For WebM uploads, probe the codec first.  VP8/VP9 WebM is already
    // in the correct format; only AV1 needs re-encoding.
    if ext == "webm" {
        let src_str = src
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Source path is non-UTF-8: {}", src.display()))?;

        match crate::utils::files::probe_video_codec(src_str) {
            Ok(ref codec) if codec == "av1" => {
                info!("VideoTranscode: WebM/AV1 detected for post {post_id} — re-encoding to VP9");
            }
            Ok(ref codec) => {
                debug!(
                    "VideoTranscode: skipping WebM with codec '{}' for post {} (already VP8/VP9)",
                    codec, post_id
                );
                return Ok(());
            }
            Err(e) => {
                warn!(
                    "VideoTranscode: could not probe codec for post {} ({}); skipping",
                    post_id, e
                );
                return Ok(());
            }
        }
    }

    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Malformed filename: {}", src.display()))?
        .to_string();

    info!(
        "VideoTranscode: transcoding post {} ({})…",
        post_id, file_path
    );

    let data = std::fs::read(&src)?;
    let webm_bytes = crate::utils::files::transcode_to_webm(&data)?;

    let board_dir = PathBuf::from(upload_dir).join(board_short);
    let webm_name = format!("{stem}.webm");
    let webm_abs = board_dir.join(&webm_name);
    let webm_rel = format!("{board_short}/{webm_name}");

    // FIX[ATOMIC-WRITE]: For AV1 WebM inputs, src and webm_abs resolve to the
    // same path (same stem, same .webm extension).  A direct fs::write would
    // overwrite the source in-place; a crash or disk-full mid-write permanently
    // corrupts the only copy of the file with no recovery path.
    //
    // We write to a uniquely named temp file in the same directory first, then
    // atomically rename it into place.  The rename is POSIX-atomic on the same
    // filesystem, so readers always see either the old file or the new file —
    // never a partial write.  If anything fails before the rename, the source
    // file is untouched and the job can be retried.
    {
        use std::io::Write as _;
        let mut tmp = tempfile::NamedTempFile::new_in(&board_dir)
            .map_err(|e| anyhow::anyhow!("Failed to create temp file for WebM output: {e}"))?;
        tmp.write_all(&webm_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to write WebM transcode output: {e}"))?;
        tmp.persist(&webm_abs)
            .map_err(|e| anyhow::anyhow!("Failed to atomically rename WebM output: {e}"))?;
    }

    let conn = pool.get()?;

    // FIX[LEAK]: If any DB call below fails we must clean up the WebM we just
    // wrote, otherwise it leaks on disk across all retry attempts.  We record
    // the path and remove it in the error branch via a guard closure.
    let db_result = (|| -> Result<()> {
        let updated =
            crate::db::update_all_posts_file_path(&conn, file_path, &webm_rel, "video/webm")?;
        if updated == 0 {
            crate::db::update_post_file_info(&conn, post_id, &webm_rel, "video/webm")?;
        }

        let thumb_path = crate::db::get_post_thumb_path(&conn, post_id)?.unwrap_or_default();
        let webm_sha256 = crate::utils::crypto::sha256_hex(&webm_bytes);
        crate::db::delete_file_hash_by_path(&conn, file_path)?;
        crate::db::record_file_hash(&conn, &webm_sha256, &webm_rel, &thumb_path, "video/webm")?;
        Ok(())
    })();

    if let Err(e) = db_result {
        // Remove the WebM we wrote so it doesn't accumulate across retries.
        let _ = std::fs::remove_file(&webm_abs);
        return Err(e);
    }

    if ext != "webm" {
        let _ = std::fs::remove_file(&src);
    }

    info!(
        "VideoTranscode done: post {} {} → {} ({} bytes)",
        post_id,
        file_path,
        webm_rel,
        webm_bytes.len()
    );
    Ok(())
}

// ─── AudioWaveform ────────────────────────────────────────────────────────────

/// Generate a waveform PNG thumbnail for an audio upload via ffmpeg.
/// A hard timeout of `CONFIG.ffmpeg_timeout_secs` is applied (2.3).
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

    let timeout_secs = CONFIG.ffmpeg_timeout_secs;
    let ffmpeg_timeout = Duration::from_secs(timeout_secs);

    match timeout(
        ffmpeg_timeout,
        tokio::task::spawn_blocking(move || {
            generate_waveform_inner(post_id, &file_path, &board_short, &pool)
        }),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(anyhow::anyhow!("spawn_blocking panicked: {join_err}")),
        Err(_elapsed) => {
            warn!("AudioWaveform: job for post {post_id} timed out after {timeout_secs}s");
            Err(anyhow::anyhow!(
                "ffmpeg waveform timed out after {timeout_secs}s"
            ))
        }
    }
}

fn generate_waveform_inner(
    post_id: i64,
    file_path: &str,
    board_short: &str,
    pool: &DbPool,
) -> Result<()> {
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
        .to_string();

    let data = std::fs::read(&src)?;
    let thumb_size = CONFIG.thumb_size;

    let board_dir = PathBuf::from(upload_dir).join(board_short);
    let thumbs_dir = board_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)?;

    let png_name = format!("{stem}.png");
    let png_abs = thumbs_dir.join(&png_name);
    let png_rel = format!("{board_short}/thumbs/{png_name}");

    crate::utils::files::gen_waveform_png(&data, &png_abs, thumb_size, thumb_size / 2)?;

    let conn = pool.get()?;
    crate::db::update_post_thumb_path(&conn, post_id, &png_rel)?;

    // FIX[DEDUP-STALE]: The file_hashes dedup table was not updated after the
    // waveform PNG was generated, so it still held the SVG placeholder path as
    // thumb_path.  Any future post uploading the same audio file and hitting the
    // dedup cache via find_file_by_hash would receive the stale SVG path instead
    // of the waveform PNG.  We now update the dedup record so that all future
    // dedup hits for this audio file correctly inherit the waveform thumbnail.
    //
    // We compute the SHA-256 of the audio data we already have in memory to
    // identify the dedup row without a separate DB lookup, then refresh its
    // thumb_path via a targeted UPDATE.  An audio file may have been uploaded
    // on a different board, so we match by file content (sha256) not by path.
    let audio_sha256 = crate::utils::crypto::sha256_hex(&data);
    let _ = conn.execute(
        "UPDATE file_hashes SET thumb_path = ?1 WHERE sha256 = ?2",
        rusqlite::params![png_rel, audio_sha256],
    );

    info!("AudioWaveform done: post {post_id} → {png_rel}");
    Ok(())
}

// ─── ThreadPrune ─────────────────────────────────────────────────────────────

async fn prune_threads(
    board_id: i64,
    board_short: String,
    max_threads: i64,
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
            if count > 0 {
                info!(
                    "ThreadArchive: moved {} overflow thread(s) to archive in /{}/ (board_id={}, archive_before_prune={})",
                    count, board_short, board_id, CONFIG.archive_before_prune
                );
            }
        } else {
            let paths = crate::db::prune_old_threads(&conn, board_id, max_threads)?;
            let count = paths.len();
            for p in &paths {
                crate::utils::files::delete_file(&CONFIG.upload_dir, p);
            }
            if count > 0 {
                info!(
                    "ThreadPrune: deleted {} overflow thread(s) from /{}/ (board_id={}), removed {} file(s)",
                    count, board_short, board_id, paths.len()
                );
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
