// handlers/thread.rs
//
// Handles:
//   GET  /:board/thread/:id   — view thread with all posts
//   POST /:board/thread/:id   — post a reply

use crate::{
    config::CONFIG,
    db::{self, NewPost},
    error::{AppError, Result},
    handlers::{parse_post_multipart, board::ensure_csrf},
    middleware::AppState,
    utils::{
        // FIX[LOW-8]: sha256_hex now comes from utils::crypto (deduplicated)
        crypto::{hash_ip, new_deletion_token, sha256_hex},
        files::save_upload,
        // FIX[MEDIUM-8]: apply_word_filters runs before escape_html
        sanitize::{apply_word_filters, escape_html, render_post_body, validate_body, validate_name},
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{ConnectInfo, Multipart, Path, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use std::net::SocketAddr;
use tracing::info;

// ─── GET /:board/thread/:id ───────────────────────────────────────────────────

pub async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        // FIX[HIGH-2]: Move admin session check inside spawn_blocking to avoid
        // blocking DB calls on the Tokio worker thread.
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<String> {
            let conn = pool.get()?;

            // Resolve admin status inside the blocking task
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

            Ok(crate::templates::thread_page(
                &board, &thread, &posts, &csrf_clone, &all_boards, is_admin,
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

    let body_text = validate_body(&form.body)
        .map_err(AppError::BadRequest)?
        .to_string();

    let client_ip     = addr.ip().to_string();
    let upload_dir    = CONFIG.upload_dir.clone();
    let thumb_size    = CONFIG.thumb_size;
    let max_image_size  = CONFIG.max_image_size;
    let max_video_size  = CONFIG.max_video_size;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data     = form.file;
    let name_val      = form.name;
    let del_token_val = form.deletion_token;

    let redirect_url = tokio::task::spawn_blocking({
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

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping.
            let filtered_body    = apply_word_filters(&body_text, &filters);
            let escaped_body     = escape_html(&filtered_body);
            let body_html        = render_post_body(&escaped_body);

            let uploaded = if let Some((data, fname)) = file_data {
                // Reject video if board has it disabled
                let is_video = data.get(4..8) == Some(b"ftyp")
                    || data.starts_with(b"\x1a\x45\xdf\xa3");
                if is_video && !board.allow_video {
                    return Err(AppError::BadRequest("This board does not allow video uploads.".into()));
                }

                // SHA-256 deduplication — FIX[LOW-8]: use sha256_hex from crypto module
                let hash = sha256_hex(&data);
                if let Some(cached) = db::find_file_by_hash(&conn, &hash)? {
                    Some(crate::utils::files::UploadedFile {
                        file_path:     cached.file_path,
                        thumb_path:    cached.thumb_path,
                        original_name: crate::utils::sanitize::sanitize_filename(&fname),
                        mime_type:     cached.mime_type,
                        file_size:     data.len() as i64,
                    })
                } else {
                    let f = save_upload(&data, &fname, &upload_dir, thumb_size, max_image_size, max_video_size)
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
                del_token_val.trim().to_string()
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
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_url).into_response())
}
