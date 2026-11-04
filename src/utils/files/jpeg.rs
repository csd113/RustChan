//! JPEG metadata removal without touching the compressed image data.

use anyhow::{ensure, Context as _, Result};
use std::io::Read as _;
use std::path::Path;

/// Strip private metadata while retaining only a fresh orientation tag.
///
/// Scan data and coding tables are copied verbatim, including progressive scans.
/// Adobe's fixed color-transform marker is needed for correct CMYK decoding.
pub(super) fn strip_jpeg_exif_file(input_path: &Path, output_path: &Path) -> Result<()> {
    let file = std::fs::File::open(input_path)?;
    let limit = crate::media::UNTRUSTED_IMAGE_DECODER_MAX_ALLOC_BYTES;
    ensure!(
        file.metadata()?.len() <= limit,
        "JPEG input exceeds safety limit"
    );
    let mut data = Vec::new();
    let _read = file.take(limit + 1).read_to_end(&mut data)?;
    ensure!(
        u64::try_from(data.len())? <= limit,
        "JPEG input grew beyond safety limit"
    );
    let mut output = std::fs::File::create(output_path)?;
    strip_metadata(&data, &mut output)
}

/// Copy a JPEG marker stream, dropping ancillary segments and trailing bytes.
fn strip_metadata(data: &[u8], output: &mut impl std::io::Write) -> Result<()> {
    ensure!(data.get(..2) == Some(&[0xff, 0xd8]), "invalid JPEG start");
    output.write_all(&[0xff, 0xd8])?;
    let orientation = crate::media::exif::read_exif_orientation(data);
    if (2..=8).contains(&orientation) {
        // A new TIFF directory with exactly one nonprivate orientation entry.
        output.write_all(
            b"\xff\xe1\x00\x22Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0",
        )?;
        output.write_all(&u16::try_from(orientation)?.to_le_bytes())?;
        output.write_all(&[0; 6])?;
    }
    let mut offset = 2_usize;
    let mut in_scan = false;
    loop {
        if in_scan {
            let end = scan_end(data, offset)?;
            output.write_all(data.get(offset..end).context("invalid JPEG scan")?)?;
            offset = end;
        }
        ensure!(data.get(offset) == Some(&0xff), "invalid JPEG marker");
        while data.get(offset) == Some(&0xff) {
            offset = offset.checked_add(1).context("JPEG offset overflow")?;
        }
        let marker = *data.get(offset).context("truncated JPEG marker")?;
        offset = offset.checked_add(1).context("JPEG offset overflow")?;
        if marker == 0xd9 {
            output.write_all(&[0xff, marker])?;
            return Ok(());
        }
        ensure!(
            !matches!(marker, 0 | 0xd8 | 0xd0..=0xd7),
            "invalid JPEG segment"
        );
        if marker == 1 {
            output.write_all(&[0xff, marker])?;
            continue;
        }
        let length_end = offset.checked_add(2).context("JPEG offset overflow")?;
        let length = u16::from_be_bytes(
            data.get(offset..length_end)
                .context("truncated JPEG length")?
                .try_into()?,
        );
        ensure!(length >= 2, "invalid JPEG length");
        let end = offset
            .checked_add(usize::from(length))
            .context("JPEG segment overflow")?;
        let segment = data.get(offset..end).context("truncated JPEG segment")?;
        let adobe = marker == 0xee && length == 14 && segment.get(2..7) == Some(b"Adobe");
        if !matches!(marker, 0xe0..=0xef | 0xfe) || adobe {
            output.write_all(&[0xff, marker])?;
            output.write_all(segment)?;
        }
        // DNL may occur inside a scan; ordinary table markers end the scan.
        in_scan = marker == 0xda || (in_scan && marker == 0xdc);
        offset = end;
    }
}

/// Find an unstuffed, non-restart marker without interpreting entropy data.
fn scan_end(data: &[u8], start: usize) -> Result<usize> {
    let mut offset = start;
    loop {
        let relative = data
            .get(offset..)
            .context("invalid JPEG scan offset")?
            .iter()
            .position(|byte| *byte == 0xff)
            .context("JPEG has no end marker")?;
        offset = offset.checked_add(relative).context("JPEG scan overflow")?;
        let next = offset.checked_add(1).context("JPEG scan overflow")?;
        if matches!(data.get(next), Some(0 | 0xd0..=0xd7)) {
            offset = next.checked_add(1).context("JPEG scan overflow")?;
        } else if data.get(next) == Some(&0xff) {
            offset = next;
        } else {
            return Ok(offset);
        }
    }
}

#[cfg(test)]
#[path = "tests/jpeg.rs"]
mod tests;
