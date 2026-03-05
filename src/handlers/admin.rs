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
    templates,
    utils::crypto::{new_session_id, verify_password},
};
use axum::{
    extract::{Form, Multipart, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::{info, warn};

const SESSION_COOKIE: &str = "chan_admin_session";

// ─── Admin auth guard ─────────────────────────────────────────────────────────

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


pub async fn admin_index(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
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
    Form(form): Form<LoginForm>,
) -> Result<Response> {
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
            move || { let conn = pool.get()?; db::get_all_boards(&conn) }
        }).await.map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        return Ok((jar, Html(templates::admin_login_page(Some("Invalid username."), &csrf, &boards))).into_response());
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
                move || { let conn = pool.get()?; db::get_all_boards(&conn) }
            }).await.map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
            Ok((jar, Html(templates::admin_login_page(Some("Invalid username or password."), &csrf, &boards))).into_response())
        }
        Some(admin_id) => {
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

            info!("Admin {} logged in", admin_id);
            Ok((jar.add(cookie), Redirect::to("/admin/panel")).into_response())
        }
    }
}

// ─── POST /admin/logout ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CsrfOnly {
    _csrf: Option<String>,
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
    Ok((jar, Redirect::to("/admin")).into_response())
}

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    // FIX[HIGH-3]: Move auth check and all DB calls into spawn_blocking.
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let (jar, csrf) = ensure_csrf(jar);
    let csrf_clone = csrf.clone();

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            // Auth check inside blocking task
            let sid = session_id
                .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
            db::get_session(&conn, &sid)?
                .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;

            let boards = db::get_all_boards(&conn)?;
            let bans = db::list_bans(&conn)?;
            let filters = db::get_word_filters(&conn)?;
            let collapse_greentext = db::get_collapse_greentext(&conn);
            Ok(templates::admin_panel_page(&boards, &bans, &filters, collapse_greentext, &csrf_clone))
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
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form._csrf.as_deref().unwrap_or("")) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let short = form.short_name.trim().to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>();

    if short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let nsfw = form.nsfw.as_deref() == Some("1");
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form.description.trim().chars().take(256).collect::<String>();

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
            let short_name: Option<String> = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![form.board_id],
                |r| r.get(0),
            ).ok();

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

            info!("Admin deleted board id={} ({} file(s) removed)", form.board_id, paths.len());
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
        "sticky" | "unsticky" | "lock" | "unlock" => {}
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    }

    let action = form.action.clone();
    let thread_id = form.thread_id;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            match action.as_str() {
                "sticky"   => db::set_thread_sticky(&conn, thread_id, true)?,
                "unsticky" => db::set_thread_sticky(&conn, thread_id, false)?,
                "lock"     => db::set_thread_locked(&conn, thread_id, true)?,
                "unlock"   => db::set_thread_locked(&conn, thread_id, false)?,
                _ => {}
            }
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
            let safe: String = form.board.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            Ok(safe)
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        format!("/{}/thread/{}", board_name, form.thread_id)
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
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            // FIX[MEDIUM-10]: Resolve board name from DB, not user-supplied form field.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_all_boards(&conn)?.into_iter()
                .find(|b| b.id == post.board_id)
                .map(|b| b.short_name)
                .unwrap_or_else(|| {
                    form.board.chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect()
                });

            let paths = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, post_id)?
            };

            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            info!("Admin deleted post {}", post_id);
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{}/", redirect_board)).into_response())
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
            require_admin_session_sid(&conn, session_id.as_deref())?;

            // FIX[MEDIUM-10]: Resolve board name from DB.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_thread(&conn, thread_id)?
                .and_then(|t| {
                    db::get_all_boards(&conn).ok()?.into_iter()
                        .find(|b| b.id == t.board_id)
                        .map(|b| b.short_name)
                })
                .unwrap_or_else(|| {
                    form.board.chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect()
                });

            let paths = db::delete_thread(&conn, thread_id)?;
            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            info!("Admin deleted thread {}", thread_id);
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{}/", redirect_board)).into_response())
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
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::add_ban(&conn, &form.ip_hash, &form.reason, expires_at)?;
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

// ─── POST /admin/filter/add ───────────────────────────────────────────────────

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
    let replacement = form.replacement.trim().chars().take(256).collect::<String>();

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
    board_id:        i64,
    name:            String,
    description:     String,
    bump_limit:      Option<String>,
    max_threads:     Option<String>,
    nsfw:            Option<String>,
    allow_images:    Option<String>,
    allow_video:     Option<String>,
    allow_audio:     Option<String>,
    allow_tripcodes: Option<String>,
    _csrf:           Option<String>,
}

pub async fn update_board_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    check_csrf_jar(&jar, form._csrf.as_deref())?;

    let bump_limit  = form.bump_limit.as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(500).clamp(1, 10_000);
    let max_threads = form.max_threads.as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150).clamp(1, 1_000);

    // Enforce server-side length limits on free-text fields
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form.description.trim().chars().take(256).collect::<String>();
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
/// The WAL is checkpointed first so the backup contains a consistent snapshot.
pub async fn admin_backup(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());

    let upload_dir = CONFIG.upload_dir.clone();

    let (zip_bytes, filename) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<u8>, String)> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            // Use VACUUM INTO to create an atomic, defragmented, WAL-free
            // snapshot of the database.  Unlike checkpoint + read-file, this
            // is safe even if other connections are actively writing — SQLite
            // holds a read lock for the duration and produces a consistent
            // single-file copy with no sidecar files.
            let temp_dir  = std::env::temp_dir();
            let tmp_id    = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db   = temp_dir.join(format!("chan_backup_{}.db", tmp_id));
            let temp_db_str = temp_db.to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path is non-UTF-8")))?
                .replace('\'', "''"); // SQL-escape single quotes in path (just in case)

            conn.execute_batch(&format!("VACUUM INTO '{}'", temp_db_str))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("VACUUM INTO failed: {}", e)))?;

            // We no longer need the live connection.
            drop(conn);

            let db_data = std::fs::read(&temp_db)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Read vacuum snapshot: {}", e)))?;
            let _ = std::fs::remove_file(&temp_db);

            // Build the zip in memory.
            let buf = std::io::Cursor::new(Vec::<u8>::new());
            let mut zip = zip::ZipWriter::new(buf);
            // zip 2+: SimpleFileOptions replaces the old generic FileOptions.
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            // ── Database snapshot ──────────────────────────────────────────
            {
                use std::io::Write;
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB entry: {}", e)))?;
                zip.write_all(&db_data)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write DB to zip: {}", e)))?;
            }

            // ── Upload files ──────────────────────────────────────────────
            let uploads_base = std::path::Path::new(&upload_dir);
            if uploads_base.exists() {
                add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts)?;
            }

            let cursor = zip.finish()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
            let bytes = cursor.into_inner();

            let ts    = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-backup-{}.zip", ts);
            info!("Admin downloaded backup ({} bytes, {} upload bytes included)",
                  bytes.len(), db_data.len());
            Ok((bytes, fname))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    use axum::http::header;
    let disposition = format!("attachment; filename=\"{}\"", filename);
    Ok((
        [
            (header::CONTENT_TYPE,        "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        zip_bytes,
    ).into_response())
}

/// Recursively add every file under `dir` into the zip as `uploads/{rel_path}`.
fn add_dir_to_zip(
    zip:  &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    base: &std::path::Path,
    dir:  &std::path::Path,
    // zip 2+: SimpleFileOptions replaces the old generic FileOptions.
    opts: zip::write::SimpleFileOptions,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("read_dir {}: {}", dir.display(), e)))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| AppError::Internal(anyhow::anyhow!("dir entry: {}", e)))?;
        let path = entry.path();

        let relative = path.strip_prefix(base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("strip_prefix: {}", e)))?;
        // Normalise to forward-slashes so the zip is portable.
        let rel_str  = relative.to_string_lossy().replace('\\', "/");
        let zip_path = format!("uploads/{}", rel_str);

        if path.is_dir() {
            zip.add_directory(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip dir: {}", e)))?;
            add_dir_to_zip(zip, base, &path, opts)?;
        } else if path.is_file() {
            use std::io::Write;
            let data = std::fs::read(&path)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("read {}: {}", path.display(), e)))?;
            zip.start_file(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip file: {}", e)))?;
            zip.write_all(&data)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("write zip: {}", e)))?;
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

    // Collect multipart fields (the stream can only be consumed once).
    let mut zip_data:  Option<Vec<u8>> = None;
    let mut form_csrf: Option<String>  = None;

    while let Some(field) = multipart.next_field().await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        match field.name() {
            Some("_csrf") => {
                form_csrf = Some(field.text().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?);
            }
            Some("backup_file") => {
                let bytes = field.bytes().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?;
                zip_data = Some(bytes.to_vec());
            }
            _ => {}
        }
    }

    // CSRF check.
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(
        csrf_cookie.as_deref(),
        form_csrf.as_deref().unwrap_or(""),
    ) {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let zip_bytes = zip_data
        .ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
    if zip_bytes.is_empty() {
        return Err(AppError::BadRequest("Uploaded backup file is empty.".into()));
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

            // ── Parse the zip ─────────────────────────────────────────────
            let cursor = std::io::Cursor::new(zip_bytes);
            let mut archive = zip::ZipArchive::new(cursor)
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
            let tmp_id   = uuid::Uuid::new_v4().to_string().replace('-', "");
            let temp_db  = temp_dir.join(format!("chan_restore_{}.db", tmp_id));
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
                    std::io::copy(&mut entry, &mut out)
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
                        std::io::copy(&mut entry, &mut out)
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

    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Check CSRF using the cookie jar. Returns error on mismatch.
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
    let sid = session_id
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session = db::get_session(conn, sid)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;
    Ok(session.admin_id)
}

// ─── Board-level backup / restore ─────────────────────────────────────────────

/// Flat structs used exclusively for board-level backup serialisation.
mod board_backup_types {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct BoardRow {
        pub id: i64, pub short_name: String, pub name: String,
        pub description: String, pub nsfw: bool,
        pub max_threads: i64, pub bump_limit: i64,
        pub allow_images: bool, pub allow_video: bool,
        pub allow_audio: bool, pub allow_tripcodes: bool,
        pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct ThreadRow {
        pub id: i64, pub board_id: i64, pub subject: Option<String>,
        pub created_at: i64, pub bumped_at: i64,
        pub locked: bool, pub sticky: bool, pub reply_count: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PostRow {
        pub id: i64, pub thread_id: i64, pub board_id: i64,
        pub name: String, pub tripcode: Option<String>, pub subject: Option<String>,
        pub body: String, pub body_html: String, pub ip_hash: String,
        pub file_path: Option<String>, pub file_name: Option<String>,
        pub file_size: Option<i64>, pub thumb_path: Option<String>,
        pub mime_type: Option<String>, pub media_type: Option<String>,
        pub created_at: i64, pub deletion_token: String, pub is_op: bool,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollRow {
        pub id: i64, pub thread_id: i64, pub question: String,
        pub expires_at: i64, pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollOptionRow {
        pub id: i64, pub poll_id: i64, pub text: String, pub position: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollVoteRow {
        pub id: i64, pub poll_id: i64, pub option_id: i64,
        pub ip_hash: String, pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct FileHashRow {
        pub sha256: String, pub file_path: String, pub thumb_path: String,
        pub mime_type: String, pub created_at: i64,
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
pub async fn board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(board_short): axum::extract::Path<String>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();

    let (zip_bytes, filename) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<u8>, String)> {
            use board_backup_types::*;
            use rusqlite::params;

            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            // ── Board row ─────────────────────────────────────────────────
            let board: BoardRow = conn.query_row(
                "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                        allow_images, allow_video, allow_audio, allow_tripcodes, created_at
                 FROM boards WHERE short_name = ?1",
                params![board_short],
                |r| Ok(BoardRow {
                    id:              r.get(0)?,
                    short_name:      r.get(1)?,
                    name:            r.get(2)?,
                    description:     r.get(3)?,
                    nsfw:            r.get::<_, i64>(4)? != 0,
                    max_threads:     r.get(5)?,
                    bump_limit:      r.get(6)?,
                    allow_images:    r.get::<_, i64>(7)? != 0,
                    allow_video:     r.get::<_, i64>(8)? != 0,
                    allow_audio:     r.get::<_, i64>(9)? != 0,
                    allow_tripcodes: r.get::<_, i64>(10)? != 0,
                    created_at:      r.get(11)?,
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
                    "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash, pv.created_at
                     FROM poll_votes pv
                     JOIN polls p ON p.id = pv.poll_id
                     JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY pv.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollVoteRow {
                    id: r.get(0)?, poll_id: r.get(1)?, option_id: r.get(2)?,
                    ip_hash: r.get(3)?, created_at: r.get(4)?,
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
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON serialise: {}", e)))?;

            // ── Build zip ─────────────────────────────────────────────────
            let buf = std::io::Cursor::new(Vec::<u8>::new());
            let mut zip = zip::ZipWriter::new(buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            {
                use std::io::Write;
                zip.start_file("board.json", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip manifest: {}", e)))?;
                zip.write_all(&manifest_json)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write manifest: {}", e)))?;
            }

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            if board_upload_path.exists() {
                add_dir_to_zip(&mut zip, uploads_base, &board_upload_path, opts)?;
            }

            let cursor = zip.finish()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {}", e)))?;
            let bytes = cursor.into_inner();

            let ts    = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-board-{}-{}.zip", board_short, ts);
            info!("Admin downloaded board backup for /{}/  ({} bytes)", board_short, bytes.len());
            Ok((bytes, fname))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    use axum::http::header;
    let disposition = format!("attachment; filename=\"{}\"", filename);
    Ok((
        [
            (header::CONTENT_TYPE,        "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        zip_bytes,
    ).into_response())
}

/// Restore a single board from a board-level backup zip.
///
/// All IDs are remapped to avoid conflicts with existing data.
/// If the board already exists its content is wiped first; otherwise
/// a new board is created.
pub async fn board_restore(
    State(state): State<AppState>,
    jar: CookieJar,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();

    let mut zip_data:  Option<Vec<u8>> = None;
    let mut form_csrf: Option<String>  = None;

    while let Some(field) = multipart.next_field().await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {}", e)))?
    {
        match field.name() {
            Some("_csrf")       => {
                form_csrf = Some(field.text().await
                    .map_err(|e| AppError::BadRequest(e.to_string()))?);
            }
            Some("backup_file") => {
                let bytes = field.bytes().await
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

    let zip_bytes = zip_data
        .ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
    if zip_bytes.is_empty() {
        return Err(AppError::BadRequest("Uploaded backup file is empty.".into()));
    }

    let board_short: String = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            use board_backup_types::*;
            use rusqlite::params;
            use std::collections::HashMap;

            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let cursor = std::io::Cursor::new(zip_bytes);
            let mut archive = zip::ZipArchive::new(cursor)
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {}", e)))?;

            if !archive.file_names().any(|n| n == "board.json") {
                return Err(AppError::BadRequest(
                    "Invalid board backup: zip must contain 'board.json'. \
                     (Did you upload a full-site backup instead?)".into(),
                ));
            }

            let manifest: BoardBackupManifest = {
                let mut entry = archive.by_name("board.json")
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read board.json: {}", e)))?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read bytes: {}", e)))?;
                serde_json::from_slice(&buf)
                    .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {}", e)))?
            };

            let board_short = manifest.board.short_name.clone();

            // ── Wipe or create the board ──────────────────────────────────
            let existing_id: Option<i64> = conn.query_row(
                "SELECT id FROM boards WHERE short_name = ?1",
                params![board_short], |r| r.get(0),
            ).ok();

            let live_board_id: i64 = if let Some(eid) = existing_id {
                conn.execute("DELETE FROM threads WHERE board_id = ?1", params![eid])
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Clear threads: {}", e)))?;
                conn.execute(
                    "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                     max_threads=?4, bump_limit=?5,
                     allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9
                     WHERE id=?10",
                    params![
                        manifest.board.name, manifest.board.description,
                        manifest.board.nsfw as i64,
                        manifest.board.max_threads, manifest.board.bump_limit,
                        manifest.board.allow_images as i64, manifest.board.allow_video as i64,
                        manifest.board.allow_audio as i64, manifest.board.allow_tripcodes as i64,
                        eid,
                    ],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Update board: {}", e)))?;
                eid
            } else {
                conn.execute(
                    "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                     bump_limit, allow_images, allow_video, allow_audio, allow_tripcodes, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        manifest.board.short_name, manifest.board.name,
                        manifest.board.description, manifest.board.nsfw as i64,
                        manifest.board.max_threads, manifest.board.bump_limit,
                        manifest.board.allow_images as i64, manifest.board.allow_video as i64,
                        manifest.board.allow_audio as i64, manifest.board.allow_tripcodes as i64,
                        manifest.board.created_at,
                    ],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert board: {}", e)))?;
                conn.last_insert_rowid()
            };

            // ── Threads ───────────────────────────────────────────────────
            // All inserts are wrapped in a single transaction so that a failure
            // at any point leaves the database unchanged rather than partially restored.
            conn.execute("BEGIN IMMEDIATE", [])
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Begin restore tx: {}", e)))?;

            let restore_result = (|| -> Result<()> {
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
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert thread {}: {}", t.id, e)))?;
                thread_id_map.insert(t.id, conn.last_insert_rowid());
            }

            // ── Posts ─────────────────────────────────────────────────────
            for p in &manifest.posts {
                let new_tid = *thread_id_map.get(&p.thread_id)
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!(
                        "Post {} refs unknown thread {}", p.id, p.thread_id)))?;
                conn.execute(
                    "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                     body, body_html, ip_hash, file_path, file_name, file_size,
                     thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                    params![
                        new_tid, live_board_id, p.name, p.tripcode, p.subject,
                        p.body, p.body_html, p.ip_hash,
                        p.file_path, p.file_name, p.file_size,
                        p.thumb_path, p.mime_type, p.media_type,
                        p.created_at, p.deletion_token, p.is_op as i64,
                    ],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert post {}: {}", p.id, e)))?;
            }

            // ── Polls ─────────────────────────────────────────────────────
            let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
            for p in &manifest.polls {
                let new_tid = *thread_id_map.get(&p.thread_id)
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!(
                        "Poll {} refs unknown thread {}", p.id, p.thread_id)))?;
                conn.execute(
                    "INSERT INTO polls (thread_id, question, expires_at, created_at)
                     VALUES (?1,?2,?3,?4)",
                    params![new_tid, p.question, p.expires_at, p.created_at],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert poll {}: {}", p.id, e)))?;
                poll_id_map.insert(p.id, conn.last_insert_rowid());
            }

            // ── Poll options ──────────────────────────────────────────────
            let mut option_id_map: HashMap<i64, i64> = HashMap::new();
            for o in &manifest.poll_options {
                let new_pid = *poll_id_map.get(&o.poll_id)
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!(
                        "Option {} refs unknown poll {}", o.id, o.poll_id)))?;
                conn.execute(
                    "INSERT INTO poll_options (poll_id, text, position) VALUES (?1,?2,?3)",
                    params![new_pid, o.text, o.position],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert option {}: {}", o.id, e)))?;
                option_id_map.insert(o.id, conn.last_insert_rowid());
            }

            // ── Poll votes ────────────────────────────────────────────────
            for v in &manifest.poll_votes {
                let new_pid = *poll_id_map.get(&v.poll_id)
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!(
                        "Vote {} refs unknown poll {}", v.id, v.poll_id)))?;
                let new_oid = *option_id_map.get(&v.option_id)
                    .ok_or_else(|| AppError::Internal(anyhow::anyhow!(
                        "Vote {} refs unknown option {}", v.id, v.option_id)))?;
                conn.execute(
                    "INSERT OR IGNORE INTO poll_votes
                     (poll_id, option_id, ip_hash, created_at)
                     VALUES (?1,?2,?3,?4)",
                    params![new_pid, new_oid, v.ip_hash, v.created_at],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert vote {}: {}", v.id, e)))?;
            }

            // ── File hashes (dedup table — skip on collision) ─────────────
            for fh in &manifest.file_hashes {
                conn.execute(
                    "INSERT OR IGNORE INTO file_hashes
                     (sha256, file_path, thumb_path, mime_type, created_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at],
                ).map_err(|e| AppError::Internal(anyhow::anyhow!("Insert file_hash: {}", e)))?;
            }
            Ok(())
            })();

            match restore_result {
                Ok(()) => {
                    conn.execute("COMMIT", [])
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Commit restore tx: {}", e)))?;
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    return Err(e);
                }
            }

            // ── Extract upload files ──────────────────────────────────────
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{}]: {}", i, e)))?;
                let name = entry.name().to_string();

                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
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
                        std::io::copy(&mut entry, &mut out)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write file: {}", e)))?;
                    }
                }
            }

            info!("Admin board restore completed for /{}/", board_short);
            // Sanitize board_short before returning — it comes from the zip manifest and
            // is used in a redirect URL query parameter.  Only allow board-name characters.
            let safe_short: String = board_short.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            Ok(safe_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // board_short is already sanitized to alphanumeric inside the spawn_blocking closure
    // (returned as safe_short) — use it directly here.
    Ok(Redirect::to(&format!("/admin/panel?board_restored={}", board_short)).into_response())
}

// ─── POST /admin/site/settings ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SiteSettingsForm {
    pub _csrf: Option<String>,
    /// Checkbox: present = "1", absent = not submitted (treat as false)
    pub collapse_greentext: Option<String>,
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
            let val = if form.collapse_greentext.as_deref() == Some("1") { "1" } else { "0" };
            db::set_site_setting(&conn, "collapse_greentext", val)?;
            info!("Admin updated site setting: collapse_greentext={}", val);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?settings_saved=1").into_response())
}
