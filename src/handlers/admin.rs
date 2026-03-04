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
    extract::{Form, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use serde::Deserialize;
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
            Ok(templates::admin_panel_page(&boards, &bans, &filters, &csrf_clone))
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

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            db::delete_board(&conn, form.board_id)?;
            info!("Admin deleted board id={}", form.board_id);
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
            Ok(form.board) // fallback to user-supplied if DB lookup fails
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
            let board_name = db::get_all_boards(&conn)?.into_iter()
                .find(|b| b.id == post.board_id)
                .map(|b| b.short_name)
                .unwrap_or_else(|| form.board.clone());

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
            let board_name = db::get_thread(&conn, thread_id)?
                .and_then(|t| {
                    db::get_all_boards(&conn).ok()?.into_iter()
                        .find(|b| b.id == t.board_id)
                        .map(|b| b.short_name)
                })
                .unwrap_or_else(|| form.board.clone());

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
        .map(|h| Utc::now().timestamp() + h * 3600);

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
        .unwrap_or(500)
        .max(1).min(10_000);
    let max_threads = form.max_threads.as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150)
        .max(1).min(1_000);

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
