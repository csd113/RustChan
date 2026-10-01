//! Pure-Rust first-page PDF previews with a conservative resource safety gate.
//!
//! Hayro does not expose an allocation/work budget for arbitrary PDF programs.
//! Until it does, compressed streams and recursive rendering resources use the
//! existing placeholder. Small vector/text documents can be rendered safely.

use anyhow::{ensure, Context as _, Result};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Name, ObjectIdentifier};
use hayro::hayro_syntax::reader::{Reader, ReaderExt as _};
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Maximum source size admitted to the in-process preview renderer.
const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum syntax tokens admitted to one bounded rendering program.
const MAX_PREVIEW_TOKENS: usize = 100_000;
/// Bound indirect objects before Hayro constructs its cached page tree.
const MAX_PREVIEW_OBJECTS: usize = 4096;
/// Conservative source-byte × viewport-pixel ceiling for raster work.
/// Oversized programs or viewports use SVG rather than starting the renderer.
const MAX_PREVIEW_WORK: u64 = 512 * 1024 * 1024;

/// Render a supported first page, rejecting unsafe/unsupported preview features.
#[expect(
    clippy::redundant_pub_crate,
    reason = "PDF rendering is an internal media helper"
)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "finite positive viewport dimensions are clamped to a checked u16 bound before rounding"
)]
pub(crate) fn render(input: &Path, output: &Path, max_dim: u32) -> Result<()> {
    let max_dim = u16::try_from(max_dim).context("PDF thumbnail bound is too large")?;
    ensure!(max_dim > 0, "PDF thumbnail bound is empty");
    let source = std::fs::File::open(input)?;
    ensure!(
        source.metadata()?.len() <= MAX_PREVIEW_BYTES,
        "PDF preview exceeds source budget"
    );
    let mut bytes = Vec::new();
    source.take(MAX_PREVIEW_BYTES + 1).read_to_end(&mut bytes)?;
    preflight(&bytes)?;
    let source_length = u64::try_from(bytes.len())?;
    let pdf = Pdf::new(bytes).map_err(|error| anyhow::anyhow!("PDF parse failed: {error:?}"))?;
    ensure!(
        pdf.len() <= MAX_PREVIEW_OBJECTS,
        "PDF preview exceeds object budget"
    );
    let page = pdf.pages().first().context("PDF has no first page")?;
    let (width, height) = page.render_dimensions();
    ensure!(
        width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0
            && width <= 1_000_000.0
            && height <= 1_000_000.0,
        "invalid PDF page geometry"
    );
    let scale = f32::from(max_dim) / width.max(height);
    let pixel_width = (width * scale).round().clamp(1.0, f32::from(max_dim)) as u16;
    let pixel_height = (height * scale).round().clamp(1.0, f32::from(max_dim)) as u16;
    super::images::validate_dimensions(u32::from(pixel_width), u32::from(pixel_height))?;
    ensure!(
        source_length * u64::from(pixel_width) * u64::from(pixel_height) <= MAX_PREVIEW_WORK,
        "PDF preview exceeds raster work budget"
    );
    let warning = Arc::new(AtomicBool::new(false));
    let warning_sink = Arc::clone(&warning);
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |_| {
            warning_sink.store(true, Ordering::Relaxed);
        }),
        render_annotations: false,
        ..InterpreterSettings::default()
    };
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &settings,
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some(pixel_width),
            height: Some(pixel_height),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        },
    );
    ensure!(
        !warning.load(Ordering::Relaxed),
        "PDF renderer reported an unsupported feature"
    );
    let image = image::RgbaImage::from_raw(
        u32::from(pixel_width),
        u32::from(pixel_height),
        pixmap.data_as_u8_slice().to_vec(),
    )
    .context("invalid PDF raster buffer")?;
    let parent = output.parent().context("PDF output has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    image::DynamicImage::ImageRgba8(image)
        .write_to(temporary.as_file_mut(), image::ImageFormat::WebP)?;
    temporary.persist(output).context("publish PDF preview")?;
    Ok(())
}

/// Admit a small nonrecursive PDF program before parsing any compressed data.
///
/// This gate is deliberately conservative: unsupported previews never reject an
/// upload. Name escapes are refused so escaped resource keys cannot bypass it.
fn preflight(bytes: &[u8]) -> Result<()> {
    ensure!(
        u64::try_from(bytes.len())? <= MAX_PREVIEW_BYTES,
        "PDF preview exceeds source budget"
    );
    ensure!(
        !bytes.contains(&b'#'),
        "escaped PDF names require placeholder preview"
    );
    for name in bytes.split(|byte| byte.is_ascii_whitespace() || b"/[]<>(){}".contains(byte)) {
        ensure!(
            !matches!(
                name,
                b"Filter"
                    | b"F"
                    | b"Encrypt"
                    | b"XObject"
                    | b"Pattern"
                    | b"Shading"
                    | b"Function"
                    | b"FontFile"
                    | b"FontFile2"
                    | b"FontFile3"
                    | b"Type3"
                    | b"SMask"
                    | b"AcroForm"
                    | b"ObjStm"
                    | b"XRef"
                    | b"Annots"
                    | b"Prev"
                    | b"XRefStm"
            ),
            "unsupported PDF preview resource"
        );
    }
    let mut depth = 0_u32;
    let mut tokens = 0_usize;
    for token in bytes.split(u8::is_ascii_whitespace) {
        tokens = tokens.checked_add(1).context("PDF token count overflow")?;
        ensure!(
            tokens <= MAX_PREVIEW_TOKENS,
            "PDF preview exceeds work budget"
        );
        // Bound declared xref sizes, lengths and coordinates before parsing.
        for value in token.split(|byte| b"/[]<>(){}%".contains(byte)) {
            if let Ok(number) = std::str::from_utf8(value).unwrap_or("").parse::<f64>() {
                ensure!(
                    number.is_finite() && number.abs() <= 1_000_000.0,
                    "PDF numeric value exceeds safety budget"
                );
            }
        }
        for byte in token {
            match byte {
                b'[' | b'<' | b'(' => {
                    depth = depth.checked_add(1).context("PDF nesting overflow")?;
                    ensure!(depth <= 64, "PDF syntax nesting exceeds safety budget");
                }
                b']' | b'>' | b')' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    preflight_page_tree(bytes)
}

/// Admit only flat page trees made from bounded, direct dictionary objects.
///
/// Hayro eagerly traverses indirect `/Kids` during `Pdf::new`, without a graph
/// depth/cycle budget. Inspect dictionaries without resolving references first.
/// Nested trees, aliases, duplicate revisions and indirect stream lengths use
/// the placeholder rather than entering that unbudgeted traversal.
fn preflight_page_tree(bytes: &[u8]) -> Result<()> {
    let mut reader = Reader::new(bytes);
    let mut objects = BTreeMap::new();
    loop {
        reader.skip_white_spaces_and_comments();
        if reader.peek_tag(b"xref").is_some() {
            break;
        }
        ensure!(
            objects.len() < MAX_PREVIEW_OBJECTS,
            "PDF object budget exceeded"
        );
        let id = reader
            .read_without_context::<ObjectIdentifier>()
            .context("PDF preview requires simple indirect objects")?;
        reader.skip_white_spaces_and_comments();
        let dict = reader
            .read_without_context::<Dict<'_>>()
            .context("PDF preview requires dictionary objects")?;
        if dict.contains_key(b"Type") {
            ensure!(
                matches!(dict.get_raw::<Name<'_>>(b"Type"), Some(MaybeRef::NotRef(_))),
                "indirect PDF object types require placeholder preview"
            );
        }
        reader.skip_white_spaces_and_comments();
        if reader.forward_tag(b"stream").is_some() {
            match reader.read_byte() {
                Some(b'\r') => {
                    let _newline = reader.forward_tag(b"\n");
                }
                Some(b'\n') => {}
                _ => anyhow::bail!("invalid PDF stream delimiter"),
            }
            let length = usize::try_from(
                dict.get::<i32>(b"Length")
                    .context("PDF preview requires direct stream lengths")?,
            )?;
            let stream = reader.read_bytes(length).context("truncated PDF stream")?;
            ensure!(
                !stream
                    .split(|byte| byte.is_ascii_whitespace() || b"/[]<>(){}".contains(byte))
                    .any(|token| token == b"obj"),
                "object-like stream data requires placeholder preview"
            );
            reader.skip_white_spaces_and_comments();
            reader
                .forward_tag(b"endstream")
                .context("PDF stream length mismatch")?;
            reader.skip_white_spaces_and_comments();
        }
        reader
            .forward_tag(b"endobj")
            .context("unterminated PDF object")?;
        ensure!(
            objects
                .insert((id.obj_number, id.gen_number), dict)
                .is_none(),
            "duplicate PDF objects require placeholder preview"
        );
    }
    let mut leaves = 0;
    for dict in objects.values() {
        if !dict.contains_key(b"Kids") {
            continue;
        }
        let Some(MaybeRef::NotRef(kids)) = dict.get_raw::<Array<'_>>(b"Kids") else {
            anyhow::bail!("indirect PDF page lists require placeholder preview");
        };
        for kid in kids.raw_iter() {
            leaves += 1;
            ensure!(leaves <= MAX_PREVIEW_OBJECTS, "PDF page budget exceeded");
            let reference = kid
                .as_obj_ref()
                .context("PDF preview requires indirect leaf pages")?;
            let leaf = objects
                .get(&(reference.obj_number, reference.gen_number))
                .context("PDF page reference is missing")?;
            ensure!(
                !leaf.contains_key(b"Kids")
                    && leaf.get::<Name<'_>>(b"Type").as_deref() == Some(b"Page".as_slice()),
                "recursive PDF page trees require placeholder preview"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_has_bounded_dimensions_and_expected_colour() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("page.webp");
        render(&root.join("simple.pdf"), &output, 100)?;
        let image = image::open(output)?.into_rgba8();
        ensure!(image.dimensions() == (100, 80), "PDF aspect ratio changed");
        let pixel = image.get_pixel(20, 20);
        ensure!(
            pixel.0.get(..3) == Some(&[255, 0, 0]),
            "first-page red rectangle was not rendered"
        );
        Ok(())
    }

    #[test]
    fn standard_font_text_renders_with_bounded_dimensions() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("text.webp");
        render(&root.join("text.pdf"), &output, 100)?;
        let image = image::open(output)?.into_rgb8();
        ensure!(image.dimensions() == (100, 80), "text PDF aspect changed");
        ensure!(
            image
                .pixels()
                .filter(|pixel| pixel.0.iter().all(|channel| *channel < 100))
                .count()
                > 50,
            "text PDF has no visible glyphs"
        );
        let oversized = dir.path().join("large-text.webp");
        ensure!(
            render(&root.join("text.pdf"), &oversized, 4096).is_err(),
            "unbudgeted viewport rendered"
        );
        ensure!(!oversized.exists(), "unbudgeted viewport published output");
        Ok(())
    }

    #[test]
    fn malformed_and_unbudgeted_features_fail_before_publication() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("unsafe.pdf");
        let output = dir.path().join("page.webp");
        for bytes in [
            b"%PDF-1.7\n%%EOF".as_slice(),
            b"%PDF-1.7\n/Filter /FlateDecode\n%%EOF",
            b"%PDF-1.7\n/Fi#6cter\n%%EOF",
            b"%PDF-1.7\n/XObject << >>\n%%EOF",
            b"%PDF-1.7\n/FontFile2 1 0 R\n%%EOF",
        ] {
            std::fs::write(&input, bytes)?;
            ensure!(render(&input, &output, 100).is_err(), "unsafe PDF rendered");
            ensure!(!output.exists(), "failed render published output");
        }
        Ok(())
    }

    #[test]
    fn cyclic_indirect_page_trees_and_delimited_numbers_fail_preflight() -> Result<()> {
        for bytes in [
            b"%PDF-1.4\n1 0 obj << /Type /Pages /Kids [1 0 R] >> endobj\nxref".as_slice(),
            b"%PDF-1.4\n1 0 obj << /Type /Pages /Kids [2 0 R] >> endobj\n2 0 obj << /Type /Pages /Kids [1 0 R] >> endobj\nxref",
            b"%PDF-1.4\n1 0 obj << /Type /Pages /Kids 2 0 R >> endobj\n2 0 obj [1 0 R] endobj\nxref",
            b"%PDF-1.4\n1 0 obj << /Type /Page >> endobj\n1 0 obj << /Type /Pages /Kids [1 0 R] >> endobj\nxref",
            b"%PDF-1.4\n1 0 obj << /MediaBox[0 0 9999999999 80] >> endobj\nxref",
        ] {
            ensure!(preflight(bytes).is_err(), "hostile PDF passed preflight");
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        preflight(&std::fs::read(root.join("simple.pdf"))?)?;
        Ok(())
    }
}
