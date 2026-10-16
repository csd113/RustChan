/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers::posting");

use crate::{
    db,
    error::{AppError, Result},
    models::Board,
    utils::{
        crypto::{hash_ip, new_deletion_token},
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name, validate_subject,
        },
        tripcode::parse_name_tripcode,
    },
};

use crate::db::NewPost;

pub(super) enum SubmitPostMode {
    NewThread {
        subject: String,
        poll_question: String,
        poll_options: Vec<String>,
        poll_duration_secs: Option<i64>,
    },
    Reply {
        thread_id: i64,
        sage: bool,
    },
}

pub(super) struct SubmitPostCommand {
    pub mode: SubmitPostMode,
    pub board_short: String,
    pub identity_key: String,
    pub cookie_secret: String,
    pub admin_session_id: Option<String>,
    pub access_cookie: Option<String>,
    pub ban_csrf_token: String,
    pub submission_token: String,
    pub name: String,
    pub body: String,
    pub deletion_token: String,
    pub captcha_id: String,
    pub captcha_answer: String,
    pub image_file_data: Option<(crate::handlers::TempUpload, String)>,
    pub file_data: Option<(crate::handlers::TempUpload, String)>,
    pub audio_file_data: Option<(crate::handlers::TempUpload, String)>,
    pub upload_dir: String,
    pub thumb_size: u32,
    pub ffmpeg_available: bool,
}

pub(super) struct SubmitPostResult {
    pub redirect_url: String,
    pub board_short: String,
    pub thread_id: i64,
    pub post_id: i64,
    pub deletion_token: String,
    pub created_at: i64,
}

/// Resolve a committed submission token to its canonical public post response.
///
/// # Errors
/// Returns an error if the canonical post cannot be loaded from the database.
fn existing_submission_result(
    conn: &rusqlite::Connection,
    board_short: String,
    existing: db::PostSubmissionRecord,
) -> Result<SubmitPostResult> {
    let stored_post = db::get_post(conn, existing.post_id)?
        .ok_or_else(|| AppError::NotFound("Existing post submission target not found.".into()))?;
    Ok(SubmitPostResult {
        redirect_url: format!(
            "/{board_short}/thread/{}#p{}",
            existing.thread_id, existing.post_id
        ),
        board_short,
        thread_id: existing.thread_id,
        post_id: existing.post_id,
        deletion_token: stored_post.deletion_token,
        created_at: stored_post.created_at,
    })
}

struct UploadConfig<'a> {
    pub upload_dir: &'a str,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub max_pdf_size: usize,
    pub ffmpeg_available: bool,
}

#[derive(Clone)]
struct PendingUploadFinalize {
    pub op_id: String,
    pub payload: crate::pending_fs::UploadFinalizePayload,
}

struct ProcessedUploads {
    pub primary: Option<crate::utils::files::UploadedFile>,
    pub audio: Option<crate::utils::files::UploadedFile>,
    pub pending_finalize: Option<PendingUploadFinalize>,
}

impl ProcessedUploads {
    pub(crate) fn rollback_new_files(&self, pool: &db::DbPool, upload_dir: &str) -> Result<()> {
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
        tracing::warn!(target: LOG_TARGET,
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
                    tracing::warn!(target: LOG_TARGET,
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

fn build_pending_upload_op(
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

fn finalize_pending_uploads(pool: &db::DbPool, upload_dir: &str, uploads: &ProcessedUploads) {
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
            tracing::error!(target: LOG_TARGET,
                op_id = %pending.op_id,
                error = %error,
                "upload finalization failed; leaving pending_fs_op for startup reconciliation"
            );
        }
    }
}

/// Clean uncommitted staged files if the final pool checkout fails.
fn checkout_post_connection(
    pool: &db::DbPool,
    uploads: &ProcessedUploads,
    upload_dir: &str,
) -> Result<r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>> {
    match pool.get() {
        Ok(conn) => Ok(conn),
        Err(error) => {
            uploads.rollback_new_files(pool, upload_dir)?;
            Err(error.into())
        }
    }
}

/// Preserve a transaction's typed public rejection.
pub(super) fn post_creation_error(error: anyhow::Error) -> AppError {
    match error.downcast::<AppError>() {
        Ok(error) => error,
        Err(error) => error.into(),
    }
}

/// Check mutable request policy while the post transaction owns the write lock.
fn revalidate_posting_policy(
    conn: &rusqlite::Connection,
    prepared: &Board,
    prepared_filters: &[(String, String)],
    admin_session_id: Option<&str>,
    access_cookie: Option<&str>,
    ip_hash: &str,
    ban_csrf_token: &str,
) -> Result<()> {
    let context = crate::handlers::board::load_board_access_context(
        conn,
        &prepared.short_name,
        admin_session_id,
        access_cookie,
    )?;
    let current = &context.board;
    if current.id != prepared.id || !context.can_post {
        return Err(AppError::Forbidden(
            "Board posting access changed. Reload the board.".into(),
        ));
    }
    crate::handlers::board::ensure_actor_not_banned(conn, ip_hash, ban_csrf_token.to_owned())?;
    // These settings determined media acceptance and body preparation. Refuse
    // stale preparation rather than publishing under a different policy.
    let media_policy = |board: &Board| {
        (
            (
                board.allow_images,
                board.allow_video,
                board.allow_audio,
                board.allow_pdf,
                board.allow_any_files,
            ),
            (
                board.max_image_size,
                board.max_video_size,
                board.max_audio_size,
                board.max_pdf_size,
            ),
            (
                board.allow_tripcodes,
                board.allow_captcha,
                board.collapse_greentext,
            ),
        )
    };
    if media_policy(current) != media_policy(prepared)
        || load_word_filters(conn)? != prepared_filters
    {
        return Err(AppError::Conflict(
            "Board posting settings changed. Reload the board and try again.".into(),
        ));
    }
    if !context.is_admin && current.post_cooldown_secs > 0 {
        if let Some(seconds) = db::get_seconds_since_last_post(conn, current.id, ip_hash)? {
            let remaining = current.post_cooldown_secs.saturating_sub(seconds);
            if remaining > 0 {
                return Err(AppError::BadRequest(format!(
                    "Please wait {remaining} more second{} before posting again.",
                    if remaining == 1 { "" } else { "s" }
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn is_admin_session(
    conn: &rusqlite::Connection,
    admin_session_id: Option<&str>,
) -> bool {
    admin_session_id.is_some_and(|sid| db::get_session(conn, sid).ok().flatten().is_some())
}

fn load_word_filters(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>> {
    Ok(db::get_word_filters(conn)?
        .into_iter()
        .map(|f| (f.pattern, f.replacement))
        .collect())
}

fn resolve_post_identity(raw_name: &str, allow_tripcodes: bool) -> (String, Option<String>) {
    let (name, tripcode) = parse_name_tripcode(&validate_name(raw_name));
    let tripcode = if allow_tripcodes { tripcode } else { None };
    (name, tripcode)
}

fn build_post_body(
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
            .to_owned()
    };
    let filtered_body = apply_word_filters(&body_text, filters);
    let escaped_body = escape_html(&filtered_body);
    let body_html = render_post_body(&escaped_body, collapse_greentext);
    Ok((body_text, body_html))
}

fn resolve_deletion_token(raw_token: &str) -> String {
    if raw_token.trim().is_empty() {
        new_deletion_token()
    } else {
        raw_token.trim().chars().take(64).collect()
    }
}

fn process_uploads(
    image_file_data: Option<(crate::handlers::TempUpload, String)>,
    file_data: Option<(crate::handlers::TempUpload, String)>,
    audio_file_data: Option<(crate::handlers::TempUpload, String)>,
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

    let processed = crate::handlers::process_audio_first_uploads(
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

#[expect(
    clippy::too_many_arguments,
    reason = "the constructor mirrors the persisted post record assembled from validated form fields"
)]
fn build_new_post(
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
    let primary = uploads.primary.as_ref();
    let audio = uploads.audio.as_ref();

    NewPost {
        thread_id,
        board_id,
        name,
        tripcode,
        subject,
        body,
        body_html,
        ip_hash: Some(ip_hash),
        file_path: primary.map(|u| u.file_path.clone()),
        file_name: primary.map(|u| u.original_name.clone()),
        file_size: primary.map(|u| u.file_size),
        thumb_path: primary.and_then(|u| (!u.thumb_path.is_empty()).then(|| u.thumb_path.clone())),
        mime_type: primary.map(|u| u.mime_type.clone()),
        media_type: primary.map(|u| u.media_type.as_str().to_owned()),
        audio_file_path: audio.map(|u| u.file_path.clone()),
        audio_file_name: audio.map(|u| u.original_name.clone()),
        audio_file_size: audio.map(|u| u.file_size),
        audio_mime_type: audio.map(|u| u.mime_type.clone()),
        deletion_token,
        is_op,
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "post validation and transactional creation share one consistency boundary"
)]
#[expect(
    clippy::too_many_lines,
    reason = "validation, transactional insertion, attachment updates, and job enqueueing share one boundary"
)]
/// Submit with independently scoped preparation, persistence and promotion.
pub(super) fn submit_post(
    pool: &db::DbPool,
    job_queue: &crate::workers::JobQueue,
    command: SubmitPostCommand,
) -> Result<SubmitPostResult> {
    let SubmitPostCommand {
        mode,
        board_short,
        identity_key,
        cookie_secret,
        admin_session_id,
        access_cookie,
        ban_csrf_token,
        submission_token,
        name,
        body,
        deletion_token,
        captcha_id,
        captcha_answer,
        image_file_data,
        file_data,
        audio_file_data,
        upload_dir,
        thumb_size,
        ffmpeg_available,
    } = command;

    if submission_token.len() > 128
        || !submission_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::BadRequest(
            "Invalid submission token. Reload the posting form.".into(),
        ));
    }
    let conn = pool.get()?;
    let board = db::get_board_by_short(&conn, &board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
    let effective_max_image_size = board.max_image_size_bytes();
    let effective_max_video_size = board.max_video_size_bytes();
    let effective_max_audio_size = board.max_audio_size_bytes();
    let effective_max_pdf_size = board.max_pdf_size_bytes();

    let ip_hash = hash_ip(&identity_key, &cookie_secret);
    crate::handlers::board::ensure_actor_not_banned(&conn, &ip_hash, ban_csrf_token.clone())?;
    if let Some(existing) = db::get_post_submission(&conn, &submission_token, &ip_hash, board.id)? {
        let target = match &mode {
            SubmitPostMode::Reply { thread_id, sage: _ } => Some(*thread_id),
            SubmitPostMode::NewThread {
                subject: _,
                poll_question: _,
                poll_options: _,
                poll_duration_secs: _,
            } => None,
        };
        db::threads::validate_submission_target(existing, target).map_err(post_creation_error)?;
        return existing_submission_result(&conn, board.short_name, existing);
    }

    let reply_context = match &mode {
        SubmitPostMode::Reply { thread_id, sage } => {
            let thread = db::get_thread(&conn, *thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }
            if thread.archived {
                return Err(AppError::Forbidden("This thread is archived.".into()));
            }
            if thread.locked {
                return Err(AppError::Forbidden("This thread is locked.".into()));
            }

            Some((*thread_id, *sage))
        }
        SubmitPostMode::NewThread {
            subject: _,
            poll_question: _,
            poll_options: _,
            poll_duration_secs: _,
        } => None,
    };

    let is_admin = is_admin_session(&conn, admin_session_id.as_deref());
    if board.post_cooldown_secs > 0 && !is_admin {
        let elapsed = db::get_seconds_since_last_post(&conn, board.id, &ip_hash)?;
        if let Some(secs) = elapsed {
            let remaining = board.post_cooldown_secs.saturating_sub(secs);
            if remaining > 0 {
                return Err(AppError::BadRequest(format!(
                    "Please wait {remaining} more second{} before posting again.",
                    if remaining == 1 { "" } else { "s" }
                )));
            }
        }
    }

    if board.allow_captcha {
        crate::captcha::verify_captcha(&board_short, &captcha_id, &captcha_answer)
            .map_err(|error| AppError::BadRequest(error.user_message().to_owned()))?;
    }

    let filters = load_word_filters(&conn)?;
    drop(conn);
    let (name, tripcode) = resolve_post_identity(&name, board.allow_tripcodes);
    let board_allows_media = board.allow_images
        || board.allow_video
        || board.allow_audio
        || board.allow_pdf
        || (crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files);
    let has_file = file_data.is_some() || audio_file_data.is_some() || image_file_data.is_some();
    let (body_text, body_html) = build_post_body(
        &body,
        has_file,
        board_allows_media,
        board.collapse_greentext,
        &filters,
    )?;

    let uploads = process_uploads(
        image_file_data,
        file_data,
        audio_file_data,
        &board,
        pool,
        &UploadConfig {
            upload_dir: &upload_dir,
            thumb_size,
            max_image_size: effective_max_image_size,
            max_video_size: effective_max_video_size,
            max_audio_size: effective_max_audio_size,
            max_pdf_size: effective_max_pdf_size,
            ffmpeg_available,
        },
    )?;
    let deletion_token = resolve_deletion_token(&deletion_token);
    let pending_upload_op = match build_pending_upload_op(&uploads) {
        Ok(op) => op,
        Err(error) => {
            uploads.rollback_new_files(pool, &upload_dir)?;
            return Err(error);
        }
    };
    let deduplicated_paths: Vec<&str> = uploads
        .primary
        .iter()
        .filter(|upload| upload.dedup_reused)
        .map(|upload| upload.file_path.as_str())
        .collect();

    let validate = |validation_conn: &rusqlite::Connection| {
        revalidate_posting_policy(
            validation_conn,
            &board,
            &filters,
            admin_session_id.as_deref(),
            access_cookie.as_deref(),
            &ip_hash,
            &ban_csrf_token,
        )
        .map_err(anyhow::Error::new)
    };
    let pending_media_board = uploads
        .primary
        .as_ref()
        .filter(|upload| upload.processing_pending)
        .map(|_upload| board.short_name.as_str());
    let (post_id, thread_id, redirect_url, prune_board_id, created_at) = match mode {
        SubmitPostMode::NewThread {
            subject,
            poll_question,
            poll_options,
            poll_duration_secs,
        } => {
            let subject = validate_subject(&subject);
            let new_post = build_new_post(
                0,
                board.id,
                name,
                tripcode,
                subject.clone(),
                body_text.clone(),
                body_html,
                ip_hash.clone(),
                &uploads,
                deletion_token.clone(),
                true,
            );
            let q = poll_question.trim().to_owned();
            let valid_opts: Vec<String> = poll_options
                .iter()
                .map(|option| option.trim().to_owned())
                .filter(|option| !option.is_empty())
                .collect();
            let poll_insert = if q.is_empty() && valid_opts.is_empty() {
                None
            } else {
                if q.is_empty() {
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return Err(AppError::BadRequest(
                        "Polls need a question and at least two options.".into(),
                    ));
                }
                if valid_opts.len() < 2 {
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return Err(AppError::BadRequest(
                        "Polls need a question and at least two options.".into(),
                    ));
                }
                let Some(secs) = poll_duration_secs else {
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return Err(AppError::BadRequest(
                        "A duration is required when creating a poll.".into(),
                    ));
                };
                let secs = secs.clamp(60, 30 * 24 * 3600);
                let expires_at = chrono::Utc::now().timestamp().saturating_add(secs);
                Some(db::threads::PollInsert {
                    question: &q,
                    options: &valid_opts,
                    expires_at,
                })
            };
            let recovered_conn = checkout_post_connection(pool, &uploads, &upload_dir)?;
            let create_result = db::threads::create_thread_submission(
                &recovered_conn,
                board.id,
                subject.as_deref(),
                &new_post,
                &submission_token,
                poll_insert.as_ref(),
                db::threads::PostFilesystemCommit::new_with_media_validation(
                    pending_upload_op.as_ref(),
                    &deduplicated_paths,
                    true,
                    pending_media_board,
                    validate,
                ),
            );
            let ((thread_id, post_id, _), created_at) = match create_result {
                Ok(db::threads::PostCreationOutcome::Created(ids, created_at)) => {
                    drop(recovered_conn);
                    (ids, created_at)
                }
                Ok(db::threads::PostCreationOutcome::Replayed(existing)) => {
                    let result =
                        existing_submission_result(&recovered_conn, board.short_name, existing);
                    drop(recovered_conn);
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return result;
                }
                Err(error) => {
                    drop(recovered_conn);
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return Err(post_creation_error(error));
                }
            };
            (
                post_id,
                thread_id,
                format!("/{}/thread/{thread_id}#p{post_id}", board.short_name),
                Some(board.id),
                created_at,
            )
        }
        SubmitPostMode::Reply {
            thread_id: _,
            sage: _,
        } => {
            let (thread_id, sage) = reply_context
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Missing reply context")))?;
            let should_bump = !sage;
            let new_post = build_new_post(
                thread_id,
                board.id,
                name,
                tripcode,
                None,
                body_text.clone(),
                body_html,
                ip_hash.clone(),
                &uploads,
                deletion_token.clone(),
                false,
            );
            let recovered_conn = checkout_post_connection(pool, &uploads, &upload_dir)?;
            let (post_id, created_at) = match db::threads::create_reply_submission(
                &recovered_conn,
                &new_post,
                &submission_token,
                should_bump,
                db::threads::PostFilesystemCommit::new_with_media_validation(
                    pending_upload_op.as_ref(),
                    &deduplicated_paths,
                    false,
                    pending_media_board,
                    validate,
                ),
            ) {
                Ok(db::threads::PostCreationOutcome::Created(post_id, created_at)) => {
                    drop(recovered_conn);
                    (post_id, created_at)
                }
                Ok(db::threads::PostCreationOutcome::Replayed(existing)) => {
                    let result =
                        existing_submission_result(&recovered_conn, board.short_name, existing);
                    drop(recovered_conn);
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    return result;
                }
                Err(error) => {
                    drop(recovered_conn);
                    uploads.rollback_new_files(pool, &upload_dir)?;
                    if let Some(closed) = error.downcast_ref::<db::threads::ThreadClosed>() {
                        return Err(AppError::Forbidden(closed.message().to_owned()));
                    }
                    return Err(post_creation_error(error));
                }
            };
            (
                post_id,
                thread_id,
                format!("/{}/thread/{thread_id}#p{post_id}", board.short_name),
                None,
                created_at,
            )
        }
    };

    finalize_pending_uploads(pool, &upload_dir, &uploads);
    if let Err(error) = crate::handlers::enqueue_post_jobs(
        job_queue,
        pool,
        post_id,
        &ip_hash,
        body_text.len(),
        None,
        &board.short_name,
    ) {
        tracing::warn!(target: "workers", post_id, %error, "post committed; auxiliary scheduling failed");
    }
    if prune_board_id.is_some() || pending_media_board.is_some() {
        if let Err(error) = pool
            .get()
            .map_err(anyhow::Error::new)
            .and_then(|notification_conn| {
                job_queue.notify_persisted_thread_prune(&notification_conn)
            })
        {
            tracing::warn!(target: "workers", post_id, %error, "durable posting work will be discovered by worker polling");
        }
    }
    if let Some(prune_board_id) = prune_board_id {
        tracing::info!(
            target: "board",
            board = %board.short_name,
            thread_id = thread_id,
            board_id = prune_board_id,
            "Created new thread"
        );
    } else {
        tracing::info!(target: "board", post_id = post_id, thread_id = thread_id, board = %board.short_name, "Reply posted");
    }
    if let Err(error) = crate::media::prune::run_configured_prune_with_pool(pool, &upload_dir) {
        tracing::warn!(
            target: "media_prune",
            post_id,
            error = %error,
            "active media pruning failed after post upload"
        );
    }

    Ok(SubmitPostResult {
        redirect_url,
        board_short: board.short_name,
        thread_id,
        post_id,
        deletion_token,
        created_at,
    })
}
