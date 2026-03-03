// handlers/board.rs
//
// Handles:
//   GET  /                    — board list
//   GET  /:board/             — board index (thread list)
//   POST /:board/             — create new thread
//   GET  /:board/catalog      — catalog view
//   GET  /:board/search       — search results
//   POST /delete              — user post deletion

use crate::{
    config::CONFIG,
    db::{self, NewPost},
    error::{AppError, Result},
    handlers::{parse_post_multipart, admin::is_admin_session},
    middleware::{validate_csrf, AppState},
    models::*,
    templates,
    utils::{
        crypto::{hash_ip, new_csrf_token, new_deletion_token},
        files::save_upload,
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body, validate_name,
            validate_subject,
        },
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{ConnectInfo, Form, Multipart, Path, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::info;

const PREVIEW_REPLIES: i64 = 3;
const THREADS_PER_PAGE: i64 = 10;

// ─── GET / — board list ───────────────────────────────────────────────────────

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let boards = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<Vec<Board>> {
            let conn = pool.get()?;
            Ok(db::get_all_boards(&conn)?)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(templates::index_page(&boards, &csrf))))
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);

    let is_admin = is_admin_session(&jar, &state.db);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, THREADS_PER_PAGE, total);
            let threads = db::get_threads_for_board(
                &conn,
                board.id,
                THREADS_PER_PAGE,
                pagination.offset(),
            )?;

            let mut summaries = Vec::with_capacity(threads.len());
            for thread in threads {
                let total_replies = thread.reply_count;
                let preview = db::get_preview_posts(&conn, thread.id, PREVIEW_REPLIES)?;
                let omitted = (total_replies - preview.len() as i64).max(0);
                summaries.push(ThreadSummary {
                    thread,
                    preview_posts: preview,
                    omitted,
                });
            }

            let all_boards = db::get_all_boards(&conn)?;
            Ok(templates::board_page(
                &board,
                &summaries,
                &pagination,
                &csrf_clone,
                &all_boards,
                is_admin,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /:board/ — create new thread ───────────────────────────────────────

pub async fn create_thread(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
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

    let client_ip   = addr.ip().to_string();
    let upload_dir  = CONFIG.upload_dir.clone();
    let thumb_size  = CONFIG.thumb_size;
    let max_size    = CONFIG.max_file_size;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let max_threads = CONFIG.max_threads_per_board as i64;
    let file_data   = form.file;
    let name_val    = form.name;
    let subject_val = form.subject;
    let del_token_val = form.deletion_token;

    let redirect_url = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

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
            let subject          = validate_subject(&subject_val);
            let escaped_body     = escape_html(&body_text);
            let filtered_body    = apply_word_filters(&escaped_body, &filters);
            let body_html        = render_post_body(&filtered_body);

            let uploaded = if let Some((data, fname)) = file_data {
                Some(
                    save_upload(&data, &fname, &upload_dir, thumb_size, max_size)
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                )
            } else {
                None
            };

            let deletion_token = if del_token_val.trim().is_empty() {
                new_deletion_token()
            } else {
                del_token_val.trim().to_string()
            };

            let thread_id = db::create_thread(&conn, board.id, subject.as_deref())?;

            let new_post = NewPost {
                thread_id,
                board_id: board.id,
                name,
                tripcode,
                subject,
                body: body_text.clone(),
                body_html,
                ip_hash,
                file_path:  uploaded.as_ref().map(|u| u.file_path.clone()),
                file_name:  uploaded.as_ref().map(|u| u.original_name.clone()),
                file_size:  uploaded.as_ref().map(|u| u.file_size),
                thumb_path: uploaded.as_ref().map(|u| u.thumb_path.clone()),
                mime_type:  uploaded.as_ref().map(|u| u.mime_type.clone()),
                deletion_token,
                is_op: true,
            };
            db::create_post(&conn, &new_post)?;

            let paths = db::prune_old_threads(&conn, board.id, max_threads)?;
            for path in paths {
                crate::utils::files::delete_file(&upload_dir, &path);
            }

            info!("New thread {} created in /{}/", thread_id, board.short_name);
            Ok(format!("/{}/thread/{}", board.short_name, thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── GET /:board/catalog ──────────────────────────────────────────────────────

pub async fn catalog(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
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
            let threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let all_boards = db::get_all_boards(&conn)?;
            Ok(templates::catalog_page(&board, &threads, &csrf_clone, &all_boards))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── GET /:board/search ───────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(q): Query<SearchQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);
    const SEARCH_PER_PAGE: i64 = 20;

    let query_str = q.q.trim().to_string();
    let page = q.page.max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_search_results(&conn, board.id, &query_str)?;
            let pagination = Pagination::new(page, SEARCH_PER_PAGE, total);
            let posts = db::search_posts(
                &conn,
                board.id,
                &query_str,
                SEARCH_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = db::get_all_boards(&conn)?;
            Ok(templates::search_page(
                &board,
                &query_str,
                &posts,
                &pagination,
                &csrf_clone,
                &all_boards,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /delete — user-initiated post deletion ─────────────────────────────

#[derive(serde::Deserialize)]
pub struct UserDeleteForm {
    pub post_id: i64,
    pub deletion_token: String,
    pub board: String,
    pub _csrf: Option<String>,
}

pub async fn delete_post(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UserDeleteForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form._csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let upload_dir = CONFIG.upload_dir.clone();
    let redirect_board = form.board.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;

            if !db::verify_deletion_token(&conn, form.post_id, &form.deletion_token)? {
                return Err(AppError::Forbidden("Incorrect deletion token.".into()));
            }

            let post = db::get_post(&conn, form.post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            let paths = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, form.post_id)?
            };

            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            info!("Post {} deleted by user", form.post_id);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{}/", redirect_board)).into_response())
}

// ─── CSRF cookie helper ───────────────────────────────────────────────────────

/// Ensure the CSRF token cookie is set. Returns (updated_jar, token_string).
pub fn ensure_csrf(jar: CookieJar) -> (CookieJar, String) {
    if let Some(cookie) = jar.get("csrf_token") {
        let token = cookie.value().to_string();
        if !token.is_empty() {
            return (jar, token);
        }
    }
    let token = new_csrf_token();
    let mut cookie = Cookie::new("csrf_token", token.clone());
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    (jar.add(cookie), token)
}
