// handlers/admin/auth.rs
//
// Admin authentication: login, logout, session management.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. GET  /admin       → redirect to panel if already logged in, else show login form
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Brute-force protection:
//   After LOGIN_FAIL_LIMIT failed attempts within LOGIN_FAIL_WINDOW seconds, the IP is
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
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::Utc;
use dashmap::DashMap;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use time;
use tracing::warn;

// ─── Admin login brute-force lockout ──────────────────────────────────
//
// After LOGIN_FAIL_LIMIT failed attempts within LOGIN_FAIL_WINDOW seconds the
// IP is locked out for the remainder of that window.  On success the counter
// is cleared immediately so a genuine admin is never self-locked.
//
// Keys are SHA-256(IP) to avoid retaining raw addresses in memory.

const LOGIN_FAIL_LIMIT: u32 = 5;
const LOGIN_FAIL_WINDOW: u64 = 900; // 15 minutes
const ADMIN_LOGIN_CSRF_SCOPE: &str = "admin-login";

/// `ip_hash` → (`fail_count`, `window_start_secs`)
static ADMIN_LOGIN_FAILS: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);
static LOGIN_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

fn login_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn login_ip_key(ip: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(ip.as_bytes());
    hex::encode(h.finalize())
}

fn redact_login_username(username: &str) -> String {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
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
fn is_login_locked(ip_key: &str) -> bool {
    let now = login_now_secs();
    if let Some(entry) = ADMIN_LOGIN_FAILS.get(ip_key) {
        let (count, window_start) = *entry;
        if now.saturating_sub(window_start) <= LOGIN_FAIL_WINDOW {
            return count >= LOGIN_FAIL_LIMIT;
        }
    }
    false
}

/// Record a failed login attempt; returns the new failure count.
#[allow(clippy::significant_drop_tightening)]
fn record_login_fail(ip_key: &str) -> u32 {
    let now = login_now_secs();
    let mut entry = ADMIN_LOGIN_FAILS
        .entry(ip_key.to_string())
        .or_insert((0, now));
    let (count, window_start) = entry.value_mut();
    if now.saturating_sub(*window_start) > LOGIN_FAIL_WINDOW {
        *count = 1;
        *window_start = now;
    } else {
        *count = count.saturating_add(1);
    }
    *count
}

fn clear_login_fails(ip_key: &str) {
    ADMIN_LOGIN_FAILS.remove(ip_key);
}

/// Remove login-fail entries whose window has expired.
/// Called periodically from the background task in `server/server.rs`.
pub fn prune_login_fails() {
    let now = login_now_secs();
    // Throttle to at most once per LOGIN_FAIL_WINDOW seconds.
    let last = LOGIN_CLEANUP_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < LOGIN_FAIL_WINDOW {
        return;
    }
    LOGIN_CLEANUP_SECS.store(now, Ordering::Relaxed);
    ADMIN_LOGIN_FAILS
        .retain(|_, (_, window_start)| now.saturating_sub(*window_start) <= LOGIN_FAIL_WINDOW);
}

fn ensure_admin_login_csrf(
    jar: CookieJar,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> (CookieJar, String) {
    let token = jar
        .get("csrf_token")
        .map(|cookie| cookie.value().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(new_csrf_token);

    let mut cookie = Cookie::new("csrf_token", token.clone());
    cookie.set_http_only(false);
    // `Lax` keeps the login and redirect flow working in mobile browsers and
    // embedded webviews while CSRF validation still guards the POST itself.
    cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
    cookie.set_path("/");
    cookie.set_secure(super::should_set_secure_cookie(headers, peer));

    (
        jar.add(cookie),
        make_scoped_csrf_form_token(&token, &CONFIG.cookie_secret, ADMIN_LOGIN_CSRF_SCOPE),
    )
}

async fn render_admin_login_response(
    state: &AppState,
    jar: CookieJar,
    headers: &HeaderMap,
    peer: SocketAddr,
    error: Option<&str>,
) -> Result<Response> {
    let (jar, csrf) = ensure_admin_login_csrf(jar, headers, Some(peer));
    let boards = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<Vec<crate::models::Board>> {
            let conn = pool.get()?;
            Ok(db::get_all_boards(&conn)?)
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;
    Ok((
        jar,
        Html(templates::admin_login_page(error, &csrf, &boards)),
    )
        .into_response())
}

// ─── GET /admin ───────────────────────────────────────────────────────────────

pub async fn admin_index(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> Result<Response> {
    // Move DB I/O into spawn_blocking.
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

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

    let (jar, csrf) = ensure_admin_login_csrf(jar, &headers, Some(peer));
    Ok((jar, Html(templates::admin_login_page(None, &csrf, &boards))).into_response())
}

// ─── POST /admin/login ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
pub async fn admin_login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    Form(form): Form<LoginForm>,
) -> Result<Response> {
    let ip_key = login_ip_key(&client_ip);
    if is_login_locked(&ip_key) {
        warn!(
            ip_prefix = %&ip_key[..8],
            "Admin login blocked by brute-force lockout"
        );
        return render_admin_login_response(
            &state,
            jar,
            &headers,
            peer,
            Some("Too many failed admin login attempts. Please wait a few minutes and try again."),
        )
        .await;
    }

    super::require_same_origin_request(&headers, Some(peer))?;
    let csrf_cookie = jar
        .get("csrf_token")
        .map(axum_extra::extract::cookie::Cookie::value);
    if !crate::middleware::validate_signed_csrf(
        csrf_cookie,
        Some(ADMIN_LOGIN_CSRF_SCOPE),
        form.csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let username = form.username.trim().to_string();
    let username_log = redact_login_username(&username);
    if username.is_empty() || username.len() > 64 {
        return render_admin_login_response(&state, jar, &headers, peer, Some("Invalid username."))
            .await;
    }

    let pool = state.db.clone();
    let password = form.password.clone();

    // Argon2 verification is CPU-intensive; always use spawn_blocking.
    let result = tokio::task::spawn_blocking(move || -> Result<Option<i64>> {
        let conn = pool.get()?;
        let user = db::get_admin_by_username(&conn, &username)?;
        if let Some(u) = user {
            if verify_password(&password, &u.password_hash)? {
                return Ok(Some(u.id));
            }
        }
        Ok(None)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    match result {
        None => {
            let fails = record_login_fail(&ip_key);
            let locked_out = fails >= LOGIN_FAIL_LIMIT;
            warn!(
                username = %username_log,
                ip_prefix = %&ip_key[..8],
                attempts = fails,
                attempt_limit = LOGIN_FAIL_LIMIT,
                locked_out,
                "Failed admin login"
            );
            render_admin_login_response(
                &state,
                jar,
                &headers,
                peer,
                Some("Invalid username or password."),
            )
            .await
        }
        Some(admin_id) => {
            clear_login_fails(&ip_key);
            // Create session (in spawn_blocking)
            let session_id = new_session_id();
            let bootstrap_session_id = session_id.clone();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            let sid_clone = session_id.clone();
            tokio::task::spawn_blocking({
                let pool = state.db.clone();
                move || -> Result<()> {
                    let conn = pool.get()?;
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
            let cookie_secure = super::should_set_secure_cookie(&headers, Some(peer));
            cookie.set_secure(cookie_secure);
            // Set Max-Age so browsers expire the cookie after the
            // configured session lifetime instead of persisting it indefinitely.
            cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            tracing::info!(target: "admin", admin_id = admin_id, "Admin logged in");
            let jar = super::refresh_admin_csrf_cookie(jar.add(cookie));
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

// ─── POST /admin/logout ───────────────────────────────────────────────────────

pub async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<super::CsrfOnly>,
) -> Result<Response> {
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    if let Some(session_cookie) = jar.get(super::SESSION_COOKIE) {
        let session_id = session_cookie.value().to_string();
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
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use axum_extra::extract::cookie::{Cookie, CookieJar};
    use tower::ServiceExt as _;

    const TEST_CSRF_COOKIE: &str = "csrf123";
    const TEST_ADMIN_ORIGIN: &str = "http://localhost";

    fn signed_admin_csrf() -> String {
        make_scoped_csrf_form_token(
            TEST_CSRF_COOKIE,
            &crate::config::CONFIG.cookie_secret,
            ADMIN_LOGIN_CSRF_SCOPE,
        )
    }

    fn signed_admin_session_csrf(session_id: &str) -> String {
        crate::utils::crypto::make_scoped_csrf_form_token(
            TEST_CSRF_COOKIE,
            &crate::config::CONFIG.cookie_secret,
            session_id,
        )
    }

    fn admin_session_jar(session_id: &str) -> CookieJar {
        CookieJar::new()
            .add(Cookie::new(
                super::super::SESSION_COOKIE,
                session_id.to_string(),
            ))
            .add(Cookie::new("csrf_token", TEST_CSRF_COOKIE))
    }

    fn admin_login_request(body: String) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/admin/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, TEST_ADMIN_ORIGIN)
            .header(header::COOKIE, format!("csrf_token={TEST_CSRF_COOKIE}"))
            .extension(crate::test_support::connect_info())
            .body(Body::from(body))
            .expect("request")
    }

    // ── login_ip_key ─────────────────────────────────────────────────────────

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

    // ── is_login_locked ──────────────────────────────────────────────────────

    #[test]
    fn fresh_ip_is_not_locked() {
        let key = login_ip_key("test-fresh-ip-not-in-map");
        assert!(!is_login_locked(&key));
    }

    #[test]
    fn locked_after_exceeding_fail_limit() {
        // Use a unique key so parallel tests don't interfere
        let key = login_ip_key("test-lock-unique-99887766");
        // Clean up any residue from a previous run
        ADMIN_LOGIN_FAILS.remove(&key);

        let now = login_now_secs();
        // Insert exactly LOGIN_FAIL_LIMIT failures within the window
        ADMIN_LOGIN_FAILS.insert(key.clone(), (LOGIN_FAIL_LIMIT, now));
        assert!(is_login_locked(&key));

        // Cleanup
        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[test]
    fn not_locked_below_fail_limit() {
        let key = login_ip_key("test-below-limit-11223344");
        ADMIN_LOGIN_FAILS.remove(&key);

        let now = login_now_secs();
        ADMIN_LOGIN_FAILS.insert(key.clone(), (LOGIN_FAIL_LIMIT - 1, now));
        assert!(!is_login_locked(&key));

        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[test]
    fn expired_window_is_not_locked() {
        let key = login_ip_key("test-expired-window-55667788");
        ADMIN_LOGIN_FAILS.remove(&key);

        // window_start far in the past, beyond LOGIN_FAIL_WINDOW
        let old_ts = login_now_secs().saturating_sub(LOGIN_FAIL_WINDOW + 60);
        ADMIN_LOGIN_FAILS.insert(key.clone(), (LOGIN_FAIL_LIMIT + 10, old_ts));
        assert!(!is_login_locked(&key));

        ADMIN_LOGIN_FAILS.remove(&key);
    }

    #[tokio::test]
    async fn locked_out_admin_login_rerenders_login_form_with_specific_message() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let ip_key = login_ip_key("127.0.0.1");
        ADMIN_LOGIN_FAILS.remove(&ip_key);
        ADMIN_LOGIN_FAILS.insert(ip_key.clone(), (LOGIN_FAIL_LIMIT, login_now_secs()));

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(format!(
                "username=admin&password=wrong&_csrf={}",
                signed_admin_csrf()
            )))
            .await
            .expect("response");

        ADMIN_LOGIN_FAILS.remove(&ip_key);

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(body.contains("Too many failed admin login attempts."));
    }

    #[tokio::test]
    async fn admin_login_sets_session_cookie_for_valid_credentials() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(format!(
                "username=admin&password=hunter2&_csrf={}",
                signed_admin_csrf()
            )))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let session_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .expect("session cookie");
        assert!(session_cookie.contains("HttpOnly"));
        assert!(session_cookie.contains("SameSite=Lax"));
        let csrf_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains("csrf_token="))
            .expect("csrf cookie");
        assert!(csrf_cookie.contains("SameSite=Strict"));
        assert!(!csrf_cookie.contains("csrf_token=csrf123"));
    }

    #[tokio::test]
    async fn admin_login_rotates_csrf_cookie_on_success() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(format!(
                "username=admin&password=hunter2&_csrf={}",
                signed_admin_csrf()
            )))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let csrf_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains("csrf_token="))
            .expect("csrf cookie");
        assert!(!csrf_cookie.contains("csrf_token=csrf123"));
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
    async fn admin_logout_clears_csrf_cookie_and_session_cookie() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            let admin_id =
                crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_session(
                &conn,
                "session123",
                admin_id,
                chrono::Utc::now().timestamp() + 3600,
            )
            .expect("create session");
        }

        let router = Router::new()
            .route("/admin/logout", post(super::admin_logout))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "return_to=/admin&_csrf={}",
                        signed_admin_session_csrf("session123")
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let set_cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert!(set_cookies
            .iter()
            .any(|cookie| cookie.contains("csrf_token=;")));
        assert!(set_cookies
            .iter()
            .any(|cookie| cookie.contains(&format!("{}=;", super::super::SESSION_COOKIE))));
    }

    #[tokio::test]
    async fn admin_login_marks_session_cookie_secure_for_https_tunnel_origin() {
        let state = crate::test_support::app_state();
        clear_login_fails(&login_ip_key("127.0.0.1"));
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let (host, origin) = if crate::config::CONFIG.tls.enabled {
            let host = format!("demo.serveo.net:{}", crate::config::CONFIG.tls.port);
            let origin = format!("https://{host}");
            (host, origin)
        } else {
            ("localhost".to_string(), TEST_ADMIN_ORIGIN.to_string())
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let session_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .expect("session cookie");
        assert_eq!(
            session_cookie.contains("Secure"),
            crate::config::CONFIG.https_cookies
        );
    }

    #[tokio::test]
    async fn insecure_admin_login_redirects_through_bootstrap() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("location header");
        assert!(location.starts_with("/admin/panel?bootstrap="));
    }

    #[tokio::test]
    async fn admin_login_rejects_raw_readable_csrf_cookie_without_signed_form_token() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(admin_login_request(
                "username=admin&password=hunter2&_csrf=csrf123".to_string(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_login_rejects_same_host_different_port_origin() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_login_rejects_same_host_different_scheme_origin() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "example.test")
                    .header(header::ORIGIN, "https://example.test")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_login_rejects_missing_origin_on_state_changing_post() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
            .with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_login_accepts_null_origin_on_loopback_host() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn admin_login_accepts_loopback_alias_origin_match() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn admin_login_accepts_ipv6_loopback_url() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn admin_login_accepts_null_origin_with_same_origin_referer_on_https_tunnel() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let password_hash =
                crate::utils::crypto::hash_password("hunter2").expect("hash password");
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/admin/login", post(super::admin_login))
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
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "username=admin&password=hunter2&_csrf={}",
                        signed_admin_csrf()
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
    }
}
