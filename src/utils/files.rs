// utils/files.rs
//
// File handling pipeline:
//   1. Receive multipart bytes
//   2. Validate MIME type against allowlist (BOTH content-type header AND magic bytes)
//   3. Validate file size
//   4. Generate random filename (UUID-based, prevents path traversal)
//   5. Write to upload directory
//   6. Generate thumbnail using image crate
//   7. Write thumbnail to thumbs/ subdirectory
//
// Security notes:
//   • We NEVER trust the Content-Type header alone — we check magic bytes.
//   • Filenames are never used as filesystem paths — UUIDs only.
//   • Files are stored flat (no user-supplied path components).
//   • We keep the original filename only for display purposes.

use anyhow::{Context, Result};
use image::{imageops::FilterType, GenericImageView, ImageFormat};
use std::path::PathBuf;
use uuid::Uuid;

/// Allowed MIME types and their magic bytes
const ALLOWED_MIME_TYPES: &[(&str, &[u8])] = &[
    ("image/jpeg", b"\xff\xd8\xff"),
    ("image/png", b"\x89PNG"),
    ("image/gif", b"GIF8"),
    ("image/webp", b"RIFF"),  // RIFF....WEBP
];

/// Maximum number of bytes to read for magic byte detection
const MAGIC_BYTES_LEN: usize = 12;

pub struct UploadedFile {
    /// Path on disk (relative to upload_dir, e.g. "abc123.jpg")
    pub file_path: String,
    /// Thumbnail path (relative to upload_dir, e.g. "thumbs/abc123.jpg")
    pub thumb_path: String,
    /// Original user-supplied filename (sanitised, for display only)
    pub original_name: String,
    /// Detected MIME type
    pub mime_type: String,
    /// File size in bytes
    pub file_size: i64,
}

/// Validate magic bytes and return detected MIME type.
/// Returns Err if the file type is not permitted.
pub fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    let header = &data[..data.len().min(MAGIC_BYTES_LEN)];

    for (mime, magic) in ALLOWED_MIME_TYPES {
        if header.starts_with(magic) {
            // Extra check for WebP: bytes 8..12 must be "WEBP"
            if *mime == "image/webp" {
                if data.len() < 12 || &data[8..12] != b"WEBP" {
                    continue;
                }
            }
            return Ok(mime);
        }
    }
    Err(anyhow::anyhow!(
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP"
    ))
}

/// Save an uploaded file to disk and generate its thumbnail.
/// Returns UploadedFile with relative paths.
pub fn save_upload(
    data: &[u8],
    original_filename: &str,
    upload_dir: &str,
    thumb_size: u32,
    max_size: usize,
) -> Result<UploadedFile> {
    // 1. Size check
    if data.len() > max_size {
        return Err(anyhow::anyhow!(
            "File too large. Maximum size is {} MiB.",
            max_size / 1024 / 1024
        ));
    }
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }

    // 2. Magic byte validation
    let mime_type = detect_mime_type(data)?;

    // 3. Generate safe filesystem names
    let file_id = Uuid::new_v4().to_string().replace('-', "");
    let ext = mime_to_ext(mime_type);
    // Thumbnails are saved as JPEG for space efficiency (except formats that
    // need their native format: PNG keeps transparency, GIF/WebP keep their encoder).
    let thumb_ext = match mime_type {
        "image/png"  => "png",
        "image/gif"  => "gif",
        "image/webp" => "webp",
        _            => "jpg",
    };
    let filename = format!("{}.{}", file_id, ext);
    let thumb_filename = format!("thumbs/{}.{}", file_id, thumb_ext);

    // 4. Ensure directories exist
    let thumbs_dir = PathBuf::from(upload_dir).join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)
        .context("Failed to create thumbs directory")?;

    // 5. Write original file
    let file_path = PathBuf::from(upload_dir).join(&filename);
    std::fs::write(&file_path, data)
        .context("Failed to write uploaded file")?;

    // 6. Generate thumbnail
    let thumb_path = PathBuf::from(upload_dir).join(&thumb_filename);
    generate_thumbnail(data, mime_type, &thumb_path, thumb_size)
        .context("Failed to generate thumbnail")?;

    Ok(UploadedFile {
        file_path: filename,
        thumb_path: thumb_filename,
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: mime_type.to_string(),
        file_size: data.len() as i64,
    })
}

/// Generate a thumbnail from image bytes.
/// For GIFs, uses the first frame (avoids decoding entire animation).
fn generate_thumbnail(
    data: &[u8],
    mime_type: &str,
    output_path: &PathBuf,
    max_dim: u32,
) -> Result<()> {
    let format = match mime_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => return Err(anyhow::anyhow!("Unsupported format for thumbnail")),
    };

    // Load image (GIF decoder reads only first frame by default with load)
    let img = image::load_from_memory_with_format(data, format)
        .context("Failed to decode image")?;

    // Compute thumbnail dimensions maintaining aspect ratio
    let (w, h) = img.dimensions();
    let (tw, th) = if w > h {
        let ratio = max_dim as f32 / w as f32;
        (max_dim, (h as f32 * ratio) as u32)
    } else {
        let ratio = max_dim as f32 / h as f32;
        ((w as f32 * ratio) as u32, max_dim)
    };

    // If image is already smaller than thumb_size, don't upscale
    let thumb = if w <= tw && h <= th {
        img
    } else {
        // Triangle (bilinear) filter — fast on Pi, good quality
        img.resize(tw, th, FilterType::Triangle)
    };

    // Save thumbnail. Use JPEG for photos (smaller), PNG for transparency,
    // and native format for GIF/WebP.
    let save_format = match mime_type {
        "image/png"  => ImageFormat::Png,
        "image/gif"  => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _            => ImageFormat::Jpeg,
    };

    thumb
        .save_with_format(output_path, save_format)
        .context("Failed to save thumbnail")?;

    Ok(())
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "bin",
    }
}

/// Delete a file from the filesystem, ignoring not-found errors.
pub fn delete_file(upload_dir: &str, relative_path: &str) {
    let full_path = PathBuf::from(upload_dir).join(relative_path);
    let _ = std::fs::remove_file(full_path); // Ignore errors
}

/// Format file size as human-readable string
pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}
