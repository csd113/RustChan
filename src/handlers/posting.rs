// src/handlers/posting.rs

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

pub enum SubmitPostMode {
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

pub struct SubmitPostCommand {
    pub mode: SubmitPostMode,
    pub board_short: String,
    pub identity_key: String,
    pub cookie_secret: String,
    pub admin_session_id: Option<String>,
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
    pub ffprobe_available: bool,
    pub ffmpeg_webp_available: bool,
}

pub struct SubmitPostResult {
    pub redirect_url: String,
    pub board_short: String,
    pub thread_id: i64,
    pub post_id: i64,
    pub deletion_token: String,
    pub created_at: i64,
}

pub struct UploadConfig<'a> {
    pub upload_dir: &'a str,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub max_pdf_size: usize,
    pub ffmpeg_available: bool,
    pub ffprobe_available: bool,
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

fn cleanup_unused_upload_stage(stage_root: Option<&std::path::Path>) {
    let Some(stage_dir) = stage_root else {
        return;
    };
    if !stage_dir.exists() {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(stage_dir) {
        tracing::warn!(
            stage_dir = %stage_dir.display(),
            error = %error,
            "failed to clean unused upload stage directory"
        );
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
        primary_thumb_path: primary
            .and_then(|file| (!file.thumb_path.is_empty()).then(|| file.thumb_path.clone())),
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
            .to_owned()
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

    let processed = crate::handlers::process_audio_first_uploads(
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
        config.max_pdf_size,
        config.ffmpeg_available,
        config.ffprobe_available,
        config.ffmpeg_webp_available,
    );

    let (primary, audio, primary_hash) = match processed {
        Ok(processed) => processed,
        Err(error) => {
            cleanup_unused_upload_stage(stage_root.as_deref());
            return Err(error);
        }
    };

    let pending_finalize = stage_root.as_ref().and_then(|stage_dir| {
        build_upload_finalize_payload(stage_dir, primary.as_ref(), audio.as_ref(), primary_hash)
            .map(|payload| PendingUploadFinalize {
                op_id: uuid::Uuid::new_v4().to_string(),
                payload,
            })
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

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[expect(clippy::too_many_arguments)]
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

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(
    clippy::cognitive_complexity,
    reason = "post validation and transactional creation share one consistency boundary"
)]
#[expect(clippy::too_many_lines)]
pub fn submit_post(
    conn: &rusqlite::Connection,
    job_queue: &crate::workers::JobQueue,
    command: SubmitPostCommand,
) -> Result<SubmitPostResult> {
    let SubmitPostCommand {
        mode,
        board_short,
        identity_key,
        cookie_secret,
        admin_session_id,
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
        ffprobe_available,
        ffmpeg_webp_available,
    } = command;

    let board = db::get_board_by_short(conn, &board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
    let effective_max_image_size = board.max_image_size_bytes();
    let effective_max_video_size = board.max_video_size_bytes();
    let effective_max_audio_size = board.max_audio_size_bytes();
    let effective_max_pdf_size = board.max_pdf_size_bytes();

    let reply_context = match &mode {
        SubmitPostMode::Reply { thread_id, sage } => {
            let thread = db::get_thread(conn, *thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }
            if thread.locked {
                return Err(AppError::Forbidden("This thread is locked.".into()));
            }
            if thread.archived {
                return Err(AppError::Forbidden("This thread is archived.".into()));
            }

            Some((*thread_id, *sage, thread.reply_count))
        }
        SubmitPostMode::NewThread { .. } => None,
    };

    let ip_hash = hash_ip(&identity_key, &cookie_secret);
    if let Some(reason) = db::is_banned(conn, &ip_hash)? {
        return Err(AppError::BannedUser {
            reason: if reason.is_empty() {
                "No reason given".to_owned()
            } else {
                reason
            },
            csrf_token: ban_csrf_token,
        });
    }
    if let Some(existing) = db::get_post_submission(conn, &submission_token, &ip_hash, board.id)? {
        let stored_post = db::get_post(conn, existing.post_id)?.ok_or_else(|| {
            AppError::NotFound("Existing post submission target not found.".into())
        })?;
        return Ok(SubmitPostResult {
            redirect_url: format!(
                "/{}/thread/{}#p{}",
                board.short_name, existing.thread_id, existing.post_id
            ),
            board_short: board.short_name,
            thread_id: existing.thread_id,
            post_id: existing.post_id,
            deletion_token: stored_post.deletion_token,
            created_at: stored_post.created_at,
        });
    }

    let is_admin = is_admin_session(conn, admin_session_id.as_deref());
    if board.post_cooldown_secs > 0 && !is_admin {
        let elapsed = db::get_seconds_since_last_post(conn, board.id, &ip_hash)?;
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

    let filters = load_word_filters(conn)?;
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
        conn,
        &UploadConfig {
            upload_dir: &upload_dir,
            thumb_size,
            max_image_size: effective_max_image_size,
            max_video_size: effective_max_video_size,
            max_audio_size: effective_max_audio_size,
            max_pdf_size: effective_max_pdf_size,
            ffmpeg_available,
            ffprobe_available,
            ffmpeg_webp_available,
        },
    )?;
    let deletion_token = resolve_deletion_token(&deletion_token);
    let pending_upload_op = build_pending_upload_op(&uploads)?;

    let (post_id, thread_id, redirect_url, prune_job) = match mode {
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
                    uploads.rollback_new_files(conn, &upload_dir)?;
                    return Err(AppError::BadRequest(
                        "Polls need a question and at least two options.".into(),
                    ));
                }
                if valid_opts.len() < 2 {
                    uploads.rollback_new_files(conn, &upload_dir)?;
                    return Err(AppError::BadRequest(
                        "Polls need a question and at least two options.".into(),
                    ));
                }
                let Some(secs) = poll_duration_secs else {
                    uploads.rollback_new_files(conn, &upload_dir)?;
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
            let create_result = db::create_thread_with_optional_poll(
                conn,
                board.id,
                subject.as_deref(),
                &new_post,
                &submission_token,
                poll_insert.as_ref(),
                pending_upload_op.as_ref(),
            );
            let (thread_id, post_id, _) = match create_result {
                Ok(ids) => ids,
                Err(error) => {
                    uploads.rollback_new_files(conn, &upload_dir)?;
                    return Err(error.into());
                }
            };
            let prune_job = crate::workers::Job::ThreadPrune {
                board_id: board.id,
                board_short: board.short_name.clone(),
                max_threads: board.max_threads,
                max_archived_threads: board.max_archived_threads,
                allow_archive: board.allow_archive,
            };
            (
                post_id,
                thread_id,
                format!("/{}/thread/{thread_id}", board.short_name),
                Some(prune_job),
            )
        }
        SubmitPostMode::Reply { .. } => {
            let (thread_id, sage, reply_count) = reply_context
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Missing reply context")))?;
            let should_bump = !sage && reply_count < board.bump_limit;
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
            let post_id = match db::create_reply_with_thread_update(
                conn,
                &new_post,
                &submission_token,
                should_bump,
                pending_upload_op.as_ref(),
            ) {
                Ok(post_id) => post_id,
                Err(error) => {
                    let stale_state_error = error.to_string();
                    uploads.rollback_new_files(conn, &upload_dir)?;
                    if stale_state_error.contains("This thread is locked.") {
                        return Err(AppError::Forbidden("This thread is locked.".into()));
                    }
                    if stale_state_error.contains("This thread is archived.") {
                        return Err(AppError::Forbidden("This thread is archived.".into()));
                    }
                    return Err(error.into());
                }
            };
            (
                post_id,
                thread_id,
                format!("/{}/thread/{thread_id}#p{post_id}", board.short_name),
                None,
            )
        }
    };

    finalize_pending_uploads(conn, &upload_dir, &uploads);
    crate::handlers::enqueue_post_jobs(
        job_queue,
        conn,
        post_id,
        &ip_hash,
        body_text.len(),
        uploads.primary.as_ref(),
        &board.short_name,
    );
    if let Some(prune_job) = prune_job.as_ref() {
        let _ = job_queue.enqueue(prune_job);
        tracing::info!(
            target: "board",
            board = %board.short_name,
            thread_id = thread_id,
            "Created new thread"
        );
    } else {
        tracing::info!(target: "board", post_id = post_id, thread_id = thread_id, board = %board.short_name, "Reply posted");
    }
    if let Err(error) = crate::media::prune::run_configured_prune(conn, &upload_dir) {
        tracing::warn!(
            target: "media_prune",
            post_id,
            error = %error,
            "active media pruning failed after post upload"
        );
    }

    let stored_post = db::get_post(conn, post_id)?
        .ok_or_else(|| AppError::NotFound("Posted row not found.".into()))?;

    Ok(SubmitPostResult {
        redirect_url,
        board_short: board.short_name,
        thread_id,
        post_id,
        deletion_token,
        created_at: stored_post.created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::{process_uploads, submit_post, SubmitPostCommand, SubmitPostMode, UploadConfig};
    use crate::error::AppError;
    use crate::handlers::TempUpload;
    use sha2::Digest as _;

    const TEST_BOARD: &str = "test";
    const TEST_COOKIE_SECRET: &str = "cookie-secret";
    const TEST_IDENTITY_KEY: &str = "127.0.0.1";

    fn sample_post(
        board_id: i64,
        thread_id: i64,
        body: &str,
        is_op: bool,
        ip_hash: Option<String>,
    ) -> crate::db::NewPost {
        crate::db::NewPost {
            thread_id,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: is_op.then(|| "subject".to_owned()),
            body: body.to_owned(),
            body_html: body.to_owned(),
            ip_hash,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op,
        }
    }

    fn thread_command(
        board_short: &str,
        submission_token: &str,
        body: &str,
        upload_dir: &str,
    ) -> SubmitPostCommand {
        SubmitPostCommand {
            mode: SubmitPostMode::NewThread {
                subject: String::new(),
                poll_question: String::new(),
                poll_options: Vec::new(),
                poll_duration_secs: None,
            },
            board_short: board_short.to_owned(),
            identity_key: TEST_IDENTITY_KEY.to_owned(),
            cookie_secret: TEST_COOKIE_SECRET.to_owned(),
            admin_session_id: None,
            ban_csrf_token: "ban-csrf".to_owned(),
            submission_token: submission_token.to_owned(),
            name: "anon".to_owned(),
            body: body.to_owned(),
            deletion_token: String::new(),
            captcha_id: String::new(),
            captcha_answer: String::new(),
            image_file_data: None,
            file_data: None,
            audio_file_data: None,
            upload_dir: upload_dir.to_owned(),
            thumb_size: 250,
            ffmpeg_available: false,
            ffprobe_available: false,
            ffmpeg_webp_available: false,
        }
    }

    fn thread_command_with_poll(
        board_short: &str,
        submission_token: &str,
        body: &str,
        upload_dir: &str,
        poll_question: &str,
        poll_options: Vec<&str>,
        poll_duration_secs: Option<i64>,
    ) -> SubmitPostCommand {
        SubmitPostCommand {
            mode: SubmitPostMode::NewThread {
                subject: String::new(),
                poll_question: poll_question.to_owned(),
                poll_options: poll_options.into_iter().map(str::to_owned).collect(),
                poll_duration_secs,
            },
            board_short: board_short.to_owned(),
            identity_key: TEST_IDENTITY_KEY.to_owned(),
            cookie_secret: TEST_COOKIE_SECRET.to_owned(),
            admin_session_id: None,
            ban_csrf_token: "ban-csrf".to_owned(),
            submission_token: submission_token.to_owned(),
            name: "anon".to_owned(),
            body: body.to_owned(),
            deletion_token: String::new(),
            captcha_id: String::new(),
            captcha_answer: String::new(),
            image_file_data: None,
            file_data: None,
            audio_file_data: None,
            upload_dir: upload_dir.to_owned(),
            thumb_size: 250,
            ffmpeg_available: false,
            ffprobe_available: false,
            ffmpeg_webp_available: false,
        }
    }

    fn reply_command(
        board_short: &str,
        thread_id: i64,
        submission_token: &str,
        body: &str,
        upload_dir: &str,
    ) -> SubmitPostCommand {
        SubmitPostCommand {
            mode: SubmitPostMode::Reply {
                thread_id,
                sage: false,
            },
            board_short: board_short.to_owned(),
            identity_key: TEST_IDENTITY_KEY.to_owned(),
            cookie_secret: TEST_COOKIE_SECRET.to_owned(),
            admin_session_id: None,
            ban_csrf_token: "ban-csrf".to_owned(),
            submission_token: submission_token.to_owned(),
            name: "anon".to_owned(),
            body: body.to_owned(),
            deletion_token: String::new(),
            captcha_id: String::new(),
            captcha_answer: String::new(),
            image_file_data: None,
            file_data: None,
            audio_file_data: None,
            upload_dir: upload_dir.to_owned(),
            thumb_size: 250,
            ffmpeg_available: false,
            ffprobe_available: false,
            ffmpeg_webp_available: false,
        }
    }

    fn temp_upload(name: &str, bytes: &[u8]) -> (TempUpload, String) {
        let temp_file = tempfile::Builder::new()
            .prefix("rustchan-posting-test-upload-")
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

    fn flac_header_bytes() -> Vec<u8> {
        b"fLaC\x00\x00\x00\x22tiny test flac bytes".to_vec()
    }

    fn malformed_aac_bytes() -> Vec<u8> {
        let mut bytes = Vec::with_capacity(2_048);
        bytes.extend_from_slice(&[0xFF, 0xF1, 0x50, 0x80]);
        bytes.resize(2_048, 0);
        let mut state = 4_u32;
        for byte in bytes.iter_mut().skip(4) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *byte = u8::try_from(state & 0xFF).expect("masked byte fits in u8");
        }
        bytes
    }

    fn webm_header_bytes() -> Vec<u8> {
        b"\x1a\x45\xdf\xa3\x00\x00\x00\x00\x00\x00\x42\x82\x84webm\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()
    }

    fn pending_upload_stage_count(upload_dir: &std::path::Path) -> usize {
        let pending = upload_dir.join(".pending");
        if !pending.exists() {
            return 0;
        }
        std::fs::read_dir(pending)
            .expect("read pending dir")
            .count()
    }

    fn assert_no_partial_post_rows(conn: &rusqlite::Connection) {
        let thread_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
            .expect("thread count");
        let post_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))
            .expect("post count");
        assert_eq!(thread_count, 0);
        assert_eq!(post_count, 0);
    }

    #[test]
    fn submit_post_rejects_banned_user_before_creating_thread() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");

        let ip_hash = crate::utils::crypto::hash_ip(TEST_IDENTITY_KEY, TEST_COOKIE_SECRET);
        crate::db::add_ban(&conn, &ip_hash, "posting blocked", None).expect("add ban");

        let error = match submit_post(
            &conn,
            state.job_queue.as_ref(),
            thread_command(
                TEST_BOARD,
                "banned-thread",
                "thread body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        ) {
            Ok(result) => panic!(
                "expected banned submission to fail, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::BannedUser { reason, csrf_token } => {
                assert_eq!(reason, "posting blocked");
                assert_eq!(csrf_token, "ban-csrf");
            }
            other => panic!("expected BannedUser, got {other:?}"),
        }

        let thread_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
            .expect("thread count");
        assert_eq!(thread_count, 0);
    }

    #[test]
    fn submit_post_enforces_board_cooldown_for_reply() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET post_cooldown_secs = 60 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("enable cooldown");

        let ip_hash = crate::utils::crypto::hash_ip(TEST_IDENTITY_KEY, TEST_COOKIE_SECRET);
        let (thread_id, _, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            board_id,
            Some("cooldown thread"),
            &sample_post(board_id, 0, "op body", true, Some(ip_hash)),
            "",
            None,
            None,
        )
        .expect("create thread");

        let error = match submit_post(
            &conn,
            state.job_queue.as_ref(),
            reply_command(
                TEST_BOARD,
                thread_id,
                "cooldown-reply",
                "reply body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        ) {
            Ok(result) => panic!("expected cooldown rejection, got {}", result.redirect_url),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("Please wait"));
                assert!(message.contains("before posting again."));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn submit_post_rejects_captcha_failure_for_new_thread() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_captcha = 1 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("enable captcha");

        let error = match submit_post(
            &conn,
            state.job_queue.as_ref(),
            thread_command(
                TEST_BOARD,
                "captcha-thread",
                "thread body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        ) {
            Ok(result) => panic!("expected captcha rejection, got {}", result.redirect_url),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert_eq!(
                    message,
                    "CAPTCHA verification failed. Enter the text from the image and try again."
                );
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn submit_post_accepts_valid_captcha_once() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_captcha = 1 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("enable captcha");
        let captcha_id = "00000000000000000000000000000007";
        crate::captcha::testing::insert_challenge_for_test(
            TEST_BOARD,
            captcha_id,
            "ABC23",
            chrono::Utc::now().timestamp() + 300,
        );

        let mut command = thread_command(
            TEST_BOARD,
            "captcha-thread-success",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.captcha_id = captcha_id.to_owned();
        command.captcha_answer = "abc23".to_owned();

        let result = submit_post(&conn, state.job_queue.as_ref(), command).expect("submit post");
        assert_eq!(result.board_short, TEST_BOARD);

        let mut replay = thread_command(
            TEST_BOARD,
            "captcha-thread-replay",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        replay.captcha_id = captcha_id.to_owned();
        replay.captcha_answer = "ABC23".to_owned();
        let error = match submit_post(&conn, state.job_queue.as_ref(), replay) {
            Ok(result) => panic!(
                "expected captcha replay rejection, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };
        assert!(matches!(error, AppError::BadRequest(message) if message.contains("expired")));
    }

    #[test]
    fn submit_post_rejects_expired_captcha() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_captcha = 1 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("enable captcha");
        let captcha_id = "00000000000000000000000000000008";
        crate::captcha::testing::insert_challenge_for_test(
            TEST_BOARD,
            captcha_id,
            "ABC23",
            chrono::Utc::now().timestamp() - 1,
        );
        let mut command = thread_command(
            TEST_BOARD,
            "captcha-thread-expired",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.captcha_id = captcha_id.to_owned();
        command.captcha_answer = "ABC23".to_owned();

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "expected expired captcha rejection, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };
        assert!(matches!(error, AppError::BadRequest(message) if message.contains("expired")));
    }

    #[test]
    fn submit_post_rejects_partial_poll_submission() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");

        let error = match submit_post(
            &conn,
            state.job_queue.as_ref(),
            thread_command_with_poll(
                TEST_BOARD,
                "poll-token",
                "thread body",
                upload_dir.path().to_str().expect("upload dir"),
                "pick one",
                vec!["yes"],
                Some(3600),
            ),
        ) {
            Ok(result) => panic!(
                "expected partial poll rejection, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert_eq!(message, "Polls need a question and at least two options.");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn process_uploads_cleans_staged_image_when_combo_audio_fails() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        let board = crate::db::get_board_by_short(&conn, TEST_BOARD)
            .expect("load board")
            .expect("board exists");
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let image = temp_upload("cover.png", &one_pixel_png());
        let bad_audio = temp_upload("renamed.mp3", b"not really audio");

        let result = process_uploads(
            Some(image),
            None,
            Some(bad_audio),
            &board,
            &conn,
            &UploadConfig {
                upload_dir: upload_dir.path().to_str().expect("upload dir"),
                thumb_size: 64,
                max_image_size: 1024 * 1024,
                max_video_size: 1024 * 1024,
                max_audio_size: 1024 * 1024,
                max_pdf_size: 1024 * 1024,
                ffmpeg_available: false,
                ffprobe_available: false,
                ffmpeg_webp_available: false,
            },
        );

        assert!(result.is_err());
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
    }

    #[test]
    fn submit_post_enforces_board_specific_image_limit() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET max_image_size = ?1 WHERE short_name = ?2",
            rusqlite::params![64_i64, TEST_BOARD],
        )
        .expect("shrink image limit");
        let mut command = thread_command(
            TEST_BOARD,
            "board-image-cap",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("cover.png", &one_pixel_png()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "board-specific image cap should reject upload, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::UploadTooLarge(message) => {
                assert!(message.contains("Maximum image upload size is 64 B."));
            }
            other => panic!("expected UploadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn submit_post_rejects_disabled_audio_without_stale_uploads_or_rows() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        let mut command = thread_command(
            TEST_BOARD,
            "audio-disabled",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("tiny.flac", &flac_header_bytes()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "audio-disabled board should reject, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("Audio uploads are disabled"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        assert_no_partial_post_rows(&conn);
    }

    #[test]
    fn submit_post_rejects_malformed_aac_without_jobs_or_rows() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_audio = 1 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("enable audio");
        let mut command = thread_command(
            TEST_BOARD,
            "malformed-aac",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("broken.aac", &malformed_aac_bytes()));
        command.ffmpeg_available = true;

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!("malformed AAC should reject, got {}", result.redirect_url),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("ADTS stream is malformed"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        assert_no_partial_post_rows(&conn);
        let job_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM background_jobs", [], |row| row.get(0))
            .expect("job count");
        assert_eq!(job_count, 0);
    }

    #[test]
    fn submit_post_rejects_disabled_video_without_stale_uploads_or_rows() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_video = 0 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("disable video");
        let mut command = thread_command(
            TEST_BOARD,
            "video-disabled",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("tiny.webm", &webm_header_bytes()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "video-disabled board should reject, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("Video uploads are disabled"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        assert_no_partial_post_rows(&conn);
    }

    #[test]
    fn submit_post_rejects_overlimit_audio_without_stale_uploads_or_rows() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_audio = 1, max_audio_size = 8 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("shrink audio limit");
        let mut command = thread_command(
            TEST_BOARD,
            "audio-overlimit",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("tiny.flac", &flac_header_bytes()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "over-limit audio should reject, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::UploadTooLarge(message) => {
                assert!(message.contains("Maximum audio upload size is 8 B."));
            }
            other => panic!("expected UploadTooLarge, got {other:?}"),
        }
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        assert_no_partial_post_rows(&conn);
    }

    #[test]
    fn submit_post_rejects_overlimit_video_without_stale_uploads_or_rows() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET max_video_size = 8 WHERE short_name = ?1",
            rusqlite::params![TEST_BOARD],
        )
        .expect("shrink video limit");
        let mut command = thread_command(
            TEST_BOARD,
            "video-overlimit",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("tiny.webm", &webm_header_bytes()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "over-limit video should reject, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::UploadTooLarge(message) => {
                assert!(message.contains("Maximum video upload size is 8 B."));
            }
            other => panic!("expected UploadTooLarge, got {other:?}"),
        }
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        assert_no_partial_post_rows(&conn);
    }

    #[test]
    fn submit_post_cleans_staged_upload_when_poll_validation_fails() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");

        let mut command = thread_command_with_poll(
            TEST_BOARD,
            "poll-upload-token",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
            "pick one",
            vec!["only option"],
            Some(3600),
        );
        command.file_data = Some(temp_upload("cover.png", &one_pixel_png()));

        let error = match submit_post(&conn, state.job_queue.as_ref(), command) {
            Ok(result) => panic!(
                "poll validation should reject submission, got {}",
                result.redirect_url
            ),
            Err(error) => error,
        };

        match error {
            AppError::BadRequest(message) => {
                assert_eq!(message, "Polls need a question and at least two options.");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }

        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
        assert!(!upload_dir.path().join(TEST_BOARD).exists());
        let post_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))
            .expect("post count");
        assert_eq!(post_count, 0);
    }

    #[test]
    fn submit_post_uses_board_limit_for_upload_validation() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET max_image_size = ?1 WHERE short_name = ?2",
            rusqlite::params![1024 * 1024_i64, TEST_BOARD],
        )
        .expect("raise image limit");
        let mut command = thread_command(
            TEST_BOARD,
            "board-image-raised-cap",
            "thread body",
            upload_dir.path().to_str().expect("upload dir"),
        );
        command.file_data = Some(temp_upload("cover.png", &one_pixel_png()));

        let result = submit_post(&conn, state.job_queue.as_ref(), command)
            .expect("board-specific raised image cap should allow upload");

        assert_eq!(result.board_short, TEST_BOARD);
    }

    #[test]
    fn process_uploads_cleans_empty_stage_when_primary_upload_is_dedup_reused() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        let board = crate::db::get_board_by_short(&conn, TEST_BOARD)
            .expect("load board")
            .expect("board exists");
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let board_dir = upload_dir.path().join(TEST_BOARD);
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).expect("create media dirs");
        std::fs::write(board_dir.join("cached.png"), b"cached file").expect("cached file");
        std::fs::write(thumb_dir.join("cached.webp"), b"cached thumb").expect("cached thumb");

        let bytes = one_pixel_png();
        let hash = hex::encode(sha2::Sha256::digest(&bytes));
        crate::db::record_file_hash(
            &conn,
            &hash,
            "test/cached.png",
            "test/thumbs/cached.webp",
            "image/png",
        )
        .expect("record file hash");

        let result = process_uploads(
            None,
            Some(temp_upload("same-but-renamed.jpg", &bytes)),
            None,
            &board,
            &conn,
            &UploadConfig {
                upload_dir: upload_dir.path().to_str().expect("upload dir"),
                thumb_size: 64,
                max_image_size: 1024 * 1024,
                max_video_size: 1024 * 1024,
                max_audio_size: 1024 * 1024,
                max_pdf_size: 1024 * 1024,
                ffmpeg_available: false,
                ffprobe_available: false,
                ffmpeg_webp_available: false,
            },
        )
        .expect("dedup upload");

        assert!(result.pending_finalize.is_none());
        assert!(result
            .primary
            .as_ref()
            .is_some_and(|file| file.dedup_reused));
        assert_eq!(
            result.primary.as_ref().map(|file| file.file_size),
            Some(i64::try_from(b"cached file".len()).expect("cached size fits"))
        );
        assert_eq!(pending_upload_stage_count(upload_dir.path()), 0);
    }

    #[test]
    fn upload_finalize_payload_omits_empty_generic_thumb_path() {
        let primary = crate::utils::files::UploadedFile {
            file_path: "test/file.bin".to_owned(),
            thumb_path: String::new(),
            original_name: "file.bin".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            file_size: 4,
            media_type: crate::models::MediaType::Other,
            processing_pending: false,
            dedup_reused: false,
        };

        let payload = super::build_upload_finalize_payload(
            std::path::Path::new("/tmp/rustchan-stage"),
            Some(&primary),
            None,
            None,
        )
        .expect("generic upload payload");

        assert_eq!(payload.relative_paths, vec!["test/file.bin"]);
        assert_eq!(payload.primary_file_path.as_deref(), Some("test/file.bin"));
        assert!(payload.primary_thumb_path.is_none());
    }

    #[test]
    fn submit_post_returns_existing_op_redirect_for_duplicate_thread_submission() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");

        submit_post(
            &conn,
            state.job_queue.as_ref(),
            thread_command(
                TEST_BOARD,
                "dup-thread",
                "thread body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        )
        .expect("first submission");

        let original_created_at =
            chrono::Utc::now().timestamp() - crate::handlers::board::SELF_DELETE_WINDOW_SECS - 1;
        conn.execute(
            "UPDATE posts SET created_at = ?1",
            rusqlite::params![original_created_at],
        )
        .expect("age original post");

        let duplicate = submit_post(
            &conn,
            state.job_queue.as_ref(),
            thread_command(
                TEST_BOARD,
                "dup-thread",
                "thread body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        )
        .expect("duplicate submission");

        let (thread_id, op_post_id): (i64, i64) = conn
            .query_row(
                "SELECT t.id, p.id
                 FROM threads t
                 JOIN posts p ON p.thread_id = t.id AND p.is_op = 1
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("thread and op");
        let thread_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
            .expect("thread count");
        let post_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))
            .expect("post count");

        assert_eq!(
            duplicate.redirect_url,
            format!("/{TEST_BOARD}/thread/{thread_id}#p{op_post_id}")
        );
        assert_eq!(duplicate.created_at, original_created_at);
        assert!(
            duplicate.created_at + crate::handlers::board::SELF_DELETE_WINDOW_SECS
                <= chrono::Utc::now().timestamp(),
            "duplicate self-action expiry should stay tied to the original post"
        );
        assert_eq!(thread_count, 1);
        assert_eq!(post_count, 1);
    }

    #[test]
    fn submit_post_returns_existing_reply_redirect_for_duplicate_reply_submission() {
        let state = crate::test_support::app_state();
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, TEST_BOARD, "Test", "", false).expect("create board");
        let (thread_id, _, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            board_id,
            Some("reply target"),
            &sample_post(board_id, 0, "op body", true, Some("other-ip".to_owned())),
            "",
            None,
            None,
        )
        .expect("create thread");

        submit_post(
            &conn,
            state.job_queue.as_ref(),
            reply_command(
                TEST_BOARD,
                thread_id,
                "dup-reply",
                "reply body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        )
        .expect("first reply");

        let duplicate = submit_post(
            &conn,
            state.job_queue.as_ref(),
            reply_command(
                TEST_BOARD,
                thread_id,
                "dup-reply",
                "reply body",
                upload_dir.path().to_str().expect("upload dir"),
            ),
        )
        .expect("duplicate reply");

        let reply_post_id: i64 = conn
            .query_row(
                "SELECT id FROM posts
                 WHERE thread_id = ?1 AND is_op = 0
                 ORDER BY id DESC
                 LIMIT 1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply id");
        let reply_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1 AND is_op = 0",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");

        assert_eq!(
            duplicate.redirect_url,
            format!("/{TEST_BOARD}/thread/{thread_id}#p{reply_post_id}")
        );
        assert_eq!(reply_count, 1);
    }
}
