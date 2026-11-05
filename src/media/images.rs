//! Image codecs shared by uploads, thumbnails and animated banners.
//!
//! Pixel codecs come from `image`. Animation framing follows the WebP RIFF
//! specification; full composited frames overwrite the canvas, preserving GIF
//! disposal and transparency without implementing another pixel codec.

use super::image_budget::{BudgetWriter, ImageBudget, ImageFailure};
use anyhow::{ensure, Context as _, Result};
use image::codecs::{gif::GifDecoder, webp::WebPDecoder};
use image::{AnimationDecoder as _, DynamicImage, ImageDecoder as _, ImageFormat, RgbaImage};
use std::fs::File;
use std::io::{BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// Upper bound on animation work; frames are processed one at a time.
const MAX_ANIMATION_FRAMES: usize = 10_000;
/// Aggregate decoded canvas work, independent of frame delay or output size.
const MAX_ANIMATION_PIXELS: u64 = super::MAX_UNTRUSTED_IMAGE_PIXELS * 10;
/// Largest integer represented by WebP's three-byte fields.
const MAX_U24: u32 = 0x00ff_ffff;

/// Reject zero-sized or excessively large decoded canvases before allocation.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(
        width > 0 && height > 0,
        ImageFailure::Malformed("image has an empty canvas")
    );
    ensure!(
        u64::from(width)
            .checked_mul(u64::from(height))
            .context("image area overflow")?
            <= super::MAX_UNTRUSTED_IMAGE_PIXELS,
        ImageFailure::Resource("image dimensions exceed the safety limit")
    );
    Ok(())
}

/// Decode a still image (or an animation's first composited frame) with limits.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn decode_still(input: &Path, budget: &ImageBudget) -> Result<DynamicImage> {
    budget.check()?;
    if is_heif(input)? {
        let pixels = super::heif::decode(input)?;
        budget.check()?;
        return Ok(pixels);
    }
    let file = BufReader::new(File::open(input).context("open image")?);
    let mut reader = image::ImageReader::new(file).with_guessed_format()?;
    reader.limits(super::untrusted_image_decode_limits());
    let mut decoder = reader.into_decoder().context("read image header")?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    let orientation = decoder.orientation().context("read image orientation")?;
    let mut pixels = DynamicImage::from_decoder(decoder).context("decode image")?;
    budget.check()?;
    pixels.apply_orientation(orientation);
    budget.check()?;
    Ok(pixels)
}

/// Recognize HEIF container brands from a bounded content prefix.
fn is_heif(input: &Path) -> Result<bool> {
    let mut prefix = [0; 64];
    let count = File::open(input)?.read(&mut prefix)?;
    let bytes = prefix.get(..count).context("invalid image prefix")?;
    Ok(bytes.get(4..8) == Some(b"ftyp")
        && bytes.get(8..).is_some_and(|brands| {
            brands.as_chunks::<4>().0.iter().any(|brand| {
                matches!(
                    brand,
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
                )
            })
        }))
}

/// Encode metadata-free WebP, preserving animation and optionally fitting a box.
///
/// Output is published atomically only after every frame has succeeded.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn image_to_webp(input: &Path, output: &Path, bounds: Option<(u32, u32)>) -> Result<()> {
    image_to_webp_with_budget(input, output, bounds, &ImageBudget::default())
}

/// Encode with the validation/conversion/thumbnail deadline and output allowance.
pub(super) fn image_to_webp_with_budget(
    input: &Path,
    output: &Path,
    bounds: Option<(u32, u32)>,
    budget: &ImageBudget,
) -> Result<()> {
    budget.check()?;
    if let Some((width, height)) = bounds {
        validate_dimensions(width, height)?;
    }
    let parent = output.parent().context("image output has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("create image output")?;
    let reader =
        image::ImageReader::new(BufReader::new(File::open(input)?)).with_guessed_format()?;
    let format = reader.format();
    match format {
        Some(ImageFormat::Gif) => {
            let loops = gif_loops(input, budget)?;
            let mut decoder = GifDecoder::new(BufReader::new(File::open(input)?))?;
            let canvas = decoder.dimensions();
            validate_dimensions_pair(canvas)?;
            decoder.set_limits(super::untrusted_image_decode_limits())?;
            encode_animation(
                decoder.into_frames(),
                loops,
                bounds,
                canvas,
                temporary.as_file_mut(),
                budget,
            )?;
        }
        Some(ImageFormat::WebP) => {
            let mut decoder = WebPDecoder::new(BufReader::new(File::open(input)?))?;
            let canvas = decoder.dimensions();
            validate_dimensions_pair(canvas)?;
            decoder.set_limits(super::untrusted_image_decode_limits())?;
            if decoder.has_animation() {
                let loops = match decoder.loop_count() {
                    image::metadata::LoopCount::Infinite => 0,
                    image::metadata::LoopCount::Finite(count) => u16::try_from(count.get())?,
                };
                encode_animation(
                    decoder.into_frames(),
                    loops,
                    bounds,
                    canvas,
                    temporary.as_file_mut(),
                    budget,
                )?;
            } else {
                write_still(input, bounds, temporary.as_file_mut(), budget)?;
            }
        }
        _ => write_still(input, bounds, temporary.as_file_mut(), budget)?,
    }
    budget.check()?;
    drop(temporary.persist(output).context("publish WebP image")?);
    Ok(())
}

/// Checks a dimension pair returned by a decoder.
fn validate_dimensions_pair((width, height): (u32, u32)) -> Result<()> {
    validate_dimensions(width, height)
}

/// Encode pixels only, stripping EXIF, ICC, XMP and ancillary metadata.
fn write_still(
    input: &Path,
    bounds: Option<(u32, u32)>,
    output: &mut File,
    budget: &ImageBudget,
) -> Result<()> {
    let mut decoded = decode_still(input, budget)?;
    if let Some(bounds) = bounds {
        decoded = resize(decoded, bounds)?;
    }
    write_pixels(&decoded, output, budget)
}

/// Encode pixels atomically at callers, with a byte cap on the output writer.
pub(super) fn write_pixels(
    pixels: &DynamicImage,
    output: &mut (impl std::io::Write + std::io::Seek),
    budget: &ImageBudget,
) -> Result<()> {
    budget.check()?;
    let writer = BudgetWriter::new(output, budget.output_limit, budget);
    pixels
        .write_to(writer, ImageFormat::WebP)
        .context("encode WebP")?;
    budget.check()?;
    Ok(())
}

/// Filter associated colors at 16-bit precision, retaining the existing Triangle
/// filter and encoded color space. ICC conversion and linear-light filtering are
/// separate policies; neither is silently introduced here.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "u16 color/alpha products plus rounding fit in u32; division guards zero alpha"
)]
pub(super) fn resize(pixels: DynamicImage, (width, height): (u32, u32)) -> Result<DynamicImage> {
    validate_dimensions(width, height)?;
    let (width, height) = fit_dimensions(pixels.width(), pixels.height(), width, height)?;
    if pixels.width() == width && pixels.height() == height {
        return Ok(pixels);
    }
    let opaque_rgba = pixels
        .as_rgba8()
        .is_some_and(|rgba| rgba.pixels().all(|pixel| matches!(pixel.0, [_, _, _, 255])));
    if !pixels.color().has_alpha() || opaque_rgba {
        return Ok(pixels.resize(width, height, image::imageops::FilterType::Triangle));
    }
    let mut associated = pixels.into_rgba16();
    for pixel in associated.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        let multiply = |color: u16| -> Result<u16> {
            Ok(u16::try_from(
                (u32::from(color) * u32::from(alpha) + 32767) / 65535,
            )?)
        };
        pixel.0 = [multiply(red)?, multiply(green)?, multiply(blue)?, alpha];
    }
    let mut filtered = image::imageops::resize(
        &associated,
        width,
        height,
        image::imageops::FilterType::Triangle,
    );
    drop(associated);
    for pixel in filtered.pixels_mut() {
        let [red, green, blue, alpha] = pixel.0;
        let divide = |color: u16| -> Result<u16> {
            if alpha == 0 {
                return Ok(0);
            }
            let value = (u32::from(color) * 65535 + u32::from(alpha) / 2) / u32::from(alpha);
            Ok(u16::try_from(value.min(65535))?)
        };
        pixel.0 = [divide(red)?, divide(green)?, divide(blue)?, alpha];
    }
    // WebP is already an 8-bit output. Preserve high precision through filtering.
    Ok(DynamicImage::ImageRgba16(filtered).into_rgba8().into())
}

/// Describe failures without changing the established valid-original fallback.
pub(super) fn failure_kind(failure: &anyhow::Error) -> &'static str {
    // Look through codec wrappers first: a GIF allocation failure can otherwise
    // appear as an ordinary ImageError::Decoding and be labelled malformed.
    for cause in failure.chain() {
        if let Some(explicit) = cause.downcast_ref::<ImageFailure>() {
            return match explicit {
                ImageFailure::Resource(_) => "resource",
                ImageFailure::Unsupported(_) => "unsupported",
                ImageFailure::Malformed(_) => "malformed",
            };
        }
        if let Some(gif_error) = cause.downcast_ref::<gif::DecodingError>() {
            return match gif_error {
                gif::DecodingError::Format(_)
                | gif::DecodingError::LzwError(_)
                | gif::DecodingError::EndCodeNotFound
                | gif::DecodingError::UnexpectedEof => "malformed",
                gif::DecodingError::MemoryLimit | gif::DecodingError::OutOfMemory => "resource",
                gif::DecodingError::DecoderNotFound => "internal",
                gif::DecodingError::Io(io_error) => io_failure_kind(io_error),
                _ => "other",
            };
        }
    }
    for cause in failure.chain() {
        if let Some(image_error) = cause.downcast_ref::<image::ImageError>() {
            return match image_error {
                image::ImageError::Unsupported(_) => "unsupported",
                image::ImageError::Limits(_) => "resource",
                image::ImageError::Decoding(_) | image::ImageError::Parameter(_) => "malformed",
                image::ImageError::IoError(io_error) => io_failure_kind(io_error),
                image::ImageError::Encoding(_) => "encoding",
            };
        }
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            return io_failure_kind(io_error);
        }
    }
    "other"
}

/// Writer-budget errors retain their resource classification through codecs.
fn io_failure_kind(error: &std::io::Error) -> &'static str {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ImageFailure>())
        .map_or_else(
            || {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
                ) {
                    "malformed"
                } else {
                    "io"
                }
            },
            |failure| match failure {
                ImageFailure::Resource(_) => "resource",
                ImageFailure::Unsupported(_) => "unsupported",
                ImageFailure::Malformed(_) => "malformed",
            },
        )
}

/// Inspect GIF repetition metadata even when it follows the first frame.
fn gif_loops(input: &Path, budget: &ImageBudget) -> Result<u16> {
    let mut options = gif::DecodeOptions::new();
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        std::num::NonZeroU64::new(super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES)
            .context("image allocation budget is zero")?,
    ));
    let mut decoder = options.read_info(BufReader::new(File::open(input)?))?;
    validate_dimensions(u32::from(decoder.width()), u32::from(decoder.height()))?;
    let mut count = 0_usize;
    loop {
        budget.check()?;
        if decoder.next_frame_info()?.is_none() {
            break;
        }
        count = count.checked_add(1).context("GIF frame count overflow")?;
        ensure!(
            count <= MAX_ANIMATION_FRAMES,
            ImageFailure::Resource("animation exceeds frame safety limit")
        );
    }
    budget.check()?;
    ensure!(count > 0, ImageFailure::Malformed("GIF contains no frames"));
    match decoder.repeat() {
        gif::Repeat::Infinite => Ok(0),
        // GIF counts repeats after the first play; WebP counts total plays.
        gif::Repeat::Finite(repeats) => repeats.checked_add(1).ok_or_else(|| {
            ImageFailure::Unsupported("GIF loop count cannot be represented by WebP").into()
        }),
    }
}

/// Stream composited frames into ANMF chunks, retaining at most one frame.
fn encode_animation(
    frames: image::Frames<'_>,
    loops: u16,
    bounds: Option<(u32, u32)>,
    canvas: (u32, u32),
    output_file: &mut File,
    budget: &ImageBudget,
) -> Result<()> {
    let mut limited = BudgetWriter::new(output_file, budget.output_limit, budget);
    let output = &mut limited;
    let mut dimensions = None;
    let mut work = 0_u64;
    let mut frames = frames.enumerate();
    loop {
        budget.check()?;
        // Charge the decoder's composited canvas before requesting another frame.
        // Includes one conservative EOF probe; no unbounded frame is decoded.
        work = work
            .checked_add(u64::from(canvas.0).saturating_mul(u64::from(canvas.1)))
            .context("animation work overflow")?;
        ensure!(
            work <= MAX_ANIMATION_PIXELS,
            ImageFailure::Resource("animation exceeds decoded work budget")
        );
        let Some((index, frame)) = frames.next() else {
            break;
        };
        ensure!(
            index < MAX_ANIMATION_FRAMES,
            ImageFailure::Resource("animation exceeds frame safety limit")
        );
        budget.check()?;
        let frame = frame.context("decode animation frame")?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let duration = numerator
            .checked_div(denominator)
            .ok_or(ImageFailure::Malformed("invalid frame delay"))?;
        let mut pixels = frame.into_buffer();
        validate_dimensions_pair(pixels.dimensions())?;
        if let Some((width, height)) = bounds {
            pixels = resize(DynamicImage::ImageRgba8(pixels), (width, height))?.into_rgba8();
        }
        let frame_dimensions = pixels.dimensions();
        if let Some(expected) = dimensions {
            ensure!(
                frame_dimensions == expected,
                ImageFailure::Malformed("animation canvas changes size")
            );
        } else {
            write_animation_header(output, frame_dimensions, loops)?;
            dimensions = Some(frame_dimensions);
        }
        budget.check()?;
        write_animation_frame(output, &pixels, duration, budget)?;
        budget.check()?;
    }
    budget.check()?;
    ensure!(
        dimensions.is_some(),
        ImageFailure::Malformed("animation contains no frames")
    );
    let end = output.stream_position()?;
    let riff_size = u32::try_from(end.checked_sub(8).context("invalid RIFF size")?)?;
    let _position = output.seek(SeekFrom::Start(4))?;
    output.write_all(&riff_size.to_le_bytes())?;
    Ok(())
}

/// Write RIFF, VP8X and ANIM; canvas alpha and independent frames are supported.
fn write_animation_header(
    output: &mut impl std::io::Write,
    (width, height): (u32, u32),
    loops: u16,
) -> Result<()> {
    validate_dimensions(width, height)?;
    output.write_all(b"RIFF\0\0\0\0WEBPVP8X")?;
    output.write_all(&10_u32.to_le_bytes())?;
    output.write_all(&[0x12, 0, 0, 0])?;
    write_u24(
        output,
        width.checked_sub(1).context("WebP canvas width is empty")?,
    )?;
    write_u24(
        output,
        height
            .checked_sub(1)
            .context("WebP canvas height is empty")?,
    )?;
    output.write_all(b"ANIM")?;
    output.write_all(&6_u32.to_le_bytes())?;
    output.write_all(&[0; 4])?;
    output.write_all(&loops.to_le_bytes())?;
    Ok(())
}

/// Write one lossless, fully composited frame without blending it again.
fn write_animation_frame(
    output: &mut (impl std::io::Write + std::io::Seek),
    pixels: &RgbaImage,
    duration: u32,
    budget: &ImageBudget,
) -> Result<()> {
    ensure!(
        duration <= MAX_U24,
        ImageFailure::Unsupported("frame duration exceeds WebP limits")
    );
    // Avoid an application-owned encoded Vec; image-webp still buffers internally.
    let remaining = budget
        .output_limit
        .checked_sub(output.stream_position()?)
        .and_then(|bytes| bytes.checked_sub(24))
        .ok_or(ImageFailure::Resource(
            "animation output exceeds safety budget",
        ))?;
    let mut encoded = tempfile::tempfile().context("create animation frame spool")?;
    image::codecs::webp::WebPEncoder::new_lossless(BudgetWriter::new(
        &mut encoded,
        remaining,
        budget,
    ))
    .encode(
        pixels.as_raw(),
        pixels.width(),
        pixels.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    let payload_size = u32::try_from(
        encoded
            .stream_position()?
            .checked_sub(12)
            .context("invalid encoded frame")?,
    )?;
    let end = output
        .stream_position()?
        .checked_add(u64::from(payload_size) + 24)
        .context("animation output size overflow")?;
    ensure!(
        end <= budget.output_limit,
        ImageFailure::Resource("animation output exceeds safety budget")
    );
    output.write_all(b"ANMF")?;
    output.write_all(
        &payload_size
            .checked_add(16)
            .context("animation frame overflow")?
            .to_le_bytes(),
    )?;
    output.write_all(&[0; 6])?;
    write_u24(
        output,
        pixels
            .width()
            .checked_sub(1)
            .context("WebP frame width is empty")?,
    )?;
    write_u24(
        output,
        pixels
            .height()
            .checked_sub(1)
            .context("WebP frame height is empty")?,
    )?;
    write_u24(output, duration)?;
    output.write_all(&[2])?;
    let _position = encoded.seek(SeekFrom::Start(12))?;
    let _bytes_copied = std::io::copy(&mut encoded.take(u64::from(payload_size)), output)?;
    Ok(())
}

/// Serialize a checked little-endian three-byte WebP field.
fn write_u24(output: &mut impl std::io::Write, value: u32) -> Result<()> {
    ensure!(value <= MAX_U24, "WebP field exceeds 24 bits");
    let bytes = value.to_le_bytes();
    output.write_all(bytes.get(..3).context("invalid three-byte field")?)?;
    Ok(())
}

/// Match aspect-preserving, nearest-integer sizing without floating-point casts.
fn fit_dimensions(
    width: u32,
    height: u32,
    bound_width: u32,
    bound_height: u32,
) -> Result<(u32, u32)> {
    validate_dimensions(width, height)?;
    let scale = |side: u32, bound: u32, denominator: u32| -> Result<u32> {
        let value = u64::from(side)
            .checked_mul(u64::from(bound))
            .and_then(|product| product.checked_add(u64::from(denominator) / 2))
            .and_then(|rounded| rounded.checked_div(u64::from(denominator)))
            .context("invalid resize ratio")?;
        Ok(u32::try_from(value.max(1))?)
    };
    if u64::from(width).saturating_mul(u64::from(bound_height))
        > u64::from(height).saturating_mul(u64::from(bound_width))
    {
        Ok((bound_width, scale(height, bound_width, width)?))
    } else {
        Ok((scale(width, bound_height, height)?, bound_height))
    }
}

#[cfg(test)]
#[path = "tests/images.rs"]
mod tests;
