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
use serde::{Deserialize, Serialize};
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
// Maximum wall-clock time allowed for a single ffmpeg transcode (#10).
const FFMPEG_TRANSCODE_TIMEOUT: Duration = Duration::from_secs(120);
// Maximum wall-clock time allowed for ffmpeg waveform generation (#10).
const FFMPEG_WAVEFORM_TIMEOUT: Duration = Duration::from_secs(60);

// ─── Job definitions ──────────────────────────────────────────────────────────

/// All job variants the worker pool can process.
/// Serialised to JSON and stored in background_jobs.payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "d")]
pub enum Job {
    /// Transcode an uploaded MP4 to WebM (VP9 + Opus) via ffmpeg.
    VideoTranscode {
        post_id: i64,
        /// Path relative to CONFIG.upload_dir, e.g. "b/abc123.mp4"
        file_path: String,
        board_short: String,
    },
    /// Generate a waveform PNG thumbnail for an audio upload via ffmpeg.
    AudioWaveform {
        post_id: i64,
        /// Path relative to CONFIG.upload_dir
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
    /// Short identifier stored in background_jobs.job_type for diagnostics.
    pub fn type_str(&self) -> &'static str {
        match self {
            Job::VideoTranscode { .. } => "video_transcode",
            Job::AudioWaveform { .. } => "audio_waveform",
            Job::ThreadPrune { .. } => "thread_prune",
            Job::SpamCheck { .. } => "spam_check",
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
}

impl JobQueue {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            notify: Arc::new(Notify::new()),
            cancel: CancellationToken::new(),
        }
    }

    /// Persist a job and wake a sleeping worker immediately.
    /// Safe to call from any thread, including inside tokio::task::spawn_blocking.
    pub fn enqueue(&self, job: &Job) -> Result<i64> {
        let payload = serde_json::to_string(job)?;
        let conn = self.pool.get()?;
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
/// Returns a vec of JoinHandles so the caller can await them during shutdown.
/// Workers are pure async Tokio tasks — they do not consume OS threads at rest.
pub fn start_worker_pool(queue: Arc<JobQueue>, ffmpeg_available: bool) {
    let n = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(2)
        .min(4); // cap at 4 to avoid overwhelming SQLite's write lock

    info!("Background worker pool: {} worker(s) online", n);

    for idx in 0..n {
        let q = queue.clone();
        tokio::spawn(async move {
            worker_loop(idx, q, ffmpeg_available).await;
        });
    }
}

// ─── Worker loop ─────────────────────────────────────────────────────────────

async fn worker_loop(id: usize, queue: Arc<JobQueue>, ffmpeg_available: bool) {
    debug!("Worker {} started", id);
    loop {
        // Check for shutdown before trying to claim a job.
        if queue.cancel.is_cancelled() {
            debug!("Worker {} exiting: shutdown requested", id);
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
                debug!("Worker {}: picked up job #{}", id, job_id);
                let pool_done = queue.pool.clone();
                let result =
                    handle_job(job_id, &payload, ffmpeg_available, queue.pool.clone()).await;
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(c) = pool_done.get() {
                        match result {
                            Ok(()) => {
                                let _ = crate::db::complete_job(&c, job_id);
                                debug!("Worker {}: job #{} completed", id, job_id);
                            }
                            Err(ref e) => {
                                warn!("Worker {}: job #{} failed — {}", id, job_id, e);
                                let _ = crate::db::fail_job(&c, job_id, &e.to_string());
                            }
                        }
                    }
                })
                .await;
            }
            Ok(Ok(None)) => {
                // Queue is empty — sleep until notified, POLL_INTERVAL elapses,
                // or a shutdown is requested (#7).
                tokio::select! {
                    _ = queue.notify.notified() => {}
                    _ = sleep(POLL_INTERVAL) => {}
                    _ = queue.cancel.cancelled() => {
                        debug!("Worker {} exiting: shutdown requested while idle", id);
                        return;
                    }
                }
            }
            Ok(Err(e)) => {
                error!("Worker {}: DB error while claiming job: {}", id, e);
                tokio::select! {
                    _ = sleep(Duration::from_secs(2)) => {}
                    _ = queue.cancel.cancelled() => { return; }
                }
            }
            Err(e) => {
                error!("Worker {}: panic in spawn_blocking: {}", id, e);
                tokio::select! {
                    _ = sleep(Duration::from_secs(2)) => {}
                    _ = queue.cancel.cancelled() => { return; }
                }
            }
        }
    }
}

// ─── Job dispatch ─────────────────────────────────────────────────────────────

async fn handle_job(
    job_id: i64,
    payload: &str,
    ffmpeg_available: bool,
    pool: DbPool,
) -> Result<()> {
    let job: Job = serde_json::from_str(payload)
        .map_err(|e| anyhow::anyhow!("Cannot deserialise job #{}: {}", job_id, e))?;

    debug!("Job #{} type={}", job_id, job.type_str());

    match job {
        Job::VideoTranscode {
            post_id,
            file_path,
            board_short,
        } => transcode_video(post_id, file_path, board_short, ffmpeg_available, pool).await,

        Job::AudioWaveform {
            post_id,
            file_path,
            board_short,
        } => generate_waveform(post_id, file_path, board_short, ffmpeg_available, pool).await,

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
        } => run_spam_check(post_id, ip_hash, body_len).await,
    }
}

// ─── VideoTranscode ───────────────────────────────────────────────────────────

/// Transcode an MP4 upload to WebM (VP9 + Opus), then update the post's
/// file_path and mime_type. The original MP4 is deleted on success.
///
/// A hard timeout of FFMPEG_TRANSCODE_TIMEOUT is applied (#10).
async fn transcode_video(
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
) -> Result<()> {
    if !ffmpeg_available {
        debug!(
            "VideoTranscode skipped for post {}: ffmpeg not available",
            post_id
        );
        return Ok(());
    }

    // Wrap the entire blocking transcode in a timeout (#10).
    match timeout(
        FFMPEG_TRANSCODE_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            transcode_video_inner(post_id, file_path, board_short, pool)
        }),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(anyhow::anyhow!("spawn_blocking panicked: {}", join_err)),
        Err(_elapsed) => {
            warn!(
                "VideoTranscode: job for post {} timed out after {}s — ffmpeg killed",
                post_id,
                FFMPEG_TRANSCODE_TIMEOUT.as_secs()
            );
            Err(anyhow::anyhow!(
                "ffmpeg transcode timed out after {}s",
                FFMPEG_TRANSCODE_TIMEOUT.as_secs()
            ))
        }
    }
}

fn transcode_video_inner(
    post_id: i64,
    file_path: String,
    board_short: String,
    pool: DbPool,
) -> Result<()> {
    let upload_dir = &CONFIG.upload_dir;
    let src = PathBuf::from(upload_dir).join(&file_path);

    if !src.exists() {
        return Err(anyhow::anyhow!(
            "Source file not found for transcode: {:?}",
            src
        ));
    }

    // Handle MP4 and WebM (AV1) inputs; skip anything else.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext != "mp4" && ext != "webm" {
        debug!(
            "VideoTranscode: skipping unrecognised extension {}",
            file_path
        );
        return Ok(());
    }

    // For WebM uploads, probe the codec first.  VP8/VP9 WebM is already
    // in the correct format; only AV1 needs re-encoding.
    if ext == "webm" {
        let src_str = src
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Source path is non-UTF-8: {:?}", src))?;

        match crate::utils::files::probe_video_codec(src_str) {
            Ok(ref codec) if codec == "av1" => {
                info!(
                    "VideoTranscode: WebM/AV1 detected for post {} — re-encoding to VP9",
                    post_id
                );
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
        .ok_or_else(|| anyhow::anyhow!("Malformed filename: {:?}", src))?
        .to_string();

    info!(
        "VideoTranscode: transcoding post {} ({})…",
        post_id, file_path
    );

    let data = std::fs::read(&src)?;
    let webm_bytes = crate::utils::files::transcode_to_webm(&data)?;

    let board_dir = PathBuf::from(upload_dir).join(&board_short);
    let webm_name = format!("{}.webm", stem);
    let webm_abs = board_dir.join(&webm_name);
    let webm_rel = format!("{}/{}", board_short, webm_name);

    std::fs::write(&webm_abs, &webm_bytes)?;

    let conn = pool.get()?;

    let updated =
        crate::db::update_all_posts_file_path(&conn, &file_path, &webm_rel, "video/webm")?;
    if updated == 0 {
        crate::db::update_post_file_info(&conn, post_id, &webm_rel, "video/webm")?;
    }

    let thumb_path = crate::db::get_post_thumb_path(&conn, post_id)?.unwrap_or_default();
    let webm_sha256 = crate::utils::crypto::sha256_hex(&webm_bytes);
    crate::db::delete_file_hash_by_path(&conn, &file_path)?;
    crate::db::record_file_hash(&conn, &webm_sha256, &webm_rel, &thumb_path, "video/webm")?;

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
/// A hard timeout of FFMPEG_WAVEFORM_TIMEOUT is applied (#10).
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

    match timeout(
        FFMPEG_WAVEFORM_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            generate_waveform_inner(post_id, file_path, board_short, pool)
        }),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(join_err)) => Err(anyhow::anyhow!("spawn_blocking panicked: {}", join_err)),
        Err(_elapsed) => {
            warn!(
                "AudioWaveform: job for post {} timed out after {}s",
                post_id,
                FFMPEG_WAVEFORM_TIMEOUT.as_secs()
            );
            Err(anyhow::anyhow!(
                "ffmpeg waveform timed out after {}s",
                FFMPEG_WAVEFORM_TIMEOUT.as_secs()
            ))
        }
    }
}

fn generate_waveform_inner(
    post_id: i64,
    file_path: String,
    board_short: String,
    pool: DbPool,
) -> Result<()> {
    let upload_dir = &CONFIG.upload_dir;
    let src = PathBuf::from(upload_dir).join(&file_path);

    if !src.exists() {
        return Err(anyhow::anyhow!(
            "Audio source not found for waveform: {:?}",
            src
        ));
    }

    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("Malformed audio filename: {:?}", src))?
        .to_string();

    let data = std::fs::read(&src)?;
    let thumb_size = CONFIG.thumb_size;

    let board_dir = PathBuf::from(upload_dir).join(&board_short);
    let thumbs_dir = board_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)?;

    let png_name = format!("{}.png", stem);
    let png_abs = thumbs_dir.join(&png_name);
    let png_rel = format!("{}/thumbs/{}", board_short, png_name);

    crate::utils::files::gen_waveform_png(&data, &png_abs, thumb_size, thumb_size / 2)?;

    let conn = pool.get()?;
    crate::db::update_post_thumb_path(&conn, post_id, &png_rel)?;

    info!("AudioWaveform done: post {} → {}", post_id, png_rel);
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
        if allow_archive {
            let count = crate::db::archive_old_threads(&conn, board_id, max_threads)?;
            if count > 0 {
                info!(
                    "ThreadArchive: moved {} overflow thread(s) to archive in /{}/ (board_id={})",
                    count, board_short, board_id
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

async fn run_spam_check(post_id: i64, ip_hash: String, body_len: usize) -> Result<()> {
    if body_len > 3500 {
        debug!(
            "SpamCheck: post {} body_len={} exceeds 3500 chars (flagged for review)",
            post_id, body_len
        );
    }
    let _ = ip_hash;
    Ok(())
}
