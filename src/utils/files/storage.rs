// src/utils/files/storage.rs

use anyhow::{Context as _, Result};
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

#[expect(clippy::struct_excessive_bools)]
// These booleans are independent upload policy and media capability flags.
pub struct SaveUploadOptions<'a> {
    pub original_filename: &'a str,
    pub boards_dir: &'a str,
    pub board_short: &'a str,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub ffmpeg_available: bool,
    pub ffprobe_available: bool,
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

const MAX_UPLOAD_IMAGE_PIXELS: u64 = 100_000_000;

/// Classify an uploaded file into the MIME type `RustChan` should persist.
///
/// # Errors
/// Returns an error if MIME sniffing fails and arbitrary file uploads are not
/// allowed, or if `ffprobe` probing fails in a way that must be surfaced.
pub fn classify_upload_mime(
    input_path: &Path,
    sniff_bytes: &[u8],
    ffprobe_available: bool,
    allow_any_files: bool,
) -> Result<String> {
    let detected = match detect_mime_type(sniff_bytes) {
        Ok(mime) => mime.to_owned(),
        Err(_) if allow_any_files => super::fallback_download_mime_type().to_owned(),
        Err(error) => return Err(error),
    };

    refine_probe_mime(input_path, &detected, ffprobe_available)
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
    let validated = validate_upload(input_path, sniff_bytes, original_size, options)?;
    let plan = build_upload_plan(validated, original_size, options)?;
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
#[expect(clippy::too_many_arguments)]
pub fn save_audio_with_image_thumb_from_path(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    original_filename: &str,
    boards_dir: &str,
    board_short: &str,
    max_audio_size: usize,
    ffprobe_available: bool,
) -> Result<UploadedFile> {
    if original_size == 0 {
        return Err(anyhow::anyhow!("Audio file is empty."));
    }

    let mime_type = classify_upload_mime(input_path, sniff_bytes, ffprobe_available, false)?;
    let media_type = crate::models::MediaType::from_mime(&mime_type);
    if !matches!(media_type, crate::models::MediaType::Audio) {
        return Err(anyhow::anyhow!(
            "Expected an audio file for the audio slot; got {mime_type}"
        ));
    }
    if original_size > max_audio_size {
        return Err(anyhow::anyhow!(
            "Audio file too large. Maximum audio upload size is {}.",
            format_upload_limit(max_audio_size)
        ));
    }
    validate_av_stream_kind(
        input_path,
        &mime_type,
        crate::models::MediaType::Audio,
        ffprobe_available,
    )?;

    let file_id = Uuid::new_v4().simple().to_string();
    let ext = mime_to_ext(&mime_type);
    let filename = format!("{file_id}.{ext}");
    let dest_dir = PathBuf::from(boards_dir).join(board_short);
    std::fs::create_dir_all(&dest_dir).context("Failed to create board directory")?;
    crate::utils::fs_security::assert_dir_no_symlink(&dest_dir)
        .context("Upload board directory failed safety validation")?;
    check_disk_space(&dest_dir, original_size)?;

    let file_path_abs = dest_dir.join(&filename);
    crate::utils::fs_security::canonical_parent_for_new_child(
        Path::new(boards_dir),
        &file_path_abs,
    )
    .context("Upload destination failed safety validation")?;
    let tmp = tempfile::NamedTempFile::new_in(&dest_dir)
        .context("Failed to create temp file for audio upload")?;
    std::fs::copy(input_path, tmp.path()).context("Failed to copy audio upload to temp file")?;
    tmp.persist(&file_path_abs)
        .context("Failed to atomically rename audio temp file")?;

    Ok(UploadedFile {
        file_path: format!("{board_short}/{filename}"),
        thumb_path: String::new(),
        original_name: crate::utils::sanitize::sanitize_filename(original_filename),
        mime_type: mime_type.clone(),
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

/// Remove a stored upload path while rejecting traversal attempts.
///
/// # Errors
/// Returns an error if the path is suspicious or the underlying filesystem
/// removal fails for a reason other than the file already being absent.
pub fn delete_file_checked(boards_dir: &str, relative_path: &str) -> Result<()> {
    let path = match crate::utils::fs_security::existing_regular_file_child(
        Path::new(boards_dir),
        relative_path,
    ) {
        Ok(path) => path,
        Err(error) if is_not_found_error(&error) => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("delete_file: rejected unsafe runtime path {relative_path:?}")
            });
        }
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[must_use]
// This cast is a local display or math conversion, and the values are already bounded by surrounding invariants.
#[expect(clippy::cast_precision_loss)]
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
    validated: ValidatedUpload,
    original_size: usize,
    options: &SaveUploadOptions<'_>,
) -> Result<UploadPlan> {
    let dest_dir = PathBuf::from(options.boards_dir).join(options.board_short);
    let thumbs_dir = dest_dir.join("thumbs");
    std::fs::create_dir_all(&dest_dir).context("Failed to create board directory")?;
    crate::utils::fs_security::assert_dir_no_symlink(&dest_dir)
        .context("Upload board directory failed safety validation")?;
    if validated.media_type != crate::models::MediaType::Other {
        std::fs::create_dir_all(&thumbs_dir).context("Failed to create board thumbs directory")?;
        crate::utils::fs_security::assert_dir_no_symlink(&thumbs_dir)
            .context("Upload thumbnail directory failed safety validation")?;
    }
    check_disk_space(&dest_dir, original_size)?;
    let processing_pending = options.ffmpeg_available
        && matches!(
            validated.media_type,
            crate::models::MediaType::Video | crate::models::MediaType::Audio
        )
        && (validated.media_type != crate::models::MediaType::Video
            || validated.mime_type == "video/mp4"
            || validated.mime_type == "video/webm"
            || is_matroska_mime(&validated.mime_type));

    Ok(UploadPlan {
        mime_type: validated.mime_type,
        media_type: validated.media_type,
        jpeg_orientation: validated.jpeg_orientation,
        processing_pending,
        dest_dir,
        thumbs_dir,
    })
}

struct ValidatedUpload {
    mime_type: String,
    media_type: crate::models::MediaType,
    jpeg_orientation: u32,
}

/// Validate an upload against media policy before deduplication or persistence.
///
/// # Errors
/// Returns an error when MIME sniffing, type policy, size checks, or image
/// decoding validation fail.
pub fn validate_upload_from_path(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    options: &SaveUploadOptions<'_>,
) -> Result<()> {
    validate_upload(input_path, sniff_bytes, original_size, options).map(|_| ())
}

fn validate_upload(
    input_path: &Path,
    sniff_bytes: &[u8],
    original_size: usize,
    options: &SaveUploadOptions<'_>,
) -> Result<ValidatedUpload> {
    let mime_type = classify_upload_mime(
        input_path,
        sniff_bytes,
        options.ffprobe_available,
        options.allow_any_files,
    )?;
    if mime_type == "image/svg+xml" {
        anyhow::bail!(
            "File type not allowed. SVG files are not accepted because they can contain executable JavaScript."
        );
    }

    let media_type = crate::models::MediaType::from_mime(&mime_type);
    let max_size = max_size_for_media(media_type, options);
    if original_size > max_size {
        anyhow::bail!(
            "File too large. Maximum {} upload size is {}.",
            media_label(media_type),
            format_upload_limit(max_size)
        );
    }
    if media_type == crate::models::MediaType::Image {
        validate_decodable_image(input_path, &mime_type)?;
    } else if media_type == crate::models::MediaType::Pdf {
        validate_pdf_structure(input_path)?;
    } else if matches!(
        media_type,
        crate::models::MediaType::Audio | crate::models::MediaType::Video
    ) {
        validate_av_stream_kind(
            input_path,
            &mime_type,
            media_type,
            options.ffprobe_available,
        )?;
    }

    let jpeg_orientation = if mime_type == "image/jpeg" {
        read_exif_orientation_from_file(input_path)?
    } else {
        1
    };

    Ok(ValidatedUpload {
        mime_type,
        media_type,
        jpeg_orientation,
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
    crate::utils::fs_security::canonical_parent_for_new_child(
        Path::new(options.boards_dir),
        &file_path_abs,
    )
    .context("Generic upload destination failed safety validation")?;
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

    let final_size = final_processed_size_within_limit(&processed, options)?;

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
        file_size: i64::try_from(final_size).context("File size overflows i64")?,
        media_type: crate::models::MediaType::from_mime(&processed.mime_type),
        processing_pending: if processed.was_converted {
            false
        } else {
            plan.processing_pending
        },
        dedup_reused: false,
    })
}

fn final_processed_size_within_limit(
    processed: &crate::media::ProcessedMedia,
    options: &SaveUploadOptions<'_>,
) -> Result<u64> {
    let media_type = crate::models::MediaType::from_mime(&processed.mime_type);
    let max_size = max_size_for_media(media_type, options);
    let final_size = std::fs::metadata(&processed.file_path)
        .with_context(|| {
            format!(
                "Failed to stat stored upload {}",
                processed.file_path.display()
            )
        })?
        .len();
    if final_size != processed.final_size {
        tracing::debug!(
            before_orientation_bytes = processed.final_size,
            final_bytes = final_size,
            "stored upload size changed after post-processing"
        );
    }

    if final_size <= u64::try_from(max_size).unwrap_or(u64::MAX) {
        return Ok(final_size);
    }

    cleanup_processed_outputs(processed);
    anyhow::bail!(
        "File too large after media processing. Maximum {} upload size is {}; processed file is {}.",
        media_label(media_type),
        format_upload_limit(max_size),
        format_upload_limit_u64(final_size)
    );
}

fn cleanup_processed_outputs(processed: &crate::media::ProcessedMedia) {
    for path in [&processed.file_path, &processed.thumbnail_path] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %error,
                    "failed to remove oversized processed upload output"
                );
            }
        }
    }
}

fn format_upload_limit(max_bytes: usize) -> String {
    format_upload_limit_u64(u64::try_from(max_bytes).unwrap_or(u64::MAX))
}

fn format_upload_limit_u64(max_bytes: u64) -> String {
    let display_bytes = i64::try_from(max_bytes).unwrap_or(i64::MAX);
    format_file_size(display_bytes)
}

fn max_size_for_media(
    media_type: crate::models::MediaType,
    options: &SaveUploadOptions<'_>,
) -> usize {
    match media_type {
        crate::models::MediaType::Video => options.max_video_size,
        crate::models::MediaType::Audio => options.max_audio_size,
        crate::models::MediaType::Image => options.max_image_size,
        crate::models::MediaType::Pdf | crate::models::MediaType::Other => options
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
        crate::models::MediaType::Pdf => "PDF",
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
        .map_or_else(|| "bin".to_owned(), |ext| ext.to_ascii_lowercase())
}

fn validate_decodable_image(input_path: &Path, mime_type: &str) -> Result<()> {
    let Some(format) = mime_to_image_format(mime_type) else {
        return Ok(());
    };

    let data = std::fs::read(input_path).with_context(|| {
        format!(
            "Failed to read {} for image validation",
            input_path.display()
        )
    })?;
    if mime_type == "image/png" {
        validate_png_structure(&data)?;
    }
    let reader = image::ImageReader::with_format(std::io::Cursor::new(&data), format);
    let (width, height) = reader.into_dimensions().with_context(|| {
        format!("File appears to be {mime_type}, but its image header is malformed or incomplete.")
    })?;
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_UPLOAD_IMAGE_PIXELS {
        anyhow::bail!("Image dimensions {width}x{height} exceed the safety limit.");
    }

    image::load_from_memory_with_format(&data, format).with_context(|| {
        format!("File appears to be {mime_type}, but the image data could not be decoded.")
    })?;
    Ok(())
}

fn validate_pdf_structure(input_path: &Path) -> Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = std::fs::File::open(input_path)
        .with_context(|| format!("Failed to open {} for PDF validation", input_path.display()))?;

    let mut header = [0u8; 5];
    file.read_exact(&mut header)
        .with_context(|| format!("Failed to read PDF header from {}", input_path.display()))?;
    if header != *b"%PDF-" {
        anyhow::bail!("File appears to be application/pdf, but its header is malformed.");
    }

    let file_len = file
        .metadata()
        .with_context(|| format!("Inspect {} for PDF validation", input_path.display()))?
        .len();
    let tail_len_u64 = file_len.min(4096);
    let tail_len = usize::try_from(tail_len_u64).context("PDF tail length overflows usize")?;
    let tail_start = file_len.saturating_sub(tail_len_u64);
    file.seek(SeekFrom::Start(tail_start))
        .with_context(|| format!("Seek to PDF trailer window in {}", input_path.display()))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)
        .with_context(|| format!("Read PDF trailer window from {}", input_path.display()))?;
    if !tail.windows(5).any(|window| window == b"%%EOF") {
        anyhow::bail!("File appears to be application/pdf, but its trailer is missing.");
    }
    Ok(())
}

fn validate_png_structure(data: &[u8]) -> Result<()> {
    const MALFORMED_PNG_ERROR: &str =
        "File appears to be image/png, but its image header is malformed or incomplete.";
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < PNG_SIGNATURE.len() + 12 {
        anyhow::bail!(MALFORMED_PNG_ERROR);
    }
    if data.get(..PNG_SIGNATURE.len()) != Some(PNG_SIGNATURE.as_slice()) {
        anyhow::bail!(MALFORMED_PNG_ERROR);
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut saw_ihdr = false;

    while offset + 12 <= data.len() {
        let length_bytes: [u8; 4] = data
            .get(offset..offset + 4)
            .ok_or_else(|| anyhow::anyhow!(MALFORMED_PNG_ERROR))?
            .try_into()
            .map_err(|error| anyhow::anyhow!("{MALFORMED_PNG_ERROR}: {error}"))?;
        let length = u32::from_be_bytes(length_bytes) as usize;
        let chunk_type = data
            .get(offset + 4..offset + 8)
            .ok_or_else(|| anyhow::anyhow!(MALFORMED_PNG_ERROR))?;
        let chunk_data_start = offset + 8;
        let chunk_data_end = chunk_data_start.saturating_add(length);
        let crc_end = chunk_data_end.saturating_add(4);
        if crc_end > data.len() {
            anyhow::bail!(MALFORMED_PNG_ERROR);
        }

        if !saw_ihdr {
            if chunk_type != b"IHDR" || length != 13 {
                anyhow::bail!(MALFORMED_PNG_ERROR);
            }
            let width_bytes: [u8; 4] = data
                .get(chunk_data_start..chunk_data_start + 4)
                .ok_or_else(|| anyhow::anyhow!(MALFORMED_PNG_ERROR))?
                .try_into()
                .map_err(|error| anyhow::anyhow!("{MALFORMED_PNG_ERROR}: {error}"))?;
            let height_bytes: [u8; 4] = data
                .get(chunk_data_start + 4..chunk_data_start + 8)
                .ok_or_else(|| anyhow::anyhow!(MALFORMED_PNG_ERROR))?
                .try_into()
                .map_err(|error| anyhow::anyhow!("{MALFORMED_PNG_ERROR}: {error}"))?;
            let width = u32::from_be_bytes(width_bytes);
            let height = u32::from_be_bytes(height_bytes);
            if width == 0 || height == 0 {
                anyhow::bail!(MALFORMED_PNG_ERROR);
            }
            saw_ihdr = true;
        } else if chunk_type == b"IEND" {
            return Ok(());
        }

        offset = crc_end;
    }

    anyhow::bail!(MALFORMED_PNG_ERROR);
}

fn mime_to_image_format(mime_type: &str) -> Option<image::ImageFormat> {
    match mime_type {
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/png" => Some(image::ImageFormat::Png),
        "image/gif" => Some(image::ImageFormat::Gif),
        "image/webp" => Some(image::ImageFormat::WebP),
        "image/bmp" => Some(image::ImageFormat::Bmp),
        "image/tiff" => Some(image::ImageFormat::Tiff),
        _ => None,
    }
}

fn refine_probe_mime(input_path: &Path, detected: &str, ffprobe_available: bool) -> Result<String> {
    let media_type = crate::models::MediaType::from_mime(detected);
    let should_probe = matches!(
        media_type,
        crate::models::MediaType::Audio | crate::models::MediaType::Video
    );
    if !should_probe {
        return Ok(detected.to_owned());
    }

    if !ffprobe_available {
        if is_matroska_mime(detected) {
            anyhow::bail!(
                "File appears to be Matroska/MKV, but ffprobe is required to validate MKV uploads."
            );
        }
        if detected == "video/webm" {
            tracing::debug!(
                path = %input_path.display(),
                "ffprobe unavailable; treating WebM upload as video/webm"
            );
        }
        return Ok(detected.to_owned());
    }

    let stream_kind = crate::media::ffmpeg::probe_stream_kind(input_path).with_context(|| {
        format!("File appears to be {detected}, but ffprobe could not validate its streams")
    })?;

    match (media_type, stream_kind) {
        (crate::models::MediaType::Audio, crate::media::ffmpeg::StreamKind::AudioOnly) => {
            canonical_audio_mime(input_path, detected)
        }
        (crate::models::MediaType::Video, crate::media::ffmpeg::StreamKind::Video) => {
            Ok(canonical_video_mime(detected).to_owned())
        }
        (crate::models::MediaType::Video, crate::media::ffmpeg::StreamKind::AudioOnly)
            if detected == "video/mp4" || detected == "video/webm" =>
        {
            canonical_audio_mime(input_path, detected)
        }
        (crate::models::MediaType::Video, crate::media::ffmpeg::StreamKind::AudioOnly)
            if is_matroska_mime(detected) =>
        {
            anyhow::bail!("Matroska/MKV uploads must contain a video stream.")
        }
        (crate::models::MediaType::Video, crate::media::ffmpeg::StreamKind::AudioOnly) => {
            anyhow::bail!("File appears to be {detected}, but contains only audio streams.")
        }
        (crate::models::MediaType::Audio, crate::media::ffmpeg::StreamKind::Video) => {
            anyhow::bail!("File appears to be {detected}, but contains a video stream.")
        }
        (
            crate::models::MediaType::Image
            | crate::models::MediaType::Pdf
            | crate::models::MediaType::Other,
            _,
        ) => Ok(detected.to_owned()),
    }
}

fn canonical_audio_mime(input_path: &Path, detected: &str) -> Result<String> {
    let codec = crate::media::ffmpeg::probe_audio_codec(input_path).with_context(|| {
        format!("File appears to be {detected}, but ffprobe could not identify its audio codec")
    })?;
    let mime = match codec.as_str() {
        "flac" => "audio/flac",
        "mp3" | "mp2" | "mp1" => "audio/mpeg",
        "aac" => {
            if detected == "audio/aac" {
                "audio/aac"
            } else {
                "audio/mp4"
            }
        }
        "opus" => {
            if detected == "video/webm" || detected == "audio/webm" {
                "audio/webm"
            } else {
                "audio/opus"
            }
        }
        "vorbis" => "audio/ogg",
        codec if codec.starts_with("pcm_") => "audio/wav",
        _ => detected,
    };
    Ok(canonical_audio_mime_variant(mime).to_owned())
}

fn canonical_audio_mime_variant(mime: &str) -> &str {
    match mime {
        "audio/mp3" => "audio/mpeg",
        "audio/x-flac" => "audio/flac",
        "audio/wave" | "audio/x-wav" | "audio/vnd.wave" => "audio/wav",
        "application/ogg" | "audio/oga" => "audio/ogg",
        "audio/m4a" | "audio/x-m4a" => "audio/mp4",
        "audio/x-aac" => "audio/aac",
        _ => mime,
    }
}

fn canonical_video_mime(mime: &str) -> &str {
    match mime {
        "video/matroska" => "video/x-matroska",
        _ => mime,
    }
}

fn is_matroska_mime(mime: &str) -> bool {
    matches!(mime, "video/x-matroska" | "video/matroska")
}

fn validate_av_stream_kind(
    input_path: &Path,
    mime_type: &str,
    media_type: crate::models::MediaType,
    ffprobe_available: bool,
) -> Result<()> {
    if !ffprobe_available {
        return Ok(());
    }

    let stream_kind = crate::media::ffmpeg::probe_stream_kind(input_path).with_context(|| {
        format!("File appears to be {mime_type}, but ffprobe could not validate its streams")
    })?;
    let expected_stream_kind = match media_type {
        crate::models::MediaType::Audio => crate::media::ffmpeg::StreamKind::AudioOnly,
        crate::models::MediaType::Video => crate::media::ffmpeg::StreamKind::Video,
        crate::models::MediaType::Image
        | crate::models::MediaType::Pdf
        | crate::models::MediaType::Other => return Ok(()),
    };
    if stream_kind == expected_stream_kind {
        return Ok(());
    }
    if media_type == crate::models::MediaType::Audio {
        anyhow::bail!("File appears to be {mime_type}, but contains a video stream.");
    }
    anyhow::bail!("File appears to be {mime_type}, but contains only audio streams.");
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
    if let Err(error) = write_image_atomic(thumb_path, &rotated, image::ImageFormat::WebP) {
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
    if let Err(error) = write_image_atomic(image_path, &rotated, format) {
        tracing::warn!("failed to re-orient stored image: {error}");
    }
}

fn write_image_atomic(
    output_path: &Path,
    image: &image::DynamicImage,
    format: image::ImageFormat,
) -> Result<()> {
    let parent = output_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("output path has no parent: {}", output_path.display()))?;
    let tmp = tempfile::Builder::new()
        .prefix("rustchan-orient-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| format!("failed to create temp file for {}", output_path.display()))?;
    image
        .save_with_format(tmp.path(), format)
        .with_context(|| {
            format!(
                "failed to write re-oriented image to {}",
                tmp.path().display()
            )
        })?;
    tmp.persist(output_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace {}", output_path.display()))?;
    Ok(())
}

fn mime_to_ext(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" | "audio/webm" => "webm",
        "video/x-matroska" | "video/matroska" => "mkv",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" | "application/ogg" | "audio/oga" => "ogg",
        "audio/opus" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/wave" | "audio/x-wav" | "audio/vnd.wave" => "wav",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/aac" | "audio/x-aac" => "aac",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        delete_file_checked, save_audio_with_image_thumb_from_path, save_upload_from_path,
        SaveUploadOptions,
    };
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn one_pixel_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encode png");
        bytes
    }

    fn test_upload_options<'a>(
        root: &'a std::path::Path,
        original_filename: &'a str,
    ) -> SaveUploadOptions<'a> {
        SaveUploadOptions {
            original_filename,
            boards_dir: root.to_str().expect("utf8 root"),
            board_short: "test",
            thumb_size: 64,
            max_image_size: 1024 * 1024,
            max_video_size: 1024 * 1024,
            max_audio_size: 1024 * 1024,
            ffmpeg_available: false,
            ffprobe_available: false,
            ffmpeg_webp_available: false,
            allow_any_files: false,
        }
    }

    fn arbitrary_upload_options<'a>(
        root: &'a std::path::Path,
        original_filename: &'a str,
    ) -> SaveUploadOptions<'a> {
        SaveUploadOptions {
            allow_any_files: true,
            ..test_upload_options(root, original_filename)
        }
    }

    fn valid_pdf() -> &'static [u8] {
        b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R >> endobj
4 0 obj << /Length 0 >> stream

endstream endobj
trailer << /Root 1 0 R >>
%%EOF
"
    }

    #[test]
    fn final_processed_size_validation_removes_oversized_outputs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let board_dir = tempdir.path().join("test");
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).expect("create dirs");
        let file_path = board_dir.join("stored.png");
        let thumbnail_path = thumb_dir.join("stored.webp");
        std::fs::write(&file_path, b"too large").expect("write output");
        std::fs::write(&thumbnail_path, b"thumb").expect("write thumb");

        let mut options = test_upload_options(tempdir.path(), "stored.png");
        options.max_image_size = 4;
        let processed = crate::media::ProcessedMedia {
            file_path: file_path.clone(),
            thumbnail_path: thumbnail_path.clone(),
            mime_type: "image/png".to_owned(),
            was_converted: false,
            final_size: 9,
        };

        let error = super::final_processed_size_within_limit(&processed, &options)
            .expect_err("oversized processed output should be rejected");

        assert!(error
            .to_string()
            .contains("Maximum image upload size is 4 B"));
        assert!(!file_path.exists());
        assert!(!thumbnail_path.exists());
    }

    fn valid_webm_header() -> &'static [u8] {
        b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"
    }

    fn sniff(bytes: &[u8]) -> &[u8] {
        bytes.get(..bytes.len().min(512)).unwrap_or(bytes)
    }

    fn ffmpeg_available() -> bool {
        Command::new(&crate::config::CONFIG.ffmpeg_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn ffprobe_available() -> bool {
        Command::new(&crate::config::CONFIG.ffprobe_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn generate_ffmpeg_fixture(output: &Path, args: &[&str]) -> Option<Vec<u8>> {
        let mut command = Command::new(&crate::config::CONFIG.ffmpeg_path);
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .args(args)
            .arg(output)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if !command.status().ok()?.success() {
            return None;
        }
        std::fs::read(output).ok()
    }

    struct AudioFixtureCase<'a> {
        file_name: &'a str,
        expected_mime: &'a str,
        args: &'a [&'a str],
    }

    const AUDIO_FIXTURE_CASES: &[AudioFixtureCase<'static>] = &[
        AudioFixtureCase {
            file_name: "tiny.flac",
            expected_mime: "audio/flac",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "flac",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.mp3",
            expected_mime: "audio/mpeg",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "64k",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.wav",
            expected_mime: "audio/wav",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "pcm_s16le",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.ogg",
            expected_mime: "audio/ogg",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "libvorbis",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.oga",
            expected_mime: "audio/ogg",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "libvorbis",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.opus",
            expected_mime: "audio/opus",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "libopus",
                "-b:a",
                "32k",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.m4a",
            expected_mime: "audio/mp4",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "aac",
                "-b:a",
                "64k",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny-isom.mp4",
            expected_mime: "audio/mp4",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "aac",
                "-b:a",
                "64k",
                "-f",
                "mp4",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny.aac",
            expected_mime: "audio/aac",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "aac",
                "-b:a",
                "64k",
                "-f",
                "adts",
            ],
        },
        AudioFixtureCase {
            file_name: "tiny-audio.webm",
            expected_mime: "audio/webm",
            args: &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.05",
                "-c:a",
                "libopus",
                "-b:a",
                "32k",
                "-f",
                "webm",
            ],
        },
    ];

    #[test]
    fn valid_audio_uploads_accept_common_formats_with_ffprobe() {
        if !ffmpeg_available() || !ffprobe_available() {
            eprintln!("skipping ffmpeg audio fixture test; ffmpeg/ffprobe unavailable");
            return;
        }

        let tempdir = tempfile::tempdir().expect("tempdir");
        let mut checked = 0usize;
        for case in AUDIO_FIXTURE_CASES {
            let input_path = tempdir.path().join(case.file_name);
            let Some(bytes) = generate_ffmpeg_fixture(&input_path, case.args) else {
                eprintln!(
                    "skipping audio fixture {}; ffmpeg could not generate it",
                    case.file_name
                );
                continue;
            };
            let mut options = test_upload_options(tempdir.path(), case.file_name);
            options.ffprobe_available = true;

            let uploaded = save_upload_from_path(&input_path, sniff(&bytes), bytes.len(), &options)
                .unwrap_or_else(|error| panic!("{} should upload: {error:#}", case.file_name));

            assert_eq!(uploaded.mime_type, case.expected_mime, "{}", case.file_name);
            assert_eq!(uploaded.media_type, crate::models::MediaType::Audio);
            assert!(tempdir.path().join(&uploaded.file_path).exists());
            checked = checked.saturating_add(1);
        }

        assert!(checked >= 8, "expected most audio fixtures to be generated");
    }

    #[test]
    fn valid_mkv_uploads_save_as_video_with_probe() {
        if !ffmpeg_available() || !ffprobe_available() {
            eprintln!("skipping MKV fixture test; ffmpeg/ffprobe unavailable");
            return;
        }

        let tempdir = tempfile::tempdir().expect("tempdir");
        let input_path = tempdir.path().join("tiny.mkv");
        let bytes = generate_ffmpeg_fixture(
            &input_path,
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=16x16:d=0.1",
                "-an",
                "-c:v",
                "mpeg4",
                "-f",
                "matroska",
            ],
        )
        .expect("generate mkv fixture");
        let mut options = test_upload_options(tempdir.path(), "browser-octet-stream.bin");
        options.ffprobe_available = true;

        let uploaded = save_upload_from_path(&input_path, sniff(&bytes), bytes.len(), &options)
            .expect("valid MKV upload should save");

        assert_eq!(uploaded.mime_type, "video/x-matroska");
        assert_eq!(uploaded.media_type, crate::models::MediaType::Video);
        assert!(std::path::Path::new(&uploaded.file_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv")));
        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(tempdir.path().join(&uploaded.thumb_path).exists());
    }

    #[test]
    fn fake_mkv_is_rejected_without_outputs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input_path = tempdir.path().join("fake.mkv");
        let fake_mkv = b"\x1a\x45\xdf\xa3\xa3\x42\x86\x81\x01\x42\xf7\x81\x01\x42\xf2\x81\x04\x42\xf3\x81\x08\x42\x82\x88matroska\x42\x87\x81\x04not real media";
        std::fs::write(&input_path, fake_mkv).expect("write fake mkv");
        let mut options = test_upload_options(tempdir.path(), "fake.mkv");
        options.ffprobe_available = true;

        let Err(error) =
            save_upload_from_path(&input_path, sniff(fake_mkv), fake_mkv.len(), &options)
        else {
            panic!("fake MKV should be rejected");
        };

        assert!(error
            .to_string()
            .contains("ffprobe could not validate its streams"));
        assert!(!tempdir.path().join("test").exists());
    }

    #[test]
    fn delete_file_checked_removes_valid_in_tree_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let upload_root = tempdir.path().join("uploads");
        let board_dir = upload_root.join("test");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        std::fs::write(board_dir.join("file.txt"), b"ok").expect("write file");

        delete_file_checked(
            upload_root.to_str().expect("utf8 upload root"),
            "test/file.txt",
        )
        .expect("delete file");

        assert!(!board_dir.join("file.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn delete_file_checked_rejects_symlink_parent_escape() {
        use std::os::unix::fs as unix_fs;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let upload_root = tempdir.path().join("uploads");
        let board_dir = upload_root.join("test");
        let outside = tempdir.path().join("outside");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        std::fs::create_dir_all(&outside).expect("create outside dir");
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, b"secret").expect("write outside file");
        unix_fs::symlink(&outside, board_dir.join("link")).expect("symlink");

        assert!(delete_file_checked(
            upload_root.to_str().expect("utf8 upload root"),
            "test/link/secret.txt",
        )
        .is_err());
        assert!(outside_file.exists());
    }

    #[test]
    fn webm_classification_skips_ffprobe_when_startup_marked_it_unavailable() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".webm")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let webm = valid_webm_header();
        std::fs::write(input.path(), webm).expect("write webm");

        let mime = super::classify_upload_mime(input.path(), webm, false, false)
            .expect("classify webm without ffprobe");

        assert_eq!(mime, "video/webm");
    }

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
            false,
        )
        .expect("save flac");

        assert_eq!(uploaded.mime_type, "audio/flac");
        assert_eq!(uploaded.file_path.split('.').next_back(), Some("flac"));
        assert!(!uploaded.processing_pending);

        let stored_bytes =
            std::fs::read(tempdir.path().join(&uploaded.file_path)).expect("read stored flac");
        assert_eq!(stored_bytes, flac_bytes);
    }

    #[test]
    fn malformed_png_magic_is_rejected_before_storage() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let malformed = b"\x89PNG\r\n\x1a\nthis is not a complete png";
        std::fs::write(input.path(), malformed).expect("write malformed png");

        let Err(error) = save_upload_from_path(
            input.path(),
            malformed,
            malformed.len(),
            &test_upload_options(tempdir.path(), "broken.png"),
        ) else {
            panic!("malformed png should be rejected");
        };

        assert!(error.to_string().contains("image header is malformed"));
        assert!(!tempdir.path().join("test").exists());
    }

    #[test]
    fn malformed_png_without_tempfile_suffix_is_rejected_before_storage() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let malformed = b"\x89PNG\r\n\x1a\nthis is not a complete png";
        std::fs::write(input.path(), malformed).expect("write malformed png");

        let Err(error) = save_upload_from_path(
            input.path(),
            malformed,
            malformed.len(),
            &test_upload_options(tempdir.path(), "broken.png"),
        ) else {
            panic!("malformed png should be rejected");
        };

        assert!(error.to_string().contains("image header is malformed"));
        assert!(!tempdir.path().join("test").exists());
    }

    #[test]
    fn arbitrary_file_upload_is_saved_when_opted_in() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let contents = b"plain text attachment\n";
        std::fs::write(input.path(), contents).expect("write text");

        let uploaded = save_upload_from_path(
            input.path(),
            contents,
            contents.len(),
            &arbitrary_upload_options(tempdir.path(), "notes.txt"),
        )
        .expect("save text upload");

        assert_eq!(uploaded.mime_type, "application/octet-stream");
        assert_eq!(uploaded.media_type, crate::models::MediaType::Other);
        assert!(std::path::Path::new(&uploaded.file_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt")));

        let stored =
            std::fs::read(tempdir.path().join(&uploaded.file_path)).expect("read stored upload");
        assert_eq!(stored, contents);
    }

    #[test]
    fn video_upload_saves_with_svg_placeholder_when_ffmpeg_is_missing() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".webm")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let webm = valid_webm_header();
        std::fs::write(input.path(), webm).expect("write webm");

        let uploaded = save_upload_from_path(
            input.path(),
            webm,
            webm.len(),
            &test_upload_options(tempdir.path(), "clip.webm"),
        )
        .expect("video upload should save with fallback thumbnail");

        assert_eq!(uploaded.mime_type, "video/webm");
        assert_eq!(uploaded.media_type, crate::models::MediaType::Video);
        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(std::path::Path::new(&uploaded.thumb_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")));
        assert!(tempdir.path().join(&uploaded.thumb_path).exists());
    }

    #[test]
    fn decodable_png_upload_still_saves_and_thumbnails() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".png")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let png = one_pixel_png();
        std::fs::write(input.path(), &png).expect("write png");

        let uploaded = save_upload_from_path(
            input.path(),
            &png,
            png.len(),
            &test_upload_options(tempdir.path(), "renamed.txt"),
        )
        .expect("valid png saves");

        assert_eq!(uploaded.mime_type, "image/png");
        assert_eq!(uploaded.original_name, "renamed.txt");
        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(tempdir.path().join(&uploaded.thumb_path).exists());
    }

    #[test]
    fn valid_pdf_upload_saves_generic_thumbnail_when_renderer_is_unavailable() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let pdf = valid_pdf();
        std::fs::write(input.path(), pdf).expect("write pdf");
        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Unavailable,
        );

        let uploaded = save_upload_from_path(
            input.path(),
            pdf,
            pdf.len(),
            &test_upload_options(tempdir.path(), "doc.pdf"),
        )
        .expect("valid PDF saves");

        assert_eq!(uploaded.mime_type, "application/pdf");
        assert_eq!(uploaded.media_type, crate::models::MediaType::Pdf);
        assert!(std::path::Path::new(&uploaded.file_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")));
        assert!(std::path::Path::new(&uploaded.thumb_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")));
        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(tempdir.path().join(&uploaded.thumb_path).exists());
        let thumb = std::fs::read_to_string(tempdir.path().join(&uploaded.thumb_path))
            .expect("read generic pdf thumbnail");
        assert!(thumb.contains("PDF"));
    }

    #[test]
    fn pdf_thumbnail_renderer_failure_keeps_pdf_and_uses_generic_thumbnail() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let pdf = valid_pdf();
        std::fs::write(input.path(), pdf).expect("write pdf");
        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Fail,
        );

        let uploaded = save_upload_from_path(
            input.path(),
            pdf,
            pdf.len(),
            &test_upload_options(tempdir.path(), "broken.pdf"),
        )
        .expect("save pdf with fallback thumbnail");

        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(tempdir.path().join(&uploaded.thumb_path).exists());
        assert!(std::path::Path::new(&uploaded.thumb_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")));
    }

    #[test]
    fn pdf_thumbnail_timeout_cleans_tempdirs_and_partial_files() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let pdf = valid_pdf();
        std::fs::write(input.path(), pdf).expect("write pdf");
        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Timeout,
        );

        let uploaded = save_upload_from_path(
            input.path(),
            pdf,
            pdf.len(),
            &test_upload_options(tempdir.path(), "slow.pdf"),
        )
        .expect("save pdf after thumbnail timeout");

        let board_dir = tempdir.path().join("test");
        let thumb_path = tempdir.path().join(&uploaded.thumb_path);
        assert!(tempdir.path().join(&uploaded.file_path).exists());
        assert!(thumb_path.exists());
        assert!(thumb_path.extension().is_some_and(|ext| ext == "svg"));

        let stray_entries = std::fs::read_dir(board_dir.join("thumbs"))
            .expect("read thumbs dir")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("collect thumbs entries");
        assert!(stray_entries.iter().all(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            !name.starts_with("rustchan-pdf-thumb-")
        }));
    }

    #[test]
    fn pdf_without_eof_marker_is_rejected_before_storage() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let input = tempfile::Builder::new()
            .suffix(".pdf")
            .tempfile_in(tempdir.path())
            .expect("temp file");
        let malformed_pdf = b"%PDF-1.4\n1 0 obj <<>> endobj\ntrailer <<>>\n";
        std::fs::write(input.path(), malformed_pdf).expect("write malformed pdf");

        let Err(error) = save_upload_from_path(
            input.path(),
            malformed_pdf,
            malformed_pdf.len(),
            &test_upload_options(tempdir.path(), "broken.pdf"),
        ) else {
            panic!("missing EOF marker should be rejected");
        };

        assert!(error.to_string().contains("trailer is missing"));
    }
}
