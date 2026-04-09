// src/server/server/headers.rs

use crate::config::CONFIG;
use axum::{
    http::{self, header},
    response::IntoResponse,
};
use std::net::{IpAddr, SocketAddr};

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
    behind_proxy: bool,
) -> axum::response::Response {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0);
    let emit_hsts = should_emit_hsts(&req, peer, direct_https, behind_proxy);

    let mut resp = next.run(req).await;
    if emit_hsts {
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
    let path = req.uri().path();
    let is_post_upload_route =
        matches!(*req.method(), http::Method::POST) && is_post_upload_path(path);
    let bypass_timeout = path.starts_with("/admin/backup/download/")
        || matches!(
            path,
            "/admin/backup"
                | "/admin/backup/create"
                | "/admin/board/backup/create"
                | "/admin/restore"
                | "/admin/backup/restore-saved"
                | "/admin/board/restore"
                | "/admin/board/backup/restore-saved"
                | "/admin/vacuum"
                | "/admin/db/check"
                | "/admin/db/repair"
        );
    if bypass_timeout {
        return next.run(req).await;
    }

    let timeout = if is_post_upload_route {
        std::time::Duration::from_secs(900)
    } else {
        match *req.method() {
            http::Method::GET | http::Method::HEAD => std::time::Duration::from_secs(30),
            _ => std::time::Duration::from_secs(300),
        }
    };

    tokio::time::timeout(timeout, next.run(req))
        .await
        .unwrap_or_else(|_| {
            (http::StatusCode::REQUEST_TIMEOUT, "Request timed out").into_response()
        })
}

fn is_post_upload_path(path: &str) -> bool {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return false;
    }

    let mut segments = trimmed.split('/');
    matches!(
        (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ),
        (Some(_), None, None, None) | (Some(_), Some("thread"), Some(_), None)
    )
}

fn should_emit_hsts(
    req: &axum::extract::Request,
    peer: Option<SocketAddr>,
    direct_https: bool,
    behind_proxy: bool,
) -> bool {
    let is_https = direct_https
        || req.uri().scheme_str() == Some("https")
        || crate::middleware::forwarded_proto_is_https(req.headers(), peer, behind_proxy);

    let Some((host, port)) = request_host_parts(req.headers()) else {
        return false;
    };

    if !is_https || is_loopback_host(&host) {
        return false;
    }

    if port.is_some_and(|port| port != 443) {
        return false;
    }

    if behind_proxy {
        return host_is_configured_public_host(&host);
    }

    CONFIG.tls.port == 443
}

fn request_host_parts(headers: &http::HeaderMap) -> Option<(String, Option<u16>)> {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<http::uri::Authority>().ok())
        .map(|authority| (authority.host().to_string(), authority.port_u16()))
}

fn host_is_configured_public_host(host: &str) -> bool {
    CONFIG
        .public_hosts
        .iter()
        .chain(CONFIG.tls.acme.domains.iter())
        .filter_map(|candidate| crate::config::normalize_public_host(candidate))
        .any(|candidate| candidate.eq_ignore_ascii_case(host))
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::{hsts_middleware_with_mode, should_emit_hsts, CONTENT_SECURITY_POLICY};
    use axum::{
        body::Body, http::Request, middleware::from_fn, response::IntoResponse, routing::get,
        Router,
    };
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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
    async fn hsts_is_not_added_for_loopback_direct_https_hosts() {
        let app = Router::new()
            .route("/", get(|| async { "ok".into_response() }))
            .layer(from_fn(|req, next| {
                hsts_middleware_with_mode(req, next, true, false)
            }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("host", "localhost:8443")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert!(!response.headers().contains_key("strict-transport-security"));
    }

    #[test]
    fn hsts_is_not_added_for_public_nonstandard_https_ports() {
        let request = Request::builder()
            .uri("/")
            .header("host", "example.test:8443")
            .body(Body::empty())
            .expect("request");

        assert!(!should_emit_hsts(&request, None, true, false));
    }

    #[test]
    fn hsts_is_not_added_for_unconfigured_proxy_tunnel_hosts() {
        let request = Request::builder()
            .uri("/")
            .header("host", "demo.serveo.net")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("request");

        let peer = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080));
        assert!(!should_emit_hsts(&request, peer, false, true));
    }

    #[tokio::test]
    async fn hsts_ignores_spoofed_forwarded_proto_from_public_peer() {
        use axum::extract::ConnectInfo;

        let app = Router::new()
            .route("/", get(|| async { "ok".into_response() }))
            .layer(from_fn(|req, next| {
                hsts_middleware_with_mode(req, next, false, true)
            }));

        let mut request = Request::builder()
            .uri("/")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
            8080,
        )));

        let response = app.oneshot(request).await.expect("response");
        assert!(!response.headers().contains_key("strict-transport-security"));
    }
}
