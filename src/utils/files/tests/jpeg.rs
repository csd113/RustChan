//! Protect lossless metadata stripping, orientation, and original-JPEG fallback.

use anyhow::{ensure, Result};

/// Every EXIF orientation reaches full output and no-upscale thumbnails once.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "keeps the eight-orientation pixel table, metadata checks, and fallback verification together"
)]
fn jpeg_pixels_orientation_and_private_metadata() -> Result<()> {
    let image = image::RgbImage::from_fn(3, 2, |x, y| {
        image::Rgb([
            u8::try_from(x * 80).unwrap_or(0),
            u8::try_from(y * 180).unwrap_or(0),
            50,
        ])
    });
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&image)?;
    let source = image::load_from_memory(&jpeg)?.into_rgba8();
    let maps = [
        [0, 1, 2, 3, 4, 5],
        [2, 1, 0, 5, 4, 3],
        [5, 4, 3, 2, 1, 0],
        [3, 4, 5, 0, 1, 2],
        [0, 3, 1, 4, 2, 5],
        [3, 0, 4, 1, 5, 2],
        [5, 2, 4, 1, 3, 0],
        [2, 5, 1, 4, 0, 3],
    ];
    for (index, map) in maps.iter().enumerate() {
        let orientation = u16::try_from(index + 1)?;
        let mut tagged = jpeg
            .get(..2)
            .ok_or_else(|| anyhow::anyhow!("missing SOI"))?
            .to_vec();
        tagged.extend_from_slice(
            b"\xff\xe1\x00\x22Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0",
        );
        tagged.extend_from_slice(&orientation.to_le_bytes());
        tagged.extend_from_slice(&[0; 6]);
        tagged.extend_from_slice(b"\xff\xfe\x00\x09private");
        tagged.extend_from_slice(
            jpeg.get(2..)
                .ok_or_else(|| anyhow::anyhow!("missing image"))?,
        );
        let mut clean = Vec::new();
        super::strip_metadata(&tagged, &mut clean)?;
        ensure!(
            !clean.windows(7).any(|bytes| bytes == b"private"),
            "private JPEG comment survived"
        );
        // Strip the fresh orientation tag too: original coding and entropy bytes match.
        let tag_length = if orientation > 1 { 36 } else { 0 };
        let mut baseline = Vec::new();
        super::strip_metadata(&jpeg, &mut baseline)?;
        ensure!(
            clean.get(2 + tag_length..) == baseline.get(2..),
            "JPEG coding bytes changed"
        );
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.jpg");
        std::fs::write(&input, &clean)?;
        let expected: Vec<_> = map
            .iter()
            .map(|position| {
                source
                    .pixels()
                    .nth(*position)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("invalid orientation map"))
            })
            .collect::<Result<_>>()?;
        let processor = crate::media::MediaProcessor::new_with_ffmpeg(false);
        let processed = processor.process_upload(
            &input,
            "image/jpeg",
            dir.path(),
            "output",
            dir.path(),
            250,
        )?;
        let output = image::open(processed.file_path)?.into_rgba8();
        ensure!(
            output.pixels().copied().eq(expected.iter().copied()),
            "JPEG pixels changed for orientation {orientation}"
        );
        ensure!(
            output.dimensions() == if orientation >= 5 { (2, 3) } else { (3, 2) },
            "orientation dimensions changed"
        );
        let thumbnail = image::open(
            processed
                .thumbnail_path
                .ok_or_else(|| anyhow::anyhow!("no thumbnail"))?,
        )?
        .into_rgba8();
        ensure!(thumbnail == output, "thumbnail reoriented or enlarged");
        // Exercise fallback with an exhausted output allowance, retaining a clean,
        // correctly tagged original instead of rejecting a valid upload.
        let budget = crate::media::image_budget::ImageBudget::new(1);
        let fallback = processor.process_upload_with_budget(
            &input,
            "image/jpeg",
            dir.path(),
            "fallback",
            dir.path(),
            250,
            &budget,
        )?;
        ensure!(
            !fallback.was_converted && std::fs::read(fallback.file_path)? == clean,
            "JPEG fallback changed compressed data"
        );
    }
    Ok(())
}

/// Metadata between progressive scans and after entropy must also be removed.
#[test]
fn progressive_scan_markers_and_stuffing_are_preserved() -> Result<()> {
    let original = b"\xff\xd8\xff\xda\x00\x02\x12\xff\x00\x34\xff\xd0\x56\xff\xfe\x00\x09private\xff\xda\x00\x02\x78\xff\x00\x90\xff\xd9trailing-private";
    let expected = b"\xff\xd8\xff\xda\x00\x02\x12\xff\x00\x34\xff\xd0\x56\xff\xda\x00\x02\x78\xff\x00\x90\xff\xd9";
    let mut clean = Vec::new();
    super::strip_metadata(original, &mut clean)?;
    ensure!(clean == expected, "scan bytes changed or metadata survived");
    Ok(())
}
