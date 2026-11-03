//! Regressions for confirmed CSRF provenance, token, and login-handoff failures.

use super::{
    consume_admin_session_bootstrap, create_admin_session_bootstrap,
    require_admin_post_origin_and_csrf, require_same_origin_request, ADMIN_SESSION_BOOTSTRAPS,
    SESSION_COOKIE,
};
use crate::{config::CONFIG, middleware::SecureCookieContext, utils::crypto};
use anyhow::{ensure, Result};
use axum::http::{header, HeaderMap, HeaderValue};
use axum_extra::extract::cookie::{Cookie, CookieJar};

fn headers(host: &'static str, origin: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    drop(headers.insert(header::HOST, HeaderValue::from_static(host)));
    drop(headers.insert(header::ORIGIN, HeaderValue::from_static(origin)));
    headers
}

#[test]
fn malformed_and_conflicting_provenance_is_rejected() -> Result<()> {
    let context = SecureCookieContext::default();
    for origin in [
        "",
        "http://example.test:99999",
        "http://example.test:bad",
        "http://example.test/path",
        "http://example.test?query=1",
        "http://user@example.test",
        "https://example.test",
        "http://foreign.test",
    ] {
        ensure!(
            require_same_origin_request(&headers("example.test", origin), context).is_err(),
            "invalid or foreign origin accepted: {origin}"
        );
    }
    for name in [
        header::ORIGIN,
        header::REFERER,
        header::HeaderName::from_static("sec-fetch-site"),
    ] {
        let mut request = headers("example.test", "http://example.test");
        drop(request.remove(&name));
        let _appended = request.append(
            name.clone(),
            HeaderValue::from_static("http://example.test"),
        );
        let _second_value = request.append(name, HeaderValue::from_static("http://foreign.test"));
        ensure!(require_same_origin_request(&request, context).is_err());
    }
    let mut request = headers("example.test", "http://example.test");
    drop(request.insert(
        header::REFERER,
        HeaderValue::from_static("http://foreign.test/form"),
    ));
    ensure!(require_same_origin_request(&request, context).is_err());
    drop(request.insert(header::ORIGIN, HeaderValue::from_bytes(b"\xff")?));
    ensure!(require_same_origin_request(&request, context).is_err());
    Ok(())
}

#[test]
fn origin_uses_actual_transport_and_strict_host_identity() -> Result<()> {
    let http = SecureCookieContext::default();
    let https = SecureCookieContext::new(None, true);
    let tls_request = headers("example.test", "https://example.test");
    ensure!(require_same_origin_request(&tls_request, http).is_err());
    require_same_origin_request(&tls_request, https)?;
    ensure!(
        require_same_origin_request(&headers("localhost:8080", "http://127.0.0.1:8080"), http)
            .is_err()
    );
    let mut request = headers("localhost:8080", "null");
    drop(request.insert(
        header::REFERER,
        HeaderValue::from_static("http://foreign.test/form"),
    ));
    ensure!(require_same_origin_request(&request, http).is_err());
    drop(request.remove(header::REFERER));
    ensure!(require_same_origin_request(&request, http).is_err());
    drop(request.insert("sec-fetch-site", HeaderValue::from_static("same-origin")));
    require_same_origin_request(&request, http)?;
    drop(request.insert("sec-fetch-site", HeaderValue::from_static("cross-site")));
    ensure!(require_same_origin_request(&request, http).is_err());
    let mut future_metadata = headers("example.test", "http://example.test");
    drop(future_metadata.insert("sec-fetch-site", HeaderValue::from_static("future-value")));
    require_same_origin_request(&future_metadata, http)?;
    drop(future_metadata.remove(header::ORIGIN));
    ensure!(require_same_origin_request(&future_metadata, http).is_err());
    Ok(())
}

#[test]
fn public_signatures_and_admin_session_binding_are_mandatory() -> Result<()> {
    let raw = "synthetic-cookie";
    let public = crypto::make_csrf_form_token(raw, &CONFIG.cookie_secret);
    ensure!(!crate::middleware::validate_csrf(Some(raw), raw));
    ensure!(crate::middleware::validate_csrf(Some(raw), &public));
    ensure!(!crate::middleware::validate_csrf(
        Some("another-cookie"),
        &public
    ));
    let scoped = crypto::make_scoped_csrf_form_token(raw, &CONFIG.cookie_secret, "session-a");
    let jar = CookieJar::new()
        .add(Cookie::new("csrf_token", raw))
        .add(Cookie::new(SESSION_COOKIE, "session-a"));
    let context = SecureCookieContext::default();
    let request = headers("example.test", "http://example.test");
    require_admin_post_origin_and_csrf(&jar, &request, context, Some(&scoped))?;
    for token in [None, Some(raw), Some(public.as_str())] {
        ensure!(require_admin_post_origin_and_csrf(&jar, &request, context, token).is_err());
    }
    let changed_session = jar.add(Cookie::new(SESSION_COOKIE, "session-b"));
    ensure!(
        require_admin_post_origin_and_csrf(&changed_session, &request, context, Some(&scoped))
            .is_err()
    );
    Ok(())
}

#[test]
fn login_handoff_requires_its_browser_and_is_single_use() -> Result<()> {
    let token = create_admin_session_bootstrap("synthetic-session", "browser-a");
    ensure!(consume_admin_session_bootstrap(&token, None).is_none());
    ensure!(consume_admin_session_bootstrap(&token, Some("browser-b")).is_none());
    ensure!(
        consume_admin_session_bootstrap(&token, Some("browser-a")).as_deref()
            == Some("synthetic-session")
    );
    ensure!(consume_admin_session_bootstrap(&token, Some("browser-a")).is_none());
    let expired = create_admin_session_bootstrap("synthetic-session", "browser-a");
    if let Some(mut entry) = ADMIN_SESSION_BOOTSTRAPS.get_mut(&expired) {
        entry.expires_at = 0;
    }
    ensure!(consume_admin_session_bootstrap(&expired, Some("browser-a")).is_none());
    Ok(())
}
