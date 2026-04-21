use super::*;

pub async fn catalog(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = match board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::View,
        format!("/{board_short}/catalog"),
    )
    .await?
    {
        BoardAccessDecision::Allowed(context) => context,
        BoardAccessDecision::Denied(denial) => {
            return Ok(board_access_denied_response(
                jar,
                &denial,
                &csrf,
                current_theme.as_deref(),
            ));
        }
    };

    // Add ETag caching to the catalog. Previously every request
    // fetched up to 200 full thread rows and re-rendered the entire page
    // regardless of whether anything changed. The ETag is derived from the
    // most-recently-bumped thread, mirroring the board index handler.
    let catalog_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let viewer_key = viewer_key.clone();
        move || -> Result<(CatalogRenderData, crate::banner::BannerSelection)> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let all_threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let prefs = db::get_preferences_for_board(&conn, &viewer_key, board.id)?;
            let (threads, hidden_threads, pinned_ids) = split_catalog_threads(all_threads, &prefs);
            let catalog_sig = threads
                .iter()
                .map(|thread| {
                    format!(
                        "{}:{}:{}:{}",
                        thread.id,
                        thread.bumped_at,
                        i32::from(thread.sticky),
                        i32::from(thread.archived)
                    )
                })
                .collect::<Vec<_>>()
                .join("|");
            let mut pref_sig_parts = prefs
                .iter()
                .map(|(thread_id, pref)| {
                    format!(
                        "{thread_id}:{}:{}",
                        i32::from(pref.pinned),
                        i32::from(pref.hidden)
                    )
                })
                .collect::<Vec<_>>();
            pref_sig_parts.sort();
            let pref_sig = pref_sig_parts.join("|");
            let etag_signature = format!("{catalog_sig}-{pref_sig}");
            let banner_selection = crate::banner::resolve_board_banner(
                &conn,
                &board,
                crate::models::BannerPlacement::Catalog,
                &format!("/{board_short}/catalog"),
            )?;
            Ok((
                (
                    board,
                    threads,
                    pinned_ids,
                    hidden_threads.len(),
                    etag_signature,
                ),
                banner_selection,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let can_post = access_context.can_post;
    let ((board, threads, pinned_ids, hidden_count, etag_signature), banner_selection) =
        catalog_data;
    let admin_tag = if access_context.is_admin { "-a" } else { "" };
    let post_tag = if can_post { "-p1" } else { "-p0" };
    let greentext_tag = if board.collapse_greentext {
        "-cg1"
    } else {
        "-cg0"
    };
    let etag = format!(
        "\"{etag_signature}-catalog{admin_tag}{post_tag}{greentext_tag}-b{}\"",
        banner_selection.etag_fragment
    );

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
    let all_boards = crate::templates::live_boards();
    let html = templates::catalog_page(
        &board,
        &threads,
        &pinned_ids,
        hidden_count,
        false,
        &csrf,
        all_boards.as_slice(),
        access_context.is_admin,
        &banner_html,
        current_theme.as_deref(),
        board.collapse_greentext,
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

pub async fn hidden_threads(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = match board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::View,
        format!("/{board_short}/hidden"),
    )
    .await?
    {
        BoardAccessDecision::Allowed(context) => context,
        BoardAccessDecision::Denied(denial) => {
            return Ok(board_access_denied_response(
                jar,
                &denial,
                &csrf,
                current_theme.as_deref(),
            ));
        }
    };

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let all_threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let prefs = db::get_preferences_for_board(&conn, &viewer_key, board.id)?;
            let (_visible, hidden_threads, pinned_ids) = split_catalog_threads(all_threads, &prefs);

            let all_boards = crate::templates::live_boards();
            Ok(templates::catalog_page(
                &board,
                &hidden_threads,
                &pinned_ids,
                hidden_threads.len(),
                true,
                &csrf,
                all_boards.as_slice(),
                access_context.is_admin,
                "",
                current_theme.as_deref(),
                board.collapse_greentext,
                access_context.can_post,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── GET /:board/archive ──────────────────────────────────────────────────────

pub async fn board_archive(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response> {
    const ARCHIVE_PER_PAGE: i64 = 20;
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let return_to = if page > 1 {
        format!("/{board_short}/archive?page={page}")
    } else {
        format!("/{board_short}/archive")
    };
    if let BoardAccessDecision::Denied(denial) = board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::View,
        return_to,
    )
    .await?
    {
        return Ok(board_access_denied_response(
            jar,
            &denial,
            &csrf,
            current_theme.as_deref(),
        ));
    }

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

            let all_boards = crate::templates::live_boards();
            Ok(templates::archive_page(
                &board,
                &threads,
                &pagination,
                &csrf_clone,
                all_boards.as_slice(),
                current_theme.as_deref(),
                board.collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── GET /:board/search ───────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(q): Query<SearchQuery>,
    jar: CookieJar,
) -> Result<Response> {
    const SEARCH_PER_PAGE: i64 = 20;
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    // Cap query length to prevent excessively large LIKE pattern scans.
    let query_str: String = q.q.trim().chars().take(SEARCH_QUERY_MAX_CHARS).collect();
    let page = q.page.max(1);
    let mut return_to = format!(
        "/{board_short}/search?q={}",
        crate::templates::urlencoding_simple(&query_str)
    );
    if page > 1 {
        return_to.push_str(&format!("&page={page}"));
    }
    if let BoardAccessDecision::Denied(denial) = board_access_preflight(
        &state,
        &board_short,
        admin_session_id.clone(),
        access_cookie,
        BoardAccessRequirement::View,
        return_to,
    )
    .await?
    {
        return Ok(board_access_denied_response(
            jar,
            &denial,
            &csrf,
            current_theme.as_deref(),
        ));
    }

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

            let all_boards = crate::templates::live_boards();
            Ok(templates::search_page(
                &board,
                &query_str,
                &posts,
                &pagination,
                &csrf_clone,
                all_boards.as_slice(),
                current_theme.as_deref(),
                board.collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}
