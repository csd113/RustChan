// EXIF orientation helpers for decoded images.

/// Read the EXIF Orientation tag from JPEG bytes.
///
/// Returns `1` when the tag is missing or unreadable.
#[must_use]
pub fn read_exif_orientation(data: &[u8]) -> u32 {
    use std::io::Cursor;
    let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(data)) else {
        return 1;
    };
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| {
            if let exif::Value::Short(v) = &f.value {
                v.first().copied().map(u32::from)
            } else {
                None
            }
        })
        .unwrap_or(1)
}

/// Apply an EXIF orientation transformation to a decoded `DynamicImage`.
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
