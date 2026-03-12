// media/thumbnail.rs
//
// Thumbnail generation for all media types.
//
// Rules (from project spec):
//   • All thumbnails are WebP, regardless of source format.
//   • For video and converted-GIF (WebM) sources: extract the first frame
//     via ffmpeg and save as WebP.
//   • Max dimension: 250 × 250, aspect ratio preserved.
//   • WebP quality: 80.
//   • If ffmpeg is unavailable, write a static SVG placeholder for video;
//     for images, fall back to the `image` crate (no ffmpeg required).

use anyhow::{Context, Result};
use image::{imageops::FilterType, GenericImageView, ImageFormat};
use std::path::{Path, PathBuf};

use super::ffmpeg;

// ─── Static placeholder SVGs ──────────────────────────────────────────────────

// Note: these SVG strings contain `"#` sequences (e.g. fill="#0a0f0a") which
// would terminate a `r#"..."#` raw string early.  We use `r##"..."##` so the
// closing delimiter requires two consecutive `#` signs, which never appear in
// the SVG body.
const VIDEO_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <polygon points="108,95 108,155 165,125" fill="#00c840"/>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">VIDEO</text>
</svg>"##;

const AUDIO_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <text x="125" y="140" text-anchor="middle" fill="#00c840" font-family="monospace" font-size="48">&#9835;</text>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">AUDIO</text>
</svg>"##;

// ─── Public API ───────────────────────────────────────────────────────────────

/// What kind of static placeholder to write when the real thumbnail cannot be
/// generated.
#[derive(Debug, Clone, Copy)]
pub enum PlaceholderKind {
    Video,
    Audio,
}

/// Generate a thumbnail for a media file and write it to `output_path`.
///
/// All thumbnails are produced as WebP.  The strategy depends on the MIME
/// type and whether ffmpeg is available:
///
/// | Source MIME       | ffmpeg present | Action                          |
/// |-------------------|----------------|---------------------------------|
/// | `image/*`         | yes            | ffmpeg first-frame + WebP       |
/// | `image/*`         | no             | `image` crate → resize → WebP   |
/// | `video/webm`      | yes            | ffmpeg first-frame + WebP       |
/// | `video/webm`      | no             | static SVG placeholder          |
/// | `image/svg+xml`   | either         | static SVG placeholder          |
/// | `audio/*`         | either         | static SVG placeholder          |
///
/// # Arguments
/// * `input_path`      — Absolute path to the (already converted) media file.
/// * `mime`            — Final MIME type of the input file.
/// * `output_path`     — Where to write the thumbnail (WebP or SVG).
/// * `max_dim`         — Maximum width and height in pixels (aspect preserved).
/// * `ffmpeg_available`— Whether ffmpeg was detected at startup.
///
/// # Errors
/// Returns an error only if all strategies (including placeholder writing)
/// fail.  Individual strategy failures are demoted to warnings so that a
/// thumbnail failure never causes the upload to fail.
pub fn generate_thumbnail(
    input_path: &Path,
    mime: &str,
    output_path: &Path,
    max_dim: u32,
    ffmpeg_available: bool,
) -> Result<()> {
    match mime {
        // ── SVG and audio: always use static placeholder ──────────────────
        "image/svg+xml" => write_placeholder(output_path, PlaceholderKind::Video),
        m if m.starts_with("audio/") => write_placeholder(output_path, PlaceholderKind::Audio),

        // ── Video (WebM): requires ffmpeg ─────────────────────────────────
        "video/webm" | "audio/webm" => {
            if ffmpeg_available {
                match ffmpeg::ffmpeg_thumbnail(input_path, output_path, max_dim) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        tracing::warn!("ffmpeg video thumbnail failed ({}); using placeholder", e);
                        write_placeholder(output_path, PlaceholderKind::Video)
                    }
                }
            } else {
                write_placeholder(output_path, PlaceholderKind::Video)
            }
        }

        // ── Images: try ffmpeg, fall back to image crate ──────────────────
        _ if mime.starts_with("image/") => {
            if ffmpeg_available {
                match ffmpeg::ffmpeg_thumbnail(input_path, output_path, max_dim) {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        tracing::warn!(
                            "ffmpeg image thumbnail failed ({}); falling back to image crate",
                            e
                        );
                        image_crate_thumbnail(input_path, mime, output_path, max_dim)
                    }
                }
            } else {
                image_crate_thumbnail(input_path, mime, output_path, max_dim)
            }
        }

        // ── Unknown MIME: placeholder ─────────────────────────────────────
        _ => write_placeholder(output_path, PlaceholderKind::Video),
    }
}

/// Determine the correct output path for a thumbnail given the media MIME type.
///
/// Always returns a `.webp` path, except for types that produce an
/// SVG placeholder (video without ffmpeg, audio, svg source).
///
/// # Arguments
/// * `thumb_dir`  — The `thumbs/` directory (absolute path).
/// * `file_stem`  — UUID stem shared with the media file.
/// * `mime`       — Final MIME of the converted media file.
/// * `ffmpeg_available` — Whether ffmpeg was detected.
#[must_use]
pub fn thumbnail_output_path(
    thumb_dir: &Path,
    file_stem: &str,
    mime: &str,
    ffmpeg_available: bool,
) -> PathBuf {
    let ext = thumbnail_extension(mime, ffmpeg_available);
    thumb_dir.join(format!("{file_stem}.{ext}"))
}

/// Write a static SVG placeholder for video or audio media.
///
/// # Errors
/// Returns an error if the file cannot be written to `output_path`.
pub fn write_placeholder(output_path: &Path, kind: PlaceholderKind) -> Result<()> {
    let svg = match kind {
        PlaceholderKind::Video => VIDEO_PLACEHOLDER_SVG,
        PlaceholderKind::Audio => AUDIO_PLACEHOLDER_SVG,
    };
    std::fs::write(output_path, svg).with_context(|| {
        format!(
            "failed to write SVG placeholder to {}",
            output_path.display()
        )
    })
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Generate a thumbnail using the `image` crate (no ffmpeg required).
///
/// Decodes `input_path`, resizes to fit within `max_dim × max_dim` (aspect
/// preserved), and saves as WebP.  This path is taken for image uploads when
/// ffmpeg is unavailable.
fn image_crate_thumbnail(
    input_path: &Path,
    mime: &str,
    output_path: &Path,
    max_dim: u32,
) -> Result<()> {
    let format = mime_to_image_format(mime)
        .ok_or_else(|| anyhow::anyhow!("unsupported image MIME for thumbnail: {mime}"))?;

    let data = std::fs::read(input_path)
        .with_context(|| format!("failed to read {} for thumbnailing", input_path.display()))?;

    let img = image::load_from_memory_with_format(&data, format)
        .context("failed to decode image for thumbnail")?;

    let (w, h) = img.dimensions();
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let (tw, th) = if w > h {
        let r = max_dim as f32 / w as f32;
        (max_dim, (h as f32 * r) as u32)
    } else {
        let r = max_dim as f32 / h as f32;
        ((w as f32 * r) as u32, max_dim)
    };

    let thumb = if w <= tw && h <= th {
        img
    } else {
        img.resize(tw, th, FilterType::Triangle)
    };

    thumb
        .save_with_format(output_path, ImageFormat::WebP)
        .with_context(|| format!("failed to save WebP thumbnail to {}", output_path.display()))
}

/// Map a MIME type to an `image::ImageFormat` for decoding.
///
/// Returns `None` for types the `image` crate cannot decode (video, SVG,
/// audio).
fn mime_to_image_format(mime: &str) -> Option<ImageFormat> {
    match mime {
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/png" => Some(ImageFormat::Png),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        "image/bmp" => Some(ImageFormat::Bmp),
        "image/tiff" => Some(ImageFormat::Tiff),
        _ => None,
    }
}

/// Return the file extension to use for a thumbnail.
///
/// All thumbnails are `.webp` unless the source requires a static SVG
/// placeholder (video without ffmpeg, audio, SVG sources).
fn thumbnail_extension(mime: &str, ffmpeg_available: bool) -> &'static str {
    match mime {
        "image/svg+xml" => "svg",
        m if m.starts_with("audio/") => "svg",
        "video/webm" | "audio/webm" if !ffmpeg_available => "svg",
        _ => "webp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_ext_is_webp_for_images_no_ffmpeg() {
        // Images use image-crate fallback, so webp even without ffmpeg
        assert_eq!(thumbnail_extension("image/jpeg", false), "webp");
        assert_eq!(thumbnail_extension("image/png", false), "webp");
        assert_eq!(thumbnail_extension("image/webp", false), "webp");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_video_without_ffmpeg() {
        assert_eq!(thumbnail_extension("video/webm", false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_webp_for_video_with_ffmpeg() {
        assert_eq!(thumbnail_extension("video/webm", true), "webp");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_audio() {
        assert_eq!(thumbnail_extension("audio/mpeg", true), "svg");
        assert_eq!(thumbnail_extension("audio/mpeg", false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_svg_source() {
        assert_eq!(thumbnail_extension("image/svg+xml", true), "svg");
        assert_eq!(thumbnail_extension("image/svg+xml", false), "svg");
    }
}
