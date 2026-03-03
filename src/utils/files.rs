// utils/files.rs
//
// File handling pipeline:
//   1. Receive multipart bytes
//   2. Validate MIME type against allowlist (BOTH content-type header AND magic bytes)
//   3. Validate file size
//   4. Generate random filename (UUID-based, prevents path traversal)
//   5. Write to upload directory
//   6. Generate thumbnail
//      • Images  → scaled with the `image` crate
//      • Videos  → first-frame JPEG via ffmpeg (falls back to SVG if unavailable)
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
    ("image/png",  b"\x89PNG"),
    ("image/gif",  b"GIF8"),
    ("image/webp", b"RIFF"),        // RIFF....WEBP — extra check below
    ("video/mp4",  b"\x00\x00\x00"), // ftyp box check below
    ("video/webm", b"\x1a\x45\xdf\xa3"),
];

const MAGIC_BYTES_LEN: usize = 12;

pub struct UploadedFile {
    /// Path on disk relative to upload_dir (e.g. "abc123.webm")
    pub file_path:     String,
    /// Thumbnail path relative to upload_dir (e.g. "thumbs/abc123.jpg")
    pub thumb_path:    String,
    /// Original user-supplied filename, sanitised, for display only
    pub original_name: String,
    /// Detected MIME type
    pub mime_type:     String,
    /// File size in bytes
    pub file_size:     i64,
}

// ─── MIME detection ───────────────────────────────────────────────────────────

/// Validate magic bytes and return detected MIME type.
pub fn detect_mime_type(data: &[u8]) -> Result<&'static str> {
    let header = &data[..data.len().min(MAGIC_BYTES_LEN)];

    // MP4: variable-length box; bytes 4..8 must be b"ftyp"
    if data.len() >= 8 && &data[4..8] == b"ftyp" {
        return Ok("video/mp4");
    }

    for (mime, magic) in ALLOWED_MIME_TYPES {
        if *mime == "video/mp4" { continue; }
        if header.starts_with(magic) {
            if *mime == "image/webp" {
                if data.len() < 12 || &data[8..12] != b"WEBP" {
                    continue;
                }
            }
            return Ok(mime);
        }
    }
    Err(anyhow::anyhow!(
        "File type not allowed. Accepted: JPEG, PNG, GIF, WebP, MP4, WebM"
    ))
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Save an uploaded file to disk and generate its thumbnail.
pub fn save_upload(
    data:              &[u8],
    original_filename: &str,
    upload_dir:        &str,
    thumb_size:        u32,
    max_size:          usize,
) -> Result<UploadedFile> {
    if data.len() > max_size {
        return Err(anyhow::anyhow!(
            "File too large. Maximum size is {} MiB.",
            max_size / 1024 / 1024
        ));
    }
    if data.is_empty() {
        return Err(anyhow::anyhow!("File is empty."));
    }

    let mime_type = detect_mime_type(data)?;

    let file_id  = Uuid::new_v4().to_string().replace('-', "");
    let ext      = mime_to_ext(mime_type);
    let is_video = mime_type.starts_with("video/");
    let filename = format!("{}.{}", file_id, ext);

    // Ensure directories exist
    let thumbs_dir = PathBuf::from(upload_dir).join("thumbs");
    std::fs::create_dir_all(&thumbs_dir)
        .context("Failed to create thumbs directory")?;

    // Write original file
    let file_path = PathBuf::from(upload_dir).join(&filename);
    std::fs::write(&file_path, data)
        .context("Failed to write uploaded file")?;

    // Generate thumbnail — returns (relative_name, absolute_path)
    let (thumb_filename, _thumb_abs) = if is_video {
        generate_video_thumb(data, upload_dir, &file_id, thumb_size)
    } else {
        let thumb_ext = match mime_type {
            "image/png"  => "png",
            "image/gif"  => "gif",
            "image/webp" => "webp",
            _            => "jpg",
        };
        let name = format!("thumbs/{}.{}", file_id, thumb_ext);
        let path = PathBuf::from(upload_dir).join(&name);
        generate_image_thumb(data, mime_type, &path, thumb_size)
            .context("Failed to generate image thumbnail")?;
        (name, path)
    };

    Ok(UploadedFile {
        file_path:     filename,
        thumb_path:    thumb_filename,
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type:     mime_type.to_string(),
        file_size:     data.len() as i64,
    })
}

// ─── Video thumbnail ──────────────────────────────────────────────────────────

/// Try ffmpeg first-frame extraction; fall back to SVG placeholder on failure.
/// Returns (relative_thumb_name, absolute_path).
fn generate_video_thumb(
    video_data: &[u8],
    upload_dir: &str,
    file_id:    &str,
    thumb_size: u32,
) -> (String, PathBuf) {
    // Attempt real first-frame thumbnail via ffmpeg
    let jpg_name = format!("thumbs/{}.jpg", file_id);
    let jpg_path = PathBuf::from(upload_dir).join(&jpg_name);

    match ffmpeg_first_frame(video_data, &jpg_path, thumb_size) {
        Ok(()) => {
            tracing::debug!("ffmpeg thumbnail generated for {}", file_id);
            return (jpg_name, jpg_path);
        }
        Err(e) => {
            tracing::warn!("ffmpeg thumbnail failed ({}), using SVG placeholder", e);
        }
    }

    // Fall back to SVG placeholder
    let svg_name = format!("thumbs/{}.svg", file_id);
    let svg_path = PathBuf::from(upload_dir).join(&svg_name);
    if let Err(e) = generate_video_placeholder(&svg_path) {
        tracing::error!("Failed to write SVG placeholder: {}", e);
    }
    (svg_name, svg_path)
}

/// Shell out to ffmpeg to extract the first frame as a scaled JPEG.
///
/// Requirements: `ffmpeg` must be on PATH.
/// The video bytes are written to a temp file, ffmpeg runs, the JPEG is
/// moved to `output_path`, and the temp file is cleaned up.
fn ffmpeg_first_frame(
    video_data:  &[u8],
    output_path: &PathBuf,
    thumb_size:  u32,
) -> Result<()> {
    use std::process::Command;

    let temp_dir = std::env::temp_dir();
    let tmp_id   = Uuid::new_v4().to_string().replace('-', "");
    let temp_in  = temp_dir.join(format!("chan_vid_{}.tmp",   tmp_id));
    let temp_out = temp_dir.join(format!("chan_thm_{}.jpg", tmp_id));

    std::fs::write(&temp_in, video_data)
        .context("Failed to write temp video for ffmpeg")?;

    // scale=W:-2 : scale width to thumb_size, height to nearest even number
    let vf = format!("scale={}:-2", thumb_size);

    let output = Command::new("ffmpeg")
        .args([
            "-loglevel", "error",
            "-i",        temp_in.to_str().unwrap_or(""),
            "-vframes",  "1",
            "-ss",       "0",
            "-vf",       &vf,
            "-y",
            temp_out.to_str().unwrap_or(""),
        ])
        .output();

    // Always remove the temp input
    let _ = std::fs::remove_file(&temp_in);

    let out = output.context("ffmpeg not found or failed to spawn")?;

    if out.status.success() && temp_out.exists() {
        std::fs::rename(&temp_out, output_path)
            .context("Failed to move ffmpeg output")?;
        Ok(())
    } else {
        let _ = std::fs::remove_file(&temp_out);
        Err(anyhow::anyhow!(
            "ffmpeg exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Minimal SVG play-button fallback (used when ffmpeg is unavailable).
fn generate_video_placeholder(output_path: &PathBuf) -> Result<()> {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="250" height="250" viewBox="0 0 250 250">
  <rect width="250" height="250" fill="#0a0f0a"/>
  <circle cx="125" cy="125" r="60" fill="#0d120d" stroke="#00c840" stroke-width="2"/>
  <polygon points="108,95 108,155 165,125" fill="#00c840"/>
  <text x="125" y="215" text-anchor="middle" fill="#3a4a3a" font-family="monospace" font-size="12">VIDEO</text>
</svg>"##;
    std::fs::write(output_path, svg)?;
    Ok(())
}

// ─── Image thumbnail ──────────────────────────────────────────────────────────

fn generate_image_thumb(
    data:        &[u8],
    mime_type:   &str,
    output_path: &PathBuf,
    max_dim:     u32,
) -> Result<()> {
    let format = match mime_type {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png"  => ImageFormat::Png,
        "image/gif"  => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _            => return Err(anyhow::anyhow!("Unsupported image format")),
    };

    let img = image::load_from_memory_with_format(data, format)
        .context("Failed to decode image")?;

    let (w, h) = img.dimensions();
    let (tw, th) = if w > h {
        let r = max_dim as f32 / w as f32;
        (max_dim, (h as f32 * r) as u32)
    } else {
        let r = max_dim as f32 / h as f32;
        ((w as f32 * r) as u32, max_dim)
    };

    let thumb = if w <= tw && h <= th {
        img
    } else {
        img.resize(tw, th, FilterType::Triangle)
    };

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

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png"  => "png",
        "image/gif"  => "gif",
        "image/webp" => "webp",
        "video/mp4"  => "mp4",
        "video/webm" => "webm",
        _            => "bin",
    }
}

/// Delete a file from the filesystem, ignoring not-found errors.
pub fn delete_file(upload_dir: &str, relative_path: &str) {
    let full_path = PathBuf::from(upload_dir).join(relative_path);
    let _ = std::fs::remove_file(full_path);
}

/// Format file size as human-readable string.
pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}
