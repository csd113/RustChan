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
    models::{Pagination, SearchQuery, ThreadSummary},
    templates,
    utils::{
        crypto::{hash_ip, new_csrf_token, new_deletion_token, verify_pow},
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name, validate_subject,
        },
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::{header, HeaderMap},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use std::collections::HashMap;
use tracing::info;

const PREVIEW_REPLIES: i64 = 3;
const THREADS_PER_PAGE: i64 = 10;

// ─── GET / — board list ───────────────────────────────────────────────────────

pub async fn index(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let (jar, csrf) = ensure_csrf(jar);

    let (board_stats, site_data) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<crate::models::BoardStats>, crate::models::SiteStats)> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let site_data = db::get_site_stats(&conn).unwrap_or_default();
            Ok((boards, site_data))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Read the tor onion address from the hostname file if tor is enabled.
    let onion_address: Option<String> = if crate::config::CONFIG.enable_tor_support {
        let data_dir = std::path::PathBuf::from(&crate::config::CONFIG.database_path)
            .parent()
            .map_or_else(
                || std::path::PathBuf::from("."),
                std::path::Path::to_path_buf,
            );
        let hostname_path = data_dir.join("tor_hidden_service").join("hostname");
        std::fs::read_to_string(&hostname_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    Ok((
        jar,
        Html(templates::index_page(
            &board_stats,
            &site_data,
            &csrf,
            onion_address.as_deref(),
        )),
    ))
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let (jar, csrf) = ensure_csrf(jar);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<(String, String)> {
            let conn = pool.get()?;

            let is_admin = jar_session
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, THREADS_PER_PAGE, total);
            let threads =
                db::get_threads_for_board(&conn, board.id, THREADS_PER_PAGE, pagination.offset())?;

            // 3.2: Derive ETag from the most-recently-bumped thread on this page
            // combined with the page number.  This is a cheap proxy for "has
            // anything on this page changed?".
            let max_bump = threads.iter().map(|t| t.bumped_at).max().unwrap_or(0);
            let etag = format!("\"{max_bump}-{page}\"");

            let mut summaries = Vec::with_capacity(threads.len());
            for thread in threads {
                let total_replies = thread.reply_count;
                let preview = db::get_preview_posts(&conn, thread.id, PREVIEW_REPLIES)?;
                let omitted = (total_replies - i64::try_from(preview.len()).unwrap_or(0)).max(0);
                summaries.push(ThreadSummary {
                    thread,
                    preview_posts: preview,
                    omitted,
                });
            }

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            let html = templates::board_page(
                &board,
                &summaries,
                &pagination,
                &csrf_clone,
                &all_boards,
                is_admin,
                None,
                collapse_greentext,
            );
            Ok((etag, html))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let (etag, html) = result;

    // 3.2: Return 304 Not Modified when the client's cached version is current.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if client_etag == etag {
        let mut resp = axum::http::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .unwrap_or_default();
        resp.headers_mut().insert(
            "etag",
            axum::http::HeaderValue::from_str(&etag)
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("\"0\"")),
        );
        return Ok((jar, resp).into_response());
    }

    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    Ok((jar, resp).into_response())
}

// ─── POST /:board/ — create new thread ───────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn create_thread(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    multipart: Multipart,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let form = parse_post_multipart(multipart, csrf_cookie.as_deref()).await?;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;

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
    let subject_val = form.subject;
    let del_token_val = form.deletion_token;
    let poll_question = form.poll_question;
    let poll_options = form.poll_options;
    let poll_duration = form.poll_duration_secs;
    let pow_nonce = form.pow_nonce;

    // Extract admin session before spawn_blocking (cookie jar is !Send).
    let admin_session_id = jar.get("chan_admin_session").map(|c| c.value().to_string());
    // Also extract csrf_token before spawn_blocking so the ban page appeal form works.
    let ban_csrf_token = csrf_cookie.clone().unwrap_or_default();

    let _board_short_err = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = state.job_queue.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let ip_hash = hash_ip(&client_ip, &cookie_secret);
            if let Some(reason) = db::is_banned(&conn, &ip_hash)? {
                return Err(AppError::BannedUser {
                    reason: if reason.is_empty() {
                        "No reason given".to_string()
                    } else {
                        reason
                    },
                    csrf_token: ban_csrf_token,
                });
            }

            // Verify admin session — admins bypass the per-board cooldown entirely.
            let is_admin = admin_session_id
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());

            // Per-board post cooldown — the SOLE post rate control.
            // post_cooldown_secs = 0 means no cooldown at all; admins always bypass it.
            if board.post_cooldown_secs > 0 && !is_admin {
                let elapsed = db::get_seconds_since_last_post(&conn, board.id, &ip_hash)?;
                if let Some(secs) = elapsed {
                    let remaining = board.post_cooldown_secs.saturating_sub(secs);
                    if remaining > 0 {
                        return Err(AppError::BadRequest(format!("Please wait {remaining} more second{} before posting again.", if remaining == 1 { "" } else { "s" })));
                    }
                }
            }

            // PoW CAPTCHA — verified only when the board has it enabled
            if board.allow_captcha && !verify_pow(&board_short, &pow_nonce) {
                return Err(AppError::BadRequest(
                    "CAPTCHA verification failed. Please wait for the solver to complete before posting.".into()
                ));
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
            let subject = validate_subject(&subject_val);

            // Validate body: if the board allows media uploads a file may substitute
            // for text, but at least one of the two must be non-empty.
            let board_allows_media = board.allow_images || board.allow_video || board.allow_audio;
            let has_file = file_data.is_some();
            let body_text = if board_allows_media {
                validate_body_with_file(&raw_body, has_file).map_err(AppError::BadRequest)?
            } else {
                validate_body(&raw_body)
                    .map_err(AppError::BadRequest)?
                    .to_string()
            };

            // FIX[MEDIUM-8]: Apply word filters BEFORE HTML escaping so that
            // filter patterns are plain text, not HTML-entity strings.
            let filtered_body = apply_word_filters(&body_text, &filters);
            let escaped_body = escape_html(&filtered_body);
            let body_html = render_post_body(&escaped_body);

            let uploaded = crate::handlers::process_primary_upload(
                file_data,
                &board,
                &conn,
                &upload_dir,
                thumb_size,
                max_image_size,
                max_video_size,
                max_audio_size,
                ffmpeg_available,
            )?;

            // ── Image+audio combo ─────────────────────────────────────────────
            let audio_uploaded = crate::handlers::process_audio_combo(
                audio_file_data,
                uploaded.as_ref(),
                &board,
                &upload_dir,
                max_audio_size,
            )?;

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
                thread_id: 0, // will be overwritten by create_thread_with_op
                board_id: board.id,
                name,
                tripcode,
                subject: subject.clone(),
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
                is_op: true,
            };
            let (thread_id, post_id) =
                db::create_thread_with_op(&conn, board.id, subject.as_deref(), &new_post)?;

            // Create poll if question + at least 2 options were supplied
            let q = poll_question.trim().to_string();
            let valid_opts: Vec<String> = poll_options
                .iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect();
            if !q.is_empty() && valid_opts.len() >= 2 {
                let secs = poll_duration.ok_or_else(|| {
                    AppError::BadRequest(
                        "A duration is required when creating a poll.".into(),
                    )
                })?;
                let secs = secs.clamp(60, 30 * 24 * 3600); // clamp 1 min..30 days
                let expires_at = chrono::Utc::now().timestamp() + secs;
                db::create_poll(&conn, thread_id, &q, &valid_opts, expires_at)?;
            }

            // ── Background jobs ───────────────────────────────────────────────
            // 1 & 2. Media post-processing + spam check (shared helper)
            crate::handlers::enqueue_post_jobs(
                &job_queue,
                post_id,
                &ip_hash,
                body_text.len(),
                uploaded.as_ref(),
                &board.short_name,
            );

            // 3. Thread pruning — now async so HTTP response returns immediately.
            let max_threads = board.max_threads;
            let _ = job_queue.enqueue(&crate::workers::Job::ThreadPrune {
                board_id: board.id,
                board_short: board.short_name.clone(),
                max_threads,
                allow_archive: board.allow_archive,
            });

            info!("New thread {thread_id} created in /{}/", board.short_name);
            Ok(format!("/{}/thread/{thread_id}", board.short_name))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → return a lightweight 422 page instead of re-querying the
    // entire board index (which wastes significant DB and CPU under spam load).
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let mut resp = Html(templates::error_page(422, &msg)).into_response();
            *resp.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(resp);
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
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<String> {
            let conn = pool.get()?;
            let is_admin = jar_session
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::catalog_page(
                &board,
                &threads,
                &csrf_clone,
                &all_boards,
                is_admin,
                collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── GET /:board/archive ──────────────────────────────────────────────────────

pub async fn board_archive(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    const ARCHIVE_PER_PAGE: i64 = 20;
    let (jar, csrf) = ensure_csrf(jar);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            if !board.allow_archive {
                return Err(AppError::NotFound(format!(
                    "/{board_short}/ does not have an archive."
                )));
            }

            let total = db::count_archived_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, ARCHIVE_PER_PAGE, total);
            let threads = db::get_archived_threads_for_board(
                &conn,
                board.id,
                ARCHIVE_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = db::get_all_boards(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::archive_page(
                &board,
                &threads,
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

// ─── GET /:board/search ───────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(q): Query<SearchQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    const SEARCH_PER_PAGE: i64 = 20;
    let (jar, csrf) = ensure_csrf(jar);

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

// ─── CSRF cookie helper ───────────────────────────────────────────────────────

/// Ensure the CSRF token cookie is set. Returns (`updated_jar`, `token_string`).
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

// ─── POST /report ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct ReportForm {
    pub post_id: i64,
    #[allow(dead_code)]
    pub thread_id: i64,
    pub board: String,
    pub reason: Option<String>,
    pub csrf: Option<String>,
}

pub async fn file_report(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<ReportForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let ip_hash = hash_ip(&client_ip, &CONFIG.cookie_secret);
    let reason = form
        .reason
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    let post_id = form.post_id;
    let board_raw = form
        .board
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();

    let board_raw_closure = board_raw.clone();
    let db_thread_id = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<i64> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_raw_closure)?
                .ok_or_else(|| AppError::NotFound("Board not found.".into()))?;
            // Verify post exists and belongs to this board to prevent spoofed reports.
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.board_id != board.id {
                return Err(AppError::BadRequest(
                    "Post does not belong to this board.".into(),
                ));
            }
            // Use the DB's thread_id for the redirect — not the user-submitted value.
            let authoritative_thread_id = post.thread_id;
            db::file_report(
                &conn,
                post_id,
                authoritative_thread_id,
                board.id,
                &reason,
                &ip_hash,
            )?;
            Ok(authoritative_thread_id)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Redirect back to the thread using the DB-resolved IDs.
    // `board_raw` is already sanitised to alphanumeric earlier in this handler.
    Ok(Redirect::to(&format!(
        "/{board_raw}/thread/{db_thread_id}#p{}",
        form.post_id
    ))
    .into_response())
}

// ─── GET /boards/{*media_path} — serve media with mp4→webm redirect ──────────
//
// Replaces the former nest_service(ServeDir) so we can intercept stale .mp4
// links (created before the background transcoder replaced them with .webm)
// and issue a permanent redirect. All other paths are served via ServeFile.

pub async fn serve_board_media(
    Path(media_path): Path<String>,
    req: axum::extract::Request,
) -> Response {
    use axum::http::StatusCode;
    use std::path::PathBuf;
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    // Reject path-traversal attempts and absolute-path escapes.
    if media_path.contains("..") || media_path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let base = PathBuf::from(&CONFIG.upload_dir);
    let target = base.join(&media_path);

    // Verify the resolved path is still inside the upload directory.
    // This catches any edge cases that slip past the string checks above
    // (e.g. symlinks, exotic percent-encoding handled by the OS).
    if !target.starts_with(&base) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    if target.exists() {
        // File present — forward the real request (with Range, ETag, etc.) to
        // ServeFile so it can respond with 206 Partial Content when needed.
        // iOS Safari requires Range request support to play video — dropping
        // the request headers caused it to receive 200 instead of 206 and
        // refuse playback on videos it tried to stream in chunks.
        let req = req.map(|_| axum::body::Body::empty());
        ServeFile::new(&target).oneshot(req).await.map_or_else(
            |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            |resp| resp.map(axum::body::Body::new).into_response(),
        )
    } else if std::path::Path::new(&media_path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        // MP4 was transcoded away — redirect permanently to the .webm sibling.
        let webm_path_str = format!("{}.webm", &media_path[..media_path.len() - 4]);
        let webm_abs = base.join(&webm_path_str);
        if webm_abs.exists() {
            Redirect::permanent(&format!("/boards/{webm_path_str}")).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

// ─── GET /api/post/{board}/{post_id} ──────────────────────────────────────────
//
// Lightweight JSON endpoint for cross-board quotelink hover previews.
//
// `post_id` is the **global** post ID (the AUTOINCREMENT primary key of the
// `posts` table).  The board name is used only to validate ownership — a link
// like >>>/tech/12345 will 404 if post 12345 actually lives on /b/, preventing
// cross-board information leakage.
//
// Response on success:
//   { "html": "<div class=\"post …\">…</div>", "thread_id": 42 }
// The `thread_id` field lets the client update the link's href to the canonical
// /{board}/thread/{thread_id}#p{post_id} URL after the first hover.
//
// Response on failure: 404 { "error": "not found" }

pub async fn api_post_preview(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
) -> impl axum::response::IntoResponse {
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<Option<(String, i64)>> {
            let conn = pool.get()?;

            // Fetch the post, validating it belongs to this board.
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            match post {
                None => Ok(None),
                Some(p) => {
                    let thread_id = p.thread_id;
                    let html = crate::templates::render_post(
                        &p,
                        &board_short,
                        "",
                        crate::templates::thread::RenderPostOpts {
                            show_delete: false,
                            is_admin: false,
                            show_media: true,
                            allow_editing: false, // no edit link in read-only preview
                        },
                        0, // no edit window
                    );
                    Ok(Some((html, thread_id)))
                }
            }
        }
    })
    .await;

    let json_ct = [(header::CONTENT_TYPE, "application/json")];

    match result {
        Ok(Ok(Some((html, thread_id)))) => {
            let body =
                serde_json::to_string(&serde_json::json!({ "html": html, "thread_id": thread_id }))
                    .unwrap_or_else(|_| r#"{"html":"","thread_id":0}"#.to_string());
            (axum::http::StatusCode::OK, json_ct, body).into_response()
        }
        Ok(Ok(None)) => {
            let body = r#"{"error":"not found"}"#.to_string();
            (axum::http::StatusCode::NOT_FOUND, json_ct, body).into_response()
        }
        _ => {
            let body = r#"{"error":"internal error"}"#.to_string();
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, json_ct, body).into_response()
        }
    }
}

// ─── GET /{board}/post/{post_id} ──────────────────────────────────────────────
//
// Canonical redirect for `>>>/board/N` links.  Resolves the global post ID to
// its containing thread and issues a 302 to /{board}/thread/{thread_id}#p{post_id}.
//
// Users clicking a cross-board quotelink land here on the first click; after
// the first hover preview the JS upgrades the href in-place so subsequent
// clicks go directly to the thread anchor without a server round-trip.

pub async fn redirect_to_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
) -> impl axum::response::IntoResponse {
    use axum::response::Redirect;

    let board_short_for_url = board_short.clone();
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<Option<i64>> {
            let conn = pool.get()?;
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            Ok(post.map(|p| p.thread_id))
        }
    })
    .await;

    if let Ok(Ok(Some(thread_id))) = result {
        let url = format!("/{board_short_for_url}/thread/{thread_id}#p{post_id}");
        Redirect::to(&url).into_response()
    } else {
        // Post not found or wrong board — render the error page template
        // so the user gets a readable message instead of a blank HTTP 404.
        // This is the fallback path when JavaScript is disabled or when
        // a user manually navigates to a quotelink URL after a board
        // restore that assigned new IDs to the restored posts.
        let html = crate::templates::error_page(
            404,
            &format!("Post #{post_id} not found. It may have been deleted or the board was restored from a backup."),
        );
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::response::Html(html),
        )
            .into_response()
    }
}

// ─── POST /appeal ─────────────────────────────────────────────────────────────
// Banned users submit a brief appeal message here.
// Appeals appear in the admin panel under // ban appeals.

#[derive(serde::Deserialize)]
pub struct AppealForm {
    pub reason: String,
    pub csrf: Option<String>,
}

pub async fn submit_appeal(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> impl axum::response::IntoResponse {
    use axum::response::Html;

    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Html(crate::templates::error_page(403, "CSRF token mismatch.")).into_response();
    }

    let ip_hash = hash_ip(&client_ip, &CONFIG.cookie_secret);
    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return Html(crate::templates::error_page(
            400,
            "Appeal message cannot be empty.",
        ))
        .into_response();
    }

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<&'static str> {
            let conn = pool.get()?;
            // Rate-limit: one appeal per IP per 24 hours
            if db::has_recent_appeal(&conn, &ip_hash)? {
                return Ok("already_filed");
            }
            // Only allow appeals from actually-banned IPs
            if db::is_banned(&conn, &ip_hash)?.is_none() {
                return Ok("not_banned");
            }
            db::file_ban_appeal(&conn, &ip_hash, &reason)?;
            Ok("ok")
        }
    })
    .await;

    let msg = match result {
        Ok(Ok("ok")) => "Your appeal has been submitted. An admin will review it.",
        Ok(Ok("already_filed")) => "You have already filed an appeal in the last 24 hours.",
        Ok(Ok("not_banned")) => "Your IP is not currently banned.",
        _ => "An error occurred. Please try again.",
    };

    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Appeal Submitted</title>
<link rel="stylesheet" href="/static/style.css">
</head><body><div class="page-box error-page">
<h1>appeal submitted</h1>
<p>{msg}</p>
<p><a href="/">return home</a></p>
</div></body></html>"#,
        msg = crate::utils::sanitize::escape_html(msg)
    );
    Html(html).into_response()
}
