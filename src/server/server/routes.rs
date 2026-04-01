// src/server/server/routes.rs

use axum::{
    extract::DefaultBodyLimit,
    middleware as axum_middleware,
    routing::{get, post},
    Router,
};

use crate::config::CONFIG;
use crate::middleware::AppState;
use crate::server::server::observability;

pub(super) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(observability::healthz))
        .route("/readyz", get(observability::readyz))
        .route("/metrics", get(observability::metrics))
        .route("/nsfw/accept", post(crate::handlers::board::accept_nsfw))
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
        .route("/", get(crate::handlers::board::index))
        .route("/{board}", get(crate::handlers::board::board_index))
        .route(
            "/{board}",
            post(crate::handlers::board::create_thread).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route("/{board}/catalog", get(crate::handlers::board::catalog))
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
            "/{board}/thread/{id}/updates",
            get(crate::handlers::thread::thread_updates),
        )
        .route(
            "/boards/{*media_path}",
            get(crate::handlers::board::serve_board_media),
        )
}

pub(super) fn admin_routes() -> Router<AppState> {
    Router::new()
        .merge(admin_auth_routes())
        .merge(admin_board_routes())
        .merge(admin_backup_routes())
        .merge(admin_moderation_routes())
}

fn admin_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/admin", get(crate::handlers::admin::admin_index))
        .route(
            "/admin/login",
            post(crate::handlers::admin::admin_login).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/admin/logout", post(crate::handlers::admin::admin_logout))
        .route("/admin/panel", get(crate::handlers::admin::admin_panel))
}

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
}

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
        .route("/admin/vacuum", post(crate::handlers::admin::admin_vacuum))
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

fn admin_backup_routes() -> Router<AppState> {
    Router::new()
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
            crate::handlers::admin::backup_request_logging_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use super::admin_routes;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
    };
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn board_restore_route_accepts_large_multipart_body_without_global_media_limit() {
        let app = admin_routes().with_state(crate::test_support::app_state());
        let file_bytes = vec![b'a'; 60 * 1024 * 1024];
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123")],
            Some(("backup_file", "board.zip", &file_bytes, "application/zip")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/board/restore")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Board restore"));
    }
}
