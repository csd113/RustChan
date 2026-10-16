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
/// Maximum request-target bytes, before query decoding or diagnostic logging.
pub(super) const HTTP_MAX_URI_BYTES: usize = 8 * 1024;
/// Default form/body cap; explicit multipart routes retain their larger limits.
pub(super) const HTTP_FORM_MAX_BYTES: usize = 64 * 1024;

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
    let uri_bytes = req
        .uri()
        .path_and_query()
        .map_or(0, |part| part.as_str().len())
        .saturating_add(req.uri().authority().map_or(0, |part| part.as_str().len()))
        .saturating_add(req.uri().scheme_str().map_or(0, str::len));
    if uri_bytes > HTTP_MAX_URI_BYTES {
        return boundary_error_response(http::StatusCode::URI_TOO_LONG, "Request URL is too long");
    }
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

    if !valid_request_host(req.headers()) {
        return boundary_error_response(http::StatusCode::BAD_REQUEST, "Invalid Host header");
    }
    let limit = request_body_limit(req.method(), req.uri().path());
    if req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > limit)
    {
        return boundary_error_response(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "Request body is too large",
        );
    }

    next.run(req).await
}

/// Reject explicit cross-site evidence before any mutation body is consumed.
pub(super) async fn mutation_origin_middleware(
    context: crate::middleware::SecureCookieContext,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if !matches!(
        *req.method(),
        http::Method::GET | http::Method::HEAD | http::Method::OPTIONS
    ) {
        if req
            .headers()
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"))
        {
            return (http::StatusCode::FORBIDDEN, "Cross-site action denied").into_response();
        }
        if req.headers().contains_key(header::ORIGIN) || req.headers().contains_key(header::REFERER)
        {
            let jar = axum_extra::extract::CookieJar::from_headers(req.headers());
            // A Strict public CSRF cookie is unavailable to cross-site forms;
            // preserve supported same-origin browsers that send Origin: null.
            let null_with_cookie = req
                .headers()
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok())
                == Some("null")
                && jar.get("csrf_token").is_some()
                && !req.headers().contains_key(header::REFERER);
            if !null_with_cookie {
                if let Err(error) =
                    crate::handlers::admin::require_same_origin_request(req.headers(), context.peer)
                {
                    return error.into_response();
                }
            }
        }
    }
    next.run(req).await
}

/// Match the explicit multipart overrides without trusting the submitted MIME type.
fn request_body_limit(method: &http::Method, path: &str) -> u64 {
    if *method == http::Method::POST {
        if is_post_upload_path(path) {
            return u64::try_from(crate::handlers::PUBLIC_MULTIPART_REQUEST_MAX_BYTES)
                .unwrap_or(u64::MAX);
        }
        return match path {
            "/admin/restore" | "/admin/board/restore" => 8 * 1024 * 1024 * 1024,
            "/admin/site/favicon" | "/admin/board/favicon" => 5 * 1024 * 1024,
            "/admin/site/banner" | "/admin/home/banner" | "/admin/board/banner" => 8 * 1024 * 1024,
            _ => u64::try_from(HTTP_FORM_MAX_BYTES).unwrap_or(u64::MAX),
        };
    }
    u64::try_from(HTTP_FORM_MAX_BYTES).unwrap_or(u64::MAX)
}

/// Reject duplicate authorities and parser-ambiguous Host values.
fn valid_request_host(headers: &http::HeaderMap) -> bool {
    let mut hosts = headers.get_all(header::HOST).iter();
    let Some(host) = hosts.next() else {
        return true;
    };
    if hosts.next().is_some() {
        return false;
    }
    let Ok(host) = host.to_str() else {
        return false;
    };
    !host.is_empty()
        && !host.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || matches!(byte, b'@' | b'\\' | b',' | b'%' | b'/' | b'#' | b'?')
        })
        && host.parse::<http::uri::Authority>().is_ok_and(|authority| {
            !authority.host().is_empty()
                && host.strip_prefix(authority.host()).is_some_and(|suffix| {
                    suffix.is_empty()
                        || suffix
                            .strip_prefix(':')
                            .is_some_and(|port| port.parse::<u16>().is_ok())
                })
        })
}

/// Render application notices with request preferences before response compression.
pub(super) async fn theme_error_response(
    secure_context: crate::middleware::SecureCookieContext,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use crate::{error::ErrorPage, handlers::board, templates};
    let headers = request.headers().clone();
    let jar = axum_extra::extract::CookieJar::from_headers(&headers);
    let theme = jar
        .get("rustchan_theme")
        .map(|cookie| cookie.value().to_owned());
    let boards = templates::live_boards();
    let board_default = request
        .uri()
        .path()
        .split('/')
        .nth(1)
        .and_then(|short| boards.iter().find(|board| board.short_name == short))
        .map(|board| board.default_theme.as_str());
    let mut response = next.run(request).await;
    if let Some(page) = response.extensions_mut().remove::<ErrorPage>() {
        let preferences = board::user_preferences_from_jar(&jar);
        let (jar, csrf) = board::ensure_csrf_for_request(jar, &headers, secure_context);
        let html = match page {
            ErrorPage::RateLimit => templates::rate_limit_page_with_preferences(
                theme.as_deref(),
                board_default,
                &csrf,
                preferences,
            ),
            ErrorPage::Content { title, body } => templates::base_layout_with_preferences(
                &title,
                None,
                &body,
                &csrf,
                &boards,
                theme.as_deref(),
                board_default,
                false,
                "/",
                preferences,
            ),
            ErrorPage::Message(message) => templates::error_page_with_preferences(
                response.status().as_u16(),
                &message,
                theme.as_deref(),
                board_default,
                &csrf,
                preferences,
            ),
            ErrorPage::Ban { reason, csrf_token } => templates::ban_page_with_theme(
                &reason,
                &csrf_token,
                theme.as_deref(),
                board_default,
            ),
        };
        *response.body_mut() = axum::body::Body::from(html);
        let _previous_content_length = response.headers_mut().remove(header::CONTENT_LENGTH);
        let _previous_value = response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("private, no-store"),
        );
        return (jar, response).into_response();
    }
    response
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
    let _previous_value = response.headers_mut().insert(
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
        let _previous_value = resp.headers_mut().insert(
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
        http::Method::GET | http::Method::HEAD => {
            std::time::Duration::from_secs(CONFIG.operator.read_timeout_secs)
        }
        _ => std::time::Duration::from_secs(CONFIG.operator.write_timeout_secs),
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

    if matches!(
        path.trim_end_matches('/'),
        "/vote"
            | "/report"
            | "/appeal"
            | "/preferences"
            | "/admin"
            | "/setup"
            | "/banned"
            | "/healthz"
            | "/readyz"
            | "/metrics"
    ) {
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
