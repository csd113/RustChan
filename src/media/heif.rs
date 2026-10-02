//! Pure-Rust HEIC still-picture decoding; no inter-frame video processing.

use anyhow::{ensure, Context as _, Result};
use heic_rs::context::Context;
use std::io::Read as _;
use std::path::Path;

/// Bound the sum of primary/alpha tile areas, including encoded padding.
const MAX_CODED_PIXELS: u64 = super::MAX_UNTRUSTED_IMAGE_PIXELS * 2;

/// Decode the primary HEIC picture and apply its container transforms.
#[expect(
    clippy::redundant_pub_crate,
    reason = "keep codec helpers internal while allowing upload and banner callers"
)]
pub(crate) fn decode(input: &Path) -> Result<image::DynamicImage> {
    let file = std::fs::File::open(input).context("open HEIC image")?;
    let limit = super::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES;
    ensure!(
        file.metadata()?.len() <= limit,
        "HEIC input exceeds memory safety limit"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= limit,
        "HEIC input grew beyond safety limit"
    );
    let context = Context::open(&bytes).context("read HEIC container")?;
    let info = heic_rs::probe(&bytes).context("inspect HEIC primary image")?;
    super::images::validate_dimensions(info.width, info.height)?;
    let mut coded_pixels = preflight_item(&context, context.meta.primary)?;
    if let Some(alpha) = context.alpha_item(context.meta.primary)? {
        coded_pixels = coded_pixels
            .checked_add(preflight_item(&context, alpha)?)
            .context("HEIC tile area overflow")?;
    }
    ensure!(
        coded_pixels <= MAX_CODED_PIXELS,
        "HEIC tile allocation exceeds safety limit"
    );
    let options = heic_rs::DecodeOptions {
        layout: heic_rs::PixelLayout::Rgba8,
        max_pixels: Some(super::MAX_UNTRUSTED_IMAGE_PIXELS),
        apply_transforms: true,
        decode_alpha: true,
        threads: Some(1),
        ..Default::default()
    };
    let pixels = heic_rs::decode(&bytes, &options).context("decode HEIC still picture")?;
    let image = image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.data)
        .context("HEIC decoder returned invalid pixel buffer")?;
    Ok(image::DynamicImage::ImageRgba8(image))
}

/// Inspect every coded tile before the decoder allocates its picture planes.
fn preflight_item(context: &Context<'_>, item: u32) -> Result<u64> {
    let tiles = if let Some((_, tiles)) = context.grid(item)? {
        tiles
    } else {
        vec![item]
    };
    let mut area = 0_u64;
    ensure!(tiles.len() <= 4096, "HEIC has too many tiles");
    for tile in tiles {
        let properties = context.props(tile)?;
        let config = properties
            .hvcc
            .context("HEIC coded item has no HEVC configuration")?;
        let mut largest = 0;
        for array in &config.arrays {
            if array.nal_type == heic_rs::props::hvcc::NAL_SPS {
                for nal in &array.nals {
                    let (width, height) = sps_dimensions(nal)?;
                    super::images::validate_dimensions(width, height)?;
                    largest = largest.max(u64::from(width) * u64::from(height));
                }
            }
        }
        ensure!(largest > 0, "HEIC item has no sequence geometry");
        area = area
            .checked_add(largest)
            .context("HEIC coded area overflow")?;
        ensure!(
            area <= MAX_CODED_PIXELS,
            "HEIC tile allocation exceeds safety limit"
        );
    }
    Ok(area)
}

/// Inspect raw SPS dimensions, before cropping, to prevent forged `ispe` limits.
///
/// This is a narrow metadata reader, not a video decoder. The still decoder's
/// public probe reports cropped dimensions, while allocation uses raw geometry.
fn sps_dimensions(nal: &[u8]) -> Result<(u32, u32)> {
    ensure!(
        nal.len() <= 1024 * 1024,
        "HEIC sequence header exceeds safety limit"
    );
    let mut rbsp = Vec::with_capacity(nal.len());
    let mut zeroes = 0_u8;
    for byte in nal.get(2..).context("truncated HEIC sequence header")? {
        if zeroes >= 2 && *byte == 3 {
            zeroes = 0;
            continue;
        }
        rbsp.push(*byte);
        zeroes = if *byte == 0 {
            zeroes.saturating_add(1)
        } else {
            0
        };
    }
    let mut bits = Bits {
        data: &rbsp,
        position: 0,
    };
    bits.skip(4)?;
    let layers = usize::try_from(bits.read(3)?)?;
    bits.skip(1 + 96)?;
    let mut sublayers = Vec::with_capacity(layers);
    for _ in 0..layers {
        sublayers.push((bits.read(1)? != 0, bits.read(1)? != 0));
    }
    if layers > 0 {
        bits.skip((8 - layers) * 2)?;
    }
    for (profile, level) in sublayers {
        if profile {
            bits.skip(88)?;
        }
        if level {
            bits.skip(8)?;
        }
    }
    let _sequence_id = bits.unsigned_exp_golomb()?;
    let chroma = bits.unsigned_exp_golomb()?;
    ensure!(chroma <= 3, "invalid HEIC chroma format");
    if chroma == 3 {
        bits.skip(1)?;
    }
    Ok((bits.unsigned_exp_golomb()?, bits.unsigned_exp_golomb()?))
}

/// Checked bit cursor for the small prefix of sequence-parameter metadata.
struct Bits<'a> {
    /// Unescaped sequence header bytes.
    data: &'a [u8],
    /// Bit offset within the bounded header.
    position: usize,
}

impl Bits<'_> {
    /// Read up to 32 bits, failing on truncation.
    fn read(&mut self, count: usize) -> Result<u32> {
        ensure!(count <= 32, "invalid HEIC bit count");
        let mut value = 0_u32;
        for _ in 0..count {
            let byte = self
                .data
                .get(self.position / 8)
                .context("truncated HEIC sequence bits")?;
            value = (value << 1) | u32::from((byte >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(value)
    }

    /// Skip a checked fixed-width field.
    fn skip(&mut self, count: usize) -> Result<()> {
        self.position = self
            .position
            .checked_add(count)
            .context("HEIC bit offset overflow")?;
        ensure!(
            self.position <= self.data.len().saturating_mul(8),
            "truncated HEIC sequence field"
        );
        Ok(())
    }

    /// Read a bounded unsigned Exp-Golomb metadata integer.
    fn unsigned_exp_golomb(&mut self) -> Result<u32> {
        let mut zeroes = 0;
        while self.read(1)? == 0 {
            zeroes += 1;
            ensure!(zeroes < 32, "HEIC sequence integer overflows");
        }
        Ok((1_u32 << zeroes) - 1 + self.read(zeroes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_heic_tiles_alpha_orientation_and_heif_extension_decode() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let outputs = tempfile::tempdir()?;
        for (name, expected) in [
            ("photo.heic", (2048, 1536)),
            ("phone-hdr.heic", (1512, 850)),
            ("alpha.heic", (48, 48)),
            ("irot90.heic", (40, 64)),
        ] {
            let image = decode(&root.join(name))?;
            ensure!(
                (image.width(), image.height()) == expected,
                "HEIC canvas changed for {name}"
            );
            let converted = outputs.path().join(format!("{name}.webp"));
            super::super::images::image_to_webp(&root.join(name), &converted, None)?;
            ensure!(
                image::open(&converted)?.into_rgba8() == image.into_rgba8(),
                "HEIC conversion changed pixels for {name}"
            );
            let thumbnail = outputs.path().join(format!("{name}-thumb.webp"));
            super::super::thumbnail::generate_thumbnail(
                &root.join(name),
                "image/heic",
                &thumbnail,
                64,
                false,
            )?;
            let thumb = image::open(thumbnail)?;
            ensure!(
                thumb.width() <= 64 && thumb.height() <= 64,
                "HEIC thumbnail exceeded bounds"
            );
        }
        let alpha = decode(&root.join("alpha.heic"))?.into_rgba8();
        ensure!(
            (20..235).contains(&alpha.get_pixel(20, 20).0[3]),
            "HEIC alpha plane was lost"
        );
        let rotated = decode(&root.join("irot90.heic"))?.into_rgba8();
        let green = rotated.get_pixel(4, 4).0;
        let red = rotated.get_pixel(4, 59).0;
        ensure!(
            green[1] > green[0] && green[1] > green[2],
            "HEIC rotation changed top-left colour"
        );
        ensure!(
            red[0] > red[1] && red[0] > red[2],
            "HEIC rotation changed bottom-left colour"
        );
        let dir = tempfile::tempdir()?;
        let heif = dir.path().join("image.heif");
        std::fs::copy(root.join("photo.heic"), &heif)?;
        ensure!(
            decode(&heif)?.width() == 2048,
            "HEIF extension lost support"
        );
        Ok(())
    }

    #[test]
    fn truncated_heic_and_sps_headers_fail_without_panicking() -> Result<()> {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/alpha.heic"),
        )?;
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("truncated.heic");
        for cut in [0, 1, 8, 32, 100, 500] {
            std::fs::write(&input, bytes.get(..cut).context("fixture cut")?)?;
            ensure!(decode(&input).is_err(), "truncated HEIC accepted");
        }
        ensure!(
            sps_dimensions(&[0; 32]).is_err(),
            "unbounded sequence integer accepted"
        );
        Ok(())
    }
}
