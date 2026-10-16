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

/// Record generic server failures without exposing request query strings.
fn log_http_failure(
    error: &tower_http::classify::ServerErrorsFailureClass,
    latency: std::time::Duration,
    _span: &tracing::Span,
) {
    tracing::error!(target: "server", %error, latency_ms = latency.as_millis(), "request failed");
}

/// Build the complete application router around shared state and transport mode.
pub(super) fn build_router(state: AppState, direct_https: bool) -> Router {
    let behind_proxy = crate::config::CONFIG.behind_proxy;

    Router::new()
        .fallback(|| async { crate::error::AppError::NotFound("Page not found.".into()) })
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/admin.css", get(serve_admin_css))
        .route("/static/admin.js", get(serve_admin_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .merge(public_routes().layer(axum_middleware::from_fn(public_cache_middleware)))
        .merge(admin_routes().layer(axum_middleware::from_fn(admin_cache_middleware)))
        // Inner upload routes explicitly override this streaming extractor cap.
        .layer(axum::extract::DefaultBodyLimit::max(
            super::headers::HTTP_FORM_MAX_BYTES,
        ))
        .layer(axum_middleware::from_fn(
            crate::middleware::rate_limit_middleware,
        ))
        .layer(axum_middleware::from_fn(
            super::headers::mutation_origin_middleware,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            track_requests,
        ))
        .layer(axum_middleware::from_fn(
            super::headers::theme_error_response,
        ))
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
                let _previous_value = req
                    .extensions_mut()
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
                        path = %request.uri().path().chars().take(256).collect::<String>(),
                    )
                })
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::TRACE),
                )
                .on_failure(|error, latency, span: &tracing::Span| {
                    log_http_failure(&error, latency, span);
                }),
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
