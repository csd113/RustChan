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
        crypto::{hash_ip, new_deletion_token, verify_pow},
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name,
        },
        tripcode::parse_name_tripcode,
    },
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use tracing::info;

// ─── GET /:board/thread/:id ───────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let (jar, csrf) = ensure_csrf(jar);

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

            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound(format!("Thread {thread_id} not found")))?;

            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }

            // ETag derived from the thread's last-bump timestamp AND the
            // current board-list version.  The board version component ensures
            // that adding or deleting a board invalidates cached thread pages,
            // so the nav bar always reflects the current board list rather than
            // showing stale/deleted boards until the thread receives a reply.
            let boards_ver = crate::templates::live_boards_version();
            let etag = format!("\"{}-b{boards_ver}\"", thread.bumped_at);

            let posts = db::get_posts_for_thread(&conn, thread_id)?;
            let all_boards = db::get_all_boards(&conn)?;

            let ip_hash =
                crate::utils::crypto::hash_ip(&client_ip, &crate::config::CONFIG.cookie_secret);
            let thread_poll = db::get_poll_for_thread(&conn, thread_id, &ip_hash)?;

            let collapse_greentext = db::get_collapse_greentext(&conn);
            let html = crate::templates::thread_page(
                &board,
                &thread,
                &posts,
                &csrf_clone,
                &all_boards,
                is_admin,
                thread_poll.as_ref(),
                None,
                collapse_greentext,
            );
            Ok((etag, html))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let (etag, html) = result;

    // 3.2: Return 304 Not Modified when client's cached copy is still current.
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
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data = form.file;
    let audio_file_data = form.audio_file;
    let name_val = form.name;
    let del_token_val = form.deletion_token;
    let form_sage = form.sage;
    let pow_nonce = form.pow_nonce; // FIX[NEW-C1]: needed for per-reply PoW check
                                    // Extract admin session before spawn_blocking so we can skip the per-board
                                    // cooldown for admins (the cookie value is !Send and can't cross the boundary).
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
            let is_admin = admin_session_id
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());

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

            // FIX[NEW-C1]: PoW CAPTCHA check for replies, mirroring create_thread().
            // Previously this check was absent, allowing bots to bypass CAPTCHA on
            // captcha-protected boards by posting replies instead of new threads.
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

            crate::handlers::enqueue_post_jobs(
                &job_queue,
                post_id,
                &ip_hash,
                body_text.len(),
                uploaded.as_ref(),
                &board.short_name,
            );

            info!("Reply {post_id} posted in thread {thread_id} on /{}/", board.short_name);
            Ok(format!("/{}/thread/{thread_id}#p{post_id}", board.short_name))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → return a lightweight 422 page instead of re-querying the
    // entire thread (which wastes significant DB and CPU under spam load).
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let mut resp =
                axum::response::Html(crate::templates::error_page(422, &msg)).into_response();
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
                let all_boards = db::get_all_boards(&conn)?;
                let collapse_greentext = db::get_collapse_greentext(&conn);
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
                    &all_boards,
                    &token,
                    Some(err_msg),
                    collapse_greentext,
                );
                return Ok(EditOutcome::ErrorPage(html));
            }

            info!("Post {post_id} edited on /{}/", board.short_name);
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
                "Vote cast on poll {poll_id} option {option_id} by {}",
                &ip_hash[..8]
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
    let boards_version = crate::templates::live_boards_version();
    let boards = crate::templates::live_boards_snapshot();
    let nav_inner: String = boards
        .iter()
        .map(|b| {
            format!(
                r#"<a href="/{s}/catalog">{s}</a>"#,
                s = crate::utils::sanitize::escape_html(&b.short_name)
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    let nav_html = if nav_inner.is_empty() {
        String::new()
    } else {
        format!("[ {nav_inner} ]")
    };

    // Build a JSON envelope with new-post HTML plus current thread state.
    // boards_version / nav_html let the client keep the nav bar in sync when
    // boards are added or deleted while the user has a thread open.
    let json = format!(
        r#"{{"html":{html_json},"last_id":{last_id},"count":{count},"reply_count":{reply_count},"bump_time":{bump_time},"locked":{locked},"sticky":{sticky},"boards_version":{boards_version},"nav_html":{nav_html_json}}}"#,
        html_json = serde_json::to_string(&html).unwrap_or_else(|_| "\"\"".to_string()),
        last_id = last_id,
        count = count,
        reply_count = reply_count,
        bump_time = bump_time,
        locked = locked,
        sticky = sticky,
        boards_version = boards_version,
        nav_html_json = serde_json::to_string(&nav_html).unwrap_or_else(|_| "\"\"".to_string()),
    );

    Ok(([(header::CONTENT_TYPE, "application/json")], json).into_response())
}
