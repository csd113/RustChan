// Request handlers.

pub mod admin;
pub mod banner;
pub mod board;
pub mod favicon;
pub mod posting;
pub mod render;
pub mod thread;

// ─── Shared multipart form parsing ───────────────────────────────────────────
//
// Both create_thread and post_reply parse the same multipart fields.
// This helper consolidates that duplicated logic into one place.

use crate::error::{AppError, Result};
use crate::middleware::validate_csrf;
use crate::workers::JobQueue;
use axum::extract::Multipart;
use std::collections::HashSet;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

const MIME_SNIFF_BYTES: usize = 512;
const TEXT_MULTIPART_FIELD_MAX_BYTES: usize = 64 * 1024;
const UNKNOWN_MULTIPART_FIELD_MAX_BYTES: usize = 64 * 1024;
// Public post uploads intentionally bypass Axum's global body cap so per-board
// limits above old defaults still work. This aggregate budget bounds the whole
// multipart stream after board settings are loaded.
const PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES: usize = 512 * 1024 * 1024;
// Caps field spam and duplicate-slot churn before bodies are streamed.
const PUBLIC_MULTIPART_MAX_FIELDS: usize = 64;
// Conservative whole-request upload timeout. True min-rate enforcement would be
// more precise, but this avoids indefinite request-slot pinning without breaking
// normal large LAN uploads.
pub const PUBLIC_UPLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

fn multipart_read_error(
    context: &'static str,
    error: &axum::extract::multipart::MultipartError,
) -> AppError {
    let message = error.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("body write aborted")
        || lower.contains("error reading a body")
        || lower.contains("connection")
        || lower.contains("early eof")
        || lower.contains("unexpected eof")
    {
        tracing::warn!(context, error = %message, "client disconnected during multipart upload");
    } else {
        tracing::warn!(context, error = %message, "multipart parsing failed");
    }
    AppError::BadRequest(message)
}

#[derive(Default)]
struct PublicMultipartBudget {
    fields_seen: usize,
    bytes_seen: usize,
}

impl PublicMultipartBudget {
    fn note_field(&mut self) -> Result<()> {
        self.fields_seen = self.fields_seen.saturating_add(1);
        if self.fields_seen > PUBLIC_MULTIPART_MAX_FIELDS {
            return Err(AppError::BadRequest(
                "Multipart form contains too many fields.".into(),
            ));
        }
        Ok(())
    }

    fn note_chunk(&mut self, len: usize) -> Result<()> {
        self.bytes_seen = self.bytes_seen.saturating_add(len);
        if self.bytes_seen > PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Multipart upload is too large.".into(),
            ));
        }
        Ok(())
    }
}

async fn read_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    budget: &mut PublicMultipartBudget,
) -> Result<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| multipart_read_error("text field", &e))?
    {
        budget.note_chunk(chunk.len())?;
        if bytes.len().saturating_add(chunk.len()) > TEXT_MULTIPART_FIELD_MAX_BYTES {
            tracing::warn!(
                limit_bytes = TEXT_MULTIPART_FIELD_MAX_BYTES,
                "multipart text field exceeded parser limit"
            );
            return Err(AppError::UploadTooLarge(
                "Multipart text field is too large.".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_error| AppError::BadRequest("Multipart text field is not valid UTF-8.".into()))
}

pub async fn discard_unknown_multipart_field(
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<()> {
    let mut total = 0usize;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| multipart_read_error("unknown field", &e))?
    {
        total = total.saturating_add(chunk.len());
        if total > UNKNOWN_MULTIPART_FIELD_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Unexpected multipart field is too large.".into(),
            ));
        }
    }
    Ok(())
}

async fn discard_unknown_public_multipart_field(
    mut field: axum::extract::multipart::Field<'_>,
    budget: &mut PublicMultipartBudget,
) -> Result<()> {
    let mut total = 0usize;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| multipart_read_error("unknown field", &e))?
    {
        budget.note_chunk(chunk.len())?;
        total = total.saturating_add(chunk.len());
        if total > UNKNOWN_MULTIPART_FIELD_MAX_BYTES {
            return Err(AppError::UploadTooLarge(
                "Unexpected multipart field is too large.".into(),
            ));
        }
    }
    Ok(())
}

// ─── Streaming multipart size limit ──────────────────────────────────────────
//
// 3.1: The previous implementation called `field.bytes().await` which buffers
// the entire file in memory before any size check, allowing a malicious client
// to exhaust server RAM with a multi-GB upload.
//
// `stream_field_to_temp_file` writes chunks directly to disk and aborts —
// returning HTTP 413 — the moment the running total exceeds the configured
// board limit for that field.
//
// Text fields (CSRF token, post body, …) use the same chunked parser with a
// small fixed cap, so disabling Axum's route-level body limit for upload routes
// does not leave text fields unbounded.

async fn stream_field_to_temp_file(
    mut field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    field_name: &'static str,
    budget: &mut PublicMultipartBudget,
) -> Result<TempUpload> {
    let temp_file = tempfile::Builder::new()
        .prefix("rustchan-upload-")
        .tempfile()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp upload file: {e}")))?;
    let std_file = temp_file
        .reopen()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen temp upload file: {e}")))?;
    let mut file = tokio::fs::File::from_std(std_file);
    let mut sniff_bytes = Vec::with_capacity(MIME_SNIFF_BYTES);
    let mut size_bytes = 0usize;

    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| multipart_read_error(field_name, &e))?
    {
        budget.note_chunk(chunk.len())?;
        if size_bytes.saturating_add(chunk.len()) > max_bytes {
            tracing::warn!(
                field = field_name,
                streamed_bytes = size_bytes,
                next_chunk_bytes = chunk.len(),
                limit_bytes = max_bytes,
                "multipart upload field exceeded board limit"
            );
            return Err(AppError::UploadTooLarge(format!(
                "File too large. Maximum upload size is {} MiB.",
                max_bytes / 1024 / 1024
            )));
        }
        if sniff_bytes.len() < MIME_SNIFF_BYTES {
            let remaining = MIME_SNIFF_BYTES.saturating_sub(sniff_bytes.len());
            let take = remaining.min(chunk.len());
            if let Some(prefix) = chunk.get(..take) {
                sniff_bytes.extend_from_slice(prefix);
            }
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp upload file: {e}")))?;
        size_bytes = size_bytes.saturating_add(chunk.len());
    }
    file.flush()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush temp upload file: {e}")))?;

    tracing::info!(
        field = field_name,
        size_bytes,
        limit_bytes = max_bytes,
        "multipart upload field staged successfully"
    );

    Ok(TempUpload {
        temp_file,
        sniff_bytes,
        size_bytes,
    })
}

async fn read_upload_field(
    field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
    default_name: &str,
    field_name: &'static str,
    budget: &mut PublicMultipartBudget,
) -> Result<Option<(TempUpload, String)>> {
    let fname = field.file_name().unwrap_or(default_name).to_owned();
    let upload = stream_field_to_temp_file(field, max_bytes, field_name, budget).await?;
    Ok((upload.size_bytes > 0).then_some((upload, fname)))
}

pub struct TempUpload {
    pub temp_file: tempfile::NamedTempFile,
    pub sniff_bytes: Vec<u8>,
    pub size_bytes: usize,
}

/// Parsed fields from a post/thread creation multipart form.
pub struct PostFormData {
    pub csrf_verified: bool,
    pub submission_token: String,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    /// Legacy/general upload slot (used for video or arbitrary files).
    pub file: Option<(TempUpload, String)>,
    /// Primary audio slot shown first in the posting UI.
    pub audio_file: Option<(TempUpload, String)>,
    /// Optional cover-image slot shown second in the posting UI.
    pub image_file: Option<(TempUpload, String)>,
    // ── Poll fields (only used when creating a new thread) ────────────────
    pub poll_question: String,
    pub poll_options: Vec<String>,
    /// Duration in seconds (parsed from value + unit)
    pub poll_duration_secs: Option<i64>,
    /// Sage — when true the reply must not bump the thread.
    pub sage: bool,
    /// `PoW` CAPTCHA nonce — submitted by the thread-creation form when enabled.
    pub pow_nonce: String,
}

/// Drain all fields from a multipart form into [`PostFormData`].
/// `csrf_cookie` is the value from the browser cookie for CSRF verification.
#[expect(clippy::too_many_lines)]
pub async fn parse_post_multipart(
    mut multipart: Multipart,
    csrf_cookie: Option<&str>,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
) -> Result<PostFormData> {
    tracing::info!(
        max_image_bytes = max_image_size,
        max_video_bytes = max_video_size,
        max_audio_bytes = max_audio_size,
        "accepted multipart upload limits for post request"
    );

    let mut csrf_verified = false;
    let mut submission_token = String::new();
    let mut name = String::new();
    let mut subject = String::new();
    let mut body = String::new();
    let mut deletion_token = String::new();
    let mut file: Option<(TempUpload, String)> = None;
    let mut audio_file: Option<(TempUpload, String)> = None;
    let mut image_file: Option<(TempUpload, String)> = None;
    let mut poll_question = String::new();
    let mut poll_options: Vec<String> = Vec::new();
    let mut poll_duration_value: Option<i64> = None;
    let mut poll_duration_unit = String::from("hours");
    let mut sage = false;
    let mut pow_nonce = String::new();
    let mut budget = PublicMultipartBudget::default();
    let mut seen_upload_slots = HashSet::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| multipart_read_error("multipart", &e))?
    {
        budget.note_field()?;
        match field.name() {
            Some("_csrf") => {
                let v = read_text_field(field, &mut budget).await?;
                if validate_csrf(csrf_cookie, &v) {
                    csrf_verified = true;
                }
            }
            Some("submission_token") => {
                submission_token = read_text_field(field, &mut budget).await?;
            }
            Some("name") => name = read_text_field(field, &mut budget).await?,
            Some("subject") => subject = read_text_field(field, &mut budget).await?,
            Some("body") => body = read_text_field(field, &mut budget).await?,
            Some("deletion_token") => deletion_token = read_text_field(field, &mut budget).await?,
            Some("sage") => {
                let v = read_text_field(field, &mut budget).await?;
                sage = v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true");
            }
            Some("pow_nonce") => pow_nonce = read_text_field(field, &mut budget).await?,
            Some("poll_question") => {
                let v = read_text_field(field, &mut budget).await?;
                if v.chars().count() > 500 {
                    return Err(AppError::BadRequest(
                        "Poll question must be 500 characters or fewer.".into(),
                    ));
                }
                poll_question = v;
            }
            Some("poll_option") => {
                let v = read_text_field(field, &mut budget).await?;
                let trimmed = v.trim().to_owned();
                if !trimmed.is_empty() {
                    if poll_options.len() >= 20 {
                        return Err(AppError::BadRequest(
                            "Polls are limited to 20 options.".into(),
                        ));
                    }
                    if trimmed.chars().count() > 200 {
                        return Err(AppError::BadRequest(
                            "Each poll option must be 200 characters or fewer.".into(),
                        ));
                    }
                    poll_options.push(trimmed);
                }
            }
            Some("poll_duration_value") => {
                let v = read_text_field(field, &mut budget).await?;
                poll_duration_value = v.trim().parse::<i64>().ok();
            }
            Some("poll_duration_unit") => {
                poll_duration_unit = read_text_field(field, &mut budget).await?;
            }
            Some("file") => {
                if !seen_upload_slots.insert("file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'file'.".into(),
                    ));
                }
                file = read_upload_field(
                    field,
                    max_image_size.max(max_video_size).max(max_audio_size),
                    "upload",
                    "file",
                    &mut budget,
                )
                .await?;
            }
            Some("audio_file") => {
                if !seen_upload_slots.insert("audio_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'audio_file'.".into(),
                    ));
                }
                audio_file =
                    read_upload_field(field, max_audio_size, "audio", "audio_file", &mut budget)
                        .await?;
            }
            Some("image_file") => {
                if !seen_upload_slots.insert("image_file") {
                    return Err(AppError::BadRequest(
                        "Duplicate upload field 'image_file'.".into(),
                    ));
                }
                image_file =
                    read_upload_field(field, max_image_size, "image", "image_file", &mut budget)
                        .await?;
            }
            _ => {
                discard_unknown_public_multipart_field(field, &mut budget).await?;
            }
        }
    }

    // Convert duration value + unit → seconds (saturating to prevent overflow).
    // The unit is validated against an explicit allow-list (case-insensitive) so
    // that a tampered form field does not silently multiply by an arbitrary factor.
    let poll_duration_secs = if poll_question.trim().is_empty() {
        None
    } else {
        match poll_duration_value {
            None => None,
            Some(v) => {
                let unit = poll_duration_unit.trim().to_ascii_lowercase();
                let secs = match unit.as_str() {
                    "minutes" => v.saturating_mul(60),
                    "hours" => v.saturating_mul(3600),
                    "days" => v.saturating_mul(86_400),
                    other => {
                        return Err(AppError::BadRequest(format!("Invalid poll duration unit '{other}'. Use 'minutes', 'hours', or 'days'.")));
                    }
                };
                Some(secs)
            }
        }
    };

    Ok(PostFormData {
        csrf_verified,
        submission_token,
        name,
        subject,
        body,
        deletion_token,
        file,
        audio_file,
        image_file,
        poll_question,
        poll_options,
        poll_duration_secs,
        sage,
        pow_nonce,
    })
}

// ─── Upload error classifier (#6) ────────────────────────────────────────────

/// Convert an anyhow error from `save_upload` into the most appropriate
/// `AppError` variant, giving clients accurate HTTP status codes:
///   • "File too large"          → 413 `UploadTooLarge`
///   • "Insufficient disk space" → 413 `UploadTooLarge`
///   • "File type not allowed"   → 415 `InvalidMediaType`
///   • "Not an audio file"       → 415 `InvalidMediaType`
///   • anything else             → 400 `BadRequest`
pub fn classify_upload_error(e: &anyhow::Error) -> AppError {
    let msg = e.to_string();
    // Compare lower-cased so minor wording changes in save_upload don't silently
    // fall through to a generic 400 instead of the correct 413 / 415.
    let lower = msg.to_ascii_lowercase();
    if lower.starts_with("file too large") || lower.starts_with("insufficient disk space") {
        AppError::UploadTooLarge(msg)
    } else if lower.starts_with("file type not allowed") || lower.starts_with("not an audio file") {
        AppError::InvalidMediaType(msg)
    } else {
        AppError::BadRequest(msg)
    }
}

// ─── Shared media upload processing (R2-2) ───────────────────────────────────
//
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
#[expect(clippy::too_many_arguments)]
// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[expect(clippy::too_many_lines)]
pub fn process_primary_upload(
    file_data: Option<(TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    upload_dir: &str,
    save_root: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<(Option<crate::utils::files::UploadedFile>, Option<String>)> {
    let Some((upload, fname)) = file_data else {
        return Ok((None, None));
    };
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let detected_mime = crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        ffprobe_available,
        allow_any_files,
    )
    .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let detected_media = crate::models::MediaType::from_mime(&detected_mime);

    match detected_media {
        crate::models::MediaType::Image if !board.allow_images => {
            return Err(AppError::BadRequest(
                "Image uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Video if !board.allow_video => {
            return Err(AppError::BadRequest(
                "Video uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Audio if !board.allow_audio => {
            return Err(AppError::BadRequest(
                "Audio uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Pdf if !board.allow_pdf => {
            return Err(AppError::BadRequest(
                "PDF uploads are disabled on this board.".into(),
            ))
        }
        crate::models::MediaType::Other if !allow_any_files => {
            return Err(AppError::BadRequest(
                "This board only accepts image, video, audio, or PDF uploads.".into(),
            ))
        }
        crate::models::MediaType::Image
        | crate::models::MediaType::Video
        | crate::models::MediaType::Audio
        | crate::models::MediaType::Pdf
        | crate::models::MediaType::Other => {}
    }

    crate::utils::files::validate_upload_from_path(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        upload.size_bytes,
        &crate::utils::files::SaveUploadOptions {
            original_filename: &fname,
            boards_dir: save_root,
            board_short: &board.short_name,
            thumb_size,
            max_image_size,
            max_video_size,
            max_audio_size,
            ffmpeg_available,
            ffprobe_available,
            ffmpeg_webp_available,
            allow_any_files,
        },
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
    if let Some(cached) = crate::db::find_file_by_hash(conn, &hash)? {
        let file_ok = std::path::Path::new(upload_dir)
            .join(&cached.file_path)
            .exists();
        let thumb_ok = cached.thumb_path.is_empty()
            || std::path::Path::new(upload_dir)
                .join(&cached.thumb_path)
                .exists();

        if file_ok && thumb_ok {
            let cached_media = crate::models::MediaType::from_mime(&cached.mime_type);
            return Ok((
                Some(crate::utils::files::UploadedFile {
                    file_path: cached.file_path,
                    thumb_path: cached.thumb_path,
                    original_name: crate::utils::sanitize::sanitize_filename(&fname),
                    mime_type: cached.mime_type,
                    file_size: i64::try_from(upload.size_bytes).unwrap_or(0),
                    media_type: cached_media,
                    processing_pending: false,
                    dedup_reused: true,
                }),
                None,
            ));
        }

        // One or both paths are gone — the entry is stale.  Log and fall
        // through so the file is re-saved and the cache is updated below.
        tracing::debug!(
            "dedup cache miss (files deleted): file_ok={file_ok} thumb_ok={thumb_ok}, \
             re-processing upload for hash {hash}"
        );
    }

    let f = crate::utils::files::save_upload_from_path(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        upload.size_bytes,
        &crate::utils::files::SaveUploadOptions {
            original_filename: &fname,
            boards_dir: save_root,
            board_short: &board.short_name,
            thumb_size,
            max_image_size,
            max_video_size,
            max_audio_size,
            ffmpeg_available,
            ffprobe_available,
            ffmpeg_webp_available,
            allow_any_files,
        },
    )
    .map_err(|e| classify_upload_error(&e))?;
    Ok((Some(f), Some(hash)))
}

fn temp_upload_mime(
    upload: &TempUpload,
    ffprobe_available: bool,
    allow_any_files: bool,
) -> Result<String> {
    crate::utils::files::classify_upload_mime(
        upload.temp_file.path(),
        &upload.sniff_bytes,
        ffprobe_available,
        allow_any_files,
    )
    .map_err(|error| AppError::BadRequest(error.to_string()))
}

/// Process the secondary audio file for an image+audio combo upload.
/// `primary_upload` must already be the processed primary image.
///
/// Returns `Ok(None)` when `audio_file_data` is `None`.
/// Must be called from inside a `spawn_blocking` closure.
pub fn process_audio_combo(
    audio_file_data: Option<(TempUpload, String)>,
    primary_upload: Option<&crate::utils::files::UploadedFile>,
    board: &Board,
    upload_dir: &str,
    max_audio_size: usize,
    ffprobe_available: bool,
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
        ffprobe_available,
    )
    .map_err(|e| classify_upload_error(&e))?;

    // Use the image thumbnail as the audio's visual.
    if let Some(img) = primary_upload {
        aud_file.thumb_path.clone_from(&img.thumb_path);
    }
    Ok(Some(aud_file))
}

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[expect(clippy::too_many_arguments)]
pub fn process_audio_first_uploads(
    audio_file_data: Option<(TempUpload, String)>,
    image_file_data: Option<(TempUpload, String)>,
    fallback_file_data: Option<(TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    upload_dir: &str,
    save_root_str: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<(
    Option<crate::utils::files::UploadedFile>,
    Option<crate::utils::files::UploadedFile>,
    Option<String>,
)> {
    let allow_any_files =
        crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files;
    let has_audio_or_image_upload = audio_file_data.is_some() || image_file_data.is_some();
    let save_primary = |file_data| {
        process_primary_upload(
            file_data,
            board,
            conn,
            upload_dir,
            save_root_str,
            thumb_size,
            max_image_size,
            max_video_size,
            max_audio_size,
            ffmpeg_available,
            ffprobe_available,
            ffmpeg_webp_available,
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
            ffprobe_available,
        )?;

        return Ok((primary, audio, primary_hash));
    }

    if let Some((audio_upload, audio_name)) = audio_file_data {
        let audio_mime = temp_upload_mime(&audio_upload, ffprobe_available, allow_any_files)?;
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

/// Enqueue background media-processing and spam-check jobs for a newly created
/// post.  Shared by `create_thread` and `post_reply`.
pub fn enqueue_post_jobs(
    job_queue: &JobQueue,
    conn: &rusqlite::Connection,
    post_id: i64,
    ip_hash: &str,
    body_len: usize,
    uploaded: Option<&crate::utils::files::UploadedFile>,
    board_short: &str,
) {
    // 1. Media post-processing (video transcode / audio waveform)
    if let Some(up) = uploaded {
        if up.processing_pending {
            let job = match up.media_type {
                crate::models::MediaType::Video => Some(crate::workers::Job::VideoTranscode {
                    post_id,
                    file_path: up.file_path.clone(),
                    board_short: board_short.to_owned(),
                }),
                crate::models::MediaType::Audio => Some(crate::workers::Job::AudioWaveform {
                    post_id,
                    file_path: up.file_path.clone(),
                    board_short: board_short.to_owned(),
                }),
                crate::models::MediaType::Image
                | crate::models::MediaType::Pdf
                | crate::models::MediaType::Other => None,
            };
            if let Some(j) = job {
                match job_queue.enqueue(&j) {
                    Ok(crate::workers::EnqueueOutcome::Enqueued(_)) => {
                        if let Err(error) = crate::db::set_post_media_processing_state(
                            conn,
                            post_id,
                            Some(crate::db::MEDIA_PROCESSING_PENDING),
                            None,
                        ) {
                            tracing::warn!(
                                post_id,
                                error = %error,
                                "Failed to mark post media processing as pending"
                            );
                        }
                    }
                    Ok(crate::workers::EnqueueOutcome::DroppedAtCapacity) => {
                        let detail = "Background media queue is full; upload kept original file but deferred processing was skipped.";
                        tracing::warn!(post_id, "Media job dropped at queue capacity");
                        if let Err(error) = crate::db::set_post_media_processing_state(
                            conn,
                            post_id,
                            Some(crate::db::MEDIA_PROCESSING_FAILED),
                            Some(detail),
                        ) {
                            tracing::warn!(
                                post_id,
                                error = %error,
                                "Failed to persist queue-capacity media failure"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to enqueue media job: {e}");
                        if let Err(error) = crate::db::set_post_media_processing_state(
                            conn,
                            post_id,
                            Some(crate::db::MEDIA_PROCESSING_FAILED),
                            Some(&format!("Could not enqueue background media job: {e}")),
                        ) {
                            tracing::warn!(
                                post_id,
                                error = %error,
                                "Failed to persist media enqueue failure"
                            );
                        }
                    }
                }
            }
        }
    }

    // 2. Spam analysis
    let _ = job_queue.enqueue(&crate::workers::Job::SpamCheck {
        post_id,
        ip_hash: ip_hash.to_owned(),
        body_len,
    });
}

#[cfg(test)]
mod tests {
    use super::{parse_post_multipart, process_audio_first_uploads, TempUpload};
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use sha2::Digest as _;
    use tower::ServiceExt as _;

    const MIB: i64 = 1024 * 1024;

    fn sample_board() -> crate::models::Board {
        crate::models::Board {
            allow_any_files: true,
            ..crate::test_fixtures::sample_board()
        }
    }

    fn temp_upload(name: &str, bytes: &[u8]) -> (TempUpload, String) {
        let temp_file = tempfile::Builder::new()
            .prefix("rustchan-test-upload-")
            .tempfile()
            .expect("temp upload");
        std::fs::write(temp_file.path(), bytes).expect("write temp upload");
        (
            TempUpload {
                temp_file,
                sniff_bytes: bytes.to_vec(),
                size_bytes: bytes.len(),
            },
            name.to_owned(),
        )
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

    fn create_file_hash_table(conn: &rusqlite::Connection) {
        conn.execute(
            "CREATE TABLE file_hashes (
                sha256 TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                thumb_path TEXT NOT NULL DEFAULT '',
                mime_type TEXT NOT NULL DEFAULT ''
            )",
            [],
        )
        .expect("create file_hashes");
    }

    async fn parse_scaled_audio_limit(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        let form = parse_post_multipart(multipart, Some("csrf123"), 1_024, 1_024, 5_000).await?;
        let (upload, _) = form.audio_file.expect("audio upload");
        assert_eq!(upload.size_bytes, 4_500);
        Ok("ok")
    }

    async fn parse_scaled_audio_oversize(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        parse_post_multipart(multipart, Some("csrf123"), 1_024, 1_024, 5_000).await?;
        Ok("ok")
    }

    #[test]
    fn board_specific_audio_limit_500_mib_is_not_clamped_to_default() {
        let board = crate::models::Board {
            allow_audio: true,
            max_audio_size: 500 * MIB,
            ..crate::test_fixtures::sample_board()
        };

        let limit = board.max_audio_size_bytes();
        let upload_size = 450usize * 1024 * 1024;

        assert_eq!(limit, 500usize * 1024 * 1024);
        assert!(limit > 150usize * 1024 * 1024);
        assert!(limit > upload_size + 1024 * 1024);
    }

    #[tokio::test]
    async fn multipart_parser_accepts_audio_within_board_specific_limit() {
        let router = Router::new().route("/parse", post(parse_scaled_audio_limit));
        let audio = vec![b'a'; 4_500];
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "audio post")],
            Some(("audio_file", "track.mp3", &audio, "audio/mpeg")),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn multipart_parser_rejects_oversized_audio_cleanly() {
        let router = Router::new().route("/parse", post(parse_scaled_audio_oversize));
        let audio = vec![b'a'; 5_001];
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "audio post")],
            Some(("audio_file", "track.mp3", &audio, "audio/mpeg")),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    async fn parse_default_limits(
        multipart: axum::extract::Multipart,
    ) -> crate::error::Result<&'static str> {
        parse_post_multipart(multipart, Some("csrf123"), 1_024, 1_024, 1_024).await?;
        Ok("ok")
    }

    fn multipart_body_with_files(
        fields: &[(&str, &str)],
        files: &[(&str, &str, &[u8], &str)],
    ) -> (String, Vec<u8>) {
        let boundary = "rustchan-test-boundary".to_owned();
        let mut body = Vec::new();

        for (name, value) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        }

        for (field_name, filename, contents, content_type) in files {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n"
                )
                .as_bytes(),
            );
            body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
            body.extend_from_slice(contents);
            body.extend_from_slice(b"\r\n");
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (boundary, body)
    }

    #[tokio::test]
    async fn multipart_parser_rejects_duplicate_upload_slot_before_second_body() {
        let router = Router::new().route("/parse", post(parse_default_limits));
        let first = b"one";
        let second = vec![b'a'; 10_000];
        let (boundary, body) = multipart_body_with_files(
            &[("_csrf", "csrf123"), ("body", "duplicate")],
            &[
                ("file", "one.bin", first, "application/octet-stream"),
                ("file", "two.bin", &second, "application/octet-stream"),
            ],
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn public_multipart_budget_enforces_aggregate_bytes_and_field_count() {
        let mut budget = super::PublicMultipartBudget::default();
        for _ in 0..super::PUBLIC_MULTIPART_MAX_FIELDS {
            budget.note_field().expect("field within limit");
        }
        assert!(budget.note_field().is_err());

        let mut budget = super::PublicMultipartBudget::default();
        budget
            .note_chunk(super::PUBLIC_MULTIPART_AGGREGATE_MAX_BYTES)
            .expect("aggregate limit inclusive");
        assert!(budget.note_chunk(1).is_err());
    }

    #[test]
    fn audio_first_flow_rejects_mixing_other_slot_with_audio_or_image_slots() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        let board = sample_board();
        let audio = temp_upload("track.flac", b"fLaC\x00\x00\x00\x22test");
        let other = temp_upload("clip.webm", b"\x1a\x45\xdf\xa3webm");
        let boards_dir = tempfile::tempdir().expect("boards dir");
        let uploads_dir = tempfile::tempdir().expect("uploads dir");

        let result = process_audio_first_uploads(
            Some(audio),
            None,
            Some(other),
            &board,
            &conn,
            boards_dir.path().to_str().expect("boards dir path"),
            uploads_dir.path().to_str().expect("uploads dir path"),
            150,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        match result {
            Ok(_) => panic!("mixed upload modes should be rejected"),
            Err(error) => assert!(error
                .to_string()
                .contains("Use either the audio/image upload flow or the other-file slot")),
        }
    }

    #[test]
    fn primary_upload_rejects_malformed_image_even_when_hash_is_cached() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        create_file_hash_table(&conn);

        let board = crate::test_fixtures::sample_board();
        let uploads_dir = tempfile::tempdir().expect("uploads dir");
        let save_root = tempfile::tempdir().expect("save root");
        let malformed = b"\x89PNG\r\n\x1a\nthis is not a complete png";
        let upload = temp_upload("broken.png", malformed);

        let mut hasher = sha2::Sha256::new();
        hasher.update(malformed);
        let hash = hex::encode(hasher.finalize());

        let board_dir = uploads_dir.path().join(&board.short_name);
        let thumbs_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumbs_dir).expect("create thumb dir");
        std::fs::write(board_dir.join("cached.png"), malformed).expect("write cached file");
        std::fs::write(thumbs_dir.join("cached.webp"), b"fake thumb").expect("write cached thumb");
        crate::db::record_file_hash(
            &conn,
            &hash,
            &format!("{}/cached.png", board.short_name),
            &format!("{}/thumbs/cached.webp", board.short_name),
            "image/png",
        )
        .expect("record hash");

        let result = super::process_primary_upload(
            Some(upload),
            &board,
            &conn,
            uploads_dir.path().to_str().expect("uploads dir path"),
            save_root.path().to_str().expect("save root path"),
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        match result {
            Ok(_) => panic!("malformed image should be rejected before dedup reuse"),
            Err(error) => assert!(error.to_string().contains("image header is malformed")),
        }
    }

    #[test]
    fn primary_upload_rejects_pdf_when_board_disables_pdf() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        create_file_hash_table(&conn);

        let board = crate::models::Board {
            allow_pdf: false,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().expect("uploads dir");
        let save_root = tempfile::tempdir().expect("save root");
        let result = super::process_primary_upload(
            Some(temp_upload("doc.pdf", valid_pdf())),
            &board,
            &conn,
            uploads_dir.path().to_str().expect("uploads dir path"),
            save_root.path().to_str().expect("save root path"),
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        match result {
            Ok(_) => panic!("PDF upload should be rejected when disabled"),
            Err(error) => assert!(error.to_string().contains("PDF uploads are disabled")),
        }
        assert!(!save_root.path().join(&board.short_name).exists());
    }

    #[test]
    fn primary_upload_rejects_renamed_non_pdf() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        create_file_hash_table(&conn);

        let board = crate::models::Board {
            allow_pdf: true,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().expect("uploads dir");
        let save_root = tempfile::tempdir().expect("save root");
        let result = super::process_primary_upload(
            Some(temp_upload("not-really.pdf", b"plain text")),
            &board,
            &conn,
            uploads_dir.path().to_str().expect("uploads dir path"),
            save_root.path().to_str().expect("save root path"),
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        );

        match result {
            Ok(_) => panic!("renamed non-PDF should be rejected"),
            Err(error) => assert!(error.to_string().contains("File type not allowed")),
        }
        assert!(!save_root.path().join(&board.short_name).exists());
    }

    #[test]
    fn primary_upload_accepts_pdf_when_board_enables_pdf() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory sqlite");
        create_file_hash_table(&conn);

        let board = crate::models::Board {
            allow_pdf: true,
            ..crate::test_fixtures::sample_board()
        };
        let uploads_dir = tempfile::tempdir().expect("uploads dir");
        let save_root = tempfile::tempdir().expect("save root");
        let _override = crate::media::thumbnail::override_pdf_renderer_mode(
            crate::media::thumbnail::TestPdfRendererMode::Unavailable,
        );
        let (uploaded, _) = super::process_primary_upload(
            Some(temp_upload("doc.pdf", valid_pdf())),
            &board,
            &conn,
            uploads_dir.path().to_str().expect("uploads dir path"),
            save_root.path().to_str().expect("save root path"),
            64,
            1024 * 1024,
            1024 * 1024,
            1024 * 1024,
            false,
            false,
            false,
        )
        .expect("PDF upload accepted");
        let uploaded = uploaded.expect("uploaded PDF");

        assert_eq!(uploaded.mime_type, "application/pdf");
        assert_eq!(uploaded.media_type, crate::models::MediaType::Pdf);
        assert!(save_root.path().join(uploaded.file_path).exists());
        assert!(std::path::Path::new(&uploaded.thumb_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")));
        assert!(save_root.path().join(&uploaded.thumb_path).exists());
    }
}
