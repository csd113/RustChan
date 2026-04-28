// src/server/server/headers.rs

use crate::config::CONFIG;
use axum::{
    http::{self, header},
    response::IntoResponse,
};
use std::net::{IpAddr, SocketAddr};

pub(super) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     script-src 'self'; \
     script-src-elem 'self'; \
     script-src-attr 'none'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data: blob: https://img.youtube.com; \
     media-src 'self' blob:; \
     font-src 'self'; \
     connect-src 'self'; \
     frame-src 'self' https://www.youtube-nocookie.com https://streamable.com; \
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
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::{Path, PathBuf},
    };
    use tower::ServiceExt;

    #[test]
    fn csp_allows_core_end_user_media_features() {
        assert!(CONTENT_SECURITY_POLICY.contains("script-src 'self'"));
        assert!(CONTENT_SECURITY_POLICY.contains("script-src-elem 'self'"));
        assert!(CONTENT_SECURITY_POLICY.contains("script-src-attr 'none'"));
        assert!(
            CONTENT_SECURITY_POLICY.contains("img-src 'self' data: blob: https://img.youtube.com")
        );
        assert!(CONTENT_SECURITY_POLICY.contains("media-src 'self' blob:"));
        assert!(CONTENT_SECURITY_POLICY.contains("connect-src 'self'"));
        assert!(CONTENT_SECURITY_POLICY
            .contains("frame-src 'self' https://www.youtube-nocookie.com https://streamable.com"));
    }

    #[test]
    fn csp_keeps_inline_script_execution_disabled() {
        assert!(!CONTENT_SECURITY_POLICY.contains("script-src 'unsafe-inline'"));
        assert!(CONTENT_SECURITY_POLICY.contains("object-src 'none'"));
        assert!(CONTENT_SECURITY_POLICY.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn served_templates_do_not_embed_inline_script_bodies() {
        for source_path in served_html_source_files() {
            let source = fs::read_to_string(&source_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
            assert!(
                !contains_inline_script_body(&source),
                "served HTML source reintroduced an inline <script> body: {}",
                source_path.display()
            );
        }
    }

    fn served_html_source_files() -> Vec<PathBuf> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for relative_dir in ["src/templates", "src/middleware", "src/handlers"] {
            collect_rust_files(&repo_root.join(relative_dir), &mut files);
        }
        files.sort();
        files
    }

    fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
        let entries =
            fs::read_dir(dir).unwrap_or_else(|error| panic!("read dir {}: {error}", dir.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("read entry under {}: {error}", dir.display()));
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    fn contains_inline_script_body(source: &str) -> bool {
        let mut search_from = 0;
        let script_open = "<script";
        let script_close = "</script>";

        while let Some(relative_open) = source[search_from..].find(script_open) {
            let open = search_from + relative_open;
            let after_open = &source[open..];
            let Some(tag_end_relative) = after_open.find('>') else {
                break;
            };
            let tag_end = open + tag_end_relative;
            let tag = &source[open..=tag_end];
            let body_start = tag_end + 1;

            let Some(close_relative) = source[body_start..].find(script_close) else {
                break;
            };
            let body_end = body_start + close_relative;
            let body = source[body_start..body_end].trim();

            if !tag.contains("src=") && !body.is_empty() {
                return true;
            }

            search_from = body_end + script_close.len();
        }

        false
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
