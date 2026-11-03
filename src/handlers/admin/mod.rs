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

#[cfg(test)]
#[path = "tests/csrf.rs"]
mod tests;

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
use axum::{http::HeaderMap, response::Redirect};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use serde::Deserialize;
use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

// Shared constant
/// Session cookie used by this handler.
const SESSION_COOKIE: &str = "chan_admin_session";
/// Admin cookie same site used by this handler.
const ADMIN_COOKIE_SAME_SITE: SameSite = SameSite::Lax;
/// Admin bootstrap TTL secs used by this handler.
const ADMIN_BOOTSTRAP_TTL_SECS: u64 = 120;

/// Single-use login redirect bound to the browser's freshly rotated CSRF cookie.
struct AdminSessionBootstrap {
    session_id: String,
    csrf_binding: String,
    expires_at: u64,
}

static ADMIN_SESSION_BOOTSTRAPS: LazyLock<DashMap<String, AdminSessionBootstrap>> =
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

/// Require unambiguous browser provenance for the actual request transport.
pub(in crate::server) fn require_same_origin_request(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
) -> Result<()> {
    crate::middleware::check_same_origin(headers, context).map_err(Into::into)
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

/// Requires admin post origin and CSRF.
pub(in crate::server::handlers) fn require_admin_post_origin_and_csrf(
    jar: &CookieJar,
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
    form_token: Option<&str>,
) -> Result<()> {
    let csrf_valid = admin_csrf_is_valid(jar, form_token);
    require_same_origin_request(headers, context)?;
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

pub(super) fn create_admin_session_bootstrap(session_id: &str, csrf_cookie: &str) -> String {
    let token = crate::utils::crypto::new_session_id();
    let now = admin_bootstrap_now_secs();
    ADMIN_SESSION_BOOTSTRAPS.retain(|_, entry| entry.expires_at > now);
    let entry = AdminSessionBootstrap {
        session_id: session_id.to_owned(),
        csrf_binding: crate::utils::crypto::sign_scoped_csrf_token(
            csrf_cookie,
            &CONFIG.cookie_secret,
            "admin-bootstrap",
        ),
        expires_at: now.saturating_add(ADMIN_BOOTSTRAP_TTL_SECS),
    };
    let _previous_value = ADMIN_SESSION_BOOTSTRAPS.insert(token.clone(), entry);
    token
}

/// Consumes admin session bootstrap.
pub(super) fn consume_admin_session_bootstrap(
    token: &str,
    csrf_cookie: Option<&str>,
) -> Option<String> {
    let csrf_cookie = csrf_cookie.filter(|value| !value.is_empty())?;
    let now = admin_bootstrap_now_secs();
    ADMIN_SESSION_BOOTSTRAPS.retain(|_, entry| entry.expires_at > now);
    let binding = crate::utils::crypto::sign_scoped_csrf_token(
        csrf_cookie,
        &CONFIG.cookie_secret,
        "admin-bootstrap",
    );

    let entry = ADMIN_SESSION_BOOTSTRAPS
        .remove_if(token, |_, entry| {
            entry.expires_at > now && entry.csrf_binding == binding
        })?
        .1;
    Some(entry.session_id)
}
