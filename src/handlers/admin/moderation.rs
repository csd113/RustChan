// handlers/admin/moderation.rs
//
// Moderation handlers: bans, ban appeals, word filters, reports, IP history,
// and the mod log. All routes require a valid admin session cookie.

use crate::{
    db,
    error::{AppError, Result},
    middleware::AppState,
};
use axum::{
    extract::{Form, Path, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use serde::Deserialize;
use tracing::info;

// ─── POST /admin/ban/add ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddBanForm {
    ip_hash: String,
    reason: String,
    duration_hours: Option<i64>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[allow(clippy::arithmetic_side_effects)]
pub async fn add_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AddBanForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;
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
            info!("Admin added ban for ip_hash {ip_hash_log}…");
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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn remove_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BanIdForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[allow(clippy::arithmetic_side_effects)]
pub async fn admin_ban_and_delete(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BanDeleteForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;

            // Validate ip_hash: must be a well-formed SHA-256 hex string (64 hex
            // chars).  The value comes from a form field in the post toolbar; a
            // confused or tampered submission should be rejected cleanly.
            if form.ip_hash.len() != 64 || !form.ip_hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(AppError::BadRequest("Invalid IP hash format.".into()));
            }

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
                &format!("inline ban — ip_hash={reason}… reason={}", &ip_hash_log),
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

            info!("Admin ban+delete: post={post_id} ip_hash={ip_hash_log}… board={board_short}");
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
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    // If OP was deleted, the thread is gone — send to board index
    let redirect = if is_op {
        format!("/{safe_board}")
    } else {
        format!("/{safe_board}/thread/{thread_id}#p{post_id}")
    };
    Ok(Redirect::to(&redirect).into_response())
}

// ─── POST /admin/appeal/dismiss ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AppealActionForm {
    appeal_id: i64,
    ip_hash: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn dismiss_appeal(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AppealActionForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;
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

// ─── POST /admin/filter/add ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddFilterForm {
    pattern: String,
    replacement: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn add_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AddFilterForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn remove_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<FilterIdForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::remove_word_filter(&conn, form.filter_id)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
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

const fn default_page() -> i64 {
    1
}

pub async fn admin_ip_history(
    State(state): State<AppState>,
    Path(ip_hash): Path<String>,
    Query(params): Query<IpHistoryQuery>,
    jar: CookieJar,
) -> Result<(CookieJar, Html<String>)> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let (jar, csrf) = crate::handlers::board::ensure_csrf(jar);
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
            const PER_PAGE: i64 = 25;
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn resolve_report(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ResolveReportForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;

            db::resolve_report(&conn, form.report_id, admin_id)?;

            // Optionally ban the reporter's target while resolving.
            if let Some(ref ip) = form.ban_ip_hash {
                let ip = ip.trim();
                if !ip.is_empty() {
                    // Validate the ip_hash is a well-formed SHA-256 hex string
                    // (64 hex chars) before inserting — guards against form tampering.
                    if ip.len() != 64 || !ip.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Err(AppError::BadRequest("Invalid IP hash format.".into()));
                    }
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
                        &format!("via report {} — {reason}", form.report_id),
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

const fn default_mod_log_page() -> i64 {
    1
}

pub async fn mod_log_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<ModLogQuery>,
) -> Result<(CookieJar, Html<String>)> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let (jar, csrf) = crate::handlers::board::ensure_csrf(jar);
    let csrf_clone = csrf.clone();
    let page = params.page.max(1);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            const PER_PAGE: i64 = 50;
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

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
