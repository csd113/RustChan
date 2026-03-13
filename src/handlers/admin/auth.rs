// handlers/admin/auth.rs
//
// Admin authentication: login, logout, session management.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. GET  /admin       → redirect to panel if already logged in, else show login form
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Brute-force protection (CRIT-6):
//   After LOGIN_FAIL_LIMIT failed attempts within LOGIN_FAIL_WINDOW seconds, the IP is
//   locked out for the remainder of that window.  On success the counter is cleared.
//   Keys are SHA-256(IP) to avoid retaining raw addresses in memory (CRIT-5).

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
    templates,
    utils::crypto::{new_session_id, verify_password},
};
use axum::{
    extract::{Form, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use dashmap::DashMap;
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use time;
use tracing::{info, warn};

// ─── CRIT-6: Admin login brute-force lockout ──────────────────────────────────
//
// After LOGIN_FAIL_LIMIT failed attempts within LOGIN_FAIL_WINDOW seconds the
// IP is locked out for the remainder of that window.  On success the counter
// is cleared immediately so a genuine admin is never self-locked.
//
// Keys are SHA-256(IP) to avoid retaining raw addresses in memory (CRIT-5).

const LOGIN_FAIL_LIMIT: u32 = 5;
const LOGIN_FAIL_WINDOW: u64 = 900; // 15 minutes

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
#[allow(clippy::arithmetic_side_effects)]
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

fn require_admin_sync(jar: &CookieJar, pool: &crate::db::DbPool) -> Result<i64> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;

    let conn = pool.get()?;
    let session = db::get_session(&conn, &session_id)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;

    Ok(session.admin_id)
}

/// Public helper — returns true if the jar contains a valid admin session.
/// Used by other handlers to conditionally show admin controls.
/// FIX[HIGH-2]/[HIGH-3]: Callers must invoke this from inside `spawn_blocking`.
#[allow(dead_code)]
pub fn is_admin_session(jar: &CookieJar, pool: &crate::db::DbPool) -> bool {
    require_admin_sync(jar, pool).is_ok()
}

// ─── GET /admin ───────────────────────────────────────────────────────────────

pub async fn admin_index(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    // FIX[HIGH-3]: Move DB I/O into spawn_blocking.
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

    let (jar, csrf) = ensure_csrf(jar);
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

#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn admin_login(
    State(state): State<AppState>,
    jar: CookieJar,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    Form(form): Form<LoginForm>,
) -> Result<Response> {
    // CRIT-6: Reject IPs that are currently locked out due to repeated failures.
    let ip_key = login_ip_key(&client_ip);
    if is_login_locked(&ip_key) {
        warn!(
            "Admin login blocked (brute-force lockout) for ip_prefix={}",
            &ip_key[..8]
        );
        return Err(AppError::RateLimited);
    }

    // CSRF check
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let username = form.username.trim().to_string();
    if username.is_empty() || username.len() > 64 {
        let (jar, csrf) = ensure_csrf(jar);
        let boards = tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || {
                let conn = pool.get()?;
                db::get_all_boards(&conn)
            }
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        return Ok((
            jar,
            Html(templates::admin_login_page(
                Some("Invalid username."),
                &csrf,
                &boards,
            )),
        )
            .into_response());
    }

    let pool = state.db.clone();
    let password = form.password.clone();

    // FIX[HIGH-3]: Argon2 verification is CPU-intensive; always use spawn_blocking.
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
            warn!("Failed admin login attempt for '{}'", form.username.trim());
            let (jar, csrf) = ensure_csrf(jar);
            let boards = tokio::task::spawn_blocking({
                let pool = state.db.clone();
                move || {
                    let conn = pool.get()?;
                    db::get_all_boards(&conn)
                }
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
            // CRIT-6: Record failed attempt and check if now locked.
            let fails = record_login_fail(&ip_key);
            warn!(
                "Failed admin login for '{}' (attempt {}/{})",
                form.username.trim(),
                fails,
                LOGIN_FAIL_LIMIT
            );
            Ok((
                jar,
                Html(templates::admin_login_page(
                    Some("Invalid username or password."),
                    &csrf,
                    &boards,
                )),
            )
                .into_response())
        }
        Some(admin_id) => {
            // CRIT-6: Successful login — reset any failure counter.
            clear_login_fails(&ip_key);
            // Create session (FIX[HIGH-3]: in spawn_blocking)
            let session_id = new_session_id();
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
            cookie.set_same_site(SameSite::Strict);
            cookie.set_path("/");
            // FIX[MEDIUM-11]: Derive Secure flag from config; true when CHAN_HTTPS_COOKIES=true.
            cookie.set_secure(CONFIG.https_cookies);
            // FIX[HIGH-1]: Set Max-Age so browsers expire the cookie after the
            // configured session lifetime instead of persisting it indefinitely.
            cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            info!("Admin {admin_id} logged in");
            Ok((jar.add(cookie), Redirect::to("/admin/panel")).into_response())
        }
    }
}

// ─── POST /admin/logout ───────────────────────────────────────────────────────

pub async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<super::CsrfOnly>,
) -> Result<Response> {
    // Verify CSRF to prevent forced-logout attacks
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    if let Some(session_cookie) = jar.get(super::SESSION_COOKIE) {
        let session_id = session_cookie.value().to_string();
        // FIX[HIGH-3]: DB call in spawn_blocking
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
    let jar = jar.remove(Cookie::from(super::SESSION_COOKIE));
    // Redirect back to the page where logout was triggered, or fall back to login.
    // FIX[HIGH-4]: Reject backslash (and its percent-encoded form %5C) in
    // addition to the existing checks.  On some browsers /\\evil.com and
    // /%5Cevil.com are treated as protocol-relative redirects to evil.com.
    let destination = form
        .return_to
        .as_deref()
        .filter(|s| {
            s.starts_with('/')
                && !s.contains("//")
                && !s.contains("..")
                && !s.contains('\\')
                && !s.to_ascii_lowercase().contains("%5c")
        })
        .unwrap_or("/admin");
    Ok((jar, Redirect::to(destination)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
