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
    handlers::{parse_post_multipart, board::ensure_csrf},
    middleware::{validate_csrf, AppState},
    utils::{
        crypto::{hash_ip, new_deletion_token, sha256_hex},
        files::save_upload,
        sanitize::{apply_word_filters, escape_html, render_post_body, validate_body, validate_body_with_file, validate_name},
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{ConnectInfo, Form, Multipart, Path, State},
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
            let ip_hash = crate::utils::crypto::hash_ip(&client_ip, &crate::config::CONFIG.cookie_secret);
            let poll = db::get_poll_for_thread(&conn, thread_id, &ip_hash)?;

            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(crate::templates::thread_page(
                &board, &thread, &posts, &csrf_clone, &all_boards, is_admin, poll.as_ref(), None, collapse_greentext,
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

    let client_ip     = addr.ip().to_string();
    let upload_dir      = CONFIG.upload_dir.clone();
    let thumb_size      = CONFIG.thumb_size;
    let max_image_size  = CONFIG.max_image_size;
    let max_video_size  = CONFIG.max_video_size;
    let max_audio_size  = CONFIG.max_audio_size;
    let ffmpeg_available = state.ffmpeg_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data     = form.file;
    let name_val      = form.name;
    let del_token_val = form.deletion_token;

    let board_short_err = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
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
                    if reason.is_empty() { "No reason given".to_string() } else { reason }
                )));
            }

            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let (name, tripcode) = parse_name_tripcode(&validate_name(&name_val));
            // Respect per-board tripcode setting
            let tripcode = if board.allow_tripcodes { tripcode } else { None };

            // Validate body: a file may substitute for text on media-enabled boards,
            // but at least one of body or file must be present.
            let board_allows_media = board.allow_images || board.allow_video || board.allow_audio;
            let has_file = file_data.is_some();
            let body_text = if board_allows_media {
                validate_body_with_file(&raw_body, has_file)
                    .map_err(AppError::BadRequest)?
            } else {
                validate_body(&raw_body)
                    .map_err(AppError::BadRequest)?
                    .to_string()
            };

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping.
            let filtered_body    = apply_word_filters(&body_text, &filters);
            let escaped_body     = escape_html(&filtered_body);
            let body_html        = render_post_body(&escaped_body);

            let uploaded = if let Some((data, fname)) = file_data {
                // Enforce per-board media type toggles using magic-byte detection.
                let detected_mime = crate::utils::files::detect_mime_type(&data)
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                let detected_media = crate::models::MediaType::from_mime(detected_mime)
                    .ok_or_else(|| AppError::BadRequest("Unsupported file type.".into()))?;

                match detected_media {
                    crate::models::MediaType::Image if !board.allow_images =>
                        return Err(AppError::BadRequest("Image uploads are disabled on this board.".into())),
                    crate::models::MediaType::Video if !board.allow_video =>
                        return Err(AppError::BadRequest("Video uploads are disabled on this board.".into())),
                    crate::models::MediaType::Audio if !board.allow_audio =>
                        return Err(AppError::BadRequest("Audio uploads are disabled on this board.".into())),
                    _ => {}
                }

                // SHA-256 deduplication — FIX[LOW-8]: use sha256_hex from crypto module
                let hash = sha256_hex(&data);
                if let Some(cached) = db::find_file_by_hash(&conn, &hash)? {
                    let cached_media = crate::models::MediaType::from_mime(&cached.mime_type)
                        .unwrap_or(crate::models::MediaType::Image);
                    Some(crate::utils::files::UploadedFile {
                        file_path:     cached.file_path,
                        thumb_path:    cached.thumb_path,
                        original_name: crate::utils::sanitize::sanitize_filename(&fname),
                        mime_type:     cached.mime_type,
                        file_size:     data.len() as i64,
                        media_type:    cached_media,
                    })
                } else {
                    let f = save_upload(&data, &fname, &upload_dir, &board.short_name, thumb_size, max_image_size, max_video_size, max_audio_size, ffmpeg_available)
                        .map_err(|e| AppError::BadRequest(e.to_string()))?;
                    db::record_file_hash(&conn, &hash, &f.file_path, &f.thumb_path, &f.mime_type)?;
                    Some(f)
                }
            } else {
                None
            };

            let deletion_token = if del_token_val.trim().is_empty() {
                new_deletion_token()
            } else {
                del_token_val.trim().chars().take(64).collect()
            };

            let should_bump = thread.reply_count < board.bump_limit;

            let new_post = NewPost {
                thread_id,
                board_id: board.id,
                name,
                tripcode,
                subject: None,
                body: body_text.clone(),
                body_html,
                ip_hash,
                file_path:  uploaded.as_ref().map(|u| u.file_path.clone()),
                file_name:  uploaded.as_ref().map(|u| u.original_name.clone()),
                file_size:  uploaded.as_ref().map(|u| u.file_size),
                thumb_path: uploaded.as_ref().map(|u| u.thumb_path.clone()),
                mime_type:  uploaded.as_ref().map(|u| u.mime_type.clone()),
                media_type: uploaded.as_ref().map(|u| u.media_type.as_str().to_string()),
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

            info!("Reply {} posted in thread {} on /{}/", post_id, thread_id, board.short_name);
            Ok(format!("/{}/thread/{}#p{}", board.short_name, thread_id, post_id))
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
                let pool        = state.db.clone();
                let csrf_err    = csrf_cookie.clone().unwrap_or_default();
                let board_short = board_short_err.clone();
                let msg         = msg.clone();
                move || -> String {
                    let conn = match pool.get() { Ok(c) => c, Err(_) => return String::new() };
                    let board = match db::get_board_by_short(&conn, &board_short) {
                        Ok(Some(b)) => b, _ => return String::new(),
                    };
                    let thread = match db::get_thread(&conn, thread_id) {
                        Ok(Some(t)) => t, _ => return String::new(),
                    };
                    let posts      = db::get_posts_for_thread(&conn, thread_id).unwrap_or_default();
                    let all_boards = db::get_all_boards(&conn).unwrap_or_default();
                    let ip_hash    = crate::utils::crypto::hash_ip(&client_ip_err, &crate::config::CONFIG.cookie_secret);
                    let poll       = db::get_poll_for_thread(&conn, thread_id, &ip_hash).ok().flatten();
                    crate::templates::thread_page(&board, &thread, &posts, &csrf_err, &all_boards, false, poll.as_ref(), Some(&msg), db::get_collapse_greentext(&conn))
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

// ─── POST /vote — cast poll vote ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct VoteForm {
    pub _csrf:     Option<String>,
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

    let client_ip     = addr.ip().to_string();
    let cookie_secret = CONFIG.cookie_secret.clone();
    let option_id     = form.option_id;

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
            info!("Vote cast on poll {} option {} by {}", poll_id, option_id, &ip_hash[..8]);
            Ok(format!("/{}/thread/{}#poll", board_short, thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_url).into_response())
}
