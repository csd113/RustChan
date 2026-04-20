use super::*;

pub async fn index(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let nsfw_consent = has_nsfw_consent(&jar);

    let admin_session = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let (board_stats, site_data, is_admin, home_banner_html) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(
            Vec<crate::models::BoardStats>,
            Option<crate::models::SiteStats>,
            bool,
            String,
        )> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let site_data = match db::get_site_stats(&conn) {
                Ok(stats) => Some(stats),
                Err(error) => {
                    tracing::warn!(target: "db", %error, "Failed to load home page site stats");
                    None
                }
            };
            let is_admin = admin_session
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            let home_banner = crate::banner::resolve_home_banner(&conn, "/")?;
            let home_banner_html =
                crate::banner::render_banner_html(&home_banner, "home-banner-box", "board-banner-image");
            Ok((boards, site_data, is_admin, home_banner_html))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Read the onion address from AppState (populated by the Arti task on startup).
    let onion_address: Option<String> = if crate::config::CONFIG.enable_tor_support {
        state.onion_address.read().await.clone()
    } else {
        None
    };
    let nsfw_prompt_board = params
        .get("nsfw")
        .and_then(|short| board_stats.iter().find(|s| s.board.short_name == *short))
        .map(|s| &s.board);

    if nsfw_consent {
        if let Some(board) = nsfw_prompt_board {
            let redirect_to = if board.access_mode.requires_view_password() {
                format!("/{}/unlock", board.short_name)
            } else {
                format!("/{}/catalog", board.short_name)
            };
            return Ok((jar, Redirect::to(&redirect_to)).into_response());
        }
    }

    Ok((
        jar,
        Html(templates::index_page(
            &board_stats,
            site_data.as_ref(),
            &csrf,
            onion_address.as_deref(),
            &home_banner_html,
            current_theme.as_deref(),
            nsfw_prompt_board,
            nsfw_consent,
            is_admin,
        )),
    )
        .into_response())
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

#[allow(clippy::arithmetic_side_effects)]
pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let return_to = if page > 1 {
        format!("/{board_short}?page={page}")
    } else {
        format!("/{board_short}")
    };

    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = match board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::View,
        return_to.clone(),
    )
    .await?
    {
        BoardAccessDecision::Allowed(context) => context,
        BoardAccessDecision::Denied(denial) => {
            let html = render_board_unlock_html(
                &denial.context.board,
                &csrf,
                &denial.return_to,
                None,
                current_theme.as_deref(),
            );
            return Ok(board_access_required_response(jar, html));
        }
    };

    let page_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let current_path = return_to.clone();
        move || -> Result<(String, render::BoardPageData, crate::banner::BannerSelection)> {
            let conn = pool.get()?;
            let page_data = render::load_board_page_data(
                &conn,
                &board_short,
                page,
                THREADS_PER_PAGE,
                PREVIEW_REPLIES,
                admin_session_id.as_deref(),
            )?;
            let banner_selection = crate::banner::resolve_board_banner(
                &conn,
                &page_data.board,
                crate::models::BannerPlacement::Index,
                &current_path,
            )?;
            Ok((
                render::board_page_etag_signature(&page_data),
                page_data,
                banner_selection,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let can_post = access_context.can_post;
    let (page_sig, page_data, banner_selection) = page_data;
    let admin_tag = if page_data.is_admin { "-a" } else { "" };
    let post_tag = if can_post { "-p1" } else { "-p0" };
    let greentext_tag = if page_data.board.collapse_greentext {
        "-cg1"
    } else {
        "-cg0"
    };
    let banner_tag = format!("-b{}", banner_selection.etag_fragment);
    let etag = format!(
        "\"{}-{}-{page}{admin_tag}{post_tag}{greentext_tag}{banner_tag}\"",
        page_data.pagination.total, page_sig
    );

    // 3.2: Return 304 Not Modified when the client's cached version is current.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if client_etag == etag && !banner_selection.disable_not_modified_short_circuit {
        // StatusCode::NOT_MODIFIED and Body::empty() are always valid constants;
        // this builder call is infallible.
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
            header::CACHE_CONTROL,
            HeaderValue::from_static(HTML_CACHE_CONTROL),
        );
        return Ok((jar, resp).into_response());
    }

    let banner_html = crate::banner::render_banner_html(
        &banner_selection,
        "board-banner-slot",
        "board-banner-image",
    );
    let html = render::render_board_page(
        &page_data,
        &csrf,
        None,
        None,
        &banner_html,
        current_theme.as_deref(),
        can_post,
    );
    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(HTML_CACHE_CONTROL),
    );
    Ok((jar, resp).into_response())
}
