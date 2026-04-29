// src/server/server/router.rs

use axum::{http::header, middleware as axum_middleware, routing::get, Router};

use crate::middleware::AppState;

#[path = "routes.rs"]
mod routes;

use super::{
    assets::{serve_admin_css, serve_admin_js, serve_css, serve_main_js, serve_theme_init_js},
    headers::{hsts_middleware_with_mode, safe_timeout_middleware, CONTENT_SECURITY_POLICY},
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
        .merge(public_routes())
        .merge(admin_routes())
        .layer(axum_middleware::from_fn(
            crate::middleware::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(track_requests))
        .layer(tower_http::compression::CompressionLayer::new())
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
            .map(str::to_string)
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
            .map(str::to_string)
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

        let _ = std::fs::remove_file(pdf_path);
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
