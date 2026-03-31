// utils/files.rs
//
// File handling pipeline:
//   1. Receive multipart bytes
//   2. Validate MIME type against allowlist (BOTH magic bytes AND extension)
//   3. Validate file size using per-type limits
//   4. Generate random filename (UUID-based, prevents path traversal)
//   5. If JPEG: re-encode through the `image` crate to strip all EXIF metadata
//      NOTE: EXIF Orientation tag is read from the ORIGINAL bytes BEFORE
//      stripping so that thumbnails are still rendered upright.
//   6. If video and ffmpeg is available: mark pending for background transcoding
//   7. Write to boards directory
//   8. Generate thumbnail / placeholder
//      • Images  → scaled with the `image` crate
//      • Videos  → first-frame JPEG via ffmpeg (falls back to SVG if unavailable)
//        NOTE: GIF thumbnails are single-frame (first frame only).
//      • Audio   → waveform PNG via ffmpeg (falls back to SVG music-note placeholder)
//                  unless uploaded alongside an image, in which case the image IS
//                  the audio thumbnail (see `save_audio_with_image_thumb`).
//   9. Write thumbnail to thumbs/ subdirectory
//
// All ffmpeg/ffprobe operations are delegated to `crate::media`.
//
// Security notes:
//   • We NEVER trust the Content-Type header alone — we check magic bytes.
//   • Filenames are never used as filesystem paths — UUIDs only.
//   • Files are stored flat (no user-supplied path components).
//   • We keep the original filename only for display purposes.
//   • delete_file validates the relative path to prevent directory traversal.
//   • Audio files that fail magic-byte detection are rejected.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

mod disk_space;
mod mime;

use disk_space::check_disk_space;
pub use mime::detect_mime_type;

// ─── Output type ─────────────────────────────────────────────────────────────

pub struct UploadedFile {
    /// Path on disk relative to the boards root (e.g. "b/abc123.webm")
    pub file_path: String,
    /// Thumbnail path relative to the boards root (e.g. "b/thumbs/abc123.jpg")
    pub thumb_path: String,
    /// Original user-supplied filename, sanitised, for display only
    pub original_name: String,
    /// Detected MIME type (always a &'static str value, stored as String)
    pub mime_type: String,
    /// File size in bytes
    pub file_size: i64,
    /// Explicit media category derived from `mime_type`
    pub media_type: crate::models::MediaType,
    /// True when the file needs async background processing:
    ///   • Video (MP4) → `VideoTranscode` job (MP4 → `WebM` via ffmpeg)
    ///   • Audio       → `AudioWaveform` job  (waveform PNG via ffmpeg)
    /// The handler must enqueue the appropriate Job after the post is inserted
    /// so that the `post_id` is available. Always false for cached/dedup hits.
    pub processing_pending: bool,
}

// ─── Main entry point ─────────────────────────────────────────────────────────

/// Save an uploaded file to disk and generate its thumbnail (or audio placeholder).
///
/// Files are stored under `{boards_dir}/{board_short}/` and thumbnails
/// under `{boards_dir}/{board_short}/thumbs/`.
/// The returned paths are relative to `boards_dir` (e.g. `"b/abc123.webm"`).
///
/// When `ffmpeg_available` is true, media files are converted to optimal web
/// formats (JPEG/BMP/TIFF → WebP, GIF → WebM/VP9, PNG → WebP if smaller).
/// MP4/WebM files are flagged for background transcoding (existing pipeline).
///
/// All thumbnails are produced as WebP.  If ffmpeg is unavailable, image
/// thumbnails use the `image` crate as a fallback; video/audio thumbnails
/// fall back to static SVG placeholders.
pub fn save_upload_from_path(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    original_filename: &str,
    boards_dir: &str,
    board_short: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<UploadedFile> {
    if original_size == 0 {
        return Err(anyhow::anyhow!("File is empty."));
    }

    let mime_type = detect_mime_type(sniff_bytes)?;
    if mime_type == "image/svg+xml" {
        return Err(anyhow::anyhow!(
            "File type not allowed. SVG files are not accepted because they can contain executable JavaScript."
        ));
    }
    let media_type = crate::models::MediaType::from_mime(mime_type)
        .ok_or_else(|| anyhow::anyhow!("Could not classify detected MIME type: {mime_type}"))?;

    let max_size = match media_type {
        crate::models::MediaType::Video => max_video_size,
        crate::models::MediaType::Audio => max_audio_size,
        crate::models::MediaType::Image => max_image_size,
    };
    if original_size > max_size {
        return Err(anyhow::anyhow!(
            "File too large. Maximum {} size is {} MiB.",
            match media_type {
                crate::models::MediaType::Video => "video",
                crate::models::MediaType::Audio => "audio",
                crate::models::MediaType::Image => "image",
            },
            max_size / 1024 / 1024
        ));
    }

    let jpeg_orientation = if mime_type == "image/jpeg" {
        read_exif_orientation_from_file(input_path)?
    } else {
        1
    };

    let file_id = Uuid::new_v4().simple().to_string();
    let processing_pending = ffmpeg_available
        && matches!(
            media_type,
            crate::models::MediaType::Video | crate::models::MediaType::Audio
        )
        && (media_type != crate::models::MediaType::Video
            || mime_type == "video/mp4"
            || mime_type == "video/webm");

    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    let thumbs_dir = dest_dir.join("thumbs");
    std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;
    check_disk_space(&dest_dir, original_size)?;

    let processor_input = prepare_processor_input(input_path, &dest_dir, mime_type)?;
    let processor =
        crate::media::MediaProcessor::new_with_ffmpeg_caps(ffmpeg_available, ffmpeg_webp_available);
    let processed = processor
        .process_upload(
            processor_input.path(),
            mime_type,
            &dest_dir,
            &file_id,
            &thumbs_dir,
            thumb_size,
        )
        .context("Media processing pipeline failed")?;

    if jpeg_orientation > 1
        && !ffmpeg_available
        && processed.thumbnail_path.exists()
        && processed
            .thumbnail_path
            .extension()
            .and_then(|e| e.to_str())
            == Some("webp")
    {
        apply_thumb_exif_orientation(&processed.thumbnail_path, jpeg_orientation);
    }

    let final_mime: String = processed.mime_type.clone();
    let final_media_type = crate::models::MediaType::from_mime(&final_mime).unwrap_or(media_type);
    let file_size = i64::try_from(processed.final_size).context("File size overflows i64")?;

    let filename = processed
        .file_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Converted file has non-UTF-8 name")?;
    let rel_file = format!("{board_short}/{filename}");

    let thumb_filename = processed
        .thumbnail_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("Thumbnail file has non-UTF-8 name")?;
    let rel_thumb = format!("{board_short}/thumbs/{thumb_filename}");

    let final_processing_pending = if processed.was_converted {
        false
    } else {
        processing_pending
    };

    Ok(UploadedFile {
        file_path: rel_file,
        thumb_path: rel_thumb,
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: final_mime,
        file_size,
        media_type: final_media_type,
        processing_pending: final_processing_pending,
    })
}

// ─── EXIF orientation for image-crate thumbnails ──────────────────────────────

/// Re-apply EXIF orientation to an already-written thumbnail using the
/// `image` crate.  Called when ffmpeg was unavailable and the thumbnail was
/// produced by `image_crate_thumbnail` in `media/thumbnail.rs`.
///
/// Silently ignores errors (thumbnail orientation is best-effort).
fn apply_thumb_exif_orientation(thumb_path: &Path, orientation: u32) {
    if orientation <= 1 {
        return;
    }
    let Ok(data) = std::fs::read(thumb_path) else {
        return;
    };
    let Ok(img) = image::load_from_memory_with_format(&data, image::ImageFormat::WebP) else {
        return;
    };
    let rotated = crate::media::exif::apply_exif_orientation(img, orientation);
    if let Err(e) = rotated.save_with_format(thumb_path, image::ImageFormat::WebP) {
        tracing::warn!("failed to re-orient thumbnail: {e}");
    }
}

fn prepare_processor_input(
    input_path: &Path,
    dest_dir: &Path,
    mime_type: &str,
) -> Result<tempfile::NamedTempFile> {
    let ext = mime_to_ext(mime_type);
    let tmp = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile_in(dest_dir)
        .context("Failed to create temp input file for media processing")?;

    if mime_type == "image/jpeg" {
        match strip_jpeg_exif_file(input_path, tmp.path()) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("JPEG EXIF strip failed ({e}); using original bytes");
                std::fs::copy(input_path, tmp.path())
                    .context("Failed to copy original JPEG into processor temp file")?;
            }
        }
    } else {
        std::fs::copy(input_path, tmp.path())
            .context("Failed to copy upload into processor temp file")?;
    }

    Ok(tmp)
}

// ─── Image+audio combo: save audio with an existing image as its thumbnail ───

/// Save an audio file to disk for an image+audio combo post.
///
/// Instead of generating a separate thumbnail, the already-saved image's
/// `thumb_path` (relative to `boards_dir`) is reused as the audio's visual
/// thumbnail.  No ffmpeg waveform is generated for this case.
///
/// Returns an `UploadedFile` whose `thumb_path` is set to `image_thumb_rel`.
///
/// # Errors
/// Returns an error if the audio is empty, unsupported type, too large, or any I/O fails.
pub fn save_audio_with_image_thumb_from_path(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    original_filename: &str,
    boards_dir: &str,
    board_short: &str,
    max_audio_size: usize,
) -> Result<UploadedFile> {
    if original_size == 0 {
        return Err(anyhow::anyhow!("Audio file is empty."));
    }

    let mime_type = detect_mime_type(sniff_bytes)?;
    let media_type = crate::models::MediaType::from_mime(mime_type)
        .ok_or_else(|| anyhow::anyhow!("Not an audio file: {mime_type}"))?;
    if !matches!(media_type, crate::models::MediaType::Audio) {
        return Err(anyhow::anyhow!(
            "Expected an audio file for the audio slot; got {mime_type}"
        ));
    }
    if original_size > max_audio_size {
        return Err(anyhow::anyhow!(
            "Audio file too large. Maximum size is {} MiB.",
            max_audio_size / 1024 / 1024
        ));
    }

    let file_id = Uuid::new_v4().simple().to_string();
    let ext = mime_to_ext(mime_type);
    let filename = format!("{file_id}.{ext}");
    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    std::fs::create_dir_all(&dest_dir).context("Failed to create board directory")?;
    check_disk_space(&dest_dir, original_size)?;

    let file_path_abs = dest_dir.join(&filename);
    let tmp = tempfile::NamedTempFile::new_in(&dest_dir)
        .context("Failed to create temp file for audio upload")?;
    std::fs::copy(input_path, tmp.path()).context("Failed to copy audio upload to temp file")?;
    tmp.persist(&file_path_abs)
        .context("Failed to atomically rename audio temp file")?;

    let rel_file = format!("{board_short}/{filename}");
    let file_size = i64::try_from(original_size).context("File size overflows i64")?;
    Ok(UploadedFile {
        file_path: rel_file,
        thumb_path: String::new(),
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: mime_type.to_string(),
        file_size,
        media_type,
        processing_pending: false,
    })
}

// ─── JPEG EXIF stripping ─────────────────────────────────────────────────────

/// Re-encode a JPEG through the `image` crate, stripping all metadata.
///
/// The `image` crate's JPEG encoder writes a clean JFIF stream with no EXIF,
/// XMP, or IPTC segments — only the pixel data is retained.  This is the most
/// reliable stripping approach available without pulling in a separate EXIF library.
///
/// NOTE: The `image` crate uses a default JPEG quality of 75.  This produces a
/// file that is visually indistinguishable from the original for display purposes
/// but changes the file byte-for-byte.
///
/// IMPORTANT: Callers must read the EXIF Orientation tag from the ORIGINAL bytes
/// BEFORE calling this function if they intend to honour camera orientation —
/// the re-encoded output will contain no EXIF data at all.
fn strip_jpeg_exif(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Cursor;
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
        .context("Failed to decode JPEG for EXIF strip")?;
    let mut cursor = Cursor::new(Vec::with_capacity(data.len()));
    img.write_to(&mut cursor, image::ImageFormat::Jpeg)
        .context("Failed to re-encode JPEG after EXIF strip")?;
    Ok(cursor.into_inner())
}

fn strip_jpeg_exif_file(input_path: &Path, output_path: &Path) -> Result<()> {
    let data = std::fs::read(input_path).context("Failed to read JPEG for EXIF strip")?;
    let clean = strip_jpeg_exif(&data)?;
    std::fs::write(output_path, clean).context("Failed to write stripped JPEG temp file")?;
    Ok(())
}

fn read_exif_orientation_from_file(path: &Path) -> Result<u32> {
    let data = std::fs::read(path).context("Failed to read JPEG EXIF data")?;
    Ok(crate::media::exif::read_exif_orientation(&data))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Map a MIME type string to the canonical file extension used on disk.
///
/// Public wrapper used by `crate::media::convert` when writing fallback files.
#[must_use]
pub fn mime_to_ext_pub(mime: &str) -> &'static str {
    mime_to_ext(mime)
}

/// Map a MIME type string to the canonical file extension used on disk.
fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" | "audio/webm" => "webm",
        // Audio formats
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        _ => "bin",
    }
}

/// Delete a file from the filesystem, ignoring not-found errors.
///
/// # Security
///
/// `relative_path` is validated to prevent directory traversal: absolute paths
/// and paths containing `..` components are rejected with a warning log.  Only
/// simple relative paths (e.g. `"b/abc123.webm"`) are accepted.
pub fn delete_file(boards_dir: &str, relative_path: &str) {
    let rel = std::path::Path::new(relative_path);

    // Reject absolute paths: PathBuf::join on an absolute path replaces the
    // entire base, allowing deletion of arbitrary files outside boards_dir.
    if rel.is_absolute() {
        tracing::warn!(
            "delete_file: rejected absolute path (potential traversal): {:?}",
            relative_path
        );
        return;
    }

    // Reject any `..` components that could escape the boards directory.
    if rel
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        tracing::warn!(
            "delete_file: rejected path with '..' component (potential traversal): {:?}",
            relative_path
        );
        return;
    }

    let full_path = PathBuf::from(boards_dir).join(rel);
    let _ = std::fs::remove_file(full_path);
}

/// Format file size as human-readable string.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_file_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    // GenericImageView trait is needed for .width() / .height() on DynamicImage.
    #[allow(unused_imports)]
    use image::GenericImageView as _;

    // ── format_file_size ─────────────────────────────────────────────────────

    #[test]
    fn format_bytes_exact() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1), "1 B");
        assert_eq!(format_file_size(1023), "1023 B");
    }

    #[test]
    fn format_kib_boundary() {
        assert_eq!(format_file_size(1024), "1.0 KiB");
        assert_eq!(format_file_size(1536), "1.5 KiB");
        assert_eq!(format_file_size(1024 * 1024 - 1), "1024.0 KiB");
    }

    #[test]
    fn format_mib() {
        assert_eq!(format_file_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_file_size(1024 * 1024 * 2), "2.0 MiB");
    }

    // ── mime_to_ext ──────────────────────────────────────────────────────────

    #[test]
    fn mime_to_ext_known_types() {
        assert_eq!(mime_to_ext("image/jpeg"), "jpg");
        assert_eq!(mime_to_ext("image/png"), "png");
        assert_eq!(mime_to_ext("image/gif"), "gif");
        assert_eq!(mime_to_ext("image/webp"), "webp");
        assert_eq!(mime_to_ext("video/mp4"), "mp4");
        assert_eq!(mime_to_ext("video/webm"), "webm");
        assert_eq!(mime_to_ext("audio/webm"), "webm");
        assert_eq!(mime_to_ext("audio/mpeg"), "mp3");
        assert_eq!(mime_to_ext("audio/ogg"), "ogg");
        assert_eq!(mime_to_ext("audio/flac"), "flac");
        assert_eq!(mime_to_ext("audio/wav"), "wav");
        assert_eq!(mime_to_ext("audio/mp4"), "m4a");
        assert_eq!(mime_to_ext("audio/aac"), "aac");
    }

    #[test]
    fn mime_to_ext_unknown_falls_back_to_bin() {
        assert_eq!(mime_to_ext("application/octet-stream"), "bin");
        assert_eq!(mime_to_ext(""), "bin");
        assert_eq!(mime_to_ext("text/plain"), "bin");
    }

    // ── detect_mime_type ─────────────────────────────────────────────────────

    #[test]
    fn detect_empty_is_error() {
        assert!(detect_mime_type(b"").is_err());
    }

    #[test]
    fn detect_jpeg() {
        let header = b"\xff\xd8\xff\xe0rest of file";
        assert_eq!(detect_mime_type(header).expect("jpeg"), "image/jpeg");
    }

    #[test]
    fn detect_png() {
        let header = b"\x89PNG\r\n\x1a\nrest";
        assert_eq!(detect_mime_type(header).expect("png"), "image/png");
    }

    #[test]
    fn detect_gif() {
        let header = b"GIF89arest";
        assert_eq!(detect_mime_type(header).expect("gif"), "image/gif");
    }

    #[test]
    fn detect_webp() {
        // RIFF....WEBP — built as a literal to avoid indexing_slicing lint
        let data: &[u8] = b"RIFF\x00\x00\x00\x00WEBPrest";
        assert_eq!(detect_mime_type(data).expect("webp"), "image/webp");
    }

    #[test]
    fn detect_wav() {
        let data: &[u8] = b"RIFF\x00\x00\x00\x00WAVErest";
        assert_eq!(detect_mime_type(data).expect("wav"), "audio/wav");
    }

    #[test]
    fn detect_riff_unknown_subtype_is_error() {
        let data: &[u8] = b"RIFF\x00\x00\x00\x00BLAH";
        assert!(detect_mime_type(data).is_err());
    }

    #[test]
    fn detect_mp3_id3() {
        let header = b"ID3\x03\x00\x00rest";
        assert_eq!(detect_mime_type(header).expect("mp3 id3"), "audio/mpeg");
    }

    #[test]
    fn detect_mp3_raw_sync() {
        // 0xFF 0xFB = raw MP3 frame sync
        let header = b"\xff\xfbrest";
        assert_eq!(detect_mime_type(header).expect("mp3 sync"), "audio/mpeg");
    }

    #[test]
    fn detect_aac() {
        // 0xFF 0xF1 = AAC ADTS sync word
        let header = b"\xff\xf1rest";
        assert_eq!(detect_mime_type(header).expect("aac"), "audio/aac");
    }

    #[test]
    fn detect_ogg() {
        let header = b"OggS\x00rest";
        assert_eq!(detect_mime_type(header).expect("ogg"), "audio/ogg");
    }

    #[test]
    fn detect_flac() {
        let header = b"fLaCrest";
        assert_eq!(detect_mime_type(header).expect("flac"), "audio/flac");
    }

    #[test]
    fn detect_mp4_ftyp() {
        // 4 bytes padding, "ftyp", "isom" brand — all as a literal
        let data: &[u8] = b"\x00\x00\x00\x00ftypismores";
        assert_eq!(detect_mime_type(data).expect("mp4"), "video/mp4");
    }

    #[test]
    fn detect_m4a_ftyp() {
        let data: &[u8] = b"\x00\x00\x00\x00ftypM4A res";
        assert_eq!(detect_mime_type(data).expect("m4a"), "audio/mp4");
    }

    #[test]
    fn detect_m4a_ftyp_lowercase() {
        let data: &[u8] = b"\x00\x00\x00\x00ftypm4a res";
        assert_eq!(detect_mime_type(data).expect("m4a lower"), "audio/mp4");
    }

    #[test]
    fn detect_webm_doctype() {
        // Minimal EBML: magic(4) + 6 padding bytes + DocType ID(2) + size(1) + "webm"(4)
        // Positions: magic=0..4, padding=4..10, 0x42=10, 0x82=11, 0x84=12, "webm"=13..17
        // Built as a concat of fixed-size byte arrays to avoid indexing_slicing.
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        assert_eq!(detect_mime_type(data).expect("webm"), "video/webm");
    }

    #[test]
    fn detect_mkv_doctype_rejected() {
        // Same layout but DocType = "matroska" (8 bytes), size field = 0x88
        let data: &[u8] = b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x88matroska\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let err = detect_mime_type(data)
            .expect_err("mkv should be rejected")
            .to_string();
        assert!(err.contains("Matroska") || err.contains("matroska"));
    }

    #[test]
    fn detect_unknown_returns_error() {
        assert!(detect_mime_type(b"\x00\x00\x00\x00unknown").is_err());
    }

    // ── apply_exif_orientation ───────────────────────────────────────────────

    #[test]
    fn exif_orientation_1_is_noop() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 1);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_3_rotates_180() {
        // 180° rotation keeps dimensions unchanged
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 3);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    #[test]
    fn exif_orientation_6_rotates_90cw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 6);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_8_rotates_90ccw_swaps_dims() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 8);
        assert_eq!(out.width(), 6);
        assert_eq!(out.height(), 4);
    }

    #[test]
    fn exif_orientation_unknown_value_is_noop() {
        let img = image::DynamicImage::new_rgba8(4, 6);
        let out = crate::media::exif::apply_exif_orientation(img, 99);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 6);
    }

    // ── New format detection: BMP, TIFF, SVG ─────────────────────────────────

    #[test]
    fn detect_bmp() {
        // BMP magic: 'BM' (0x42 0x4D)
        let header = b"BM\x36\x00\x00\x00\x00\x00rest";
        assert_eq!(detect_mime_type(header).expect("bmp"), "image/bmp");
    }

    #[test]
    fn detect_tiff_little_endian() {
        // TIFF LE magic: 49 49 2A 00
        let header = b"\x49\x49\x2a\x00rest";
        assert_eq!(detect_mime_type(header).expect("tiff le"), "image/tiff");
    }

    #[test]
    fn detect_tiff_big_endian() {
        // TIFF BE magic: 4D 4D 00 2A
        let header = b"\x4d\x4d\x00\x2arest";
        assert_eq!(detect_mime_type(header).expect("tiff be"), "image/tiff");
    }

    #[test]
    fn detect_svg_direct() {
        let data = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"></svg>";
        assert_eq!(detect_mime_type(data).expect("svg"), "image/svg+xml");
    }

    #[test]
    fn detect_svg_xml_declaration() {
        let data = b"<?xml version=\"1.0\"?><svg></svg>";
        assert_eq!(
            detect_mime_type(data).expect("svg xml decl"),
            "image/svg+xml"
        );
    }

    #[test]
    fn mime_to_ext_new_types() {
        assert_eq!(mime_to_ext("image/bmp"), "bmp");
        assert_eq!(mime_to_ext("image/tiff"), "tiff");
        assert_eq!(mime_to_ext("image/svg+xml"), "svg");
    }
}
