// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]
// The format string stays inline because the handler already has several
// branching paths and this keeps the redirect target easy to scan.
#![allow(clippy::uninlined_format_args)]

use super::*;

// ─── POST /:board/ — create new thread ───────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn create_thread(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let xhr_request = is_xml_http_request(&req_headers);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = match board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::Post,
        format!("/{board_short}"),
    )
    .await?
    {
        BoardAccessDecision::Allowed(context) => context,
        BoardAccessDecision::Denied(denial) => {
            let redirect_to = unlock_redirect_url(&board_short, &denial.return_to);
            return Ok(Redirect::to(&redirect_to).into_response());
        }
    };

    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let form = parse_post_multipart(
        multipart,
        csrf_cookie.as_deref(),
        access_context.board.max_image_size_bytes(),
        access_context.board.max_video_size_bytes(),
        access_context.board.max_audio_size_bytes(),
    )
    .await?;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let post_form_state = crate::templates::forms::PostFormState {
        name: form.name.clone(),
        subject: form.subject.clone(),
        body: form.body.clone(),
        sage: form.sage,
    };

    // Also extract csrf_token before spawn_blocking so the ban page appeal form works.
    let ban_csrf_token = csrf_cookie.clone().unwrap_or_default();

    // Clones kept outside the closure so we can re-render the board page inline on error.
    let admin_session_err = admin_session_id.clone();
    let csrf_for_error = csrf_cookie.clone().unwrap_or_default();

    let board_short_err = board_short.clone();
    let identity_key = identity_key(&client_ip, &jar);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = state.job_queue.clone();
        let ffmpeg_available = state.ffmpeg_available;
        let ffprobe_available = state.ffprobe_available;
        let ffmpeg_webp_available = state.ffmpeg_webp_available;
        move || -> Result<posting::SubmitPostResult> {
            let conn = pool.get()?;
            posting::submit_post(
                &conn,
                &job_queue,
                posting::SubmitPostCommand {
                    mode: posting::SubmitPostMode::NewThread {
                        subject: form.subject,
                        poll_question: form.poll_question,
                        poll_options: form.poll_options,
                        poll_duration_secs: form.poll_duration_secs,
                    },
                    board_short,
                    identity_key,
                    cookie_secret: CONFIG.cookie_secret.clone(),
                    admin_session_id,
                    ban_csrf_token,
                    submission_token: form.submission_token,
                    name: form.name,
                    body: form.body,
                    deletion_token: form.deletion_token,
                    pow_nonce: form.pow_nonce,
                    image_file_data: form.image_file,
                    file_data: form.file,
                    audio_file_data: form.audio_file,
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

    // BadRequest → re-render the board page with an inline error banner so the
    // user sees the message in context without being sent to a separate error page.
    let submit_result = match result {
        Ok(submit_result) => submit_result,
        Err(AppError::BadRequest(msg)) => {
            if xhr_request {
                return xhr_handled_error_response(StatusCode::UNPROCESSABLE_ENTITY, &msg);
            }
            let board_short_render = board_short_err.clone();
            let pool = state.db.clone();
            let current_theme = current_theme.clone();
            let html = tokio::task::spawn_blocking(move || -> Result<String> {
                let conn = pool.get()?;
                let page_data = render::load_board_page_data(
                    &conn,
                    &board_short_render,
                    1,
                    THREADS_PER_PAGE,
                    PREVIEW_REPLIES,
                    admin_session_err.as_deref(),
                )?;
                let banner_selection = crate::banner::resolve_board_banner(
                    &conn,
                    &page_data.board,
                    crate::models::BannerPlacement::Index,
                    &format!("/{}", board_short_render),
                )?;
                let banner_html = crate::banner::render_banner_html(
                    &banner_selection,
                    "board-banner-slot",
                    "board-banner-image",
                );
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
                Ok(render::render_board_page(
                    &page_data,
                    &csrf_for_error,
                    admin_csrf_for_error.as_deref(),
                    Some(&msg),
                    Some(&post_form_state),
                    &std::collections::HashMap::new(),
                    false,
                    &banner_html,
                    current_theme.as_deref(),
                    true,
                ))
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

            let mut resp = Html(html).into_response();
            *resp.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(resp);
        }
        Err(error) => {
            if xhr_request {
                return xhr_post_error_response(error);
            }
            return Err(error);
        }
    };

    let jar = remember_owned_post_until_with_secure(
        jar,
        &submit_result.board_short,
        submit_result.thread_id,
        submit_result.post_id,
        &submit_result.deletion_token,
        submit_result.created_at + SELF_DELETE_WINDOW_SECS,
        should_set_public_secure_cookie(&req_headers, Some(peer)),
    );

    if xhr_request {
        return Ok((jar, xhr_redirect_response(&submit_result.redirect_url)?).into_response());
    }

    Ok((jar, Redirect::to(&submit_result.redirect_url)).into_response())
}

// ─── GET /:board/catalog ──────────────────────────────────────────────────────
