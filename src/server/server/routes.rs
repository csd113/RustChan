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

const POST_MULTIPART_HEADROOM_BYTES: usize = 1024 * 1024;

fn post_upload_body_limit() -> usize {
    CONFIG
        .max_video_size
        .max(CONFIG.max_audio_size)
        .saturating_add(POST_MULTIPART_HEADROOM_BYTES)
}

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
            "/android-chrome-192x192.png",
            get(crate::handlers::favicon::serve_android_chrome_192),
        )
        .route(
            "/android-chrome-512x512.png",
            get(crate::handlers::favicon::serve_android_chrome_512),
        )
        .route("/nsfw/accept", post(crate::handlers::board::accept_nsfw))
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
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
        .route("/", get(crate::handlers::board::index))
        .route("/{board}", get(crate::handlers::board::board_index))
        .route(
            "/{board}",
            post(crate::handlers::board::create_thread)
                .layer(DefaultBodyLimit::max(post_upload_body_limit())),
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
                .layer(DefaultBodyLimit::max(post_upload_body_limit())),
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
        .route(
            "/admin/log/live",
            get(crate::handlers::admin::admin_live_log),
        )
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
        .route(
            "/admin/board/reorder",
            post(crate::handlers::admin::reorder_board),
        )
        .route(
            "/admin/site/favicon",
            post(crate::handlers::admin::update_site_favicon)
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
        .route(
            "/admin/board/favicon",
            post(crate::handlers::admin::update_board_favicon)
                .layer(DefaultBodyLimit::max(5 * 1024 * 1024)),
        )
        .route(
            "/admin/board/favicon/clear",
            post(crate::handlers::admin::clear_board_favicon_override),
        )
        .route(
            "/admin/site/banner",
            post(crate::handlers::admin::upload_global_banner)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/admin/home/banner",
            post(crate::handlers::admin::upload_home_banner)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/admin/board/banner",
            post(crate::handlers::admin::upload_board_banner)
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
            "/admin/db/repair",
            post(crate::handlers::admin::admin_db_repair),
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

#[cfg(test)]
mod tests {
    use super::admin_routes;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
    };
    use std::io::{Cursor, Write as _};
    use tower::ServiceExt as _;

    fn board_backup_zip_bytes() -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            writer
                .start_file("board.json", options)
                .expect("start board.json");
            writer
                .write_all(br#"{"version":1,"board":{"short_name":"b","name":"Random","description":"","nsfw":false,"thread_limit":100,"reply_limit":300,"bump_limit":300,"max_threads_per_ip":0,"require_thread_title":false,"enable_flags":false,"text_only":false,"forced_anon":false,"sage_without_cap":false,"max_file_size":0,"max_webm_size":0,"max_comment_chars":2000,"max_replies_per_thread":300,"max_subject_chars":100,"cooldown_seconds":0,"thread_cooldown_seconds":0,"show_thread_stats":false,"archive_threads":false,"public_logs":false,"allow_post_deletion":true,"allow_thread_deletion":true,"allow_media_uploads":true,"allow_polls":true,"default_name":"Anonymous","id":0},"threads":[],"posts":[],"polls":[],"file_hashes":[]}"#)
                .expect("write board.json");
            writer.finish().expect("finish zip");
        }
        cursor.into_inner()
    }

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

    #[tokio::test]
    async fn board_restore_get_redirects_back_to_admin_panel() {
        let app = admin_routes().with_state(crate::test_support::app_state());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/admin/board/restore")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .expect("redirect location header"),
            "/admin/panel"
        );
    }

    #[tokio::test]
    async fn full_restore_board_backup_upload_redirects_with_helpful_error() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            let admin_id =
                crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create board");
            crate::db::create_session(
                &conn,
                "session123",
                admin_id,
                chrono::Utc::now().timestamp() + 3600,
            )
            .expect("create session");
        }

        let app = admin_routes().with_state(state);
        let zip_bytes = board_backup_zip_bytes();
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123")],
            Some(("backup_file", "board.zip", &zip_bytes, "application/zip")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/restore")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::HOST, "localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let refresh = response
            .headers()
            .get("refresh")
            .and_then(|value| value.to_str().ok())
            .expect("refresh header");
        assert!(refresh.contains("/admin/panel?restore_error="));
        assert!(refresh.contains("board+backup"));

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Restore failed."));
        assert!(body.contains("/admin/panel?restore_error="));
    }
}
