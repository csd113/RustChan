// media/exif.rs
//
// EXIF orientation helpers for decoded images.
//
// These functions are called from `utils/files.rs` during upload processing
// to ensure thumbnails are rendered upright regardless of camera orientation.
// They operate purely on in-memory pixel data — no I/O.

/// Read the EXIF Orientation tag from raw image bytes.
///
/// Returns the orientation value (1–8) or 1 (no rotation) if the tag is
/// absent or unreadable.  JPEG and TIFF files commonly carry EXIF orientation;
/// PNG/WebP/GIF typically do not.
///
/// Values follow the EXIF spec:
///   1 = normal (0°), 2 = flip-H, 3 = 180°, 4 = flip-V,
///   5 = transpose, 6 = 90° CW, 7 = transverse, 8 = 90° CCW
///
/// NOTE: This must be called on the ORIGINAL bytes before any EXIF-stripping
/// re-encode.  Once EXIF has been stripped, this function will always return 1.
#[must_use]
pub fn read_exif_orientation(data: &[u8]) -> u32 {
    use std::io::Cursor;

    let Ok(exif_data) = exif::Reader::new().read_from_container(&mut Cursor::new(data)) else {
        return 1;
    };

    let orientation = exif_data
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            // Most cameras write orientation as Short
            exif::Value::Short(v) => v.first().copied().map(u32::from),
            // Some cameras/software write it as Long
            exif::Value::Long(v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(1);

    // Clamp to the valid EXIF orientation range; treat out-of-range as 1
    if (1..=8).contains(&orientation) {
        orientation
    } else {
        eprintln!("Warning: invalid EXIF orientation value {orientation}, treating as 1 (normal)");
        1
    }
}

/// Apply an EXIF orientation transformation to a decoded `DynamicImage`.
///
/// This corrects the pixel layout so that thumbnails appear upright regardless
/// of which way the camera was held when the photo was taken.  The `image`
/// crate operations used here are pure in-memory pixel transforms — no I/O.
///
/// The output pixel format matches the input for the identity case
/// (orientation 1).  All other orientations produce RGBA8 output.
#[must_use]
pub fn apply_exif_orientation(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    match orientation {
        1 => img,
        _ => apply_orientation_inner(img, orientation),
    }
}

/// Inner helper that dispatches the actual pixel transform.
///
/// We convert to RGBA8 only when a transform is actually needed, and we
/// minimise intermediate allocations for the compound transforms (5 and 7)
/// by using in-place flips.
fn apply_orientation_inner(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    use image::imageops;

    // For transforms that operate on the image, we work on RgbaImage to keep
    // transparency if present.  The image crate's imageops functions require
    // concrete buffer types.
    let buf = img.to_rgba8();

    match orientation {
        2 => image::DynamicImage::ImageRgba8(imageops::flip_horizontal(&buf)),
        3 => image::DynamicImage::ImageRgba8(imageops::rotate180(&buf)),
        4 => image::DynamicImage::ImageRgba8(imageops::flip_vertical(&buf)),
        5 => {
            // Transpose = rotate 90° CW then flip horizontally
            // Flip in-place to avoid a second full allocation.
            let mut rot = imageops::rotate90(&buf);
            imageops::flip_horizontal_in_place(&mut rot);
            image::DynamicImage::ImageRgba8(rot)
        }
        6 => image::DynamicImage::ImageRgba8(imageops::rotate90(&buf)),
        7 => {
            // Transverse = rotate 90° CW then flip vertically
            // Flip in-place to avoid a second full allocation.
            let mut rot = imageops::rotate90(&buf);
            imageops::flip_vertical_in_place(&mut rot);
            image::DynamicImage::ImageRgba8(rot)
        }
        8 => image::DynamicImage::ImageRgba8(imageops::rotate270(&buf)),
        // orientation 1 is handled in the caller; anything else is a no-op
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_1_is_identity() {
        let img = image::DynamicImage::new_rgb8(10, 20);
        let result = apply_exif_orientation(img, 1);
        // Should be the same image, not converted to RGBA
        assert_eq!(result.color(), image::ColorType::Rgb8);
        assert_eq!(result.width(), 10);
        assert_eq!(result.height(), 20);
    }

    #[test]
    fn orientation_6_rotates_dimensions() {
        let img = image::DynamicImage::new_rgb8(10, 20);
        let result = apply_exif_orientation(img, 6);
        // 90° CW rotation swaps width and height
        assert_eq!(result.width(), 20);
        assert_eq!(result.height(), 10);
    }

    #[test]
    fn orientation_8_rotates_dimensions() {
        let img = image::DynamicImage::new_rgb8(10, 20);
        let result = apply_exif_orientation(img, 8);
        // 90° CCW rotation swaps width and height
        assert_eq!(result.width(), 20);
        assert_eq!(result.height(), 10);
    }

    #[test]
    fn orientation_3_preserves_dimensions() {
        let img = image::DynamicImage::new_rgb8(10, 20);
        let result = apply_exif_orientation(img, 3);
        // 180° rotation preserves dimensions
        assert_eq!(result.width(), 10);
        assert_eq!(result.height(), 20);
    }

    #[test]
    fn unknown_orientation_is_identity() {
        let img = image::DynamicImage::new_rgb8(10, 20);
        let result = apply_exif_orientation(img, 0);
        assert_eq!(result.width(), 10);
        assert_eq!(result.height(), 20);
    }

    #[test]
    fn read_orientation_from_empty_bytes_returns_1() {
        assert_eq!(read_exif_orientation(&[]), 1);
    }

    #[test]
    fn read_orientation_from_garbage_returns_1() {
        assert_eq!(read_exif_orientation(&[0xFF, 0xD8, 0x00, 0x00]), 1);
    }
}
