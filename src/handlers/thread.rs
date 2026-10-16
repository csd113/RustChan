/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers::thread");

// Routes:
//   GET  /:board/thread/:id   — view a bounded thread window
//   POST /:board/thread/:id   — post a reply
//   POST /vote                — cast a poll vote

use crate::{
    config::CONFIG,
    db::{self},
    error::{AppError, Result},
    handlers::{
        board::{
            admin_scoped_csrf_token, check_csrf_jar, ensure_csrf_for_request,
            ensure_csrf_with_secure, BoardAccessContext,
        },
        is_xml_http_request, parse_post_multipart, posting, render, PostFormData,
    },
    middleware::AppState,
    utils::crypto::hash_ip,
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

type ThreadViewLoadResult = (
    String,
    render::ThreadPageData,
    bool,
    bool,
    bool,
    bool,
    Option<(i64, i64)>,
);

// GET /:board/thread/:id
#[expect(
    clippy::too_many_lines,
    reason = "access control, thread loading, unread-state calculation, and rendering form one request"
)]
pub(in crate::server) async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    Query(params): Query<ThreadPageQuery>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::middleware::SecureCookieContext,
) -> Result<Response> {
    if params.before.is_some_and(|id| id <= 0) || params.post.is_some_and(|id| id <= 0) {
        return Err(AppError::BadRequest(
            "Thread cursors must be positive post IDs.".into(),
        ));
    }
    // The largest SQLite ID is already in the latest window; it cannot have
    // an exclusive successor cursor. Preserve post focus at that boundary.
    let before = params.post.map_or(params.before, |id| id.checked_add(1));
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let user_preferences = crate::handlers::board::user_preferences_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let owned_post_grants = crate::handlers::board::owned_post_grants_from_jar(&jar);
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let return_to = format!("/{board_short}/thread/{thread_id}");
    let access_context = match crate::handlers::board::board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        crate::handlers::board::BoardAccessRequirement::View,
        return_to,
    )
    .await?
    {
        crate::handlers::board::BoardAccessDecision::Allowed(context) => context,
        crate::handlers::board::BoardAccessDecision::Denied(denial) => {
            return Ok(crate::handlers::board::board_access_denied_response(
                jar,
                &denial,
                &csrf,
                current_theme.as_deref(),
            ));
        }
    };

    let BoardAccessContext {
        board,
        is_admin,
        can_post,
        can_view: _,
    } = access_context;
    let page_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<ThreadViewLoadResult> {
            let conn = pool.get()?;
            let page_data = render::load_thread_page_data_before(
                &conn,
                board,
                thread_id,
                &identity_key,
                &CONFIG.cookie_secret,
                is_admin,
                before,
            )?;
            let effective_is_admin = page_data.is_admin;
            let thread_badges_enabled = db::get_thread_new_reply_badges_enabled(&conn);
            let homepage_thread_badges_enabled = db::get_homepage_new_thread_badges_enabled(&conn);
            let homepage_reply_badges_enabled = db::get_homepage_new_reply_badges_enabled(&conn);
            let board_id = page_data.board.id;
            Ok((
                render::thread_page_etag_signature(&page_data),
                page_data,
                effective_is_admin,
                thread_badges_enabled,
                homepage_thread_badges_enabled,
                homepage_reply_badges_enabled,
                if homepage_thread_badges_enabled {
                    db::get_latest_visible_thread_marker(&conn, board_id)?
                } else {
                    None
                },
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let (
        thread_sig,
        mut page_data,
        _is_admin,
        thread_badges_enabled,
        homepage_thread_badges_enabled,
        homepage_reply_badges_enabled,
        latest_thread_marker,
    ) = page_data;
    page_data.owned_post_controls = owned_post_grants
        .into_iter()
        .filter(|grant| grant.thread_id == thread_id && grant.board_short == board_short)
        .map(|grant| {
            (
                grant.post_id,
                crate::templates::thread::OwnedPostControls {
                    expires_at: grant.expires_at,
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let boards_ver = crate::templates::live_boards_version();
    let admin_csrf = admin_scoped_csrf_token(&jar, admin_session_id.as_deref(), page_data.is_admin);
    let admin_tag = admin_csrf.as_deref().map_or_else(String::new, |token| {
        format!(
            "-a{}",
            crate::utils::crypto::sha256_hex(token.as_bytes())
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    let post_tag = if can_post { "-p1" } else { "-p0" };
    let greentext_tag = if page_data.board.collapse_greentext {
        "-cg1"
    } else {
        "-cg0"
    };
    let theme_tag = crate::templates::page_theme_etag_fragment(
        current_theme.as_deref(),
        Some(&page_data.board.default_theme),
    );
    let ownership_sig = {
        let mut owned = page_data
            .owned_post_controls
            .iter()
            .map(|(post_id, controls)| format!("{post_id}:{}", controls.expires_at))
            .collect::<Vec<_>>();
        owned.sort_unstable();
        crate::utils::crypto::sha256_hex(owned.join("|").as_bytes())
    };
    let etag = format!(
        "\"{thread_sig}-b{boards_ver}{admin_tag}{post_tag}{greentext_tag}-t{theme_tag}-o{ownership_sig}-{}\"",
        user_preferences.etag_fragment()
    );
    let (latest_created_at, latest_thread_id) =
        crate::handlers::board::latest_visible_thread_marker_tuple(latest_thread_marker);
    let jar = if thread_badges_enabled || homepage_reply_badges_enabled {
        crate::handlers::board::remember_thread_activity(
            jar,
            page_data.thread.id,
            page_data.thread.reply_count,
        )
    } else {
        jar
    };
    let jar = if homepage_thread_badges_enabled {
        crate::handlers::board::remember_board_activity(
            jar,
            page_data.board.id,
            latest_created_at,
            latest_thread_id,
        )
    } else {
        jar
    };

    // Reuse the client's current cached representation.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let activity_markers_enabled =
        thread_badges_enabled || homepage_thread_badges_enabled || homepage_reply_badges_enabled;
    if client_etag == etag && !activity_markers_enabled {
        // StatusCode::NOT_MODIFIED and Body::empty() are always valid; this
        // builder call is infallible in practice.
        let mut resp = axum::http::Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        drop(resp.headers_mut().insert(
            "etag",
            HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static("\"0\"")),
        ));
        drop(resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(crate::handlers::board::personal_html_cache_control(
                activity_markers_enabled,
            )),
        ));
        crate::cache::insert_vary_cookie(resp.headers_mut());
        return Ok((jar, resp).into_response());
    }

    let success_message = match params.reported.as_deref() {
        Some("1") => Some("Report submitted. Thank you."),
        Some("duplicate") => Some("A matching report is already open. Thank you."),
        _ => None,
    };
    // HTML assembly is CPU work on already-owned data; keep it off the async workers.
    let html = tokio::task::spawn_blocking(move || {
        render::render_thread_page(
            &page_data,
            &csrf,
            admin_csrf.as_deref(),
            None,
            success_message,
            None,
            None,
            current_theme.as_deref(),
            can_post,
            user_preferences,
        )
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let mut resp = Html(html).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        drop(resp.headers_mut().insert("etag", v));
    }
    drop(resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::handlers::board::personal_html_cache_control(
            activity_markers_enabled,
        )),
    ));
    crate::cache::insert_vary_cookie(resp.headers_mut());
    Ok((jar, resp).into_response())
}

// POST /:board/thread/:id — post reply
#[expect(
    clippy::too_many_lines,
    reason = "access checks, multipart validation, transactional reply creation, and cookies form one request"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the media permit intentionally moves from the parsed form into non-cancellable blocking submission work"
)]
pub(in crate::server) async fn post_reply(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    secure_context: crate::middleware::SecureCookieContext,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let xhr_request = is_xml_http_request(&req_headers);
    let user_preferences = crate::handlers::board::user_preferences_from_jar(&jar);
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let access_decision = crate::handlers::board::board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie.clone(),
        crate::handlers::board::BoardAccessRequirement::Post,
        format!("/{board_short}/thread/{thread_id}"),
    )
    .await?;

    let access_context = match access_decision {
        crate::handlers::board::BoardAccessDecision::Allowed(context) => context,
        crate::handlers::board::BoardAccessDecision::Denied(denial) => {
            let redirect_to =
                crate::handlers::board::unlock_redirect_url(&board_short, &denial.return_to);
            return Ok(Redirect::to(&redirect_to).into_response());
        }
    };

    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_owned());
    let mut parse_draft = crate::templates::forms::PostFormState::default();
    let parsed_form = tokio::time::timeout(
        crate::handlers::PUBLIC_UPLOAD_TIMEOUT,
        parse_post_multipart(
            multipart,
            csrf_cookie.as_deref(),
            access_context.board.max_image_size_bytes(),
            access_context.board.max_video_size_bytes(),
            access_context.board.max_audio_size_bytes(),
            access_context.board.max_pdf_size_bytes(),
            crate::handlers::PostMultipartContext {
                media_upload_gate: &state.media_upload_gate,
                draft: &mut parse_draft,
            },
        ),
    )
    .await
    .map_err(|_error| AppError::BadRequest("Upload timed out. Please try again.".into()))
    .and_then(std::convert::identity);
    let form = match parsed_form {
        Ok(form) => form,
        Err(error) => {
            if xhr_request {
                return crate::handlers::board::xhr_post_error_response(error);
            }
            return Ok(crate::handlers::native_post_error_response(
                error,
                Some(&parse_draft),
            ));
        }
    };

    // Replies do not expose thread subject or poll controls.
    parse_draft.subject.clear();
    parse_draft.poll = None;
    let post_form_state = parse_draft;

    if !form.csrf_verified {
        let error = AppError::Forbidden("CSRF token mismatch. Reload the posting page for a fresh form after copying your draft.".into());
        if xhr_request {
            return crate::handlers::board::xhr_post_error_response(error);
        }
        return Ok(crate::handlers::native_post_error_response(
            error,
            Some(&post_form_state),
        ));
    }

    // Extract csrf_token before spawn_blocking so the ban page appeal form works.
    let ban_csrf_token = csrf_cookie.clone().unwrap_or_default();

    // Clones kept outside the closure so we can re-render the thread page inline on error.
    let board_short_err = board_short.clone();
    let admin_session_err = admin_session_id.clone();
    let csrf_for_error = csrf_cookie.clone().unwrap_or_default();

    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let identity_key_err = identity_key.clone();
    let PostFormData {
        media_upload_guard,
        csrf_verified: _,
        submission_token,
        name,
        subject: _,
        body,
        deletion_token,
        file,
        audio_file,
        image_file,
        poll_question: _,
        poll_options: _,
        poll_duration_secs: _,
        sage,
        captcha_id,
        captcha_answer,
    } = form;
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = std::sync::Arc::clone(&state.job_queue);
        let ffmpeg_available = state.ffmpeg_available;
        move || -> Result<posting::SubmitPostResult> {
            // `spawn_blocking` work is not cancelled when its join handle is
            // dropped. Keep the permit inside this closure so a disconnected
            // request cannot release the media gate while parsing continues.
            let _media_upload_guard = media_upload_guard;
            posting::submit_post(
                &pool,
                &job_queue,
                posting::SubmitPostCommand {
                    mode: posting::SubmitPostMode::Reply { thread_id, sage },
                    board_short,
                    identity_key,
                    cookie_secret: CONFIG.cookie_secret.clone(),
                    admin_session_id,
                    access_cookie,
                    ban_csrf_token,
                    submission_token,
                    name,
                    body,
                    deletion_token,
                    captcha_id,
                    captcha_answer,
                    image_file_data: image_file,
                    file_data: file,
                    audio_file_data: audio_file,
                    upload_dir: CONFIG.upload_dir.clone(),
                    thumb_size: CONFIG.thumb_size,
                    ffmpeg_available,
                },
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // Expected post validation errors re-render the thread page with an inline
    // error banner so the user sees the message in context without being
    // redirected to a separate error page and losing their scroll position.
    let submit_result = match result {
        Ok(submit_result) => submit_result,
        Err(error) => {
            let (status, msg) = match crate::handlers::board::handled_post_error_status(error) {
                Ok(handled) => handled,
                Err(error) => {
                    if xhr_request {
                        return crate::handlers::board::xhr_post_error_response(error);
                    }
                    return Ok(crate::handlers::native_post_error_response(
                        error,
                        Some(&post_form_state),
                    ));
                }
            };
            if xhr_request {
                return crate::handlers::board::xhr_error_response(status, &msg);
            }
            let db_pool = state.db.clone();
            let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
            let html = tokio::task::spawn_blocking(move || -> Result<String> {
                let conn = db_pool.get()?;
                // Error re-render only: resolve the board for this request.
                let board = db::get_board_by_short(&conn, &board_short_err)?.ok_or_else(|| {
                    AppError::NotFound(format!("Board /{board_short_err}/ not found"))
                })?;
                let is_admin = posting::is_admin_session(&conn, admin_session_err.as_deref());
                let page_data = render::load_thread_page_data(
                    &conn,
                    board,
                    thread_id,
                    &identity_key_err,
                    &CONFIG.cookie_secret,
                    is_admin,
                )?;
                let admin_csrf_for_error = if page_data.is_admin {
                    admin_session_err.as_deref().map(|session_id| {
                        crate::utils::crypto::make_scoped_csrf_form_token(
                            &csrf_for_error,
                            &CONFIG.cookie_secret,
                            session_id,
                        )
                    })
                } else {
                    None
                };
                Ok(render::render_thread_page(
                    &page_data,
                    &csrf_for_error,
                    admin_csrf_for_error.as_deref(),
                    Some(&msg),
                    None,
                    Some(&post_form_state),
                    None,
                    current_theme.as_deref(),
                    true,
                    user_preferences,
                ))
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

            let mut resp = Html(html).into_response();
            *resp.status_mut() = status;
            return Ok(resp);
        }
    };

    let jar = crate::handlers::board::remember_owned_post_until_with_secure(
        jar,
        &submit_result.board_short,
        submit_result.thread_id,
        submit_result.post_id,
        &submit_result.deletion_token,
        crate::handlers::board::self_action_expiry(submit_result.created_at)?,
        crate::handlers::board::should_set_public_secure_cookie(&req_headers, secure_context),
    );

    if xhr_request {
        return Ok((
            jar,
            crate::handlers::board::xhr_redirect_response(&submit_result.redirect_url)?,
        )
            .into_response());
    }

    Ok((jar, Redirect::to(&submit_result.redirect_url)).into_response())
}

#[derive(Deserialize, Default)]
pub(in crate::server) struct ThreadPageQuery {
    pub reported: Option<String>,
    pub before: Option<i64>,
    pub post: Option<i64>,
}

struct SelfActionPostContext {
    board: crate::models::Board,
    thread: crate::models::Thread,
    post: crate::models::Post,
    can_post: bool,
    thread_allows_self_actions: bool,
}

async fn load_self_action_post_context(
    state: &AppState,
    board_short: &str,
    post_id: i64,
    admin_session_id: Option<String>,
    access_cookie: Option<String>,
) -> Result<SelfActionPostContext> {
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.to_owned();
        move || -> Result<SelfActionPostContext> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.board_id != board.id {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }
            let thread = db::get_thread(&conn, post.thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }
            let access_context = crate::handlers::board::load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            Ok(SelfActionPostContext {
                board,
                thread,
                post,
                can_post: access_context.can_post,
                thread_allows_self_actions: db::posts::post_thread_allows_self_actions(
                    &conn, post_id,
                )?,
            })
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
}

struct EditPostErrorPageRequest<'a> {
    board_short: &'a str,
    post_id: i64,
    jar: &'a CookieJar,
    admin_session_id: Option<String>,
    body: &'a str,
    message: &'a str,
    csrf_cookie_secure: bool,
}

async fn render_edit_post_error_page(
    state: &AppState,
    request: EditPostErrorPageRequest<'_>,
) -> Result<Response> {
    let (jar, csrf_token) =
        ensure_csrf_with_secure(request.jar.clone(), request.csrf_cookie_secure);
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let access_cookie =
        crate::handlers::board::board_access_cookie_from_jar(&jar, request.board_short);
    let context = load_self_action_post_context(
        state,
        request.board_short,
        request.post_id,
        request.admin_session_id,
        access_cookie,
    )
    .await?;
    let mut post = context.post.clone();
    request.body.clone_into(&mut post.body);
    let boards = crate::templates::live_boards();
    let html = crate::templates::thread::edit_post_page(
        &context.board,
        &context.thread,
        &post,
        &csrf_token,
        boards.as_ref(),
        current_theme.as_deref(),
        Some(request.message),
    );

    let mut response = Html(html).into_response();
    *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    drop(response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    ));
    Ok((jar, response).into_response())
}

pub(in crate::server) async fn edit_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::middleware::SecureCookieContext,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let context = load_self_action_post_context(
        &state,
        &board_short,
        post_id,
        admin_session_id.clone(),
        access_cookie.clone(),
    )
    .await?;

    if !context.can_post {
        let html = crate::handlers::board::render_board_unlock_html(
            &context.board,
            &csrf,
            &format!("/{board_short}/thread/{}", context.thread.id),
            None,
            current_theme.as_deref(),
        );
        return Ok(crate::handlers::board::board_access_required_response(
            jar, html,
        ));
    }

    if !context.board.allow_editing {
        return Err(AppError::Forbidden(
            "Users cannot edit their own posts on this board.".into(),
        ));
    }
    if !context.thread_allows_self_actions {
        return Err(AppError::Forbidden(
            "Self-actions are not available after a thread is locked or archived.".into(),
        ));
    }

    let now = chrono::Utc::now().timestamp();
    if now > crate::handlers::board::self_action_expiry(context.post.created_at)? {
        return Err(AppError::Forbidden(
            "The 60-second edit window for this post has closed.".into(),
        ));
    }

    let owned_grant =
        crate::handlers::board::owned_post_grant_from_jar(&jar, &board_short, post_id).ok_or_else(
            || {
                AppError::Forbidden(
                    "Edit permission for this post is no longer available in this browser.".into(),
                )
            },
        )?;
    if owned_grant.expires_at <= now {
        return Err(AppError::Forbidden(
            "Edit permission for this post is no longer available in this browser.".into(),
        ));
    }

    let boards = crate::templates::live_boards();
    let html = crate::templates::thread::edit_post_page(
        &context.board,
        &context.thread,
        &context.post,
        &csrf,
        boards.as_ref(),
        current_theme.as_deref(),
        None,
    );
    let mut response = Html(html).into_response();
    drop(response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    ));
    Ok((jar, response).into_response())
}

// POST /:board/post/:id/edit — submit edit
#[derive(Deserialize)]
pub(in crate::server) struct EditForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub body: String,
}

#[derive(Deserialize)]
pub(in crate::server) struct DeletePostForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "ownership checks, edit validation, transactional update, media cleanup, and jobs form one request"
)]
pub(in crate::server) async fn edit_post_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::middleware::SecureCookieContext,
    Form(form): Form<EditForm>,
) -> Result<Response> {
    let xhr_request = is_xml_http_request(&req_headers);
    check_csrf_jar(&jar, form.csrf.as_deref())?;
    let owned_grant =
        crate::handlers::board::owned_post_grant_from_jar(&jar, &board_short, post_id).ok_or_else(
            || {
                AppError::Forbidden(
                    "Edit permission for this post is no longer available in this browser.".into(),
                )
            },
        )?;
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let can_post = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
        move || -> Result<bool> {
            let conn = pool.get()?;
            let access_context = crate::handlers::board::load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            Ok(access_context.can_post)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if !can_post {
        let redirect_to = crate::handlers::board::unlock_redirect_url(
            &board_short,
            &format!("/{board_short}/thread/{}", owned_grant.thread_id),
        );
        if xhr_request {
            return crate::handlers::board::xhr_redirect_response(&redirect_to);
        }
        return Ok(Redirect::to(&redirect_to).into_response());
    }

    let raw_body = form.body;
    let board_short_for_edit = board_short.clone();
    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let admin_session_id = admin_session_id.clone();
        let raw_body = raw_body.clone();
        let deletion_token = owned_grant.deletion_token.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short_for_edit)?.ok_or_else(|| {
                AppError::NotFound(format!("Board /{board_short_for_edit}/ not found"))
            })?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            if post.board_id != board.id {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }

            if !board.allow_editing {
                return Err(AppError::Forbidden(
                    "Users cannot edit their own posts on this board.".into(),
                ));
            }

            let now = chrono::Utc::now().timestamp();
            if now.saturating_sub(post.created_at) > crate::handlers::board::self_action_window_secs()
            {
                return Err(AppError::Forbidden(
                    "The 60-second edit window for this post has closed.".into(),
                ));
            }

            let body_text = crate::utils::sanitize::validate_body(&raw_body)
                .map_err(AppError::BadRequest)?.to_owned();

            let filters: Vec<(String, String)> = db::get_word_filters(&conn)?
                .into_iter()
                .map(|f| (f.pattern, f.replacement))
                .collect();

            let filtered = crate::utils::sanitize::apply_word_filters(&body_text, &filters);
            let escaped = crate::utils::sanitize::escape_html(&filtered);
            let body_html =
                crate::utils::sanitize::render_post_body(&escaped, board.collapse_greentext);

            let success = db::posts::edit_post_with_validation(
                &conn, post_id, &deletion_token, &body_text, &body_html,
                crate::handlers::board::self_action_window_secs(),
                |validation_conn| {
                    let current = crate::handlers::board::load_board_access_context(
                        validation_conn, &board_short_for_edit, admin_session_id.as_deref(), access_cookie.as_deref(),
                    )?;
                    if !current.can_post || !current.board.allow_editing || current.board.id != post.board_id {
                        return Err(AppError::Forbidden("Board editing permission changed. Reload the page.".into()));
                    }
                    let current_filters: Vec<_> = db::get_word_filters(validation_conn)?.into_iter()
                        .map(|filter| (filter.pattern, filter.replacement)).collect();
                    if current_filters != filters || current.board.collapse_greentext != board.collapse_greentext {
                        return Err(AppError::Conflict("Board posting settings changed. Reload the page.".into()));
                    }
                    Ok(())
                },
            ).map_err(posting::post_creation_error)?;

            if !success {
                return Err(AppError::Forbidden(
                    "Edit permission for this post is no longer available in this browser.".into(),
                ));
            }

            tracing::info!(target: "board", post_id = post_id, board = %board.short_name, "Post edited");
            Ok(format!("/{}/thread/{}#p{post_id}", board.short_name, post.thread_id))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match outcome {
        Ok(url) => {
            if xhr_request {
                crate::handlers::board::xhr_redirect_response(&url)
            } else {
                Ok(Redirect::to(&url).into_response())
            }
        }
        Err(AppError::BadRequest(message)) => {
            if xhr_request {
                crate::handlers::board::xhr_handled_error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &message,
                )
            } else {
                render_edit_post_error_page(
                    &state,
                    EditPostErrorPageRequest {
                        board_short: &board_short,
                        post_id,
                        jar: &jar,
                        admin_session_id,
                        body: &raw_body,
                        message: &message,
                        csrf_cookie_secure: crate::handlers::board::should_set_public_secure_cookie(
                            &req_headers,
                            peer,
                        ),
                    },
                )
                .await
            }
        }
        Err(AppError::Forbidden(message)) => {
            if xhr_request {
                crate::handlers::board::xhr_handled_error_response(StatusCode::FORBIDDEN, &message)
            } else {
                Err(AppError::Forbidden(message))
            }
        }
        Err(AppError::NotFound(message)) => {
            if xhr_request {
                crate::handlers::board::xhr_handled_error_response(StatusCode::NOT_FOUND, &message)
            } else {
                Err(AppError::NotFound(message))
            }
        }
        Err(error) => Err(error),
    }
}

pub(in crate::server) async fn delete_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::middleware::SecureCookieContext,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let context = load_self_action_post_context(
        &state,
        &board_short,
        post_id,
        admin_session_id.clone(),
        access_cookie.clone(),
    )
    .await?;

    if !context.can_post {
        let html = crate::handlers::board::render_board_unlock_html(
            &context.board,
            &csrf,
            &format!("/{board_short}/thread/{}", context.thread.id),
            None,
            current_theme.as_deref(),
        );
        return Ok(crate::handlers::board::board_access_required_response(
            jar, html,
        ));
    }

    if !context.board.allow_self_delete {
        return Err(AppError::Forbidden(
            "Users cannot delete their own posts on this board.".into(),
        ));
    }
    if !context.thread_allows_self_actions {
        return Err(AppError::Forbidden(
            "Self-actions are not available after a thread is locked or archived.".into(),
        ));
    }

    let now = chrono::Utc::now().timestamp();
    if now > crate::handlers::board::self_action_expiry(context.post.created_at)? {
        return Err(AppError::Forbidden(
            "The 60-second self-delete window for this post has closed.".into(),
        ));
    }

    let owned_grant =
        crate::handlers::board::owned_post_grant_from_jar(&jar, &board_short, post_id).ok_or_else(
            || {
                AppError::Forbidden(
                    "Delete permission for this post is no longer available in this browser."
                        .into(),
                )
            },
        )?;
    if owned_grant.expires_at <= now {
        return Err(AppError::Forbidden(
            "Delete permission for this post is no longer available in this browser.".into(),
        ));
    }

    let boards = crate::templates::live_boards();
    let html = crate::templates::thread::delete_post_page(
        &context.board,
        &context.thread,
        &context.post,
        &csrf,
        boards.as_ref(),
        current_theme.as_deref(),
        None,
    );
    let mut response = Html(html).into_response();
    drop(response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    ));
    Ok((jar, response).into_response())
}

#[expect(
    clippy::too_many_lines,
    reason = "ownership checks, deletion rules, transactional mutation, and media cleanup form one request"
)]
pub(in crate::server) async fn delete_own_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<DeletePostForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;
    let owned_grant =
        crate::handlers::board::owned_post_grant_from_jar(&jar, &board_short, post_id).ok_or_else(
            || {
                AppError::Forbidden(
                    "Delete permission for this post is no longer available in this browser."
                        .into(),
                )
            },
        )?;
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let can_post = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
        move || -> Result<bool> {
            let conn = pool.get()?;
            let access_context = crate::handlers::board::load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            Ok(access_context.can_post)
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    if !can_post {
        let redirect_to = crate::handlers::board::unlock_redirect_url(
            &board_short,
            &format!("/{board_short}/thread/{}", owned_grant.thread_id),
        );
        return Ok(Redirect::to(&redirect_to).into_response());
    }

    let deletion_token = owned_grant.deletion_token;
    let board_short_for_delete = board_short.clone();
    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(i64, db::posts::SelfDeleteOutcome)> {
            let conn = pool.get()?;
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            let board =
                db::get_board_by_short(&conn, &board_short_for_delete)?.ok_or_else(|| {
                    AppError::NotFound(format!("Board /{board_short_for_delete}/ not found"))
                })?;
            if post.board_id != board.id {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }
            if !board.allow_self_delete {
                return Err(AppError::Forbidden(
                    "Users cannot delete their own posts on this board.".into(),
                ));
            }

            let redirect_thread_id = post.thread_id;
            let (result, deleted) = db::posts::self_delete_post_with_validation(
                &conn,
                post_id,
                &deletion_token,
                crate::handlers::board::self_action_window_secs(),
                |validation_conn| {
                    let current = crate::handlers::board::load_board_access_context(
                        validation_conn,
                        &board_short_for_delete,
                        admin_session_id.as_deref(),
                        access_cookie.as_deref(),
                    )?;
                    if !current.can_post
                        || !current.board.allow_self_delete
                        || current.board.id != post.board_id
                    {
                        return Err(AppError::Forbidden(
                            "Board deletion permission changed. Reload the page.".into(),
                        ));
                    }
                    Ok(())
                },
            )?;

            if let Some(deleted) = deleted.as_ref() {
                if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                    &conn,
                    &CONFIG.upload_dir,
                    deleted.pending_fs_op_id.as_deref(),
                    &deleted.paths,
                ) {
                    tracing::error!(target: LOG_TARGET,
                        post_id,
                        error = %error,
                        "self-delete post cleanup did not fully complete"
                    );
                }
            }

            Ok((redirect_thread_id, result))
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    let (thread_id, result) = outcome;
    let cookie_secure =
        crate::handlers::board::should_set_public_secure_cookie(&req_headers, secure_context);
    match result {
        db::posts::SelfDeleteOutcome::DeletedReply => {
            let jar = crate::handlers::board::forget_owned_post_with_secure(
                jar,
                &board_short,
                post_id,
                cookie_secure,
            );
            let redirect_url = format!("/{board_short}/thread/{thread_id}");
            Ok((jar, Redirect::to(&redirect_url)).into_response())
        }
        db::posts::SelfDeleteOutcome::DeletedThread => {
            let jar = crate::handlers::board::forget_owned_post_with_secure(
                jar,
                &board_short,
                post_id,
                cookie_secure,
            );
            let redirect_url = format!("/{board_short}/catalog");
            Ok((jar, Redirect::to(&redirect_url)).into_response())
        }
        db::posts::SelfDeleteOutcome::NotFound => Err(AppError::NotFound("Post not found.".into())),
        db::posts::SelfDeleteOutcome::WrongToken => Err(AppError::Forbidden(
            "Delete permission for this post is no longer available in this browser.".into(),
        )),
        db::posts::SelfDeleteOutcome::WindowClosed => Err(AppError::Forbidden(
            "The 60-second self-delete window for this post has closed.".into(),
        )),
        db::posts::SelfDeleteOutcome::ThreadClosed => Err(AppError::Forbidden(
            "Self-actions are not available after a thread is locked or archived.".into(),
        )),
        db::posts::SelfDeleteOutcome::ThreadHasReplies => Err(AppError::Forbidden(
            "You can only self-delete a thread starter before anyone replies.".into(),
        )),
    }
}

// POST /vote — cast poll vote
#[derive(Deserialize)]
pub(in crate::server) struct VoteForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub option_id: i64,
}

pub(in crate::server) async fn vote_handler(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<VoteForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let cookie_secret = CONFIG.cookie_secret.clone();
    let option_id = form.option_id;

    // Reject non-positive IDs before touching the DB.
    if option_id <= 0 {
        return Err(AppError::BadRequest("Invalid poll option.".into()));
    }

    let context = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(i64, i64, String)> {
            let conn = pool.get()?;
            db::get_poll_context(&conn, option_id)?
                .ok_or_else(|| AppError::NotFound("Poll option not found.".into()))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    let (_poll_id, thread_id, board_short) = context;
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let can_post = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
        move || -> Result<bool> {
            let conn = pool.get()?;
            let access_context = crate::handlers::board::load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            Ok(access_context.can_post)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if !can_post {
        let redirect_to = crate::handlers::board::unlock_redirect_url(
            &board_short,
            &format!("/{board_short}/thread/{thread_id}#poll"),
        );
        return Ok(Redirect::to(&redirect_to).into_response());
    }

    let identity_key = crate::handlers::board::identity_key(&client_ip, &jar);
    let ban_csrf_token = jar
        .get("csrf_token")
        .map(|cookie| cookie.value().to_owned())
        .unwrap_or_default();
    let redirect_url = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let ip_hash = hash_ip(&identity_key, &cookie_secret);
            // Voting is persisted participation in the thread, so the same ban
            // gate as posting applies; CSRF was validated above.
            crate::handlers::board::ensure_actor_not_banned(&conn, &ip_hash, ban_csrf_token)?;
            let (poll_id, resolved_thread_id, resolved_board_short) =
                db::get_poll_context(&conn, option_id)?
                    .ok_or_else(|| AppError::NotFound("Poll option not found.".into()))?;

            let recorded = db::cast_vote(&conn, poll_id, option_id, &ip_hash)?;
            if !recorded {
                return Err(AppError::BadRequest(
                    "This poll is closed, archived, or you have already voted.".into(),
                ));
            }
            tracing::info!(
                target: "board",
                poll_id = poll_id,
                option_id = option_id,
                "Vote cast"
            );
            Ok(format!(
                "/{resolved_board_short}/thread/{resolved_thread_id}#poll"
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_url).into_response())
}

// GET /:board/thread/:id/updates?since=N
// Delta-compressed polling endpoint for the thread auto-update toggle.
//
// Returns a JSON envelope with:
//   html         — rendered HTML for any new posts since `since`
//   last_id      — highest post ID seen (client bumps its cursor)
//   count        — number of new posts in this response
//   refreshed_posts — re-rendered HTML for already-loaded posts still being
//                     watched for async media completion
//   reply_count  — current total reply count for the thread
//   bump_time    — current bumped_at UNIX timestamp
//   locked       — current lock state
//   sticky       — current sticky state
//
// The extra state fields let the client keep the nav-bar thread stats in
// sync without a full page reload.  Posts are rendered without delete/admin
// controls so the response is auth-state-independent (safe to cache).

#[derive(Deserialize)]
pub(in crate::server) struct UpdatesQuery {
    since: i64,
    refresh: Option<String>,
}

#[derive(serde::Serialize)]
struct RefreshedPostPayload {
    id: i64,
    html: String,
}

#[derive(serde::Serialize)]
struct ThreadUpdatesPayload {
    html: String,
    last_id: i64,
    count: usize,
    refreshed_posts: Vec<RefreshedPostPayload>,
    reply_count: i64,
    bump_time: i64,
    locked: bool,
    sticky: bool,
    archived: bool,
    boards_version: u64,
    nav_html: String,
    mobile_nav_html: String,
}

struct ActivityBadgeSettings {
    thread_badges_enabled: bool,
    homepage_thread_badges_enabled: bool,
    homepage_reply_badges_enabled: bool,
}

struct ThreadUpdatesRender {
    html: String,
    last_id: i64,
    count: usize,
    refreshed_posts: Vec<RefreshedPostPayload>,
    reply_count: i64,
    bump_time: i64,
    locked: bool,
    sticky: bool,
    archived: bool,
    board_id: i64,
    activity_badges: ActivityBadgeSettings,
    latest_thread_marker: Option<(i64, i64)>,
}

fn parse_refresh_post_ids(raw: Option<&str>) -> Vec<i64> {
    let mut ids = raw
        .unwrap_or("")
        .split(',')
        .filter_map(|value| value.trim().parse::<i64>().ok())
        .filter(|id| *id > 0)
        .take(64)
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[expect(
    clippy::too_many_lines,
    reason = "access checks, update filtering, unread state, and response rendering form one polling request"
)]
pub(in crate::server) async fn thread_updates(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    Query(params): Query<UpdatesQuery>,
    jar: CookieJar,
) -> Result<Response> {
    let since = params.since;
    let user_preferences = crate::handlers::board::user_preferences_from_jar(&jar);
    let refresh_post_ids = parse_refresh_post_ids(params.refresh.as_deref());
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = crate::handlers::board::board_access_cookie_from_jar(&jar, &board_short);
    let can_view = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
        move || -> Result<bool> {
            let conn = pool.get()?;
            let access_context = crate::handlers::board::load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            Ok(access_context.can_view)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if !can_view {
        return Ok((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"forbidden"}"#.to_owned(),
        )
            .into_response());
    }

    let updates = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let refresh_post_ids = refresh_post_ids.clone();
        move || -> Result<ThreadUpdatesRender> {
            let conn = pool.get()?;

            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound("Board not found.".into()))?;
            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found in this board.".into()));
            }
            let thread_badges_enabled = db::get_thread_new_reply_badges_enabled(&conn);
            let homepage_thread_badges_enabled = db::get_homepage_new_thread_badges_enabled(&conn);
            let homepage_reply_badges_enabled = db::get_homepage_new_reply_badges_enabled(&conn);
            let activity_badges = ActivityBadgeSettings {
                thread_badges_enabled,
                homepage_thread_badges_enabled,
                homepage_reply_badges_enabled,
            };
            let latest_thread_marker = if homepage_thread_badges_enabled {
                db::get_latest_visible_thread_marker(&conn, board.id)?
            } else {
                None
            };

            // Fetch posts newer than `since`, ordered oldest-first so they
            // render in the correct chronological order when appended.
            let posts = db::get_new_posts_since(&conn, board.id, thread_id, since, 100)?;
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
                        admin_csrf_token: None,
                        show_media: true,
                        allow_editing: false, // no edit link in auto-appended HTML; reload restores it
                        allow_self_delete: false,
                        owned_post_controls: None,
                        show_poster_ids: thread.board_id == board.id && board.show_poster_ids,
                        collapse_greentext: board.collapse_greentext,
                        thread_state: None,
                        thread_op_id: thread.op_id,
                        video_audio_muted: user_preferences.video_audio_muted,
                    },
                    0,
                ));
            }

            let refreshed_posts =
                db::get_posts_by_ids_in_thread(&conn, board.id, thread_id, &refresh_post_ids)?
                    .into_iter()
                    .map(|post| RefreshedPostPayload {
                        id: post.id,
                        html: crate::templates::render_post(
                            &post,
                            &board_short,
                            "",
                            crate::templates::thread::RenderPostOpts {
                                show_delete: false,
                                is_admin: false,
                                admin_csrf_token: None,
                                show_media: true,
                                allow_editing: false,
                                allow_self_delete: false,
                                owned_post_controls: None,
                                show_poster_ids: thread.board_id == board.id
                                    && board.show_poster_ids,
                                collapse_greentext: board.collapse_greentext,
                                thread_state: Some((thread.sticky, thread.locked, thread.archived)),
                                thread_op_id: thread.op_id,
                                video_audio_muted: user_preferences.video_audio_muted,
                            },
                            0,
                        ),
                    })
                    .collect::<Vec<_>>();

            Ok(ThreadUpdatesRender {
                html,
                last_id,
                count,
                refreshed_posts,
                reply_count: thread.reply_count,
                bump_time: thread.bumped_at,
                locked: thread.locked,
                sticky: thread.sticky,
                archived: thread.archived,
                board_id: board.id,
                activity_badges,
                latest_thread_marker,
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    let jar = if updates.activity_badges.thread_badges_enabled
        || updates.activity_badges.homepage_reply_badges_enabled
    {
        crate::handlers::board::remember_thread_activity(jar, thread_id, updates.reply_count)
    } else {
        jar
    };
    let (latest_created_at, latest_thread_id) =
        crate::handlers::board::latest_visible_thread_marker_tuple(updates.latest_thread_marker);
    let jar = if updates.activity_badges.homepage_thread_badges_enabled {
        crate::handlers::board::remember_board_activity(
            jar,
            updates.board_id,
            latest_created_at,
            latest_thread_id,
        )
    } else {
        jar
    };

    // Current board-list version + rendered nav links — lets the JS refresh
    // the nav bar when boards are added or deleted while a thread is open,
    // without requiring a full page reload.
    let boards_version = crate::templates::live_boards_version();
    let boards = crate::templates::live_boards_snapshot();
    let nav_html =
        crate::templates::board_nav_html_for_preferences(boards.as_ref(), user_preferences);
    let mobile_nav_html =
        crate::templates::mobile_board_nav_html_for_preferences(boards.as_ref(), user_preferences);
    let payload = ThreadUpdatesPayload {
        html: updates.html,
        last_id: updates.last_id,
        count: updates.count,
        refreshed_posts: updates.refreshed_posts,
        reply_count: updates.reply_count,
        bump_time: updates.bump_time,
        locked: updates.locked,
        sticky: updates.sticky,
        archived: updates.archived,
        boards_version,
        nav_html,
        mobile_nav_html,
    };

    let mut response = (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&payload)
            .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
    )
        .into_response();
    crate::cache::insert_vary_cookie(response.headers_mut());
    Ok((jar, response).into_response())
}
