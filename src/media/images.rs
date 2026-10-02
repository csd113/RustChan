//! Image codecs shared by uploads, thumbnails and animated banners.
//!
//! Pixel codecs come from `image`. Animation framing follows the WebP RIFF
//! specification; full composited frames overwrite the canvas, preserving GIF
//! disposal and transparency without implementing another pixel codec.

use anyhow::{ensure, Context as _, Result};
use image::codecs::{gif::GifDecoder, webp::WebPDecoder};
use image::{AnimationDecoder as _, DynamicImage, ImageDecoder as _, ImageFormat, RgbaImage};
use std::fs::File;
use std::io::{BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

/// Upper bound on animation work; frames are processed one at a time.
const MAX_ANIMATION_FRAMES: usize = 10_000;
/// Largest integer represented by WebP's three-byte fields.
const MAX_U24: u32 = 0x00ff_ffff;

/// Reject zero-sized or excessively large decoded canvases before allocation.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(width > 0 && height > 0, "image has an empty canvas");
    ensure!(
        u64::from(width) * u64::from(height) <= super::MAX_UNTRUSTED_IMAGE_PIXELS,
        "image dimensions {width}x{height} exceed the safety limit"
    );
    Ok(())
}

/// Decode a still image (or an animation's first composited frame) with limits.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn decode_still(input: &Path) -> Result<DynamicImage> {
    if is_heif(input)? {
        return super::heif::decode(input);
    }
    let file = BufReader::new(File::open(input).context("open image")?);
    let mut reader = image::ImageReader::new(file).with_guessed_format()?;
    reader.limits(super::untrusted_image_decode_limits());
    let mut decoder = reader.into_decoder().context("read image header")?;
    let (width, height) = decoder.dimensions();
    validate_dimensions(width, height)?;
    let orientation = decoder.orientation().context("read image orientation")?;
    let mut pixels = DynamicImage::from_decoder(decoder).context("decode image")?;
    pixels.apply_orientation(orientation);
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
    if let Some((width, height)) = bounds {
        validate_dimensions(width, height)?;
    }
    let parent = output.parent().context("image output has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("create image output")?;
    let reader =
        image::ImageReader::new(BufReader::new(File::open(input)?)).with_guessed_format()?;
    let format = reader.format();
    let budget = AnimationBudget::new();
    match format {
        Some(ImageFormat::Gif) => {
            let loops = gif_loops(input, &budget)?;
            let mut decoder = GifDecoder::new(BufReader::new(File::open(input)?))?;
            validate_dimensions_pair(decoder.dimensions())?;
            decoder.set_limits(super::untrusted_image_decode_limits())?;
            encode_animation(
                decoder.into_frames(),
                loops,
                bounds,
                temporary.as_file_mut(),
                &budget,
            )?;
        }
        Some(ImageFormat::WebP) => {
            let mut decoder = WebPDecoder::new(BufReader::new(File::open(input)?))?;
            validate_dimensions_pair(decoder.dimensions())?;
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
                    temporary.as_file_mut(),
                    &budget,
                )?;
            } else {
                write_still(input, bounds, temporary.as_file_mut())?;
            }
        }
        _ => write_still(input, bounds, temporary.as_file_mut())?,
    }
    temporary.persist(output).context("publish WebP image")?;
    Ok(())
}

/// Checks a dimension pair returned by a decoder.
fn validate_dimensions_pair((width, height): (u32, u32)) -> Result<()> {
    validate_dimensions(width, height)
}

/// Encode pixels only, stripping EXIF, ICC, XMP and ancillary metadata.
fn write_still(input: &Path, bounds: Option<(u32, u32)>, output: &mut File) -> Result<()> {
    let mut decoded = decode_still(input)?;
    if let Some((width, height)) = bounds {
        decoded = decoded.resize(width, height, image::imageops::FilterType::Triangle);
    }
    decoded
        .write_to(output, ImageFormat::WebP)
        .context("encode WebP")
}

/// Time bound for synchronous animation work on an existing blocking worker.
struct AnimationBudget {
    /// Start of this processing operation.
    started: Instant,
    /// Existing operator-configured media deadline.
    timeout: Duration,
}

impl AnimationBudget {
    /// Starts a fresh deadline.
    fn new() -> Self {
        Self {
            started: Instant::now(),
            timeout: Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        }
    }

    /// Refuses excessive animation work between decoder/encoder calls.
    fn check(&self, index: usize) -> Result<()> {
        ensure!(
            index < MAX_ANIMATION_FRAMES,
            "animation exceeds frame safety limit"
        );
        ensure!(
            self.started.elapsed() < self.timeout,
            "image processing timed out"
        );
        Ok(())
    }
}

/// Inspect GIF repetition metadata even when it follows the first frame.
fn gif_loops(input: &Path, budget: &AnimationBudget) -> Result<u16> {
    let mut options = gif::DecodeOptions::new();
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        std::num::NonZeroU64::new(super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES)
            .context("image allocation budget is zero")?,
    ));
    let mut decoder = options.read_info(BufReader::new(File::open(input)?))?;
    validate_dimensions(u32::from(decoder.width()), u32::from(decoder.height()))?;
    let mut count = 0;
    while decoder.next_frame_info()?.is_some() {
        budget.check(count)?;
        count += 1;
    }
    ensure!(count > 0, "GIF contains no frames");
    match decoder.repeat() {
        gif::Repeat::Infinite => Ok(0),
        // GIF counts repeats after the first play; WebP counts total plays.
        gif::Repeat::Finite(repeats) => repeats
            .checked_add(1)
            .context("GIF loop count cannot be represented by WebP"),
    }
}

/// Stream composited frames into ANMF chunks, retaining at most one frame.
fn encode_animation(
    frames: image::Frames<'_>,
    loops: u16,
    bounds: Option<(u32, u32)>,
    output: &mut File,
    budget: &AnimationBudget,
) -> Result<()> {
    let mut dimensions = None;
    for (index, frame) in frames.enumerate() {
        budget.check(index)?;
        let frame = frame.context("decode animation frame")?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let duration = numerator
            .checked_div(denominator)
            .context("invalid frame delay")?;
        let mut pixels = frame.into_buffer();
        validate_dimensions_pair(pixels.dimensions())?;
        if let Some((width, height)) = bounds {
            pixels = DynamicImage::ImageRgba8(pixels)
                .resize(width, height, image::imageops::FilterType::Triangle)
                .into_rgba8();
        }
        let canvas = pixels.dimensions();
        if let Some(expected) = dimensions {
            ensure!(canvas == expected, "animation canvas changes size");
        } else {
            write_animation_header(output, canvas, loops)?;
            dimensions = Some(canvas);
        }
        write_animation_frame(output, &pixels, duration)?;
    }
    ensure!(dimensions.is_some(), "animation contains no frames");
    let end = output.stream_position()?;
    let riff_size = u32::try_from(end.checked_sub(8).context("invalid RIFF size")?)?;
    output.seek(SeekFrom::Start(4))?;
    output.write_all(&riff_size.to_le_bytes())?;
    Ok(())
}

/// Write RIFF, VP8X and ANIM; canvas alpha and independent frames are supported.
fn write_animation_header(
    output: &mut File,
    (width, height): (u32, u32),
    loops: u16,
) -> Result<()> {
    validate_dimensions(width, height)?;
    output.write_all(b"RIFF\0\0\0\0WEBPVP8X")?;
    output.write_all(&10_u32.to_le_bytes())?;
    output.write_all(&[0x12, 0, 0, 0])?;
    write_u24(output, width - 1)?;
    write_u24(output, height - 1)?;
    output.write_all(b"ANIM")?;
    output.write_all(&6_u32.to_le_bytes())?;
    output.write_all(&[0; 4])?;
    output.write_all(&loops.to_le_bytes())?;
    Ok(())
}

/// Write one lossless, fully composited frame without blending it again.
fn write_animation_frame(output: &mut File, pixels: &RgbaImage, duration: u32) -> Result<()> {
    ensure!(duration <= MAX_U24, "frame duration exceeds WebP limits");
    // Spool encoded bytes to disk instead of retaining a second frame-sized Vec.
    let mut encoded = tempfile::tempfile().context("create animation frame spool")?;
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded).encode(
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
        end <= super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES,
        "animation output exceeds safety budget"
    );
    output.write_all(b"ANMF")?;
    output.write_all(
        &payload_size
            .checked_add(16)
            .context("animation frame overflow")?
            .to_le_bytes(),
    )?;
    output.write_all(&[0; 6])?;
    write_u24(output, pixels.width() - 1)?;
    write_u24(output, pixels.height() - 1)?;
    write_u24(output, duration)?;
    output.write_all(&[2])?;
    encoded.seek(SeekFrom::Start(12))?;
    std::io::copy(&mut encoded.take(u64::from(payload_size)), output)?;
    Ok(())
}

/// Serialize a checked little-endian three-byte WebP field.
fn write_u24(output: &mut File, value: u32) -> Result<()> {
    ensure!(value <= MAX_U24, "WebP field exceeds 24 bits");
    let bytes = value.to_le_bytes();
    output.write_all(bytes.get(..3).context("invalid three-byte field")?)?;
    Ok(())
}

#[cfg(test)]
mod tests;
