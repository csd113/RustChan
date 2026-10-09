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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context as _, Result};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt as _;

    /// Assign different test threads distinct loopback client identities.
    static NEXT_TEST_PEER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    std::thread_local! {
        /// Preserve one identity throughout each asynchronous authentication test.
        static TEST_PEER: std::net::SocketAddr = std::net::SocketAddr::from((
            std::net::Ipv4Addr::from(0x7f00_0001_u32.saturating_add(NEXT_TEST_PEER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))), 41000));
    }

    /// Keep independent tests from consuming each other's authentication reservations.
    fn test_connect_info() -> axum::extract::ConnectInfo<std::net::SocketAddr> {
        TEST_PEER.with(|peer| axum::extract::ConnectInfo(*peer))
    }

    const TEST_CSRF_COOKIE: &str = "csrf123";

    #[test]
    fn missing_user_password_work_cannot_authenticate_the_dummy_hash() -> Result<()> {
        anyhow::ensure!(verify_login_password(None, "legacy-admin-password")?.is_none());
        anyhow::ensure!(verify_login_password(None, "incorrect-password")?.is_none());
        let dummy = argon2::password_hash::phc::PasswordHash::new(MISSING_USER_HASH)?;
        let current = crate::utils::crypto::hash_password("current-password")?;
        let current = argon2::password_hash::phc::PasswordHash::new(&current)?;
        anyhow::ensure!(dummy.algorithm == current.algorithm && dummy.params == current.params);
        Ok(())
    }
    const TEST_ADMIN_ORIGIN: &str = "http://localhost";
    const TEST_ONION_HOST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion";
    const TEST_ONION_ORIGIN: &str =
        "http://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion";

    fn signed_admin_csrf() -> String {
        make_scoped_csrf_form_token(
            TEST_CSRF_COOKIE,
            &CONFIG.cookie_secret,
            ADMIN_LOGIN_CSRF_SCOPE,
        )
    }

    fn signed_admin_session_csrf(session_id: &str) -> String {
        make_scoped_csrf_form_token(TEST_CSRF_COOKIE, &CONFIG.cookie_secret, session_id)
    }

    fn admin_session_jar(session_id: &str) -> CookieJar {
        CookieJar::new()
            .add(Cookie::new(
                super::super::SESSION_COOKIE,
                session_id.to_owned(),
            ))
            .add(Cookie::new("csrf_token", TEST_CSRF_COOKIE))
    }

    fn admin_login_request(body: String) -> Result<Request<Body>> {
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, TEST_ADMIN_ORIGIN)
            .header(header::COOKIE, format!("csrf_token={TEST_CSRF_COOKIE}"))
            .extension(test_connect_info())
            .body(Body::from(body))
            .context("build admin login request")
    }

    fn create_test_board(state: &AppState) -> Result<()> {
        let conn = state
            .db
            .get()
            .context("get database connection for test board")?;
        db::create_board(&conn, "test", "Test", "", false)
            .context("create test board")
            .map(|_completed_value| ())?;
        Ok(())
    }

    fn create_test_admin_and_board(state: &AppState) -> Result<()> {
        let conn = state
            .db
            .get()
            .context("get database connection for test administrator")?;
        let password_hash = crate::utils::crypto::hash_password("hunter2")
            .context("hash test administrator password")?;
        db::create_admin(&conn, "admin", &password_hash)
            .context("create test administrator")
            .map(|_completed_value| ())?;
        db::create_board(&conn, "test", "Test", "", false)
            .context("create test board")
            .map(|_completed_value| ())?;
        Ok(())
    }

    // login_ip_key
    #[test]
    fn ip_key_is_hex_sha256() {
        let key = login_ip_key("127.0.0.1");
        // SHA-256 produces 32 bytes = 64 hex chars
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ip_key_same_ip_same_key() {
        assert_eq!(login_ip_key("192.168.1.1"), login_ip_key("192.168.1.1"));
    }

    #[test]
    fn ip_key_different_ips_different_keys() {
        assert_ne!(login_ip_key("192.168.1.1"), login_ip_key("192.168.1.2"));
    }

    #[test]
    fn ip_key_hides_raw_ip() {
        // The raw IP should not appear anywhere in the hash output
        let key = login_ip_key("10.0.0.1");
        assert!(!key.contains("10.0.0.1"));
    }

    #[test]
    fn redact_login_username_omits_full_attacker_input() {
        let redacted = redact_login_username("bad<script>");
        assert!(redacted.contains("bad"));
        assert!(redacted.contains("len="));
        assert!(!redacted.contains("<script>"));
    }

    // is_login_locked
    #[test]
    fn fresh_ip_is_not_locked() {
        let key = login_ip_key("test-fresh-ip-not-in-map");
        assert!(!is_login_locked(&key));
    }

    #[test]
    fn locked_after_exceeding_fail_limit() {
        let key = login_ip_key("test-lock-unique-99887766");
        // Clean up any residue from a previous run
        ADMIN_LOGIN_FAILS.remove(&key);

        let now = login_now_secs();
        ADMIN_LOGIN_FAILS.insert(
            key.clone(),
            (admin_login_fail_limit(), now),
            admin_login_fail_window_secs(),
        );
        assert!(is_login_locked(&key));

        // Cleanup
        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[test]
    fn not_locked_below_fail_limit() {
        let key = login_ip_key("test-below-limit-11223344");
        ADMIN_LOGIN_FAILS.remove(&key);

        let now = login_now_secs();
        ADMIN_LOGIN_FAILS.insert(
            key.clone(),
            (admin_login_fail_limit() - 1, now),
            admin_login_fail_window_secs(),
        );
        assert!(!is_login_locked(&key));

        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[test]
    fn expired_window_is_not_locked() {
        let key = login_ip_key("test-expired-window-55667788");
        ADMIN_LOGIN_FAILS.remove(&key);

        // window_start far in the past, beyond admin_login_fail_window_secs()
        let old_ts = login_now_secs().saturating_sub(admin_login_fail_window_secs() + 60);
        ADMIN_LOGIN_FAILS.insert(
            key.clone(),
            (admin_login_fail_limit() + 10, old_ts),
            admin_login_fail_window_secs(),
        );
        assert!(!is_login_locked(&key));

        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[tokio::test]
    async fn locked_out_admin_login_rerenders_login_form_with_specific_message() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_board(&state)?;

        let ip_key = login_ip_key("192.0.2.44");
        ADMIN_LOGIN_FAILS.remove(&ip_key);
        ADMIN_LOGIN_FAILS.insert(
            ip_key.clone(),
            (admin_login_fail_limit(), login_now_secs()),
            admin_login_fail_window_secs(),
        );

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        // This test installs a lockout, so it must not share other tests' peer.
        let mut request = admin_login_request(format!(
            "username=admin&password=wrong&_csrf={}",
            signed_admin_csrf()
        ))?;
        let _previous_peer = request.extensions_mut().insert(axum::extract::ConnectInfo(
            std::net::SocketAddr::from(([192, 0, 2, 44], 41000)),
        ));
        let response = router
            .oneshot(request)
            .await
            .context("send locked-out admin login request")?;

        ADMIN_LOGIN_FAILS.remove(&ip_key);

        anyhow::ensure!(
            response.status() == StatusCode::OK,
            "expected locked-out login to render with status 200, got {}",
            response.status()
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read locked-out login response body")?;
        let body = String::from_utf8(body.to_vec())
            .context("decode locked-out login response body as UTF-8")?;
        anyhow::ensure!(
            body.contains("Too many failed admin login attempts."),
            "locked-out login response did not contain the specific lockout message"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_sets_session_cookie_for_valid_credentials() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let mut request = admin_login_request(format!(
            "username=admin&password=hunter2&_csrf={}",
            signed_admin_csrf()
        ))?;
        let _previous_transport = request
            .extensions_mut()
            .insert(crate::middleware::RequestTransport { direct_https: true });
        let response = router
            .oneshot(request)
            .await
            .context("send valid admin login request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected valid admin login to redirect, got {}",
            response.status()
        );
        let session_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .context("find session cookie in valid login response")?;
        anyhow::ensure!(
            session_cookie.contains("HttpOnly"),
            "session cookie was missing HttpOnly: {session_cookie}"
        );
        anyhow::ensure!(
            session_cookie.contains("SameSite=Lax"),
            "session cookie was missing SameSite=Lax: {session_cookie}"
        );
        anyhow::ensure!(
            session_cookie.contains("Secure"),
            "HTTPS session cookie was missing Secure: {session_cookie}"
        );
        let csrf_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains("csrf_token="))
            .context("find CSRF cookie in valid login response")?;
        anyhow::ensure!(
            csrf_cookie.contains("SameSite=Strict"),
            "CSRF cookie was missing SameSite=Strict: {csrf_cookie}"
        );
        anyhow::ensure!(
            csrf_cookie.contains("Secure"),
            "HTTPS CSRF cookie was missing Secure: {csrf_cookie}"
        );
        anyhow::ensure!(
            !csrf_cookie.contains("csrf_token=csrf123"),
            "login did not rotate the original CSRF cookie: {csrf_cookie}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_rotates_csrf_cookie_on_success() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(format!(
                "username=admin&password=hunter2&_csrf={}",
                signed_admin_csrf()
            ))?)
            .await
            .context("send admin login request for CSRF rotation")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected successful login to redirect, got {}",
            response.status()
        );
        let csrf_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains("csrf_token="))
            .context("find rotated CSRF cookie in login response")?;
        anyhow::ensure!(
            !csrf_cookie.contains("csrf_token=csrf123"),
            "login retained the original CSRF cookie: {csrf_cookie}"
        );
        Ok(())
    }

    #[test]
    fn admin_scoped_csrf_rejects_session_swap() {
        let jar_a = admin_session_jar("session-a");
        let token_a = signed_admin_session_csrf("session-a");
        assert!(super::super::check_admin_csrf_jar(&jar_a, Some(&token_a)).is_ok());

        let jar_b = admin_session_jar("session-b");
        assert!(super::super::check_admin_csrf_jar(&jar_b, Some(&token_a)).is_err());
    }

    #[test]
    fn admin_scoped_csrf_rejects_raw_cookie_equality() {
        let jar = admin_session_jar("session-a");
        assert!(super::super::check_admin_csrf_jar(&jar, Some(TEST_CSRF_COOKIE)).is_err());
    }

    #[tokio::test]
    async fn admin_logout_clears_csrf_cookie_and_session_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        {
            let conn = state
                .db
                .get()
                .context("get database connection for logout test")?;
            let password_hash = crate::utils::crypto::hash_password("hunter2")
                .context("hash logout test administrator password")?;
            let admin_id = db::create_admin(&conn, "admin", &password_hash)
                .context("create logout test administrator")?;
            db::create_session(&conn, "session123", admin_id, Utc::now().timestamp() + 3600)
                .context("create logout test session")?;
        }

        let router = Router::new()
            .route("/admin/logout", post(admin_logout))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/logout")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        format!(
                            "csrf_token=csrf123; {}=session123",
                            super::super::SESSION_COOKIE
                        ),
                    )
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "return_to=/admin&_csrf={}",
                        signed_admin_session_csrf("session123")
                    )))
                    .context("build admin logout request")?,
            )
            .await
            .context("send admin logout request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected logout to redirect, got {}",
            response.status()
        );
        let set_cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            set_cookies
                .iter()
                .any(|cookie| cookie.contains("csrf_token=;")),
            "logout response did not clear the CSRF cookie: {set_cookies:?}"
        );
        anyhow::ensure!(
            set_cookies
                .iter()
                .any(|cookie| cookie.contains(&format!("{}=;", super::super::SESSION_COOKIE))),
            "logout response did not clear the session cookie: {set_cookies:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_marks_session_cookie_secure_for_direct_https_request() -> Result<()> {
        let state = crate::test_support::app_state();
        clear_login_fails(&login_ip_key(&test_connect_info().0.ip().to_string()));
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let (host, origin) = if CONFIG.tls.enabled {
            let host = format!("demo.serveo.net:{}", CONFIG.tls.port);
            let origin = format!("https://{host}");
            (host, origin)
        } else {
            ("localhost".to_owned(), TEST_ADMIN_ORIGIN.to_owned())
        };
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, &host)
                    .header(header::ORIGIN, &origin)
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .extension(crate::middleware::RequestTransport { direct_https: true })
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build direct HTTPS admin login request")?,
            )
            .await
            .context("send direct HTTPS admin login request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected direct HTTPS admin login to redirect, got {}",
            response.status()
        );
        let session_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .context("find session cookie in direct HTTPS login response")?;
        anyhow::ensure!(
            session_cookie.contains("Secure"),
            "direct HTTPS session cookie was missing Secure: {session_cookie}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn insecure_admin_login_redirects_through_bootstrap() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "192.168.1.20:8080")
                    .header(header::ORIGIN, "http://192.168.1.20:8080")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build insecure admin login request")?,
            )
            .await
            .context("send insecure admin login request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected insecure admin login to redirect, got {}",
            response.status()
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("find valid Location header in insecure login response")?;
        anyhow::ensure!(
            location.starts_with("/admin/panel?bootstrap="),
            "insecure login did not redirect through bootstrap: {location}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_over_onion_http_sets_insecure_session_cookie() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, TEST_ONION_HOST)
                    .header(header::ORIGIN, TEST_ONION_ORIGIN)
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build onion HTTP admin login request")?,
            )
            .await
            .context("send onion HTTP admin login request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected onion HTTP admin login to redirect, got {}",
            response.status()
        );
        let session_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .context("find session cookie in onion HTTP login response")?;
        anyhow::ensure!(
            !session_cookie.contains("; Secure"),
            "onion HTTP session cookie unexpectedly used Secure: {session_cookie}"
        );
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("find valid Location header in onion HTTP login response")?;
        anyhow::ensure!(
            location.starts_with("/admin/panel?bootstrap="),
            "onion HTTP login did not redirect through bootstrap: {location}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_rejects_raw_readable_csrf_cookie_without_signed_form_token() -> Result<()>
    {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(
                "username=admin&password=hunter2&_csrf=csrf123".to_owned(),
            )?)
            .await
            .context("send admin login request with raw CSRF cookie value")?;

        anyhow::ensure!(
            response.status() == StatusCode::FORBIDDEN,
            "expected raw CSRF cookie value to be rejected, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_rejects_same_host_different_port_origin() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost:3000")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build admin login request with a mismatched origin port")?,
            )
            .await
            .context("send admin login request with a mismatched origin port")?;

        anyhow::ensure!(
            response.status() == StatusCode::FORBIDDEN,
            "expected mismatched origin port to be rejected, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_rejects_same_host_different_origin_port() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "example.test:8080")
                    .header(header::ORIGIN, "https://example.test")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build admin login request with a missing origin port")?,
            )
            .await
            .context("send admin login request with a missing origin port")?;

        anyhow::ensure!(
            response.status() == StatusCode::FORBIDDEN,
            "expected missing origin port to be rejected, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_missing_origin_when_signed_csrf_is_valid() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build origin-less admin login request with valid CSRF")?,
            )
            .await
            .context("send origin-less admin login request with valid CSRF")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected valid signed CSRF without Origin to be accepted, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_rejects_missing_origin_when_signed_csrf_is_invalid() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from("username=admin&password=hunter2&_csrf=csrf123"))
                    .context("build origin-less admin login request with invalid CSRF")?,
            )
            .await
            .context("send origin-less admin login request with invalid CSRF")?;

        anyhow::ensure!(
            response.status() == StatusCode::FORBIDDEN,
            "expected invalid signed CSRF without Origin to be rejected, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_null_origin_on_loopback_host() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "null")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build loopback admin login request with null Origin")?,
            )
            .await
            .context("send loopback admin login request with null Origin")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected null Origin on loopback to be accepted, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_loopback_alias_origin_match() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "127.0.0.1:8080")
                    .header(header::ORIGIN, "http://localhost:8080")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build admin login request with a loopback alias Origin")?,
            )
            .await
            .context("send admin login request with a loopback alias Origin")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected loopback alias Origin to be accepted, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_ipv6_loopback_url() -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "[::1]:8080")
                    .header(header::ORIGIN, "http://[::1]:8080")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build IPv6 loopback admin login request")?,
            )
            .await
            .context("send IPv6 loopback admin login request")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected IPv6 loopback URL to be accepted, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_null_origin_with_same_origin_referer_on_https_tunnel() -> Result<()>
    {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "demo.serveo.net")
                    .header(header::ORIGIN, "null")
                    .header(header::REFERER, "https://demo.serveo.net/admin")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build HTTPS tunnel login request with same-origin Referer")?,
            )
            .await
            .context("send HTTPS tunnel login request with same-origin Referer")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected same-origin Referer with null Origin to be accepted, got {}",
            response.status()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_login_accepts_missing_origin_and_referer_with_same_origin_fetch_metadata(
    ) -> Result<()> {
        let state = crate::test_support::app_state();
        create_test_admin_and_board(&state)?;

        let router = Router::new()
            .route("/admin/login", post(admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "demo.serveo.net")
                    .header("sec-fetch-site", "same-origin")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(test_connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .context("build admin login request with same-origin fetch metadata")?,
            )
            .await
            .context("send admin login request with same-origin fetch metadata")?;

        anyhow::ensure!(
            response.status() == StatusCode::SEE_OTHER,
            "expected same-origin fetch metadata to be accepted, got {}",
            response.status()
        );
        Ok(())
    }
}
