use axum::{
    extract::DefaultBodyLimit, http::header, middleware as axum_middleware, routing::get, Router,
};

use crate::config::CONFIG;
use crate::middleware::AppState;

#[path = "routes.rs"]
mod routes;

use super::{
    assets::{serve_css, serve_main_js, serve_theme_init_js},
    headers::{hsts_middleware, safe_timeout_middleware, CONTENT_SECURITY_POLICY},
    lifecycle::track_requests,
    onion_location_middleware,
};
use routes::{admin_routes, public_routes};

pub(super) fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .merge(public_routes())
        .merge(admin_routes())
        .layer(DefaultBodyLimit::max(CONFIG.max_video_size))
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
