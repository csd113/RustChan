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

pub struct ProcessedUploads {
    pub primary: Option<crate::utils::files::UploadedFile>,
    pub audio: Option<crate::utils::files::UploadedFile>,
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
    let body_html = render_post_body(&escaped_body);
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
    file_data: Option<(crate::handlers::TempUpload, String)>,
    audio_file_data: Option<(crate::handlers::TempUpload, String)>,
    board: &Board,
    conn: &rusqlite::Connection,
    config: &UploadConfig<'_>,
) -> Result<ProcessedUploads> {
    let primary = crate::handlers::process_primary_upload(
        file_data,
        board,
        conn,
        config.upload_dir,
        config.thumb_size,
        config.max_image_size,
        config.max_video_size,
        config.max_audio_size,
        config.ffmpeg_available,
        config.ffmpeg_webp_available,
    )?;

    let audio = crate::handlers::process_audio_combo(
        audio_file_data,
        primary.as_ref(),
        board,
        config.upload_dir,
        config.max_audio_size,
    )?;

    Ok(ProcessedUploads { primary, audio })
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
        ip_hash,
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
