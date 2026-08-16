// Media processing pipeline helpers.

/// Uploaded-image format conversion.
pub mod convert;
/// EXIF orientation extraction and correction.
pub mod exif;
/// Bounded `FFmpeg` and `FFprobe` subprocess helpers.
pub mod ffmpeg;
pub mod process;
/// Active-media size pruning.
pub mod prune;
/// Managed-media reference auditing and conservative orphan reconciliation.
pub mod reconcile;
/// Image, video, audio, and PDF thumbnail generation.
pub mod thumbnail;

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

#[cfg(test)]
static THUMBNAIL_FAILURE_TEST_MODE: std::sync::RwLock<Option<TestThumbnailFailure>> =
    std::sync::RwLock::new(None);
#[cfg(test)]
static THUMBNAIL_FAILURE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum TestThumbnailFailure {
    OutputCreation,
    Encoder,
    TempRename,
    InvalidDerivedOutput,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ThumbnailFailureTestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for ThumbnailFailureTestGuard {
    fn drop(&mut self) {
        *THUMBNAIL_FAILURE_TEST_MODE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
pub(crate) fn override_thumbnail_failure(mode: TestThumbnailFailure) -> ThumbnailFailureTestGuard {
    let guard = THUMBNAIL_FAILURE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *THUMBNAIL_FAILURE_TEST_MODE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mode);
    ThumbnailFailureTestGuard { _lock: guard }
}

// ─── ProcessedMedia ───────────────────────────────────────────────────────────

/// Outcome of a single upload processed through the media pipeline.
///
/// Returned by [`MediaProcessor::process_upload`].  All paths are absolute.
#[derive(Debug)]
pub struct ProcessedMedia {
    /// Absolute path to the (possibly converted) file on disk.
    pub file_path: PathBuf,
    /// Absolute path to the generated thumbnail (WebP) or SVG placeholder.
    ///
    /// `None` means thumbnail generation failed after the original was
    /// processed successfully. Callers must not persist an expected path in
    /// that case.
    pub thumbnail_path: Option<PathBuf>,
    /// MIME type of the final stored file.  May differ from the uploaded
    /// MIME when conversion changes the format (e.g. `image/gif` → `video/webm`).
    pub mime_type: String,
    /// `true` when the file was converted to a different format.
    pub was_converted: bool,
    /// Size of the final stored file in bytes.
    pub final_size: u64,
}

// ─── MediaProcessor ───────────────────────────────────────────────────────────

/// Stateless processor that converts uploaded media and generates thumbnails.
///
/// Holds a single boolean indicating whether the `ffmpeg` binary was found on
/// the current `PATH`.  All conversion and thumbnail operations consult this
/// flag and degrade gracefully when ffmpeg is absent.
///
/// ## Construction
/// ```rust,no_run
/// # use chan::media::MediaProcessor;
/// // Detect ffmpeg now (blocking):
/// let processor = MediaProcessor::new();
///
/// // Re-use a flag detected at startup (preferred in request handlers):
/// let processor = MediaProcessor::new_with_ffmpeg(true);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct MediaProcessor {
    /// Whether the `ffmpeg` binary was detected on startup.
    pub ffmpeg_available: bool,
    /// Whether the libwebp encoder is compiled into the detected ffmpeg build.
    /// Controls image→WebP conversion independently of video/audio capabilities.
    pub ffmpeg_webp_available: bool,
}

impl MediaProcessor {
    /// Create a new `MediaProcessor`, probing for `ffmpeg` immediately.
    ///
    /// This performs a blocking process spawn (`ffmpeg -version`).  For
    /// request handlers, prefer [`MediaProcessor::new_with_ffmpeg`] with the
    /// flag pre-detected at startup to avoid redundant spawns.
    #[must_use]
    pub fn new() -> Self {
        let available = ffmpeg::detect_ffmpeg();
        if !available {
            tracing::warn!(
                "ffmpeg not found — media conversion and video thumbnails are disabled. \
                 Install ffmpeg to enable optimal format conversion."
            );
        }
        let mut processor = Self::new_with_ffmpeg(available);
        processor.ffmpeg_webp_available = available && ffmpeg::check_webp_encoder();
        processor
    }

    /// Create a `MediaProcessor` with pre-detected capability flags.
    ///
    /// Use this in request handlers to avoid re-detecting ffmpeg on every upload.
    /// Both flags should come from `AppState` which is populated once at startup.
    #[must_use]
    pub const fn new_with_ffmpeg_caps(ffmpeg_available: bool, ffmpeg_webp_available: bool) -> Self {
        Self {
            ffmpeg_available,
            ffmpeg_webp_available,
        }
    }

    /// Convenience constructor when only the base ffmpeg flag is known.
    /// `ffmpeg_webp_available` defaults to the same value as `ffmpeg_available`.
    /// Prefer [`new_with_ffmpeg_caps`](Self::new_with_ffmpeg_caps) in handlers.
    #[must_use]
    pub const fn new_with_ffmpeg(ffmpeg_available: bool) -> Self {
        Self {
            ffmpeg_available,
            ffmpeg_webp_available: ffmpeg_available,
        }
    }

    /// Process an uploaded file: convert to an optimal web format and generate
    /// a thumbnail.
    ///
    /// The `input_path` must be a temporary file written by the caller; the
    /// processor may rename or delete it after processing.  The final output
    /// is placed at `output_dir / {file_stem}.{ext}` where `ext` is
    /// determined by the conversion rules.
    ///
    /// # Arguments
    /// * `input_path`  — Temp file holding the original upload bytes.
    /// * `mime`        — Detected MIME type of the upload.
    /// * `output_dir`  — Directory for the final converted file.
    /// * `file_stem`   — UUID stem (no extension) for output file names.
    /// * `thumb_dir`   — Directory for the generated thumbnail.
    /// * `thumb_max`   — Maximum thumbnail dimension (pixels, aspect preserved).
    ///
    /// # Errors
    /// Returns an error only for unrecoverable I/O failures (disk full, no
    /// permissions).  Conversion failures are logged as warnings and the
    /// original file is kept instead — the function never propagates ffmpeg
    /// errors to the caller.
    pub fn process_upload(
        self,
        input_path: &Path,
        mime: &str,
        output_dir: &Path,
        file_stem: &str,
        thumb_dir: &Path,
        thumb_max: u32,
    ) -> Result<ProcessedMedia> {
        let original_size = std::fs::metadata(input_path)
            .map(|m| m.len())
            .context("failed to stat upload temp file")?;

        // ── Step 1: Convert file ──────────────────────────────────────────
        let conv = convert::convert_file(
            input_path,
            mime,
            output_dir,
            file_stem,
            self.ffmpeg_available,
            self.ffmpeg_webp_available,
        )
        .context("conversion step failed")?;

        tracing::debug!(
            "media: {} → {} (converted={}, {}→{}B)",
            mime,
            conv.final_mime,
            conv.was_converted,
            original_size,
            conv.final_size,
        );

        // ── Step 2: Generate thumbnail ────────────────────────────────────
        // generate_thumbnail returns the actual path written, which may differ
        // from thumb_path when a video thumbnail falls back to an SVG placeholder
        // (the pre-selected .webp extension would mismatch the SVG content).
        let generated_thumbnail = {
            #[cfg(test)]
            {
                let mode = *THUMBNAIL_FAILURE_TEST_MODE
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match mode {
                    Some(TestThumbnailFailure::OutputCreation) => Err(anyhow::anyhow!(
                        "injected thumbnail output creation failure"
                    )),
                    Some(TestThumbnailFailure::Encoder) => {
                        Err(anyhow::anyhow!("injected thumbnail encoder failure"))
                    }
                    Some(TestThumbnailFailure::TempRename) => Err(anyhow::anyhow!(
                        "injected thumbnail temporary rename failure"
                    )),
                    Some(TestThumbnailFailure::InvalidDerivedOutput) => {
                        Ok(thumbnail::thumbnail_output_path(
                            thumb_dir,
                            file_stem,
                            conv.final_mime,
                            self.ffmpeg_available,
                            self.ffmpeg_webp_available,
                        ))
                    }
                    None => self.generate_thumbnail(
                        &conv.final_path,
                        conv.final_mime,
                        thumb_dir,
                        file_stem,
                        thumb_max,
                    ),
                }
            }
            #[cfg(not(test))]
            self.generate_thumbnail(
                &conv.final_path,
                conv.final_mime,
                thumb_dir,
                file_stem,
                thumb_max,
            )
        };
        let actual_thumb_path = match generated_thumbnail {
            Ok(path)
                if std::fs::symlink_metadata(&path)
                    .is_ok_and(|metadata| metadata.file_type().is_file()) =>
            {
                Some(path)
            }
            Ok(_) => {
                tracing::warn!(
                    media_mime = conv.final_mime,
                    "thumbnail generator returned no regular output; preserving original without a thumbnail"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    media_mime = conv.final_mime,
                    "thumbnail generation failed; preserving original without a thumbnail"
                );
                None
            }
        };

        Ok(ProcessedMedia {
            file_path: conv.final_path,
            thumbnail_path: actual_thumb_path,
            mime_type: conv.final_mime.to_owned(),
            was_converted: conv.was_converted,
            final_size: conv.final_size,
        })
    }

    /// Generate a thumbnail for an already-processed file.
    ///
    /// Useful when you need to re-generate a thumbnail separately from the
    /// conversion step (e.g. background workers regenerating after manual
    /// admin replacement).
    ///
    /// Writes a WebP file (or SVG placeholder) to `thumb_dir / {file_stem}.{ext}`.
    ///
    /// # Errors
    /// Returns an error only if both ffmpeg and the image-crate fallback fail
    /// AND writing the placeholder also fails.
    pub fn generate_thumbnail(
        self,
        input_path: &Path,
        mime: &str,
        thumb_dir: &Path,
        file_stem: &str,
        thumb_max: u32,
    ) -> Result<PathBuf> {
        let thumb_path = thumbnail::thumbnail_output_path(
            thumb_dir,
            file_stem,
            mime,
            self.ffmpeg_available,
            self.ffmpeg_webp_available,
        );

        // Forward the actual path returned by generate_thumbnail (may differ from
        // thumb_path when a video placeholder falls back to .svg extension).
        thumbnail::generate_thumbnail(
            input_path,
            mime,
            &thumb_path,
            thumb_max,
            self.ffmpeg_available,
            self.ffmpeg_webp_available,
        )
    }
}

impl Default for MediaProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructing `MediaProcessor` with ffmpeg=false should not panic.
    #[test]
    fn new_with_ffmpeg_false_does_not_panic() {
        let p = MediaProcessor::new_with_ffmpeg(false);
        assert!(!p.ffmpeg_available);
    }

    /// Constructing `MediaProcessor` with ffmpeg=true should not panic.
    #[test]
    fn new_with_ffmpeg_true_does_not_panic() {
        let p = MediaProcessor::new_with_ffmpeg(true);
        assert!(p.ffmpeg_available);
    }
}
