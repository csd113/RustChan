use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::disk_space::check_disk_space;
use super::jpeg::{read_exif_orientation_from_file, strip_jpeg_exif_file};
use super::mime::detect_mime_type;

pub struct UploadedFile {
    pub file_path: String,
    pub thumb_path: String,
    pub original_name: String,
    pub mime_type: String,
    pub file_size: i64,
    pub media_type: crate::models::MediaType,
    pub processing_pending: bool,
}

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
            .and_then(|ext| ext.to_str())
            == Some("webp")
    {
        apply_thumb_exif_orientation(&processed.thumbnail_path, jpeg_orientation);
    }

    let final_mime = processed.mime_type.clone();
    let final_media_type = crate::models::MediaType::from_mime(&final_mime).unwrap_or(media_type);
    let file_size = i64::try_from(processed.final_size).context("File size overflows i64")?;

    let filename = processed
        .file_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Converted file has non-UTF-8 name")?;
    let thumb_filename = processed
        .thumbnail_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Thumbnail file has non-UTF-8 name")?;

    Ok(UploadedFile {
        file_path: format!("{board_short}/{filename}"),
        thumb_path: format!("{board_short}/thumbs/{thumb_filename}"),
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: final_mime,
        file_size,
        media_type: final_media_type,
        processing_pending: if processed.was_converted {
            false
        } else {
            processing_pending
        },
    })
}

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

    Ok(UploadedFile {
        file_path: format!("{board_short}/{filename}"),
        thumb_path: String::new(),
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: mime_type.to_string(),
        file_size: i64::try_from(original_size).context("File size overflows i64")?,
        media_type,
        processing_pending: false,
    })
}

#[must_use]
pub fn mime_to_ext_pub(mime: &str) -> &'static str {
    mime_to_ext(mime)
}

pub fn delete_file(boards_dir: &str, relative_path: &str) {
    let rel = std::path::Path::new(relative_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        tracing::warn!(
            "delete_file: rejected suspicious path (potential traversal): {:?}",
            relative_path
        );
        return;
    }

    let _ = std::fs::remove_file(PathBuf::from(boards_dir).join(rel));
}

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
            Err(error) => {
                tracing::warn!("JPEG EXIF strip failed ({error}); using original bytes");
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
    if let Err(error) = rotated.save_with_format(thumb_path, image::ImageFormat::WebP) {
        tracing::warn!("failed to re-orient thumbnail: {error}");
    }
}

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
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        _ => "bin",
    }
}
