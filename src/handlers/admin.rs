// handlers/admin.rs
//
// Admin panel. All routes require a valid session cookie.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. All /admin/* routes → check session cookie → get session from DB → proceed
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Session cookie: HTTPOnly (not readable by JS), SameSite=Strict (prevents CSRF).
// Secure=true when CHAN_HTTPS_COOKIES=true (default: same as CHAN_BEHIND_PROXY).
//
// FIX[HIGH-3] + FIX[MEDIUM-12]: All admin handlers now wrap DB and file I/O in
// spawn_blocking to avoid blocking the Tokio event loop. Direct DB calls from
// async context were stalling worker threads under concurrent load.

use crate::{
    config::CONFIG,
    db::{self, DbPool},
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
    models::BackupInfo,
    templates,
    utils::crypto::{new_session_id, verify_password},
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::io::{Seek, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use time;
use tokio_util::io::ReaderStream;
use tracing::{info, warn};

const SESSION_COOKIE: &str = "chan_admin_session";

// ─── CRIT-6: Admin login brute-force lockout ──────────────────────────────────
//
// After LOGIN_FAIL_LIMIT failed attempts within LOGIN_FAIL_WINDOW seconds the
// IP is locked out for the remainder of that window.  On success the counter
// is cleared immediately so a genuine admin is never self-locked.
//
// Keys are SHA-256(IP) to avoid retaining raw addresses in memory (CRIT-5).

const LOGIN_FAIL_LIMIT: u32 = 5;
const LOGIN_FAIL_WINDOW: u64 = 900; // 15 minutes

/// ip_hash → (fail_count, window_start_secs)
static ADMIN_LOGIN_FAILS: Lazy<DashMap<String, (u32, u64)>> = Lazy::new(DashMap::new);
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
        *count += 1;
    }
    *count
}

/// Clear failure counter after a successful login.
fn clear_login_fails(ip_key: &str) {
    ADMIN_LOGIN_FAILS.remove(ip_key);
    // Opportunistically prune stale entries every ~15 min.
    let now = login_now_secs();
    let last = LOGIN_CLEANUP_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) > 900
        && LOGIN_CLEANUP_SECS
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        ADMIN_LOGIN_FAILS.retain(|_, (_, ws)| now.saturating_sub(*ws) <= LOGIN_FAIL_WINDOW);
    }
}

/// Verify admin session. Returns the admin_id if valid.
/// NOTE: This function performs blocking DB I/O. Only call it from within a
/// spawn_blocking closure or synchronous (non-async) context.
#[allow(dead_code)]
fn require_admin_sync(jar: &CookieJar, pool: &DbPool) -> Result<i64> {
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;

    let conn = pool.get()?;
    let session = db::get_session(&conn, &session_id)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;

    Ok(session.admin_id)
}

/// Public helper — returns true if the jar contains a valid admin session.
/// Used by other handlers to conditionally show admin controls.
/// FIX[HIGH-2]/[HIGH-3]: Callers must invoke this from inside spawn_blocking.
#[allow(dead_code)]
pub fn is_admin_session(jar: &CookieJar, pool: &DbPool) -> bool {
    require_admin_sync(jar, pool).is_ok()
}

pub async fn admin_index(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    // FIX[HIGH-3]: Move DB I/O into spawn_blocking.
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());

    let (is_logged_in, boards) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(bool, Vec<crate::models::Board>)> {
            let conn = pool.get()?;
            let logged_in = session_id
                .as_deref()
                .map(|sid| db::get_session(&conn, sid).ok().flatten().is_some())
                .unwrap_or(false);
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
    _csrf: Option<String>,
}

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
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
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

            let mut cookie = Cookie::new(SESSION_COOKIE, session_id);
            cookie.set_http_only(true);
            cookie.set_same_site(SameSite::Strict);
            cookie.set_path("/");
            // FIX[MEDIUM-11]: Derive Secure flag from config; true when CHAN_HTTPS_COOKIES=true.
            cookie.set_secure(CONFIG.https_cookies);
            // FIX[HIGH-1]: Set Max-Age so browsers expire the cookie after the
            // configured session lifetime instead of persisting it indefinitely.
            cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            info!("Admin {} logged in", admin_id);
            Ok((jar.add(cookie), Redirect::to("/admin/panel")).into_response())
        }
    }
}

// ─── POST /admin/logout ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CsrfOnly {
    _csrf: Option<String>,
    return_to: Option<String>,
}

pub async fn admin_logout(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CsrfOnly>,
) -> Result<Response> {
    // Verify CSRF to prevent forced-logout attacks
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    if let Some(session_cookie) = jar.get(SESSION_COOKIE) {
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
    let jar = jar.remove(Cookie::from(SESSION_COOKIE));
    // Redirect back to the page where logout was triggered, or fall back to login.
    // FIX[HIGH-4]: Reject backslash (and its percent-encoded form %5C) in
    // addition to the existing checks.  On some browsers /\evil.com and
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

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

/// Query params accepted by GET /admin/panel.
/// All fields are optional — missing = no flash message.
#[derive(Deserialize, Default)]
pub struct AdminPanelQuery {
    /// Set by board_restore on success: the short_name of the restored board.
    pub board_restored: Option<String>,
    /// Set by board_restore / restore_saved_board_backup on failure.
    pub restore_error: Option<String>,
    /// Set by update_site_settings on success.
    pub settings_saved: Option<String>,
}

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<AdminPanelQuery>,
) -> Result<(CookieJar, Html<String>)> {
    // FIX[HIGH-3]: Move auth check and all DB calls into spawn_blocking.
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let (jar, csrf) = ensure_csrf(jar);
    let csrf_clone = csrf.clone();

    // Build the flash message from query params before entering spawn_blocking.
    let flash: Option<(bool, String)> = if let Some(err) = params.restore_error {
        Some((true, format!("Restore failed: {}", err)))
    } else if let Some(board) = params.board_restored {
        Some((false, format!("Board /{board}/ restored successfully.")))
    } else if params.settings_saved.is_some() {
        Some((false, "Site settings saved.".to_string()))
    } else {
        None
    };

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            // Auth check inside blocking task
            let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
            db::get_session(&conn, &sid)?
                .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;

            let boards = db::get_all_boards(&conn)?;
            let bans = db::list_bans(&conn)?;
            let filters = db::get_word_filters(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            let reports = db::get_open_reports(&conn)?;
            let appeals = db::get_open_ban_appeals(&conn)?;
            let site_name = db::get_site_name(&conn);
            let site_subtitle = db::get_site_subtitle(&conn);
            let default_theme = db::get_default_user_theme(&conn);

            // Collect saved backup file lists (read from disk, not DB).
            let full_backups = list_backup_files(&full_backup_dir());
            let board_backups_list = list_backup_files(&board_backup_dir());

            let db_size_bytes = db::get_db_size_bytes(&conn).unwrap_or(0);

            // Read the tor onion address from the hostname file if tor is enabled.
            let tor_address: Option<String> = if CONFIG.enable_tor_support {
                let data_dir = std::path::PathBuf::from(&CONFIG.database_path)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                let hostname_path = data_dir.join("tor_hidden_service").join("hostname");
                std::fs::read_to_string(&hostname_path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            };

            let flash_ref = flash.as_ref().map(|(is_err, msg)| (*is_err, msg.as_str()));

            Ok(templates::admin_panel_page(
                &boards,
                &bans,
                &filters,
                collapse_greentext,
                &csrf_clone,
                &full_backups,
                &board_backups_list,
                db_size_bytes,
                &reports,
                &appeals,
                &site_name,
                &site_subtitle,
                &default_theme,
                tor_address.as_deref(),
                flash_ref,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /admin/board/create ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateBoardForm {
    short_name: String,
    name: String,
    description: String,
    nsfw: Option<String>,
    _csrf: Option<String>,
}

pub async fn create_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateBoardForm>,
) -> Result<Response> {
    // FIX[HIGH-3]: auth + DB write in spawn_blocking
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let short = form
        .short_name
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();

    if short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let nsfw = form.nsfw.as_deref() == Some("1");
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form
        .description
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::create_board(&conn, &short, &name, &description, nsfw)?;
            info!("Admin created board /{}/", short);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/board/delete ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardIdForm {
    board_id: i64,
    _csrf: Option<String>,
}

pub async fn delete_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardIdForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            // Fetch the board's short_name before deletion so we can remove
            // its upload directory entirely after cleaning tracked files.
            let short_name: Option<String> = conn
                .query_row(
                    "SELECT short_name FROM boards WHERE id = ?1",
                    rusqlite::params![form.board_id],
                    |r| r.get(0),
                )
                .ok();

            // delete_board returns all file paths for posts in this board.
            let paths = db::delete_board(&conn, form.board_id)?;

            // Delete every tracked file and thumbnail from disk.
            for p in &paths {
                crate::utils::files::delete_file(&upload_dir, p);
            }

            // Remove the entire board upload directory — handles the thumbs/
            // sub-directory and any orphaned/untracked files too.
            if let Some(short) = short_name {
                let board_dir = PathBuf::from(&upload_dir).join(&short);
                if board_dir.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&board_dir) {
                        warn!("Could not remove board dir {:?}: {}", board_dir, e);
                    }
                }
            }

            info!(
                "Admin deleted board id={} ({} file(s) removed)",
                form.board_id,
                paths.len()
            );
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/thread/action ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ThreadActionForm {
    thread_id: i64,
    board: String,
    action: String, // "sticky", "unsticky", "lock", "unlock"
    _csrf: Option<String>,
}

pub async fn thread_action(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ThreadActionForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    // Validate action before spawning to give early error
    match form.action.as_str() {
        "sticky" | "unsticky" | "lock" | "unlock" | "archive" => {}
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    }

    let action = form.action.clone();
    let thread_id = form.thread_id;
    let board_for_log = form.board.clone();
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;
            match action.as_str() {
                "sticky" => db::set_thread_sticky(&conn, thread_id, true)?,
                "unsticky" => db::set_thread_sticky(&conn, thread_id, false)?,
                "lock" => db::set_thread_locked(&conn, thread_id, true)?,
                "unlock" => db::set_thread_locked(&conn, thread_id, false)?,
                "archive" => db::set_thread_archived(&conn, thread_id, true)?,
                _ => {}
            }
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                &action,
                "thread",
                Some(thread_id),
                &board_for_log,
                "",
            );
            info!("Admin {} thread {}", action, thread_id);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // FIX[MEDIUM-10]: Use the board name from the DB (via the thread's board_id),
    // not the user-supplied form.board, to prevent path-confusion redirects.
    let redirect_url = {
        let pool = state.db.clone();
        let board_name = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = pool.get()?;
            let thread = db::get_thread(&conn, thread_id)?;
            if let Some(t) = thread {
                let boards = db::get_all_boards(&conn)?;
                if let Some(b) = boards.iter().find(|b| b.id == t.board_id) {
                    return Ok(b.short_name.clone());
                }
            }
            // Fallback: sanitize the user-supplied board name to prevent open-redirect.
            // Only allow alphanumeric characters (matching the board short_name format).
            let safe: String = form
                .board
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            Ok(safe)
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        // After archiving, send to the board archive; for all other actions
        // stay on the thread.
        if form.action == "archive" {
            format!("/{}/archive", board_name)
        } else {
            format!("/{}/thread/{}", board_name, form.thread_id)
        }
    };

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── POST /admin/post/delete ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AdminDeletePostForm {
    post_id: i64,
    board: String,
    _csrf: Option<String>,
}

pub async fn admin_delete_post(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AdminDeletePostForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let post_id = form.post_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            // FIX[MEDIUM-10]: Resolve board name from DB, not user-supplied form field.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_all_boards(&conn)?
                .into_iter()
                .find(|b| b.id == post.board_id)
                .map(|b| b.short_name)
                .unwrap_or_else(|| {
                    form.board
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect()
                });

            let thread_id = post.thread_id;
            let is_op = post.is_op;

            let paths = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, post_id)?
            };

            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            let action = if is_op {
                "delete_thread"
            } else {
                "delete_post"
            };
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                action,
                "post",
                Some(post_id),
                &board_name,
                &post.body.chars().take(80).collect::<String>(),
            );
            info!("Admin deleted post {}", post_id);
            // Return board_name + thread context so we can redirect back to the thread.
            // If the post was an OP, redirect to the board index (thread is gone).
            if is_op {
                Ok(format!("/{}", board_name))
            } else {
                Ok(format!("/{}/thread/{}", board_name, thread_id))
            }
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_board).into_response())
}

// ─── POST /admin/thread/delete ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AdminDeleteThreadForm {
    thread_id: i64,
    board: String,
    _csrf: Option<String>,
}

pub async fn admin_delete_thread(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AdminDeleteThreadForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let thread_id = form.thread_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;

            // FIX[MEDIUM-10]: Resolve board name from DB.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_thread(&conn, thread_id)?
                .and_then(|t| {
                    db::get_all_boards(&conn)
                        .ok()?
                        .into_iter()
                        .find(|b| b.id == t.board_id)
                        .map(|b| b.short_name)
                })
                .unwrap_or_else(|| {
                    form.board
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect()
                });

            let paths = db::delete_thread(&conn, thread_id)?;
            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "delete_thread",
                "thread",
                Some(thread_id),
                &board_name,
                "",
            );
            info!("Admin deleted thread {}", thread_id);
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{}", redirect_board)).into_response())
}

// ─── POST /admin/ban/add ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddBanForm {
    ip_hash: String,
    reason: String,
    duration_hours: Option<i64>,
    _csrf: Option<String>,
}

pub async fn add_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AddBanForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let expires_at = form
        .duration_hours
        .filter(|&h| h > 0)
        // Cap at 87_600 hours (10 years) to prevent overflow in h * 3600.
        // Permanent bans are represented by None (duration_hours absent or zero).
        .map(|h| Utc::now().timestamp() + h.min(87_600).saturating_mul(3600));

    let ip_hash_log = form.ip_hash.chars().take(8).collect::<String>();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;
            db::add_ban(&conn, &form.ip_hash, &form.reason, expires_at)?;
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "ban",
                "ban",
                None,
                "",
                &format!("ip_hash={}… reason={}", &ip_hash_log, form.reason),
            );
            info!("Admin added ban for ip_hash {}…", ip_hash_log);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/ban/remove ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BanIdForm {
    ban_id: i64,
    _csrf: Option<String>,
}

pub async fn remove_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BanIdForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::remove_ban(&conn, form.ban_id)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/post/ban-delete ──────────────────────────────────────────────
// Inline ban + delete from the per-post admin toolbar.
// Bans the post author's IP hash, deletes the post, then redirects back to
// the thread (or the board index if the OP is deleted).

#[derive(Deserialize)]
pub struct BanDeleteForm {
    post_id: i64,
    ip_hash: String,
    board: String,
    thread_id: i64,
    is_op: Option<String>,
    reason: Option<String>,
    duration_hours: Option<i64>,
    _csrf: Option<String>,
}

pub async fn admin_ban_and_delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BanDeleteForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let reason = form
        .reason
        .as_deref()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "Rule violation".to_string());

    let expires_at = form
        .duration_hours
        .filter(|&h| h > 0)
        .map(|h| chrono::Utc::now().timestamp() + h.min(87_600).saturating_mul(3600));

    let ip_hash_log = form.ip_hash.chars().take(8).collect::<String>();
    let post_id = form.post_id;
    let board_short = form.board.clone();
    let thread_id = form.thread_id;
    let is_op = form.is_op.as_deref() == Some("1");

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;

            // Ban first so the IP cannot re-post before the delete lands
            db::add_ban(&conn, &form.ip_hash, &reason, expires_at)?;
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "ban",
                "ban",
                None,
                &board_short,
                &format!("inline ban — ip_hash={}… reason={}", &ip_hash_log, reason),
            );

            // Delete post (or whole thread if OP)
            if is_op {
                let paths = db::delete_thread(&conn, thread_id)?;
                for p in paths {
                    crate::utils::files::delete_file(&crate::config::CONFIG.upload_dir, &p);
                }
                let _ = db::log_mod_action(
                    &conn,
                    admin_id,
                    &admin_name,
                    "delete_thread",
                    "thread",
                    Some(thread_id),
                    &board_short,
                    "",
                );
            } else {
                let paths = db::delete_post(&conn, post_id)?;
                for p in paths {
                    crate::utils::files::delete_file(&crate::config::CONFIG.upload_dir, &p);
                }
                let _ = db::log_mod_action(
                    &conn,
                    admin_id,
                    &admin_name,
                    "delete_post",
                    "post",
                    Some(post_id),
                    &board_short,
                    "",
                );
            }

            info!(
                "Admin ban+delete: post={} ip_hash={}… board={}",
                post_id, ip_hash_log, board_short
            );
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // FIX[A1]: form.board is user-supplied; sanitise to alphanumeric only before
    // embedding in the redirect URL to prevent open-redirect via "//" prefixes.
    let safe_board: String = form
        .board
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    // If OP was deleted, the thread is gone — send to board index
    let redirect = if is_op {
        format!("/{}", safe_board)
    } else {
        format!("/{}/thread/{}#p{}", safe_board, thread_id, post_id)
    };
    Ok(Redirect::to(&redirect).into_response())
}

// ─── POST /admin/appeal/dismiss ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AppealActionForm {
    appeal_id: i64,
    ip_hash: Option<String>,
    _csrf: Option<String>,
}

pub async fn dismiss_appeal(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AppealActionForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::dismiss_ban_appeal(&conn, form.appeal_id)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel#appeals").into_response())
}

// ─── POST /admin/appeal/accept ────────────────────────────────────────────────

pub async fn accept_appeal(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AppealActionForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;
            let ip = form.ip_hash.as_deref().unwrap_or("");
            db::accept_ban_appeal(&conn, form.appeal_id, ip)?;
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "accept_appeal",
                "ban",
                None,
                "",
                &format!("appeal {} — ip unban", form.appeal_id),
            );
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel#appeals").into_response())
}

#[derive(Deserialize)]
pub struct AddFilterForm {
    pattern: String,
    replacement: String,
    _csrf: Option<String>,
}

pub async fn add_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AddFilterForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    if form.pattern.trim().is_empty() {
        return Err(AppError::BadRequest("Pattern cannot be empty.".into()));
    }

    let pattern = form.pattern.trim().chars().take(256).collect::<String>();
    let replacement = form
        .replacement
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::add_word_filter(&conn, &pattern, &replacement)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/filter/remove ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FilterIdForm {
    filter_id: i64,
    _csrf: Option<String>,
}

pub async fn remove_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<FilterIdForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::remove_word_filter(&conn, form.filter_id)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/board/settings ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardSettingsForm {
    board_id: i64,
    name: String,
    description: String,
    bump_limit: Option<String>,
    max_threads: Option<String>,
    nsfw: Option<String>,
    allow_images: Option<String>,
    allow_video: Option<String>,
    allow_audio: Option<String>,
    allow_tripcodes: Option<String>,
    allow_editing: Option<String>,
    edit_window_secs: Option<String>,
    allow_archive: Option<String>,
    allow_video_embeds: Option<String>,
    allow_captcha: Option<String>,
    post_cooldown_secs: Option<String>,
    _csrf: Option<String>,
}

pub async fn update_board_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let bump_limit = form
        .bump_limit
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(500)
        .clamp(1, 10_000);
    let max_threads = form
        .max_threads
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150)
        .clamp(1, 1_000);
    let edit_window_secs = form
        .edit_window_secs
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(300)
        .clamp(0, 86_400); // 0 = disabled, max 24 h
    let post_cooldown_secs = form
        .post_cooldown_secs
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 3_600); // 0 = disabled, max 1 hour

    // Enforce server-side length limits on free-text fields
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form
        .description
        .trim()
        .chars()
        .take(256)
        .collect::<String>();
    let board_id = form.board_id;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::update_board_settings(
                &conn,
                board_id,
                &name,
                &description,
                form.nsfw.as_deref() == Some("1"),
                bump_limit,
                max_threads,
                form.allow_images.as_deref() == Some("1"),
                form.allow_video.as_deref() == Some("1"),
                form.allow_audio.as_deref() == Some("1"),
                form.allow_tripcodes.as_deref() == Some("1"),
                edit_window_secs,
                form.allow_editing.as_deref() == Some("1"),
                form.allow_archive.as_deref() == Some("1"),
                form.allow_video_embeds.as_deref() == Some("1"),
                form.allow_captcha.as_deref() == Some("1"),
                post_cooldown_secs,
            )?;
            info!("Admin updated settings for board id={}", board_id);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── GET /admin/backup ────────────────────────────────────────────────────────

/// Stream a full zip backup of the database + all uploaded files.
///
/// MEM-FIX: The zip is built to a NamedTempFile on disk (not a Vec<u8> in
/// RAM), so peak heap usage is O(compression-buffer) not O(zip-size).
/// The response body is streamed from disk in 64 KiB chunks via ReaderStream.
pub async fn admin_backup(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);

            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db = temp_dir.join(format!("chan_backup_{}.db", tmp_id));
            let temp_db_str = temp_db
                .to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path is non-UTF-8")))?
                .replace('\'', "''");

            conn.execute_batch(&format!("VACUUM INTO '{}'", temp_db_str))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("VACUUM INTO failed: {}", e)))?;
            drop(conn);

            // Count files for progress bar before compressing.
            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            let uploads_base = std::path::Path::new(&upload_dir);
            let file_count = count_files_in_dir(uploads_base);
            // +1 for chan.db
            progress
                .files_total
                .store(file_count + 1, Ordering::Relaxed);

            // MEM-FIX: write zip directly to a NamedTempFile instead of Vec<u8>.
            let zip_tmp = tempfile::NamedTempFile::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp zip: {}", e)))?;
            {
                let out_file =
                    std::io::BufWriter::new(zip_tmp.as_file().try_clone().map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Clone temp file handle: {}", e))
                    })?);
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                progress.reset(crate::middleware::backup_phase::COMPRESS);
                progress
                    .files_total
                    .store(file_count + 1, Ordering::Relaxed);

                // ── Database snapshot (streamed, not read into RAM) ────────
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB entry: {}", e)))?;
                let mut db_src = std::fs::File::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {}", e)))?;
                let copied = std::io::copy(&mut db_src, &mut zip)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {}", e)))?;
                drop(db_src);
                let _ = std::fs::remove_file(&temp_db);
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);

                // ── Upload files (streamed file-by-file via io::copy) ──────
                if uploads_base.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
                }

                zip.finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
            }

            let file_size = zip_tmp.as_file().metadata().map(|m| m.len()).unwrap_or(0);

            // Persist the temp file (prevents auto-delete on drop).
            // We delete it manually in the background after serving.
            let (_, tmp_path_obj) = zip_tmp.into_parts();
            let final_path = tmp_path_obj
                .keep()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Persist temp zip: {}", e)))?;

            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-backup-{}.zip", ts);
            info!("Admin downloaded full backup ({} bytes on disk)", file_size);
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok((final_path, fname, file_size))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // MEM-FIX: Stream the zip file from disk in chunks — never load it all into heap.
    let file = tokio::fs::File::open(&tmp_path)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open backup for streaming: {}", e)))?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    // Schedule temp-file cleanup after a generous window so even slow clients finish.
    let cleanup_path = tmp_path;
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        let _ = tokio::fs::remove_file(cleanup_path).await;
    });

    use axum::http::header;
    let disposition = format!("attachment; filename=\"{}\"", filename);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

/// Count regular files (not directories) under `dir` recursively.
/// Used to initialise the progress bar's files_total before compression starts.
fn count_files_in_dir(dir: &std::path::Path) -> u64 {
    if !dir.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().fold(0u64, |acc, entry| {
        let p = entry.path();
        if p.is_dir() {
            acc + count_files_in_dir(&p)
        } else if p.is_file() {
            acc + 1
        } else {
            acc
        }
    })
}

/// Recursively add every file under `dir` into the zip as `uploads/{rel_path}`.
///
/// MEM-FIX: Uses std::io::copy with the zip writer directly, streaming each
/// file through a kernel buffer (~8 KiB) instead of reading the whole file
/// into a Vec<u8> first.  Peak RAM per file = io::copy's 8 KiB stack buffer.
///
/// Progress tracking: increments progress.files_done and progress.bytes_done
/// after each file is written to the zip.
fn add_dir_to_zip<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    base: &std::path::Path,
    dir: &std::path::Path,
    opts: zip::write::SimpleFileOptions,
    progress: &crate::middleware::BackupProgress,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("read_dir {}: {}", dir.display(), e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| AppError::Internal(anyhow::anyhow!("dir entry: {}", e)))?;
        let path = entry.path();

        let relative = path
            .strip_prefix(base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("strip_prefix: {}", e)))?;
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        let zip_path = format!("uploads/{}", rel_str);

        if path.is_dir() {
            zip.add_directory(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip dir: {}", e)))?;
            add_dir_to_zip(zip, base, &path, opts, progress)?;
        } else if path.is_file() {
            // MEM-FIX: open file, stream through io::copy — no Vec<u8> allocation.
            let mut src = std::fs::File::open(&path).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("open {}: {}", path.display(), e))
            })?;
            zip.start_file(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip file entry: {}", e)))?;
            let copied = std::io::copy(&mut src, zip).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("copy {} to zip: {}", path.display(), e))
            })?;
            progress.files_done.fetch_add(1, Ordering::Relaxed);
            progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
        }
    }
    Ok(())
}

// ─── POST /admin/restore ──────────────────────────────────────────────────────

/// Replace the live database with the contents of a backup zip.
///
/// Design — why we use SQLite's backup API instead of swapping files:
///
///   The r2d2 pool keeps up to 8 SQLite connections open permanently.  On
///   Linux, renaming a new file over chan.db does NOT update the connections
///   already open — they still hold file descriptors to the old inode.  File-
///   swapping therefore leaves the pool reading stale data until the process
///   restarts, and deleting the WAL while live connections are active can
///   corrupt the database.
///
///   `rusqlite::backup::Backup` wraps SQLite's sqlite3_backup_init() API,
///   which copies data directly into the destination connection's live file —
///   through the WAL, through the same file descriptors, safely.  After
///   run_to_completion() returns, every connection in the pool immediately
///   sees the restored data.  No file swapping, no WAL deletion, no restart
///   required.
///
/// Security:
///   • Admin session + CSRF required before any data is touched.
///   • Zip path-traversal entries (containing ".." or absolute paths) are
///     rejected.
///   • Only "chan.db" and "uploads/…" entries are extracted; everything else
///     is silently ignored.
///   • The uploaded DB is written to a temp file then opened read-only as the
///     backup source; it is deleted on success or failure.
pub async fn admin_restore(
    State(state): State<AppState>,
    jar: CookieJar,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());

    // FIX[A7]: Stream the uploaded zip to a NamedTempFile on disk instead of
    // buffering the entire upload into a Vec<u8>.  Full-site backups can be
    // several GiB; loading them entirely into the heap exhausts available memory.
    let mut zip_tmp: Option<tempfile::NamedTempFile> = None;
    let mut form_csrf: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        match field.name() {
            Some("_csrf") => {
                form_csrf = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                );
            }
            Some("backup_file") => {
                use tokio::io::AsyncWriteExt as _;
                let tmp = tempfile::NamedTempFile::new()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Tempfile: {}", e)))?;
                // Clone the underlying fd for async writing; the original
                // NamedTempFile retains ownership and the delete-on-drop guard.
                let std_clone = tmp
                    .as_file()
                    .try_clone()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Clone fd: {}", e)))?;
                let async_file = tokio::fs::File::from_std(std_clone);
                let mut writer = tokio::io::BufWriter::new(async_file);
                let mut field = field;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?
                {
                    writer
                        .write_all(&chunk)
                        .await
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write chunk: {}", e)))?;
                }
                writer
                    .flush()
                    .await
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush: {}", e)))?;
                zip_tmp = Some(tmp);
            }
            _ => {
                // Drain unknown fields so the multipart stream advances.
                let _ = field.bytes().await;
            }
        }
    }

    // CSRF check.
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form_csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let zip_tmp = zip_tmp.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
    // Determine size without reading into RAM: seeking to end gives the byte count.
    let zip_size = zip_tmp
        .as_file()
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Seek check: {}", e)))?;
    if zip_size == 0 {
        return Err(AppError::BadRequest(
            "Uploaded backup file is empty.".into(),
        ));
    }

    let upload_dir = CONFIG.upload_dir.clone();

    let fresh_sid: String = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            // ── Auth ──────────────────────────────────────────────────────
            // Hold this connection open for the entire restore so the pool
            // can't recycle it and open a fresh one mid-copy.
            let mut live_conn = pool.get()?;
            // Save admin_id now — we'll need it to create a new session
            // in the restored DB once the backup completes.
            let admin_id = require_admin_session_sid(&live_conn, session_id.as_deref())?;

            // ── Open the on-disk zip (FIX[A7]) ───────────────────────────
            // reopen() gives a fresh File descriptor seeked to position 0,
            // so ZipArchive can navigate entries without loading into RAM.
            let zip_file = zip_tmp
                .reopen()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip: {}", e)))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {}", e)))?;

            // Quick pre-flight: make sure there is a chan.db entry.
            // file_names() is a stable iterator available in zip 2+ and zip 8+.
            let has_db = archive.file_names().any(|n| n == "chan.db");
            if !has_db {
                return Err(AppError::BadRequest(
                    "Invalid backup: zip must contain 'chan.db' at the root.".into(),
                ));
            }

            // ── Single-pass extraction ────────────────────────────────────
            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db = temp_dir.join(format!("chan_restore_{}.db", tmp_id));
            let mut db_extracted = false;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip read [{}]: {}", i, e)))?;
                let name = entry.name().to_string();

                // Security: skip any path-traversal attempts.
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Restore: skipping suspicious zip entry '{}'", name);
                    continue;
                }

                if name == "chan.db" {
                    let mut out = std::fs::File::create(&temp_db)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp DB: {}", e)))?;
                    copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp DB: {}", e)))?;
                    db_extracted = true;

                } else if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() { continue; }
                    let target = PathBuf::from(&upload_dir).join(rel);

                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir {}: {}", target.display(), e)))?;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir parent: {}", e)))?;
                        }
                        let mut out = std::fs::File::create(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Create {}: {}", target.display(), e)))?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write {}: {}", target.display(), e)))?;
                    }
                }
            }
            drop(archive);

            if !db_extracted {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "chan.db was found in pre-flight but not extracted — corrupted zip?"
                )));
            }

            // ── SQLite backup API: copy temp DB → live DB ─────────────────
            let backup_result = (|| -> Result<()> {
                let src = rusqlite::Connection::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open backup source: {}", e)))?;
                use rusqlite::backup::Backup;
                let backup = Backup::new(&src, &mut live_conn)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup init: {}", e)))?;
                backup.run_to_completion(100, std::time::Duration::from_millis(0), None)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup copy: {}", e)))?;
                Ok(())
            })();

            let _ = std::fs::remove_file(&temp_db);
            backup_result?;

            // ── Re-issue session cookie ───────────────────────────────────
            //
            // The backup API just replaced the admin_sessions table with the
            // one from the backup file, so the browser's current session ID is
            // now invalid against the restored DB.  Create a fresh session for
            // the same admin_id so the redirect to /admin/panel succeeds.
            //
            // If admin_id doesn't exist in the restored DB (e.g. restoring
            // from a much older backup) the INSERT will fail with a FK error.
            // We catch that, log it, and return an empty string to signal that
            // the handler should redirect to the login page instead.
            let fresh_sid = new_session_id();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            match db::create_session(&live_conn, &fresh_sid, admin_id, expires_at) {
                Ok(_) => {
                    info!("Admin restore completed; new session issued for admin_id={}", admin_id);
                    Ok(fresh_sid)
                }
                Err(e) => {
                    warn!("Restore: could not create new session (admin_id={} may not exist in backup): {}", admin_id, e);
                    // Return empty string as a sentinel — handler will send to login.
                    Ok(String::new())
                }
            }
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // If we got a valid session ID back, replace the cookie and go to the
    // panel.  If not (admin didn't exist in the backup), go to login instead.
    if fresh_sid.is_empty() {
        let jar = jar.remove(Cookie::from(SESSION_COOKIE));
        return Ok((jar, Redirect::to("/admin")).into_response());
    }

    let mut new_cookie = Cookie::new(SESSION_COOKIE, fresh_sid);
    new_cookie.set_http_only(true);
    new_cookie.set_same_site(SameSite::Strict);
    new_cookie.set_path("/");
    new_cookie.set_secure(CONFIG.https_cookies);
    // FIX[A5]: Set Max-Age so the browser expires the cookie after the configured
    // session lifetime — matching the behaviour of the normal login handler.
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

// ─── CRIT-4: Zip decompression size limiter ────────────────────────────────────
//
// std::io::copy() has no bound on how much data it will write.  A malicious
// 1 KiB zip (a "zip bomb") can expand to gigabytes, exhausting disk or memory.
// copy_limited() caps the decompressed size of each entry.

/// Maximum bytes to extract from any single zip entry.
/// Set to 16 GiB — these are admin-only restore endpoints, so individual
/// entries (large videos, the SQLite DB) can legitimately be several GiB.
const ZIP_ENTRY_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Like `std::io::copy` but returns `InvalidData` if more than `max_bytes`
/// would be written.  Reads in 64 KiB chunks; aborts as soon as the limit
/// is exceeded so disk space is not wasted.
fn copy_limited<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> std::io::Result<u64> {
    let mut buf = [0u8; 65536];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Decompressed entry exceeds {} MiB limit — possible zip bomb",
                    max_bytes / 1024 / 1024
                ),
            ));
        }
        if let Some(slice) = buf.get(..n) {
            writer.write_all(slice)?;
        }
    }
    Ok(total)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Check CSRF using the cookie jar. Returns error on mismatch.
/// Verify admin session and also return the admin's username.
/// For use inside spawn_blocking closures.
fn require_admin_session_with_name(
    conn: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<(i64, String)> {
    let admin_id = require_admin_session_sid(conn, session_id)?;
    let name = db::get_admin_name_by_id(conn, admin_id)?.unwrap_or_else(|| "unknown".to_string());
    Ok((admin_id, name))
}

fn check_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let cookie_token = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(cookie_token.as_deref(), form_token.unwrap_or("")) {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    } else {
        Ok(())
    }
}

/// Verify admin session from a session ID string.
/// For use inside spawn_blocking closures where we have an open connection.
fn require_admin_session_sid(conn: &rusqlite::Connection, session_id: Option<&str>) -> Result<i64> {
    let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session = db::get_session(conn, sid)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;
    Ok(session.admin_id)
}

// ─── Backup directory helpers ────────────────────────────────────────────────

/// Returns the directory that contains chan.db (i.e. rustchan-data/).
fn db_dir() -> PathBuf {
    PathBuf::from(&CONFIG.database_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// rustchan-data/full-backups/
pub fn full_backup_dir() -> PathBuf {
    db_dir().join("full-backups")
}

/// rustchan-data/board-backups/
pub fn board_backup_dir() -> PathBuf {
    db_dir().join("board-backups")
}

/// List `.zip` files in `dir`, newest-filename-first.
fn list_backup_files(dir: &std::path::Path) -> Vec<BackupInfo> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") {
                continue;
            }
            if let (Some(name), Ok(meta)) = (
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string()),
                std::fs::metadata(&path),
            ) {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        let secs = d.as_secs() as i64;
                        #[allow(deprecated)]
                        chrono::DateTime::<Utc>::from_timestamp(secs, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                files.push(BackupInfo {
                    filename: name,
                    size_bytes: meta.len(),
                    modified,
                });
            }
        }
    }
    // Sort newest first (filename encodes timestamp for full/board backups).
    files.sort_by(|a, b| b.filename.cmp(&a.filename));
    files
}

// ─── POST /admin/backup/create ────────────────────────────────────────────────

/// Create a full backup and save it to rustchan-data/full-backups/.
///
/// MEM-FIX: The zip is written directly to the final destination file via a
/// BufWriter, so peak RAM usage is O(compression-buffer) not O(zip-size).
/// A `.tmp` suffix is used during writing; the file is renamed on success so
/// the backup list never shows a partial/corrupt zip.
pub async fn create_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CsrfOnly>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);

            // VACUUM INTO for a consistent snapshot.
            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db = temp_dir.join(format!("chan_backup_{}.db", tmp_id));
            let temp_db_str = temp_db
                .to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path non-UTF-8")))?
                .replace('\'', "''");

            conn.execute_batch(&format!("VACUUM INTO '{}'", temp_db_str))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {}", e)))?;
            drop(conn);

            // Count files for progress bar before compressing.
            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            let uploads_base = std::path::Path::new(&upload_dir);
            let file_count = count_files_in_dir(uploads_base);
            progress
                .files_total
                .store(file_count + 1, Ordering::Relaxed);

            // MEM-FIX: write zip directly to a .tmp file on disk, not a Vec<u8>.
            let backup_dir = full_backup_dir();
            std::fs::create_dir_all(&backup_dir).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("Create full-backups dir: {}", e))
            })?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-backup-{}.zip", ts);
            let final_path = backup_dir.join(&fname);
            let tmp_path = backup_dir.join(format!("{}.tmp", fname));

            {
                let out_file =
                    std::io::BufWriter::new(std::fs::File::create(&tmp_path).map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Create zip tmp: {}", e))
                    })?);
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                progress.reset(crate::middleware::backup_phase::COMPRESS);
                progress
                    .files_total
                    .store(file_count + 1, Ordering::Relaxed);

                // ── Database snapshot (streamed, not read into RAM) ────────
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB: {}", e)))?;
                let mut db_src = std::fs::File::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {}", e)))?;
                let copied = std::io::copy(&mut db_src, &mut zip)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {}", e)))?;
                drop(db_src);
                let _ = std::fs::remove_file(&temp_db);
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);

                // ── Upload files (streamed via io::copy) ───────────────────
                if uploads_base.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
                }

                let writer = zip
                    .finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
                // Flush the BufWriter so the OS buffer is committed to disk.
                writer
                    .into_inner()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush zip writer: {}", e)))?
                    .sync_all()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Sync zip file: {}", e)))?;
            }

            // Atomic rename: only becomes visible in the list when complete.
            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename backup: {}", e))
            })?;

            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            info!("Admin created full backup: {} ({} bytes)", fname, size);
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_created=1").into_response())
}

// ─── POST /admin/board/backup/create ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardBackupCreateForm {
    board_short: String,
    _csrf: Option<String>,
}

/// Create a board backup and save it to rustchan-data/board-backups/.
pub async fn create_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardBackupCreateForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let board_short = form
        .board_short
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();
    if board_short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            use board_backup_types::*;
            use rusqlite::params;

            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            let board: BoardRow = conn
                .query_row(
                    "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                             allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                             allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                             post_cooldown_secs, created_at
                      FROM boards WHERE short_name = ?1",
                    params![board_short],
                    |r| {
                        Ok(BoardRow {
                            id: r.get(0)?,
                            short_name: r.get(1)?,
                            name: r.get(2)?,
                            description: r.get(3)?,
                            nsfw: r.get::<_, i64>(4)? != 0,
                            max_threads: r.get(5)?,
                            bump_limit: r.get(6)?,
                            allow_images: r.get::<_, i64>(7)? != 0,
                            allow_video: r.get::<_, i64>(8)? != 0,
                            allow_audio: r.get::<_, i64>(9)? != 0,
                            allow_tripcodes: r.get::<_, i64>(10)? != 0,
                            edit_window_secs: r.get(11)?,
                            allow_editing: r.get::<_, i64>(12)? != 0,
                            allow_archive: r.get::<_, i64>(13)? != 0,
                            allow_video_embeds: r.get::<_, i64>(14)? != 0,
                            allow_captcha: r.get::<_, i64>(15)? != 0,
                            post_cooldown_secs: r.get(16)?,
                            created_at: r.get(17)?,
                        })
                    },
                )
                .map_err(|_| AppError::NotFound(format!("Board '{}' not found", board_short)))?;

            let board_id = board.id;

            let threads: Vec<ThreadRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, reply_count
                         FROM threads WHERE board_id = ?1 ORDER BY id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(ThreadRow {
                            id: r.get(0)?,
                            board_id: r.get(1)?,
                            subject: r.get(2)?,
                            created_at: r.get(3)?,
                            bumped_at: r.get(4)?,
                            locked: r.get::<_, i64>(5)? != 0,
                            sticky: r.get::<_, i64>(6)? != 0,
                            reply_count: r.get(7)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let posts: Vec<PostRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                                media_type, created_at, deletion_token, is_op
                         FROM posts WHERE board_id = ?1 ORDER BY id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PostRow {
                            id: r.get(0)?,
                            thread_id: r.get(1)?,
                            board_id: r.get(2)?,
                            name: r.get(3)?,
                            tripcode: r.get(4)?,
                            subject: r.get(5)?,
                            body: r.get(6)?,
                            body_html: r.get(7)?,
                            ip_hash: r.get(8)?,
                            file_path: r.get(9)?,
                            file_name: r.get(10)?,
                            file_size: r.get(11)?,
                            thumb_path: r.get(12)?,
                            mime_type: r.get(13)?,
                            media_type: r.get(14)?,
                            created_at: r.get(15)?,
                            deletion_token: r.get(16)?,
                            is_op: r.get::<_, i64>(17)? != 0,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let polls: Vec<PollRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT p.id, p.thread_id, p.question, p.expires_at, p.created_at
                         FROM polls p JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY p.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollRow {
                            id: r.get(0)?,
                            thread_id: r.get(1)?,
                            question: r.get(2)?,
                            expires_at: r.get(3)?,
                            created_at: r.get(4)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let poll_options: Vec<PollOptionRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT po.id, po.poll_id, po.text, po.position
                         FROM poll_options po
                         JOIN polls p ON p.id = po.poll_id
                         JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY po.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollOptionRow {
                            id: r.get(0)?,
                            poll_id: r.get(1)?,
                            text: r.get(2)?,
                            position: r.get(3)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let poll_votes: Vec<PollVoteRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash
                         FROM poll_votes pv
                         JOIN polls p ON p.id = pv.poll_id
                         JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY pv.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollVoteRow {
                            id: r.get(0)?,
                            poll_id: r.get(1)?,
                            option_id: r.get(2)?,
                            ip_hash: r.get(3)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let file_hashes: Vec<FileHashRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT DISTINCT fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at
                         FROM file_hashes fh
                         JOIN posts po ON po.file_path = fh.file_path
                         WHERE po.board_id = ?1 ORDER BY fh.created_at ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(FileHashRow {
                            sha256: r.get(0)?,
                            file_path: r.get(1)?,
                            thumb_path: r.get(2)?,
                            mime_type: r.get(3)?,
                            created_at: r.get(4)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let manifest = BoardBackupManifest {
                version: 1,
                board,
                threads,
                posts,
                polls,
                poll_options,
                poll_votes,
                file_hashes,
            };
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON: {}", e)))?;

            // MEM-FIX: write zip directly to a .tmp file on disk, not a Vec<u8>.
            let backup_dir = board_backup_dir();
            std::fs::create_dir_all(&backup_dir).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("Create board-backups dir: {}", e))
            })?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-board-{}-{}.zip", board_short, ts);
            let final_path = backup_dir.join(&fname);
            let tmp_path = backup_dir.join(format!("{}.tmp", fname));

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = count_files_in_dir(&board_upload_path);
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            // +1 for board.json manifest
            progress.files_total.store(file_count + 1, Ordering::Relaxed);

            {
                let out_file = std::io::BufWriter::new(
                    std::fs::File::create(&tmp_path).map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Create zip tmp: {}", e))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                zip.start_file("board.json", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip manifest: {}", e)))?;
                zip.write_all(&manifest_json)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write manifest: {}", e)))?;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(manifest_json.len() as u64, Ordering::Relaxed);

                if board_upload_path.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, &board_upload_path, opts, &progress)?;
                }

                let writer = zip
                    .finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
                writer
                    .into_inner()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush zip writer: {}", e)))?
                    .sync_all()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Sync zip file: {}", e)))?;
            }

            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename board backup: {}", e))
            })?;

            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            info!("Admin created board backup: {} ({} bytes)", fname, size);
            progress.phase.store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_created=1").into_response())
}

// ─── GET /admin/backup/download/{kind}/{filename} ────────────────────────────

/// Download a saved backup file.  `kind` must be "full" or "board".
///
/// MEM-FIX (original bug): The old implementation used `tokio::fs::read()`
/// which loaded the entire file into a Vec<u8> before beginning the HTTP
/// response.  For a 5 GiB backup on a slow connection that means 5 GiB of
/// heap held for the entire download duration.
///
/// The fix: open a tokio::fs::File and wrap it in a ReaderStream so Axum
/// sends the data in 64 KiB chunks pulled directly from the OS page cache.
/// Peak heap = one 64 KiB chunk; the rest stays on disk.
pub async fn download_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path((kind, filename)): axum::extract::Path<(String, String)>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());

    // Auth check.
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Validate filename — only allow safe characters to prevent path traversal.
    let safe_filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != filename || safe_filename.contains("..") {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    if !safe_filename.ends_with(".zip") {
        return Err(AppError::BadRequest(
            "Only .zip files can be downloaded.".into(),
        ));
    }

    let backup_dir = match kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
    };

    let path = backup_dir.join(&safe_filename);

    // Get file size for Content-Length (so the browser shows a progress bar).
    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::NotFound("Backup file not found.".into()))?
        .len();

    // MEM-FIX: stream the file in 64 KiB chunks instead of loading it all.
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    use axum::http::header;
    let disposition = format!("attachment; filename=\"{}\"", safe_filename);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

// ─── GET /admin/backup/progress ──────────────────────────────────────────────

/// Return current backup progress as JSON.  Polled by the admin panel JS.
///
/// Response: { phase: u64, files_done: u64, files_total: u64,
///              bytes_done: u64, bytes_total: u64 }
///
/// phase codes: 0=idle, 1=snapshot_db, 2=count_files, 3=compress, 4=save, 5=done
///
/// Auth is required to prevent any guest from watching backup progress.
pub async fn backup_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    use std::sync::atomic::Ordering::Relaxed;
    let p = &state.backup_progress;
    let json = format!(
        r#"{{"phase":{},"files_done":{},"files_total":{},"bytes_done":{},"bytes_total":{}}}"#,
        p.phase.load(Relaxed),
        p.files_done.load(Relaxed),
        p.files_total.load(Relaxed),
        p.bytes_done.load(Relaxed),
        p.bytes_total.load(Relaxed),
    );

    use axum::http::header;
    Ok((
        [(header::CONTENT_TYPE, "application/json".to_string())],
        json,
    )
        .into_response())
}

// ─── POST /admin/backup/delete ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteBackupForm {
    kind: String, // "full" or "board"
    filename: String,
    _csrf: Option<String>,
}

/// Delete a saved backup file from disk.
pub async fn delete_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteBackupForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    // Validate filename.
    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename || safe_filename.contains("..") {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    if !safe_filename.ends_with(".zip") {
        return Err(AppError::BadRequest(
            "Only .zip files can be deleted.".into(),
        ));
    }

    let backup_dir = match form.kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
    };

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let path = backup_dir.join(&safe_filename);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Delete backup: {}", e)))?;
                info!("Admin deleted backup file: {}", safe_filename);
            }
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_deleted=1").into_response())
}

// ─── POST /admin/backup/restore-saved ────────────────────────────────────────

#[derive(Deserialize)]
pub struct RestoreSavedForm {
    filename: String,
    _csrf: Option<String>,
}

/// Restore a full backup from a saved file in full-backups/.
pub async fn restore_saved_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RestoreSavedForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename
        || safe_filename.contains("..")
        || !safe_filename.ends_with(".zip")
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }

    let path = full_backup_dir().join(&safe_filename);
    // FIX[A3]: Do NOT read the file in the async context before auth is verified.
    // std::fs::read() blocks the Tokio runtime and an unauthenticated caller could
    // force the server to read gigabytes off disk before being rejected.  The read
    // is deferred into spawn_blocking where it runs only after the session check.
    let upload_dir = CONFIG.upload_dir.clone();

    let fresh_sid: String = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut live_conn = pool.get()?;
            // Auth check first — only read the (potentially huge) file if valid.
            let admin_id = require_admin_session_sid(&live_conn, session_id.as_deref())?;

            let zip_bytes = std::fs::read(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;

            let cursor = std::io::Cursor::new(zip_bytes);
            let mut archive = zip::ZipArchive::new(cursor)
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {}", e)))?;

            let has_db = archive.file_names().any(|n| n == "chan.db");
            if !has_db {
                return Err(AppError::BadRequest(
                    "Invalid backup: zip must contain 'chan.db' at the root.".into(),
                ));
            }

            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db = temp_dir.join(format!("chan_restore_{}.db", tmp_id));
            let mut db_extracted = false;

            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{}]: {}", i, e)))?;
                let name = entry.name().to_string();
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Restore-saved: skipping suspicious entry '{}'", name);
                    continue;
                }
                if name == "chan.db" {
                    let mut out = std::fs::File::create(&temp_db).map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Create temp DB: {}", e))
                    })?;
                    copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp DB: {}", e)))?;
                    db_extracted = true;
                } else if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() {
                        continue;
                    }
                    let target = PathBuf::from(&upload_dir).join(rel);
                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {}", e)))?;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| {
                                AppError::Internal(anyhow::anyhow!("mkdir parent: {}", e))
                            })?;
                        }
                        let mut out = std::fs::File::create(&target).map_err(|e| {
                            AppError::Internal(anyhow::anyhow!(
                                "Create {}: {}",
                                target.display(),
                                e
                            ))
                        })?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES).map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Write {}: {}", target.display(), e))
                        })?;
                    }
                }
            }
            drop(archive);

            if !db_extracted {
                return Err(AppError::Internal(anyhow::anyhow!("chan.db not extracted")));
            }

            let backup_result = (|| -> Result<()> {
                let src = rusqlite::Connection::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open source: {}", e)))?;
                use rusqlite::backup::Backup;
                let backup = Backup::new(&src, &mut live_conn)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup init: {}", e)))?;
                backup
                    .run_to_completion(100, std::time::Duration::from_millis(0), None)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup copy: {}", e)))?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&temp_db);
            backup_result?;

            let fresh_sid = new_session_id();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            match db::create_session(&live_conn, &fresh_sid, admin_id, expires_at) {
                Ok(_) => {
                    info!(
                        "Admin restore-saved completed; new session for admin_id={}",
                        admin_id
                    );
                    Ok(fresh_sid)
                }
                Err(e) => {
                    warn!("Restore-saved: could not create session: {}", e);
                    Ok(String::new())
                }
            }
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if fresh_sid.is_empty() {
        let jar = jar.remove(Cookie::from(SESSION_COOKIE));
        return Ok((jar, Redirect::to("/admin")).into_response());
    }

    let mut new_cookie = Cookie::new(SESSION_COOKIE, fresh_sid);
    new_cookie.set_http_only(true);
    new_cookie.set_same_site(SameSite::Strict);
    new_cookie.set_path("/");
    new_cookie.set_secure(CONFIG.https_cookies);
    // FIX[A5]: Set Max-Age to match normal login behaviour.
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));
    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

// ─── POST /admin/board/backup/restore-saved ───────────────────────────────────

/// Restore a board backup from a saved file in board-backups/.
pub async fn restore_saved_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RestoreSavedForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename
        || safe_filename.contains("..")
        || !safe_filename.ends_with(".zip")
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }

    let path = board_backup_dir().join(&safe_filename);
    // FIX[A4]: Defer the blocking file read until after auth is verified inside
    // spawn_blocking — mirrors the fix applied to restore_saved_full_backup (A3).
    let upload_dir = CONFIG.upload_dir.clone();

    let board_short_result: Result<Result<String>> = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            use board_backup_types::*;
            use rusqlite::params;
            use std::collections::HashMap;

            let conn = pool.get()?;
            // Auth check first — only read the file if the session is valid.
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let zip_bytes = std::fs::read(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;

            let cursor = std::io::Cursor::new(zip_bytes);
            let mut archive = zip::ZipArchive::new(cursor)
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {}", e)))?;

            if !archive.file_names().any(|n| n == "board.json") {
                return Err(AppError::BadRequest(
                    "Invalid board backup: zip must contain 'board.json'.".into(),
                ));
            }

            let manifest: BoardBackupManifest = {
                let mut entry = archive
                    .by_name("board.json")
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read board.json: {}", e)))?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read bytes: {}", e)))?;
                serde_json::from_slice(&buf)
                    .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {}", e)))?
            };

            let board_short = manifest.board.short_name.clone();

            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM boards WHERE short_name = ?1",
                    params![board_short],
                    |r| r.get(0),
                )
                .ok();

            // FIX[A6]: BEGIN IMMEDIATE must cover the DELETE + UPDATE/INSERT of the
            // board row as well as the thread/post/poll inserts.  Previously those
            // DDL statements ran outside any transaction, so a crash between the
            // DELETE and the first INSERT left the board with zero threads and no way
            // to recover without manual intervention.
            conn.execute("BEGIN IMMEDIATE", [])
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Begin tx: {}", e)))?;

            let restore_result = (|| -> Result<()> {
                let live_board_id: i64 = if let Some(eid) = existing_id {
                    conn.execute("DELETE FROM threads WHERE board_id = ?1", params![eid])
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Clear threads: {}", e)))?;
                    conn.execute(
                        "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                         max_threads=?4, bump_limit=?5,
                         allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
                         edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
                         allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
                         WHERE id=?16",
                        params![
                            manifest.board.name,
                            manifest.board.description,
                            manifest.board.nsfw as i64,
                            manifest.board.max_threads,
                            manifest.board.bump_limit,
                            manifest.board.allow_images as i64,
                            manifest.board.allow_video as i64,
                            manifest.board.allow_audio as i64,
                            manifest.board.allow_tripcodes as i64,
                            manifest.board.edit_window_secs,
                            manifest.board.allow_editing as i64,
                            manifest.board.allow_archive as i64,
                            manifest.board.allow_video_embeds as i64,
                            manifest.board.allow_captcha as i64,
                            manifest.board.post_cooldown_secs,
                            eid,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Update board: {}", e)))?;
                    eid
                } else {
                    conn.execute(
                        "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                         bump_limit, allow_images, allow_video, allow_audio, allow_tripcodes,
                         edit_window_secs, allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                         post_cooldown_secs, created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        params![
                            manifest.board.short_name,
                            manifest.board.name,
                            manifest.board.description,
                            manifest.board.nsfw as i64,
                            manifest.board.max_threads,
                            manifest.board.bump_limit,
                            manifest.board.allow_images as i64,
                            manifest.board.allow_video as i64,
                            manifest.board.allow_audio as i64,
                            manifest.board.allow_tripcodes as i64,
                            manifest.board.edit_window_secs,
                            manifest.board.allow_editing as i64,
                            manifest.board.allow_archive as i64,
                            manifest.board.allow_video_embeds as i64,
                            manifest.board.allow_captcha as i64,
                            manifest.board.post_cooldown_secs,
                            manifest.board.created_at,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert board: {}", e)))?;
                    conn.last_insert_rowid()
                };

                let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
                for t in &manifest.threads {
                    conn.execute(
                        "INSERT INTO threads (board_id, subject, created_at, bumped_at,
                         locked, sticky, reply_count) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            live_board_id,
                            t.subject,
                            t.created_at,
                            t.bumped_at,
                            t.locked as i64,
                            t.sticky as i64,
                            t.reply_count,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert thread: {}", e)))?;
                    thread_id_map.insert(t.id, conn.last_insert_rowid());
                }

                for p in &manifest.posts {
                    let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown thread {}", p.thread_id))
                    })?;
                    conn.execute(
                        "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                         body, body_html, ip_hash, file_path, file_name, file_size,
                         thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        params![
                            new_tid,
                            live_board_id,
                            p.name,
                            p.tripcode,
                            p.subject,
                            p.body,
                            p.body_html,
                            p.ip_hash,
                            p.file_path,
                            p.file_name,
                            p.file_size,
                            p.thumb_path,
                            p.mime_type,
                            p.media_type,
                            p.created_at,
                            p.deletion_token,
                            p.is_op as i64,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert post: {}", e)))?;
                }

                let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
                for p in &manifest.polls {
                    let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown thread {}", p.thread_id))
                    })?;
                    conn.execute(
                        "INSERT INTO polls (thread_id, question, expires_at, created_at)
                         VALUES (?1,?2,?3,?4)",
                        params![new_tid, p.question, p.expires_at, p.created_at],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert poll: {}", e)))?;
                    poll_id_map.insert(p.id, conn.last_insert_rowid());
                }

                let mut option_id_map: HashMap<i64, i64> = HashMap::new();
                for o in &manifest.poll_options {
                    let new_pid = *poll_id_map.get(&o.poll_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown poll {}", o.poll_id))
                    })?;
                    conn.execute(
                        "INSERT INTO poll_options (poll_id, text, position) VALUES (?1,?2,?3)",
                        params![new_pid, o.text, o.position],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert option: {}", e)))?;
                    option_id_map.insert(o.id, conn.last_insert_rowid());
                }

                for v in &manifest.poll_votes {
                    let new_pid = *poll_id_map.get(&v.poll_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown poll {}", v.poll_id))
                    })?;
                    let new_oid = *option_id_map.get(&v.option_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown option {}", v.option_id))
                    })?;
                    conn.execute(
                        "INSERT OR IGNORE INTO poll_votes
                         (poll_id, option_id, ip_hash) VALUES (?1,?2,?3)",
                        params![new_pid, new_oid, v.ip_hash],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert vote: {}", e)))?;
                }

                for fh in &manifest.file_hashes {
                    conn.execute(
                        "INSERT OR IGNORE INTO file_hashes
                         (sha256, file_path, thumb_path, mime_type, created_at)
                         VALUES (?1,?2,?3,?4,?5)",
                        params![
                            fh.sha256,
                            fh.file_path,
                            fh.thumb_path,
                            fh.mime_type,
                            fh.created_at,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert file_hash: {}", e)))?;
                }
                Ok(())
            })();

            match restore_result {
                Ok(()) => {
                    conn.execute("COMMIT", [])
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Commit: {}", e)))?;
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    return Err(e);
                }
            }

            // Extract upload files.
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{}]: {}", i, e)))?;
                let name = entry.name().to_string();
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Board restore-saved: skipping suspicious entry '{}'", name);
                    continue;
                }
                if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() {
                        continue;
                    }
                    let target = PathBuf::from(&upload_dir).join(rel);
                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {}", e)))?;
                    } else {
                        if let Some(p) = target.parent() {
                            std::fs::create_dir_all(p).map_err(|e| {
                                AppError::Internal(anyhow::anyhow!("mkdir parent: {}", e))
                            })?;
                        }
                        let mut out = std::fs::File::create(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Create: {}", e)))?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write: {}", e)))?;
                    }
                }
            }

            info!("Admin board restore-saved completed for /{}/", board_short);
            let safe_short: String = board_short
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            Ok(safe_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)));

    fn encode_q(s: &str) -> String {
        fn nibble(n: u8) -> char {
            match n {
                0..=9 => (b'0' + n) as char,
                _ => (b'A' + n - 10) as char,
            }
        }
        s.bytes()
            .flat_map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    vec![b as char]
                }
                b' ' => vec!['+'],
                b => vec!['%', nibble(b >> 4), nibble(b & 0xf)],
            })
            .collect()
    }
    match board_short_result {
        Ok(Ok(board_short)) => Ok(Redirect::to(&format!(
            "/admin/panel?board_restored={}",
            board_short
        ))
        .into_response()),
        Ok(Err(app_err)) => {
            let msg = encode_q(&app_err.to_string());
            Ok(Redirect::to(&format!("/admin/panel?restore_error={}", msg)).into_response())
        }
        Err(join_err) => {
            let msg = encode_q(&join_err.to_string());
            Ok(Redirect::to(&format!("/admin/panel?restore_error={}", msg)).into_response())
        }
    }
}

// ─── Board-level backup / restore ─────────────────────────────────────────────

/// Flat structs used exclusively for board-level backup serialisation.
mod board_backup_types {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct BoardRow {
        pub id: i64,
        pub short_name: String,
        pub name: String,
        pub description: String,
        pub nsfw: bool,
        pub max_threads: i64,
        pub bump_limit: i64,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_images: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_video: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default false.
        #[serde(default)]
        pub allow_audio: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_tripcodes: bool,
        /// Added in a later version — absent in older backups; default to 300 s.
        #[serde(default = "default_edit_window_secs")]
        pub edit_window_secs: i64,
        /// Added in a later version — absent in older backups; default to false.
        #[serde(default)]
        pub allow_editing: bool,
        /// Added in a later version — absent in older backups; default to true.
        #[serde(default = "default_true")]
        pub allow_archive: bool,
        /// Added in v1.0.10 — absent in older backups; default to false.
        #[serde(default)]
        pub allow_video_embeds: bool,
        /// Added in v1.0.10 — absent in older backups; default to false.
        #[serde(default)]
        pub allow_captcha: bool,
        /// Added for per-board post cooldowns — absent in older backups; default 0 (disabled).
        #[serde(default)]
        pub post_cooldown_secs: i64,
        pub created_at: i64,
    }

    fn default_true() -> bool {
        true
    }

    fn default_edit_window_secs() -> i64 {
        300
    }
    #[derive(Serialize, Deserialize)]
    pub struct ThreadRow {
        pub id: i64,
        pub board_id: i64,
        pub subject: Option<String>,
        pub created_at: i64,
        pub bumped_at: i64,
        pub locked: bool,
        pub sticky: bool,
        pub reply_count: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PostRow {
        pub id: i64,
        pub thread_id: i64,
        pub board_id: i64,
        pub name: String,
        pub tripcode: Option<String>,
        pub subject: Option<String>,
        pub body: String,
        pub body_html: String,
        pub ip_hash: String,
        pub file_path: Option<String>,
        pub file_name: Option<String>,
        pub file_size: Option<i64>,
        pub thumb_path: Option<String>,
        pub mime_type: Option<String>,
        pub media_type: Option<String>,
        pub created_at: i64,
        pub deletion_token: String,
        pub is_op: bool,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollRow {
        pub id: i64,
        pub thread_id: i64,
        pub question: String,
        pub expires_at: i64,
        pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollOptionRow {
        pub id: i64,
        pub poll_id: i64,
        pub text: String,
        pub position: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollVoteRow {
        pub id: i64,
        pub poll_id: i64,
        pub option_id: i64,
        pub ip_hash: String,
    }
    #[derive(Serialize, Deserialize)]
    pub struct FileHashRow {
        pub sha256: String,
        pub file_path: String,
        pub thumb_path: String,
        pub mime_type: String,
        pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct BoardBackupManifest {
        pub version: u32,
        pub board: BoardRow,
        pub threads: Vec<ThreadRow>,
        pub posts: Vec<PostRow>,
        pub polls: Vec<PollRow>,
        pub poll_options: Vec<PollOptionRow>,
        pub poll_votes: Vec<PollVoteRow>,
        pub file_hashes: Vec<FileHashRow>,
    }
}

/// Stream a board-level backup zip: manifest JSON + that board's upload files.
///
/// MEM-FIX: Same approach as admin_backup — build zip into a NamedTempFile on
/// disk, then stream the result in 64 KiB chunks.
pub async fn board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(board_short): axum::extract::Path<String>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
            use board_backup_types::*;
            use rusqlite::params;

            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            let board: BoardRow = conn.query_row(
                "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                        allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                        allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                        post_cooldown_secs, created_at
                 FROM boards WHERE short_name = ?1",
                params![board_short],
                |r| Ok(BoardRow {
                    id: r.get(0)?,
                    short_name: r.get(1)?,
                    name: r.get(2)?,
                    description: r.get(3)?,
                    nsfw: r.get::<_, i64>(4)? != 0,
                    max_threads: r.get(5)?,
                    bump_limit: r.get(6)?,
                    allow_images: r.get::<_, i64>(7)? != 0,
                    allow_video: r.get::<_, i64>(8)? != 0,
                    allow_audio: r.get::<_, i64>(9)? != 0,
                    allow_tripcodes: r.get::<_, i64>(10)? != 0,
                    edit_window_secs: r.get(11)?,
                    allow_editing: r.get::<_, i64>(12)? != 0,
                    allow_archive: r.get::<_, i64>(13)? != 0,
                    allow_video_embeds: r.get::<_, i64>(14)? != 0,
                    allow_captcha: r.get::<_, i64>(15)? != 0,
                    post_cooldown_secs: r.get(16)?,
                    created_at: r.get(17)?,
                }),
            ).map_err(|_| AppError::NotFound(format!("Board '{}' not found", board_short)))?;

            let board_id = board.id;

            // ── Threads ───────────────────────────────────────────────────
            let threads: Vec<ThreadRow> = {
                let mut s = conn.prepare(
                    "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, reply_count
                     FROM threads WHERE board_id = ?1 ORDER BY id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(ThreadRow {
                    id: r.get(0)?, board_id: r.get(1)?, subject: r.get(2)?,
                    created_at: r.get(3)?, bumped_at: r.get(4)?,
                    locked: r.get::<_,i64>(5)? != 0, sticky: r.get::<_,i64>(6)? != 0,
                    reply_count: r.get(7)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Posts ─────────────────────────────────────────────────────
            let posts: Vec<PostRow> = {
                let mut s = conn.prepare(
                    "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                            ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                            media_type, created_at, deletion_token, is_op
                     FROM posts WHERE board_id = ?1 ORDER BY id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PostRow {
                    id: r.get(0)?, thread_id: r.get(1)?, board_id: r.get(2)?,
                    name: r.get(3)?, tripcode: r.get(4)?, subject: r.get(5)?,
                    body: r.get(6)?, body_html: r.get(7)?, ip_hash: r.get(8)?,
                    file_path: r.get(9)?, file_name: r.get(10)?, file_size: r.get(11)?,
                    thumb_path: r.get(12)?, mime_type: r.get(13)?, media_type: r.get(14)?,
                    created_at: r.get(15)?, deletion_token: r.get(16)?,
                    is_op: r.get::<_,i64>(17)? != 0,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Polls ─────────────────────────────────────────────────────
            let polls: Vec<PollRow> = {
                let mut s = conn.prepare(
                    "SELECT p.id, p.thread_id, p.question, p.expires_at, p.created_at
                     FROM polls p JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY p.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollRow {
                    id: r.get(0)?, thread_id: r.get(1)?, question: r.get(2)?,
                    expires_at: r.get(3)?, created_at: r.get(4)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Poll options ──────────────────────────────────────────────
            let poll_options: Vec<PollOptionRow> = {
                let mut s = conn.prepare(
                    "SELECT po.id, po.poll_id, po.text, po.position
                     FROM poll_options po
                     JOIN polls p ON p.id = po.poll_id
                     JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY po.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollOptionRow {
                    id: r.get(0)?, poll_id: r.get(1)?, text: r.get(2)?, position: r.get(3)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Poll votes ────────────────────────────────────────────────
            let poll_votes: Vec<PollVoteRow> = {
                let mut s = conn.prepare(
                    "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash
                     FROM poll_votes pv
                     JOIN polls p ON p.id = pv.poll_id
                     JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY pv.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollVoteRow {
                    id: r.get(0)?, poll_id: r.get(1)?, option_id: r.get(2)?,
                    ip_hash: r.get(3)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── File hashes referenced by this board ──────────────────────
            let file_hashes: Vec<FileHashRow> = {
                let mut s = conn.prepare(
                    "SELECT DISTINCT fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at
                     FROM file_hashes fh
                     JOIN posts po ON po.file_path = fh.file_path
                     WHERE po.board_id = ?1 ORDER BY fh.created_at ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(FileHashRow {
                    sha256: r.get(0)?, file_path: r.get(1)?, thumb_path: r.get(2)?,
                    mime_type: r.get(3)?, created_at: r.get(4)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Serialise manifest ────────────────────────────────────────
            let manifest = BoardBackupManifest {
                version: 1, board, threads, posts, polls,
                poll_options, poll_votes, file_hashes,
            };

            // ── Build zip to NamedTempFile (MEM-FIX) ─────────────────────
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON serialise: {}", e)))?;

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = count_files_in_dir(&board_upload_path);
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            progress.files_total.store(file_count + 1, Ordering::Relaxed);

            let zip_tmp = tempfile::NamedTempFile::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp zip: {}", e)))?;
            {
                let out_file = std::io::BufWriter::new(
                    zip_tmp.as_file().try_clone().map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Clone temp file handle: {}", e))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                zip.start_file("board.json", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip manifest: {}", e)))?;
                zip.write_all(&manifest_json)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write manifest: {}", e)))?;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(manifest_json.len() as u64, Ordering::Relaxed);

                if board_upload_path.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, &board_upload_path, opts, &progress)?;
                }

                zip.finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
            }

            let file_size = zip_tmp.as_file().metadata().map(|m| m.len()).unwrap_or(0);

            let (_, tmp_path_obj) = zip_tmp.into_parts();
            let final_path = tmp_path_obj.keep().map_err(|e| {
                AppError::Internal(anyhow::anyhow!("Persist temp zip: {}", e))
            })?;

            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-board-{}-{}.zip", board_short, ts);
            info!("Admin downloaded board backup for /{}/  ({} bytes on disk)", board_short, file_size);
            progress.phase.store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok((final_path, fname, file_size))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let file = tokio::fs::File::open(&tmp_path).await.map_err(|e| {
        AppError::Internal(anyhow::anyhow!("Open board backup for streaming: {}", e))
    })?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    let cleanup_path = tmp_path;
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        let _ = tokio::fs::remove_file(cleanup_path).await;
    });

    use axum::http::header;
    let disposition = format!("attachment; filename=\"{}\"", filename);
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

/// Restore a single board from a board-level backup zip or raw board.json.
///
/// Returns `Response` (not `Result<Response>`) so ALL errors — including
/// CSRF failures and multipart parse errors — redirect to the admin panel
/// with a flash message instead of producing a blank crash page.
pub async fn board_restore(
    State(state): State<AppState>,
    jar: CookieJar,
    mut multipart: Multipart,
) -> Response {
    fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            _ => (b'A' + n - 10) as char,
        }
    }
    fn encode_q(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                b => {
                    out.push('%');
                    out.push(nibble(b >> 4));
                    out.push(nibble(b & 0xf));
                }
            }
        }
        out
    }

    // Run the whole operation as a fallible async block so any early return
    // with Err(...) is caught below and turned into a redirect.
    let result: Result<String> = async {
        let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
        let upload_dir = CONFIG.upload_dir.clone();

        let mut zip_data: Option<Vec<u8>> = None;
        let mut form_csrf: Option<String> = None;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
        {
            match field.name() {
                Some("_csrf") => {
                    form_csrf = Some(
                        field
                            .text()
                            .await
                            .map_err(|e| AppError::BadRequest(e.to_string()))?,
                    );
                }
                Some("backup_file") => {
                    let bytes = field
                        .bytes()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?;
                    zip_data = Some(bytes.to_vec());
                }
                _ => {}
            }
        }

        let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
        if !crate::middleware::validate_csrf(
            csrf_cookie.as_deref(),
            form_csrf.as_deref().unwrap_or(""),
        ) {
            return Err(AppError::Forbidden("CSRF token mismatch.".into()));
        }

        let file_bytes = match zip_data {
            None => return Err(AppError::BadRequest("No backup file uploaded.".into())),
            Some(b) if b.is_empty() => {
                return Err(AppError::BadRequest("Uploaded backup file is empty.".into()))
            }
            Some(b) => b,
        };

        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<String> {
                use board_backup_types::*;
                use rusqlite::params;
                use std::collections::HashMap;

                let conn = pool.get()?;
                require_admin_session_sid(&conn, session_id.as_deref())?;

                // Detect format: ZIP magic bytes or raw JSON (skipping optional BOM)
                let is_zip = file_bytes.starts_with(b"PK\x03\x04");
                let json_lead = if file_bytes.starts_with(b"\xef\xbb\xbf") {
                    file_bytes.get(3).copied()
                } else {
                    file_bytes.first().copied()
                };
                let is_json = json_lead == Some(b'{');

                if !is_zip && !is_json {
                    return Err(AppError::BadRequest(
                        "Unrecognized format. Upload a .zip board backup or a raw board.json file.".into(),
                    ));
                }

                let (manifest, mut archive_opt): (
                    BoardBackupManifest,
                    Option<zip::ZipArchive<std::io::Cursor<Vec<u8>>>>,
                ) = if is_zip {
                    let cursor = std::io::Cursor::new(file_bytes);
                    let mut archive = zip::ZipArchive::new(cursor)
                        .map_err(|e| AppError::BadRequest(format!("Invalid zip: {}", e)))?;
                    if !archive.file_names().any(|n| n == "board.json") {
                        return Err(AppError::BadRequest(
                            "Invalid board backup: zip must contain 'board.json'.                              (Did you upload a full-site backup instead?)".into(),
                        ));
                    }
                    let manifest: BoardBackupManifest = {
                        let mut entry = archive.by_name("board.json").map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Read board.json: {}", e))
                        })?;
                        let mut buf = Vec::new();
                        std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Read bytes: {}", e))
                        })?;
                        serde_json::from_slice(&buf).map_err(|e| {
                            AppError::BadRequest(format!("Invalid board.json: {}", e))
                        })?
                    };
                    (manifest, Some(archive))
                } else {
                    let manifest: BoardBackupManifest = serde_json::from_slice(&file_bytes)
                        .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {}", e)))?;
                    (manifest, None)
                };

                let board_short = manifest.board.short_name.clone();

                let existing_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM boards WHERE short_name = ?1",
                        params![board_short],
                        |r| r.get(0),
                    )
                    .ok();


                // FIX[A6]: BEGIN IMMEDIATE must cover the DELETE + UPDATE/INSERT of the
                // board row.  Previously those statements ran outside any transaction.
                conn.execute("BEGIN IMMEDIATE", [])
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Begin tx: {}", e)))?;

                let restore_result = (|| -> Result<()> {
                    let live_board_id: i64 = if let Some(eid) = existing_id {
                        conn.execute("DELETE FROM threads WHERE board_id = ?1", params![eid])
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Clear threads: {}", e)))?;
                        conn.execute(
                            "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                             max_threads=?4, bump_limit=?5,
                             allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
                             edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
                             allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
                             WHERE id=?16",
                            params![
                                manifest.board.name,
                                manifest.board.description,
                                manifest.board.nsfw as i64,
                                manifest.board.max_threads,
                                manifest.board.bump_limit,
                                manifest.board.allow_images as i64,
                                manifest.board.allow_video as i64,
                                manifest.board.allow_audio as i64,
                                manifest.board.allow_tripcodes as i64,
                                manifest.board.edit_window_secs,
                                manifest.board.allow_editing as i64,
                                manifest.board.allow_archive as i64,
                                manifest.board.allow_video_embeds as i64,
                                manifest.board.allow_captcha as i64,
                                manifest.board.post_cooldown_secs,
                                eid,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Update board: {}", e)))?;
                        eid
                    } else {
                        conn.execute(
                            "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                             bump_limit, allow_images, allow_video, allow_audio, allow_tripcodes,
                             edit_window_secs, allow_editing, allow_archive, allow_video_embeds,
                             allow_captcha, post_cooldown_secs, created_at)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                            params![
                                manifest.board.short_name,
                                manifest.board.name,
                                manifest.board.description,
                                manifest.board.nsfw as i64,
                                manifest.board.max_threads,
                                manifest.board.bump_limit,
                                manifest.board.allow_images as i64,
                                manifest.board.allow_video as i64,
                                manifest.board.allow_audio as i64,
                                manifest.board.allow_tripcodes as i64,
                                manifest.board.edit_window_secs,
                                manifest.board.allow_editing as i64,
                                manifest.board.allow_archive as i64,
                                manifest.board.allow_video_embeds as i64,
                                manifest.board.allow_captcha as i64,
                                manifest.board.post_cooldown_secs,
                                manifest.board.created_at,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert board: {}", e)))?;
                        conn.last_insert_rowid()
                    };

                    let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
                    for t in &manifest.threads {
                        conn.execute(
                            "INSERT INTO threads (board_id, subject, created_at, bumped_at,
                             locked, sticky, reply_count)
                             VALUES (?1,?2,?3,?4,?5,?6,?7)",
                            params![
                                live_board_id, t.subject, t.created_at, t.bumped_at,
                                t.locked as i64, t.sticky as i64, t.reply_count,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert thread {}: {}", t.id, e)))?;
                        thread_id_map.insert(t.id, conn.last_insert_rowid());
                    }

                    for p in &manifest.posts {
                        let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!("Post {} refs unknown thread {}", p.id, p.thread_id))
                        })?;
                        conn.execute(
                            "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                             body, body_html, ip_hash, file_path, file_name, file_size,
                             thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                            params![
                                new_tid, live_board_id, p.name, p.tripcode, p.subject,
                                p.body, p.body_html, p.ip_hash, p.file_path, p.file_name,
                                p.file_size, p.thumb_path, p.mime_type, p.media_type,
                                p.created_at, p.deletion_token, p.is_op as i64,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert post {}: {}", p.id, e)))?;
                    }

                    let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
                    for p in &manifest.polls {
                        let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!("Poll {} refs unknown thread {}", p.id, p.thread_id))
                        })?;
                        conn.execute(
                            "INSERT INTO polls (thread_id, question, expires_at, created_at)
                             VALUES (?1,?2,?3,?4)",
                            params![new_tid, p.question, p.expires_at, p.created_at],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert poll {}: {}", p.id, e)))?;
                        poll_id_map.insert(p.id, conn.last_insert_rowid());
                    }

                    let mut option_id_map: HashMap<i64, i64> = HashMap::new();
                    for o in &manifest.poll_options {
                        let new_pid = *poll_id_map.get(&o.poll_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!("Option {} refs unknown poll {}", o.id, o.poll_id))
                        })?;
                        conn.execute(
                            "INSERT INTO poll_options (poll_id, text, position) VALUES (?1,?2,?3)",
                            params![new_pid, o.text, o.position],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert option {}: {}", o.id, e)))?;
                        option_id_map.insert(o.id, conn.last_insert_rowid());
                    }

                    for v in &manifest.poll_votes {
                        let new_pid = *poll_id_map.get(&v.poll_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!("Vote {} refs unknown poll {}", v.id, v.poll_id))
                        })?;
                        let new_oid = *option_id_map.get(&v.option_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!("Vote {} refs unknown option {}", v.id, v.option_id))
                        })?;
                        conn.execute(
                            "INSERT OR IGNORE INTO poll_votes
                             (poll_id, option_id, ip_hash) VALUES (?1,?2,?3)",
                            params![new_pid, new_oid, v.ip_hash],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert vote {}: {}", v.id, e)))?;
                    }

                    for fh in &manifest.file_hashes {
                        conn.execute(
                            "INSERT OR IGNORE INTO file_hashes
                             (sha256, file_path, thumb_path, mime_type, created_at)
                             VALUES (?1,?2,?3,?4,?5)",
                            params![fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert file_hash: {}", e)))?;
                    }
                    Ok(())
                })();

                match restore_result {
                    Ok(()) => {
                        conn.execute("COMMIT", [])
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Commit tx: {}", e)))?;
                    }
                    Err(e) => {
                        let _ = conn.execute("ROLLBACK", []);
                        return Err(e);
                    }
                }

                if let Some(ref mut archive) = archive_opt {
                    for i in 0..archive.len() {
                        let mut entry = archive.by_index(i)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{}]: {}", i, e)))?;
                        let name = entry.name().to_string();
                        if name.contains("..") || name.starts_with('/') || name.starts_with('\\'){ 
                            warn!("Board restore: skipping suspicious entry '{}'", name);
                            continue;
                        }
                        if let Some(rel) = name.strip_prefix("uploads/") {
                            if rel.is_empty() { continue; }
                            let target = PathBuf::from(&upload_dir).join(rel);
                            if entry.is_dir() {
                                std::fs::create_dir_all(&target)
                                    .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {}", e)))?;
                            } else {
                                if let Some(p) = target.parent() {
                                    std::fs::create_dir_all(p)
                                        .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir parent: {}", e)))?;
                                }
                                let mut out = std::fs::File::create(&target)
                                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Create file: {}", e)))?;
                                copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write file: {}", e)))?;
                            }
                        }
                    }
                }

                info!("Admin board restore completed for /{}/", board_short);
                let safe_short: String = board_short
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .take(8)
                    .collect();
                Ok(safe_short)
            }
        })
        .await
        .unwrap_or_else(|e| Err(AppError::Internal(anyhow::anyhow!("Task panicked: {}", e))))
    }.await;

    match result {
        Ok(board_short) => {
            Redirect::to(&format!("/admin/panel?board_restored={}", board_short)).into_response()
        }
        Err(e) => Redirect::to(&format!(
            "/admin/panel?restore_error={}",
            encode_q(&e.to_string())
        ))
        .into_response(),
    }
}

// ─── POST /admin/site/settings ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SiteSettingsForm {
    pub _csrf: Option<String>,
    /// Checkbox: present = "1", absent = not submitted (treat as false)
    pub collapse_greentext: Option<String>,
    /// Custom site name (replaces [ RustChan ] on home page and footer).
    pub site_name: Option<String>,
    /// Custom home page subtitle line below the site name.
    pub site_subtitle: Option<String>,
    /// Default theme served to first-time visitors.
    /// Valid slugs: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
    pub default_theme: Option<String>,
}

pub async fn update_site_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<SiteSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            let val = if form.collapse_greentext.as_deref() == Some("1") {
                "1"
            } else {
                "0"
            };
            db::set_site_setting(&conn, "collapse_greentext", val)?;
            info!("Admin updated site setting: collapse_greentext={}", val);

            // Save the custom site name (trimmed, max 64 chars).
            let new_name = form
                .site_name
                .as_deref()
                .unwrap_or("")
                .trim()
                .chars()
                .take(64)
                .collect::<String>();
            db::set_site_setting(&conn, "site_name", &new_name)?;
            // Update the in-memory live name so all pages reflect it immediately.
            crate::templates::set_live_site_name(&new_name);
            info!("Admin updated site name to: {:?}", new_name);

            // Save the custom subtitle.
            let new_subtitle = form
                .site_subtitle
                .as_deref()
                .unwrap_or("")
                .trim()
                .chars()
                .take(128)
                .collect::<String>();
            db::set_site_setting(&conn, "site_subtitle", &new_subtitle)?;
            crate::templates::set_live_site_subtitle(&new_subtitle);
            info!("Admin updated site subtitle to: {:?}", new_subtitle);

            // Save the default theme slug (validated against allowed values).
            const VALID_THEMES: &[&str] = &[
                "terminal",
                "aero",
                "dorfic",
                "fluorogrid",
                "neoncubicle",
                "chanclassic",
            ];
            let new_theme = form
                .default_theme
                .as_deref()
                .unwrap_or("terminal")
                .trim()
                .to_string();
            let new_theme = if VALID_THEMES.contains(&new_theme.as_str()) {
                new_theme
            } else {
                "terminal".to_string()
            };
            db::set_site_setting(&conn, "default_theme", &new_theme)?;
            crate::templates::set_live_default_theme(&new_theme);
            info!("Admin updated default theme to: {:?}", new_theme);

            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?settings_saved=1").into_response())
}

// ─── POST /admin/vacuum ───────────────────────────────────────────────────────
//
// Runs SQLite VACUUM to reclaim space after bulk deletions.
// Returns an inline result page showing DB size before and after.

#[derive(Deserialize)]
pub struct VacuumForm {
    pub _csrf: Option<String>,
}

pub async fn admin_vacuum(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<VacuumForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form._csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let size_before = db::get_db_size_bytes(&conn).unwrap_or(0);

            db::run_vacuum(&conn)?;

            let size_after = db::get_db_size_bytes(&conn).unwrap_or(0);

            let saved = size_before.saturating_sub(size_after);

            info!(
                "Admin ran VACUUM: {} → {} bytes ({} reclaimed)",
                size_before, size_after, saved
            );

            Ok(crate::templates::admin_vacuum_result_page(
                size_before,
                size_after,
                &csrf_clone,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── GET /admin/ip/{ip_hash} ──────────────────────────────────────────────────
//
// Shows all posts made by a given IP hash across all boards, newest first,
// with pagination.  Requires an active admin session.

#[derive(Deserialize)]
pub struct IpHistoryQuery {
    #[serde(default = "default_page")]
    pub page: i64,
}

fn default_page() -> i64 {
    1
}

pub async fn admin_ip_history(
    State(state): State<AppState>,
    Path(ip_hash): Path<String>,
    Query(params): Query<IpHistoryQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let (jar, csrf) = ensure_csrf(jar);
    let csrf_clone = csrf.clone();

    // Sanitise the IP hash: must be exactly a SHA-256 hex string (64 hex chars).
    // The previous guard used `> 64` which accepted any string of 0–64 chars,
    // including an empty string.  Require exactly 64.
    if ip_hash.len() != 64 || !ip_hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::BadRequest("Invalid IP hash.".into()));
    }

    let page = params.page.max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            const PER_PAGE: i64 = 25;
            let total = db::count_posts_by_ip_hash(&conn, &ip_hash)?;
            let pagination = crate::models::Pagination::new(page, PER_PAGE, total);
            let posts_with_boards =
                db::get_posts_by_ip_hash(&conn, &ip_hash, PER_PAGE, pagination.offset())?;

            let all_boards = db::get_all_boards(&conn)?;

            Ok(crate::templates::admin_ip_history_page(
                &ip_hash,
                &posts_with_boards,
                &pagination,
                &all_boards,
                &csrf_clone,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /admin/report/resolve ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ResolveReportForm {
    report_id: i64,
    /// Optional: also ban the reported post's author
    ban_ip_hash: Option<String>,
    ban_reason: Option<String>,
    _csrf: Option<String>,
}

pub async fn resolve_report(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ResolveReportForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                require_admin_session_with_name(&conn, session_id.as_deref())?;

            db::resolve_report(&conn, form.report_id, admin_id)?;

            // Optionally ban the reporter's target while resolving.
            if let Some(ref ip) = form.ban_ip_hash {
                if !ip.trim().is_empty() {
                    let reason = form.ban_reason.as_deref().unwrap_or("Reported content");
                    db::add_ban(&conn, ip, reason, None)?; // permanent ban
                    let _ = db::log_mod_action(
                        &conn,
                        admin_id,
                        &admin_name,
                        "ban",
                        "ban",
                        None,
                        "",
                        &format!("via report {} — {}", form.report_id, reason),
                    );
                }
            }

            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "resolve_report",
                "report",
                Some(form.report_id),
                "",
                "",
            );
            info!("Admin resolved report {}", form.report_id);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel#reports").into_response())
}

// ─── GET /admin/mod-log ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ModLogQuery {
    #[serde(default = "default_mod_log_page")]
    page: i64,
}

fn default_mod_log_page() -> i64 {
    1
}

pub async fn mod_log_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<ModLogQuery>,
) -> Result<(CookieJar, Html<String>)> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let (jar, csrf) = ensure_csrf(jar);
    let csrf_clone = csrf.clone();
    let page = params.page.max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            const PER_PAGE: i64 = 50;
            let total = db::count_mod_log(&conn)?;
            let pagination = crate::models::Pagination::new(page, PER_PAGE, total);
            let entries = db::get_mod_log(&conn, PER_PAGE, pagination.offset())?;
            let boards = db::get_all_boards(&conn)?;
            Ok(crate::templates::mod_log_page(
                &entries,
                &pagination,
                &csrf_clone,
                &boards,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}
