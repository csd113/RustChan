// media/exif.rs
//
// EXIF orientation helpers for decoded images.
//
// These functions are called from `utils/files.rs` during upload processing
// to ensure thumbnails are rendered upright regardless of camera orientation.
// They operate purely on in-memory pixel data — no I/O.

/// Read the EXIF Orientation tag from JPEG bytes.
///
/// Returns the orientation value (1–8) or 1 (no rotation) if the tag is
/// absent or unreadable.  Only JPEG files carry reliable EXIF orientation;
/// PNG/WebP/GIF do not use this tag.
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
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(data)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| {
            if let exif::Value::Short(ref v) = f.value {
                v.first().copied().map(u32::from)
            } else {
                None
            }
        })
        .unwrap_or(1)
}

/// Apply an EXIF orientation transformation to a decoded `DynamicImage`.
///
/// This corrects the pixel layout so that thumbnails appear upright regardless
/// of which way the camera was held when the photo was taken.  The `image`
/// crate operations used here are pure in-memory pixel transforms — no I/O.
#[must_use]
pub fn apply_exif_orientation(img: image::DynamicImage, orientation: u32) -> image::DynamicImage {
    use image::imageops;
    match orientation {
        2 => image::DynamicImage::ImageRgba8(imageops::flip_horizontal(&img)),
        3 => image::DynamicImage::ImageRgba8(imageops::rotate180(&img)),
        4 => image::DynamicImage::ImageRgba8(imageops::flip_vertical(&img)),
        5 => {
            // Transpose = rotate 90° CW then flip horizontally
            let rot = imageops::rotate90(&img);
            image::DynamicImage::ImageRgba8(imageops::flip_horizontal(&rot))
        }
        6 => image::DynamicImage::ImageRgba8(imageops::rotate90(&img)),
        7 => {
            // Transverse = rotate 90° CW then flip vertically
            let rot = imageops::rotate90(&img);
            image::DynamicImage::ImageRgba8(imageops::flip_vertical(&rot))
        }
        8 => image::DynamicImage::ImageRgba8(imageops::rotate270(&img)),
        _ => img, // 1 = normal, or unknown value — no transform
    }
}
