//! Upload validation, staged preparation, rollback, and durable finalization.

use super::super::{TempUpload, LOG_TARGET as HANDLER_LOG_TARGET};
use super::LOG_TARGET as POSTING_LOG_TARGET;
use crate::{
    db,
    error::{AppError, Result},
    models::Board,
};

/// Map typed upload rejections to HTTP errors without inspecting diagnostic text.
/// Size/free-space failures are 413, prohibited content is 415, malformed content
/// is 400 (422 in processed posting responses), and I/O failures remain private 500s.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private upload owner preserves the existing crate-visible handler facade"
)]
pub(crate) fn classify_upload_error(error: anyhow::Error) -> AppError {
    use crate::utils::files::UploadRejection;

    match error.downcast_ref::<UploadRejection>() {
        Some(UploadRejection::TooLarge(message)) => {
            return AppError::UploadTooLarge(message.clone())
        }
        Some(UploadRejection::Disallowed(message)) => {
            return AppError::InvalidMediaType(message.clone())
        }
        Some(UploadRejection::MalformedImageHeader(_) | UploadRejection::Malformed(_)) | None => {}
    }
    if error
        .chain()
        .any(<dyn std::error::Error + 'static>::is::<std::io::Error>)
    {
        AppError::Internal(error.context("Upload filesystem operation failed"))
    } else if matches!(
        error.downcast_ref::<UploadRejection>(),
        Some(UploadRejection::MalformedImageHeader(_))
    ) {
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
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private upload owner preserves the existing crate-visible handler facade"
)]
pub(crate) fn process_primary_upload(
    file_data: Option<(TempUpload, String)>,
    board: &Board,
    mut lookup: impl FnMut(&str) -> Result<Option<db::CachedFile>>,
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
    .map_err(classify_upload_error)?;
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
    .map_err(classify_upload_error)?;

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
        tracing::debug!(target: HANDLER_LOG_TARGET,
            "dedup cache miss: same_board_cache={same_board_cache} file_ok={file_ok} thumb_ok={thumb_ok}, \
             re-processing upload for hash {hash}"
        );
    }

    let f =
        crate::utils::files::storage::save_validated_upload_from_path(validated, &upload_options)
            .map_err(classify_upload_error)?;
    Ok((Some(f), Some(hash)))
}

fn cached_paths_belong_to_board(cached: &db::CachedFile, board_short: &str) -> bool {
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
    .map_err(classify_upload_error)
}

/// Process the secondary audio file for an image+audio combo upload.
/// `primary_upload` must already be the processed primary image.
///
/// Returns `Ok(None)` when `audio_file_data` is `None`.
/// Must be called from inside a `spawn_blocking` closure.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private upload owner preserves the existing crate-visible handler facade"
)]
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
    .map_err(classify_upload_error)?;

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
#[expect(
    clippy::redundant_pub_crate,
    reason = "the private upload owner preserves the existing crate-visible handler facade"
)]
pub(crate) fn process_audio_first_uploads(
    audio_file_data: Option<(TempUpload, String)>,
    image_file_data: Option<(TempUpload, String)>,
    fallback_file_data: Option<(TempUpload, String)>,
    board: &Board,
    mut lookup: impl FnMut(&str) -> Result<Option<db::CachedFile>>,
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

pub(super) struct UploadConfig<'a> {
    pub(super) upload_dir: &'a str,
    pub(super) thumb_size: u32,
    pub(super) max_image_size: usize,
    pub(super) max_video_size: usize,
    pub(super) max_audio_size: usize,
    pub(super) max_pdf_size: usize,
    pub(super) ffmpeg_available: bool,
}

#[derive(Clone)]
struct PendingUploadFinalize {
    op_id: String,
    payload: crate::pending_fs::UploadFinalizePayload,
}

pub(super) struct ProcessedUploads {
    pub(super) primary: Option<crate::utils::files::UploadedFile>,
    pub(super) audio: Option<crate::utils::files::UploadedFile>,
    pending_finalize: Option<PendingUploadFinalize>,
}

impl ProcessedUploads {
    pub(super) fn rollback_new_files(&self, pool: &db::DbPool, upload_dir: &str) -> Result<()> {
        if let Some(pending) = self.pending_finalize.as_ref() {
            let stage_dir = std::path::Path::new(&pending.payload.stage_dir);
            if stage_dir.exists() {
                std::fs::remove_dir_all(stage_dir).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!(
                        "Rollback cleanup incomplete: failed to remove upload stage {}: {error}",
                        stage_dir.display()
                    ))
                })?;
            }
            return Ok(());
        }

        let mut cleanup_errors = Vec::new();

        if let Some(primary) = self.primary.as_ref().filter(|file| !file.dedup_reused) {
            if !primary.thumb_path.is_empty() {
                if let Err(error) =
                    crate::utils::files::delete_file_checked(upload_dir, &primary.thumb_path)
                {
                    cleanup_errors.push(error);
                }
            }
            match crate::utils::files::delete_file_checked(upload_dir, &primary.file_path) {
                Ok(()) => {
                    if let Err(error) =
                        db::delete_file_hash_by_path(&*pool.get()?, &primary.file_path)
                    {
                        return Err(AppError::Internal(error));
                    }
                }
                Err(error) => cleanup_errors.push(error),
            }
        }

        if let Some(audio) = self.audio.as_ref().filter(|file| !file.dedup_reused) {
            if let Err(error) =
                crate::utils::files::delete_file_checked(upload_dir, &audio.file_path)
            {
                cleanup_errors.push(error);
            }
        }

        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            let detail = cleanup_errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            Err(AppError::Internal(anyhow::anyhow!(
                "Rollback cleanup incomplete: {detail}"
            )))
        }
    }
}

fn cleanup_unused_upload_stage(stage_root: Option<&std::path::Path>) {
    let Some(stage_dir) = stage_root else {
        return;
    };
    if !stage_dir.exists() {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(stage_dir) {
        tracing::warn!(target: POSTING_LOG_TARGET,
            stage_dir = %stage_dir.display(),
            error = %error,
            "failed to clean unused upload stage directory"
        );
    }
}

fn build_upload_finalize_payload(
    stage_dir: &std::path::Path,
    mut primary: Option<&mut crate::utils::files::UploadedFile>,
    audio: Option<&mut crate::utils::files::UploadedFile>,
    primary_hash: Option<String>,
) -> Result<Option<crate::pending_fs::UploadFinalizePayload>> {
    let mut relative_paths = Vec::new();
    let mut optional_paths = Vec::new();
    let mut artifact_sha256 = std::collections::BTreeMap::new();

    if let Some(file) = primary.as_deref().filter(|file| !file.dedup_reused) {
        relative_paths.push(file.file_path.clone());
    }

    if let Some(file) = audio.as_deref().filter(|file| !file.dedup_reused) {
        relative_paths.push(file.file_path.clone());
    }

    relative_paths.sort_unstable();
    relative_paths.dedup();

    for relative_path in &relative_paths {
        let _previous_value = artifact_sha256.insert(
            relative_path.clone(),
            crate::pending_fs::staged_upload_artifact_sha256(stage_dir, relative_path, "required")
                .map_err(AppError::Internal)?,
        );
    }

    if let Some(file) = primary.as_deref_mut().filter(|file| !file.dedup_reused) {
        if !file.thumb_path.is_empty() {
            let staged_path = stage_dir.join(&file.thumb_path);
            match std::fs::symlink_metadata(&staged_path) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    optional_paths.push(file.thumb_path.clone());
                    let _previous_value = artifact_sha256.insert(
                        file.thumb_path.clone(),
                        crate::pending_fs::staged_upload_artifact_sha256(
                            stage_dir,
                            &file.thumb_path,
                            "optional",
                        )
                        .map_err(AppError::Internal)?,
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tracing::warn!(target: POSTING_LOG_TARGET,
                        artifact = "optional_thumbnail",
                        outcome = "omitted_before_commit",
                        "generated thumbnail was absent; preserving upload without it"
                    );
                    file.thumb_path.clear();
                }
                Ok(_) => {
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "Optional staged upload artifact is not a regular file"
                    )));
                }
                Err(error) => {
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "Optional staged upload artifact cannot be inspected: {error}"
                    )));
                }
            }
        }
    }

    if primary
        .as_deref()
        .is_some_and(|primary| primary.thumb_path.is_empty())
    {
        if let Some(audio) = audio {
            audio.thumb_path.clear();
        }
    }

    let payload = (!relative_paths.is_empty()).then(|| crate::pending_fs::UploadFinalizePayload {
        stage_dir: stage_dir.display().to_string(),
        relative_paths,
        optional_paths,
        artifact_sha256,
        primary_hash,
        primary_file_path: primary
            .as_deref()
            .filter(|file| !file.dedup_reused)
            .map(|file| file.file_path.clone()),
        primary_thumb_path: primary
            .as_deref()
            .filter(|file| !file.dedup_reused)
            .and_then(|file| (!file.thumb_path.is_empty()).then(|| file.thumb_path.clone())),
        primary_mime_type: primary
            .as_deref()
            .filter(|file| !file.dedup_reused)
            .map(|file| file.mime_type.clone()),
    });
    if let Some(payload) = payload.as_ref() {
        let upload_root = stage_dir
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("Upload stage has no managed root"))
            })?;
        crate::pending_fs::validate_upload_finalize_payload(upload_root, payload)
            .map_err(AppError::Internal)?;
    }
    Ok(payload)
}

pub(super) fn build_pending_upload_op(
    uploads: &ProcessedUploads,
) -> Result<Option<crate::pending_fs::PendingFsOpInsert>> {
    let Some(pending) = uploads.pending_finalize.as_ref() else {
        return Ok(None);
    };

    Ok(Some(crate::pending_fs::PendingFsOpInsert {
        id: pending.op_id.clone(),
        kind: crate::pending_fs::UPLOAD_FINALIZE_KIND,
        payload_json: serde_json::to_string(&pending.payload).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Serialize upload finalize payload: {error}"
            ))
        })?,
    }))
}

pub(super) fn finalize_pending_uploads(
    pool: &db::DbPool,
    upload_dir: &str,
    uploads: &ProcessedUploads,
) {
    let Some(pending) = uploads.pending_finalize.as_ref() else {
        return;
    };

    match crate::pending_fs::finalize_upload_payload_with_pool(
        pool,
        upload_dir,
        Some(&pending.op_id),
        &pending.payload,
    ) {
        Ok(()) => {}
        Err(error) => {
            tracing::error!(target: POSTING_LOG_TARGET,
                op_id = %pending.op_id,
                error = %error,
                "upload finalization failed; leaving pending_fs_op for startup reconciliation"
            );
        }
    }
}

pub(super) fn process_uploads(
    image_file_data: Option<(TempUpload, String)>,
    file_data: Option<(TempUpload, String)>,
    audio_file_data: Option<(TempUpload, String)>,
    board: &Board,
    pool: &db::DbPool,
    config: &UploadConfig<'_>,
) -> Result<ProcessedUploads> {
    let stage_root =
        (file_data.is_some() || audio_file_data.is_some() || image_file_data.is_some())
            .then(|| crate::pending_fs::create_stage_root(config.upload_dir, "upload"))
            .transpose()
            .map_err(AppError::Internal)?;
    let save_root = stage_root
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new(config.upload_dir));
    let save_root_str = save_root
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Upload stage root is non-UTF-8")))?;

    let processed = process_audio_first_uploads(
        audio_file_data,
        image_file_data,
        file_data,
        board,
        |hash| {
            let conn = pool.get()?;
            Ok(db::find_file_by_hash(&conn, hash)?)
        },
        config.upload_dir,
        save_root_str,
        config.thumb_size,
        config.max_image_size,
        config.max_video_size,
        config.max_audio_size,
        config.max_pdf_size,
        config.ffmpeg_available,
    );

    let (mut primary, mut audio, primary_hash) = match processed {
        Ok(processed) => processed,
        Err(error) => {
            cleanup_unused_upload_stage(stage_root.as_deref());
            return Err(error);
        }
    };

    let pending_payload = stage_root
        .as_ref()
        .map(|stage_dir| {
            build_upload_finalize_payload(stage_dir, primary.as_mut(), audio.as_mut(), primary_hash)
        })
        .transpose();
    let pending_finalize = match pending_payload {
        Ok(payload) => payload,
        Err(error) => {
            cleanup_unused_upload_stage(stage_root.as_deref());
            return Err(error);
        }
    }
    .flatten()
    .map(|payload| PendingUploadFinalize {
        op_id: uuid::Uuid::new_v4().to_string(),
        payload,
    });

    if pending_finalize.is_none() {
        cleanup_unused_upload_stage(stage_root.as_deref());
    }

    Ok(ProcessedUploads {
        primary,
        audio,
        pending_finalize,
    })
}

#[cfg(test)]
#[path = "tests/upload_rejections.rs"]
mod tests;
