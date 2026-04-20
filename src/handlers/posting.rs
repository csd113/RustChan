// src/handlers/posting.rs

use crate::{
    db,
    error::{AppError, Result},
    models::Board,
    utils::{
        crypto::{hash_ip, new_deletion_token, verify_pow},
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
    pub pow_nonce: String,
    pub image_file_data: Option<(crate::handlers::TempUpload, String)>,
    pub file_data: Option<(crate::handlers::TempUpload, String)>,
    pub audio_file_data: Option<(crate::handlers::TempUpload, String)>,
    pub upload_dir: String,
    pub thumb_size: u32,
    pub max_image_size: usize,
    pub max_video_size: usize,
    pub max_audio_size: usize,
    pub ffmpeg_available: bool,
    pub ffmpeg_webp_available: bool,
}

pub struct SubmitPostResult {
    pub redirect_url: String,
}

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
        media_type: primary.map(|u| u.media_type.as_str().to_string()),
        audio_file_path: audio.map(|u| u.file_path.clone()),
        audio_file_name: audio.map(|u| u.original_name.clone()),
        audio_file_size: audio.map(|u| u.file_size),
        audio_mime_type: audio.map(|u| u.mime_type.clone()),
        deletion_token,
        is_op,
    }
}

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
        pow_nonce,
        image_file_data,
        file_data,
        audio_file_data,
        upload_dir,
        thumb_size,
        max_image_size,
        max_video_size,
        max_audio_size,
        ffmpeg_available,
        ffmpeg_webp_available,
    } = command;

    let board = db::get_board_by_short(conn, &board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

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

            Some((*thread_id, *sage, thread.reply_count))
        }
        SubmitPostMode::NewThread { .. } => None,
    };

    let ip_hash = hash_ip(&identity_key, &cookie_secret);
    if let Some(reason) = db::is_banned(conn, &ip_hash)? {
        return Err(AppError::BannedUser {
            reason: if reason.is_empty() {
                "No reason given".to_string()
            } else {
                reason
            },
            csrf_token: ban_csrf_token,
        });
    }
    if let Some(existing) = db::get_post_submission(conn, &submission_token, &ip_hash, board.id)? {
        return Ok(SubmitPostResult {
            redirect_url: format!(
                "/{}/thread/{}#p{}",
                board.short_name, existing.thread_id, existing.post_id
            ),
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

    if board.allow_captcha && !verify_pow(&board_short, &pow_nonce) {
        return Err(AppError::BadRequest(
            "CAPTCHA verification failed. Please wait for the solver to complete before posting."
                .into(),
        ));
    }

    let filters = load_word_filters(conn)?;
    let (name, tripcode) = resolve_post_identity(&name, board.allow_tripcodes);
    let board_allows_media = board.allow_images
        || board.allow_video
        || board.allow_audio
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
            max_image_size,
            max_video_size,
            max_audio_size,
            ffmpeg_available,
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
                deletion_token,
                true,
            );
            let q = poll_question.trim().to_string();
            let valid_opts: Vec<String> = poll_options
                .iter()
                .map(|option| option.trim().to_string())
                .filter(|option| !option.is_empty())
                .collect();
            let poll_insert = if !q.is_empty() && valid_opts.len() >= 2 {
                let secs = poll_duration_secs.ok_or_else(|| {
                    AppError::BadRequest("A duration is required when creating a poll.".into())
                })?;
                let secs = secs.clamp(60, 30 * 24 * 3600);
                let expires_at = chrono::Utc::now().timestamp().saturating_add(secs);
                Some(db::threads::PollInsert {
                    question: &q,
                    options: &valid_opts,
                    expires_at,
                })
            } else {
                None
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
                deletion_token,
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
                    uploads.rollback_new_files(conn, &upload_dir)?;
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

    Ok(SubmitPostResult { redirect_url })
}
