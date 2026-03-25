// media/convert.rs
//
// Per-format conversion logic.
//
// Conversion rules (from project spec):
//   jpg / jpeg → WebP  (quality 85, metadata stripped)
//   gif        → WebP  (quality 85, -loop 0 preserves animation if libwebp supports it)
//   bmp        → WebP
//   tiff       → WebP
//   png        → WebP  ONLY if the WebP output is smaller; otherwise keep PNG
//   svg        → keep as-is (no conversion)
//   webp       → keep as-is
//   webm       → keep as-is
//   all audio  → keep as-is
//   mp4        → keep as-is (background worker handles MP4→WebM separately)
//
// All conversion functions require ffmpeg.  Callers must check
// `MediaProcessor::ffmpeg_available` before calling into this module.
// On failure, all functions log a warning and the caller falls back to
// storing the original bytes.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::ffmpeg;

/// Describes what action the conversion pipeline should take for a given
/// source MIME type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionAction {
    /// Convert to WebP (JPEG, GIF, BMP, TIFF).
    ToWebp,
    /// Attempt WebP; keep original if WebP is not smaller (PNG).
    ToWebpIfSmaller,
    /// No conversion; store file as-is.
    KeepAsIs,
}

/// Determine the conversion action for a given MIME type.
///
/// Returns `KeepAsIs` for any MIME type not explicitly handled so that
/// unknown or new formats are stored without modification.
#[must_use]
pub fn conversion_action(mime: &str) -> ConversionAction {
    match mime {
        // GIF → animated WebP: keeps the media type as Image so it renders in
        // an <img> tag rather than a <video> player.  The -loop 0 flag in
        // ffmpeg_image_to_webp preserves animation for multi-frame GIFs.
        // Falls back to storing the original GIF if libwebp is unavailable.
        "image/jpeg" | "image/bmp" | "image/tiff" | "image/gif" => ConversionAction::ToWebp,
        "image/png" => ConversionAction::ToWebpIfSmaller,
        // Keep these formats as-is
        "image/svg+xml" | "image/webp" | "video/webm" | "audio/webm" | "video/mp4"
        | "audio/mpeg" | "audio/ogg" | "audio/flac" | "audio/wav" | "audio/mp4" | "audio/aac" => {
            ConversionAction::KeepAsIs
        }
        _ => ConversionAction::KeepAsIs,
    }
}

/// Result of a conversion operation.
pub struct ConversionResult {
    /// Absolute path to the final file on disk.
    pub final_path: PathBuf,
    /// MIME type of the final file (may differ from source if converted).
    pub final_mime: &'static str,
    /// `true` when the file was actually converted to a new format.
    pub was_converted: bool,
    /// Size of the final file in bytes.
    pub final_size: u64,
}

/// Convert `input_path` according to its MIME type and write the output to
/// `output_dir` using `file_stem` as the base name.
///
/// If `ffmpeg_available` is `false`, no conversion is attempted and the
/// input file is copied to the output directory with its original extension.
/// If `ffmpeg_webp_available` is `false`, WebP conversion is skipped even
/// when ffmpeg is otherwise available (e.g. stock build without libwebp).
///
/// # Arguments
/// * `input_path`           — Temporary file containing the original upload bytes.
/// * `mime`                 — Detected MIME type of the input.
/// * `output_dir`           — Directory where the final file should be placed.
/// * `file_stem`            — UUID-based stem (no extension) for the output filename.
/// * `ffmpeg_available`     — Whether the ffmpeg binary was detected at startup.
/// * `ffmpeg_webp_available`— Whether ffmpeg has the libwebp encoder compiled in.
///
/// # Errors
/// Returns an error only for I/O failures (copy / rename).  ffmpeg failures
/// are logged as warnings and the function falls back to the original file.
pub fn convert_file(
    input_path: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<ConversionResult> {
    // Reject file_stem values that could escape output_dir.
    if file_stem.contains('/')
        || file_stem.contains('\\')
        || file_stem.contains("..")
        || file_stem.is_empty()
    {
        bail!("invalid file_stem: must be a plain filename component without path separators");
    }

    let action = if ffmpeg_available {
        let base = conversion_action(mime);
        // Downgrade webp conversion actions if libwebp encoder is absent.
        match base {
            ConversionAction::ToWebp | ConversionAction::ToWebpIfSmaller
                if !ffmpeg_webp_available =>
            {
                ConversionAction::KeepAsIs
            }
            other => other,
        }
    } else {
        ConversionAction::KeepAsIs
    };

    match action {
        ConversionAction::ToWebp => convert_to_webp(input_path, mime, output_dir, file_stem),
        ConversionAction::ToWebpIfSmaller => {
            convert_png_if_smaller(input_path, mime, output_dir, file_stem)
        }
        ConversionAction::KeepAsIs => copy_as_is(input_path, mime, output_dir, file_stem),
    }
}

// ─── Internal conversion helpers ──────────────────────────────────────────────

/// Convert any ffmpeg-readable image to WebP at quality 85.
///
/// On ffmpeg failure, logs a warning and falls back to copying the original
/// file unchanged (so the post still succeeds).
fn convert_to_webp(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
) -> Result<ConversionResult> {
    let output = output_dir.join(format!("{file_stem}.webp"));
    let tmp_out = temp_sibling(&output);

    match ffmpeg::ffmpeg_image_to_webp(input, &tmp_out) {
        Ok(()) => {
            if let Err(e) = atomic_rename(&tmp_out, &output) {
                // Clean up temp file before propagating the rename error.
                let _ = std::fs::remove_file(&tmp_out);
                return Err(e);
            }
            let final_size = file_size(&output)?;
            tracing::info!(
                "image→webp: converted {} → {} ({final_size} bytes)",
                input.display(),
                output.display()
            );
            Ok(ConversionResult {
                final_path: output,
                final_mime: "image/webp",
                was_converted: true,
                final_size,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_out);
            tracing::warn!("ffmpeg image→webp failed ({:#}); storing original", e);
            // Use the known MIME to derive the correct extension,
            // instead of inspecting the (possibly extension-less) temp file.
            copy_as_is(input, mime, output_dir, file_stem)
        }
    }
}

/// Attempt PNG → WebP conversion; keep the PNG if WebP is not smaller.
fn convert_png_if_smaller(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
) -> Result<ConversionResult> {
    let webp_path = output_dir.join(format!("{file_stem}.webp"));
    let tmp_webp = temp_sibling(&webp_path);

    // Try conversion first
    match ffmpeg::ffmpeg_image_to_webp(input, &tmp_webp) {
        Ok(()) => {
            let original_size = file_size(input)?;
            let webp_size = file_size(&tmp_webp)?;

            if webp_size < original_size {
                // WebP wins — keep the converted file
                if let Err(e) = atomic_rename(&tmp_webp, &webp_path) {
                    let _ = std::fs::remove_file(&tmp_webp);
                    return Err(e);
                }
                let saved = original_size.saturating_sub(webp_size);
                tracing::info!(
                    "png→webp: converted {} → {} ({webp_size} bytes, saved {saved} bytes)",
                    input.display(),
                    webp_path.display(),
                );
                Ok(ConversionResult {
                    final_path: webp_path,
                    final_mime: "image/webp",
                    was_converted: true,
                    final_size: webp_size,
                })
            } else {
                // PNG is already optimal
                let _ = std::fs::remove_file(&tmp_webp);
                tracing::debug!("PNG→WebP skipped: webp ({webp_size}B) ≥ png ({original_size}B)");
                copy_as_is(input, mime, output_dir, file_stem)
            }
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_webp);
            tracing::warn!("ffmpeg png→webp failed ({:#}); storing original PNG", e);
            copy_as_is(input, mime, output_dir, file_stem)
        }
    }
}

/// Copy the input file to `output_dir/{file_stem}.{ext}` without conversion.
///
/// Uses the original MIME type directly (via [`mime_to_static`]) to avoid
/// the lossy MIME→ext→MIME round-trip that could corrupt types like
/// `audio/webm`.
fn copy_as_is(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
) -> Result<ConversionResult> {
    let ext = crate::utils::files::mime_to_ext_pub(mime);
    copy_as_is_with_ext(input, mime, output_dir, file_stem, ext)
}

/// Copy `input` to `output_dir/{file_stem}.{ext}`, returning a `ConversionResult`.
///
/// The `original_mime` argument is the authoritative MIME of the source data
/// and is propagated directly into `ConversionResult::final_mime` via
/// [`mime_to_static`], avoiding the lossy ext→MIME reverse mapping.
fn copy_as_is_with_ext(
    input: &Path,
    original_mime: &str,
    output_dir: &Path,
    file_stem: &str,
    ext: &str,
) -> Result<ConversionResult> {
    let output = output_dir.join(format!("{file_stem}.{ext}"));
    std::fs::copy(input, &output)
        .with_context(|| format!("failed to copy upload to {}", output.display()))?;
    let final_size = file_size(&output)?;
    // Use the known MIME directly instead of reverse-mapping from
    // the file extension, which would corrupt e.g. audio/webm → video/webm.
    let final_mime = mime_to_static(original_mime);
    Ok(ConversionResult {
        final_path: output,
        final_mime,
        was_converted: false,
        final_size,
    })
}

// ─── Path and size utilities ──────────────────────────────────────────────────

/// Create a UUID-named sibling path for use as an atomic temp output.
///
/// The temp file is given the same extension as `target` so that ffmpeg can
/// determine the output format from the filename.  Without an extension,
/// ffmpeg cannot select the right muxer and fails immediately.
fn temp_sibling(target: &Path) -> PathBuf {
    let ext = target.extension().and_then(|e| e.to_str()).unwrap_or("");
    let tmp_name = if ext.is_empty() {
        format!(".tmp_{}", Uuid::new_v4().simple())
    } else {
        format!(".tmp_{}.{ext}", Uuid::new_v4().simple())
    };
    target
        .parent()
        .map_or_else(|| PathBuf::from(&tmp_name), |p| p.join(&tmp_name))
}

/// Rename `src` to `dst` atomically (same filesystem assumed).
fn atomic_rename(src: &Path, dst: &Path) -> Result<()> {
    std::fs::rename(src, dst)
        .with_context(|| format!("failed to rename {} → {}", src.display(), dst.display()))
}

/// Return the size of a file in bytes.
fn file_size(path: &Path) -> Result<u64> {
    std::fs::metadata(path)
        .map(|m| m.len())
        .with_context(|| format!("failed to stat {}", path.display()))
}

/// Map a known MIME string to a `&'static str` for embedding in
/// `ConversionResult` without lifetime issues.
///
/// This avoids the lossy MIME→ext→MIME round-trip (e.g. `audio/webm`
/// would incorrectly become `video/webm` if we went through the
/// extension-based lookup).
///
/// Falls back to `"application/octet-stream"` for unrecognised types.
fn mime_to_static(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/bmp" => "image/bmp",
        "image/tiff" => "image/tiff",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        "video/webm" => "video/webm",
        "audio/webm" => "audio/webm",
        "video/mp4" => "video/mp4",
        "audio/mpeg" => "audio/mpeg",
        "audio/ogg" => "audio/ogg",
        "audio/flac" => "audio/flac",
        "audio/wav" => "audio/wav",
        "audio/mp4" => "audio/mp4",
        "audio/aac" => "audio/aac",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_maps_to_webp() {
        assert_eq!(conversion_action("image/jpeg"), ConversionAction::ToWebp);
    }

    #[test]
    fn gif_maps_to_webp() {
        assert_eq!(conversion_action("image/gif"), ConversionAction::ToWebp);
    }

    #[test]
    fn png_maps_to_try_webp() {
        assert_eq!(
            conversion_action("image/png"),
            ConversionAction::ToWebpIfSmaller
        );
    }

    #[test]
    fn webp_is_keep_as_is() {
        assert_eq!(conversion_action("image/webp"), ConversionAction::KeepAsIs);
    }

    #[test]
    fn webm_is_keep_as_is() {
        assert_eq!(conversion_action("video/webm"), ConversionAction::KeepAsIs);
    }

    #[test]
    fn bmp_maps_to_webp() {
        assert_eq!(conversion_action("image/bmp"), ConversionAction::ToWebp);
    }

    #[test]
    fn tiff_maps_to_webp() {
        assert_eq!(conversion_action("image/tiff"), ConversionAction::ToWebp);
    }

    #[test]
    fn audio_is_keep_as_is() {
        for mime in &[
            "audio/mpeg",
            "audio/ogg",
            "audio/flac",
            "audio/wav",
            "audio/mp4",
            "audio/aac",
            "audio/webm",
        ] {
            assert_eq!(
                conversion_action(mime),
                ConversionAction::KeepAsIs,
                "expected KeepAsIs for {mime}"
            );
        }
    }

    #[test]
    fn unknown_mime_is_keep_as_is() {
        assert_eq!(
            conversion_action("application/octet-stream"),
            ConversionAction::KeepAsIs
        );
    }

    #[test]
    fn svg_is_keep_as_is() {
        assert_eq!(
            conversion_action("image/svg+xml"),
            ConversionAction::KeepAsIs
        );
    }

    #[test]
    fn mp4_is_keep_as_is() {
        assert_eq!(conversion_action("video/mp4"), ConversionAction::KeepAsIs);
    }

    #[test]
    fn mime_to_static_roundtrips_all_known_types() {
        let mimes = [
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/bmp",
            "image/tiff",
            "image/webp",
            "image/svg+xml",
            "video/webm",
            "audio/webm",
            "video/mp4",
            "audio/mpeg",
            "audio/ogg",
            "audio/flac",
            "audio/wav",
            "audio/mp4",
            "audio/aac",
        ];
        for m in &mimes {
            assert_eq!(mime_to_static(m), *m, "mime_to_static should preserve {m}");
        }
    }

    #[test]
    fn mime_to_static_unknown_falls_back() {
        assert_eq!(
            mime_to_static("application/pdf"),
            "application/octet-stream"
        );
    }

    #[test]
    fn temp_sibling_preserves_extension() {
        let target = Path::new("/uploads/abc.webp");
        let tmp = temp_sibling(target);
        assert_eq!(tmp.extension().and_then(|e| e.to_str()), Some("webp"));
        let name = tmp.file_name().and_then(|f| f.to_str());
        assert!(name.is_some(), "temp sibling should have a filename");
        assert!(
            name.is_some_and(|n| n.starts_with(".tmp_")),
            "temp sibling filename should start with .tmp_"
        );
        assert_eq!(tmp.parent(), target.parent());
    }

    #[test]
    fn temp_sibling_handles_no_extension() {
        let target = Path::new("/uploads/abc");
        let tmp = temp_sibling(target);
        // Should not have an extension
        assert!(tmp.extension().is_none());
        let name = tmp.file_name().and_then(|f| f.to_str());
        assert!(name.is_some(), "temp sibling should have a filename");
        assert!(
            name.is_some_and(|n| n.starts_with(".tmp_")),
            "temp sibling filename should start with .tmp_"
        );
    }

    #[test]
    fn temp_sibling_handles_no_parent() {
        let target = Path::new("abc.webp");
        let tmp = temp_sibling(target);
        // Should still have correct extension
        assert_eq!(tmp.extension().and_then(|e| e.to_str()), Some("webp"));
    }
}
