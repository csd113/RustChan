// Thumbnail generation for uploaded media.

use anyhow::{Context as _, Result};
use image::{imageops::FilterType, GenericImageView as _, ImageFormat};
use std::path::{Path, PathBuf};

use super::ffmpeg;

#[cfg(test)]
std::thread_local! {
    static PDF_RENDERER_TEST_MODE: std::cell::Cell<Option<TestPdfRendererMode>> = const {
        std::cell::Cell::new(None)
    };
}

// Static placeholder SVGs
// Note: these SVG strings contain `"#` sequences (e.g. fill="#0a0f0a") which
// would terminate a `r#"..."#` raw string early.  We use `r##"..."##` so the
// closing delimiter requires two consecutive `#` signs, which never appear in
// the SVG body.
/// Static placeholder used when a video preview cannot be generated.
const VIDEO_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <polygon points="108,95 108,155 165,125" fill="#00c840"/>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">VIDEO</text>
</svg>"##;

/// Static placeholder used for audio uploads.
const AUDIO_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <text x="125" y="140" text-anchor="middle" fill="#00c840" font-family="monospace" font-size="48">&#9835;</text>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">AUDIO</text>
</svg>"##;

/// Static placeholder used when a PDF preview cannot be generated.
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

// Public API
/// What kind of static placeholder to write when the real thumbnail cannot be
/// generated.
#[derive(Debug, Clone, Copy)]
pub enum PlaceholderKind {
    /// Generic video-file placeholder.
    Video,
    /// Generic audio-file placeholder.
    Audio,
    /// Generic PDF-document placeholder.
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Built-in PDF renderer used for supported first-page previews.
pub enum PdfRenderer {
    /// Pure-Rust Hayro PDF rasterizer.
    Hayro,
}

impl PdfRenderer {
    /// Return the renderer name used in maintenance reports.
    #[must_use]
    pub const fn binary_name(self) -> &'static str {
        "hayro (Rust)"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result of attempting to generate a PDF preview.
pub enum PdfThumbnailOutcome {
    /// A real first-page preview was generated.
    Rendered {
        /// Renderer that produced the preview.
        renderer: PdfRenderer,
    },
    /// No renderer succeeded, so a static placeholder was written.
    Placeholder,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Test-only override for PDF preview failure behavior.
pub enum TestPdfRendererMode {
    /// Simulate no available renderer.
    Unavailable,
    /// Simulate a renderer that exits unsuccessfully.
    Fail,
    /// Simulate a renderer that exceeds its timeout.
    Timeout,
}

#[cfg(test)]
#[derive(Debug)]
/// Restore this test thread's override without affecting concurrent tests.
pub struct PdfRendererTestGuard {
    previous: Option<TestPdfRendererMode>,
    // The override belongs to the originating thread, so its guard is not Send.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl Drop for PdfRendererTestGuard {
    fn drop(&mut self) {
        PDF_RENDERER_TEST_MODE.with(|value| value.set(self.previous));
    }
}

#[cfg(test)]
/// Install a test-only override on this synchronous processing thread.
#[must_use]
pub fn override_pdf_renderer_mode(mode: TestPdfRendererMode) -> PdfRendererTestGuard {
    PdfRendererTestGuard {
        previous: PDF_RENDERER_TEST_MODE.with(|value| value.replace(Some(mode))),
        _thread: std::marker::PhantomData,
    }
}

/// Generate a thumbnail for a media file and write it to `output_path`.
///
/// All thumbnails are produced as WebP.  The strategy depends on the MIME
/// type and whether ffmpeg (and its libwebp encoder) is available:
///
/// | Source MIME       | ffmpeg | libwebp | Action                          |
/// |-------------------|--------|---------|---------------------------------|
/// | `image/*`         | either | —       | Rust decode → resize → WebP     |
/// | `application/pdf` | either | —       | Rust preview or SVG placeholder |
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
        // SVG and audio: always use static placeholder
        "image/svg+xml" => write_placeholder(output_path, PlaceholderKind::Video)
            .map(|()| output_path.to_path_buf()),
        m if m.starts_with("audio/") => write_placeholder(output_path, PlaceholderKind::Audio)
            .map(|()| output_path.to_path_buf()),

        "application/pdf" => {
            let placeholder_path = pdf_placeholder_output_path(output_path);
            drop(std::fs::remove_file(output_path));
            drop(std::fs::remove_file(&placeholder_path));
            match pdf_first_page_thumbnail(input_path, output_path, max_dim) {
                Ok(PdfThumbnailOutcome::Rendered { .. }) => Ok(output_path.to_path_buf()),
                Ok(PdfThumbnailOutcome::Placeholder) => Ok(placeholder_path),
                Err(error) => {
                    drop(std::fs::remove_file(output_path));
                    drop(std::fs::remove_file(&placeholder_path));
                    Err(error)
                }
            }
        }

        // Video (WebM, MP4, and any other video/*): requires ffmpeg AND libwebp
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

        // WebP: skip ffmpeg entirely — use image crate directly
        // ffmpeg fails on animated WebP (VP8L) and emits a spurious warning
        // even though the image crate handles all WebP variants correctly.
        "image/webp" => image_crate_thumbnail(input_path, mime, output_path, max_dim)
            .map(|()| output_path.to_path_buf()),

        _ if mime.starts_with("image/") => {
            image_crate_thumbnail_or_placeholder(input_path, mime, output_path, max_dim)
        }

        // Unknown MIME: placeholder
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

// Internal helpers
/// Generate a thumbnail using the `image` crate (no ffmpeg required).
///
/// Decodes `input_path`, resizes to fit within `max_dim × max_dim` (aspect
/// preserved), and saves as WebP. This path handles all supported images.
fn image_crate_thumbnail(
    input_path: &Path,
    _mime: &str,
    output_path: &Path,
    max_dim: u32,
) -> Result<()> {
    let img = super::images::decode_still(input_path)?;

    let (w, h) = img.dimensions();
    let (tw, th) = if w > h {
        (max_dim, scaled_dimension(h, max_dim, w))
    } else {
        (scaled_dimension(w, max_dim, h), max_dim)
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

/// Generate an image fallback or replace a failed decode with a placeholder.
fn image_crate_thumbnail_or_placeholder(
    input_path: &Path,
    mime: &str,
    output_path: &Path,
    max_dim: u32,
) -> Result<PathBuf> {
    match image_crate_thumbnail(input_path, mime, output_path, max_dim) {
        Ok(()) => Ok(output_path.to_path_buf()),
        Err(error) => {
            tracing::warn!(
                mime,
                input = %input_path.display(),
                error = %error,
                "image thumbnail generation failed; using generic placeholder"
            );
            let svg_path = output_path.with_extension("svg");
            drop(std::fs::remove_file(output_path));
            write_placeholder(&svg_path, PlaceholderKind::Video).map(|()| svg_path)
        }
    }
}

/// Render a supported PDF first page or publish its existing placeholder.
fn pdf_first_page_thumbnail(
    input_path: &Path,
    output_path: &Path,
    max_dim: u32,
) -> Result<PdfThumbnailOutcome> {
    let result = render_pdf(input_path, output_path, max_dim);
    match result {
        Ok(()) => Ok(PdfThumbnailOutcome::Rendered {
            renderer: PdfRenderer::Hayro,
        }),
        Err(error) => {
            tracing::warn!(%error, "PDF preview unavailable; using built-in generic thumbnail");
            drop(std::fs::remove_file(output_path));
            write_placeholder(
                &pdf_placeholder_output_path(output_path),
                PlaceholderKind::Pdf,
            )?;
            Ok(PdfThumbnailOutcome::Placeholder)
        }
    }
}

/// Apply test failure injection, then invoke the in-process renderer.
fn render_pdf(input_path: &Path, output_path: &Path, max_dim: u32) -> Result<()> {
    #[cfg(test)]
    if PDF_RENDERER_TEST_MODE.with(std::cell::Cell::get).is_some() {
        anyhow::bail!("injected PDF renderer failure");
    }
    super::pdf::render(input_path, output_path, max_dim)
}

/// Scale one dimension proportionally using widened integer arithmetic.
fn scaled_dimension(side: u32, max_dimension: u32, denominator: u32) -> u32 {
    let scaled =
        u64::from(side).saturating_mul(u64::from(max_dimension)) / u64::from(denominator.max(1));
    u32::try_from(scaled).unwrap_or(max_dimension)
}

/// Return the SVG fallback sibling for a requested PDF thumbnail path.
fn pdf_placeholder_output_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("svg")
}

#[must_use]
/// Report the built-in PDF renderer; no executable detection is needed.
pub fn detect_pdf_renderers() -> Vec<PdfRenderer> {
    vec![PdfRenderer::Hayro]
}

/// Map a MIME type to an `image::ImageFormat` for decoding.
///
/// Returns `None` for types the `image` crate cannot decode (video, SVG,
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
    fn valid_unsupported_pdf_retains_upload_and_uses_svg_preview() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/inline-image.pdf");
        let before = std::fs::read(&source)?;
        let output = dir.path().join("page.webp");
        let result = generate_thumbnail(&source, "application/pdf", &output, 100, false, false)?;
        anyhow::ensure!(
            result.extension().and_then(std::ffi::OsStr::to_str) == Some("svg"),
            "unsupported preview did not use SVG"
        );
        anyhow::ensure!(
            std::fs::read_to_string(result)?.contains("PDF"),
            "PDF placeholder is invalid"
        );
        anyhow::ensure!(
            std::fs::read(&source)? == before,
            "unsupported preview changed the download"
        );
        anyhow::ensure!(!output.exists(), "unsupported preview published WebP");
        Ok(())
    }

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
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn write_pdf_placeholder_outputs_svg() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let output = tempdir.path().join("thumb.svg");
        write_placeholder(&output, PlaceholderKind::Pdf)?;
        let svg = std::fs::read_to_string(&output)?;
        assert!(svg.contains("PDF"), "placeholder must identify PDF content");
        assert!(svg.contains("<svg"), "placeholder must be SVG markup");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn unsupported_image_thumbnail_falls_back_to_svg_placeholder_without_ffmpeg() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let input = tempdir.path().join("input.heic");
        let output = tempdir.path().join("thumb.webp");
        std::fs::write(&input, b"not decoded by image crate")?;

        let actual = generate_thumbnail(&input, "image/heic", &output, 64, false, false)?;

        assert_eq!(
            actual.extension().and_then(|ext| ext.to_str()),
            Some("svg"),
            "fallback path must use the SVG extension"
        );
        assert!(actual.exists(), "fallback placeholder must exist");
        assert!(!output.exists(), "failed WebP output must not remain");
        Ok(())
    }
    #[test]
    fn pdf_failure_override_does_not_affect_another_thread() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/text.pdf");
        let output = dir.path().join("thread.webp");
        let _override = override_pdf_renderer_mode(TestPdfRendererMode::Fail);
        let generated = std::thread::spawn(move || {
            generate_thumbnail(&input, "application/pdf", &output, 100, false, false)
        })
        .join()
        .map_err(|_| anyhow::anyhow!("PDF preview test thread panicked"))??;
        anyhow::ensure!(
            generated.extension().is_some_and(|ext| ext == "webp"),
            "another thread inherited PDF failure injection"
        );
        Ok(())
    }
}
