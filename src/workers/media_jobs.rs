//! Video/audio execution and atomic installation of deterministic media outputs.

use super::{
    ffmpeg_command, transcoded_webm_name, wait_for_ffmpeg_output, AsyncWaitOutcome, JobExecution,
};
use crate::config::CONFIG;
use crate::db::DbPool;
use anyhow::{Context as _, Result};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

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
pub(super) async fn transcode_video(
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
        debug!(target: super::WORKER_LOG_TARGET, "VideoTranscode skipped for post {post_id}: ffmpeg not available");
        return Ok(JobExecution::NeedsCompletion);
    }

    if !ffmpeg_vp9_available {
        warn!(target: super::WORKER_LOG_TARGET,
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
        crate::middleware::data_access::spawn_blocking(move || {
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
    let finalise_result = crate::middleware::data_access::spawn_blocking(move || {
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
            warn!(target: super::WORKER_LOG_TARGET, "{}", video_reencode_timeout_warning(post_id, timeout_secs));
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
        debug!(target: super::WORKER_LOG_TARGET, "VideoTranscode: preserving compatible VP8/VP9 WebM for post {post_id}");
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
                    warn!(target: super::WORKER_LOG_TARGET,
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
pub(super) async fn generate_waveform(
    job_id: i64,
    post_id: i64,
    file_path: String,
    board_short: String,
    ffmpeg_available: bool,
    pool: DbPool,
    cancel: CancellationToken,
) -> Result<JobExecution> {
    let (png_abs, png_rel, src_path, expected_file_path, tmp_png) =
        crate::middleware::data_access::spawn_blocking(move || {
            waveform_prepare(&file_path, &board_short)
        })
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
    let finalised = crate::middleware::data_access::spawn_blocking(move || {
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
    let rendered = crate::middleware::data_access::spawn_blocking(move || {
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
                warn!(target: super::WORKER_LOG_TARGET,
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
            warn!(target: super::WORKER_LOG_TARGET,
                placeholder = placeholder_rel,
                error = %error,
                "AudioWaveform: rejected unsafe obsolete placeholder path"
            );
            return;
        }
    };
    if let Err(error) = std::fs::remove_file(&placeholder_abs) {
        warn!(target: super::WORKER_LOG_TARGET,
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
