//
// Admin panel. All routes require a valid session cookie.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. All /admin/* routes → check session cookie → get session from DB → proceed
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Session cookie: HTTPOnly (not readable by JS), SameSite=Lax for top-level
// login redirects. Same-origin and CSRF checks protect admin mutations.
// Secure=true when CHAN_HTTPS_COOKIES=true (default: enabled for proxy or direct TLS).
//
// + All admin handlers now wrap DB and file I/O in
// spawn_blocking to avoid blocking the Tokio event loop. Direct DB calls from
// async context were stalling worker threads under concurrent load.

mod accounts;
mod logs;
mod panel;

pub(in crate::server) use logs::admin_live_log;
#[expect(
    unused_imports,
    reason = "preserve the existing query type facade even though route consumers infer it"
)]
pub(in crate::server) use logs::LiveLogQuery;
pub(in crate::server) use panel::{
    admin_panel, admin_site_health_jobs, dismiss_failed_site_health_jobs,
};
#[expect(
    unused_imports,
    reason = "preserve the existing query and form type facade even though route consumers infer them"
)]
pub(in crate::server) use panel::{AdminPanelQuery, DismissFailedJobsForm};
pub(in crate::server) mod auth;
/// Explicit administrator settings restarts.
pub(in crate::server) mod restart;
/// Administrator-only software update handlers.
pub(in crate::server) mod updates;
pub(in crate::server) use accounts::*;
pub(in crate::server) use auth::*;

pub(in crate::server) mod backup;
pub(in crate::server) use backup::*;

pub(in crate::server) mod content;
pub(in crate::server) use content::*;

pub(in crate::server) mod moderation;
pub(in crate::server) use moderation::*;

pub(in crate::server) mod settings;
pub(in crate::server) use settings::*;

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::validate_signed_csrf,
    utils::crypto::{make_scoped_csrf_form_token, new_csrf_token},
};
use axum::{
    http::{header, HeaderMap, Uri},
    response::Redirect,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use serde::Deserialize;
use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

// Shared constant
/// Session cookie used by this handler.
const SESSION_COOKIE: &str = "chan_admin_session";
/// Admin cookie same site used by this handler.
const ADMIN_COOKIE_SAME_SITE: SameSite = SameSite::Lax;
/// Admin bootstrap TTL secs used by this handler.
const ADMIN_BOOTSTRAP_TTL_SECS: u64 = 120;
/// Missing origin referer used by this handler.
const MISSING_ORIGIN_REFERER: &str = "Missing Origin/Referer header.";

static ADMIN_SESSION_BOOTSTRAPS: LazyLock<DashMap<String, (String, u64)>> =
    LazyLock::new(DashMap::new);

// Shared form type used by auth and backup
#[derive(Deserialize)]
pub(in crate::server) struct CsrfOnly {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub return_to: Option<String>,
}

// Shared session helpers (used by all sub-modules)
/// Verify admin session and also return the admin's username.
/// For use inside `spawn_blocking` closures.
fn require_admin_session_with_name(
    conn: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<(i64, String)> {
    let admin_id = require_admin_session_sid(conn, session_id)?;
    let name = db::get_admin_name_by_id(conn, admin_id)?.unwrap_or_else(|| "unknown".to_owned());
    Ok((admin_id, name))
}

/// Check CSRF using the cookie jar. Returns error on mismatch.
/// Verify admin session from a session ID string.
/// For use inside `spawn_blocking` closures where we have an open connection.
pub(in crate::server::handlers) fn require_admin_session_sid(
    conn: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<i64> {
    let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session = db::get_session(conn, sid)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;
    Ok(session.admin_id)
}

/// Requires same origin request.
pub(in crate::server) fn require_same_origin_request(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Result<()> {
    let request_authority = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .ok_or_else(|| AppError::Forbidden("Missing Host header.".into()))?;
    let request_scheme = request_scheme_for_same_origin(headers, peer, request_authority.host());
    let request_port = request_authority
        .port_u16()
        .unwrap_or(if request_scheme == "https" { 443 } else { 80 });

    // Browsers and HTTPS tunnels can omit Origin in legitimate same-origin
    // admin form posts. We accept two narrow fallbacks instead of broadly
    // allowing headerless requests:
    //   1. Origin: null with a same-origin Referer (seen in some tunnel/webview flows)
    //   2. Missing Origin/Referer with Sec-Fetch-Site: same-origin
    // Cross-site and malformed cases still fail closed below.
    let Some(source) = effective_same_origin_source(headers, request_authority.host()) else {
        if request_has_same_origin_fetch_metadata(headers) {
            return Ok(());
        }
        return Err(AppError::Forbidden(MISSING_ORIGIN_REFERER.into()));
    };
    if source.eq_ignore_ascii_case("null") {
        if is_loopback_alias(request_authority.host()) {
            return Ok(());
        }
        return Err(AppError::Forbidden(
            "Origin/Referer header must be same-origin.".into(),
        ));
    }
    let source_uri = source
        .parse::<Uri>()
        .map_err(|_error| AppError::Forbidden("Invalid Origin/Referer header.".into()))?;
    let source_scheme = source_uri
        .scheme_str()
        .ok_or_else(|| AppError::Forbidden("Origin/Referer header has no scheme.".into()))?;
    let source_authority = source_uri
        .authority()
        .ok_or_else(|| AppError::Forbidden("Origin/Referer header has no authority.".into()))?;
    if source_authority.as_str().contains('@') {
        return Err(AppError::Forbidden(
            "Origin/Referer header contains invalid authority.".into(),
        ));
    }
    let source_port = source_authority.port_u16().unwrap_or_else(|| {
        if source_scheme.eq_ignore_ascii_case("https") {
            443
        } else {
            80
        }
    });

    if source_scheme.eq_ignore_ascii_case(request_scheme)
        && hosts_match_for_same_origin(source_authority.host(), request_authority.host())
        && source_port == request_port
    {
        return Ok(());
    }

    tracing::warn!(
        target: "admin",
        request_scheme,
        request_host = %request_authority.host(),
        request_port,
        source_scheme,
        source_host = %source_authority.host(),
        source_port,
        source = %source,
        "Admin same-origin validation rejected request"
    );
    Err(AppError::Forbidden(
        "Origin/Referer origin mismatch.".into(),
    ))
}

fn effective_same_origin_source<'a>(headers: &'a HeaderMap, request_host: &str) -> Option<&'a str> {
    let origin = header_value_trimmed(headers, header::ORIGIN);
    let referer = header_value_trimmed(headers, header::REFERER);

    match origin {
        Some(origin) if !origin.eq_ignore_ascii_case("null") => Some(origin),
        Some(origin) if is_loopback_alias(request_host) => Some(origin),
        Some(_) | None => referer,
    }
}

fn header_value_trimmed(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn request_has_same_origin_fetch_metadata(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
}

pub(super) fn check_admin_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    if admin_csrf_is_valid(jar, form_token) {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

pub(super) fn admin_csrf_is_valid(jar: &CookieJar, form_token: Option<&str>) -> bool {
    let csrf_cookie = jar.get("csrf_token").map(Cookie::value);
    let session_id = jar.get(SESSION_COOKIE).map(Cookie::value);
    validate_signed_csrf(csrf_cookie, session_id, form_token.unwrap_or(""))
}

/// Requires same origin or valid CSRF.
pub(in crate::server::handlers) fn require_same_origin_or_valid_csrf(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    csrf_valid: bool,
) -> Result<()> {
    match require_same_origin_request(headers, peer) {
        Ok(()) => Ok(()),
        Err(AppError::Forbidden(message)) if message == MISSING_ORIGIN_REFERER && csrf_valid => {
            tracing::debug!(
                target: "admin",
                "Admin POST accepted without Origin/Referer because signed CSRF token was valid"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Requires admin post origin and CSRF.
pub(in crate::server::handlers) fn require_admin_post_origin_and_csrf(
    jar: &CookieJar,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    form_token: Option<&str>,
) -> Result<()> {
    let csrf_valid = admin_csrf_is_valid(jar, form_token);
    require_same_origin_or_valid_csrf(headers, peer, csrf_valid)?;
    if csrf_valid {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

fn admin_csrf_cookie(raw_token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new("csrf_token", raw_token);
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(secure);
    cookie
}

/// Refreshes admin CSRF cookie.
pub(super) fn refresh_admin_csrf_cookie(jar: CookieJar, secure: bool) -> CookieJar {
    let cookie = admin_csrf_cookie(new_csrf_token(), secure);
    jar.add(cookie)
}

/// Ensures admin CSRF.
pub(super) fn ensure_admin_csrf(jar: CookieJar, secure: bool) -> Result<(CookieJar, String)> {
    let raw = jar
        .get("csrf_token")
        .map(Cookie::value)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut jar = jar;
    let raw = raw.unwrap_or_else(|| {
        let generated_token = new_csrf_token();
        jar = std::mem::take(&mut jar).add(admin_csrf_cookie(generated_token.clone(), secure));
        generated_token
    });
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(Cookie::value)
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session_id = session_id.to_owned();
    Ok((
        jar,
        make_scoped_csrf_form_token(&raw, &CONFIG.cookie_secret, &session_id),
    ))
}

pub(super) use crate::utils::redirect::encode_query_component;

pub(in crate::server::handlers) fn should_set_secure_cookie(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
) -> bool {
    should_set_secure_cookie_with_config(
        headers,
        context,
        CONFIG.https_cookies,
        CONFIG.behind_proxy,
    )
}

fn should_set_secure_cookie_with_config(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
    https_cookies: bool,
    behind_proxy: bool,
) -> bool {
    context.direct_https
        || (https_cookies
            && crate::middleware::forwarded_proto_is_https(headers, context.peer, behind_proxy))
}

fn request_scheme_for_same_origin(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    request_host: &str,
) -> &'static str {
    request_scheme_for_same_origin_with_config(
        headers,
        peer,
        request_host,
        CONFIG.behind_proxy,
        CONFIG.tls.enabled,
        CONFIG.tls.port,
    )
}

fn request_scheme_for_same_origin_with_config(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    request_host: &str,
    behind_proxy: bool,
    tls_enabled: bool,
    tls_port: u16,
) -> &'static str {
    if crate::middleware::forwarded_proto_is_https(headers, peer, behind_proxy)
        || request_origin_uses_https(headers)
        || (tls_enabled
            && !is_onion_host(request_host)
            && host_header_uses_https_port_with_config(headers, tls_port))
    {
        "https"
    } else {
        "http"
    }
}

fn host_header_uses_https_port_with_config(headers: &HeaderMap, tls_port: u16) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };

    authority.port_u16().unwrap_or(443) == tls_port
}

fn request_origin_uses_https(headers: &HeaderMap) -> bool {
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_owned());

    let Some(request_host) = request_host.as_deref() else {
        return false;
    };
    let Some(source) = effective_same_origin_source(headers, request_host) else {
        return false;
    };

    let Ok(source_uri) = source.parse::<Uri>() else {
        return false;
    };

    if source_uri.scheme_str() != Some("https") {
        return false;
    }

    let Some(source_host) = source_uri.authority().map(axum::http::uri::Authority::host) else {
        return false;
    };

    hosts_match_for_same_origin(source_host, request_host)
}

fn hosts_match_for_same_origin(source_host: &str, request_host: &str) -> bool {
    let source_host = normalize_same_origin_host(source_host);
    let request_host = normalize_same_origin_host(request_host);

    if source_host.eq_ignore_ascii_case(request_host) {
        return true;
    }

    is_loopback_alias(source_host) && is_loopback_alias(request_host)
}

fn normalize_same_origin_host(host: &str) -> &str {
    let Some(inner) = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return host;
    };

    if inner.parse::<std::net::Ipv6Addr>().is_ok() {
        inner
    } else {
        host
    }
}

fn is_onion_host(host: &str) -> bool {
    let host = normalize_same_origin_host(host);
    let Some((label, suffix)) = host.rsplit_once('.') else {
        return false;
    };

    !label.is_empty() && suffix.eq_ignore_ascii_case("onion")
}

fn is_loopback_alias(host: &str) -> bool {
    let host = normalize_same_origin_host(host);

    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn admin_panel_redirect_with_status(
    message: &str,
    is_error: bool,
    target: &AdminPanelTarget<'_>,
) -> Redirect {
    let key = if is_error { "flash_error" } else { "flash" };
    let mut url = format!("/admin/panel?{key}={}", encode_query_component(message));
    if let Some(section) = target.open_section_value() {
        url.push_str("&open=");
        url.push_str(&encode_query_component(section));
    }
    if let Some(anchor) = target.anchor_value() {
        url.push('#');
        url.push_str(anchor);
    }
    Redirect::to(&url)
}

#[derive(Clone, Debug, Default)]
pub(super) struct AdminPanelTarget<'a> {
    anchor: Option<Cow<'a, str>>,
    open_section: Option<Cow<'a, str>>,
}

impl<'a> AdminPanelTarget<'a> {
    pub(super) const fn none() -> Self {
        Self {
            anchor: None,
            open_section: None,
        }
    }

    pub(super) const fn anchor(anchor: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Borrowed(anchor)),
            open_section: None,
        }
    }

    pub(super) const fn anchor_open(anchor: &'a str, open_section: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Borrowed(anchor)),
            open_section: Some(Cow::Borrowed(open_section)),
        }
    }

    pub(super) const fn owned_anchor_open(anchor: String, open_section: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Owned(anchor)),
            open_section: Some(Cow::Borrowed(open_section)),
        }
    }

    pub(super) fn anchor_value(&self) -> Option<&str> {
        self.anchor.as_deref().filter(|value| !value.is_empty())
    }

    pub(super) fn open_section_value(&self) -> Option<&str> {
        self.open_section
            .as_deref()
            .filter(|value| !value.is_empty())
    }
}

pub(super) fn admin_panel_redirect(message: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, &AdminPanelTarget::none())
}

pub(super) fn admin_panel_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, &AdminPanelTarget::anchor(anchor))
}

pub(super) fn admin_panel_redirect_anchor_open(
    message: &str,
    anchor: &str,
    open_section: &str,
) -> Redirect {
    admin_panel_redirect_with_status(
        message,
        false,
        &AdminPanelTarget::anchor_open(anchor, open_section),
    )
}

pub(super) fn admin_panel_error_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, true, &AdminPanelTarget::anchor(anchor))
}

pub(super) fn admin_panel_error_redirect_anchor_open(
    message: &str,
    anchor: &str,
    open_section: &str,
) -> Redirect {
    admin_panel_redirect_with_status(
        message,
        true,
        &AdminPanelTarget::anchor_open(anchor, open_section),
    )
}

fn admin_bootstrap_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn create_admin_session_bootstrap(session_id: &str) -> String {
    let token = crate::utils::crypto::new_session_id();
    let expires_at = admin_bootstrap_now_secs().saturating_add(ADMIN_BOOTSTRAP_TTL_SECS);
    let _previous_value =
        ADMIN_SESSION_BOOTSTRAPS.insert(token.clone(), (session_id.to_owned(), expires_at));
    token
}

/// Consumes admin session bootstrap.
pub(super) fn consume_admin_session_bootstrap(token: &str) -> Option<String> {
    let now = admin_bootstrap_now_secs();
    ADMIN_SESSION_BOOTSTRAPS.retain(|_, (_, expires_at)| *expires_at > now);

    let (session_id, expires_at) = ADMIN_SESSION_BOOTSTRAPS.remove(token)?.1;
    (expires_at > now).then_some(session_id)
}
