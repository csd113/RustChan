//! Request-boundary, security-header, caching, timeout, and compression policy.

use crate::config::CONFIG;
use axum::{
    http::{self, header},
    response::IntoResponse as _,
};
use std::net::{IpAddr, SocketAddr};

/// Maximum aggregate request-header bytes accepted by the service boundary.
pub(super) const HTTP_MAX_HEADER_BYTES: usize = 64 * 1024;
/// Maximum bytes accepted in one request-header value.
pub(super) const HTTP_MAX_HEADER_VALUE_BYTES: usize = 32 * 1024;

/// Content Security Policy applied to HTML responses.
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

/// Reject ambiguous framing and oversized request headers.
pub(super) async fn request_boundary_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request_headers_exceed_limits(req.headers()) {
        return boundary_error_response(
            http::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "Request headers are too large",
        );
    }

    // Hyper follows RFC 7230 by discarding Content-Length when it appears
    // after Transfer-Encoding. That makes a TE+CL request indistinguishable
    // from a transfer-coded request at the service boundary. RustChan does not
    // require streaming request transfer codings, so reject all of them rather
    // than allow ambiguous framing to reach a handler.
    if req.headers().contains_key(header::TRANSFER_ENCODING) {
        return boundary_error_response(
            http::StatusCode::BAD_REQUEST,
            "Transfer-Encoding is not accepted",
        );
    }

    next.run(req).await
}

/// Return whether a header map exceeds per-value or aggregate limits.
fn request_headers_exceed_limits(headers: &http::HeaderMap) -> bool {
    let mut total = 0usize;
    for (name, value) in headers {
        if value.as_bytes().len() > HTTP_MAX_HEADER_VALUE_BYTES {
            return true;
        }
        let Some(next_total) = total
            .checked_add(name.as_str().len())
            .and_then(|size| size.checked_add(value.as_bytes().len()))
            // Match HTTP/2's header-list accounting and leave room for HTTP/1
            // separators and the request line under the protocol-level cap.
            .and_then(|size| size.checked_add(32))
        else {
            return true;
        };
        total = next_total;
        if total > HTTP_MAX_HEADER_BYTES {
            return true;
        }
    }
    false
}

/// Build a connection-closing request-boundary error.
fn boundary_error_response(
    status: http::StatusCode,
    message: &'static str,
) -> axum::response::Response {
    let mut response = (status, message).into_response();
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    response
}

/// Add HSTS only for eligible public HTTPS origins.
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

/// Apply route-sensitive request timeouts.
pub(super) async fn safe_timeout_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path();
    let is_post_upload_route =
        matches!(*req.method(), http::Method::POST) && is_post_upload_path(path);
    let bypass_timeout = is_post_upload_route
        || path.starts_with("/admin/backup/download/")
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

    let timeout = match *req.method() {
        http::Method::GET | http::Method::HEAD => std::time::Duration::from_secs(30),
        _ => std::time::Duration::from_mins(5),
    };

    tokio::time::timeout(timeout, next.run(req))
        .await
        .unwrap_or_else(|_| {
            (http::StatusCode::REQUEST_TIMEOUT, "Request timed out").into_response()
        })
}

/// Apply public dynamic-page caching when a route did not set policy.
pub(super) async fn public_cache_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();
    let mut resp = next.run(req).await;
    if public_dynamic_html_path(&path) {
        crate::cache::insert_cache_control_if_absent(
            resp.headers_mut(),
            crate::cache::CACHE_CONTROL_DYNAMIC_PUBLIC,
        );
    }
    resp
}

/// Apply private cache policy to administrator responses.
pub(super) async fn admin_cache_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();
    let mut resp = next.run(req).await;
    let cache_control = if sensitive_admin_html_path(&path)
        && response_content_type_starts_with(resp.headers(), "text/html")
    {
        crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE
    } else {
        crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE
    };
    crate::cache::insert_cache_control_if_absent(resp.headers_mut(), cache_control);
    resp
}

/// Return whether a response is safe and useful to compress.
pub(super) fn text_response_compression_predicate(
    status: http::StatusCode,
    _version: http::Version,
    headers: &http::HeaderMap,
    _extensions: &http::Extensions,
) -> bool {
    status == http::StatusCode::OK
        && !headers.contains_key(header::CONTENT_ENCODING)
        && !headers.contains_key(header::CONTENT_RANGE)
        && !headers.contains_key(header::ACCEPT_RANGES)
        && !has_attachment_disposition(headers)
        && response_content_type_is_compressible(headers)
}

/// Return whether a path serves public dynamic HTML.
fn public_dynamic_html_path(path: &str) -> bool {
    if path == "/" || path == "/banned" || path.starts_with("/banner/external/") {
        return true;
    }
    let mut segments = path.trim_matches('/').split('/');
    let (first, second, third, fourth) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    );
    if matches!(
        first,
        Some("api" | "boards" | "static" | "theme-css" | "banner")
    ) {
        return false;
    }
    second.is_none()
        || matches!(
            (second, third, fourth),
            (
                Some("catalog" | "hidden" | "archive" | "search" | "unlock"),
                None,
                None
            ) | (Some("thread"), Some(_), None)
                | (Some("post"), Some(_), Some("edit" | "delete"))
        )
}

/// Return whether an administrator path carries sensitive HTML.
fn sensitive_admin_html_path(path: &str) -> bool {
    if matches!(
        path,
        "/admin"
            | "/admin/panel"
            | "/admin/mod-log"
            | "/admin/backup"
            | "/admin/backup/create"
            | "/admin/backup/delete"
            | "/admin/backup/extract-board"
            | "/admin/backup/restore-saved"
            | "/admin/backup/settings"
            | "/admin/board/backup/create"
            | "/admin/board/backup/restore-saved"
            | "/admin/board/restore"
            | "/admin/db/check"
            | "/admin/db/repair"
            | "/admin/db/repair/status"
            | "/admin/restore"
            | "/admin/vacuum"
    ) {
        return true;
    }

    path.strip_prefix("/admin/ip/")
        .is_some_and(|tail| !tail.is_empty())
}

/// Match a response Content-Type prefix after leading whitespace.
fn response_content_type_starts_with(headers: &http::HeaderMap, prefix: &str) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with(prefix))
}

/// Return whether a response is an attachment download.
fn has_attachment_disposition(headers: &http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("attachment"))
}

/// Return whether a response media type is text-compressible.
fn response_content_type_is_compressible(headers: &http::HeaderMap) -> bool {
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase)
    else {
        return false;
    };

    matches!(
        content_type.as_str(),
        "text/html"
            | "text/css"
            | "text/javascript"
            | "application/javascript"
            | "application/json"
            | "application/xml"
            | "text/xml"
            | "image/svg+xml"
    )
}

/// Return whether a path identifies a post-creation upload route.
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

/// Decide whether an HSTS header is safe for this request origin.
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

/// Parse the Host header into host and optional port.
fn request_host_parts(headers: &http::HeaderMap) -> Option<(String, Option<u16>)> {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<http::uri::Authority>().ok())
        .map(|authority| (authority.host().to_owned(), authority.port_u16()))
}

/// Match a host against configured public and ACME names.
fn host_is_configured_public_host(host: &str) -> bool {
    CONFIG
        .public_hosts
        .iter()
        .chain(CONFIG.tls.acme.domains.iter())
        .filter_map(|candidate| crate::config::normalize_public_host(candidate))
        .any(|candidate| candidate.eq_ignore_ascii_case(host))
}

/// Return whether a host is localhost or a loopback IP literal.
fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
/// HTTP boundary and response-header policy tests.
mod tests {
    use super::{
        hsts_middleware_with_mode, public_dynamic_html_path, sensitive_admin_html_path,
        should_emit_hsts, CONTENT_SECURITY_POLICY, HTTP_MAX_HEADER_BYTES,
        HTTP_MAX_HEADER_VALUE_BYTES,
    };
    use axum::{
        body::Body,
        http::{header, HeaderValue, Request, StatusCode},
        middleware::from_fn,
        response::IntoResponse as _,
        routing::{get, post},
        Router,
    };
    use std::{
        fs,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        path::{Path, PathBuf},
    };
    use tower::ServiceExt as _;

    /// Build a router protected by the shared request boundary.
    fn request_boundary_app() -> Router {
        Router::new()
            .route("/", post(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn(super::request_boundary_middleware))
    }

    /// Standard fallible test result.
    type TestResult = anyhow::Result<()>;

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects a request carrying both transfer encoding and content length.
    async fn request_boundary_rejects_transfer_encoding_with_content_length() -> TestResult {
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::TRANSFER_ENCODING, "chunked")
            .header(header::CONTENT_LENGTH, "4")
            .body(Body::from("test"))?;
        let response = request_boundary_app().oneshot(request).await?;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "ambiguous request framing should be rejected"
        );
        assert_eq!(
            response.headers().get(header::CONNECTION),
            Some(&HeaderValue::from_static("close")),
            "boundary errors should close the connection"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects transfer encoding even without content length.
    async fn request_boundary_rejects_transfer_encoding_without_content_length() -> TestResult {
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::TRANSFER_ENCODING, "chunked")
            .body(Body::empty())?;
        let response = request_boundary_app().oneshot(request).await?;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "streaming transfer encoding should be rejected"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Accepts a bounded request with content length.
    async fn request_boundary_accepts_bounded_content_length_request() -> TestResult {
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_LENGTH, "4")
            .body(Body::from("test"))?;
        let response = request_boundary_app().oneshot(request).await?;

        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "a bounded content-length request should pass"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects one header value above its byte limit.
    async fn request_boundary_rejects_oversized_single_header_value() -> TestResult {
        let value = HeaderValue::from_bytes(&vec![b'a'; HTTP_MAX_HEADER_VALUE_BYTES + 1])?;
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("x-large", value)
            .body(Body::empty())?;
        let response = request_boundary_app().oneshot(request).await?;

        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "an oversized header value should be rejected"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Accepts exact per-value and aggregate header limits.
    async fn request_boundary_accepts_exact_value_and_aggregate_limits() -> TestResult {
        let exact_value = HeaderValue::from_bytes(&vec![b'a'; HTTP_MAX_HEADER_VALUE_BYTES])?;
        let exact_value_request = Request::builder()
            .method("POST")
            .uri("/")
            .header("x-boundary", exact_value)
            .body(Body::empty())?;
        let exact_value_response = request_boundary_app()
            .clone()
            .oneshot(exact_value_request)
            .await?;
        assert_eq!(
            exact_value_response.status(),
            StatusCode::NO_CONTENT,
            "a header at the per-value limit should pass"
        );

        // Header-list accounting is name + value + 32 bytes per field.
        let first_value = HeaderValue::from_bytes(&vec![b'a'; HTTP_MAX_HEADER_VALUE_BYTES])?;
        let aggregate_overhead = "x-a".len() + "x-b".len() + (2 * 32);
        let second_value_len =
            HTTP_MAX_HEADER_BYTES - HTTP_MAX_HEADER_VALUE_BYTES - aggregate_overhead;
        let second_value = HeaderValue::from_bytes(&vec![b'b'; second_value_len])?;
        let exact_aggregate_request = Request::builder()
            .method("POST")
            .uri("/")
            .header("x-a", first_value)
            .header("x-b", second_value)
            .body(Body::empty())?;
        let exact_aggregate_response = request_boundary_app()
            .oneshot(exact_aggregate_request)
            .await?;
        assert_eq!(
            exact_aggregate_response.status(),
            StatusCode::NO_CONTENT,
            "headers at the aggregate limit should pass"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects header maps above the aggregate byte limit.
    async fn request_boundary_rejects_oversized_aggregate_headers() -> TestResult {
        let value = HeaderValue::from_bytes(&vec![b'a'; HTTP_MAX_HEADER_VALUE_BYTES])?;
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("x-large-one", value.clone())
            .header("x-large-two", value)
            .body(Body::empty())?;
        let response = request_boundary_app().oneshot(request).await?;

        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers above the aggregate limit should be rejected"
        );
        assert_eq!(
            HTTP_MAX_HEADER_BYTES,
            64 * 1024,
            "aggregate header limit should remain 64 KiB"
        );
        Ok(())
    }

    #[test]
    /// Allows scripts and end-user media required by core features.
    fn csp_allows_core_end_user_media_features() {
        for directive in [
            "script-src 'self'",
            "script-src-elem 'self'",
            "script-src-attr 'none'",
            "img-src 'self' data: blob: https://img.youtube.com",
            "media-src 'self' blob:",
            "connect-src 'self'",
            "frame-src 'self' https://www.youtube-nocookie.com https://streamable.com",
        ] {
            assert!(
                CONTENT_SECURITY_POLICY.contains(directive),
                "CSP should contain {directive:?}"
            );
        }
    }

    #[test]
    /// Keeps inline scripts, objects, and framing disabled.
    fn csp_keeps_inline_script_execution_disabled() {
        assert!(
            !CONTENT_SECURITY_POLICY.contains("script-src 'unsafe-inline'"),
            "CSP must not permit inline scripts"
        );
        for directive in ["object-src 'none'", "frame-ancestors 'none'"] {
            assert!(
                CONTENT_SECURITY_POLICY.contains(directive),
                "CSP should contain {directive:?}"
            );
        }
    }

    #[test]
    /// Limits public dynamic caching to HTML routes.
    fn public_dynamic_html_cache_middleware_scope_is_narrow() {
        for path in ["/", "/b/catalog", "/b/thread/1", "/b/post/1/edit"] {
            assert!(
                public_dynamic_html_path(path),
                "{path} should receive dynamic public caching"
            );
        }
        for path in [
            "/static/style.css",
            "/boards/b/file.webp",
            "/api/post/b/1",
            "/banner/assets/1",
        ] {
            assert!(
                !public_dynamic_html_path(path),
                "{path} should not receive dynamic public caching"
            );
        }
    }

    #[test]
    /// Applies no-store only to sensitive administrator HTML.
    fn sensitive_admin_html_paths_get_no_store_policy() {
        for path in [
            "/admin",
            "/admin/panel",
            "/admin/mod-log",
            "/admin/backup",
            "/admin/backup/create",
            "/admin/backup/delete",
            "/admin/backup/extract-board",
            "/admin/backup/restore-saved",
            "/admin/backup/settings",
            "/admin/board/backup/create",
            "/admin/board/backup/restore-saved",
            "/admin/board/restore",
            "/admin/db/check",
            "/admin/db/repair",
            "/admin/db/repair/status",
            "/admin/restore",
            "/admin/vacuum",
            "/admin/ip/abcdef123456",
        ] {
            assert!(sensitive_admin_html_path(path), "{path} should be no-store");
        }

        for path in [
            "/admin/backup/progress",
            "/admin/backup/download/full/site.zip",
            "/admin/ip/",
        ] {
            assert!(
                !sensitive_admin_html_path(path),
                "{path} should not receive no-store"
            );
        }
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Uses no-store for sensitive HTML and no-cache for JSON polling.
    async fn admin_cache_middleware_no_store_for_sensitive_html_only() -> TestResult {
        let app = Router::new()
            .route(
                "/admin/backup",
                get(|| async { ([("content-type", "text/html; charset=utf-8")], "ok") }),
            )
            .route(
                "/admin/backup/progress",
                get(|| async { ([("content-type", "application/json")], "{}") }),
            )
            .layer(from_fn(super::admin_cache_middleware));

        let html_request = Request::builder()
            .uri("/admin/backup")
            .body(Body::empty())?;
        let html_response = app.clone().oneshot(html_request).await?;
        assert_eq!(
            html_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
            "sensitive administrator HTML should be no-store"
        );

        let json_request = Request::builder()
            .uri("/admin/backup/progress")
            .body(Body::empty())?;
        let json_response = app.oneshot(json_request).await?;
        assert_eq!(
            json_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE),
            "administrator JSON polling should be private no-cache"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Preserves a cache policy set by the route itself.
    async fn cache_middleware_does_not_overwrite_route_cache_control() -> TestResult {
        let app = Router::new()
            .route(
                "/",
                get(|| async {
                    (
                        [(
                            header::CACHE_CONTROL,
                            crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA,
                        )],
                        "ok",
                    )
                }),
            )
            .layer(from_fn(super::public_cache_middleware));

        let request = Request::builder().uri("/").body(Body::empty())?;
        let response = app.oneshot(request).await?;

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA),
            "middleware should preserve route-specific caching"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Rejects inline script bodies in all served HTML source files.
    fn served_templates_do_not_embed_inline_script_bodies() -> TestResult {
        for source_path in served_html_source_files()? {
            let source = fs::read_to_string(&source_path)?;
            assert!(
                !contains_inline_script_body(&source),
                "served HTML source reintroduced an inline <script> body: {}",
                source_path.display()
            );
        }
        Ok(())
    }

    /// Collect Rust sources that can render served HTML.
    fn served_html_source_files() -> std::io::Result<Vec<PathBuf>> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for relative_dir in ["src/templates", "src/middleware", "src/handlers"] {
            collect_rust_files(&repo_root.join(relative_dir), &mut files)?;
        }
        files.sort();
        Ok(files)
    }

    /// Recursively collect Rust files below one directory.
    fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let entries = fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, files)?;
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
        Ok(())
    }

    /// Return whether source contains a non-external script body.
    fn contains_inline_script_body(source: &str) -> bool {
        let mut search_from = 0;
        let script_open = "<script";
        let script_close = "</script>";

        let Some(mut search_tail) = source.get(search_from..) else {
            return false;
        };
        while let Some(relative_open) = search_tail.find(script_open) {
            let open = search_from + relative_open;
            let Some(after_open) = source.get(open..) else {
                break;
            };
            let Some(tag_end_relative) = after_open.find('>') else {
                break;
            };
            let tag_end = open + tag_end_relative;
            let Some(tag) = source.get(open..=tag_end) else {
                break;
            };
            let body_start = tag_end + 1;

            let Some(body_tail) = source.get(body_start..) else {
                break;
            };
            let Some(close_relative) = body_tail.find(script_close) else {
                break;
            };
            let body_end = body_start + close_relative;
            let Some(body) = source.get(body_start..body_end).map(str::trim) else {
                break;
            };

            if !tag.contains("src=") && !body.is_empty() {
                return true;
            }

            search_from = body_end + script_close.len();
            let Some(next_tail) = source.get(search_from..) else {
                break;
            };
            search_tail = next_tail;
        }

        false
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Does not add HSTS for a loopback direct-HTTPS host.
    async fn hsts_is_not_added_for_loopback_direct_https_hosts() -> TestResult {
        let app = Router::new()
            .route("/", get(|| async { "ok".into_response() }))
            .layer(from_fn(|req, next| {
                hsts_middleware_with_mode(req, next, true, false)
            }));

        let request = Request::builder()
            .uri("/")
            .header("host", "localhost:8443")
            .body(Body::empty())?;
        let response = app.oneshot(request).await?;

        assert!(
            !response.headers().contains_key("strict-transport-security"),
            "loopback origins must not receive HSTS"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Does not add HSTS for a public nonstandard HTTPS port.
    fn hsts_is_not_added_for_public_nonstandard_https_ports() -> TestResult {
        let request = Request::builder()
            .uri("/")
            .header("host", "example.test:8443")
            .body(Body::empty())?;

        assert!(
            !should_emit_hsts(&request, None, true, false),
            "nonstandard public HTTPS ports must not receive HSTS"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Does not add HSTS for an unconfigured proxy tunnel host.
    fn hsts_is_not_added_for_unconfigured_proxy_tunnel_hosts() -> TestResult {
        let request = Request::builder()
            .uri("/")
            .header("host", "demo.serveo.net")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())?;

        let peer = Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080));
        assert!(
            !should_emit_hsts(&request, peer, false, true),
            "unconfigured proxy tunnel hosts must not receive HSTS"
        );
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Ignores a spoofed forwarded protocol from a public peer.
    async fn hsts_ignores_spoofed_forwarded_proto_from_public_peer() -> TestResult {
        use axum::extract::ConnectInfo;

        let app = Router::new()
            .route("/", get(|| async { "ok".into_response() }))
            .layer(from_fn(|req, next| {
                hsts_middleware_with_mode(req, next, false, true)
            }));

        let mut request = Request::builder()
            .uri("/")
            .header("x-forwarded-proto", "https")
            .body(Body::empty())?;
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
            8080,
        )));

        let response = app.oneshot(request).await?;
        assert!(
            !response.headers().contains_key("strict-transport-security"),
            "untrusted forwarded protocol must not trigger HSTS"
        );
        Ok(())
    }
}
