// handlers/thread.rs
//
// Handles:
//   GET  /:board/thread/:id   — view thread with all posts
//   POST /:board/thread/:id   — post a reply
//   POST /vote                — cast a poll vote

use crate::{
    config::CONFIG,
    db::{self, NewPost},
    error::{AppError, Result},
    handlers::{board::ensure_csrf, parse_post_multipart},
    middleware::{validate_csrf, AppState},
    utils::{
        crypto::{hash_ip, new_deletion_token, sha256_hex},
        files::save_upload,
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name,
        },
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{ConnectInfo, Form, Multipart, Path, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use std::net::SocketAddr;
use tracing::info;

// ─── GET /:board/thread/:id ───────────────────────────────────────────────────

pub async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);
    let client_ip = addr.ip().to_string();

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<String> {
            let conn = pool.get()?;

            let is_admin = jar_session
                .as_deref()
                .map(|sid| db::get_session(&conn, sid).ok().flatten().is_some())
                .unwrap_or(false);

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound(format!("Thread {} not found", thread_id)))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }

            let posts = db::get_posts_for_thread(&conn, thread_id)?;
            let all_boards = db::get_all_boards(&conn)?;

            // Compute ip_hash for poll vote status
            let ip_hash =
                crate::utils::crypto::hash_ip(&client_ip, &crate::config::CONFIG.cookie_secret);
            let poll = db::get_poll_for_thread(&conn, thread_id, &ip_hash)?;

            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(crate::templates::thread_page(
                &board,
                &thread,
                &posts,
                &csrf_clone,
                &all_boards,
                is_admin,
                poll.as_ref(),
                None,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /:board/thread/:id — post reply ────────────────────────────────────

pub async fn post_reply(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    multipart: Multipart,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let form = parse_post_multipart(multipart, csrf_cookie.as_deref()).await?;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;

    let client_ip = addr.ip().to_string();
    let upload_dir = CONFIG.upload_dir.clone();
    let thumb_size = CONFIG.thumb_size;
    let max_image_size = CONFIG.max_image_size;
    let max_video_size = CONFIG.max_video_size;
    let max_audio_size = CONFIG.max_audio_size;
    let ffmpeg_available = state.ffmpeg_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data = form.file;
    let audio_file_data = form.audio_file;
    let name_val = form.name;
    let del_token_val = form.deletion_token;
    let form_sage = form.sage;

    let board_short_err = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = state.job_queue.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }
            if thread.locked {
                return Err(AppError::Forbidden("This thread is locked.".into()));
            }

            let ip_hash = hash_ip(&client_ip, &cookie_secret);
            if let Some(reason) = db::is_banned(&conn, &ip_hash)? {
                return Err(AppError::Forbidden(format!(
                    "You are banned. Reason: {}",
                    if reason.is_empty() {
                        "No reason given".to_string()
                    } else {
                        reason
                    }
                )));
            }

            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let (name, tripcode) = parse_name_tripcode(&validate_name(&name_val));
            // Respect per-board tripcode setting
            let tripcode = if board.allow_tripcodes {
                tripcode
            } else {
                None
            };

            // Validate body: a file may substitute for text on media-enabled boards,
            // but at least one of body or file must be present.
            let board_allows_media = board.allow_images || board.allow_video || board.allow_audio;
            let has_file = file_data.is_some();
            let body_text = if board_allows_media {
                validate_body_with_file(&raw_body, has_file).map_err(AppError::BadRequest)?
            } else {
                validate_body(&raw_body)
                    .map_err(AppError::BadRequest)?
                    .to_string()
            };

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping.
            let filtered_body = apply_word_filters(&body_text, &filters);
            let escaped_body = escape_html(&filtered_body);
            let body_html = render_post_body(&escaped_body);

            let uploaded = if let Some((data, fname)) = file_data {
                // Enforce per-board media type toggles using magic-byte detection.
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

                // SHA-256 deduplication — FIX[LOW-8]: use sha256_hex from crypto module
                let hash = sha256_hex(&data);
                if let Some(cached) = db::find_file_by_hash(&conn, &hash)? {
                    let cached_media = crate::models::MediaType::from_mime(&cached.mime_type)
                        .unwrap_or(crate::models::MediaType::Image);
                    Some(crate::utils::files::UploadedFile {
                        file_path: cached.file_path,
                        thumb_path: cached.thumb_path,
                        original_name: crate::utils::sanitize::sanitize_filename(&fname),
                        mime_type: cached.mime_type,
                        file_size: data.len() as i64,
                        media_type: cached_media,
                        processing_pending: false,
                    })
                } else {
                    let f = save_upload(
                        &data,
                        &fname,
                        &upload_dir,
                        &board.short_name,
                        thumb_size,
                        max_image_size,
                        max_video_size,
                        max_audio_size,
                        ffmpeg_available,
                    )
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                    db::record_file_hash(&conn, &hash, &f.file_path, &f.thumb_path, &f.mime_type)?;
                    Some(f)
                }
            } else {
                None
            };

            // ── Image+audio combo ─────────────────────────────────────────────
            let audio_uploaded: Option<crate::utils::files::UploadedFile> =
                if let Some((aud_data, aud_fname)) = audio_file_data {
                    if !board.allow_audio {
                        return Err(AppError::BadRequest(
                            "Audio uploads are disabled on this board.".into(),
                        ));
                    }
                    let primary_is_image = uploaded
                        .as_ref()
                        .map(|u| matches!(u.media_type, crate::models::MediaType::Image))
                        .unwrap_or(false);
                    if !primary_is_image {
                        return Err(AppError::BadRequest(
                            "Audio can only be combined with an image upload.".into(),
                        ));
                    }
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
                        &upload_dir,
                        &board.short_name,
                        max_audio_size,
                    )
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                    if let Some(ref img) = uploaded {
                        aud_file.thumb_path = img.thumb_path.clone();
                    }
                    Some(aud_file)
                } else {
                    None
                };

            let deletion_token = if del_token_val.trim().is_empty() {
                new_deletion_token()
            } else {
                del_token_val.trim().chars().take(64).collect()
            };

            // Sage suppresses the bump regardless of reply count.
            let should_bump = !form_sage && thread.reply_count < board.bump_limit;

            let new_post = NewPost {
                thread_id,
                board_id: board.id,
                name,
                tripcode,
                subject: None,
                body: body_text.clone(),
                body_html,
                ip_hash: ip_hash.clone(),
                file_path: uploaded.as_ref().map(|u| u.file_path.clone()),
                file_name: uploaded.as_ref().map(|u| u.original_name.clone()),
                file_size: uploaded.as_ref().map(|u| u.file_size),
                thumb_path: uploaded.as_ref().map(|u| u.thumb_path.clone()),
                mime_type: uploaded.as_ref().map(|u| u.mime_type.clone()),
                media_type: uploaded.as_ref().map(|u| u.media_type.as_str().to_string()),
                audio_file_path: audio_uploaded.as_ref().map(|u| u.file_path.clone()),
                audio_file_name: audio_uploaded.as_ref().map(|u| u.original_name.clone()),
                audio_file_size: audio_uploaded.as_ref().map(|u| u.file_size),
                audio_mime_type: audio_uploaded.as_ref().map(|u| u.mime_type.clone()),
                deletion_token,
                is_op: false,
            };
            let post_id = db::create_post(&conn, &new_post)?;

            if should_bump {
                db::bump_thread(&conn, thread_id)?;
            } else {
                conn.execute(
                    "UPDATE threads SET reply_count = reply_count + 1 WHERE id = ?1",
                    rusqlite::params![thread_id],
                )?;
            }

            // ── Background jobs ───────────────────────────────────────────────
            if let Some(ref up) = uploaded {
                if up.processing_pending {
                    let job = match up.media_type {
                        crate::models::MediaType::Video => {
                            Some(crate::workers::Job::VideoTranscode {
                                post_id,
                                file_path: up.file_path.clone(),
                                board_short: board.short_name.clone(),
                            })
                        }
                        crate::models::MediaType::Audio => {
                            Some(crate::workers::Job::AudioWaveform {
                                post_id,
                                file_path: up.file_path.clone(),
                                board_short: board.short_name.clone(),
                            })
                        }
                        _ => None,
                    };
                    if let Some(j) = job {
                        if let Err(e) = job_queue.enqueue(&j) {
                            tracing::warn!("Failed to enqueue media job for reply: {}", e);
                        }
                    }
                }
            }
            let _ = job_queue.enqueue(&crate::workers::Job::SpamCheck {
                post_id,
                ip_hash: ip_hash.clone(),
                body_len: body_text.len(),
            });

            info!(
                "Reply {} posted in thread {} on /{}/",
                post_id, thread_id, board.short_name
            );
            Ok(format!(
                "/{}/thread/{}#p{}",
                board.short_name, thread_id, post_id
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → re-render the thread page with an inline error banner.
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let client_ip_err = addr.ip().to_string();
            let html = tokio::task::spawn_blocking({
                let pool = state.db.clone();
                let csrf_err = csrf_cookie.clone().unwrap_or_default();
                let board_short = board_short_err.clone();
                let msg = msg.clone();
                move || -> String {
                    let conn = match pool.get() {
                        Ok(c) => c,
                        Err(_) => return String::new(),
                    };
                    let board = match db::get_board_by_short(&conn, &board_short) {
                        Ok(Some(b)) => b,
                        _ => return String::new(),
                    };
                    let thread = match db::get_thread(&conn, thread_id) {
                        Ok(Some(t)) => t,
                        _ => return String::new(),
                    };
                    let posts = db::get_posts_for_thread(&conn, thread_id).unwrap_or_default();
                    let all_boards = db::get_all_boards(&conn).unwrap_or_default();
                    let ip_hash = crate::utils::crypto::hash_ip(
                        &client_ip_err,
                        &crate::config::CONFIG.cookie_secret,
                    );
                    let poll = db::get_poll_for_thread(&conn, thread_id, &ip_hash)
                        .ok()
                        .flatten();
                    crate::templates::thread_page(
                        &board,
                        &thread,
                        &posts,
                        &csrf_err,
                        &all_boards,
                        false,
                        poll.as_ref(),
                        Some(&msg),
                        db::get_collapse_greentext(&conn),
                    )
                }
            })
            .await
            .unwrap_or_default();

            if !html.is_empty() {
                return Ok((jar, Html(html)).into_response());
            }
            return Err(AppError::BadRequest(msg));
        }
        Err(e) => return Err(e),
    };

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── GET /:board/post/:id/edit — show edit form ───────────────────────────────

#[derive(Deserialize)]
pub struct EditQuery {
    pub token: Option<String>,
}

pub async fn edit_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    Query(query): Query<EditQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let prefill_token = query.token.clone().unwrap_or_default();
        move || -> Result<String> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            if post.board_id != board.id {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }

            if !board.allow_editing {
                return Err(AppError::NotFound(
                    "Post editing is not enabled on this board.".into(),
                ));
            }

            let window = if board.edit_window_secs <= 0 {
                300
            } else {
                board.edit_window_secs
            };
            let now = chrono::Utc::now().timestamp();
            if now - post.created_at > window {
                return Err(AppError::Forbidden(
                    "The edit window for this post has closed.".into(),
                ));
            }

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);

            Ok(crate::templates::edit_post_page(
                &board,
                &post,
                &csrf,
                &all_boards,
                &prefill_token,
                None,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /:board/post/:id/edit — submit edit ─────────────────────────────────

#[derive(Deserialize)]
pub struct EditForm {
    pub _csrf: Option<String>,
    pub deletion_token: String,
    pub body: String,
}

/// Internal result type for the edit submission handler.
enum EditOutcome {
    /// Redirect the user to this URL.
    Redirect(String),
    /// Re-render the edit page with this error HTML.
    ErrorPage(String),
}

pub async fn edit_post_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    Form(form): Form<EditForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form._csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;
    let token = form.deletion_token;

    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf_cookie.clone().unwrap_or_default();
        move || -> Result<EditOutcome> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            if post.board_id != board.id {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }

            if !board.allow_editing {
                return Err(AppError::NotFound(
                    "Post editing is not enabled on this board.".into(),
                ));
            }

            // Validate body text length / content before attempting the edit
            let body_text = crate::utils::sanitize::validate_body(&raw_body)
                .map_err(AppError::BadRequest)?
                .to_string();

            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let filtered = crate::utils::sanitize::apply_word_filters(&body_text, &filters);
            let escaped = crate::utils::sanitize::escape_html(&filtered);
            let body_html = crate::utils::sanitize::render_post_body(&escaped);

            let success = db::edit_post(
                &conn,
                post_id,
                &token,
                &body_text,
                &body_html,
                board.edit_window_secs,
            )?;

            let window = if board.edit_window_secs <= 0 {
                300
            } else {
                board.edit_window_secs
            };
            if !success {
                let all_boards = db::get_all_boards(&conn)?;
                let collapse_greentext = db::get_collapse_greentext(&conn);
                let now = chrono::Utc::now().timestamp();
                let err_msg = if now - post.created_at > window {
                    "The edit window for this post has closed."
                } else {
                    "Incorrect edit token."
                };
                let html = crate::templates::edit_post_page(
                    &board,
                    &post,
                    &csrf_clone,
                    &all_boards,
                    &token,
                    Some(err_msg),
                    collapse_greentext,
                );
                return Ok(EditOutcome::ErrorPage(html));
            }

            info!("Post {} edited on /{}/", post_id, board.short_name);
            Ok(EditOutcome::Redirect(format!(
                "/{}/thread/{}#p{}",
                board.short_name, post.thread_id, post_id
            )))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    match outcome {
        EditOutcome::Redirect(url) => Ok(Redirect::to(&url).into_response()),
        EditOutcome::ErrorPage(html) => Ok(Html(html).into_response()),
    }
}

// ─── POST /vote — cast poll vote ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct VoteForm {
    pub _csrf: Option<String>,
    pub option_id: i64,
}

pub async fn vote_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    Form(form): Form<VoteForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form._csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let client_ip = addr.ip().to_string();
    let cookie_secret = CONFIG.cookie_secret.clone();
    let option_id = form.option_id;

    let redirect_url = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let ip_hash = hash_ip(&client_ip, &cookie_secret);

            let (poll_id, thread_id, board_short) = db::get_poll_context(&conn, option_id)?
                .ok_or_else(|| AppError::NotFound("Poll option not found.".into()))?;

            // Check poll has not expired
            let now = chrono::Utc::now().timestamp();
            let expires_at: i64 = conn.query_row(
                "SELECT expires_at FROM polls WHERE id = ?1",
                rusqlite::params![poll_id],
                |r| r.get(0),
            )?;
            if expires_at <= now {
                return Err(AppError::BadRequest("This poll has closed.".into()));
            }

            // Verify option belongs to this poll
            let belongs: i64 = conn.query_row(
                "SELECT COUNT(*) FROM poll_options WHERE id = ?1 AND poll_id = ?2",
                rusqlite::params![option_id, poll_id],
                |r| r.get(0),
            )?;
            if belongs == 0 {
                return Err(AppError::BadRequest("Invalid poll option.".into()));
            }

            db::cast_vote(&conn, poll_id, option_id, &ip_hash)?;
            info!(
                "Vote cast on poll {} option {} by {}",
                poll_id,
                option_id,
                &ip_hash[..8]
            );
            Ok(format!("/{}/thread/{}#poll", board_short, thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── GET /:board/thread/:id/updates?since=N ──────────────────────────────────
//
// Delta-compressed polling endpoint for the thread auto-update toggle.
//
// Returns a JSON envelope with:
//   html         — rendered HTML for any new posts since `since`
//   last_id      — highest post ID seen (client bumps its cursor)
//   count        — number of new posts in this response
//   reply_count  — current total reply count for the thread
//   bump_time    — current bumped_at UNIX timestamp
//   locked       — current lock state
//   sticky       — current sticky state
//
// The extra state fields let the client keep the nav-bar thread stats in
// sync without a full page reload.  Posts are rendered without delete/admin
// controls so the response is auth-state-independent (safe to cache).

#[derive(Deserialize)]
pub struct UpdatesQuery {
    since: i64,
}

pub async fn thread_updates(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    Query(params): Query<UpdatesQuery>,
) -> Result<Response> {
    use axum::http::header;

    let since = params.since;

    let (html, last_id, count, reply_count, bump_time, locked, sticky) =
        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> crate::error::Result<(String, i64, usize, i64, i64, bool, bool)> {
                let conn = pool.get()?;

                // Validate board + thread exist (returns 404 for bad URLs).
                let _board = db::get_board_by_short(&conn, &board_short)?
                    .ok_or_else(|| crate::error::AppError::NotFound("Board not found.".into()))?;
                let thread = db::get_thread(&conn, thread_id)?
                    .ok_or_else(|| crate::error::AppError::NotFound("Thread not found.".into()))?;

                // Fetch posts newer than `since`, ordered oldest-first so they
                // render in the correct chronological order when appended.
                let posts = db::get_new_posts_since(&conn, thread_id, since)?;
                let last_id = posts.iter().map(|p| p.id).max().unwrap_or(since);
                let count = posts.len();

                let mut html = String::new();
                for post in &posts {
                    // show_delete=false, is_admin=false — no user controls in
                    // auto-appended HTML; a full reload restores them.
                    html.push_str(&crate::templates::render_post(
                        post,
                        &board_short,
                        "",
                        false,
                        false,
                        true,
                        0, // no edit link in auto-appended HTML; reload restores it
                    ));
                }

                Ok((
                    html,
                    last_id,
                    count,
                    thread.reply_count,
                    thread.bumped_at,
                    thread.locked,
                    thread.sticky,
                ))
            }
        })
        .await
        .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!(e)))??;

    // Build a JSON envelope with new-post HTML plus current thread state.
    // The client consumes the state fields to keep the nav bar in sync.
    let json = format!(
        r#"{{"html":{html_json},"last_id":{last_id},"count":{count},"reply_count":{reply_count},"bump_time":{bump_time},"locked":{locked},"sticky":{sticky}}}"#,
        html_json = serde_json::to_string(&html).unwrap_or_else(|_| "\"\"".to_string()),
        last_id = last_id,
        count = count,
        reply_count = reply_count,
        bump_time = bump_time,
        locked = locked,
        sticky = sticky,
    );

    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}
