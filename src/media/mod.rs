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
//   // result.mime_type       — final MIME (may differ from original for gif→webm)
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

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Minimum acceptable value for `thumb_max` dimension.
const MIN_THUMB_MAX: u32 = 1;

// ─── ProcessedMedia ───────────────────────────────────────────────────────────

/// Outcome of a single upload processed through the media pipeline.
///
/// Returned by [`MediaProcessor::process_upload`].  All paths are absolute.
#[derive(Debug)]
pub struct ProcessedMedia {
    /// Absolute path to the (possibly converted) file on disk.
    pub file_path: PathBuf,
    /// Absolute path to the generated thumbnail (WebP) or SVG placeholder.
    /// **Note:** When thumbnail generation fails, this path may reference a
    /// file that does not exist on disk.  Callers should check existence
    /// before serving.
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
    /// `true` when a thumbnail was successfully generated.
    /// When `false`, `thumbnail_path` may not exist on disk.
    #[allow(dead_code)]
    pub thumbnail_generated: bool,
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

/// Validate that `file_stem` does not contain path separators or other
/// dangerous characters.  Returns an error if the stem is empty or contains
/// path traversal components.
fn validate_file_stem(file_stem: &str) -> Result<()> {
    if file_stem.is_empty() {
        bail!("file_stem must not be empty");
    }
    if file_stem.contains('/')
        || file_stem.contains('\\')
        || file_stem.contains("..")
        || file_stem.contains('\0')
    {
        bail!(
            "file_stem contains invalid characters (path separators or null bytes): {file_stem:?}"
        );
    }
    Ok(())
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
        let webp_available = if available {
            ffmpeg::check_webp_encoder()
        } else {
            false
        };
        Self {
            ffmpeg_available: available,
            ffmpeg_webp_available: webp_available,
        }
    }

    /// Create a `MediaProcessor` with pre-detected capability flags.
    ///
    /// Use this in request handlers to avoid re-detecting ffmpeg on every upload.
    /// Both flags should come from `AppState` which is populated once at startup.
    ///
    /// # Invariants
    /// If `ffmpeg_available` is `false`, `ffmpeg_webp_available` is forced to
    /// `false` regardless of the supplied value.
    #[must_use]
    pub const fn new_with_ffmpeg_caps(ffmpeg_available: bool, ffmpeg_webp_available: bool) -> Self {
        Self {
            ffmpeg_available,
            // webp can't be available if ffmpeg itself isn't
            ffmpeg_webp_available: ffmpeg_available && ffmpeg_webp_available,
        }
    }

    /// Convenience constructor when only the base ffmpeg flag is known.
    ///
    /// **Warning:** `ffmpeg_webp_available` defaults to the same value as
    /// `ffmpeg_available`, which is optimistic — ffmpeg may be installed
    /// without the libwebp encoder.  Prefer
    /// [`new_with_ffmpeg_caps`](Self::new_with_ffmpeg_caps) in handlers where
    /// the webp flag has been properly detected at startup.
    #[must_use]
    #[allow(dead_code)]
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
    /// * `input_path` — Temp file holding the original upload bytes.
    /// * `mime` — Detected MIME type of the upload.
    /// * `output_dir` — Directory for the final converted file.
    /// * `file_stem` — UUID stem (no extension) for output file names.
    ///   Must not contain path separators or `..`.
    /// * `thumb_dir` — Directory for the generated thumbnail.
    /// * `thumb_max` — Maximum thumbnail dimension (pixels, aspect preserved).
    ///   Must be ≥ 1.
    ///
    /// # Errors
    /// Returns an error only for unrecoverable I/O failures (disk full, no
    /// permissions) or invalid arguments.  Conversion failures are logged as
    /// warnings and the original file is kept instead — the function never
    /// propagates ffmpeg errors to the caller.
    #[must_use = "ProcessedMedia contains the final file path and metadata needed for storage"]
    pub fn process_upload(
        self,
        input_path: &Path,
        mime: &str,
        output_dir: &Path,
        file_stem: &str,
        thumb_dir: &Path,
        thumb_max: u32,
    ) -> Result<ProcessedMedia> {
        // ── Validate arguments ────────────────────────────────────────────
        validate_file_stem(file_stem)?;

        if thumb_max < MIN_THUMB_MAX {
            bail!("thumb_max must be at least {MIN_THUMB_MAX} but got {thumb_max}");
        }

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
        let thumb_path = thumbnail::thumbnail_output_path(
            thumb_dir,
            file_stem,
            conv.final_mime,
            self.ffmpeg_available,
            self.ffmpeg_webp_available,
        );

        // generate_thumbnail returns the actual path written, which may differ
        // from thumb_path when a video thumbnail falls back to an SVG placeholder
        // (the pre-selected .webp extension would mismatch the SVG content).
        let (actual_thumb_path, thumbnail_generated) = match thumbnail::generate_thumbnail(
            &conv.final_path,
            conv.final_mime,
            &thumb_path,
            thumb_max,
            self.ffmpeg_available,
            self.ffmpeg_webp_available,
        ) {
            Ok(p) => (p, true),
            Err(e) => {
                // Thumbnail failure must never abort an upload.  Log and fall
                // back to the pre-computed path.  The `thumbnail_generated`
                // flag signals callers that the path may not exist.
                tracing::warn!(
                    "thumbnail generation failed for {}: {e:#}",
                    conv.final_path.display()
                );
                (thumb_path, false)
            }
        };

        Ok(ProcessedMedia {
            file_path: conv.final_path,
            thumbnail_path: actual_thumb_path,
            mime_type: conv.final_mime.to_string(),
            was_converted: conv.was_converted,
            original_size,
            final_size: conv.final_size,
            thumbnail_generated,
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
    /// # Arguments
    /// * `input_path` — Path to the media file.
    /// * `mime` — MIME type of the media file.
    /// * `thumb_dir` — Directory for the generated thumbnail.
    /// * `file_stem` — Stem for the output filename.  Must not contain path
    ///   separators or `..`.
    /// * `thumb_max` — Maximum thumbnail dimension (pixels). Must be ≥ 1.
    ///
    /// # Errors
    /// Returns an error only if both ffmpeg and the image-crate fallback fail
    /// AND writing the placeholder also fails, or if arguments are invalid.
    #[allow(dead_code)]
    pub fn generate_thumbnail(
        self,
        input_path: &Path,
        mime: &str,
        thumb_dir: &Path,
        file_stem: &str,
        thumb_max: u32,
    ) -> Result<PathBuf> {
        validate_file_stem(file_stem)?;

        if thumb_max < MIN_THUMB_MAX {
            bail!("thumb_max must be at least {MIN_THUMB_MAX} but got {thumb_max}");
        }

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
        assert!(!p.ffmpeg_webp_available);
    }

    /// Constructing `MediaProcessor` with ffmpeg=true should not panic.
    #[test]
    fn new_with_ffmpeg_true_does_not_panic() {
        let p = MediaProcessor::new_with_ffmpeg(true);
        assert!(p.ffmpeg_available);
        assert!(p.ffmpeg_webp_available);
    }

    /// `new_with_ffmpeg_caps` enforces invariant: no webp without ffmpeg.
    #[test]
    fn new_with_ffmpeg_caps_enforces_invariant() {
        let p = MediaProcessor::new_with_ffmpeg_caps(false, true);
        assert!(!p.ffmpeg_available);
        assert!(!p.ffmpeg_webp_available);

        let p = MediaProcessor::new_with_ffmpeg_caps(true, false);
        assert!(p.ffmpeg_available);
        assert!(!p.ffmpeg_webp_available);

        let p = MediaProcessor::new_with_ffmpeg_caps(true, true);
        assert!(p.ffmpeg_available);
        assert!(p.ffmpeg_webp_available);
    }

    /// `validate_file_stem` rejects empty stems.
    #[test]
    fn validate_file_stem_rejects_empty() {
        assert!(validate_file_stem("").is_err());
    }

    /// `validate_file_stem` rejects path traversal.
    #[test]
    fn validate_file_stem_rejects_traversal() {
        assert!(validate_file_stem("../etc/passwd").is_err());
        assert!(validate_file_stem("foo/bar").is_err());
        assert!(validate_file_stem("foo\\bar").is_err());
        assert!(validate_file_stem("foo\0bar").is_err());
    }

    /// `validate_file_stem` accepts normal UUID-like stems.
    #[test]
    fn validate_file_stem_accepts_valid() {
        assert!(validate_file_stem("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(validate_file_stem("my_file").is_ok());
        assert!(validate_file_stem("test.extra.dots").is_ok()); // dots without .. are fine
    }

    /// `thumb_max` of 0 is rejected.
    #[test]
    fn generate_thumbnail_rejects_zero_thumb_max() {
        let p = MediaProcessor::new_with_ffmpeg(false);
        let result = p.generate_thumbnail(
            Path::new("/nonexistent"),
            "image/png",
            Path::new("/tmp"),
            "test",
            0,
        );
        assert!(result.is_err());
        // Verify the error message mentions thumb_max so we know it was
        // rejected for the right reason, not a downstream I/O error.
        let err = result.unwrap_or_else(|e| {
            assert!(e.to_string().contains("thumb_max"));
            PathBuf::new()
        });
        assert!(err.as_os_str().is_empty());
    }
}
