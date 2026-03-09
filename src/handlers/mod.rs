pub mod admin;
pub mod board;
pub mod thread;

// ─── Shared multipart form parsing ───────────────────────────────────────────
//
// Both create_thread and post_reply parse the same multipart fields.
// This helper consolidates that duplicated logic into one place.

use crate::error::{AppError, Result};
use crate::middleware::validate_csrf;
use crate::workers::JobQueue;
use axum::extract::Multipart;

/// Parsed fields from a post/thread creation multipart form.
pub struct PostFormData {
    pub csrf_verified: bool,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    /// Raw bytes + original filename if a file was attached.
    pub file: Option<(Vec<u8>, String)>,
    /// Secondary audio file for image+audio combo uploads.
    pub audio_file: Option<(Vec<u8>, String)>,
    // ── Poll fields (only used when creating a new thread) ────────────────
    pub poll_question: String,
    pub poll_options: Vec<String>,
    /// Duration in seconds (parsed from value + unit)
    pub poll_duration_secs: Option<i64>,
    /// Sage — when true the reply must not bump the thread.
    pub sage: bool,
    /// PoW CAPTCHA nonce — submitted by the thread-creation form when enabled.
    pub pow_nonce: String,
}

/// Drain all fields from a multipart form into [`PostFormData`].
/// `csrf_cookie` is the value from the browser cookie for CSRF verification.
pub async fn parse_post_multipart(
    mut multipart: Multipart,
    csrf_cookie: Option<&str>,
) -> Result<PostFormData> {
    let mut csrf_verified = false;
    let mut name = String::new();
    let mut subject = String::new();
    let mut body = String::new();
    let mut deletion_token = String::new();
    let mut file: Option<(Vec<u8>, String)> = None;
    let mut audio_file: Option<(Vec<u8>, String)> = None;
    let mut poll_question = String::new();
    let mut poll_options: Vec<String> = Vec::new();
    let mut poll_duration_value: Option<i64> = None;
    let mut poll_duration_unit = String::from("hours");
    let mut sage = false;
    let mut pow_nonce = String::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => {
                let v = field.text().await.unwrap_or_default();
                if validate_csrf(csrf_cookie, &v) {
                    csrf_verified = true;
                }
            }
            Some("name") => name = field.text().await.unwrap_or_default(),
            Some("subject") => subject = field.text().await.unwrap_or_default(),
            Some("body") => body = field.text().await.unwrap_or_default(),
            Some("deletion_token") => deletion_token = field.text().await.unwrap_or_default(),
            Some("sage") => {
                let v = field.text().await.unwrap_or_default();
                sage = v == "1" || v.eq_ignore_ascii_case("on") || v.eq_ignore_ascii_case("true");
            }
            Some("pow_nonce") => pow_nonce = field.text().await.unwrap_or_default(),
            Some("poll_question") => {
                let v = field.text().await.unwrap_or_default();
                // CRIT-8: Enforce server-side length cap on poll question.
                if v.chars().count() > 500 {
                    return Err(AppError::BadRequest(
                        "Poll question must be 500 characters or fewer.".into(),
                    ));
                }
                poll_question = v;
            }
            Some("poll_option") => {
                let v = field.text().await.unwrap_or_default();
                let trimmed = v.trim().to_string();
                // CRIT-8: Enforce server-side caps on option count and length.
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
                let v = field.text().await.unwrap_or_default();
                poll_duration_value = v.trim().parse::<i64>().ok();
            }
            Some("poll_duration_unit") => {
                poll_duration_unit = field.text().await.unwrap_or_default();
            }
            Some("file") => {
                let fname = field.file_name().unwrap_or("upload").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("File read error: {e}")))?;
                if !bytes.is_empty() {
                    file = Some((bytes.to_vec(), fname));
                }
            }
            Some("audio_file") => {
                let fname = field.file_name().unwrap_or("audio").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("Audio file read error: {e}")))?;
                if !bytes.is_empty() {
                    audio_file = Some((bytes.to_vec(), fname));
                }
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    // Convert duration value + unit → seconds (saturating to prevent overflow).
    // The unit is validated against an explicit allow-list (case-insensitive) so
    // that a tampered form field does not silently multiply by an arbitrary factor.
    let poll_duration_secs = if !poll_question.trim().is_empty() {
        match poll_duration_value {
            None => None,
            Some(v) => {
                let unit = poll_duration_unit.trim().to_ascii_lowercase();
                let secs = match unit.as_str() {
                    "minutes" => v.saturating_mul(60),
                    "hours" => v.saturating_mul(3600),
                    "days" => v.saturating_mul(86_400),
                    other => {
                        return Err(AppError::BadRequest(format!(
                            "Invalid poll duration unit '{}'. Use 'minutes', 'hours', or 'days'.",
                            other
                        )));
                    }
                };
                Some(secs)
            }
        }
    } else {
        None
    };

    Ok(PostFormData {
        csrf_verified,
        name,
        subject,
        body,
        deletion_token,
        file,
        audio_file,
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
///   • "File too large"          → 413 UploadTooLarge
///   • "Insufficient disk space" → 413 UploadTooLarge
///   • "File type not allowed"   → 415 InvalidMediaType
///   • "Not an audio file"       → 415 InvalidMediaType
///   • anything else             → 400 BadRequest
pub fn classify_upload_error(e: anyhow::Error) -> AppError {
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
pub fn process_primary_upload(
    file_data: Option<(Vec<u8>, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    upload_dir: &str,
    thumb_size: u32,
    max_image_size: usize,
    max_video_size: usize,
    max_audio_size: usize,
    ffmpeg_available: bool,
) -> Result<Option<crate::utils::files::UploadedFile>> {
    let (data, fname) = match file_data {
        Some(f) => f,
        None => return Ok(None),
    };

    // Magic-byte detection for accurate type enforcement.
    let detected_mime = crate::utils::files::detect_mime_type(&data)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let detected_media = crate::models::MediaType::from_mime(detected_mime)
        .ok_or_else(|| AppError::BadRequest("Unsupported file type.".into()))?;

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
        _ => {}
    }

    // SHA-256 deduplication — serve the cached entry without re-saving.
    let hash = crate::utils::crypto::sha256_hex(&data);
    if let Some(cached) = crate::db::find_file_by_hash(conn, &hash)? {
        let cached_media = crate::models::MediaType::from_mime(&cached.mime_type)
            .unwrap_or(crate::models::MediaType::Image);
        return Ok(Some(crate::utils::files::UploadedFile {
            file_path: cached.file_path,
            thumb_path: cached.thumb_path,
            original_name: crate::utils::sanitize::sanitize_filename(&fname),
            mime_type: cached.mime_type,
            file_size: data.len() as i64,
            media_type: cached_media,
            processing_pending: false,
        }));
    }

    let f = crate::utils::files::save_upload(
        &data,
        &fname,
        upload_dir,
        &board.short_name,
        thumb_size,
        max_image_size,
        max_video_size,
        max_audio_size,
        ffmpeg_available,
    )
    .map_err(classify_upload_error)?;
    crate::db::record_file_hash(conn, &hash, &f.file_path, &f.thumb_path, &f.mime_type)?;
    Ok(Some(f))
}

/// Process the secondary audio file for an image+audio combo upload.
/// `primary_upload` must already be the processed primary image.
///
/// Returns `Ok(None)` when `audio_file_data` is `None`.
/// Must be called from inside a `spawn_blocking` closure.
pub fn process_audio_combo(
    audio_file_data: Option<(Vec<u8>, String)>,
    primary_upload: Option<&crate::utils::files::UploadedFile>,
    board: &Board,
    upload_dir: &str,
    max_audio_size: usize,
) -> Result<Option<crate::utils::files::UploadedFile>> {
    let (aud_data, aud_fname) = match audio_file_data {
        Some(f) => f,
        None => return Ok(None),
    };

    if !board.allow_audio {
        return Err(AppError::BadRequest(
            "Audio uploads are disabled on this board.".into(),
        ));
    }

    // Audio combo requires the primary file to be an image.
    let primary_is_image = primary_upload
        .map(|u| matches!(u.media_type, crate::models::MediaType::Image))
        .unwrap_or(false);
    if !primary_is_image {
        return Err(AppError::BadRequest(
            "Audio can only be combined with an image upload.".into(),
        ));
    }

    // Confirm the secondary file is actually audio.
    let aud_mime = crate::utils::files::detect_mime_type(&aud_data)
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    let aud_media = crate::models::MediaType::from_mime(aud_mime)
        .ok_or_else(|| AppError::BadRequest("Unsupported audio type.".into()))?;
    if !matches!(aud_media, crate::models::MediaType::Audio) {
        return Err(AppError::BadRequest(
            "The audio slot only accepts audio files.".into(),
        ));
    }

    let mut aud_file = crate::utils::files::save_audio_with_image_thumb(
        &aud_data,
        &aud_fname,
        upload_dir,
        &board.short_name,
        max_audio_size,
    )
    .map_err(classify_upload_error)?;

    // Use the image thumbnail as the audio's visual.
    if let Some(img) = primary_upload {
        aud_file.thumb_path = img.thumb_path.clone();
    }
    Ok(Some(aud_file))
}

/// Enqueue background media-processing and spam-check jobs for a newly created
/// post.  Shared by create_thread and post_reply.
pub fn enqueue_post_jobs(
    job_queue: &JobQueue,
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
                    board_short: board_short.to_string(),
                }),
                crate::models::MediaType::Audio => Some(crate::workers::Job::AudioWaveform {
                    post_id,
                    file_path: up.file_path.clone(),
                    board_short: board_short.to_string(),
                }),
                _ => None,
            };
            if let Some(j) = job {
                if let Err(e) = job_queue.enqueue(&j) {
                    tracing::warn!("Failed to enqueue media job: {}", e);
                }
            }
        }
    }

    // 2. Spam analysis
    let _ = job_queue.enqueue(&crate::workers::Job::SpamCheck {
        post_id,
        ip_hash: ip_hash.to_string(),
        body_len,
    });
}
