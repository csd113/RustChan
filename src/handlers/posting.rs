// src/handlers/posting.rs

use crate::{
    db,
    error::{AppError, Result},
    models::Board,
    utils::{
        crypto::new_deletion_token,
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name,
        },
        tripcode::parse_name_tripcode,
    },
};

use crate::db::NewPost;

pub struct UploadConfig<'a> {
    pub upload_dir: &'a str,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub ffmpeg_available: bool,
    pub ffmpeg_webp_available: bool,
}

#[derive(Clone)]
pub struct PendingUploadFinalize {
    pub op_id: String,
    pub payload: crate::pending_fs::UploadFinalizePayload,
}

pub struct ProcessedUploads {
    pub primary: Option<crate::utils::files::UploadedFile>,
    pub audio: Option<crate::utils::files::UploadedFile>,
    pub pending_finalize: Option<PendingUploadFinalize>,
}

impl ProcessedUploads {
    pub fn rollback_new_files(&self, conn: &rusqlite::Connection, upload_dir: &str) -> Result<()> {
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
                    if let Err(error) = db::delete_file_hash_by_path(conn, &primary.file_path) {
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

fn build_upload_finalize_payload(
    stage_dir: &std::path::Path,
    primary: Option<&crate::utils::files::UploadedFile>,
    audio: Option<&crate::utils::files::UploadedFile>,
    primary_hash: Option<String>,
) -> Option<crate::pending_fs::UploadFinalizePayload> {
    let mut relative_paths = Vec::new();

    if let Some(file) = primary.filter(|file| !file.dedup_reused) {
        relative_paths.push(file.file_path.clone());
        if !file.thumb_path.is_empty() {
            relative_paths.push(file.thumb_path.clone());
        }
    }

    if let Some(file) = audio.filter(|file| !file.dedup_reused) {
        relative_paths.push(file.file_path.clone());
    }

    relative_paths.sort_unstable();
    relative_paths.dedup();

    (!relative_paths.is_empty()).then(|| crate::pending_fs::UploadFinalizePayload {
        stage_dir: stage_dir.display().to_string(),
        relative_paths,
        primary_hash,
        primary_file_path: primary.map(|file| file.file_path.clone()),
        primary_thumb_path: primary.map(|file| file.thumb_path.clone()),
        primary_mime_type: primary.map(|file| file.mime_type.clone()),
    })
}

pub fn build_pending_upload_op(
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

pub fn finalize_pending_uploads(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    uploads: &ProcessedUploads,
) {
    let Some(pending) = uploads.pending_finalize.as_ref() else {
        return;
    };

    match crate::pending_fs::finalize_upload_payload(conn, upload_dir, &pending.payload) {
        Ok(()) => {
            if let Err(error) = crate::db::delete_pending_fs_op(conn, &pending.op_id) {
                tracing::error!(
                    op_id = %pending.op_id,
                    error = %error,
                    "finalized upload files but failed to clear pending_fs_op"
                );
            }
        }
        Err(error) => {
            tracing::error!(
                op_id = %pending.op_id,
                error = %error,
                "upload finalization failed; leaving pending_fs_op for startup reconciliation"
            );
        }
    }
}

pub fn is_admin_session(conn: &rusqlite::Connection, admin_session_id: Option<&str>) -> bool {
    admin_session_id.is_some_and(|sid| db::get_session(conn, sid).ok().flatten().is_some())
}

pub fn load_word_filters(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>> {
    Ok(db::get_word_filters(conn)?
        .into_iter()
        .map(|f| (f.pattern, f.replacement))
        .collect())
}

pub fn resolve_post_identity(raw_name: &str, allow_tripcodes: bool) -> (String, Option<String>) {
    let (name, tripcode) = parse_name_tripcode(&validate_name(raw_name));
    let tripcode = if allow_tripcodes { tripcode } else { None };
    (name, tripcode)
}

pub fn build_post_body(
    raw_body: &str,
    has_file: bool,
    board_allows_media: bool,
    collapse_greentext: bool,
    filters: &[(String, String)],
) -> Result<(String, String)> {
    let body_text = if board_allows_media {
        validate_body_with_file(raw_body, has_file).map_err(AppError::BadRequest)?
    } else {
        validate_body(raw_body)
            .map_err(AppError::BadRequest)?
            .to_string()
    };
    let filtered_body = apply_word_filters(&body_text, filters);
    let escaped_body = escape_html(&filtered_body);
    let body_html = render_post_body(&escaped_body, collapse_greentext);
    Ok((body_text, body_html))
}

pub fn resolve_deletion_token(raw_token: &str) -> String {
    if raw_token.trim().is_empty() {
        new_deletion_token()
    } else {
        raw_token.trim().chars().take(64).collect()
    }
}

pub fn process_uploads(
    image_file_data: Option<(crate::handlers::TempUpload, String)>,
    file_data: Option<(crate::handlers::TempUpload, String)>,
    audio_file_data: Option<(crate::handlers::TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
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

    let (primary, audio, primary_hash) = crate::handlers::process_audio_first_uploads(
        audio_file_data,
        image_file_data,
        file_data,
        board,
        conn,
        config.upload_dir,
        save_root_str,
        config.thumb_size,
        config.max_image_size,
        config.max_video_size,
        config.max_audio_size,
        config.ffmpeg_available,
        config.ffmpeg_webp_available,
    )?;

    let pending_finalize = stage_root.as_ref().and_then(|stage_dir| {
        build_upload_finalize_payload(stage_dir, primary.as_ref(), audio.as_ref(), primary_hash)
            .map(|payload| PendingUploadFinalize {
                op_id: uuid::Uuid::new_v4().to_string(),
                payload,
            })
    });

    Ok(ProcessedUploads {
        primary,
        audio,
        pending_finalize,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_new_post(
    thread_id: i64,
    board_id: i64,
    name: String,
    tripcode: Option<String>,
    subject: Option<String>,
    body: String,
    body_html: String,
    ip_hash: String,
    uploads: &ProcessedUploads,
    deletion_token: String,
    is_op: bool,
) -> NewPost {
    NewPost {
        thread_id,
        board_id,
        name,
        tripcode,
        subject,
        body,
        body_html,
        ip_hash: Some(ip_hash),
        file_path: uploads.primary.as_ref().map(|u| u.file_path.clone()),
        file_name: uploads.primary.as_ref().map(|u| u.original_name.clone()),
        file_size: uploads.primary.as_ref().map(|u| u.file_size),
        thumb_path: uploads
            .primary
            .as_ref()
            .and_then(|u| (!u.thumb_path.is_empty()).then(|| u.thumb_path.clone())),
        mime_type: uploads.primary.as_ref().map(|u| u.mime_type.clone()),
        media_type: uploads
            .primary
            .as_ref()
            .map(|u| u.media_type.as_str().to_string()),
        audio_file_path: uploads.audio.as_ref().map(|u| u.file_path.clone()),
        audio_file_name: uploads.audio.as_ref().map(|u| u.original_name.clone()),
        audio_file_size: uploads.audio.as_ref().map(|u| u.file_size),
        audio_mime_type: uploads.audio.as_ref().map(|u| u.mime_type.clone()),
        deletion_token,
        is_op,
    }
}
