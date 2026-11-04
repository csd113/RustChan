//! Regressions for transparent edges, animation fallback, and atomic output.

use super::{ImageBudget, ImageFailure};
use anyhow::{ensure, Result};
use image::{AnimationDecoder as _, DynamicImage, Rgba, RgbaImage};

/// Invisible RGB must not tint visible edges, including very faint pixels.
#[test]
fn resize_associates_alpha_and_preserves_aspect() -> Result<()> {
    for alpha in [0, 1, 128, 255] {
        let image = RgbaImage::from_fn(2, 1, |x, _| {
            if x == 0 {
                Rgba([128, 64, 32, alpha])
            } else {
                Rgba([0, 255, 255, 0])
            }
        });
        let result = super::resize(DynamicImage::ImageRgba8(image), (1, 1))?.into_rgba8();
        let pixel = result
            .pixels()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty resize"))?;
        if alpha == 0 {
            ensure!(
                pixel.0 == [0; 4],
                "fully transparent result retained hidden colors"
            );
        } else {
            let [red, green, blue, _alpha] = pixel.0;
            ensure!(
                red.abs_diff(128) <= 1 && green.abs_diff(64) <= 1 && blue.abs_diff(32) <= 1,
                "transparent edge contaminated: {pixel:?}"
            );
        }
    }
    let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 4, Rgba([10, 20, 30, 128])));
    ensure!(
        super::resize(image, (3, 3))?.into_rgba8().dimensions() == (3, 2),
        "aspect ratio changed"
    );
    Ok(())
}

/// Failed encodes leave existing outputs and surrounding directories untouched.
#[test]
fn image_output_cap_is_atomic_and_classified() -> Result<()> {
    let wrapped = image::ImageError::Decoding(image::error::DecodingError::new(
        image::ImageFormat::Gif.into(),
        gif::DecodingError::MemoryLimit,
    ));
    ensure!(
        super::failure_kind(&wrapped.into()) == "resource",
        "wrapped GIF limit misclassified as corruption"
    );
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("input.png");
    let output = directory.path().join("output.webp");
    RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])).save(&input)?;
    std::fs::write(&output, b"existing")?;
    let budget = ImageBudget::new(1);
    let error = super::image_to_webp_with_budget(&input, &output, None, &budget)
        .err()
        .ok_or_else(|| anyhow::anyhow!("output cap was ignored"))?;
    ensure!(
        super::failure_kind(&error) == "resource",
        "output failure misclassified: {error:#}"
    );
    ensure!(
        std::fs::read(&output)? == b"existing",
        "failed encode replaced prior data"
    );
    ensure!(
        std::fs::read_dir(directory.path())?.count() == 2,
        "temporary output leaked"
    );
    Ok(())
}

/// Unrepresentable loops keep valid GIF bytes; corrupt later frames stay a
/// classified conversion failure under the established original fallback.
#[test]
fn animation_fallback_and_composited_frames() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let input = directory.path().join("input.gif");
    let output = directory.path().join("output.webp");
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &[255, 0, 0, 0, 0, 255])?;
        encoder.set_repeat(gif::Repeat::Finite(u16::MAX))?;
        let frame = gif::Frame {
            width: 2,
            height: 1,
            buffer: std::borrow::Cow::Borrowed(&[0, 1]),
            delay: 7,
            ..gif::Frame::default()
        };
        encoder.write_frame(&frame)?;
    }
    std::fs::write(&input, &bytes)?;
    let error = super::image_to_webp(&input, &output, None)
        .err()
        .ok_or_else(|| anyhow::anyhow!("unrepresentable loop count converted"))?;
    ensure!(
        super::failure_kind(&error) == "unsupported",
        "loop failure misclassified"
    );
    let result =
        crate::media::convert::convert_file(&input, "image/gif", directory.path(), "fallback")?;
    ensure!(
        !result.was_converted && std::fs::read(result.final_path)? == bytes,
        "valid GIF fallback lost"
    );
    // Two independently composited animation frames with a transparent second
    // frame and background disposal; decode both sides through the shared codec.
    bytes.clear();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &[255, 0, 0, 0, 0, 255])?;
        encoder.set_repeat(gif::Repeat::Infinite)?;
        let first = gif::Frame {
            width: 2,
            height: 1,
            buffer: std::borrow::Cow::Borrowed(&[0, 0]),
            delay: 7,
            dispose: gif::DisposalMethod::Background,
            ..gif::Frame::default()
        };
        encoder.write_frame(&first)?;
        let second = gif::Frame {
            width: 2,
            height: 1,
            buffer: std::borrow::Cow::Borrowed(&[1, 0]),
            delay: 11,
            transparent: Some(0),
            ..gif::Frame::default()
        };
        encoder.write_frame(&second)?;
    }
    std::fs::write(&input, &bytes)?;
    super::image_to_webp(&input, &output, None)?;
    let source = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))?
        .into_frames()
        .collect_frames()?;
    let decoded = image::codecs::webp::WebPDecoder::new(std::io::BufReader::new(
        std::fs::File::open(&output)?,
    ))?
    .into_frames()
    .collect_frames()?;
    ensure!(
        source.len() == 2 && decoded.len() == 2,
        "animation frames lost"
    );
    for (source_frame, decoded_frame) in source.iter().zip(&decoded) {
        ensure!(
            source_frame.buffer() == decoded_frame.buffer()
                && source_frame.delay() == decoded_frame.delay(),
            "animation disposal, alpha, or timing changed"
        );
    }
    // Truncate the last image data, after the first frame remains valid.
    bytes.truncate(bytes.len().saturating_sub(5));
    std::fs::write(&input, &bytes)?;
    let corrupt_error = super::image_to_webp(&input, &output, None)
        .err()
        .ok_or_else(|| anyhow::anyhow!("late corruption was accepted"))?;
    ensure!(
        super::failure_kind(&corrupt_error) == "malformed",
        "late corruption misclassified: {corrupt_error:#}"
    );
    let corrupt_result =
        crate::media::convert::convert_file(&input, "image/gif", directory.path(), "corrupt")?;
    ensure!(
        !corrupt_result.was_converted && std::fs::read(corrupt_result.final_path)? == bytes,
        "late-corruption fallback contract changed"
    );
    ensure!(
        super::failure_kind(&anyhow::Error::new(ImageFailure::Resource("limit"))) == "resource",
        "resource classification lost"
    );
    Ok(())
}
