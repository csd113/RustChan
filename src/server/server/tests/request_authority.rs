//! Standard HTTP/2 authority must reach the shared CSRF guard unambiguously.

use super::normalize_request_host;
use anyhow::{ensure, Result};
use axum::{
    body::Body,
    http::{header, HeaderValue, Request, Version},
};

#[test]
fn http2_authority_supports_same_origin_without_conflicting_host() -> Result<()> {
    let mut request = Request::builder()
        .version(Version::HTTP_2)
        .method("POST")
        .uri("https://example.test:8443/admin/home/banner")
        .header(header::ORIGIN, "https://example.test:8443")
        .body(Body::empty())?;
    ensure!(normalize_request_host(&mut request));
    crate::handlers::admin::require_same_origin_request(
        request.headers(),
        crate::middleware::SecureCookieContext::new(None, true),
    )?;
    ensure!(normalize_request_host(&mut request));
    for host in ["foreign.test:8443", "example.test", "example.test:bad"] {
        drop(
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_str(host)?),
        );
        ensure!(!normalize_request_host(&mut request));
    }
    drop(
        request
            .headers_mut()
            .insert(header::HOST, HeaderValue::from_static("example.test:8443")),
    );
    let _duplicate = request
        .headers_mut()
        .append(header::HOST, HeaderValue::from_static("example.test:8443"));
    ensure!(!normalize_request_host(&mut request));
    Ok(())
}
