//! Strict uploads must never publish retained source metadata or failed output.

use super::{sanitize, ImageBudget};
use anyhow::{ensure, Context as _, Result};
use image::{AnimationDecoder as _, ImageDecoder as _};
use std::io::Cursor;

/// Ordinary small PNG/WebP may retain metadata; strict output must always rebuild.
#[test]
fn strict_reencodes_png_and_webp_and_strips_ancillary_data() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let pixels = image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255]));
    for (extension, mime, format) in [
        ("png", "image/png", image::ImageFormat::Png),
        ("webp", "image/webp", image::ImageFormat::WebP),
        ("bmp", "image/bmp", image::ImageFormat::Bmp),
    ] {
        let mut encoded = Cursor::new(Vec::new());
        pixels.write_to(&mut encoded, format)?;
        let mut bytes = encoded.into_inner();
        if extension == "png" {
            let chunk = png_chunk(*b"tEXt", b"Author\0private-location");
            drop(bytes.splice(33..33, chunk));
        } else if extension == "webp" {
            bytes.extend_from_slice(b"XMP \x10\0\0\0private-location");
            let size = u32::try_from(bytes.len().saturating_sub(8))?;
            bytes
                .get_mut(4..8)
                .context("RIFF header")?
                .copy_from_slice(&size.to_le_bytes());
        } else {
            bytes.extend_from_slice(b"private-location");
        }
        let input = dir.path().join(format!("input.{extension}"));
        std::fs::write(&input, &bytes)?;
        let result = sanitize(&input, mime, dir.path(), extension, &ImageBudget::default())?;
        ensure!(
            result.final_mime == "image/webp" && result.was_converted,
            "strict retained source format"
        );
        let output = std::fs::read(&result.final_path)?;
        ensure!(
            !output.windows(16).any(|chunk| chunk == b"private-location"),
            "private ancillary data survived"
        );
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&output))?;
        ensure!(
            decoder.exif_metadata()?.is_none()
                && decoder.xmp_metadata()?.is_none()
                && decoder.icc_profile()?.is_none(),
            "metadata was copied to output"
        );
        ensure!(
            image::open(result.final_path)?.into_rgba8() == pixels,
            "strict output changed pixels"
        );
    }
    Ok(())
}

/// Orientation must be baked into pixels before removing camera metadata.
#[test]
fn strict_jpeg_keeps_orientation_and_removes_comments() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let pixels = image::RgbImage::from_fn(3, 2, |x, y| {
        image::Rgb([
            u8::try_from(x * 70).unwrap_or(0),
            u8::try_from(y * 150).unwrap_or(0),
            20,
        ])
    });
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&pixels)?;
    let expected = image::load_from_memory(&jpeg)?.rotate90().into_rgba8();
    let mut bytes = jpeg.get(..2).context("SOI")?.to_vec();
    bytes.extend_from_slice(b"\xff\xe1\x00\x22Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
    bytes.extend_from_slice(b"\xff\xfe\x00\x09private");
    bytes.extend_from_slice(jpeg.get(2..).context("JPEG body")?);
    let input = dir.path().join("oriented.jpg");
    std::fs::write(&input, bytes)?;
    let result = sanitize(
        &input,
        "image/jpeg",
        dir.path(),
        "rotated",
        &ImageBudget::default(),
    )?;
    ensure!(
        image::open(&result.final_path)?.into_rgba8() == expected,
        "JPEG orientation was lost or applied twice"
    );
    let output = std::fs::read(result.final_path)?;
    ensure!(
        !output.windows(7).any(|chunk| chunk == b"private"),
        "JPEG comment survived"
    );
    Ok(())
}

/// Later frame failures and output limits must reject instead of retaining input.
#[test]
fn strict_animation_preserves_frames_and_rejects_late_failure_atomically() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("animation.gif");
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &[255, 0, 0, 0, 0, 255])?;
        encoder.set_repeat(gif::Repeat::Finite(2))?;
        for (colors, delay) in [([0, 1], 7), ([1, 0], 11)] {
            encoder.write_frame(&gif::Frame {
                width: 2,
                height: 1,
                delay,
                buffer: std::borrow::Cow::Borrowed(&colors),
                ..gif::Frame::default()
            })?;
        }
    }
    std::fs::write(&input, &bytes)?;
    let result = sanitize(
        &input,
        "image/gif",
        dir.path(),
        "animation",
        &ImageBudget::default(),
    )?;
    let source = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))?
        .into_frames()
        .collect_frames()?;
    let decoder = image::codecs::webp::WebPDecoder::new(std::io::BufReader::new(
        std::fs::File::open(&result.final_path)?,
    ))?;
    ensure!(
        matches!(decoder.loop_count(), image::metadata::LoopCount::Finite(count) if count.get() == 3),
        "animation loop count changed"
    );
    let frames = decoder.into_frames().collect_frames()?;
    ensure!(
        frames.len() == 2
            && source
                .iter()
                .zip(&frames)
                .all(|(a, b)| a.buffer() == b.buffer() && a.delay() == b.delay()),
        "animation pixels or timing changed"
    );
    // Re-encoding an already animated WebP must keep its animation too.
    let webp = sanitize(
        &result.final_path,
        "image/webp",
        dir.path(),
        "reencoded",
        &ImageBudget::default(),
    )?;
    let roundtrip_frames = image::codecs::webp::WebPDecoder::new(std::io::BufReader::new(
        std::fs::File::open(webp.final_path)?,
    ))?
    .into_frames()
    .collect_frames()?;
    ensure!(roundtrip_frames.len() == 2, "WebP was flattened");
    std::fs::write(dir.path().join("failed.webp"), b"previous")?;
    bytes.truncate(bytes.len().saturating_sub(5));
    std::fs::write(&input, &bytes)?;
    let before = std::fs::read_dir(dir.path())?.count();
    let error = sanitize(
        &input,
        "image/gif",
        dir.path(),
        "failed",
        &ImageBudget::default(),
    )
    .err()
    .context("late corrupt frame accepted")?;
    ensure!(
        error
            .downcast_ref::<crate::utils::files::UploadRejection>()
            .is_some(),
        "failure lost client-safe category"
    );
    ensure!(
        std::fs::read(dir.path().join("failed.webp"))? == b"previous"
            && std::fs::read_dir(dir.path())?.count() == before,
        "failed conversion replaced data or leaked staging"
    );
    let limited = sanitize(
        &result.final_path,
        "image/webp",
        dir.path(),
        "limited",
        &ImageBudget::new(1),
    );
    ensure!(
        limited.is_err() && !dir.path().join("limited.webp").exists(),
        "resource failure retained original bytes"
    );
    Ok(())
}

/// Unsupported classes must fail, including animation the old PNG path flattens.
#[test]
fn strict_rejects_unsupported_formats_and_apng() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("source.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255])).save(&input)?;
    for mime in [
        "audio/mpeg",
        "audio/ogg",
        "audio/flac",
        "audio/wav",
        "audio/mp4",
        "audio/aac",
        "video/mp4",
        "video/webm",
        "video/x-matroska",
        "application/pdf",
        "image/svg+xml",
        "application/octet-stream",
        "image/heic",
        "image/heif",
        "image/tiff",
    ] {
        ensure!(
            sanitize(
                &input,
                mime,
                dir.path(),
                "rejected",
                &ImageBudget::default()
            )
            .is_err(),
            "unsupported type was published"
        );
    }
    let mut bytes = std::fs::read(&input)?;
    drop(bytes.splice(33..33, png_chunk(*b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0])));
    std::fs::write(&input, bytes)?;
    let error = super::validate_format(&input, "image/png")
        .err()
        .context("APNG was flattened")?;
    ensure!(
        error.to_string().contains("APNG"),
        "APNG rejection lacks clear advice"
    );
    ensure!(
        !dir.path().join("rejected.webp").exists(),
        "unsupported input created output"
    );
    Ok(())
}

/// Build small real ancillary PNG chunks without adding a test dependency.
fn png_chunk(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut crc = flate2::Crc::new();
    crc.update(&kind);
    crc.update(payload);
    let length = u32::try_from(payload.len()).unwrap_or(0);
    [
        length.to_be_bytes().as_slice(),
        &kind,
        payload,
        crc.sum().to_be_bytes().as_slice(),
    ]
    .concat()
}
