// Request handlers.

/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers");

pub(crate) mod admin;
pub(crate) mod banner;
pub(crate) mod board;
pub(crate) mod captcha;
pub(crate) mod favicon;
pub(crate) mod posting;
pub(crate) mod render;
pub(crate) mod setup;
pub(crate) mod thread;

mod multipart;

#[expect(
    unused_imports,
    reason = "retain the existing crate-visible envelope limit path even without an external caller"
)]
pub(crate) use multipart::PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES;
pub(crate) use multipart::{
    discard_unknown_multipart_field, enforce_public_multipart_envelope, parse_post_multipart,
    PostFormData, PostMultipartContext, TempUpload, PUBLIC_MULTIPART_REQUEST_MAX_BYTES,
    PUBLIC_UPLOAD_TIMEOUT,
};

use crate::error::{AppError, Result};
use axum::{
    body::Body,
    response::{IntoResponse as _, Response},
};

/// Classify enhanced requests; authentication and CSRF checks remain independent.
#[must_use]
pub(crate) fn is_xml_http_request(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

/// Keeps native posting failures actionable without changing their status or security checks.
pub(crate) fn native_post_error_response(
    error: AppError,
    prefill: Option<&crate::templates::forms::PostFormState>,
) -> Response {
    let mut response = error.into_response();
    let Some(crate::error::ErrorPage::Message(message)) =
        response.extensions().get::<crate::error::ErrorPage>()
    else {
        // Ban pages retain their dedicated explanation and appeal form.
        return response;
    };
    let body = crate::templates::forms::post_error_recovery_body(message, prefill);
    let html = crate::templates::base_layout(
        "Posting error",
        None,
        &body,
        "",
        &crate::templates::live_boards(),
        None,
        None,
        false,
        "/",
    );
    let _previous_value = response
        .extensions_mut()
        .insert(crate::error::ErrorPage::Content {
            title: "Posting error".into(),
            body,
        });
    *response.body_mut() = Body::from(html);
    response
}

// Upload error classifier (#6)
/// Convert an anyhow error from `save_upload` into the most appropriate
/// `AppError` variant, giving clients accurate HTTP status codes:
///   • "File too large"          → 413 `UploadTooLarge`
///   • "Insufficient disk space" → 413 `UploadTooLarge`
///   • "File type not allowed"   → 415 `InvalidMediaType`
///   • "Not an audio file"       → 415 `InvalidMediaType`
///   • filesystem errors         → generic 500, with details retained for logging.
///   • other decoder errors      → a generic 400, without tool output or paths.
pub(crate) fn classify_upload_error(e: &anyhow::Error) -> AppError {
    let msg = e.to_string();
    // Compare lower-cased so minor wording changes in save_upload don't silently
    // fall through to a generic 400 instead of the correct 413 / 415.
    let lower = msg.to_ascii_lowercase();
    if lower.starts_with("file too large") || lower.starts_with("insufficient disk space") {
        AppError::UploadTooLarge(msg)
    } else if lower.starts_with("file type not allowed") || lower.starts_with("not an audio file") {
        AppError::InvalidMediaType(msg)
    } else if e
        .chain()
        .any(<dyn std::error::Error + 'static>::is::<std::io::Error>)
    {
        AppError::Internal(anyhow::anyhow!("Upload filesystem operation failed: {e:#}"))
    } else if lower.contains("image header is malformed") {
        AppError::BadRequest("The image header is malformed. Check that the image is valid.".into())
    } else {
        AppError::BadRequest(
            "The server could not validate this upload. Check that the file is valid and supported.".into(),
        )
    }
}

// Shared media upload processing (R2-2)
// create_thread (board.rs) and post_reply (thread.rs) had identical blocks for:
//   1. Magic-byte mime detection + per-board toggle enforcement
//   2. SHA-256 deduplication lookup
//   3. save_upload / save_audio_with_image_thumb
//   4. record_file_hash
//   5. Image+audio combo validation
//   6. Background job enqueueing
//
// Both handlers now call these shared functions instead of duplicating the code.

use crate::models::Board;

/// Process the primary file upload for a new post: detect mime type, enforce
/// per-board media toggles, SHA-256 dedup, save to disk and record hash.
///
/// Returns `Ok(None)` when `file_data` is `None` (no file attached).
/// Must be called from inside a `spawn_blocking` closure.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters mirror the validated upload, board policy, and destination records"
)]
#[expect(
    clippy::too_many_lines,
    reason = "media validation, persistence, thumbnailing, and cleanup form one guarded operation"
)]
pub(crate) fn process_primary_upload(
    file_data: Option<(TempUpload, String)>,
    board: &Board,
    mut lookup: impl FnMut(&str) -> Result<Option<crate::db::CachedFile>>,
    upload_dir: &str,
    save_root: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    ffmpeg_available: bool,
) -> Result<(Option<crate::utils::files::UploadedFile>, Option<String>)> {
    let Some((upload, fname)) = file_data else {
        return Ok((None, None));
    };
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let detected_mime = crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        allow_any_files,
    )
    .map_err(|error| classify_upload_error(&error))?;
    let detected_media = crate::models::MediaType::from_mime(&detected_mime);
    let ambiguous_webm = detected_mime == crate::utils::files::AMBIGUOUS_WEBM_MIME;

    match detected_media {
        crate::models::MediaType::Image if !board.allow_images => {
            return Err(AppError::BadRequest(
                "Image uploads are disabled on this board.".into(),
            ));
        }
        crate::models::MediaType::Video if !board.allow_video => {
            return Err(AppError::BadRequest(
                "Video uploads are disabled on this board.".into(),
            ));
        }
        crate::models::MediaType::Audio if !board.allow_audio => {
            return Err(AppError::BadRequest(
                "Audio uploads are disabled on this board.".into(),
            ));
        }
        crate::models::MediaType::Pdf if !board.allow_pdf => {
            return Err(AppError::BadRequest(
                "PDF uploads are disabled on this board.".into(),
            ));
        }
        crate::models::MediaType::Other
            if ambiguous_webm && (!board.allow_video || !board.allow_audio) =>
        {
            return Err(AppError::BadRequest(
                "WebM stream type could not be established; both video and audio uploads must be enabled to accept it as a neutral download."
                    .into(),
            ));
        }
        crate::models::MediaType::Other if !ambiguous_webm && !allow_any_files => {
            return Err(AppError::BadRequest(
                "This board only accepts image, video, audio, or PDF uploads.".into(),
            ));
        }
        crate::models::MediaType::Image
        | crate::models::MediaType::Video
        | crate::models::MediaType::Audio
        | crate::models::MediaType::Pdf
        | crate::models::MediaType::Other => {}
    }

    let upload_options = crate::utils::files::SaveUploadOptions {
        original_filename: &fname,
        boards_dir: save_root,
        board_short: &board.short_name,
        thumb_size,
        max_image_size,
        max_video_size,
        max_audio_size,
        max_pdf_size,
        ffmpeg_available,
        allow_any_files,
    };
    let validated = crate::utils::files::storage::validate_upload_for_storage(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        upload.size_bytes,
        &upload_options,
    )
    .map_err(|error| classify_upload_error(&error))?;

    // SHA-256 deduplication — serve the cached entry without re-saving.
    //
    // Validate that both the cached file and thumbnail still exist
    // on disk before returning the dedup hit.  When a thread or board is
    // deleted its files are removed from disk, but the file_hashes table is
    // not pruned.  Without this check, re-uploading the same image after its
    // original thread/board was deleted would return stale paths pointing at
    // deleted files, so the post would display no image and no thumbnail.
    //
    // If either path is missing we fall through to re-process the upload.
    // record_file_hash uses INSERT OR REPLACE, so the cache entry is
    // automatically refreshed to point at the newly saved files.
    let hash = sha256_file_hex(upload.temp_file.path())?;
    if let Some(cached) = lookup(&hash)? {
        let same_board_cache = cached_paths_belong_to_board(&cached, &board.short_name);
        let file_ok = std::path::Path::new(upload_dir)
            .join(&cached.file_path)
            .exists();
        let thumb_ok = cached.thumb_path.is_empty()
            || std::path::Path::new(upload_dir)
                .join(&cached.thumb_path)
                .exists();

        if same_board_cache && file_ok && thumb_ok {
            let cached_media = crate::models::MediaType::from_mime(&cached.mime_type);
            let cached_size =
                std::fs::metadata(std::path::Path::new(upload_dir).join(&cached.file_path))
                    .ok()
                    .and_then(|metadata| i64::try_from(metadata.len()).ok())
                    .unwrap_or_else(|| i64::try_from(upload.size_bytes).unwrap_or(0));
            return Ok((
                Some(crate::utils::files::UploadedFile {
                    file_path: cached.file_path,
                    thumb_path: cached.thumb_path,
                    original_name: crate::utils::sanitize::sanitize_filename(&fname),
                    mime_type: cached.mime_type,
                    file_size: cached_size,
                    media_type: cached_media,
                    processing_pending: false,
                    dedup_reused: true,
                }),
                None,
            ));
        }

        // One or both paths are gone — the entry is stale.  Log and fall
        // through so the file is re-saved and the cache is updated below.
        // Cross-board hits are also re-saved under the current board; otherwise
        // a protected board could point at public media from another board.
        tracing::debug!(target: LOG_TARGET,
            "dedup cache miss: same_board_cache={same_board_cache} file_ok={file_ok} thumb_ok={thumb_ok}, \
             re-processing upload for hash {hash}"
        );
    }

    let f =
        crate::utils::files::storage::save_validated_upload_from_path(validated, &upload_options)
            .map_err(|e| classify_upload_error(&e))?;
    Ok((Some(f), Some(hash)))
}

fn cached_paths_belong_to_board(cached: &crate::db::CachedFile, board_short: &str) -> bool {
    upload_path_belongs_to_board(&cached.file_path, board_short)
        && (cached.thumb_path.is_empty()
            || upload_path_belongs_to_board(&cached.thumb_path, board_short))
}

fn upload_path_belongs_to_board(path: &str, board_short: &str) -> bool {
    path.split('/').next() == Some(board_short)
}

fn temp_upload_mime(upload: &TempUpload, allow_any_files: bool) -> Result<String> {
    crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        allow_any_files,
    )
    .map_err(|error| classify_upload_error(&error))
}

/// Process the secondary audio file for an image+audio combo upload.
/// `primary_upload` must already be the processed primary image.
///
/// Returns `Ok(None)` when `audio_file_data` is `None`.
/// Must be called from inside a `spawn_blocking` closure.
pub(crate) fn process_audio_combo(
    audio_file_data: Option<(TempUpload, String)>,
    primary_upload: Option<&crate::utils::files::UploadedFile>,
    board: &Board,
    upload_dir: &str,
    max_audio_size: usize,
) -> Result<Option<crate::utils::files::UploadedFile>> {
    let Some((audio_upload, aud_fname)) = audio_file_data else {
        return Ok(None);
    };

    if !board.allow_audio {
        return Err(AppError::BadRequest(
            "Audio uploads are disabled on this board.".into(),
        ));
    }

    // Audio combo requires the primary file to be an image.
    let primary_is_image =
        primary_upload.is_some_and(|u| matches!(u.media_type, crate::models::MediaType::Image));
    if !primary_is_image {
        return Err(AppError::BadRequest(
            "Audio can only be combined with an image upload.".into(),
        ));
    }

    let mut aud_file = crate::utils::files::save_audio_with_image_thumb_from_path(
        audio_upload.temp_file.path(),
        &audio_upload.sniff_bytes,
        audio_upload.size_bytes,
        &aud_fname,
        upload_dir,
        &board.short_name,
        max_audio_size,
    )
    .map_err(|e| classify_upload_error(&e))?;

    // Use the image thumbnail as the audio's visual.
    if let Some(img) = primary_upload {
        aud_file.thumb_path.clone_from(&img.thumb_path);
    }
    Ok(Some(aud_file))
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters represent the three optional media inputs and their shared post context"
)]
pub(crate) fn process_audio_first_uploads(
    audio_file_data: Option<(TempUpload, String)>,
    image_file_data: Option<(TempUpload, String)>,
    fallback_file_data: Option<(TempUpload, String)>,
    board: &Board,
    mut lookup: impl FnMut(&str) -> Result<Option<crate::db::CachedFile>>,
    upload_dir: &str,
    save_root_str: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    max_pdf_size: usize,
    ffmpeg_available: bool,
) -> Result<(
    Option<crate::utils::files::UploadedFile>,
    Option<crate::utils::files::UploadedFile>,
    Option<String>,
)> {
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let has_audio_or_image_upload = audio_file_data.is_some() || image_file_data.is_some();
    let mut save_primary = |file_data| {
        process_primary_upload(
            file_data,
            board,
            &mut lookup,
            upload_dir,
            save_root_str,
            thumb_size,
            max_image_size,
            max_video_size,
            max_audio_size,
            max_pdf_size,
            ffmpeg_available,
        )
    };

    if has_audio_or_image_upload && fallback_file_data.is_some() {
        return Err(AppError::BadRequest(
            "Use either the audio/image upload flow or the other-file slot, not both in the same post."
                .into(),
        ));
    }

    if let Some((image_upload, image_name)) = image_file_data {
        let (primary, primary_hash) = save_primary(Some((image_upload, image_name)))?;

        let audio = process_audio_combo(
            audio_file_data,
            primary.as_ref(),
            board,
            save_root_str,
            max_audio_size,
        )?;

        return Ok((primary, audio, primary_hash));
    }

    if let Some((audio_upload, audio_name)) = audio_file_data {
        let audio_mime = temp_upload_mime(&audio_upload, allow_any_files)?;
        if crate::models::MediaType::from_mime(&audio_mime) != crate::models::MediaType::Audio {
            return Err(AppError::BadRequest(
                "The audio slot only accepts audio files.".into(),
            ));
        }

        let (primary, primary_hash) = save_primary(Some((audio_upload, audio_name)))?;

        return Ok((primary, None, primary_hash));
    }

    let (primary, primary_hash) = save_primary(fallback_file_data)?;

    Ok((primary, None, primary_hash))
}

fn sha256_file_hex(path: &std::path::Path) -> Result<String> {
    use sha2::Digest as _;
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open temp upload for hash: {e}")))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = std::io::Read::read(&mut file, &mut buf)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Hash temp upload: {e}")))?;
        if read == 0 {
            break;
        }
        if let Some(bytes) = buf.get(..read) {
            hasher.update(bytes);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}
