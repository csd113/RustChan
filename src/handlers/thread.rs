// handlers/thread.rs
//
// Handles:
//   GET  /:board/thread/:id   — view thread with all posts
//   POST /:board/thread/:id   — post a reply
//   POST /vote                — cast a poll vote

use crate::{
    config::CONFIG,
    db::{self},
    error::{AppError, Result},
    handlers::{board::ensure_csrf, parse_post_multipart, posting, render},
    middleware::{validate_csrf, AppState},
    utils::crypto::{hash_ip, verify_pow},
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

// ─── GET /:board/thread/:id ───────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar_session = jar.get("chan_admin_session").map(|c| c.value().to_string());
        move || -> Result<(String, String, bool)> {
            let conn = pool.get()?;
            let page_data = render::load_thread_page_data(
                &conn,
                &board_short,
                thread_id,
                &identity_key,
                jar_session.as_deref(),
                &crate::config::CONFIG.cookie_secret,
            )?;

            // ETag derived from the thread's last-bump timestamp, the current
            // board-list version, and whether the viewer is an admin.  Including
            // admin status prevents a browser from serving a cached non-admin
            // page (without delete controls) to a user who has since logged in.
            let boards_ver = crate::templates::live_boards_version();
            let admin_tag = if page_data.is_admin { "-a" } else { "" };
            let greentext_tag = if page_data.board.collapse_greentext {
                "-cg1"
            } else {
                "-cg0"
            };
            let etag = format!(
                "\"{}-b{boards_ver}{admin_tag}{greentext_tag}\"",
                page_data.thread.bumped_at
            );
            let html =
                render::render_thread_page(&page_data, &csrf, None, current_theme.as_deref());
            Ok((etag, html, page_data.is_admin))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let (etag, html, _is_admin) = result;

    // 3.2: Return 304 Not Modified when client's cached copy is still current.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if client_etag == etag {
        // StatusCode::NOT_MODIFIED and Body::empty() are always valid; this
        // builder call is infallible in practice.
        let mut resp = axum::http::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        resp.headers_mut().insert(
            "etag",
            axum::http::HeaderValue::from_str(&etag)
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("\"0\"")),
        );
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, no-cache, must-revalidate"),
        );
        return Ok((jar, resp).into_response());
    }

    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-cache, must-revalidate"),
    );
    Ok((jar, resp).into_response())
}

// ─── POST /:board/thread/:id — post reply ────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn post_reply(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
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
    let ffmpeg_webp_available = state.ffmpeg_webp_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data = form.file;
    let audio_file_data = form.audio_file;
    let image_file_data = form.image_file;
    let name_val = form.name;
    let del_token_val = form.deletion_token;
    let form_sage = form.sage;
    let pow_nonce = form.pow_nonce; // needed for per-reply PoW check
                                    // Extract admin session before spawn_blocking so we can skip the per-board
                                    // cooldown for admins (the cookie value is !Send and can't cross the boundary).
    let admin_session_id = jar.get("chan_admin_session").map(|c| c.value().to_string());
    // Also extract csrf_token before spawn_blocking so the ban page appeal form works.
    let ban_csrf_token = csrf_cookie.clone().unwrap_or_default();

    // Clones kept outside the closure so we can re-render the thread page inline on error.
    let board_short_err = board_short.clone();
    let admin_session_err = admin_session_id.clone();
    let csrf_for_error = csrf_cookie.clone().unwrap_or_default();

    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let identity_key_err = identity_key.clone();
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

            let ip_hash = hash_ip(&identity_key, &cookie_secret);
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

            // Per-board post cooldown — the SOLE post rate control.
            // Verify admin session first; admins bypass the cooldown entirely.
            let is_admin = posting::is_admin_session(&conn, admin_session_id.as_deref());

            // post_cooldown_secs = 0 means no cooldown at all on this board.
            if board.post_cooldown_secs > 0 && !is_admin {
                let elapsed = db::get_seconds_since_last_post(&conn, board.id, &ip_hash)?;
                if let Some(secs) = elapsed {
                    let remaining = board.post_cooldown_secs.saturating_sub(secs);
                    if remaining > 0 {
                        return Err(AppError::BadRequest(format!("Please wait {remaining} more second{} before posting again.", if remaining == 1 { "" } else { "s" })));
                    }
                }
            }

            // PoW CAPTCHA check for replies, mirroring create_thread().
            // Previously this check was absent, allowing bots to bypass CAPTCHA on
            // captcha-protected boards by posting replies instead of new threads.
            if board.allow_captcha && !verify_pow(&board_short, &pow_nonce) {
                return Err(AppError::BadRequest(
                    "CAPTCHA verification failed. Please wait for the solver to complete before posting.".into()
                ));
            }

            let filters = posting::load_word_filters(&conn)?;
            let (name, tripcode) = posting::resolve_post_identity(&name_val, board.allow_tripcodes);
            let board_allows_media = board.allow_images
                || board.allow_video
                || board.allow_audio
                || (crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files);
            let has_file = file_data.is_some() || audio_file_data.is_some() || image_file_data.is_some();
            let (body_text, body_html) =
                posting::build_post_body(&raw_body, has_file, board_allows_media, &filters)?;

            let uploads = posting::process_uploads(
                image_file_data,
                file_data,
                audio_file_data,
                &board,
                &conn,
                &posting::UploadConfig {
                    upload_dir: &upload_dir,
                    thumb_size,
                    max_image_size,
                    max_video_size,
                    max_audio_size,
                    ffmpeg_available,
                    ffmpeg_webp_available,
                },
            )?;

            let deletion_token = posting::resolve_deletion_token(&del_token_val);

            // Sage suppresses the bump regardless of reply count.
            let should_bump = !form_sage && thread.reply_count < board.bump_limit;

            let new_post = posting::build_new_post(
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
            let pending_upload_op = posting::build_pending_upload_op(&uploads)?;
            let post_id = match db::create_reply_with_thread_update(
                &conn,
                &new_post,
                should_bump,
                pending_upload_op.as_ref(),
            ) {
                Ok(post_id) => post_id,
                Err(error) => {
                    uploads.rollback_new_files(&conn, &upload_dir)?;
                    return Err(error.into());
                }
            };
            posting::finalize_pending_uploads(&conn, &upload_dir, &uploads);

            crate::handlers::enqueue_post_jobs(
                &job_queue,
                post_id,
                &ip_hash,
                body_text.len(),
                uploads.primary.as_ref(),
                &board.short_name,
            );

            tracing::info!(target: "board", post_id = post_id, thread_id = thread_id, board = %board.short_name, "Reply posted");
            Ok(format!("/{}/thread/{thread_id}#p{post_id}", board.short_name))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → re-render the thread page with an inline error banner so the
    // user sees the message in context (e.g. "wait for captcha to solve") without
    // being redirected to a separate error page and losing their scroll position.
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let db_pool = state.db.clone();
            let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
            let html = tokio::task::spawn_blocking(move || -> Result<String> {
                let conn = db_pool.get()?;
                let page_data = render::load_thread_page_data(
                    &conn,
                    &board_short_err,
                    thread_id,
                    &identity_key_err,
                    admin_session_err.as_deref(),
                    &CONFIG.cookie_secret,
                )?;
                Ok(render::render_thread_page(
                    &page_data,
                    &csrf_for_error,
                    Some(&msg),
                    current_theme.as_deref(),
                ))
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

            let mut resp = axum::response::Html(html).into_response();
            *resp.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(resp);
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

#[allow(clippy::arithmetic_side_effects)]
pub async fn edit_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    Query(query): Query<EditQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
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

            // edit_window_secs = 0 means no time restriction (always editable while
            // allow_editing is true).  Any positive value is enforced as a hard deadline.
            if board.edit_window_secs > 0 {
                let now = chrono::Utc::now().timestamp();
                if now - post.created_at > board.edit_window_secs {
                    return Err(AppError::Forbidden(
                        "The edit window for this post has closed.".into(),
                    ));
                }
            }

            let all_boards = crate::templates::live_boards();

            Ok(crate::templates::edit_post_page(
                &board,
                &post,
                &csrf,
                all_boards.as_slice(),
                &prefill_token,
                None,
                current_theme.as_deref(),
                board.collapse_greentext,
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
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
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

#[allow(clippy::arithmetic_side_effects)]
pub async fn edit_post_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    Form(form): Form<EditForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;
    let token = form.deletion_token;

    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
            let csrf_clone = csrf_cookie.clone().unwrap_or_default();
        let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
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

            // edit_window_secs = 0 means no time restriction.  A positive value is
            // enforced; we only distinguish "window closed" vs "wrong token" when a
            // window is actually configured.
            if !success {
                let all_boards = crate::templates::live_boards();
                let err_msg = if board.edit_window_secs > 0 {
                    let now = chrono::Utc::now().timestamp();
                    if now - post.created_at > board.edit_window_secs {
                        "The edit window for this post has closed."
                    } else {
                        "Incorrect edit token."
                    }
                } else {
                    "Incorrect edit token."
                };
                let html = crate::templates::edit_post_page(
                    &board,
                    &post,
                    &csrf_clone,
                    all_boards.as_slice(),
                    &token,
                    Some(err_msg),
                    current_theme.as_deref(),
                    board.collapse_greentext,
                );
                return Ok(EditOutcome::ErrorPage(html));
            }

            tracing::info!(target: "board", post_id = post_id, board = %board.short_name, "Post edited");
            Ok(EditOutcome::Redirect(format!(
                "/{}/thread/{}#p{post_id}",
                board.short_name, post.thread_id
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
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub option_id: i64,
}

pub async fn vote_handler(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<VoteForm>,
) -> Result<Response> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let cookie_secret = CONFIG.cookie_secret.clone();
    let option_id = form.option_id;

    // Reject non-positive IDs before touching the DB.
    if option_id <= 0 {
        return Err(AppError::BadRequest("Invalid poll option.".into()));
    }

    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let redirect_url = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let ip_hash = hash_ip(&identity_key, &cookie_secret);

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
            tracing::info!(
                target: "board",
                poll_id = poll_id,
                option_id = option_id,
                "Vote cast"
            );
            Ok(format!("/{board_short}/thread/{thread_id}#poll"))
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

#[derive(serde::Serialize)]
struct ThreadUpdatesPayload {
    html: String,
    last_id: i64,
    count: usize,
    reply_count: i64,
    bump_time: i64,
    locked: bool,
    sticky: bool,
    boards_version: u64,
    nav_html: String,
}

pub async fn thread_updates(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    Query(params): Query<UpdatesQuery>,
) -> Result<Response> {
    let since = params.since;

    let (html, last_id, count, reply_count, bump_time, locked, sticky) =
        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> crate::error::Result<(String, i64, usize, i64, i64, bool, bool)> {
                let conn = pool.get()?;

                // Validate board + thread exist (returns 404 for bad URLs).
                let board = db::get_board_by_short(&conn, &board_short)?
                    .ok_or_else(|| crate::error::AppError::NotFound("Board not found.".into()))?;
                let thread = db::get_thread(&conn, thread_id)?
                    .ok_or_else(|| crate::error::AppError::NotFound("Thread not found.".into()))?;

                // Fetch posts newer than `since`, ordered oldest-first so they
                // render in the correct chronological order when appended.
                let posts = db::get_new_posts_since(&conn, thread_id, since, 100)?;
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
                        crate::templates::thread::RenderPostOpts {
                            show_delete: false,
                            is_admin: false,
                            show_media: true,
                            allow_editing: false, // no edit link in auto-appended HTML; reload restores it
                            show_poster_ids: thread.board_id == board.id && board.show_poster_ids,
                            thread_op_id: thread.op_id,
                        },
                        0,
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

    // Current board-list version + rendered nav links — lets the JS refresh
    // the nav bar when boards are added or deleted while a thread is open,
    // without requiring a full page reload.
    let (boards_version, nav_html) = crate::templates::live_board_nav();
    let payload = ThreadUpdatesPayload {
        html,
        last_id,
        count,
        reply_count,
        bump_time,
        locked,
        sticky,
        boards_version,
        nav_html: nav_html.as_ref().to_string(),
    };

    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&payload)
            .map_err(|error| crate::error::AppError::Internal(anyhow::anyhow!(error)))?,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn post_reply_persists_quote_markup() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/{board}", post(crate::handlers::board::create_thread))
            .route("/{board}/thread/{id}", post(super::post_reply))
            .with_state(state.clone());

        let (create_boundary, create_body) =
            crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "op body")], None);
        let create_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={create_boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(create_body))
                    .expect("create request"),
            )
            .await
            .expect("create response");
        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);

        let (thread_id, op_post_id) = {
            let conn = state.db.get().expect("db connection");
            let thread_id = conn
                .query_row("SELECT id FROM threads LIMIT 1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("thread id");
            let op_post_id = conn
                .query_row(
                    "SELECT id FROM posts WHERE thread_id = ?1 AND is_op = 1",
                    rusqlite::params![thread_id],
                    |row| row.get::<_, i64>(0),
                )
                .expect("op post id");
            (thread_id, op_post_id)
        };

        let quoted_body = format!(">>{op_post_id}\nreply body");
        let (reply_boundary, reply_body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", &quoted_body)],
            None,
        );
        let reply_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/thread/{thread_id}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={reply_boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(reply_body))
                    .expect("reply request"),
            )
            .await
            .expect("reply response");
        assert_eq!(reply_response.status(), StatusCode::SEE_OTHER);

        let reply_html = {
            let conn = state.db.get().expect("db connection");
            conn.query_row(
                "SELECT body_html FROM posts WHERE thread_id = ?1 AND is_op = 0 ORDER BY id DESC LIMIT 1",
                rusqlite::params![thread_id],
                |row| row.get::<_, String>(0),
            )
            .expect("reply body html")
        };
        assert!(reply_html.contains("class=\"quotelink\""));
        assert!(reply_html.contains(&format!("data-pid=\"{op_post_id}\"")));
    }
}
