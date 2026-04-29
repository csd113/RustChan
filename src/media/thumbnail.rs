// Thumbnail generation for uploaded media.

use anyhow::{Context, Result};
use image::{imageops::FilterType, GenericImageView, ImageFormat};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::ffmpeg;

const MAX_IMAGE_THUMBNAIL_PIXELS: u64 = 100_000_000;

#[cfg(test)]
static PDF_RENDERER_TEST_MODE: std::sync::RwLock<Option<TestPdfRendererMode>> =
    std::sync::RwLock::new(None);
#[cfg(test)]
static PDF_RENDERER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

const PDF_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" rx="20" fill="#0a0f0a"/>
  <rect x="48" y="26" width="154" height="198" rx="14" fill="#f2f0ea" stroke="#203020" stroke-width="4"/>
  <path d="M161 26v44h41" fill="#d7d2c5"/>
  <path d="M161 26v44h41" fill="none" stroke="#203020" stroke-width="4" stroke-linejoin="round"/>
  <rect x="68" y="86" width="114" height="50" rx="9" fill="#8f2328"/>
  <text x="125" y="119" text-anchor="middle" fill="#fff7f2" font-family="monospace" font-size="30" font-weight="700">PDF</text>
  <rect x="72" y="154" width="106" height="8" rx="4" fill="#9aa29a"/>
  <rect x="72" y="170" width="86" height="8" rx="4" fill="#9aa29a"/>
  <rect x="72" y="186" width="96" height="8" rx="4" fill="#9aa29a"/>
</svg>"##;

// ─── Public API ───────────────────────────────────────────────────────────────

/// What kind of static placeholder to write when the real thumbnail cannot be
/// generated.
#[derive(Debug, Clone, Copy)]
pub enum PlaceholderKind {
    Video,
    Audio,
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfRenderer {
    Pdftoppm,
    Mutool,
    Qlmanage,
}

impl PdfRenderer {
    #[must_use]
    pub const fn binary_name(self) -> &'static str {
        match self {
            Self::Pdftoppm => "pdftoppm",
            Self::Mutool => "mutool",
            Self::Qlmanage => "qlmanage",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfThumbnailOutcome {
    Rendered { renderer: PdfRenderer },
    Placeholder,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPdfRendererMode {
    Unavailable,
    Fail,
    Timeout,
}

#[cfg(test)]
pub struct PdfRendererTestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for PdfRendererTestGuard {
    fn drop(&mut self) {
        *PDF_RENDERER_TEST_MODE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
pub fn override_pdf_renderer_mode(mode: TestPdfRendererMode) -> PdfRendererTestGuard {
    let guard = PDF_RENDERER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *PDF_RENDERER_TEST_MODE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mode);
    PdfRendererTestGuard { _lock: guard }
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

        "application/pdf" => {
            let placeholder_path = pdf_placeholder_output_path(output_path);
            let _ = std::fs::remove_file(output_path);
            let _ = std::fs::remove_file(&placeholder_path);
            match pdf_first_page_thumbnail(input_path, output_path, max_dim) {
                Ok(PdfThumbnailOutcome::Rendered { .. }) => Ok(output_path.to_path_buf()),
                Ok(PdfThumbnailOutcome::Placeholder) => Ok(placeholder_path),
                Err(error) => {
                    let _ = std::fs::remove_file(output_path);
                    let _ = std::fs::remove_file(&placeholder_path);
                    Err(error)
                }
            }
        }

        // ── Video (WebM, MP4, and any other video/*): requires ffmpeg AND libwebp ─────────────────────
        // `thumbnail_output_path` pre-selects `.webp` when both are present.
        // If ffmpeg_thumbnail then fails, write the SVG placeholder to the
        // `.svg`-extension sibling so the file content and extension match.
        // The `else` branch (ffmpeg absent / libwebp absent) already has the
        // `.svg` extension pre-selected by `thumbnail_output_path`, so no
        // rename is needed there.
        m if m.starts_with("video/") => {
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
        PlaceholderKind::Pdf => PDF_PLACEHOLDER_SVG,
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
    let (width, height) = image::image_dimensions(input_path).with_context(|| {
        format!(
            "failed to inspect {} before thumbnailing",
            input_path.display()
        )
    })?;
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_THUMBNAIL_PIXELS {
        anyhow::bail!("image dimensions {width}x{height} exceed thumbnail safety limit");
    }

    let data = std::fs::read(input_path)
        .with_context(|| format!("failed to read {} for thumbnailing", input_path.display()))?;

    let img = image::load_from_memory_with_format(&data, format)
        .context("failed to decode image for thumbnail")?;

    let (w, h) = img.dimensions();
    // This cast is a local display or math conversion, and the values are already bounded by surrounding invariants.
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

#[allow(clippy::too_many_lines)]
fn pdf_first_page_thumbnail(
    input_path: &Path,
    output_path: &Path,
    max_dim: u32,
) -> Result<PdfThumbnailOutcome> {
    let parent = output_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("PDF thumbnail output has no parent"))?;
    let temp_dir = tempfile::Builder::new()
        .prefix("rustchan-pdf-thumb-")
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "failed to create temp PDF thumbnail dir in {}",
                parent.display()
            )
        })?;
    let png_path = temp_dir.path().join("page1.png");

    let render_result = match render_pdf_with_pdftoppm(input_path, &png_path, max_dim) {
        Ok(()) => Some(PdfRenderer::Pdftoppm),
        Err(pdftoppm_error) => match render_pdf_with_mutool(input_path, &png_path, max_dim) {
            Ok(()) => Some(PdfRenderer::Mutool),
            Err(mutool_error) => {
                match render_pdf_with_qlmanage(input_path, &png_path, max_dim, temp_dir.path()) {
                    Ok(()) => Some(PdfRenderer::Qlmanage),
                    Err(qlmanage_error) => {
                        tracing::warn!(
                            pdftoppm = %pdftoppm_error,
                            mutool = %mutool_error,
                            qlmanage = %qlmanage_error,
                            "PDF thumbnail rendering failed; using built-in generic thumbnail"
                        );
                        write_placeholder(
                            &pdf_placeholder_output_path(output_path),
                            PlaceholderKind::Pdf,
                        )?;
                        return Ok(PdfThumbnailOutcome::Placeholder);
                    }
                }
            }
        },
    };

    let Some(renderer) = render_result else {
        write_placeholder(
            &pdf_placeholder_output_path(output_path),
            PlaceholderKind::Pdf,
        )?;
        return Ok(PdfThumbnailOutcome::Placeholder);
    };

    let (width, height) = match image::image_dimensions(&png_path) {
        Ok(dimensions) => dimensions,
        Err(error) => {
            tracing::warn!(
                renderer = renderer.binary_name(),
                path = %png_path.display(),
                %error,
                "Rendered PDF thumbnail dimensions could not be inspected; using built-in generic thumbnail"
            );
            write_placeholder(
                &pdf_placeholder_output_path(output_path),
                PlaceholderKind::Pdf,
            )?;
            return Ok(PdfThumbnailOutcome::Placeholder);
        }
    };
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_THUMBNAIL_PIXELS {
        tracing::warn!(
            renderer = renderer.binary_name(),
            path = %png_path.display(),
            width,
            height,
            "Rendered PDF thumbnail exceeds safety pixel limit; using built-in generic thumbnail"
        );
        write_placeholder(
            &pdf_placeholder_output_path(output_path),
            PlaceholderKind::Pdf,
        )?;
        return Ok(PdfThumbnailOutcome::Placeholder);
    }

    let image = match image::open(&png_path) {
        Ok(image) => image,
        Err(error) => {
            tracing::warn!(
                renderer = renderer.binary_name(),
                path = %png_path.display(),
                %error,
                "Rendered PDF thumbnail could not be decoded; using built-in generic thumbnail"
            );
            write_placeholder(
                &pdf_placeholder_output_path(output_path),
                PlaceholderKind::Pdf,
            )?;
            return Ok(PdfThumbnailOutcome::Placeholder);
        }
    };
    let thumb = image.thumbnail(max_dim, max_dim);
    if let Err(error) = thumb.save_with_format(output_path, ImageFormat::WebP) {
        tracing::warn!(
            renderer = renderer.binary_name(),
            output = %output_path.display(),
            %error,
            "Saving PDF thumbnail failed; using built-in generic thumbnail"
        );
        let _ = std::fs::remove_file(output_path);
        write_placeholder(
            &pdf_placeholder_output_path(output_path),
            PlaceholderKind::Pdf,
        )?;
        return Ok(PdfThumbnailOutcome::Placeholder);
    }

    Ok(PdfThumbnailOutcome::Rendered { renderer })
}

fn render_pdf_with_pdftoppm(input_path: &Path, png_path: &Path, max_dim: u32) -> Result<()> {
    #[cfg(test)]
    {
        if let Some(result) = test_renderer_override(PdfRenderer::Pdftoppm) {
            return result;
        }
    }
    let prefix = png_path.with_extension("");
    let status = run_pdf_renderer_with_timeout(Command::new("pdftoppm").args([
        "-f",
        "1",
        "-l",
        "1",
        "-singlefile",
        "-png",
        "-scale-to",
        &max_dim.to_string(),
        path_to_str(input_path)?,
        path_to_str(&prefix)?,
    ]))
    .context("failed to run pdftoppm")?;
    if status.success() && png_path.exists() {
        Ok(())
    } else {
        anyhow::bail!("pdftoppm failed")
    }
}

fn render_pdf_with_mutool(input_path: &Path, png_path: &Path, max_dim: u32) -> Result<()> {
    #[cfg(test)]
    {
        if let Some(result) = test_renderer_override(PdfRenderer::Mutool) {
            return result;
        }
    }
    let mut command = build_mutool_command(input_path, png_path, max_dim)?;
    let status = run_pdf_renderer_with_timeout(&mut command).context("failed to run mutool")?;
    if status.success() && png_path.exists() {
        Ok(())
    } else {
        anyhow::bail!("mutool failed")
    }
}

fn build_mutool_command(input_path: &Path, png_path: &Path, max_dim: u32) -> Result<Command> {
    let mut command = Command::new("mutool");
    command.args([
        "draw",
        "-q",
        "-w",
        &max_dim.to_string(),
        "-h",
        &max_dim.to_string(),
        "-o",
        path_to_str(png_path)?,
        path_to_str(input_path)?,
        "1",
    ]);
    Ok(command)
}

fn render_pdf_with_qlmanage(
    input_path: &Path,
    png_path: &Path,
    max_dim: u32,
    temp_dir: &Path,
) -> Result<()> {
    #[cfg(test)]
    {
        if let Some(result) = test_renderer_override(PdfRenderer::Qlmanage) {
            return result;
        }
    }
    let status = run_pdf_renderer_with_timeout(Command::new("qlmanage").args([
        "-t",
        "-s",
        &max_dim.to_string(),
        "-o",
        path_to_str(temp_dir)?,
        path_to_str(input_path)?,
    ]))
    .context("failed to run qlmanage")?;
    let ql_path = input_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| temp_dir.join(format!("{name}.png")))
        .ok_or_else(|| anyhow::anyhow!("PDF input path has no UTF-8 filename"))?;
    if status.success() && ql_path.exists() {
        std::fs::rename(&ql_path, png_path)
            .with_context(|| format!("failed to move qlmanage thumbnail {}", ql_path.display()))?;
        Ok(())
    } else {
        anyhow::bail!("qlmanage failed")
    }
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path contains non-UTF-8 characters: {}", path.display()))
}

fn run_pdf_renderer_with_timeout(command: &mut Command) -> Result<std::process::ExitStatus> {
    let timeout = Duration::from_secs(10);
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn PDF renderer")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("PDF renderer timed out after {}s", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn pdf_placeholder_output_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("svg")
}

#[must_use]
pub fn detect_pdf_renderers() -> Vec<PdfRenderer> {
    [
        PdfRenderer::Pdftoppm,
        PdfRenderer::Mutool,
        PdfRenderer::Qlmanage,
    ]
    .into_iter()
    .filter(|renderer| probe_renderer(*renderer))
    .collect()
}

fn probe_renderer(renderer: PdfRenderer) -> bool {
    #[cfg(test)]
    if matches!(
        *PDF_RENDERER_TEST_MODE
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        Some(TestPdfRendererMode::Unavailable)
    ) {
        return false;
    }

    Command::new(renderer.binary_name())
        .arg("-h")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(test)]
fn test_renderer_override(renderer: PdfRenderer) -> Option<Result<()>> {
    let mode = *PDF_RENDERER_TEST_MODE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match mode {
        Some(TestPdfRendererMode::Unavailable) => Some(Err(anyhow::anyhow!(
            "{} unavailable in test override",
            renderer.binary_name()
        ))),
        Some(TestPdfRendererMode::Fail) => Some(Err(anyhow::anyhow!(
            "{} failed in test override",
            renderer.binary_name()
        ))),
        Some(TestPdfRendererMode::Timeout) => Some(Err(anyhow::anyhow!(
            "{} timed out in test override",
            renderer.binary_name()
        ))),
        None => None,
    }
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
        // fallback is an SVG placeholder.  This applies to all video/* types,
        // not just WebM — MP4 and any other video format go through ffmpeg the
        // same way and need the same extension pre-selection logic.
        m if m.starts_with("video/") && (!ffmpeg_available || !ffmpeg_webp_available) => "svg",
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
        assert_eq!(thumbnail_extension("video/mp4", false, false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_svg_for_video_with_ffmpeg_but_no_webp() {
        // ffmpeg available but libwebp missing — placeholder path must be .svg
        assert_eq!(thumbnail_extension("video/webm", true, false), "svg");
        assert_eq!(thumbnail_extension("video/mp4", true, false), "svg");
    }

    #[test]
    fn thumbnail_ext_is_webp_for_video_with_ffmpeg_and_webp() {
        assert_eq!(thumbnail_extension("video/webm", true, true), "webp");
        assert_eq!(thumbnail_extension("video/mp4", true, true), "webp");
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

    #[test]
    fn pdf_thumbnail_prefers_svg_sibling_for_placeholder_paths() {
        let output = Path::new("/tmp/example.webp");
        assert_eq!(
            pdf_placeholder_output_path(output),
            Path::new("/tmp/example.svg")
        );

        let svg = Path::new("/tmp/example.svg");
        assert_eq!(pdf_placeholder_output_path(svg), svg);
    }

    #[test]
    fn write_pdf_placeholder_outputs_svg() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let output = tempdir.path().join("thumb.svg");
        write_placeholder(&output, PlaceholderKind::Pdf).expect("write pdf placeholder");
        let svg = std::fs::read_to_string(&output).expect("read placeholder");
        assert!(svg.contains("PDF"));
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn mutool_command_caps_render_dimensions() {
        let command = build_mutool_command(
            Path::new("/tmp/input.pdf"),
            Path::new("/tmp/page1.png"),
            250,
        )
        .expect("build mutool command");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "draw",
                "-q",
                "-w",
                "250",
                "-h",
                "250",
                "-o",
                "/tmp/page1.png",
                "/tmp/input.pdf",
                "1",
            ]
        );
    }
}
