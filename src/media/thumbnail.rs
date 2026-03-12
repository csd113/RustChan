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
/// type and whether ffmpeg (and its libwebp encoder) is available:
///
/// | Source MIME       | ffmpeg | libwebp | Action                          |
/// |-------------------|--------|---------|---------------------------------|
/// | `image/*`         | yes    | —       | ffmpeg first-frame + WebP       |
/// | `image/*`         | no     | —       | `image` crate → resize → WebP   |
/// | `video/webm`      | yes    | yes     | ffmpeg first-frame + WebP       |
/// | `video/webm`      | yes    | no      | static SVG placeholder          |
/// | `video/webm`      | no     | —       | static SVG placeholder          |
/// | `image/svg+xml`   | either | —       | static SVG placeholder          |
/// | `audio/*`         | either | —       | static SVG placeholder          |
///
/// # Arguments
/// * `input_path`             — Absolute path to the (already converted) media file.
/// * `mime`                   — Final MIME type of the input file.
/// * `output_path`            — Where to write the thumbnail (WebP or SVG).
/// * `max_dim`                — Maximum width and height in pixels (aspect preserved).
/// * `ffmpeg_available`       — Whether ffmpeg was detected at startup.
/// * `ffmpeg_webp_available`  — Whether ffmpeg has the libwebp encoder compiled in.
///
/// # Errors
/// Returns an error only if all strategies (including placeholder writing)
/// fail.  Individual strategy failures are demoted to warnings so that a
/// thumbnail failure never causes the upload to fail.
/// Generate a thumbnail and return the **actual path written**.
///
/// The returned path may differ from `output_path` when a fallback SVG
/// placeholder is written for a video whose thumbnail extraction failed.
/// `thumbnail_output_path` selects `.webp` for video when ffmpeg+libwebp are
/// both present (because it cannot know ahead of time whether ffmpeg will
/// succeed).  If extraction fails, writing SVG bytes into a `.webp` file
/// produces a file whose content and extension disagree — browsers reject it
/// and show a broken thumbnail.  To avoid this, the video fallback writes the
/// placeholder to a `.svg` sibling path instead and returns that path, so the
/// caller can store the correct path in the database.
pub fn generate_thumbnail(
    input_path: &Path,
    mime: &str,
    output_path: &Path,
    max_dim: u32,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<PathBuf> {
    match mime {
        // ── SVG and audio: always use static placeholder ──────────────────
        "image/svg+xml" => write_placeholder(output_path, PlaceholderKind::Video)
            .map(|()| output_path.to_path_buf()),
        m if m.starts_with("audio/") => write_placeholder(output_path, PlaceholderKind::Audio)
            .map(|()| output_path.to_path_buf()),

        // ── Video (WebM): requires ffmpeg AND libwebp ─────────────────────
        // `thumbnail_output_path` pre-selects `.webp` when both are present.
        // If ffmpeg_thumbnail then fails, write the SVG placeholder to the
        // `.svg`-extension sibling so the file content and extension match.
        // The `else` branch (ffmpeg absent / libwebp absent) already has the
        // `.svg` extension pre-selected by `thumbnail_output_path`, so no
        // rename is needed there.
        "video/webm" | "audio/webm" => {
            if ffmpeg_available && ffmpeg_webp_available {
                match ffmpeg::ffmpeg_thumbnail(input_path, output_path, max_dim) {
                    Ok(()) => Ok(output_path.to_path_buf()),
                    Err(e) => {
                        tracing::warn!("ffmpeg video thumbnail failed ({}); using placeholder", e);
                        // Write the SVG placeholder with a .svg extension so its
                        // content and file extension agree.  Browsers that receive
                        // SVG bytes served as image/webp silently show nothing.
                        let svg_path = output_path.with_extension("svg");
                        write_placeholder(&svg_path, PlaceholderKind::Video).map(|()| svg_path)
                    }
                }
            } else {
                // output_path already has .svg extension in this branch.
                write_placeholder(output_path, PlaceholderKind::Video)
                    .map(|()| output_path.to_path_buf())
            }
        }

        // ── WebP: skip ffmpeg entirely — use image crate directly ─────────
        // ffmpeg fails on animated WebP (VP8L) and emits a spurious warning
        // even though the image crate handles all WebP variants correctly.
        "image/webp" => image_crate_thumbnail(input_path, mime, output_path, max_dim)
            .map(|()| output_path.to_path_buf()),

        // ── Other images: try ffmpeg, fall back to image crate ────────────
        _ if mime.starts_with("image/") => {
            if ffmpeg_available {
                match ffmpeg::ffmpeg_thumbnail(input_path, output_path, max_dim) {
                    Ok(()) => Ok(output_path.to_path_buf()),
                    Err(e) => {
                        tracing::warn!(
                            "ffmpeg image thumbnail failed ({}); falling back to image crate",
                            e
                        );
                        image_crate_thumbnail(input_path, mime, output_path, max_dim)
                            .map(|()| output_path.to_path_buf())
                    }
                }
            } else {
                image_crate_thumbnail(input_path, mime, output_path, max_dim)
                    .map(|()| output_path.to_path_buf())
            }
        }

        // ── Unknown MIME: placeholder ─────────────────────────────────────
        _ => write_placeholder(output_path, PlaceholderKind::Video)
            .map(|()| output_path.to_path_buf()),
    }
}

/// Determine the correct output path for a thumbnail given the media MIME type.
///
/// Always returns a `.webp` path, except for types that produce an
/// SVG placeholder (video without ffmpeg or libwebp, audio, svg source).
///
/// # Arguments
/// * `thumb_dir`              — The `thumbs/` directory (absolute path).
/// * `file_stem`              — UUID stem shared with the media file.
/// * `mime`                   — Final MIME of the converted media file.
/// * `ffmpeg_available`       — Whether ffmpeg was detected.
/// * `ffmpeg_webp_available`  — Whether ffmpeg has the libwebp encoder.
#[must_use]
pub fn thumbnail_output_path(
    thumb_dir: &Path,
    file_stem: &str,
    mime: &str,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> PathBuf {
    let ext = thumbnail_extension(mime, ffmpeg_available, ffmpeg_webp_available);
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
///
/// For `video/webm`, a WebP thumbnail can only be produced when ffmpeg is
/// available AND the `libwebp` encoder is compiled in.  When either is
/// absent, `ffmpeg_thumbnail` will fail and `write_placeholder` will be
/// called — we must pre-select `.svg` so the placeholder is written to a
/// path whose extension matches its actual SVG content.  Mismatching the
/// extension (SVG bytes in a `.webp` file) causes browsers to reject the
/// file and display nothing.
fn thumbnail_extension(
    mime: &str,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> &'static str {
    match mime {
        "image/svg+xml" => "svg",
        m if m.starts_with("audio/") => "svg",
        // Video thumbnails need both ffmpeg (to demux the stream) AND libwebp
        // (to encode the extracted frame as WebP).  If either is missing the
        // fallback is an SVG placeholder.
        "video/webm" | "audio/webm" if !ffmpeg_available || !ffmpeg_webp_available => "svg",
        _ => "webp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_ext_is_webp_for_images_no_ffmpeg() {
        // Images use image-crate fallback, so webp even without ffmpeg
        assert_eq!(thumbnail_extension("image/jpeg", false, false), "webp");
        assert_eq!(thumbnail_extension("image/png", false, false), "webp");
        assert_eq!(thumbnail_extension("image/webp", false, false), "webp");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_video_without_ffmpeg() {
        assert_eq!(thumbnail_extension("video/webm", false, false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_video_with_ffmpeg_but_no_webp() {
        // ffmpeg available but libwebp missing — placeholder path must be .svg
        assert_eq!(thumbnail_extension("video/webm", true, false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_webp_for_video_with_ffmpeg_and_webp() {
        assert_eq!(thumbnail_extension("video/webm", true, true), "webp");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_audio() {
        assert_eq!(thumbnail_extension("audio/mpeg", true, true), "svg");
        assert_eq!(thumbnail_extension("audio/mpeg", false, false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_svg_source() {
        assert_eq!(thumbnail_extension("image/svg+xml", true, true), "svg");
        assert_eq!(thumbnail_extension("image/svg+xml", false, false), "svg");
    }
}
