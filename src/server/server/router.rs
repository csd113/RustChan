// src/server/server/router.rs

use axum::{http::header, middleware as axum_middleware, routing::get, Router};

use crate::middleware::AppState;

#[path = "routes.rs"]
mod routes;

use super::{
    assets::{serve_admin_css, serve_admin_js, serve_css, serve_main_js, serve_theme_init_js},
    headers::{
        admin_cache_middleware, hsts_middleware_with_mode, public_cache_middleware,
        safe_timeout_middleware, text_response_compression_predicate, CONTENT_SECURITY_POLICY,
    },
    lifecycle::track_requests,
    onion_location_middleware,
};
use routes::{admin_routes, public_routes};

pub(super) fn build_router(state: AppState, direct_https: bool) -> Router {
    let behind_proxy = crate::config::CONFIG.behind_proxy;

    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/admin.css", get(serve_admin_css))
        .route("/static/admin.js", get(serve_admin_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .merge(public_routes().layer(axum_middleware::from_fn(public_cache_middleware)))
        .merge(admin_routes().layer(axum_middleware::from_fn(admin_cache_middleware)))
        .layer(axum_middleware::from_fn(
            crate::middleware::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(track_requests))
        .layer(
            tower_http::compression::CompressionLayer::new()
                .compress_when(text_response_compression_predicate),
        )
        .layer(axum_middleware::from_fn(
            crate::middleware::normalize_trailing_slash,
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-content-type-options"),
            header::HeaderValue::from_static("nosniff"),
        ))
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                header::HeaderName::from_static("x-frame-options"),
                header::HeaderValue::from_static("SAMEORIGIN"),
            ),
        )
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("referrer-policy"),
            header::HeaderValue::from_static("same-origin"),
        ))
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                header::HeaderName::from_static("content-security-policy"),
                header::HeaderValue::from_static(CONTENT_SECURITY_POLICY),
            ),
        )
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("permissions-policy"),
            header::HeaderValue::from_static(
                "geolocation=(), camera=(), microphone=(), payment=()",
            ),
        ))
        .layer(axum_middleware::from_fn(move |req, next| {
            hsts_middleware_with_mode(req, next, direct_https, behind_proxy)
        }))
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

#[cfg(test)]
mod tests {
    use super::build_router;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        Router,
    };
    use axum_extra::extract::cookie::CookieJar;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;

    fn seed_public_media_board(state: &crate::middleware::AppState, short_name: &str) {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, short_name, "Board", "", false).expect("create board");
        crate::templates::set_live_boards(crate::db::get_all_boards(&conn).expect("load boards"));
    }

    fn seed_protected_media_board_with_admin(
        state: &crate::middleware::AppState,
        short_name: &str,
    ) -> String {
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, short_name, "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .expect("protect board");
        let admin_hash = crate::utils::crypto::hash_password("hunter2").expect("hash admin");
        let admin_id = crate::db::create_admin(&conn, "admin", &admin_hash).expect("create admin");
        crate::db::create_session(&conn, "media-session", admin_id, i64::MAX)
            .expect("create admin session");
        crate::templates::set_live_boards(crate::db::get_all_boards(&conn).expect("load boards"));
        format!(
            "{}=media-session",
            crate::handlers::board::ADMIN_SESSION_COOKIE
        )
    }

    fn unique_test_board(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        format!("{prefix}{nanos:x}")
    }

    fn first_cookie_pair(response: &axum::response::Response, prefix: &str) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(prefix))
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
            .expect("cookie pair")
    }

    async fn tunneled_admin_login_roundtrip(
        router: &Router,
        host: &str,
    ) -> (String, String, String) {
        let login_page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .header(header::HOST, host)
                    .header(header::REFERER, format!("https://{host}/admin"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("login page");
        assert_eq!(login_page.status(), StatusCode::OK);

        let csrf_cookie = first_cookie_pair(&login_page, "csrf_token=");
        let csrf_value = csrf_cookie.strip_prefix("csrf_token=").expect("csrf value");
        let csrf_form = crate::utils::crypto::make_scoped_csrf_form_token(
            csrf_value,
            &crate::config::CONFIG.cookie_secret,
            "admin-login",
        );

        let login_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, host)
                    .header(header::ORIGIN, "null")
                    .header(header::REFERER, format!("https://{host}/admin"))
                    .header(header::COOKIE, &csrf_cookie)
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={csrf_form}"
                    )))
                    .expect("request"),
            )
            .await
            .expect("login response");
        assert_eq!(login_response.status(), StatusCode::SEE_OTHER);

        let location = login_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .expect("location");
        let session_cookie = first_cookie_pair(&login_response, "chan_admin_session=");
        let rotated_csrf_cookie = first_cookie_pair(&login_response, "csrf_token=");

        (location, session_cookie, rotated_csrf_cookie)
    }

    #[tokio::test]
    async fn health_endpoints_emit_request_id_and_metrics() {
        let router = build_router(crate::test_support::app_state(), false);

        let health = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("health response");
        assert_eq!(health.status(), StatusCode::OK);
        assert!(health.headers().contains_key("x-request-id"));

        let ready = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("ready response");
        assert_eq!(ready.status(), StatusCode::OK);

        let metrics = router
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("metrics response");
        assert_eq!(metrics.status(), StatusCode::OK);
        let body = to_bytes(metrics.into_body(), usize::MAX)
            .await
            .expect("metrics body");
        let body = String::from_utf8(body.to_vec()).expect("utf8 metrics");
        assert!(body.contains("rustchan_requests_total"));
        assert!(body.contains("rustchan_job_queue_pending"));
    }

    #[tokio::test]
    async fn built_in_static_assets_use_versioned_cache_policy() {
        let router = build_router(crate::test_support::app_state(), false);

        for uri in ["/static/style.css", "/static/main.js", "/static/admin.css"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_STATIC_SHORT)
            );

            let versioned_uri = crate::templates::static_asset_url(uri);
            let versioned_response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(versioned_uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(versioned_response.status(), StatusCode::OK);
            assert_eq!(
                versioned_response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA)
            );

            let invalid_response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("{uri}?v=invalid"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(invalid_response.status(), StatusCode::OK);
            assert_eq!(
                invalid_response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_STATIC_SHORT)
            );
        }
    }

    #[tokio::test]
    async fn public_dynamic_html_revalidates_without_immutable_cache() {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/banned")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_DYNAMIC_PUBLIC)
        );
    }

    #[tokio::test]
    async fn admin_login_page_is_no_store() {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
        );
    }

    #[tokio::test]
    async fn compression_only_applies_to_text_like_responses() {
        let state = crate::test_support::app_state();
        let board = unique_test_board("compress");
        seed_public_media_board(&state, &board);
        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        let media_path = board_dir.join("movie.mp4");
        std::fs::write(&media_path, vec![0_u8; 512]).expect("write media");

        let router = build_router(state, false);
        for uri in ["/", "/static/style.css", "/static/main.js"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::ACCEPT_ENCODING, "gzip")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                response.headers().contains_key(header::CONTENT_ENCODING),
                "{uri} should be compressed"
            );
        }

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/movie.mp4"))
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(
            response
                .headers()
                .get(header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes")
        );

        let _ = std::fs::remove_file(media_path);
        let _ = std::fs::remove_dir(board_dir);
    }

    #[tokio::test]
    async fn uploaded_pdf_route_allows_same_origin_embedding_only() {
        let state = crate::test_support::app_state();
        let board = unique_test_board("pdfhdr");
        seed_public_media_board(&state, &board);

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        let pdf_path = board_dir.join("doc.pdf");
        std::fs::write(
            &pdf_path,
            b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n",
        )
        .expect("write pdf");

        let router = build_router(state, false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/doc.pdf"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::X_FRAME_OPTIONS),
            Some(&header::HeaderValue::from_static("SAMEORIGIN"))
        );
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&header::HeaderValue::from_static(
                "default-src 'none'; frame-ancestors 'self'; sandbox allow-same-origin allow-scripts"
            ))
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA)
        );

        let _ = std::fs::remove_file(pdf_path);
        let _ = std::fs::remove_dir(board_dir);
    }

    #[tokio::test]
    async fn uploaded_media_and_board_favicons_get_separate_cache_policies() {
        let state = crate::test_support::app_state();
        let board = unique_test_board("cachemedia");
        seed_public_media_board(&state, &board);

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let favicon_dir = board_dir.join("_favicon");
        std::fs::create_dir_all(&favicon_dir).expect("create board dirs");
        let media_path = board_dir.join("image.webp");
        let favicon_path = favicon_dir.join("favicon-32x32.png");
        std::fs::write(&media_path, b"webp bytes").expect("write media");
        std::fs::write(&favicon_path, b"png bytes").expect("write favicon");

        let router = build_router(state, false);
        let media_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/image.webp"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("media response");
        assert_eq!(media_response.status(), StatusCode::OK);
        assert_eq!(
            media_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA)
        );

        let favicon_response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/_favicon/favicon-32x32.png"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("favicon response");
        assert_eq!(favicon_response.status(), StatusCode::OK);
        assert_eq!(
            favicon_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_STATIC_SHORT)
        );

        let _ = std::fs::remove_file(media_path);
        let _ = std::fs::remove_file(favicon_path);
        let _ = std::fs::remove_dir(favicon_dir);
        let _ = std::fs::remove_dir(board_dir);
    }

    #[tokio::test]
    async fn protected_board_media_and_favicons_are_not_public_cacheable() {
        let state = crate::test_support::app_state();
        let board = unique_test_board("protectedcache");
        let cookie = seed_protected_media_board_with_admin(&state, &board);

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let favicon_dir = board_dir.join("_favicon");
        std::fs::create_dir_all(&favicon_dir).expect("create board dirs");
        let media_path = board_dir.join("image.webp");
        let favicon_path = favicon_dir.join("favicon-32x32.png");
        std::fs::write(&media_path, b"webp bytes").expect("write media");
        std::fs::write(&favicon_path, b"png bytes").expect("write favicon");

        let router = build_router(state, false);
        for uri in [
            format!("/boards/{board}/image.webp"),
            format!("/boards/{board}/_favicon/favicon-32x32.png?v=1"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE)
            );
        }

        let _ = std::fs::remove_file(media_path);
        let _ = std::fs::remove_file(favicon_path);
        let _ = std::fs::remove_dir(favicon_dir);
        let _ = std::fs::remove_dir(board_dir);
    }

    #[tokio::test]
    async fn generated_svg_thumbnails_are_inline_but_uploaded_svg_is_attachment() {
        let state = crate::test_support::app_state();
        let board = unique_test_board("svgthumb");
        seed_public_media_board(&state, &board);

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).expect("create board dirs");
        let thumb_path = thumb_dir.join("video.svg");
        let upload_path = board_dir.join("uploaded.svg");
        std::fs::write(&thumb_path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .expect("write thumb");
        std::fs::write(&upload_path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .expect("write upload");

        let router = build_router(state, false);
        let thumb_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/thumbs/video.svg"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(thumb_response.status(), StatusCode::OK);
        assert_eq!(
            thumb_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/svg+xml")
        );
        assert!(thumb_response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .is_none());

        let upload_response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/uploaded.svg"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(upload_response.status(), StatusCode::OK);
        assert_eq!(
            upload_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/octet-stream")
        );
        assert!(upload_response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("attachment;")));

        let _ = std::fs::remove_file(thumb_path);
        let _ = std::fs::remove_file(upload_path);
        let _ = std::fs::remove_dir(thumb_dir);
        let _ = std::fs::remove_dir(board_dir);
    }

    #[tokio::test]
    async fn pages_keep_remote_framing_blocked() {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::X_FRAME_OPTIONS),
            Some(&header::HeaderValue::from_static("SAMEORIGIN"))
        );
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&header::HeaderValue::from_static(
                super::super::headers::CONTENT_SECURITY_POLICY
            ))
        );
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("csp")
            .to_str()
            .expect("utf8 csp");
        assert!(csp.contains("frame-ancestors 'none'"));
    }

    #[tokio::test]
    async fn admin_login_redirect_target_resolves_on_tunneled_host() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = build_router(state, false);
        let tunneled_host = "demo.serveo.net";
        let (location, session_cookie, csrf_cookie) =
            tunneled_admin_login_roundtrip(&router, tunneled_host).await;
        assert!(location.starts_with("/admin/panel"));
        let cookie_header = CookieJar::new()
            .add(
                axum_extra::extract::cookie::Cookie::parse(session_cookie.clone())
                    .expect("session cookie parse"),
            )
            .add(
                axum_extra::extract::cookie::Cookie::parse(csrf_cookie.clone())
                    .expect("csrf cookie parse"),
            )
            .iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect::<Vec<_>>()
            .join("; ");

        let panel_response = router
            .oneshot(
                Request::builder()
                    .uri(location)
                    .header(header::HOST, tunneled_host)
                    .header(header::REFERER, "https://demo.serveo.net/admin")
                    .header(header::COOKIE, cookie_header)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("panel response");
        assert_ne!(panel_response.status(), StatusCode::NOT_FOUND);
    }
}
