//! Top-level HTTP router assembly and middleware ordering.

use axum::{http::header, middleware as axum_middleware, routing::get, Router};

use crate::middleware::AppState;

#[path = "routes.rs"]
mod routes;

use super::{
    assets::{serve_admin_css, serve_admin_js, serve_css, serve_main_js, serve_theme_init_js},
    headers::{
        admin_cache_middleware, hsts_middleware_with_mode, public_cache_middleware,
        request_boundary_middleware, safe_timeout_middleware, text_response_compression_predicate,
        CONTENT_SECURITY_POLICY,
    },
    lifecycle::track_requests,
    onion_location_middleware,
};
use routes::{admin_routes, public_routes};

/// Build the complete application router around shared state and transport mode.
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
        .layer(axum_middleware::from_fn(
            move |mut req: axum::extract::Request, next: axum_middleware::Next| async move {
                req.extensions_mut()
                    .insert(crate::middleware::RequestTransport { direct_https });
                next.run(req).await
            },
        ))
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
        // Keep framing and header validation outermost so malformed requests
        // never reach authentication, form, or upload handlers.
        .layer(axum_middleware::from_fn(request_boundary_middleware))
        .with_state(state)
}

#[cfg(test)]
/// Router-level response contract tests.
mod tests {
    use super::build_router;
    use anyhow::{anyhow, Context as _};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        Router,
    };
    use axum_extra::extract::cookie::CookieJar;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt as _;

    type TestResult = anyhow::Result<()>;

    /// Seed a public board and refresh the template board cache.
    fn seed_public_media_board(
        state: &crate::middleware::AppState,
        short_name: &str,
    ) -> TestResult {
        let conn = state.db.get().context("get database connection")?;
        crate::db::create_board(&conn, short_name, "Board", "", false)
            .context("create public media board")?;
        let boards = crate::db::get_all_boards(&conn).context("load live boards")?;
        crate::templates::set_live_boards(boards);
        Ok(())
    }

    /// Seed a protected board and administrator session for media requests.
    fn seed_protected_media_board_with_admin(
        state: &crate::middleware::AppState,
        short_name: &str,
    ) -> anyhow::Result<String> {
        let conn = state.db.get().context("get database connection")?;
        let board_id = crate::db::create_board(&conn, short_name, "Secret", "", false)
            .context("create protected media board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash board password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .context("protect media board")?;
        let admin_hash =
            crate::utils::crypto::hash_password("hunter2").context("hash admin password")?;
        let admin_id =
            crate::db::create_admin(&conn, "admin", &admin_hash).context("create admin")?;
        crate::db::create_session(&conn, "media-session", admin_id, i64::MAX)
            .context("create admin session")?;
        let boards = crate::db::get_all_boards(&conn).context("load live boards")?;
        crate::templates::set_live_boards(boards);
        Ok(format!(
            "{}=media-session",
            crate::handlers::board::ADMIN_SESSION_COOKIE
        ))
    }

    /// Generate a collision-resistant board name for filesystem tests.
    fn unique_test_board(prefix: &str) -> anyhow::Result<String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock predates the Unix epoch")?
            .as_nanos();
        Ok(format!("{prefix}{nanos:x}"))
    }

    /// Extract the first response cookie whose name starts with `prefix`.
    fn first_cookie_pair(
        response: &axum::response::Response,
        prefix: &str,
    ) -> anyhow::Result<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(prefix))
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("response did not include a cookie starting with {prefix}"))
    }

    /// Perform an administrator login through a tunneled host.
    async fn tunneled_admin_login_roundtrip(
        router: &Router,
        host: &str,
    ) -> anyhow::Result<(String, String, String)> {
        let login_page = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .header(header::HOST, host)
                    .header(header::REFERER, format!("https://{host}/admin"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())?,
            )
            .await?;
        anyhow::ensure!(
            login_page.status() == StatusCode::OK,
            "administrator login page returned {}",
            login_page.status()
        );

        let csrf_cookie = first_cookie_pair(&login_page, "csrf_token=")?;
        let csrf_value = csrf_cookie
            .strip_prefix("csrf_token=")
            .context("CSRF cookie omitted its expected name")?;
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
                    )))?,
            )
            .await?;
        anyhow::ensure!(
            login_response.status() == StatusCode::SEE_OTHER,
            "administrator login returned {}",
            login_response.status()
        );

        let location = login_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .context("login response omitted a valid redirect location")?;
        let session_cookie = first_cookie_pair(&login_response, "chan_admin_session=")?;
        let rotated_csrf_cookie = first_cookie_pair(&login_response, "csrf_token=")?;

        Ok((location, session_cookie, rotated_csrf_cookie))
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Keeps public health endpoints free of operational details.
    async fn public_health_endpoints_emit_request_id_without_observability_details() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);

        let health = router
            .clone()
            .oneshot(Request::builder().uri("/healthz").body(Body::empty())?)
            .await?;
        assert_eq!(
            health.status(),
            StatusCode::OK,
            "health endpoint should report success"
        );
        assert!(
            health.headers().contains_key("x-request-id"),
            "health response should include a request identifier"
        );
        let health_body = to_bytes(health.into_body(), usize::MAX).await?;
        let health_body: serde_json::Value = serde_json::from_slice(&health_body)?;
        assert_eq!(
            health_body
                .get("status")
                .and_then(serde_json::Value::as_str),
            Some("ok"),
            "health response should include only its aggregate status"
        );
        assert!(
            health_body.get("request_count").is_none(),
            "public health should hide request counts"
        );
        assert!(
            health_body.get("uptime_seconds").is_none(),
            "public health should hide uptime"
        );

        let ready = router
            .clone()
            .oneshot(Request::builder().uri("/readyz").body(Body::empty())?)
            .await?;
        assert_eq!(
            ready.status(),
            StatusCode::OK,
            "readiness endpoint should report success"
        );
        let ready_body = to_bytes(ready.into_body(), usize::MAX).await?;
        let ready_body: serde_json::Value = serde_json::from_slice(&ready_body)?;
        assert_eq!(
            ready_body.get("status").and_then(serde_json::Value::as_str),
            Some("ready"),
            "readiness response should include its aggregate status"
        );
        assert!(
            ready_body.get("database_schema_version").is_none(),
            "public readiness should hide the schema version"
        );
        assert!(
            ready_body.get("tor_enabled").is_none(),
            "public readiness should hide Tor configuration"
        );
        assert!(
            ready_body.get("latest_full_backup_age_hours").is_none(),
            "public readiness should hide backup age"
        );

        let metrics = router
            .oneshot(Request::builder().uri("/metrics").body(Body::empty())?)
            .await?;
        assert_eq!(
            metrics.status(),
            StatusCode::NOT_FOUND,
            "public metrics should stay disabled"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Applies immutable caching only to correctly versioned built-in assets.
    async fn built_in_static_assets_use_versioned_cache_policy() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);

        for uri in ["/static/style.css", "/static/main.js", "/static/admin.css"] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty())?)
                .await?;

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "unversioned built-in asset should be served"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_STATIC_SHORT),
                "unversioned assets should use short caching"
            );

            let versioned_uri = crate::templates::static_asset_url(uri);
            let versioned_response = router
                .clone()
                .oneshot(Request::builder().uri(versioned_uri).body(Body::empty())?)
                .await?;
            assert_eq!(
                versioned_response.status(),
                StatusCode::OK,
                "versioned built-in asset should be served"
            );
            assert_eq!(
                versioned_response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA),
                "valid version identifiers should enable immutable caching"
            );

            let invalid_response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("{uri}?v=invalid"))
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                invalid_response.status(),
                StatusCode::OK,
                "asset with invalid version should still be served"
            );
            assert_eq!(
                invalid_response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_STATIC_SHORT),
                "invalid version identifiers should not enable immutable caching"
            );
        }
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Keeps public dynamic HTML on the revalidation cache policy.
    async fn public_dynamic_html_revalidates_without_immutable_cache() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(Request::builder().uri("/banned").body(Body::empty())?)
            .await?;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "dynamic public page should be served"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_DYNAMIC_PUBLIC),
            "dynamic public page should require revalidation"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Prevents administrator login responses from being stored.
    async fn admin_login_page_is_no_store() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/admin")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "administrator login page should be served"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
            "administrator login should disable storage"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Compresses text responses while retaining byte ranges for uploaded media.
    async fn compression_only_applies_to_text_like_responses() -> TestResult {
        let state = crate::test_support::app_state();
        let board = unique_test_board("compress")?;
        seed_public_media_board(&state, &board)?;
        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        std::fs::create_dir_all(&board_dir).context("create board directory")?;
        let media_path = board_dir.join("movie.mp4");
        std::fs::write(&media_path, vec![0_u8; 512]).context("write media fixture")?;

        let router = build_router(state, false);
        for uri in ["/", "/static/style.css", "/static/main.js"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::ACCEPT_ENCODING, "gzip")
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "text response should be served"
            );
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
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "uploaded media should be served"
        );
        assert!(
            response.headers().get(header::CONTENT_ENCODING).is_none(),
            "uploaded media should not be compressed"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok()),
            Some("bytes"),
            "uploaded media should retain byte-range support"
        );

        std::fs::remove_file(media_path).context("remove media fixture")?;
        std::fs::remove_dir(board_dir).context("remove board fixture directory")?;
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects requests that forbid both compressed and identity responses.
    async fn compression_rejects_requests_that_refuse_every_content_coding() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::ACCEPT_ENCODING, "*;q=0, identity;q=0")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(
            response.status(),
            StatusCode::NOT_ACCEPTABLE,
            "request refusing every content coding should be rejected"
        );
        assert!(
            response.headers().get(header::CONTENT_ENCODING).is_none(),
            "rejected response should not claim a content coding"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Restricts uploaded PDF framing to the same origin.
    async fn uploaded_pdf_route_allows_same_origin_embedding_only() -> TestResult {
        let state = crate::test_support::app_state();
        let board = unique_test_board("pdfhdr")?;
        seed_public_media_board(&state, &board)?;

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        std::fs::create_dir_all(&board_dir).context("create board directory")?;
        let pdf_path = board_dir.join("doc.pdf");
        std::fs::write(
            &pdf_path,
            b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n",
        )
        .context("write PDF fixture")?;

        let router = build_router(state, false);
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/doc.pdf"))
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "uploaded PDF should be served"
        );
        assert_eq!(
            response.headers().get(header::X_FRAME_OPTIONS),
            Some(&header::HeaderValue::from_static("SAMEORIGIN")),
            "uploaded PDF should allow only same-origin framing"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&header::HeaderValue::from_static(
                "default-src 'none'; frame-ancestors 'self'; sandbox allow-same-origin allow-scripts"
            )),
            "uploaded PDF should receive its sandbox policy"
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA),
            "uploaded PDF should use immutable media caching"
        );

        std::fs::remove_file(pdf_path).context("remove PDF fixture")?;
        std::fs::remove_dir(board_dir).context("remove board fixture directory")?;
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Gives immutable media and replaceable board favicons distinct cache policies.
    async fn uploaded_media_and_board_favicons_get_separate_cache_policies() -> TestResult {
        let state = crate::test_support::app_state();
        let board = unique_test_board("cachemedia")?;
        seed_public_media_board(&state, &board)?;

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let favicon_dir = board_dir.join("_favicon");
        std::fs::create_dir_all(&favicon_dir).context("create favicon directory")?;
        let media_path = board_dir.join("image.webp");
        let favicon_path = favicon_dir.join("favicon-32x32.png");
        std::fs::write(&media_path, b"webp bytes").context("write media fixture")?;
        std::fs::write(&favicon_path, b"png bytes").context("write favicon fixture")?;

        let router = build_router(state, false);
        let media_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/image.webp"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            media_response.status(),
            StatusCode::OK,
            "uploaded media should be served"
        );
        assert_eq!(
            media_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA),
            "uploaded media should be immutable"
        );

        let favicon_response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/_favicon/favicon-32x32.png"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            favicon_response.status(),
            StatusCode::OK,
            "board favicon should be served"
        );
        assert_eq!(
            favicon_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_STATIC_SHORT),
            "replaceable board favicon should use short caching"
        );

        std::fs::remove_file(media_path).context("remove media fixture")?;
        std::fs::remove_file(favicon_path).context("remove favicon fixture")?;
        std::fs::remove_dir(favicon_dir).context("remove favicon fixture directory")?;
        std::fs::remove_dir(board_dir).context("remove board fixture directory")?;
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Prevents protected board assets from entering shared caches.
    async fn protected_board_media_and_favicons_are_not_public_cacheable() -> TestResult {
        let state = crate::test_support::app_state();
        let board = unique_test_board("protectedcache")?;
        let cookie = seed_protected_media_board_with_admin(&state, &board)?;

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let favicon_dir = board_dir.join("_favicon");
        std::fs::create_dir_all(&favicon_dir).context("create favicon directory")?;
        let media_path = board_dir.join("image.webp");
        let favicon_path = favicon_dir.join("favicon-32x32.png");
        std::fs::write(&media_path, b"webp bytes").context("write media fixture")?;
        std::fs::write(&favicon_path, b"png bytes").context("write favicon fixture")?;

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
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "authenticated protected asset should be served"
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok()),
                Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE),
                "protected assets must not enter a shared cache"
            );
        }

        std::fs::remove_file(media_path).context("remove media fixture")?;
        std::fs::remove_file(favicon_path).context("remove favicon fixture")?;
        std::fs::remove_dir(favicon_dir).context("remove favicon fixture directory")?;
        std::fs::remove_dir(board_dir).context("remove board fixture directory")?;
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Serves generated SVG thumbnails inline but untrusted SVG uploads as downloads.
    async fn generated_svg_thumbnails_are_inline_but_uploaded_svg_is_attachment() -> TestResult {
        let state = crate::test_support::app_state();
        let board = unique_test_board("svgthumb")?;
        seed_public_media_board(&state, &board)?;

        let board_dir = std::path::Path::new(&crate::config::CONFIG.upload_dir).join(&board);
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).context("create thumbnail directory")?;
        let thumb_path = thumb_dir.join("video.svg");
        let upload_path = board_dir.join("uploaded.svg");
        std::fs::write(&thumb_path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .context("write thumbnail fixture")?;
        std::fs::write(&upload_path, b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")
            .context("write upload fixture")?;

        let router = build_router(state, false);
        let thumb_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/thumbs/video.svg"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            thumb_response.status(),
            StatusCode::OK,
            "generated SVG thumbnail should be served"
        );
        assert_eq!(
            thumb_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/svg+xml"),
            "generated thumbnail should retain its SVG content type"
        );
        assert!(
            thumb_response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .is_none(),
            "generated SVG thumbnail should be displayed inline"
        );

        let upload_response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/boards/{board}/uploaded.svg"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            upload_response.status(),
            StatusCode::OK,
            "uploaded SVG should be served"
        );
        assert_eq!(
            upload_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/octet-stream"),
            "uploaded SVG should be treated as generic binary data"
        );
        assert!(
            upload_response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("attachment;")),
            "uploaded SVG should be forced to download"
        );

        std::fs::remove_file(thumb_path).context("remove thumbnail fixture")?;
        std::fs::remove_file(upload_path).context("remove upload fixture")?;
        std::fs::remove_dir(thumb_dir).context("remove thumbnail fixture directory")?;
        std::fs::remove_dir(board_dir).context("remove board fixture directory")?;
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Keeps ordinary pages unavailable for remote framing.
    async fn pages_keep_remote_framing_blocked() -> TestResult {
        let router = build_router(crate::test_support::app_state(), false);
        let response = router
            .oneshot(Request::builder().uri("/").body(Body::empty())?)
            .await?;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "home page should be served"
        );
        assert_eq!(
            response.headers().get(header::X_FRAME_OPTIONS),
            Some(&header::HeaderValue::from_static("SAMEORIGIN")),
            "legacy framing header should be same-origin"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&header::HeaderValue::from_static(
                super::super::headers::CONTENT_SECURITY_POLICY
            )),
            "home page should receive the global content security policy"
        );
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .context("home page omitted content security policy")?
            .to_str()
            .context("content security policy was not UTF-8")?;
        assert!(
            csp.contains("frame-ancestors 'none'"),
            "content security policy should forbid remote framing"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Keeps administrator redirects valid when accessed through a tunneled host.
    async fn admin_login_redirect_target_resolves_on_tunneled_host() -> TestResult {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().context("get database connection")?;
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").context("hash admin password")?;
            crate::db::create_admin(&conn, "admin", &password_hash).context("create admin")?;
            crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        }

        let router = build_router(state, false);
        let tunneled_host = "demo.serveo.net";
        let (location, session_cookie, csrf_cookie) =
            tunneled_admin_login_roundtrip(&router, tunneled_host).await?;
        assert!(
            location.starts_with("/admin/panel"),
            "login should redirect to the administrator panel"
        );
        let cookie_header = CookieJar::new()
            .add(
                axum_extra::extract::cookie::Cookie::parse(session_cookie.clone())
                    .context("parse administrator session cookie")?,
            )
            .add(
                axum_extra::extract::cookie::Cookie::parse(csrf_cookie.clone())
                    .context("parse rotated CSRF cookie")?,
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
                    .body(Body::empty())?,
            )
            .await?;
        assert_ne!(
            panel_response.status(),
            StatusCode::NOT_FOUND,
            "administrator panel redirect should resolve on the tunneled host"
        );
        Ok(())
    }
}
