// handlers/admin.rs
//
// Admin panel. All routes require a valid session cookie.
//
// Authentication flow:
//   1. POST /admin/login → verify Argon2 password → create session in DB → set cookie
//   2. All /admin/* routes → check session cookie → get session from DB → proceed
//   3. POST /admin/logout → delete session from DB → clear cookie
//
// Session cookie: HTTPOnly (not readable by JS), SameSite=Strict (prevents CSRF),
// Secure=true if deployed with HTTPS.
//
// Admin actions: create/delete boards, sticky/lock threads, ban IPs,
// delete any post, manage word filters.

use crate::{
    config::CONFIG,
    db::{self, DbPool},
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
    templates,
    utils::crypto::{hash_ip, new_session_id, verify_password},
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
fn require_admin(jar: &CookieJar, pool: &DbPool) -> Result<i64> {
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
pub fn is_admin_session(jar: &CookieJar, pool: &DbPool) -> bool {
    require_admin(jar, pool).is_ok()
}


pub async fn admin_index(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    // Check if already logged in
    if let Ok(_) = require_admin(&jar, &state.db) {
        return Ok(Redirect::to("/admin/panel").into_response());
    }

    let (jar, csrf) = ensure_csrf(jar);
    let boards = {
        let conn = state.db.get()?;
        db::get_all_boards(&conn)?
    };
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

    let show_error = |msg: &str, jar: CookieJar| -> Result<Response> {
        let (jar, csrf) = ensure_csrf(jar);
        let boards = {
            let conn = state.db.get()?;
            db::get_all_boards(&conn)?
        };
        Ok((
            jar,
            Html(templates::admin_login_page(Some(msg), &csrf, &boards)),
        )
            .into_response())
    };

    let username = form.username.trim().to_string();
    if username.is_empty() || username.len() > 64 {
        return show_error("Invalid username.", jar);
    }

    let pool = state.db.clone();
    let password = form.password.clone();

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
            show_error("Invalid username or password.", jar)
        }
        Some(admin_id) => {
            // Create session
            let session_id = new_session_id();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            {
                let conn = state.db.get()?;
                db::create_session(&conn, &session_id, admin_id, expires_at)?;
            }

            let mut cookie = Cookie::new(SESSION_COOKIE, session_id);
            cookie.set_http_only(true);
            cookie.set_same_site(SameSite::Strict);
            cookie.set_path("/");
            cookie.set_secure(false); // true in production with HTTPS
            // Session expires via DB expiry check; cookie lives for browser session

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
        let conn = state.db.get()?;
        db::delete_session(&conn, &session_id)?;
    }
    let jar = jar.remove(Cookie::from(SESSION_COOKIE));
    Ok((jar, Redirect::to("/admin")).into_response())
}

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;

    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

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
    let conn = state.db.get()?;
    db::create_board(
        &conn,
        &short,
        form.name.trim(),
        form.description.trim(),
        nsfw,
    )?;

    info!("Admin created board /{}/", short);
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let conn = state.db.get()?;
    db::delete_board(&conn, form.board_id)?;
    info!("Admin deleted board id={}", form.board_id);
    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/thread/sticky ────────────────────────────────────────────────

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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let conn = state.db.get()?;
    match form.action.as_str() {
        "sticky" => db::set_thread_sticky(&conn, form.thread_id, true)?,
        "unsticky" => db::set_thread_sticky(&conn, form.thread_id, false)?,
        "lock" => db::set_thread_locked(&conn, form.thread_id, true)?,
        "unlock" => db::set_thread_locked(&conn, form.thread_id, false)?,
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    }

    info!("Admin {} thread {}", form.action, form.thread_id);
    Ok(Redirect::to(&format!("/{}/thread/{}", form.board, form.thread_id)).into_response())
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();

    let conn = state.db.get()?;
    let post = db::get_post(&conn, form.post_id)?
        .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

    let paths = if post.is_op {
        db::delete_thread(&conn, post.thread_id)?
    } else {
        db::delete_post(&conn, form.post_id)?
    };

    for p in paths {
        crate::utils::files::delete_file(&upload_dir, &p);
    }

    info!("Admin deleted post {}", form.post_id);
    Ok(Redirect::to(&format!("/{}/", form.board)).into_response())
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let conn = state.db.get()?;

    let paths = db::delete_thread(&conn, form.thread_id)?;
    for p in paths {
        crate::utils::files::delete_file(&upload_dir, &p);
    }

    info!("Admin deleted thread {}", form.thread_id);
    Ok(Redirect::to(&format!("/{}/", form.board)).into_response())
}



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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let expires_at = form
        .duration_hours
        .filter(|&h| h > 0)
        .map(|h| Utc::now().timestamp() + h * 3600);

    let conn = state.db.get()?;
    db::add_ban(&conn, &form.ip_hash, &form.reason, expires_at)?;
    info!("Admin added ban for ip_hash {}", &form.ip_hash[..8]);
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let conn = state.db.get()?;
    db::remove_ban(&conn, form.ban_id)?;
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    if form.pattern.trim().is_empty() {
        return Err(AppError::BadRequest("Pattern cannot be empty.".into()));
    }

    let conn = state.db.get()?;
    db::add_word_filter(&conn, form.pattern.trim(), form.replacement.trim())?;
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
    require_admin(&jar, &state.db)
        .map_err(|_| AppError::Forbidden("Not logged in.".into()))?;
    check_csrf(&jar, form._csrf.as_deref())?;

    let conn = state.db.get()?;
    db::remove_word_filter(&conn, form.filter_id)?;
    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── Helper: check CSRF ───────────────────────────────────────────────────────

fn check_csrf(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let cookie_token = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(cookie_token.as_deref(), form_token.unwrap_or("")) {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    } else {
        Ok(())
    }
}
