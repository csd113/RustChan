//! Route-group composition for the HTTP server.

use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};

use crate::middleware::AppState;
use crate::server::server::observability;
use tower_http::limit::RequestBodyLimitLayer;

/// Compose public pages, APIs, and media routes.
#[expect(
    clippy::too_many_lines,
    reason = "keeping the public route table contiguous makes route precedence auditable"
)]
pub(super) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(observability::healthz))
        .route("/readyz", get(observability::readyz))
        .route("/metrics", get(observability::metrics))
        .route(
            "/favicon.ico",
            get(crate::handlers::favicon::serve_favicon_ico),
        )
        .route(
            "/favicon-16x16.png",
            get(crate::handlers::favicon::serve_favicon_16),
        )
        .route(
            "/favicon-32x32.png",
            get(crate::handlers::favicon::serve_favicon_32),
        )
        .route(
            "/apple-touch-icon.png",
            get(crate::handlers::favicon::serve_apple_touch_icon),
        )
        .route(
            "/captcha/{id}",
            get(crate::handlers::captcha::serve_captcha_image),
        )
        .route(
            "/android-chrome-192x192.png",
            get(crate::handlers::favicon::serve_android_chrome_192),
        )
        .route(
            "/android-chrome-512x512.png",
            get(crate::handlers::favicon::serve_android_chrome_512),
        )
        .route("/nsfw/accept", post(crate::handlers::board::accept_nsfw))
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
        .route(
            "/preferences",
            post(crate::handlers::board::set_user_preferences),
        )
        .route("/banned", get(crate::handlers::board::banned_page))
        .route(
            "/theme-css/{theme}",
            get(crate::handlers::board::serve_theme_css),
        )
        .route(
            "/banner/assets/{id}",
            get(crate::handlers::banner::serve_banner_asset),
        )
        .route(
            "/banner/external/{id}",
            get(crate::handlers::banner::external_banner_warning_page),
        )
        .route(
            "/banner/external/{id}/continue",
            get(crate::handlers::banner::external_banner_continue),
        )
        .route("/setup", get(crate::handlers::setup::setup_get))
        .route("/setup/review", post(crate::handlers::setup::setup_review))
        .route("/setup/finish", post(crate::handlers::setup::setup_finish))
        .route("/", get(crate::handlers::board::index))
        .route("/{board}", get(crate::handlers::board::board_index))
        .route(
            "/{board}",
            post(crate::handlers::board::create_thread)
                // Axum's extractor limit remains disabled so board-specific
                // limits above 2 MiB work. This outer stream limit prevents
                // Multer from buffering an unbounded malformed preamble or
                // field-header block before RustChan sees the first field.
                .layer::<_, std::convert::Infallible>(RequestBodyLimitLayer::new(
                    crate::handlers::PUBLIC_MULTIPART_REQUEST_MAX_BYTES,
                ))
                .layer::<_, std::convert::Infallible>(axum_middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/{board}/unlock",
            get(crate::handlers::board::board_unlock_page)
                .post(crate::handlers::board::unlock_board_access),
        )
        .route("/{board}/catalog", get(crate::handlers::board::catalog))
        .route(
            "/{board}/hidden",
            get(crate::handlers::board::hidden_threads),
        )
        .route(
            "/{board}/thread-preference",
            post(crate::handlers::board::update_thread_preference)
                .layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/{board}/archive",
            get(crate::handlers::board::board_archive),
        )
        .route("/{board}/search", get(crate::handlers::board::search))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .route(
            "/{board}/thread/{id}",
            post(crate::handlers::thread::post_reply)
                .layer::<_, std::convert::Infallible>(RequestBodyLimitLayer::new(
                    crate::handlers::PUBLIC_MULTIPART_REQUEST_MAX_BYTES,
                ))
                .layer::<_, std::convert::Infallible>(axum_middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::disable()),
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
            "/{board}/post/{id}/delete",
            get(crate::handlers::thread::delete_post_get),
        )
        .route(
            "/{board}/post/{id}/delete",
            post(crate::handlers::thread::delete_own_post),
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
            "/{board}/thread/{id}/updates",
            get(crate::handlers::thread::thread_updates),
        )
        .route(
            "/boards/{*media_path}",
            get(crate::handlers::board::serve_board_media),
        )
}

/// Compose all authenticated administration route groups.
pub(super) fn admin_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/restart/status",
            get(crate::handlers::admin::restart::status),
        )
        .route(
            "/admin/restart",
            post(crate::handlers::admin::restart::restart),
        )
        .route(
            "/admin/updates/status",
            get(crate::handlers::admin::updates::status),
        )
        .route(
            "/admin/updates/check",
            post(crate::handlers::admin::updates::check),
        )
        .route(
            "/admin/updates/install",
            post(crate::handlers::admin::updates::install),
        )
        .merge(admin_auth_routes())
        .route(
            "/admin/network/settings",
            post(crate::handlers::admin::update_network_settings),
        )
        .route(
            "/admin/appearance/defaults",
            post(crate::handlers::admin::update_visitor_defaults),
        )
        .route(
            "/admin/secrets/rotate",
            post(crate::handlers::admin::rotate_secret),
        )
        .route(
            "/admin/accounts/{action}",
            post(crate::handlers::admin::update_account),
        )
        .route(
            "/admin/config/{section}",
            post(crate::handlers::admin::update_runtime_settings),
        )
        .merge(admin_board_routes())
        .merge(admin_backup_routes())
        .merge(admin_moderation_routes())
}

/// Compose administrator authentication and dashboard routes.
fn admin_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(crate::handlers::admin::admin_index))
        .route(
            "/admin/login",
            post(crate::handlers::admin::admin_login).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/admin/logout", post(crate::handlers::admin::admin_logout))
        .route("/admin/panel", get(crate::handlers::admin::admin_panel))
        .route(
            "/admin/site-health/jobs",
            get(crate::handlers::admin::admin_site_health_jobs),
        )
        .route(
            "/admin/site-health/jobs/dismiss",
            post(crate::handlers::admin::dismiss_failed_site_health_jobs),
        )
        .route(
            "/admin/log/live",
            get(crate::handlers::admin::admin_live_log),
        )
}

/// Compose board and site asset management routes.
fn admin_board_routes() -> Router<AppState> {
    Router::new()
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
            "/admin/board/reorder",
            post(crate::handlers::admin::reorder_board),
        )
        .route(
            "/admin/site/favicon",
            post(crate::handlers::admin::update_site_favicon)
                .layer::<_, std::convert::Infallible>(axum::middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
        .route(
            "/admin/board/favicon",
            post(crate::handlers::admin::update_board_favicon)
                .layer::<_, std::convert::Infallible>(axum::middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
        .route(
            "/admin/board/favicon/clear",
            post(crate::handlers::admin::clear_board_favicon_override),
        )
        .route(
            "/admin/site/banner",
            post(crate::handlers::admin::upload_global_banner)
                .layer::<_, std::convert::Infallible>(axum::middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/admin/home/banner",
            post(crate::handlers::admin::upload_home_banner)
                .layer::<_, std::convert::Infallible>(axum::middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/admin/board/banner",
            post(crate::handlers::admin::upload_board_banner)
                .layer::<_, std::convert::Infallible>(axum::middleware::from_fn(
                    crate::handlers::enforce_public_multipart_envelope,
                ))
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/admin/board/banner/clear",
            post(crate::handlers::admin::clear_board_banner_override),
        )
        .route(
            "/admin/banner/update",
            post(crate::handlers::admin::update_banner_meta),
        )
        .route(
            "/admin/banner/delete",
            post(crate::handlers::admin::delete_banner),
        )
        .route(
            "/admin/banner/move",
            post(crate::handlers::admin::move_banner),
        )
}

/// Compose moderation, configuration, and database-maintenance routes.
fn admin_moderation_routes() -> Router<AppState> {
    Router::new()
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
        .route(
            "/admin/theme/create",
            post(crate::handlers::admin::create_theme),
        )
        .route(
            "/admin/theme/update",
            post(crate::handlers::admin::update_theme),
        )
        .route(
            "/admin/theme/delete",
            post(crate::handlers::admin::delete_theme),
        )
        .route(
            "/admin/db/check",
            post(crate::handlers::admin::admin_db_check),
        )
        .route(
            "/admin/media/settings",
            post(crate::handlers::admin::update_media_settings),
        )
        .route(
            "/admin/setup/reopen",
            post(crate::handlers::setup::admin_reopen_setup),
        )
        .route(
            "/admin/setup/close",
            post(crate::handlers::setup::admin_close_setup),
        )
        .route(
            "/admin/db/repair",
            get(crate::handlers::admin::admin_db_repair_status)
                .post(crate::handlers::admin::admin_db_repair),
        )
        .route(
            "/admin/db/repair/status",
            get(crate::handlers::admin::admin_db_repair_status),
        )
        .route(
            "/admin/db/repair/progress",
            get(crate::handlers::admin::admin_db_repair_progress_json),
        )
        .route("/admin/vacuum", post(crate::handlers::admin::admin_vacuum))
        .route(
            "/admin/ip/report",
            post(crate::handlers::admin::admin_ip_report),
        )
        .route(
            "/admin/ip/{ip_hash}",
            get(crate::handlers::admin::admin_ip_history),
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
}

/// Compose full-site and per-board backup and restore routes.
fn admin_backup_routes() -> Router<AppState> {
    Router::new()
        .route("/admin/backup", get(crate::handlers::admin::admin_backup))
        .route(
            "/admin/restore",
            get(|| async { axum::response::Redirect::to("/admin/panel") })
                .post(crate::handlers::admin::admin_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)),
        )
        .route(
            "/admin/board/backup/{board}",
            get(crate::handlers::admin::board_backup),
        )
        .route(
            "/admin/board/restore",
            get(|| async { axum::response::Redirect::to("/admin/panel") })
                .post(crate::handlers::admin::board_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)),
        )
        .route(
            "/admin/backup/create",
            post(crate::handlers::admin::create_full_backup),
        )
        .route(
            "/admin/backup/settings",
            post(crate::handlers::admin::update_full_backup_settings),
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
            "/admin/backup/extract-board",
            post(crate::handlers::admin::extract_board_from_full_backup),
        )
        .route(
            "/admin/board/backup/restore-saved",
            post(crate::handlers::admin::restore_saved_board_backup),
        )
        .layer(axum_middleware::from_fn(
            crate::handlers::admin::backup_request_logging_middleware,
        ))
}
