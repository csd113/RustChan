// src/utils/files/jpeg.rs

use anyhow::{Context, Result};
use std::path::Path;

pub(super) fn strip_jpeg_exif_file(input_path: &Path, output_path: &Path) -> Result<()> {
    let data = std::fs::read(input_path).context("Failed to read JPEG for EXIF strip")?;
    let clean = strip_jpeg_exif(&data)?;
    std::fs::write(output_path, clean).context("Failed to write stripped JPEG temp file")?;
    Ok(())
}

pub(super) fn read_exif_orientation_from_file(path: &Path) -> Result<u32> {
    let data = std::fs::read(path).context("Failed to read JPEG EXIF data")?;
    Ok(crate::media::exif::read_exif_orientation(&data))
}

fn strip_jpeg_exif(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;

    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
        .context("Failed to decode JPEG for EXIF strip")?;
    let mut cursor = Cursor::new(Vec::with_capacity(data.len()));
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .context("Failed to re-encode JPEG after EXIF strip")?;
    Ok(cursor.into_inner())
}
