//
// Per-format conversion logic.
//
// Conversion rules (from project spec):
//   jpg / jpeg → WebP  (lossless, metadata stripped)
//   gif        → animated WebP (composited frames, timing and repeats preserved)
//   heic/heif  → WebP
//   bmp        → WebP
//   tiff       → WebP
//   png        → WebP  ONLY if the WebP output is smaller; otherwise keep PNG
//   svg        → keep as-is (no conversion)
//   webp       → keep as-is
//   webm/mkv   → keep as-is
//   all audio  → keep as-is
//   mp4        → keep as-is (background worker handles MP4→WebM separately)
//
// Image decoding and WebP encoding run internally in Rust.
// On failure, all functions log a warning and the caller falls back to
// storing the original bytes.

use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

use super::{image_budget::ImageBudget, images};

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
        "image/jpeg" | "image/heic" | "image/heif" | "image/bmp" | "image/tiff" | "image/gif" => {
            ConversionAction::ToWebp
        }
        "image/png" => ConversionAction::ToWebpIfSmaller,
        _ => ConversionAction::KeepAsIs,
    }
}

/// Result of a conversion operation.
#[derive(Debug)]
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
/// # Arguments
/// * `input_path`           — Temporary file containing the original upload bytes.
/// * `mime`                 — Detected MIME type of the input.
/// * `output_dir`           — Directory where the final file should be placed.
/// * `file_stem`            — UUID-based stem (no extension) for the output filename.
///
/// # Errors
/// Returns an error only for I/O failures (copy / rename).  codec failures
/// are logged as warnings and the function falls back to the original file.
pub fn convert_file(
    input_path: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
) -> Result<ConversionResult> {
    convert_file_with_budget(
        input_path,
        mime,
        output_dir,
        file_stem,
        &ImageBudget::default(),
    )
}

/// Convert using the deadline and output allowance shared by this upload.
pub(super) fn convert_file_with_budget(
    input_path: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
    budget: &ImageBudget,
) -> Result<ConversionResult> {
    let action = conversion_action(mime);

    match action {
        ConversionAction::ToWebp => {
            convert_to_webp(input_path, mime, output_dir, file_stem, budget)
        }
        ConversionAction::ToWebpIfSmaller => {
            convert_png_if_smaller(input_path, output_dir, file_stem, budget)
        }
        ConversionAction::KeepAsIs => copy_as_is(input_path, mime, output_dir, file_stem),
    }
}

// Internal conversion helpers
/// Convert a supported image to metadata-free lossless WebP.
///
/// On codec failure, logs a warning and falls back to copying the original
/// file unchanged (so the post still succeeds).
fn convert_to_webp(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
    budget: &ImageBudget,
) -> Result<ConversionResult> {
    let output = output_dir.join(format!("{file_stem}.webp"));
    // The encoder replaces this path atomically. Keep path cleanup without an
    // open destination handle: tempfile's Windows replacement rejects it.
    let temporary = tempfile::NamedTempFile::new_in(output_dir)?.into_temp_path();
    let tmp_out = temporary.as_ref();

    let conversion = images::image_to_webp_with_budget(input, tmp_out, None, budget);
    match conversion {
        Ok(()) => {
            atomic_rename(tmp_out, &output)?;
            let final_size = file_size(&output)?;
            let source_name = input
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("input image");
            let output_name = output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("converted.webp");
            tracing::info!(
                target: "convert",
                source = source_name,
                saved_as = output_name,
                bytes = final_size,
                "Converted image to WebP"
            );
            Ok(ConversionResult {
                final_path: output,
                final_mime: "image/webp",
                was_converted: true,
                final_size,
            })
        }
        Err(e) => {
            drop(std::fs::remove_file(tmp_out));
            tracing::warn!(failure = images::failure_kind(&e), error = %e, "image→webp failed; storing original");
            // Preserve the validated MIME even for suffixless or misnamed temporary files.
            copy_as_is(input, mime, output_dir, file_stem)
        }
    }
}

/// Attempt PNG → WebP conversion; keep the PNG if WebP is not smaller.
fn convert_png_if_smaller(
    input: &Path,
    output_dir: &Path,
    file_stem: &str,
    budget: &ImageBudget,
) -> Result<ConversionResult> {
    let webp_path = output_dir.join(format!("{file_stem}.webp"));
    // Close the reservation handle before the encoder atomically replaces it.
    let temporary = tempfile::NamedTempFile::new_in(output_dir)?.into_temp_path();
    let tmp_webp = temporary.as_ref();

    // Try conversion first
    let conversion = images::image_to_webp_with_budget(input, tmp_webp, None, budget);
    match conversion {
        Ok(()) => {
            let original_size = file_size(input)?;
            let webp_size = file_size(tmp_webp)?;

            if webp_size < original_size {
                // WebP wins — keep the converted file
                atomic_rename(tmp_webp, &webp_path)?;
                Ok(ConversionResult {
                    final_path: webp_path,
                    final_mime: "image/webp",
                    was_converted: true,
                    final_size: webp_size,
                })
            } else {
                // PNG is already optimal
                drop(std::fs::remove_file(tmp_webp));
                tracing::debug!("PNG→WebP skipped: webp ({webp_size}B) ≥ png ({original_size}B)");
                copy_as_is(input, "image/png", output_dir, file_stem)
            }
        }
        Err(e) => {
            drop(std::fs::remove_file(tmp_webp));
            tracing::warn!(failure = images::failure_kind(&e), error = %e, "png→webp failed; storing original PNG");
            copy_as_is(input, "image/png", output_dir, file_stem)
        }
    }
}

/// Copy the input file to `output_dir/{file_stem}.{ext}` without conversion.
fn copy_as_is(
    input: &Path,
    mime: &str,
    output_dir: &Path,
    file_stem: &str,
) -> Result<ConversionResult> {
    let ext = crate::utils::files::mime_to_ext_pub(mime);
    copy_as_is_with_mime(
        input,
        output_dir,
        file_stem,
        ext,
        upload_mime_to_static(mime),
    )
}

/// Copy input bytes using an explicit canonical MIME type and extension.
fn copy_as_is_with_mime(
    input: &Path,
    output_dir: &Path,
    file_stem: &str,
    ext: &str,
    final_mime: &'static str,
) -> Result<ConversionResult> {
    let output = output_dir.join(format!("{file_stem}.{ext}"));
    let temporary = tempfile::NamedTempFile::new_in(output_dir)?;
    let _bytes_copied = std::fs::copy(input, temporary.path())
        .with_context(|| format!("failed to copy upload to {}", output.display()))?;
    drop(
        temporary
            .persist(&output)
            .context("publish original upload")?,
    );
    let final_size = file_size(&output)?;
    Ok(ConversionResult {
        final_path: output,
        final_mime,
        was_converted: false,
        final_size,
    })
}

/// Map a validated upload MIME type to the static lifetime used by results.
fn upload_mime_to_static(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/heic" => "image/heic",
        "image/heif" => "image/heif",
        "image/bmp" => "image/bmp",
        "image/tiff" => "image/tiff",
        "image/webp" => "image/webp",
        "image/svg+xml" => "image/svg+xml",
        "application/pdf" => "application/pdf",
        "video/webm" => "video/webm",
        "video/mp4" => "video/mp4",
        "video/x-matroska" | "video/matroska" => "video/x-matroska",
        "audio/webm" => "audio/webm",
        "audio/mpeg" | "audio/mp3" => "audio/mpeg",
        "audio/ogg" | "application/ogg" | "audio/oga" => "audio/ogg",
        "audio/opus" => "audio/opus",
        "audio/flac" | "audio/x-flac" => "audio/flac",
        "audio/wav" | "audio/wave" | "audio/x-wav" | "audio/vnd.wave" => "audio/wav",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "audio/mp4",
        "audio/aac" | "audio/x-aac" => "audio/aac",
        _ => "application/octet-stream",
    }
}

// Path and size utilities
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

#[cfg(test)]
#[path = "tests/convert.rs"]
mod tests;
