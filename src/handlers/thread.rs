// handlers/thread.rs
//
// Handles:
//   GET  /:board/thread/:id   — view thread with all posts
//   POST /:board/thread/:id   — post a reply

use crate::{
    config::CONFIG,
    db::{self, NewPost},
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::{validate_csrf, AppState},
    utils::{
        crypto::{hash_ip, new_deletion_token},
        files::save_upload,
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
        move || -> Result<String> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound(format!("Thread {} not found", thread_id)))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }

            let posts = db::get_posts_for_thread(&conn, thread_id)?;
            let all_boards = db::get_all_boards(&conn)?;

            Ok(crate::templates::thread_page(&board, &thread, &posts, &csrf_clone, &all_boards))
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
    mut multipart: Multipart,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let mut csrf_verified = false;

    let mut name_val = String::new();
    let mut body_val = String::new();
    let mut del_token_val = String::new();
    let mut file_data: Option<(Vec<u8>, String)> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => {
                let v = field.text().await.unwrap_or_default();
                if validate_csrf(csrf_cookie.as_deref(), &v) {
                    csrf_verified = true;
                }
            }
            Some("name")  => name_val      = field.text().await.unwrap_or_default(),
            Some("body")  => body_val      = field.text().await.unwrap_or_default(),
            Some("deletion_token") => del_token_val = field.text().await.unwrap_or_default(),
            Some("file")  => {
                let fname = field.file_name().unwrap_or("upload").to_string();
                let bytes = field.bytes().await
                    .map_err(|e| AppError::BadRequest(format!("File read error: {e}")))?;
                if !bytes.is_empty() {
                    file_data = Some((bytes.to_vec(), fname));
                }
            }
            _ => { let _ = field.bytes().await; }
        }
    }

    if !csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let body_text = validate_body(&body_val)
        .map_err(AppError::BadRequest)?
        .to_string();

    let client_ip   = addr.ip().to_string();
    let upload_dir  = CONFIG.upload_dir.clone();
    let thumb_size  = CONFIG.thumb_size;
    let max_size    = CONFIG.max_file_size;
    let cookie_secret = CONFIG.cookie_secret.clone();

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

            // Check ban
            let ip_hash = hash_ip(&client_ip, &cookie_secret);
            if let Some(reason) = db::is_banned(&conn, &ip_hash)? {
                return Err(AppError::Forbidden(format!(
                    "You are banned. Reason: {}",
                    if reason.is_empty() { "No reason given".to_string() } else { reason }
                )));
            }

            // Apply word filters
            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let (name, tripcode) = parse_name_tripcode(&validate_name(&name_val));
            let escaped_body     = escape_html(&body_text);
            let filtered_body    = apply_word_filters(&escaped_body, &filters);
            let body_html        = render_post_body(&filtered_body);

            // Optional file upload
            let uploaded = if let Some((data, fname)) = file_data {
                Some(save_upload(&data, &fname, &upload_dir, thumb_size, max_size)
                    .map_err(|e| AppError::BadRequest(e.to_string()))?)
            } else {
                None
            };

            let deletion_token = if del_token_val.trim().is_empty() {
                new_deletion_token()
            } else {
                del_token_val.trim().to_string()
            };

            // Only bump if below bump limit
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
                // Increment reply count without updating bumped_at
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
