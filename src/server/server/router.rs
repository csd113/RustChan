use axum::{
    extract::DefaultBodyLimit,
    http::header,
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};

use crate::config::CONFIG;
use crate::middleware::AppState;

use super::{
    assets::{serve_css, serve_main_js, serve_theme_init_js},
    headers::{hsts_middleware, safe_timeout_middleware, CONTENT_SECURITY_POLICY},
    lifecycle::track_requests,
    onion_location_middleware,
};

#[allow(clippy::too_many_lines)]
pub(super) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .route("/", get(crate::handlers::board::index))
        .route("/{board}", get(crate::handlers::board::board_index))
        .route(
            "/{board}",
            post(crate::handlers::board::create_thread).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route("/{board}/catalog", get(crate::handlers::board::catalog))
        .route("/{board}/archive", get(crate::handlers::board::board_archive))
        .route("/{board}/search", get(crate::handlers::board::search))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .route(
            "/{board}/thread/{id}",
            post(crate::handlers::thread::post_reply).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route(
            "/{board}/post/{id}/edit",
            get(crate::handlers::thread::edit_post_get),
        )
        .route(
            "/{board}/post/{id}/edit",
            post(crate::handlers::thread::edit_post_post),
        )
        .route(
            "/report",
            post(crate::handlers::board::file_report).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/appeal",
            post(crate::handlers::board::submit_appeal).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/vote",
            post(crate::handlers::thread::vote_handler).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/api/post/{board}/{post_id}",
            get(crate::handlers::board::api_post_preview),
        )
        .route(
            "/{board}/post/{post_id}",
            get(crate::handlers::board::redirect_to_post),
        )
        .route(
            "/admin/post/ban-delete",
            post(crate::handlers::admin::admin_ban_and_delete),
        )
        .route(
            "/admin/appeal/dismiss",
            post(crate::handlers::admin::dismiss_appeal),
        )
        .route(
            "/admin/appeal/accept",
            post(crate::handlers::admin::accept_appeal),
        )
        .route(
            "/{board}/thread/{id}/updates",
            get(crate::handlers::thread::thread_updates),
        )
        .route(
            "/boards/{*media_path}",
            get(crate::handlers::board::serve_board_media),
        )
        .route("/admin", get(crate::handlers::admin::admin_index))
        .route(
            "/admin/login",
            post(crate::handlers::admin::admin_login).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/admin/logout", post(crate::handlers::admin::admin_logout))
        .route("/admin/panel", get(crate::handlers::admin::admin_panel))
        .route(
            "/admin/board/create",
            post(crate::handlers::admin::create_board),
        )
        .route(
            "/admin/board/delete",
            post(crate::handlers::admin::delete_board),
        )
        .route(
            "/admin/board/settings",
            post(crate::handlers::admin::update_board_settings),
        )
        .route(
            "/admin/thread/action",
            post(crate::handlers::admin::thread_action),
        )
        .route(
            "/admin/thread/delete",
            post(crate::handlers::admin::admin_delete_thread),
        )
        .route(
            "/admin/post/delete",
            post(crate::handlers::admin::admin_delete_post),
        )
        .route("/admin/ban/add", post(crate::handlers::admin::add_ban))
        .route(
            "/admin/ban/remove",
            post(crate::handlers::admin::remove_ban),
        )
        .route(
            "/admin/report/resolve",
            post(crate::handlers::admin::resolve_report),
        )
        .route("/admin/mod-log", get(crate::handlers::admin::mod_log_page))
        .route(
            "/admin/filter/add",
            post(crate::handlers::admin::add_filter),
        )
        .route(
            "/admin/filter/remove",
            post(crate::handlers::admin::remove_filter),
        )
        .route(
            "/admin/site/settings",
            post(crate::handlers::admin::update_site_settings),
        )
        .route("/admin/vacuum", post(crate::handlers::admin::admin_vacuum))
        .route(
            "/admin/ip/{ip_hash}",
            get(crate::handlers::admin::admin_ip_history),
        )
        .route("/admin/backup", get(crate::handlers::admin::admin_backup))
        .route(
            "/admin/restore",
            post(crate::handlers::admin::admin_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)),
        )
        .route(
            "/admin/board/backup/{board}",
            get(crate::handlers::admin::board_backup),
        )
        .route(
            "/admin/board/restore",
            post(crate::handlers::admin::board_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)),
        )
        .route(
            "/admin/backup/create",
            post(crate::handlers::admin::create_full_backup),
        )
        .route(
            "/admin/board/backup/create",
            post(crate::handlers::admin::create_board_backup),
        )
        .route(
            "/admin/backup/download/{kind}/{filename}",
            get(crate::handlers::admin::download_backup),
        )
        .route(
            "/admin/backup/progress",
            get(crate::handlers::admin::backup_progress_json),
        )
        .route(
            "/admin/backup/delete",
            post(crate::handlers::admin::delete_backup),
        )
        .route(
            "/admin/backup/restore-saved",
            post(crate::handlers::admin::restore_saved_full_backup),
        )
        .route(
            "/admin/board/backup/restore-saved",
            post(crate::handlers::admin::restore_saved_board_backup),
        )
        .layer(axum_middleware::from_fn(
            crate::middleware::rate_limit_middleware,
        ))
        .layer(DefaultBodyLimit::max(CONFIG.max_video_size))
        .layer(axum_middleware::from_fn(track_requests))
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(axum_middleware::from_fn(
            crate::middleware::normalize_trailing_slash,
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-content-type-options"),
            header::HeaderValue::from_static("nosniff"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-frame-options"),
            header::HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("referrer-policy"),
            header::HeaderValue::from_static("same-origin"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("content-security-policy"),
            header::HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("permissions-policy"),
            header::HeaderValue::from_static(
                "geolocation=(), camera=(), microphone=(), payment=()",
            ),
        ))
        .layer(axum_middleware::from_fn(hsts_middleware))
        .layer(axum_middleware::from_fn(safe_timeout_middleware))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::debug_span!(
                        "http",
                        method = %request.method(),
                        uri    = %request.uri(),
                    )
                })
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::TRACE),
                )
                .on_failure(
                    |error: tower_http::classify::ServerErrorsFailureClass,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::error!(
                            target: "server",
                            %error,
                            latency_ms = latency.as_millis(),
                            "request failed",
                        );
                    },
                ),
        )
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            onion_location_middleware,
        ))
        .with_state(state)
}
