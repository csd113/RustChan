// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

type HomePageLoadResult = (
    Vec<crate::models::BoardStats>,
    Option<crate::models::SiteStats>,
    bool,
    String,
    bool,
    HashMap<i64, i64>,
    bool,
    HashMap<i64, i64>,
);

type BoardIndexLoadResult = (
    String,
    render::BoardPageData,
    crate::banner::BannerSelection,
    bool,
    bool,
    bool,
    Option<(i64, i64)>,
);

pub async fn index(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let user_preferences = user_preferences_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let mut jar = jar;
    let nsfw_consent = has_nsfw_consent(&jar);
    let board_activity_markers = board_activity_markers_from_jar(&jar);
    let thread_activity_markers = thread_activity_markers_from_jar(&jar);

    let admin_session = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let admin_session_for_load = admin_session.clone();
    let (
        board_stats,
        site_data,
        is_admin,
        home_banner_html,
        homepage_thread_badges_enabled,
        board_badges,
        homepage_reply_badges_enabled,
        board_reply_badges,
    ) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_activity_markers = board_activity_markers.clone();
        let thread_activity_markers = thread_activity_markers.clone();
        move || -> Result<HomePageLoadResult> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let site_data = match db::get_site_stats(&conn) {
                Ok(stats) => Some(stats),
                Err(error) => {
                    tracing::warn!(target: "db", %error, "Failed to load home page site stats");
                    None
                }
            };
            let is_admin = admin_session_for_load
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            let home_banner = crate::banner::resolve_home_banner(&conn, "/")?;
            let home_banner_html = crate::banner::render_banner_html(
                &home_banner,
                "home-banner-box",
                "board-banner-image",
            );
            let homepage_thread_badges_enabled = db::get_homepage_new_thread_badges_enabled(&conn);
            let board_badges = if homepage_thread_badges_enabled {
                let inputs = boards
                    .iter()
                    .filter_map(|stats| {
                        board_activity_markers.get(&stats.board.id).map(|marker| {
                            crate::db::BoardActivityCountInput {
                                board_id: stats.board.id,
                                seen_thread_created_at: marker.seen_thread_created_at,
                                seen_thread_id: marker.seen_thread_id,
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                db::count_new_threads_for_boards(&conn, &inputs)?
            } else {
                HashMap::new()
            };
            let homepage_reply_badges_enabled = db::get_homepage_new_reply_badges_enabled(&conn);
            let board_reply_badges = if homepage_reply_badges_enabled {
                let inputs = thread_activity_markers
                    .values()
                    .map(|marker| crate::db::BoardReplyActivityCountInput {
                        thread_id: marker.thread_id,
                        seen_reply_count: marker.seen_reply_count,
                    })
                    .collect::<Vec<_>>();
                db::count_new_replies_for_boards(&conn, &inputs)?
            } else {
                HashMap::new()
            };
            Ok((
                boards,
                site_data,
                is_admin,
                home_banner_html,
                homepage_thread_badges_enabled,
                board_badges,
                homepage_reply_badges_enabled,
                board_reply_badges,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    let board_badges = if homepage_thread_badges_enabled && user_preferences.show_activity_badges {
        board_stats
            .iter()
            .filter_map(|stats| {
                let access_cookie = board_access_cookie_from_jar(&jar, &stats.board.short_name);
                can_view_board(&stats.board, is_admin, access_cookie.as_deref())
                    .then(|| board_badges.get(&stats.board.id).copied())
                    .flatten()
                    .filter(|count| *count > 0)
                    .map(|count| (stats.board.id, count))
            })
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    let board_reply_badges =
        if homepage_reply_badges_enabled && user_preferences.show_activity_badges {
            board_stats
                .iter()
                .filter_map(|stats| {
                    let access_cookie = board_access_cookie_from_jar(&jar, &stats.board.short_name);
                    can_view_board(&stats.board, is_admin, access_cookie.as_deref())
                        .then(|| board_reply_badges.get(&stats.board.id).copied())
                        .flatten()
                        .filter(|count| *count > 0)
                        .map(|count| (stats.board.id, count))
                })
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
    if homepage_thread_badges_enabled || homepage_reply_badges_enabled {
        let known_board_ids = board_stats
            .iter()
            .map(|stats| stats.board.id)
            .collect::<std::collections::HashSet<_>>();
        jar = prune_board_activity_markers(jar, &known_board_ids);
    }

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

    let mut response = Html(templates::index_page(
        &board_stats,
        site_data.as_ref(),
        &csrf,
        admin_scoped_csrf_token(&jar, admin_session.as_deref(), is_admin).as_deref(),
        onion_address.as_deref(),
        &home_banner_html,
        &board_badges,
        &board_reply_badges,
        current_theme.as_deref(),
        nsfw_prompt_board,
        nsfw_consent,
        is_admin,
        user_preferences,
    ))
    .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(HTML_CACHE_CONTROL),
    );
    crate::cache::insert_vary_cookie(response.headers_mut());
    Ok((jar, response).into_response())
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let user_preferences = user_preferences_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());

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
            return Ok(board_access_denied_response(
                jar,
                &denial,
                &csrf,
                current_theme.as_deref(),
            ));
        }
    };

    let thread_activity_markers = thread_activity_markers_from_jar(&jar);
    let page_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let current_path = return_to.clone();
        move || -> Result<BoardIndexLoadResult> {
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
            let thread_badges_enabled = db::get_thread_new_reply_badges_enabled(&conn);
            let homepage_thread_badges_enabled = db::get_homepage_new_thread_badges_enabled(&conn);
            let homepage_reply_badges_enabled = db::get_homepage_new_reply_badges_enabled(&conn);
            let board_id = page_data.board.id;
            Ok((
                render::board_page_etag_signature(&page_data),
                page_data,
                banner_selection,
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
        page_sig,
        page_data,
        banner_selection,
        thread_badges_enabled,
        homepage_thread_badges_enabled,
        homepage_reply_badges_enabled,
        latest_thread_marker,
    ) = page_data;
    let thread_badges = if thread_badges_enabled && user_preferences.show_activity_badges {
        page_data
            .summaries
            .iter()
            .filter_map(|summary| {
                let marker = thread_activity_markers.get(&summary.thread.id)?;
                let unread = (summary.thread.reply_count - marker.seen_reply_count).max(0);
                (unread > 0).then_some((summary.thread.id, unread))
            })
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
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
    let banner_tag = format!("-b{}", banner_selection.etag_fragment);
    let activity_tag = if thread_badges_enabled {
        let mut badge_parts = thread_badges
            .iter()
            .map(|(thread_id, count)| format!("{thread_id}:{count}"))
            .collect::<Vec<_>>();
        badge_parts.sort();
        format!(
            "-na{}",
            crate::utils::crypto::sha256_hex(badge_parts.join("|").as_bytes())
        )
    } else {
        "-na0".to_owned()
    };
    let etag = format!(
        "\"{}-{}-{page}{admin_tag}{post_tag}{greentext_tag}-t{theme_tag}{banner_tag}{activity_tag}-{}\"",
        page_data.pagination.total,
        page_sig,
        user_preferences.etag_fragment()
    );
    let (latest_created_at, latest_thread_id) =
        latest_visible_thread_marker_tuple(latest_thread_marker);
    let jar = if thread_badges_enabled || homepage_reply_badges_enabled {
        let defaults = page_data
            .summaries
            .iter()
            .map(|summary| (summary.thread.id, summary.thread.reply_count));
        remember_thread_activity_defaults(jar, defaults)
    } else {
        jar
    };
    let jar = if homepage_thread_badges_enabled {
        remember_board_activity(jar, page_data.board.id, latest_created_at, latest_thread_id)
    } else {
        jar
    };

    // 3.2: Return 304 Not Modified when the client's cached version is current.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let activity_markers_enabled =
        thread_badges_enabled || homepage_thread_badges_enabled || homepage_reply_badges_enabled;
    if client_etag == etag
        && !banner_selection.disable_not_modified_short_circuit
        && !activity_markers_enabled
    {
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
        crate::cache::insert_vary_cookie(resp.headers_mut());
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
        admin_csrf.as_deref(),
        None,
        None,
        &thread_badges,
        thread_badges_enabled,
        &banner_html,
        current_theme.as_deref(),
        can_post,
        user_preferences,
    );
    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(HTML_CACHE_CONTROL),
    );
    crate::cache::insert_vary_cookie(resp.headers_mut());
    Ok((jar, resp).into_response())
}
