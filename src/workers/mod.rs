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
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

// How many times a job may be attempted before being permanently failed.
#[allow(dead_code)]
const MAX_ATTEMPTS: i64 = 3;
// How long a worker sleeps when the queue is empty.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

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
    pool: DbPool,
    notify: Arc<Notify>,
}

impl JobQueue {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            notify: Arc::new(Notify::new()),
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
                // Queue is empty — sleep until notified or POLL_INTERVAL elapses.
                tokio::select! {
                    _ = queue.notify.notified() => {}
                    _ = sleep(POLL_INTERVAL) => {}
                }
            }
            Ok(Err(e)) => {
                error!("Worker {}: DB error while claiming job: {}", id, e);
                sleep(Duration::from_secs(2)).await;
            }
            Err(e) => {
                error!("Worker {}: panic in spawn_blocking: {}", id, e);
                sleep(Duration::from_secs(2)).await;
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
        } => prune_threads(board_id, board_short, max_threads, pool).await,

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
/// Thumbnail is unaffected — it was generated synchronously from the MP4's
/// first frame (which is visually identical to the WebM frame).
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

    tokio::task::spawn_blocking(move || {
        let upload_dir = &CONFIG.upload_dir;
        let src = PathBuf::from(upload_dir).join(&file_path);

        if !src.exists() {
            return Err(anyhow::anyhow!(
                "Source file not found for transcode: {:?}",
                src
            ));
        }

        // Only MP4 → WebM makes sense here; WebM uploads are already final.
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "mp4" {
            debug!("VideoTranscode: skipping non-MP4 file {}", file_path);
            return Ok(());
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

        // Update the post record.
        let conn = pool.get()?;
        crate::db::update_post_file_info(&conn, post_id, &webm_rel, "video/webm")?;

        // Refresh the file-hash table: remove stale MP4 entry, insert WebM entry.
        let thumb_path = crate::db::get_post_thumb_path(&conn, post_id)?.unwrap_or_default();
        let webm_sha256 = crate::utils::crypto::sha256_hex(&webm_bytes);
        crate::db::delete_file_hash_by_path(&conn, &file_path)?;
        crate::db::record_file_hash(&conn, &webm_sha256, &webm_rel, &thumb_path, "video/webm")?;

        // Remove the original MP4 now that WebM is saved and DB is updated.
        let _ = std::fs::remove_file(&src);

        info!(
            "VideoTranscode done: post {} {} → {} ({} bytes)",
            post_id,
            file_path,
            webm_rel,
            webm_bytes.len()
        );
        Ok(())
    })
    .await?
}

// ─── AudioWaveform ────────────────────────────────────────────────────────────

/// Generate a waveform PNG thumbnail for an audio upload via ffmpeg,
/// then update the post's thumb_path to point to the PNG.
/// The original SVG placeholder (written inline during the request) is kept
/// as a fallback but is no longer served once the PNG is in place.
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

    tokio::task::spawn_blocking(move || {
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
    })
    .await?
}

// ─── ThreadPrune ─────────────────────────────────────────────────────────────

/// Archive threads that exceed the board's max_threads limit.
/// Archived threads are locked and marked read-only instead of deleted, so
/// their content remains accessible via /{board}/archive.
async fn prune_threads(
    board_id: i64,
    board_short: String,
    max_threads: i64,
    pool: DbPool,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let count = crate::db::archive_old_threads(&conn, board_id, max_threads)?;
        if count > 0 {
            info!(
                "ThreadArchive: moved {} overflow thread(s) to archive in /{}/ (board_id={})",
                count, board_short, board_id
            );
        }
        Ok(())
    })
    .await?
}

// ─── SpamCheck ────────────────────────────────────────────────────────────────

/// Spam / abuse analysis hook. Currently logs suspicious patterns; extend
/// this function to auto-flag, rate-limit, or shadow-ban as needed.
async fn run_spam_check(post_id: i64, ip_hash: String, body_len: usize) -> Result<()> {
    // Flag unusually long posts for manual review.
    if body_len > 3500 {
        debug!(
            "SpamCheck: post {} body_len={} exceeds 3500 chars (flagged for review)",
            post_id, body_len
        );
    }
    // ip_hash retained for future per-IP frequency analysis.
    let _ = ip_hash;
    Ok(())
}
