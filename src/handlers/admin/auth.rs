//
// Admin authentication: login, logout, session management.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. GET  /admin       → redirect to panel if already logged in, else show login form
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Brute-force protection:
//   After admin_login_fail_limit() failed attempts within admin_login_fail_window_secs() seconds, the IP is
//   locked out for the remainder of that window.  On success the counter is cleared.
//   Keys are SHA-256(IP) to avoid retaining raw addresses in memory.

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::AppState,
    templates,
    utils::crypto::{make_scoped_csrf_form_token, new_csrf_token, new_session_id, verify_password},
};
use axum::{
    extract::{Form, State},
    http::HeaderMap,
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::Utc;
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

// Admin login brute-force lockout
// After admin_login_fail_limit() failed attempts within admin_login_fail_window_secs() seconds the
// IP is locked out for the remainder of that window.  On success the counter
// is cleared immediately so a genuine admin is never self-locked.
//
// Keys are SHA-256(IP) to avoid retaining raw addresses in memory.

/// Login fail limit used by this handler.
fn admin_login_fail_limit() -> u32 {
    CONFIG.operator.admin_login_fail_limit
}
/// Login fail window used by this handler.
fn admin_login_fail_window_secs() -> u64 {
    CONFIG.operator.admin_login_fail_window_secs
}
/// Admin login CSRF scope used by this handler.
const ADMIN_LOGIN_CSRF_SCOPE: &str = "admin-login";

/// Public dummy hash with the same Argon2 parameters as newly created accounts.
/// Missing usernames perform password work without ever yielding an account ID.
const MISSING_USER_HASH: &str = "$argon2id$v=19$m=65536,t=2,p=2$cnVzdGNoYW4tdjEuNi41ISE$9bLCu60YvVwNCStcJ+Uv2kvViRlaP/Rm/dg64RcLFjI";

/// Avoid a cheap username-enumeration path within the shared password budget.
fn verify_login_password(
    user: Option<&crate::models::AdminUser>,
    password: &str,
) -> Result<Option<i64>> {
    let hash = user.map_or(MISSING_USER_HASH, |account| account.password_hash.as_str());
    let verified = verify_password(password, hash)?;
    Ok(user.filter(|_account| verified).map(|account| account.id))
}

/// Bounded hashed-IP failure state; unknown identities fail closed at capacity.
static ADMIN_LOGIN_FAILS: LazyLock<crate::middleware::RateTable> =
    LazyLock::new(crate::middleware::RateTable::default);

fn login_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn login_ip_key(ip: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(ip.as_bytes());
    hex::encode(h.finalize())
}

fn redact_login_username(username: &str) -> String {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return "<empty>".to_owned();
    }

    let safe_prefix = trimmed
        .chars()
        .take(3)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let char_len = trimmed.chars().count();
    format!("{safe_prefix}… (len={char_len})")
}

/// Returns true if this IP is currently locked out.
pub(super) fn is_login_locked(ip_key: &str) -> bool {
    ADMIN_LOGIN_FAILS
        .retry_after(ip_key, login_now_secs(), admin_login_fail_limit())
        .is_some()
}

/// Reserve authentication work before verification; returns the current attempt count.
pub(super) fn record_login_fail(ip_key: &str) -> u32 {
    ADMIN_LOGIN_FAILS.record(ip_key, login_now_secs(), admin_login_fail_window_secs())
}

/// Clear the successful client's failure budget.
pub(super) fn clear_login_fails(ip_key: &str) {
    ADMIN_LOGIN_FAILS.remove(ip_key);
}

/// Expire retained failed-login identities during background cleanup.
pub(in crate::server) fn prune_login_fails() {
    ADMIN_LOGIN_FAILS.prune(login_now_secs());
}

/// Ensures admin login CSRF.
fn ensure_admin_login_csrf(
    jar: CookieJar,
    headers: &HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> (CookieJar, String) {
    let token = jar
        .get("csrf_token")
        .map(|cookie| cookie.value().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(new_csrf_token);

    let mut cookie = Cookie::new("csrf_token", token.clone());
    cookie.set_http_only(false);
    // `Lax` keeps the login and redirect flow working in mobile browsers and
    // embedded webviews while CSRF validation still guards the POST itself.
    cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
    cookie.set_path("/");
    cookie.set_secure(super::should_set_secure_cookie(headers, secure_context));

    (
        jar.add(cookie),
        make_scoped_csrf_form_token(&token, &CONFIG.cookie_secret, ADMIN_LOGIN_CSRF_SCOPE),
    )
}

async fn render_admin_login_response(
    state: &AppState,
    jar: CookieJar,
    headers: &HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    error: Option<&str>,
) -> Result<Response> {
    let (jar, csrf) = ensure_admin_login_csrf(jar, headers, secure_context);
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let boards = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<Vec<crate::models::Board>> {
            let conn = pool.get()?;
            Ok(db::get_all_boards(&conn)?)
        }
    })
    .await
    .map_err(|join_error| AppError::Internal(anyhow::anyhow!(join_error)))??;
    Ok((
        jar,
        Html(templates::admin_login_page(
            error,
            &csrf,
            &boards,
            current_theme.as_deref(),
        )),
    )
        .into_response())
}

// GET /admin
pub(in crate::server) async fn admin_index(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> Result<Response> {
    // Move DB I/O into spawn_blocking.
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());

    let (is_logged_in, boards) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(bool, Vec<crate::models::Board>)> {
            let conn = pool.get()?;
            let logged_in = session_id
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            let boards = db::get_all_boards(&conn)?;
            Ok((logged_in, boards))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if is_logged_in {
        return Ok(Redirect::to("/admin/panel").into_response());
    }

    let (jar, csrf) = ensure_admin_login_csrf(jar, &headers, secure_context);
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    Ok((
        jar,
        Html(templates::admin_login_page(
            None,
            &csrf,
            &boards,
            current_theme.as_deref(),
        )),
    )
        .into_response())
}

// POST /admin/login
#[derive(Deserialize)]
pub(in crate::server) struct LoginForm {
    username: String,
    password: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[expect(
    clippy::cognitive_complexity,
    reason = "the authentication, lockout, and session issuance branches share one security boundary"
)]
#[expect(
    clippy::too_many_lines,
    reason = "authentication, lockout accounting, and session issuance form one security boundary"
)]
pub(in crate::server) async fn admin_login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    Form(form): Form<LoginForm>,
) -> Result<Response> {
    let ip_key = login_ip_key(&client_ip);
    if is_login_locked(&ip_key) {
        warn!(
            ip_prefix = %ip_key.get(..8).unwrap_or(&ip_key),
            "Admin login blocked by brute-force lockout"
        );
        return render_admin_login_response(
            &state,
            jar,
            &headers,
            secure_context,
            Some("Too many failed admin login attempts. Please wait a few minutes and try again."),
        )
        .await;
    }

    let csrf_cookie = jar.get("csrf_token").map(Cookie::value);
    let csrf_valid = crate::middleware::validate_signed_csrf(
        csrf_cookie,
        Some(ADMIN_LOGIN_CSRF_SCOPE),
        form.csrf.as_deref().unwrap_or(""),
    );
    super::require_same_origin_or_valid_csrf(&headers, secure_context.peer, csrf_valid)?;
    if !csrf_valid {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let username = form.username.trim().to_owned();
    let username_log = redact_login_username(&username);
    if username.is_empty() || username.len() > 64 || form.password.len() > 1024 {
        return render_admin_login_response(
            &state,
            jar,
            &headers,
            secure_context,
            Some("Invalid username."),
        )
        .await;
    }

    let permit = state.password_work_gate.try_begin()?;
    // Reserve before Argon2 so concurrent attempts cannot all pass a stale
    // failure-count check. Successful authentication clears this reservation.
    let attempts = record_login_fail(&ip_key);
    if attempts > admin_login_fail_limit() {
        return Err(AppError::Forbidden(
            "Too many login attempts. Please wait and try again.".into(),
        ));
    }
    let pool = state.db.clone();
    let password = form.password;

    // Argon2 verification is CPU-intensive; always use spawn_blocking.
    let result = tokio::task::spawn_blocking(move || -> Result<Option<i64>> {
        let _password_permit = permit;
        let conn = pool.get()?;
        let user = db::get_admin_by_username(&conn, &username)?;
        drop(conn);
        verify_login_password(user.as_ref(), &password)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    match result {
        None => {
            let fails = attempts;
            let locked_out = fails >= admin_login_fail_limit();
            warn!(
                username = %username_log,
                ip_prefix = %ip_key.get(..8).unwrap_or(&ip_key),
                attempts = fails,
                attempt_limit = admin_login_fail_limit(),
                locked_out,
                "Failed admin login"
            );
            render_admin_login_response(
                &state,
                jar,
                &headers,
                secure_context,
                Some("Invalid username or password."),
            )
            .await
        }
        Some(admin_id) => {
            clear_login_fails(&ip_key);
            let session_id = new_session_id();
            let bootstrap_session_id = session_id.clone();
            let expires_at = Utc::now()
                .timestamp()
                .checked_add(CONFIG.session_duration)
                .ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("administrator session expiry overflows"))
                })?;
            let sid_clone = session_id.clone();
            tokio::task::spawn_blocking({
                let session_pool = state.db.clone();
                move || -> Result<()> {
                    let conn = session_pool.get()?;
                    db::create_session(&conn, &sid_clone, admin_id, expires_at)?;
                    Ok(())
                }
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

            let mut cookie = Cookie::new(super::SESSION_COOKIE, session_id);
            cookie.set_http_only(true);
            // `Strict` can drop the freshly issued session on some mobile
            // redirect chains into `/admin/panel`; `Lax` preserves that
            // top-level navigation while same-origin + CSRF checks protect
            // admin mutations.
            cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
            cookie.set_path("/");
            // Only mark the session cookie Secure when this request is actually
            // arriving over HTTPS (direct TLS or proxy-forwarded HTTPS).
            let cookie_secure = super::should_set_secure_cookie(&headers, secure_context);
            cookie.set_secure(cookie_secure);
            // Set Max-Age so browsers expire the cookie after the
            // configured session lifetime instead of persisting it indefinitely.
            cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            tracing::info!(target: "admin", admin_id = admin_id, "Admin logged in");
            let jar = super::refresh_admin_csrf_cookie(jar.add(cookie), cookie_secure);
            let redirect = if cookie_secure {
                Redirect::to("/admin/panel")
            } else {
                let bootstrap = super::create_admin_session_bootstrap(&bootstrap_session_id);
                Redirect::to(&format!(
                    "/admin/panel?bootstrap={}",
                    crate::utils::redirect::encode_query_component(&bootstrap)
                ))
            };
            Ok((jar, redirect).into_response())
        }
    }
}

// POST /admin/logout
pub(in crate::server) async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<super::CsrfOnly>,
) -> Result<Response> {
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    if let Some(session_cookie) = jar.get(super::SESSION_COOKIE) {
        let session_id = session_cookie.value().to_owned();
        // DB call in spawn_blocking
        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<()> {
                let conn = pool.get()?;
                db::delete_session(&conn, &session_id)?;
                Ok(())
            }
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    }
    let jar = jar
        .remove(Cookie::from(super::SESSION_COOKIE))
        .remove(Cookie::from("csrf_token"));
    let destination =
        crate::utils::redirect::strict_safe_internal_path_or(form.return_to.as_deref(), "/admin");
    Ok((jar, Redirect::to(destination)).into_response())
}
