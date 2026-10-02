//! Semantic fixtures for the image pipeline, independent of external tools.

use super::*;
use anyhow::ensure;
use image::{Delay, Frame, Rgba};
use std::borrow::Cow;

/// Generate indexed partial-frame GIF updates with all three disposal behaviors.
fn gif_fixture(path: &Path, repeat: gif::Repeat) -> Result<()> {
    let palette = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let mut encoder = gif::Encoder::new(File::create(path)?, 4, 3, &palette)?;
    encoder.set_repeat(repeat)?;
    for (width, height, left, top, delay, dispose, buffer) in [
        (4, 3, 0, 0, 3, gif::DisposalMethod::Keep, vec![1; 12]),
        (2, 1, 1, 1, 11, gif::DisposalMethod::Previous, vec![2, 0]),
        (1, 1, 0, 0, 23, gif::DisposalMethod::Background, vec![3]),
        (1, 1, 3, 2, 7, gif::DisposalMethod::Keep, vec![2]),
    ] {
        encoder.write_frame(&gif::Frame {
            width,
            height,
            left,
            top,
            delay,
            dispose,
            transparent: Some(0),
            buffer: Cow::Owned(buffer),
            ..Default::default()
        })?;
    }
    encoder.into_inner()?;
    Ok(())
}

#[test]
fn gif_webp_preserves_partial_updates_disposal_delays_and_loops() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("fixture.gif");
    let output = dir.path().join("animation.webp");
    for (repeat, expected_loops) in [
        (gif::Repeat::Infinite, 0),
        (gif::Repeat::Finite(3), 4),
        (gif::Repeat::Finite(u16::MAX - 1), u32::from(u16::MAX)),
    ] {
        gif_fixture(&input, repeat)?;
        let expected = GifDecoder::new(BufReader::new(File::open(&input)?))?
            .into_frames()
            .collect_frames()?;
        image_to_webp(&input, &output, None)?;
        let decoder = WebPDecoder::new(BufReader::new(File::open(&output)?))?;
        ensure!(decoder.has_animation(), "GIF output must remain animated");
        let loops = match decoder.loop_count() {
            image::metadata::LoopCount::Infinite => 0,
            image::metadata::LoopCount::Finite(count) => count.get(),
        };
        ensure!(loops == expected_loops, "loop count changed");
        let actual = decoder.into_frames().collect_frames()?;
        ensure!(actual.len() == 4, "frame count changed");
        for (actual, expected) in actual.iter().zip(&expected) {
            ensure!(actual.delay() == expected.delay(), "frame delay changed");
            ensure!(
                actual.buffer() == expected.buffer(),
                "composited pixels changed"
            );
        }
        let second = actual.get(1).context("missing second frame")?.buffer();
        ensure!(
            second.get_pixel(1, 1).0 == [0, 255, 0, 255],
            "partial update missing"
        );
        ensure!(
            second.get_pixel(2, 1).0 == [255, 0, 0, 255],
            "transparent overlay erased canvas"
        );
        let third = actual.get(2).context("missing third frame")?.buffer();
        ensure!(
            third.get_pixel(1, 1).0 == [255, 0, 0, 255],
            "restore previous failed"
        );
        let fourth = actual.get(3).context("missing fourth frame")?.buffer();
        ensure!(
            fourth.get_pixel(0, 0).0 == [0, 0, 0, 0],
            "background disposal failed"
        );
    }
    Ok(())
}

/// GIF counts repeats after the first play; WebP's u16 counts total plays.
#[test]
fn unrepresentable_gif_repeat_count_does_not_publish_a_changed_animation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("max-repeat.gif");
    let output = dir.path().join("animation.webp");
    gif_fixture(&input, gif::Repeat::Finite(u16::MAX))?;
    let error = image_to_webp(&input, &output, None)
        .err()
        .context("65536 total GIF plays were silently changed to 65535")?;
    ensure!(
        error.to_string().contains("loop"),
        "missing loop diagnostic"
    );
    ensure!(!output.exists(), "unrepresentable animation was published");
    Ok(())
}

/// Conversion failures must preserve validated MIME, independent of temporary names.
#[test]
fn valid_gif_conversion_fallback_preserves_mime_frames_and_original_bytes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for (index, name) in ["upload", "misleading.jpg", "input.GIF"].iter().enumerate() {
        let input = dir.path().join(name);
        gif_fixture(&input, gif::Repeat::Finite(u16::MAX))?;
        let original = std::fs::read(&input)?;
        let stem = format!("preserved-{index}");
        let result = crate::media::convert::convert_file(&input, "image/gif", dir.path(), &stem)?;
        ensure!(!result.was_converted, "unrepresentable GIF was changed");
        ensure!(result.final_mime == "image/gif", "validated MIME was lost");
        ensure!(
            result
                .final_path
                .extension()
                .is_some_and(|ext| ext == "gif"),
            "wrong fallback extension"
        );
        ensure!(
            std::fs::read(&result.final_path)? == original,
            "fallback changed GIF bytes"
        );
        let decoder = GifDecoder::new(BufReader::new(File::open(&result.final_path)?))?;
        ensure!(
            decoder.into_frames().collect_frames()?.len() == 4,
            "fallback lost animation frames"
        );
        ensure!(
            !dir.path().join(format!("{stem}.webp")).exists(),
            "fallback left partial WebP"
        );
    }
    Ok(())
}

#[test]
fn transparent_gif_and_existing_webp_scale_without_flattening() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("transparent.gif");
    let original = dir.path().join("original.webp");
    let scaled = dir.path().join("scaled.webp");
    let mut encoder = image::codecs::gif::GifEncoder::new(File::create(&input)?);
    encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
    for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
        let mut pixels = RgbaImage::from_pixel(8, 4, Rgba([0; 4]));
        pixels.put_pixel(3, 1, Rgba(color));
        encoder.encode_frame(Frame::from_parts(
            pixels,
            0,
            0,
            Delay::from_numer_denom_ms(90, 1),
        ))?;
    }
    drop(encoder);
    image_to_webp(&input, &original, None)?;
    image_to_webp(&original, &scaled, Some((4, 2)))?;
    let decoder = WebPDecoder::new(BufReader::new(File::open(&scaled)?))?;
    ensure!(decoder.has_animation(), "WebP scaling flattened animation");
    ensure!(decoder.dimensions() == (4, 2), "wrong scaled canvas");
    let frames = decoder.into_frames().collect_frames()?;
    ensure!(frames.len() == 2, "scaled frame count changed");
    ensure!(
        frames
            .first()
            .context("first frame")?
            .buffer()
            .get_pixel(0, 0)
            .0
            == [0; 4],
        "alpha lost"
    );
    ensure!(
        decode_still(&original)?.width() == 8,
        "stored WebP no longer decodes"
    );
    Ok(())
}

#[test]
fn static_formats_encode_to_decodable_metadata_free_webp() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let pixels = RgbaImage::from_fn(12, 6, |x, y| {
        Rgba([
            u8::try_from(x * 17).unwrap_or(0),
            u8::try_from(y * 31).unwrap_or(0),
            90,
            if x < 2 { 0 } else { 255 },
        ])
    });
    for (format, ext) in [
        (ImageFormat::Jpeg, "jpg"),
        (ImageFormat::Png, "png"),
        (ImageFormat::Bmp, "bmp"),
        (ImageFormat::Tiff, "tiff"),
        (ImageFormat::WebP, "webp"),
    ] {
        let input = dir.path().join(format!("input.{ext}"));
        let output = dir.path().join(format!("output-{ext}.webp"));
        let image = if format == ImageFormat::Jpeg {
            DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(pixels.clone()).into_rgb8())
        } else {
            DynamicImage::ImageRgba8(pixels.clone())
        };
        image.save_with_format(&input, format)?;
        image_to_webp(&input, &output, None)?;
        let expected = decode_still(&input)?.into_rgba8();
        let actual = decode_still(&output)?.into_rgba8();
        let thumbnail = dir.path().join(format!("thumb-{ext}.webp"));
        super::super::thumbnail::generate_thumbnail(
            &input,
            &format!("image/{ext}"),
            &thumbnail,
            6,
            false,
        )?;
        ensure!(
            image::open(thumbnail)?.into_rgba8().dimensions() == (6, 3),
            "{ext} thumbnail dimensions changed"
        );
        ensure!(
            actual == expected,
            "{ext} pixels changed during lossless conversion"
        );
        let mut decoder = WebPDecoder::new(BufReader::new(File::open(&output)?))?;
        ensure!(
            decoder.exif_metadata()?.is_none(),
            "EXIF survived conversion"
        );
        ensure!(decoder.icc_profile()?.is_none(), "ICC survived conversion");
        ensure!(decoder.xmp_metadata()?.is_none(), "XMP survived conversion");
    }
    Ok(())
}

#[test]
fn jpeg_exif_rotation_survives_conversion_and_thumbnailing_without_tools() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("phone.jpg");
    let output = dir.path().join("phone.webp");
    DynamicImage::ImageRgb8(image::RgbImage::from_fn(12, 6, |x, y| {
        image::Rgb([
            u8::try_from(x * 17).unwrap_or(0),
            u8::try_from(y * 31).unwrap_or(0),
            90,
        ])
    }))
    .save_with_format(&input, ImageFormat::Jpeg)?;
    let expected = decode_still(&input)?.rotate90().into_rgba8();
    let original = std::fs::read(&input)?;
    // EXIF orientation 6: little-endian TIFF with one SHORT orientation entry.
    let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    let mut tagged = original.get(..2).context("JPEG start marker")?.to_vec();
    tagged.extend_from_slice(&[0xff, 0xe1]);
    tagged.extend_from_slice(&u16::try_from(exif.len() + 2)?.to_be_bytes());
    tagged.extend_from_slice(exif);
    tagged.extend_from_slice(original.get(2..).context("JPEG body")?);
    std::fs::write(&input, tagged)?;
    image_to_webp(&input, &output, None)?;
    ensure!(
        decode_still(&output)?.into_rgba8() == expected,
        "JPEG EXIF rotation was lost"
    );
    let thumbnail = dir.path().join("thumb.webp");
    super::super::thumbnail::generate_thumbnail(&input, "image/jpeg", &thumbnail, 6, false)?;
    ensure!(
        image::open(thumbnail)?.into_rgba8().dimensions() == (3, 6),
        "oriented thumbnail dimensions changed"
    );
    Ok(())
}

#[test]
fn malformed_or_over_limit_images_do_not_publish_partial_output() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("malformed.gif");
    let output = dir.path().join("image.webp");
    std::fs::write(&input, b"GIF89a\xff\xff\xff\xff\0\0\0")?;
    std::fs::write(&output, b"existing output")?;
    ensure!(
        image_to_webp(&input, &output, None).is_err(),
        "hostile canvas accepted"
    );
    ensure!(
        std::fs::read(&output)? == b"existing output",
        "partial output replaced existing file"
    );
    ensure!(validate_dimensions(0, 1).is_err(), "empty canvas accepted");
    ensure!(
        validate_dimensions(8_000, 8_000).is_err(),
        "pixel limit ignored"
    );
    Ok(())
}

#[test]
fn large_valid_image_and_truncated_formats_preserve_atomic_output() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let input = dir.path().join("misleading.mp4");
    let output = dir.path().join("thumb.webp");
    let pixels =
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(4096, 2048, Rgba([42, 73, 101, 128])));
    pixels.save_with_format(&input, ImageFormat::Png)?;
    super::super::thumbnail::generate_thumbnail(&input, "image/png", &output, 256, false)?;
    let image = decode_still(&output)?.into_rgba8();
    ensure!(
        image.dimensions() == (256, 128),
        "large thumbnail aspect changed"
    );
    ensure!(
        image.get_pixel(100, 50).0[3] == 128,
        "large thumbnail alpha lost"
    );
    for format in [
        ImageFormat::Jpeg,
        ImageFormat::Png,
        ImageFormat::Bmp,
        ImageFormat::Tiff,
        ImageFormat::WebP,
    ] {
        let image = DynamicImage::ImageRgb8(image::RgbImage::new(32, 16));
        image.save_with_format(&input, format)?;
        let bytes = std::fs::read(&input)?;
        std::fs::write(&input, bytes.get(..12).context("fixture too short")?)?;
        std::fs::write(&output, b"previous thumbnail")?;
        ensure!(
            image_to_webp(&input, &output, None).is_err(),
            "truncated {format:?} accepted"
        );
        ensure!(
            std::fs::read(&output)? == b"previous thumbnail",
            "truncated image replaced output"
        );
    }
    Ok(())
}

#[test]
fn animation_output_budget_fails_before_writing_the_next_frame() -> Result<()> {
    let mut output = tempfile::tempfile()?;
    let length = super::super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES - 1;
    output.set_len(length)?;
    output.seek(SeekFrom::End(0))?;
    let pixels = RgbaImage::from_pixel(4, 3, Rgba([1, 2, 3, 255]));
    ensure!(
        write_animation_frame(&mut output, &pixels, 100).is_err(),
        "animation output exceeded budget"
    );
    ensure!(
        output.metadata()?.len() == length,
        "oversized frame was partially written"
    );
    Ok(())
}
