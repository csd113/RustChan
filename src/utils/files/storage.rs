// src/utils/files/storage.rs

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
    pub dedup_reused: bool,
}

pub struct SaveUploadOptions<'a> {
    pub original_filename: &'a str,
    pub boards_dir: &'a str,
    pub board_short: &'a str,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub ffmpeg_available: bool,
    pub ffmpeg_webp_available: bool,
    pub allow_any_files: bool,
}

struct UploadPlan {
    mime_type: String,
    media_type: crate::models::MediaType,
    jpeg_orientation: u32,
    processing_pending: bool,
    dest_dir: PathBuf,
    thumbs_dir: PathBuf,
}

/// Save and process a primary upload from an already-streamed temporary file.
///
/// # Errors
/// Returns an error if MIME detection, policy validation, media processing,
/// disk-space checks, or the final filesystem write fails.
pub fn save_upload_from_path(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    options: &SaveUploadOptions<'_>,
) -> Result<UploadedFile> {
    if original_size == 0 {
        return Err(anyhow::anyhow!("File is empty."));
    }
    let plan = build_upload_plan(input_path, sniff_bytes, original_size, options)?;
    let file_id = Uuid::new_v4().simple().to_string();

    if plan.media_type == crate::models::MediaType::Other {
        return save_generic_upload(input_path, original_size, options, &plan, &file_id);
    }

    save_processed_upload(input_path, options, &plan, &file_id)
}

/// Save a secondary audio upload for an image+audio combo post.
///
/// # Errors
/// Returns an error if the audio MIME check fails, the file exceeds the board
/// limit, disk-space checks fail, or the file cannot be persisted.
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
    let media_type = crate::models::MediaType::from_mime(mime_type);
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
        dedup_reused: false,
    })
}

#[must_use]
pub fn mime_to_ext_pub(mime: &str) -> &'static str {
    mime_to_ext(mime)
}

pub fn delete_file(boards_dir: &str, relative_path: &str) {
    if let Err(error) = delete_file_checked(boards_dir, relative_path) {
        tracing::warn!("delete_file failed for {relative_path}: {error}");
    }
}

/// Remove a stored upload path while rejecting traversal attempts.
///
/// # Errors
/// Returns an error if the path is suspicious or the underlying filesystem
/// removal fails for a reason other than the file already being absent.
pub fn delete_file_checked(boards_dir: &str, relative_path: &str) -> Result<()> {
    let rel = std::path::Path::new(relative_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        anyhow::bail!(
            "delete_file: rejected suspicious path (potential traversal): {relative_path:?}"
        );
    }

    let path = PathBuf::from(boards_dir).join(rel);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to remove {}", path.display())),
    }
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

fn build_upload_plan(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    options: &SaveUploadOptions<'_>,
) -> Result<UploadPlan> {
    let mime_type = match detect_mime_type(sniff_bytes) {
        Ok(mime) => mime.to_string(),
        Err(_) if options.allow_any_files => super::fallback_download_mime_type().to_string(),
        Err(error) => return Err(error),
    };
    if mime_type == "image/svg+xml" {
        anyhow::bail!(
            "File type not allowed. SVG files are not accepted because they can contain executable JavaScript."
        );
    }

    let media_type = crate::models::MediaType::from_mime(&mime_type);
    let max_size = max_size_for_media(media_type, options);
    if original_size > max_size {
        anyhow::bail!(
            "File too large. Maximum {} size is {} MiB.",
            media_label(media_type),
            max_size / 1024 / 1024
        );
    }

    let jpeg_orientation = if mime_type == "image/jpeg" {
        read_exif_orientation_from_file(input_path)?
    } else {
        1
    };

    let dest_dir = PathBuf::from(options.boards_dir).join(options.board_short);
    let thumbs_dir = dest_dir.join("thumbs");
    std::fs::create_dir_all(&dest_dir).context("Failed to create board directory")?;
    if media_type != crate::models::MediaType::Other {
        std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;
    }
    check_disk_space(&dest_dir, original_size)?;
    let processing_pending = options.ffmpeg_available
        && matches!(
            media_type,
            crate::models::MediaType::Video | crate::models::MediaType::Audio
        )
        && (media_type != crate::models::MediaType::Video
            || mime_type == "video/mp4"
            || mime_type == "video/webm");

    Ok(UploadPlan {
        mime_type,
        media_type,
        jpeg_orientation,
        processing_pending,
        dest_dir,
        thumbs_dir,
    })
}

fn save_generic_upload(
    input_path: &Path,
    original_size: usize,
    options: &SaveUploadOptions<'_>,
    plan: &UploadPlan,
    file_id: &str,
) -> Result<UploadedFile> {
    let ext = arbitrary_file_ext(options.original_filename);
    let filename = format!("{file_id}.{ext}");
    let file_path_abs = plan.dest_dir.join(&filename);
    let tmp = tempfile::NamedTempFile::new_in(&plan.dest_dir)
        .context("Failed to create temp file for generic upload")?;
    std::fs::copy(input_path, tmp.path()).context("Failed to copy generic upload to temp file")?;
    tmp.persist(&file_path_abs)
        .context("Failed to atomically rename generic upload temp file")?;

    Ok(UploadedFile {
        file_path: format!("{}/{filename}", options.board_short),
        thumb_path: String::new(),
        original_name: crate::utils::sanitize::sanitize_filename(options.original_filename),
        mime_type: plan.mime_type.clone(),
        file_size: i64::try_from(original_size).context("File size overflows i64")?,
        media_type: plan.media_type,
        processing_pending: false,
        dedup_reused: false,
    })
}

fn save_processed_upload(
    input_path: &Path,
    options: &SaveUploadOptions<'_>,
    plan: &UploadPlan,
    file_id: &str,
) -> Result<UploadedFile> {
    let processor_input = prepare_processor_input(input_path, &plan.dest_dir, &plan.mime_type)?;
    let processor = crate::media::MediaProcessor::new_with_ffmpeg_caps(
        options.ffmpeg_available,
        options.ffmpeg_webp_available,
    );
    let processed = processor
        .process_upload(
            processor_input.path(),
            &plan.mime_type,
            &plan.dest_dir,
            file_id,
            &plan.thumbs_dir,
            options.thumb_size,
        )
        .context("Media processing pipeline failed")?;

    if plan.jpeg_orientation > 1 && processed.file_path.exists() {
        apply_image_exif_orientation(&processed.file_path, plan.jpeg_orientation);
    }

    if plan.jpeg_orientation > 1
        && processed.thumbnail_path.exists()
        && processed
            .thumbnail_path
            .extension()
            .and_then(|ext| ext.to_str())
            == Some("webp")
    {
        apply_thumb_exif_orientation(&processed.thumbnail_path, plan.jpeg_orientation);
    }

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
        file_path: format!("{}/{filename}", options.board_short),
        thumb_path: format!("{}/thumbs/{thumb_filename}", options.board_short),
        original_name: crate::utils::sanitize::sanitize_filename(options.original_filename),
        mime_type: processed.mime_type.clone(),
        file_size: i64::try_from(processed.final_size).context("File size overflows i64")?,
        media_type: crate::models::MediaType::from_mime(&processed.mime_type),
        processing_pending: if processed.was_converted {
            false
        } else {
            plan.processing_pending
        },
        dedup_reused: false,
    })
}

fn max_size_for_media(
    media_type: crate::models::MediaType,
    options: &SaveUploadOptions<'_>,
) -> usize {
    match media_type {
        crate::models::MediaType::Video => options.max_video_size,
        crate::models::MediaType::Audio => options.max_audio_size,
        crate::models::MediaType::Image => options.max_image_size,
        crate::models::MediaType::Other => options
            .max_image_size
            .max(options.max_video_size)
            .max(options.max_audio_size),
    }
}

const fn media_label(media_type: crate::models::MediaType) -> &'static str {
    match media_type {
        crate::models::MediaType::Video => "video",
        crate::models::MediaType::Audio => "audio",
        crate::models::MediaType::Image => "image",
        crate::models::MediaType::Other => "file",
    }
}

fn arbitrary_file_ext(original_filename: &str) -> String {
    std::path::Path::new(original_filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            ext.chars()
                .filter(char::is_ascii_alphanumeric)
                .take(16)
                .collect::<String>()
        })
        .filter(|ext| !ext.is_empty())
        .map_or_else(|| "bin".to_string(), |ext| ext.to_ascii_lowercase())
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

fn apply_image_exif_orientation(image_path: &Path, orientation: u32) {
    if orientation <= 1 {
        return;
    }

    let Ok(data) = std::fs::read(image_path) else {
        return;
    };
    let Ok(format) = image::guess_format(&data) else {
        return;
    };
    let Ok(img) = image::load_from_memory_with_format(&data, format) else {
        return;
    };
    let rotated = crate::media::exif::apply_exif_orientation(img, orientation);
    if let Err(error) = rotated.save_with_format(image_path, format) {
        tracing::warn!("failed to re-orient stored image: {error}");
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

#[cfg(test)]
mod tests {
    use super::save_audio_with_image_thumb_from_path;

    #[test]
    fn combo_flac_audio_is_saved_losslessly_without_pending_processing() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let board_dir = tempdir.path().join("test");
        std::fs::create_dir_all(&board_dir).expect("create board dir");

        let input = tempfile::Builder::new()
            .suffix(".flac")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let flac_bytes = b"fLaC\x00\x00\x00\x22test flac bytes";
        std::fs::write(input.path(), flac_bytes).expect("write flac");

        let uploaded = save_audio_with_image_thumb_from_path(
            input.path(),
            flac_bytes,
            flac_bytes.len(),
            "track.flac",
            tempdir.path().to_str().expect("utf8 path"),
            "test",
            1024 * 1024,
        )
        .expect("save flac");

        assert_eq!(uploaded.mime_type, "audio/flac");
        assert_eq!(uploaded.file_path.split('.').next_back(), Some("flac"));
        assert!(!uploaded.processing_pending);

        let stored_bytes =
            std::fs::read(tempdir.path().join(&uploaded.file_path)).expect("read stored flac");
        assert_eq!(stored_bytes, flac_bytes);
    }
}
