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
    handlers::{
        board::{
            admin_scoped_csrf_token, check_csrf_jar, ensure_csrf_for_request,
            ensure_csrf_with_secure,
        },
        parse_post_multipart, posting, render, PostFormData,
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

/// Composite value returned by thread view load result.
type ThreadViewLoadResult = (
    String,
    render::ThreadPageData,
    bool,
    bool,
    bool,
    bool,
    Option<(i64, i64)>,
);

/// Returns whether xml HTTP request.
fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

// ─── GET /:board/thread/:id ───────────────────────────────────────────────────

#[expect(
    clippy::too_many_lines,
    reason = "access control, thread loading, unread-state calculation, and rendering form one request"
)]
/// Handles the view thread request.
pub(crate) async fn view_thread(
    State(state): State<AppState>,
    Path((board_short, thread_id)): Path<(String, i64)>,
    Query(params): Query<ThreadPageQuery>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::handlers::board::OptionalConnectInfoPeer,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let user_preferences = crate::handlers::board::user_preferences_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(
        jar,
        &req_headers,
        crate::handlers::board::optional_connect_info_peer(peer),
    );
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

    let page_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        move || -> Result<ThreadViewLoadResult> {
            let conn = pool.get()?;
            let page_data = render::load_thread_page_data(
                &conn,
                &board_short,
                thread_id,
                &identity_key,
                admin_session_id.as_deref(),
                &CONFIG.cookie_secret,
            )?;
            let is_admin = page_data.is_admin;
            let thread_badges_enabled = db::get_thread_new_reply_badges_enabled(&conn);
            let homepage_thread_badges_enabled = db::get_homepage_new_thread_badges_enabled(&conn);
            let homepage_reply_badges_enabled = db::get_homepage_new_reply_badges_enabled(&conn);
            let board_id = page_data.board.id;
            Ok((
                render::thread_page_etag_signature(&page_data),
                page_data,
                is_admin,
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

    let can_post = access_context.can_post;
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

    // 3.2: Return 304 Not Modified when client's cached copy is still current.
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
        resp.headers_mut().insert(
            "etag",
            HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static("\"0\"")),
        );
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(crate::handlers::board::activity_html_cache_control(
                activity_markers_enabled,
            )),
        );
        crate::cache::insert_vary_cookie(resp.headers_mut());
        return Ok((jar, resp).into_response());
    }

    let success_message = params
        .reported
        .as_deref()
        .filter(|value| *value == "1")
        .map(|_| "Report submitted. Thank you.");
    let html = render::render_thread_page(
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
    );
    let mut resp = Html(html).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::handlers::board::activity_html_cache_control(
            activity_markers_enabled,
        )),
    );
    crate::cache::insert_vary_cookie(resp.headers_mut());
    Ok((jar, resp).into_response())
}

// ─── POST /:board/thread/:id — post reply ────────────────────────────────────

#[expect(
    clippy::too_many_lines,
    reason = "access checks, multipart validation, transactional reply creation, and cookies form one request"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the media permit intentionally moves from the parsed form into non-cancellable blocking submission work"
)]
/// Handles the post reply request.
pub(crate) async fn post_reply(
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
        access_cookie,
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
    let form = tokio::time::timeout(
        crate::handlers::PUBLIC_UPLOAD_TIMEOUT,
        parse_post_multipart(
            multipart,
            csrf_cookie.as_deref(),
            access_context.board.max_image_size_bytes(),
            access_context.board.max_video_size_bytes(),
            access_context.board.max_audio_size_bytes(),
            access_context.board.max_pdf_size_bytes(),
            &state.media_upload_gate,
        ),
    )
    .await
    .map_err(|_error| AppError::BadRequest("Upload timed out. Please try again.".into()))??;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let post_form_state = crate::templates::forms::PostFormState {
        name: form.name.clone(),
        subject: String::new(),
        body: form.body.clone(),
        sage: form.sage,
    };
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
        let ffprobe_available = state.ffprobe_available;
        let ffmpeg_webp_available = state.ffmpeg_webp_available;
        move || -> Result<posting::SubmitPostResult> {
            // `spawn_blocking` work is not cancelled when its join handle is
            // dropped. Keep the permit inside this closure so a disconnected
            // request cannot release the media gate while parsing continues.
            let _media_upload_guard = media_upload_guard;
            let conn = pool.get()?;
            posting::submit_post(
                &conn,
                &job_queue,
                posting::SubmitPostCommand {
                    mode: posting::SubmitPostMode::Reply { thread_id, sage },
                    board_short,
                    identity_key,
                    cookie_secret: CONFIG.cookie_secret.clone(),
                    admin_session_id,
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
                    ffprobe_available,
                    ffmpeg_webp_available,
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
                    return Err(error);
                }
            };
            if xhr_request {
                return crate::handlers::board::xhr_error_response(status, &msg);
            }
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
        submit_result.created_at + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
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
/// Query parameters accepted by the thread page request.
pub(crate) struct ThreadPageQuery {
    /// The optional reported.
    pub reported: Option<String>,
}

/// Template data for self action post context.
struct SelfActionPostContext {
    /// The board.
    board: crate::models::Board,
    /// The thread.
    thread: crate::models::Thread,
    /// The post.
    post: crate::models::Post,
    /// Whether the requester can post.
    can_post: bool,
    /// Whether the thread allows self actions setting is active.
    thread_allows_self_actions: bool,
}

/// Handles the load self action post context request.
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

/// Data used by the edit post error page request workflow.
struct EditPostErrorPageRequest<'a> {
    /// The board short.
    board_short: &'a str,
    /// The post identifier.
    post_id: i64,
    /// The jar.
    jar: &'a CookieJar,
    /// The admin session identifier.
    admin_session_id: Option<String>,
    /// The body.
    body: &'a str,
    /// The message.
    message: &'a str,
    /// Whether the CSRF cookie secure setting is active.
    csrf_cookie_secure: bool,
}

/// Handles the render edit post error page request.
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
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    );
    Ok((jar, response).into_response())
}

/// Handles the edit post get request.
pub(crate) async fn edit_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::handlers::board::OptionalConnectInfoPeer,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(
        jar,
        &req_headers,
        crate::handlers::board::optional_connect_info_peer(peer),
    );
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
    if now
        > context
            .post
            .created_at
            .saturating_add(crate::handlers::board::SELF_DELETE_WINDOW_SECS)
    {
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
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    );
    Ok((jar, response).into_response())
}

// ─── POST /:board/post/:id/edit — submit edit ─────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the edit request.
pub(crate) struct EditForm {
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    pub csrf: Option<String>,
    /// The body.
    pub body: String,
}

#[derive(Deserialize)]
/// Form fields accepted by the delete post request.
pub(crate) struct DeletePostForm {
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    pub csrf: Option<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "ownership checks, edit validation, transactional update, media cleanup, and jobs form one request"
)]
/// Handles the edit post post request.
pub(crate) async fn edit_post_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::handlers::board::OptionalConnectInfoPeer,
    Form(form): Form<EditForm>,
) -> Result<Response> {
    let xhr_request = is_xml_http_request(&req_headers);
    let peer = crate::handlers::board::optional_connect_info_peer(peer);
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
            if now.saturating_sub(post.created_at) > crate::handlers::board::SELF_DELETE_WINDOW_SECS
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

            let success = db::edit_post(
                &conn,
                post_id,
                &deletion_token,
                &body_text,
                &body_html,
                crate::handlers::board::SELF_DELETE_WINDOW_SECS,
            )?;

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

/// Handles the delete post get request.
pub(crate) async fn delete_post_get(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: crate::handlers::board::OptionalConnectInfoPeer,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(
        jar,
        &req_headers,
        crate::handlers::board::optional_connect_info_peer(peer),
    );
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
    if now
        > context
            .post
            .created_at
            .saturating_add(crate::handlers::board::SELF_DELETE_WINDOW_SECS)
    {
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
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    );
    Ok((jar, response).into_response())
}

#[expect(
    clippy::too_many_lines,
    reason = "ownership checks, deletion rules, transactional mutation, and media cleanup form one request"
)]
/// Handles the delete own post request.
pub(crate) async fn delete_own_post(
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
            if post.board_id
                != db::get_board_by_short(&conn, &board_short_for_delete)?
                    .ok_or_else(|| {
                        AppError::NotFound(format!("Board /{board_short_for_delete}/ not found"))
                    })?
                    .id
            {
                return Err(AppError::NotFound("Post not found in this board.".into()));
            }
            let board =
                db::get_board_by_short(&conn, &board_short_for_delete)?.ok_or_else(|| {
                    AppError::NotFound(format!("Board /{board_short_for_delete}/ not found"))
                })?;
            if !board.allow_self_delete {
                return Err(AppError::Forbidden(
                    "Users cannot delete their own posts on this board.".into(),
                ));
            }

            let redirect_thread_id = post.thread_id;
            let (result, deleted) = db::posts::self_delete_post(
                &conn,
                post_id,
                &deletion_token,
                crate::handlers::board::SELF_DELETE_WINDOW_SECS,
            )?;

            if let Some(deleted) = deleted.as_ref() {
                if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                    &conn,
                    &CONFIG.upload_dir,
                    deleted.pending_fs_op_id.as_deref(),
                    &deleted.paths,
                ) {
                    tracing::error!(
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

// ─── POST /vote — cast poll vote ──────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the vote request.
pub(crate) struct VoteForm {
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    pub csrf: Option<String>,
    /// The option identifier.
    pub option_id: i64,
}

/// Handles the vote handler request.
pub(crate) async fn vote_handler(
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
/// Query parameters accepted by the updates request.
pub(crate) struct UpdatesQuery {
    /// The since.
    since: i64,
    /// The optional refresh.
    refresh: Option<String>,
}

#[derive(serde::Serialize)]
/// Data used by the refreshed post payload workflow.
struct RefreshedPostPayload {
    /// The record identifier.
    id: i64,
    /// The HTML.
    html: String,
}

#[derive(serde::Serialize)]
/// Data used by the thread updates payload workflow.
struct ThreadUpdatesPayload {
    /// The HTML.
    html: String,
    /// The last identifier.
    last_id: i64,
    /// The count.
    count: usize,
    /// The refreshed posts collection.
    refreshed_posts: Vec<RefreshedPostPayload>,
    /// The number of replies.
    reply_count: i64,
    /// The bump time.
    bump_time: i64,
    /// Whether the locked setting is active.
    locked: bool,
    /// Whether the sticky setting is active.
    sticky: bool,
    /// The boards version.
    boards_version: u64,
    /// The nav HTML.
    nav_html: String,
}

/// Data used by the activity badge settings workflow.
struct ActivityBadgeSettings {
    /// Whether thread badges is enabled.
    thread_badges_enabled: bool,
    /// Whether homepage thread badges is enabled.
    homepage_thread_badges_enabled: bool,
    /// Whether homepage reply badges is enabled.
    homepage_reply_badges_enabled: bool,
}

/// Data used by the thread updates render workflow.
struct ThreadUpdatesRender {
    /// The HTML.
    html: String,
    /// The last identifier.
    last_id: i64,
    /// The count.
    count: usize,
    /// The refreshed posts collection.
    refreshed_posts: Vec<RefreshedPostPayload>,
    /// The number of replies.
    reply_count: i64,
    /// The bump time.
    bump_time: i64,
    /// Whether the locked setting is active.
    locked: bool,
    /// Whether the sticky setting is active.
    sticky: bool,
    /// The board identifier.
    board_id: i64,
    /// The activity badges.
    activity_badges: ActivityBadgeSettings,
    /// The optional latest thread marker.
    latest_thread_marker: Option<(i64, i64)>,
}

/// Parses refresh post IDs.
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
/// Handles the thread updates request.
pub(crate) async fn thread_updates(
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

            // Validate board + thread exist (returns 404 for bad URLs).
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
    let payload = ThreadUpdatesPayload {
        html: updates.html,
        last_id: updates.last_id,
        count: updates.count,
        refreshed_posts: updates.refreshed_posts,
        reply_count: updates.reply_count,
        bump_time: updates.bump_time,
        locked: updates.locked,
        sticky: updates.sticky,
        boards_version,
        nav_html,
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

#[cfg(test)]
mod tests {
    use anyhow::{Context as _, Result};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    macro_rules! assert {
        ($condition:expr_2021 $(,)?) => {
            anyhow::ensure!(
                $condition,
                "assertion failed: {}",
                stringify!($condition)
            );
        };
        ($condition:expr_2021, $($message:tt)+) => {
            anyhow::ensure!(
                $condition,
                "assertion failed: {}: {}",
                stringify!($condition),
                format_args!($($message)+)
            );
        };
    }

    macro_rules! assert_eq {
        ($left:expr_2021, $right:expr_2021 $(,)?) => {{
            match (&$left, &$right) {
                (left, right) => anyhow::ensure!(
                    left == right,
                    "assertion failed: `(left == right)`\n  left: `{left:?}`\n right: `{right:?}`"
                ),
            }
        }};
    }

    fn flac_fixture(size: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; size.max(4)];
        if let Some(prefix) = bytes.get_mut(..4) {
            prefix.copy_from_slice(b"fLaC");
        }
        bytes
    }

    fn seed_audio_board(state: &crate::middleware::AppState, max_audio_size: i64) -> Result<()> {
        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let board_id = crate::db::create_board(&conn, "music", "Music", "", false)
            .context("failed to create audio board")?;
        conn.execute(
            "UPDATE boards SET allow_audio = 1, max_audio_size = ?1 WHERE id = ?2",
            rusqlite::params![max_audio_size, board_id],
        )
        .context("failed to enable audio board")?;
        Ok(())
    }

    #[tokio::test]
    async fn create_thread_and_reply_accept_audio_within_board_limit() -> Result<()> {
        let state = crate::test_support::app_state();
        seed_audio_board(&state, 5_000)?;

        let router = Router::new()
            .route("/{board}", post(crate::handlers::board::create_thread))
            .route("/{board}/thread/{id}", post(super::post_reply))
            .with_state(state.clone());

        let create_audio = flac_fixture(4_500);
        let (create_boundary, create_body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "")],
            Some(("audio_file", "track.flac", &create_audio, "audio/flac")),
        );
        let create_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/music")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={create_boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(create_body))
                    .context("failed to build create-thread request")?,
            )
            .await
            .context("failed to receive create-thread response")?;
        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);

        let thread_id = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row("SELECT id FROM threads LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("failed to load thread identifier")?
        };

        let reply_audio = flac_fixture(4_500);
        let (reply_boundary, reply_body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "reply")],
            Some(("audio_file", "reply.flac", &reply_audio, "audio/flac")),
        );
        let reply_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/music/thread/{thread_id}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={reply_boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(reply_body))
                    .context("failed to build reply request")?,
            )
            .await
            .context("failed to receive reply response")?;

        assert_eq!(reply_response.status(), StatusCode::SEE_OTHER);
        Ok(())
    }

    #[tokio::test]
    async fn create_thread_rejects_audio_over_board_limit_with_413() -> Result<()> {
        let state = crate::test_support::app_state();
        seed_audio_board(&state, 5_000)?;

        let router = Router::new()
            .route("/{board}", post(crate::handlers::board::create_thread))
            .with_state(state);

        let audio = flac_fixture(5_001);
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "")],
            Some(("audio_file", "too-large.flac", &audio, "audio/flac")),
        );
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/music")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        Ok(())
    }

    #[tokio::test]
    async fn post_reply_rejects_mime_mismatch_with_415_inline_error() -> Result<()> {
        let state = crate::test_support::app_state();
        let (board_id, thread_id) = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            let board_id = crate::db::create_board(&conn, "test", "Test", "", false)
                .context("failed to create board")?;
            let post = crate::db::NewPost {
                thread_id: 0,
                board_id,
                name: "anon".to_owned(),
                tripcode: None,
                subject: Some("subject".to_owned()),
                body: "op".to_owned(),
                body_html: "op".to_owned(),
                ip_hash: None,
                file_path: None,
                file_name: None,
                file_size: None,
                thumb_path: None,
                mime_type: None,
                media_type: None,
                audio_file_path: None,
                audio_file_name: None,
                audio_file_size: None,
                audio_mime_type: None,
                deletion_token: "token".to_owned(),
                is_op: true,
            };
            let (thread_id, _, _) = crate::db::create_thread_with_optional_poll(
                &conn,
                board_id,
                Some("subject"),
                &post,
                "",
                None,
                None,
            )
            .context("failed to create thread")?;
            (board_id, thread_id)
        };
        assert!(board_id > 0);

        let router = Router::new()
            .route("/{board}/thread/{id}", post(super::post_reply))
            .with_state(state);
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "bad media reply")],
            Some(("file", "fake.png", b"plain text", "image/png")),
        );
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/thread/{thread_id}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;
        assert!(body.contains("post-error-banner"));
        assert!(body.contains("File type not allowed"));
        Ok(())
    }

    fn seed_owned_post(
        state: &crate::middleware::AppState,
        allow_editing: bool,
        allow_self_delete: bool,
        age_secs: i64,
    ) -> Result<(i64, i64, String)> {
        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let board_id = crate::db::create_board(&conn, "test", "Test", "", false)
            .context("failed to create board")?;
        conn.execute(
            "UPDATE boards SET allow_editing = ?1, allow_self_delete = ?2 WHERE id = ?3",
            rusqlite::params![
                i64::from(allow_editing),
                i64::from(allow_self_delete),
                board_id
            ],
        )
        .context("failed to update board toggles")?;
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "original body".to_owned(),
            body_html: "original body".to_owned(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "edit-token".to_owned(),
            is_op: true,
        };
        let (thread_id, post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn, board_id, None, &post, "", None, None,
        )
        .context("failed to create thread")?;
        if age_secs > 0 {
            conn.execute(
                "UPDATE posts SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![chrono::Utc::now().timestamp() - age_secs, post_id],
            )
            .context("failed to age post")?;
        }
        drop(conn);

        let jar = crate::handlers::board::remember_owned_post_until(
            axum_extra::extract::cookie::CookieJar::new(),
            "test",
            thread_id,
            post_id,
            "edit-token",
            chrono::Utc::now().timestamp() + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
        );
        let cookie = jar
            .get("rustchan_owned_posts")
            .context("owned-posts cookie was not set")?
            .value()
            .to_owned();
        Ok((thread_id, post_id, cookie))
    }

    fn set_thread_state(
        state: &crate::middleware::AppState,
        thread_id: i64,
        locked: bool,
        archived: bool,
    ) -> Result<()> {
        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        conn.execute(
            "UPDATE threads SET locked = ?1, archived = ?2 WHERE id = ?3",
            rusqlite::params![i64::from(locked), i64::from(archived), thread_id],
        )
        .context("failed to update thread state")?;
        Ok(())
    }

    #[tokio::test]
    async fn post_reply_persists_quote_markup() -> Result<()> {
        let state = crate::test_support::app_state();
        {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            crate::db::create_board(&conn, "test", "Test", "", false)
                .context("failed to create board")?;
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
                    .context("failed to build create-thread request")?,
            )
            .await
            .context("failed to receive create-thread response")?;
        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);

        let (thread_id, op_post_id) = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            let thread_id = conn
                .query_row("SELECT id FROM threads LIMIT 1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .context("failed to load thread identifier")?;
            let op_post_id = conn
                .query_row(
                    "SELECT id FROM posts WHERE thread_id = ?1 AND is_op = 1",
                    rusqlite::params![thread_id],
                    |row| row.get::<_, i64>(0),
                )
                .context("failed to load opening-post identifier")?;
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
                    .context("failed to build reply request")?,
            )
            .await
            .context("failed to receive reply response")?;
        assert_eq!(reply_response.status(), StatusCode::SEE_OTHER);

        let reply_html = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row(
                "SELECT body_html FROM posts WHERE thread_id = ?1 AND is_op = 0 ORDER BY id DESC LIMIT 1",
                rusqlite::params![thread_id],
                |row| row.get::<_, String>(0),
            )
            .context("failed to load reply body HTML")?
        };
        assert!(reply_html.contains("class=\"quotelink\""));
        assert!(reply_html.contains(&format!("data-pid=\"{op_post_id}\"")));
        Ok(())
    }

    #[tokio::test]
    async fn xhr_reply_returns_explicit_redirect_header() -> Result<()> {
        let state = crate::test_support::app_state();
        {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            crate::db::create_board(&conn, "test", "Test", "", false)
                .context("failed to create board")?;
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
                    .context("failed to build create-thread request")?,
            )
            .await
            .context("failed to receive create-thread response")?;
        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);

        let thread_id = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row("SELECT id FROM threads LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("failed to load thread identifier")?
        };

        let (reply_boundary, reply_body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "xhr reply")],
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
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(reply_body))
                    .context("failed to build reply request")?,
            )
            .await
            .context("failed to receive reply response")?;

        assert_eq!(reply_response.status(), StatusCode::NO_CONTENT);

        let redirect = reply_response
            .headers()
            .get("x-rustchan-redirect")
            .and_then(|value| value.to_str().ok())
            .context("XHR redirect header was missing or invalid")?;

        let reply_post_id = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row(
                "SELECT id FROM posts WHERE thread_id = ?1 AND is_op = 0 ORDER BY id DESC LIMIT 1",
                rusqlite::params![thread_id],
                |row| row.get::<_, i64>(0),
            )
            .context("failed to load reply-post identifier")?
        };

        assert_eq!(
            redirect,
            format!("/test/thread/{thread_id}#p{reply_post_id}")
        );
        Ok(())
    }

    #[tokio::test]
    async fn xhr_reply_validation_failure_returns_json_error() -> Result<()> {
        let state = crate::test_support::app_state();
        {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            crate::db::create_board(&conn, "test", "Test", "", false)
                .context("failed to create board")?;
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
                    .context("failed to build create-thread request")?,
            )
            .await
            .context("failed to receive create-thread response")?;
        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);

        let thread_id = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row("SELECT id FROM threads LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("failed to load thread identifier")?
        };

        let (reply_boundary, reply_body) =
            crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "")], None);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/thread/{thread_id}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={reply_boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(reply_body))
                    .context("failed to build reply request")?,
            )
            .await
            .context("failed to receive reply response")?;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::UNPROCESSABLE_ENTITY.as_str())
        );

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;
        assert!(body.contains("\"error\""));
        Ok(())
    }

    #[tokio::test]
    async fn reply_cooldown_failure_rerenders_thread_inline() -> Result<()> {
        let state = crate::test_support::app_state();
        let thread_id = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            let board_id = crate::db::create_board(&conn, "test", "Test", "", false)
                .context("failed to create board")?;
            conn.execute(
                "UPDATE boards SET post_cooldown_secs = 60 WHERE short_name = 'test'",
                [],
            )
            .context("failed to enable cooldown")?;

            let ip_hash =
                crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret);
            let post = crate::db::NewPost {
                thread_id: 0,
                board_id,
                name: "anon".to_owned(),
                tripcode: None,
                subject: Some("subject".to_owned()),
                body: "op body".to_owned(),
                body_html: "op body".to_owned(),
                ip_hash: Some(ip_hash),
                file_path: None,
                file_name: None,
                file_size: None,
                thumb_path: None,
                mime_type: None,
                media_type: None,
                audio_file_path: None,
                audio_file_name: None,
                audio_file_size: None,
                audio_mime_type: None,
                deletion_token: "token".to_owned(),
                is_op: true,
            };
            let (thread_id, _, _) = crate::db::create_thread_with_optional_poll(
                &conn,
                board_id,
                Some("subject"),
                &post,
                "",
                None,
                None,
            )
            .context("failed to create thread")?;
            thread_id
        };

        let router = Router::new()
            .route("/{board}/thread/{id}", post(super::post_reply))
            .with_state(state.clone());
        let (reply_boundary, reply_body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "reply body")],
            None,
        );

        let response = router
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
                    .context("failed to build reply request")?,
            )
            .await
            .context("failed to receive reply response")?;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            !response.headers().contains_key(header::LOCATION),
            "cooldown failures should stay inline on the thread page"
        );

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;
        assert!(body.contains("Please wait"));
        assert!(body.contains("before posting again."));

        let reply_count = {
            let conn = state
                .db
                .get()
                .context("failed to get database connection")?;
            conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1 AND is_op = 0",
                rusqlite::params![thread_id],
                |row| row.get::<_, i64>(0),
            )
            .context("failed to count replies")?
        };
        assert_eq!(reply_count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn edit_succeeds_with_owned_cookie_inside_grace_window() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state.clone());

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some(format!("/test/thread/{thread_id}#p{post_id}").as_str())
        );

        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let edited_body: String = conn
            .query_row(
                "SELECT body FROM posts WHERE id = ?1",
                rusqlite::params![post_id],
                |row| row.get(0),
            )
            .context("failed to load edited body")?;
        assert_eq!(edited_body, "edited body");
        Ok(())
    }

    #[tokio::test]
    async fn edit_get_renders_usable_form_for_owned_post() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", get(super::edit_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
        );
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;
        assert!(body.contains(&format!(r#"action="/test/post/{post_id}/edit""#)));
        assert!(body.contains(r#"name="_csrf""#));
        assert!(body.contains(
            r#"name="body" aria-label="edit post body" rows="8" maxlength="4096" required"#
        ));
        assert!(body.contains("available for up to 60 seconds after posting"));
        assert!(body.contains(&format!(r#"href="/test/thread/{thread_id}#p{post_id}""#)));
        Ok(())
    }

    #[tokio::test]
    async fn edit_get_fails_without_owned_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, _owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", get(super::edit_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn edit_fails_without_owned_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, _owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn edit_get_fails_after_expiry() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(
            &state,
            true,
            true,
            crate::handlers::board::SELF_DELETE_WINDOW_SECS + 1,
        )?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", get(super::edit_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn edit_fails_after_grace_window() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(
            &state,
            true,
            true,
            crate::handlers::board::SELF_DELETE_WINDOW_SECS + 1,
        )?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::FORBIDDEN.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn edit_get_fails_when_board_editing_is_disabled() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(&state, false, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", get(super::edit_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn edit_fails_when_board_editing_is_disabled() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(&state, false, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::FORBIDDEN.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn edit_get_fails_when_thread_is_locked() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        set_thread_state(&state, thread_id, true, false)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", get(super::edit_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn edit_fails_when_thread_is_archived() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        set_thread_state(&state, thread_id, false, true)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::FORBIDDEN.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn edit_and_delete_toggles_are_independent() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, false, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .route("/{board}/post/{id}/delete", post(super::delete_own_post))
            .with_state(state.clone());

        let edit_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build edit request")?,
            )
            .await
            .context("failed to receive edit response")?;
        assert_eq!(edit_response.status(), StatusCode::SEE_OTHER);

        let delete_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .context("failed to build delete request")?,
            )
            .await
            .context("failed to receive delete response")?;
        assert_eq!(delete_response.status(), StatusCode::FORBIDDEN);

        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let remaining_posts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .context("failed to count remaining posts")?;
        assert_eq!(remaining_posts, 1);
        Ok(())
    }

    #[tokio::test]
    async fn delete_get_renders_usable_confirmation_form_for_owned_post() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", get(super::delete_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
        );
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;
        assert!(body.contains(&format!(r#"action="/test/post/{post_id}/delete""#)));
        assert!(body.contains(r#"name="_csrf""#));
        assert!(body.contains("delete this post"));
        assert!(body.contains("available for up to 60 seconds after posting"));
        assert!(body.contains(&format!(r#"href="/test/thread/{thread_id}#p{post_id}""#)));
        Ok(())
    }

    #[tokio::test]
    async fn delete_op_succeeds_with_owned_cookie_inside_grace_window() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", post(super::delete_own_post))
            .with_state(state.clone());

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/test/catalog")
        );

        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let remaining_posts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .context("failed to count remaining posts")?;
        assert_eq!(remaining_posts, 0);
        Ok(())
    }

    #[tokio::test]
    async fn delete_fails_without_owned_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, _owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", post(super::delete_own_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .body(Body::from("_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_fails_after_grace_window() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(
            &state,
            true,
            true,
            crate::handlers::board::SELF_DELETE_WINDOW_SECS + 1,
        )?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", post(super::delete_own_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_get_fails_without_owned_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, _owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", get(super::delete_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_get_fails_after_expiry() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(
            &state,
            true,
            true,
            crate::handlers::board::SELF_DELETE_WINDOW_SECS + 1,
        )?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", get(super::delete_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_get_fails_when_board_delete_is_disabled() -> Result<()> {
        let state = crate::test_support::app_state();
        let (_thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, false, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", get(super::delete_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_get_fails_when_thread_is_locked() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        set_thread_state(&state, thread_id, true, false)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", get(super::delete_post_get))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn delete_fails_when_thread_is_archived() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        set_thread_state(&state, thread_id, false, true)?;
        let router = Router::new()
            .route("/{board}/post/{id}/delete", post(super::delete_own_post))
            .with_state(state.clone());

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/delete"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let conn = state
            .db
            .get()
            .context("failed to get database connection")?;
        let remaining_posts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .context("failed to count remaining posts")?;
        assert_eq!(remaining_posts, 1);
        Ok(())
    }

    #[tokio::test]
    async fn onion_host_thread_page_keeps_self_action_routes_internal() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/thread/{id}", get(super::view_thread))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/test/thread/{thread_id}"))
                    .header(header::HOST, "exampleonionservice1234567890.onion")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::OK);

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .context("failed to read response body")?
                .to_vec(),
        )
        .context("response body was not valid UTF-8")?;

        assert!(body.contains(&format!(r#"href="/test/post/{post_id}/edit""#)));
        assert!(body.contains(r#"id="edit-modal-form""#));
        assert!(body.contains(r#"data-board="test""#));
        assert!(body.contains(&format!(r#"data-edit-post-id="{post_id}""#)));
        assert!(body.contains(&format!(r#"href="/test/post/{post_id}/delete""#)));
        assert!(
            !body.contains(r#"action="http"#),
            "self-service forms should stay on internal relative routes"
        );
        assert!(
            !body.contains(r#"fetch("http"#),
            "inline feature JS should not hardcode an absolute host"
        );
        Ok(())
    }

    #[tokio::test]
    async fn onion_host_edit_xhr_redirect_stays_relative() -> Result<()> {
        let state = crate::test_support::app_state();
        let (thread_id, post_id, owned_cookie) = seed_owned_post(&state, true, true, 0)?;
        let router = Router::new()
            .route("/{board}/post/{id}/edit", post(super::edit_post_post))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/test/post/{post_id}/edit"))
                    .header(header::HOST, "exampleonionservice1234567890.onion")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        format!("csrf_token=csrf123; rustchan_owned_posts={owned_cookie}"),
                    )
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from("body=edited+body&_csrf=csrf123"))
                    .context("failed to build request")?,
            )
            .await
            .context("failed to receive response")?;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-redirect")
                .and_then(|value| value.to_str().ok()),
            Some(format!("/test/thread/{thread_id}#p{post_id}").as_str())
        );
        Ok(())
    }
}
