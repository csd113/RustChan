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
    handlers::parse_post_multipart,
    middleware::{validate_csrf, AppState},
    models::*,
    templates,
    utils::{
        // FIX[LOW-8]: sha256_hex now lives in utils::crypto (deduplicated)
        crypto::{hash_ip, new_csrf_token, new_deletion_token, sha256_hex},
        files::save_upload,
        sanitize::{
            // FIX[MEDIUM-8]: apply_word_filters now runs before escape_html
            apply_word_filters, escape_html, render_post_body, validate_body, validate_body_with_file, validate_name,
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

    let (board_stats, site_stats) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<crate::models::BoardStats>, crate::models::SiteStats)> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let stats  = db::get_site_stats(&conn).unwrap_or_default();
            Ok((boards, stats))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(templates::index_page(&board_stats, &site_stats, &csrf))))
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

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        // FIX[HIGH-2]: is_admin_session check moved inside spawn_blocking so
        // the blocking DB call does not stall the Tokio worker thread.
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
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::board_page(
                &board,
                &summaries,
                &pagination,
                &csrf_clone,
                &all_boards,
                is_admin,
                None,
                collapse_greentext,
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

    let raw_body = form.body;

    let client_ip     = addr.ip().to_string();
    let upload_dir    = CONFIG.upload_dir.clone();
    let thumb_size    = CONFIG.thumb_size;
    let max_image_size    = CONFIG.max_image_size;
    let max_video_size    = CONFIG.max_video_size;
    let max_audio_size    = CONFIG.max_audio_size;
    let ffmpeg_available  = state.ffmpeg_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data     = form.file;
    let name_val      = form.name;
    let subject_val   = form.subject;
    let del_token_val = form.deletion_token;
    let poll_question  = form.poll_question;
    let poll_options   = form.poll_options;
    let poll_duration  = form.poll_duration_secs;

    let board_short_err = board_short.clone();
    let result = tokio::task::spawn_blocking({
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
            // Respect per-board tripcode setting
            let tripcode = if board.allow_tripcodes { tripcode } else { None };
            let subject = validate_subject(&subject_val);

            // Validate body: if the board allows media uploads a file may substitute
            // for text, but at least one of the two must be non-empty.
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

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping so that
            // filter patterns are plain text, not HTML-entity strings.
            let filtered_body    = apply_word_filters(&body_text, &filters);
            let escaped_body     = escape_html(&filtered_body);
            let body_html        = render_post_body(&escaped_body);

            let uploaded = if let Some((data, fname)) = file_data {
                // Detect media type from magic bytes to enforce per-board toggles.
                // We call detect_mime_type here for a quick classification without
                // doing a full save; the real detection runs again in save_upload.
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
                // Cap at 64 chars to prevent abuse; anything longer is almost
                // certainly not a legitimate user-chosen token.
                del_token_val.trim().chars().take(64).collect()
            };

            // FIX[MEDIUM-3]: Thread creation and OP post insertion are now
            // wrapped in a single transaction via create_thread_with_op.
            // Previously, a crash between the two calls left an orphaned thread.
            let new_post = NewPost {
                thread_id:  0,  // will be overwritten by create_thread_with_op
                board_id: board.id,
                name,
                tripcode,
                subject: subject.clone(),
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
                is_op: true,
            };
            let (thread_id, _post_id) = db::create_thread_with_op(
                &conn,
                board.id,
                subject.as_deref(),
                &new_post,
            )?;

            // Create poll if question + at least 2 options were supplied
            let q = poll_question.trim().to_string();
            let valid_opts: Vec<String> = poll_options.iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect();
            if !q.is_empty() && valid_opts.len() >= 2 {
                if let Some(secs) = poll_duration {
                    let secs = secs.clamp(60, 30 * 24 * 3600); // clamp 1 min..30 days
                    let expires_at = chrono::Utc::now().timestamp() + secs;
                    db::create_poll(&conn, thread_id, &q, &valid_opts, expires_at)?;
                }
            }

            let max_threads = board.max_threads;
            let paths = db::prune_old_threads(&conn, board.id, max_threads)?;
            for path in paths {
                crate::utils::files::delete_file(&upload_dir, &path);
            }

            info!("New thread {} created in /{}/", thread_id, board.short_name);
            Ok(format!("/{}/thread/{}", board.short_name, thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → re-render the board index with an inline error banner.
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
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
                    let all_boards = db::get_all_boards(&conn).unwrap_or_default();
                    let total      = db::count_threads_for_board(&conn, board.id).unwrap_or(0);
                    let pagination = crate::models::Pagination::new(1, 10, total);
                    let threads    = db::get_threads_for_board(&conn, board.id, 10, 0).unwrap_or_default();
                    let summaries: Vec<crate::models::ThreadSummary> = threads.into_iter().map(|t| {
                        let preview = db::get_preview_posts(&conn, t.id, 3).unwrap_or_default();
                        let omitted = (t.reply_count - preview.len() as i64).max(0);
                        crate::models::ThreadSummary { thread: t, preview_posts: preview, omitted }
                    }).collect();
                    templates::board_page(&board, &summaries, &pagination, &csrf_err, &all_boards, false, Some(&msg), db::get_collapse_greentext(&conn))
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
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::catalog_page(&board, &threads, &csrf_clone, &all_boards, collapse_greentext))
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

    // Cap query length to prevent excessively large LIKE pattern scans.
    let query_str: String = q.q.trim().chars().take(256).collect();
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
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::search_page(
                &board,
                &query_str,
                &posts,
                &pagination,
                &csrf_clone,
                &all_boards,
                collapse_greentext,
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
    #[allow(dead_code)]
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

    // FIX[MEDIUM-10]: Validate the board short-name to prevent open-redirect
    // or path-confusion attacks via a crafted form.board value.
    // We look up the post's actual board from the DB and use that for the
    // redirect, ignoring the user-supplied board name entirely.
    let upload_dir = CONFIG.upload_dir.clone();

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            if !db::verify_deletion_token(&conn, form.post_id, &form.deletion_token)? {
                return Err(AppError::Forbidden("Incorrect deletion token.".into()));
            }

            let post = db::get_post(&conn, form.post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            // FIX[MEDIUM-10]: Resolve the board short-name from the database
            // using the post's board_id, never from the user-supplied form field.
            let board = {
                let boards = db::get_all_boards(&conn)?;
                boards.into_iter()
                    .find(|b| b.id == post.board_id)
                    .map(|b| b.short_name)
                    .unwrap_or_else(|| "unknown".to_string())
            };

            let paths = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, form.post_id)?
            };

            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            info!("Post {} deleted by user", form.post_id);
            Ok(board)
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
    // http_only=false is intentional for the double-submit CSRF pattern —
    // the token must be readable by the page so forms can embed it.
    // XSS is mitigated by SameSite=Strict and thorough HTML escaping.
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    // FIX[MEDIUM-11]: set Secure flag based on config (true when behind proxy / HTTPS)
    cookie.set_secure(CONFIG.https_cookies);
    (jar.add(cookie), token)
}
