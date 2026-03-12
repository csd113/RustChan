// media/mod.rs
//
// Public interface for the media processing pipeline.
//
// Usage from the upload pipeline:
//
//   let processor = MediaProcessor::new();   // detects ffmpeg once
//   let result = processor.process_upload(
//       &temp_path, mime, &dest_dir, &file_stem, &thumbs_dir, thumb_max,
//   )?;
//   // result.file_path      — final file on disk (converted if applicable)
//   // result.thumbnail_path — WebP thumbnail (or SVG placeholder)
//   // result.mime_type      — final MIME (may differ from original for gif→webm)
//   // result.was_converted  — true when format changed
//   // result.original_size  — bytes of input file
//   // result.final_size     — bytes of output file
//
// FFmpeg detection:
//   `MediaProcessor::new()` calls `ffmpeg::detect_ffmpeg()` exactly once and
//   stores the result in `ffmpeg_available`.  Alternatively, use
//   `MediaProcessor::new_with_ffmpeg(bool)` to supply a pre-detected value
//   (e.g. from the startup check stored in `AppState`).
//
// Graceful degradation:
//   If ffmpeg is not found, `process_upload` stores files as-is and
//   `generate_thumbnail` writes a static SVG placeholder for video; for
//   images the `image` crate is used as a fallback thumbnail generator.
//   No error is returned to the user in either case.

pub mod convert;
pub mod exif;
pub mod ffmpeg;
pub mod thumbnail;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// ─── ProcessedMedia ───────────────────────────────────────────────────────────

/// Outcome of a single upload processed through the media pipeline.
///
/// Returned by [`MediaProcessor::process_upload`].  All paths are absolute.
#[derive(Debug)]
pub struct ProcessedMedia {
    /// Absolute path to the (possibly converted) file on disk.
    pub file_path: PathBuf,
    /// Absolute path to the generated thumbnail (WebP) or SVG placeholder.
    pub thumbnail_path: PathBuf,
    /// MIME type of the final stored file.  May differ from the uploaded
    /// MIME when conversion changes the format (e.g. `image/gif` → `video/webm`).
    pub mime_type: String,
    /// `true` when the file was converted to a different format.
    pub was_converted: bool,
    /// Size of the original input in bytes.
    #[allow(dead_code)]
    pub original_size: u64,
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
        Self {
            ffmpeg_available: available,
        }
    }

    /// Create a `MediaProcessor` with a pre-detected ffmpeg availability flag.
    ///
    /// Use this in request handlers to avoid re-detecting ffmpeg on every upload.
    /// The flag should come from `AppState::ffmpeg_available` which is set once
    /// at server startup via `detect::detect_ffmpeg`.
    #[must_use]
    pub const fn new_with_ffmpeg(ffmpeg_available: bool) -> Self {
        Self { ffmpeg_available }
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
        let thumb_path = thumbnail::thumbnail_output_path(
            thumb_dir,
            file_stem,
            conv.final_mime,
            self.ffmpeg_available,
        );

        if let Err(e) = thumbnail::generate_thumbnail(
            &conv.final_path,
            conv.final_mime,
            &thumb_path,
            thumb_max,
            self.ffmpeg_available,
        ) {
            // Thumbnail failure must never abort an upload.  Log and continue;
            // the placeholder will already have been written or the path left
            // empty — callers must handle a missing thumbnail gracefully.
            tracing::warn!("thumbnail generation failed: {e}");
        }

        Ok(ProcessedMedia {
            file_path: conv.final_path,
            thumbnail_path: thumb_path,
            mime_type: conv.final_mime.to_string(),
            was_converted: conv.was_converted,
            original_size,
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
    #[allow(dead_code)]
    pub fn generate_thumbnail(
        self,
        input_path: &Path,
        mime: &str,
        thumb_dir: &Path,
        file_stem: &str,
        thumb_max: u32,
    ) -> Result<PathBuf> {
        let thumb_path =
            thumbnail::thumbnail_output_path(thumb_dir, file_stem, mime, self.ffmpeg_available);

        thumbnail::generate_thumbnail(
            input_path,
            mime,
            &thumb_path,
            thumb_max,
            self.ffmpeg_available,
        )?;

        Ok(thumb_path)
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
