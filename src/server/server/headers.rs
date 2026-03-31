// src/server/server/headers.rs

use axum::{
    http::{self, header},
    response::IntoResponse,
};

pub(super) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob: https://img.youtube.com; \
     media-src 'self' blob:; \
     font-src 'self'; \
     connect-src 'self'; \
     frame-src https://www.youtube-nocookie.com https://streamable.com; \
     frame-ancestors 'none'; \
     object-src 'none'; \
     base-uri 'self'";

pub(super) async fn hsts_middleware_with_mode(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    direct_https: bool,
) -> axum::response::Response {
    let is_https = direct_https
        || req.uri().scheme_str() == Some("https")
        || req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"));

    let mut resp = next.run(req).await;
    if is_https {
        resp.headers_mut().insert(
            header::HeaderName::from_static("strict-transport-security"),
            header::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    resp
}

pub(super) async fn safe_timeout_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    if !matches!(method, http::Method::GET | http::Method::HEAD) {
        return next.run(req).await;
    }

    tokio::time::timeout(std::time::Duration::from_secs(30), next.run(req))
        .await
        .unwrap_or_else(|_| {
            (http::StatusCode::REQUEST_TIMEOUT, "Request timed out").into_response()
        })
}

#[cfg(test)]
mod tests {
    use super::{hsts_middleware_with_mode, CONTENT_SECURITY_POLICY};
    use axum::{
        body::Body, http::Request, middleware::from_fn, response::IntoResponse, routing::get,
        Router,
    };
    use tower::ServiceExt;

    #[test]
    fn csp_allows_core_end_user_media_features() {
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self'"));
        assert!(
            CONTENT_SECURITY_POLICY.contains("img-src 'self' data: blob: https://img.youtube.com")
        );
        assert!(CONTENT_SECURITY_POLICY.contains("media-src 'self' blob:"));
        assert!(CONTENT_SECURITY_POLICY.contains("connect-src 'self'"));
        assert!(CONTENT_SECURITY_POLICY
            .contains("frame-src https://www.youtube-nocookie.com https://streamable.com"));
    }

    #[test]
    fn csp_keeps_inline_script_execution_disabled() {
        assert!(!CONTENT_SECURITY_POLICY.contains("script-src 'unsafe-inline'"));
        assert!(CONTENT_SECURITY_POLICY.contains("object-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
    }

    #[tokio::test]
    async fn hsts_is_added_for_direct_https_without_forwarded_proto() {
        let app = Router::new()
            .route("/", get(|| async { "ok".into_response() }))
            .layer(from_fn(|req, next| {
                hsts_middleware_with_mode(req, next, true)
            }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(response.headers().contains_key("strict-transport-security"));
    }
}
