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
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use chrono::Utc;
use serde::Deserialize;

// ─── POST /admin/ban/add ──────────────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the add ban request.
pub(crate) struct AddBanForm {
    /// The IP hash.
    ip_hash: String,
    /// The reason.
    reason: String,
    /// The optional duration hours.
    duration_hours: Option<i64>,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the add ban request.
pub(crate) async fn add_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AddBanForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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
            if let Err(error) = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "ban",
                "ban",
                None,
                "",
                &format!("ip_hash={}… reason={}", ip_hash_log, form.reason),
            ) {
                tracing::error!(
                    target: "admin",
                    admin_id,
                    ip_hash_prefix = %ip_hash_log,
                    error = %error,
                    "Ban completed without audit-log record"
                );
            }
            tracing::info!(target: "admin", "Ban added");
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect("Ban added.").into_response())
}

// ─── POST /admin/ban/remove ───────────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the ban ID request.
pub(crate) struct BanIdForm {
    /// The ban identifier.
    ban_id: i64,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the remove ban request.
pub(crate) async fn remove_ban(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BanIdForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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

    Ok(super::admin_panel_redirect("Ban lifted.").into_response())
}

// ─── POST /admin/post/ban-delete ──────────────────────────────────────────────
// Inline ban + delete from the per-post admin toolbar.
// Bans the post author's IP hash, deletes the post, then redirects back to
// the thread (or the board index if the OP is deleted).

#[derive(Deserialize)]
/// Form fields accepted by the ban delete request.
pub(crate) struct BanDeleteForm {
    /// The post identifier.
    post_id: i64,
    /// The IP hash.
    ip_hash: String,
    /// The board.
    board: String,
    /// The thread identifier.
    thread_id: i64,
    /// The optional is op.
    is_op: Option<String>,
    /// The optional reason.
    reason: Option<String>,
    /// The optional duration hours.
    duration_hours: Option<i64>,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[expect(
    clippy::cognitive_complexity,
    reason = "ban-and-delete keeps its authorization, mutation, cleanup, and audit steps together"
)]
#[expect(
    clippy::too_many_lines,
    reason = "authorization, ban creation, post deletion, media cleanup, and audit logging are one action"
)]
/// Handles the admin ban and delete request.
pub(crate) async fn admin_ban_and_delete(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BanDeleteForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let reason = form
        .reason
        .as_deref()
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "Rule violation".to_owned());

    let expires_at = form
        .duration_hours
        .filter(|&h| h > 0)
        .map(|h| Utc::now().timestamp() + h.min(87_600).saturating_mul(3600));

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

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.thread_id != thread_id {
                return Err(AppError::NotFound("Post not found.".into()));
            }

            // Validate ip_hash: must be a well-formed SHA-256 hex string (64 hex
            // chars).  The value comes from a form field in the post toolbar; a
            // confused or tampered submission should be rejected cleanly.
            if form.ip_hash.len() != 64 || !form.ip_hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(AppError::BadRequest("Invalid IP hash format.".into()));
            }

            // Ban first so the IP cannot re-post before the delete lands
            db::add_ban(&conn, &form.ip_hash, &reason, expires_at)?;
            if let Err(error) = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "ban",
                "ban",
                None,
                &board_short,
                &format!("inline ban — ip_hash={ip_hash_log}… reason={reason}"),
            ) {
                tracing::error!(
                    target: "admin",
                    admin_id,
                    board = %board_short,
                    ip_hash_prefix = %ip_hash_log,
                    error = %error,
                    "Inline ban completed without audit-log record"
                );
            }

            // Delete post (or whole thread if OP)
            if is_op {
                let deleted = db::delete_thread(&conn, thread_id)?;
                if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                    &conn,
                    &crate::config::CONFIG.upload_dir,
                    deleted.pending_fs_op_id.as_deref(),
                    &deleted.paths,
                ) {
                    tracing::warn!(
                        target: "admin",
                        thread_id = thread_id,
                        error = %error,
                        "ban-delete thread cleanup did not fully complete"
                    );
                }
                if let Err(error) = db::log_mod_action(
                    &conn,
                    admin_id,
                    &admin_name,
                    "delete_thread",
                    "thread",
                    Some(thread_id),
                    &board_short,
                    "",
                ) {
                    tracing::error!(
                        target: "admin",
                        admin_id,
                        thread_id,
                        board = %board_short,
                        error = %error,
                        "Ban-delete thread action completed without audit-log record"
                    );
                }
            } else {
                let deleted = db::delete_post(&conn, post_id)?;
                if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                    &conn,
                    &crate::config::CONFIG.upload_dir,
                    deleted.pending_fs_op_id.as_deref(),
                    &deleted.paths,
                ) {
                    tracing::warn!(
                        target: "admin",
                        post_id = post_id,
                        error = %error,
                        "ban-delete post cleanup did not fully complete"
                    );
                }
                if let Err(error) = db::log_mod_action(
                    &conn,
                    admin_id,
                    &admin_name,
                    "delete_post",
                    "post",
                    Some(post_id),
                    &board_short,
                    "",
                ) {
                    tracing::error!(
                        target: "admin",
                        admin_id,
                        post_id,
                        board = %board_short,
                        error = %error,
                        "Ban-delete post action completed without audit-log record"
                    );
                }
            }

            tracing::info!(target: "admin", post_id = post_id, board = %board_short, "Ban and delete");
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // form.board is user-supplied; sanitise to alphanumeric only before
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
/// Form fields accepted by the appeal action request.
pub(crate) struct AppealActionForm {
    /// The appeal identifier.
    appeal_id: i64,
    /// The optional IP hash.
    ip_hash: Option<String>,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the dismiss appeal request.
pub(crate) async fn dismiss_appeal(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AppealActionForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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

    Ok(
        super::admin_panel_redirect_anchor_open("Appeal dismissed.", "appeals", "reports")
            .into_response(),
    )
}

// ─── POST /admin/appeal/accept ────────────────────────────────────────────────

/// Handles the accept appeal request.
pub(crate) async fn accept_appeal(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AppealActionForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;
            let ip = form.ip_hash.as_deref().unwrap_or("");
            db::accept_ban_appeal(&conn, form.appeal_id, ip)?;
            if let Err(error) = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "accept_appeal",
                "ban",
                None,
                "",
                &format!("appeal {} — ip unban", form.appeal_id),
            ) {
                tracing::error!(
                    target: "admin",
                    admin_id,
                    appeal_id = form.appeal_id,
                    error = %error,
                    "Appeal acceptance completed without audit-log record"
                );
            }
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect_anchor_open(
        "Appeal accepted and ban lifted.",
        "appeals",
        "reports",
    )
    .into_response())
}

// ─── POST /admin/filter/add ───────────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the add filter request.
pub(crate) struct AddFilterForm {
    /// The pattern.
    pattern: String,
    /// The replacement.
    replacement: String,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the add filter request.
pub(crate) async fn add_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AddFilterForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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

    Ok(super::admin_panel_redirect("Word filter added.").into_response())
}

// ─── POST /admin/filter/remove ────────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the filter ID request.
pub(crate) struct FilterIdForm {
    /// The filter identifier.
    filter_id: i64,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the remove filter request.
pub(crate) async fn remove_filter(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<FilterIdForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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

    Ok(super::admin_panel_redirect("Word filter removed.").into_response())
}

// ─── GET /admin/ip/{ip_hash} ──────────────────────────────────────────────────
//
// Shows all posts made by a given IP hash across all boards, newest first,
// with pagination.  Requires an active admin session.

#[derive(Deserialize)]
/// Query parameters accepted by the IP history request.
pub(crate) struct IpHistoryQuery {
    #[serde(default = "default_page")]
    /// The page.
    pub page: i64,
    /// The optional return to.
    pub return_to: Option<String>,
}

/// Returns the default page.
const fn default_page() -> i64 {
    1
}

/// Handles the admin IP history request.
pub(crate) async fn admin_ip_history(
    State(state): State<AppState>,
    Path(ip_hash): Path<String>,
    Query(params): Query<IpHistoryQuery>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> Result<(CookieJar, Html<String>)> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    let (jar, csrf) = super::ensure_admin_csrf(
        jar,
        super::should_set_secure_cookie(&headers, secure_context),
    )?;
    let csrf_clone = csrf.clone();

    // Sanitise the IP hash: must be exactly a SHA-256 hex string (64 hex chars).
    // The previous guard used `> 64` which accepted any string of 0–64 chars,
    // including an empty string.  Require exactly 64.
    if ip_hash.len() != 64 || !ip_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest("Invalid IP hash.".into()));
    }

    let page = params.page.max(1);
    let return_to =
        crate::utils::redirect::strict_safe_internal_path_or(params.return_to.as_deref(), "")
            .to_owned();

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
                if return_to.is_empty() {
                    None
                } else {
                    Some(return_to.as_str())
                },
                current_theme.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

// ─── POST /admin/report/resolve ───────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the resolve report request.
pub(crate) struct ResolveReportForm {
    /// The report identifier.
    report_id: i64,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the resolve report request.
pub(crate) async fn resolve_report(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ResolveReportForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;

            db::resolve_report(&conn, form.report_id, admin_id)?;

            if let Err(error) = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "resolve_report",
                "report",
                Some(form.report_id),
                "",
                "",
            ) {
                tracing::error!(
                    target: "admin",
                    admin_id,
                    report_id = form.report_id,
                    error = %error,
                    "Report resolution completed without audit-log record"
                );
            }
            tracing::info!(target: "admin", report_id = form.report_id, "Report resolved");
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(
        super::admin_panel_redirect_anchor_open("Report resolved.", "reports", "reports")
            .into_response(),
    )
}

// ─── POST /admin/ip/report ──────────────────────────────────────────────────

#[derive(Deserialize)]
/// Form fields accepted by the IP report request.
pub(crate) struct IpReportForm {
    /// The post identifier.
    post_id: i64,
    /// The thread identifier.
    thread_id: i64,
    /// The board.
    board: String,
    /// The IP hash.
    ip_hash: String,
    /// The reason.
    reason: String,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the admin IP report request.
pub(crate) async fn admin_ip_report(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<IpReportForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return Err(AppError::BadRequest(
            "Report reason cannot be empty.".into(),
        ));
    }

    if form.ip_hash.len() != 64 || !form.ip_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest("Invalid IP hash.".into()));
    }

    let board_short = form
        .board
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();

    let submission = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<db::ReportSubmission> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;
            let post = db::get_post(&conn, form.post_id)?
                .ok_or_else(|| AppError::BadRequest("Post not found.".into()))?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::BadRequest("Board not found.".into()))?;
            let same_board = post.board_id == board.id;
            let same_thread = post.thread_id == form.thread_id;
            if !same_board || !same_thread {
                return Err(AppError::BadRequest(
                    "Reported post does not match the selected board/thread.".into(),
                ));
            }
            if post.ip_hash.as_deref() != Some(form.ip_hash.as_str()) {
                return Err(AppError::BadRequest(
                    "Reported post does not match the selected IP hash.".into(),
                ));
            }

            let report_reason = format!(
                "{reason}\n\nHashed IP: {ip_hash}\nIP history: /admin/ip/{ip_hash}",
                reason = reason,
                ip_hash = form.ip_hash,
            );
            let reporter_hash = format!("admin:{admin_id}");
            let submission = db::file_report(&conn, form.post_id, &report_reason, &reporter_hash)?;
            if matches!(submission, db::ReportSubmission::Filed) {
                let ip_hash_prefix = form.ip_hash.chars().take(16).collect::<String>();
                if let Err(error) = db::log_mod_action(
                    &conn,
                    admin_id,
                    &admin_name,
                    "report",
                    "report",
                    Some(form.post_id),
                    &board.short_name,
                    &format!("ip_hash={ip_hash_prefix} report filed"),
                ) {
                    tracing::error!(
                        target: "admin",
                        admin_id,
                        post_id = form.post_id,
                        board = %board.short_name,
                        error = %error,
                        "IP history report completed without audit-log record"
                    );
                }
                tracing::info!(
                    target: "admin",
                    post_id = form.post_id,
                    ip_hash = %form.ip_hash,
                    "Admin filed IP history report"
                );
            }
            Ok(submission)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let flash = match submission {
        db::ReportSubmission::Filed => "Report filed.",
        db::ReportSubmission::AlreadyFiled => "A matching report is already open.",
    };

    Ok(super::admin_panel_redirect_anchor_open(flash, "reports", "reports").into_response())
}

#[cfg(test)]
mod tests {
    use super::super::{admin_panel_redirect_anchor_open, SESSION_COOKIE};
    use super::*;
    use anyhow::{ensure, Context as _};
    use axum::extract::State;
    use axum_extra::extract::cookie::{Cookie, CookieJar};

    fn admin_signed_csrf() -> String {
        crate::utils::crypto::make_scoped_csrf_form_token(
            "csrf123",
            &crate::config::CONFIG.cookie_secret,
            "session123",
        )
    }

    fn build_admin_jar() -> CookieJar {
        CookieJar::new()
            .add(Cookie::new(SESSION_COOKIE, "session123"))
            .add(Cookie::new("csrf_token", "csrf123"))
    }

    fn admin_headers() -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::HOST,
            axum::http::HeaderValue::from_static("localhost"),
        );
        headers.insert(
            axum::http::header::ORIGIN,
            axum::http::HeaderValue::from_static("http://localhost"),
        );
        headers
    }

    fn sample_new_post(
        board_id: i64,
        thread_id: i64,
        ip_hash: Option<&str>,
        is_op: bool,
    ) -> db::NewPost {
        db::NewPost {
            thread_id,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: None,
            body: if is_op {
                "op body".to_owned()
            } else {
                "reply body".to_owned()
            },
            body_html: if is_op {
                "<p>op body</p>".to_owned()
            } else {
                "<p>reply body</p>".to_owned()
            },
            ip_hash: ip_hash.map(str::to_owned),
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: if is_op {
                "token-op".to_owned()
            } else {
                "token-reply".to_owned()
            },
            is_op,
        }
    }

    #[test]
    fn resolve_report_redirect_reopens_moderation_section() -> anyhow::Result<()> {
        let response = admin_panel_redirect_anchor_open("Report resolved.", "reports", "reports")
            .into_response();
        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("report redirect omitted a valid location header")?;

        ensure!(
            location.ends_with("#reports"),
            "report redirect did not target the reports anchor"
        );
        ensure!(
            location.contains("open=reports"),
            "report redirect did not reopen the reports section"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_ip_report_records_admin_as_reporter() -> anyhow::Result<()> {
        let state = crate::test_support::app_state();
        let target_ip =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned();
        let conn = state.db.get().context("get test database connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("hunter2").context("hash admin password")?;
        let admin_id =
            db::create_admin(&conn, "admin", &password_hash).context("create test admin")?;
        db::create_session(&conn, "session123", admin_id, Utc::now().timestamp() + 3600)
            .context("create test admin session")?;
        db::create_board(&conn, "test", "Test", "", false).context("create test board")?;
        let board = db::get_board_by_short(&conn, "test")
            .context("load test board")?
            .context("test board was not persisted")?;

        let op = sample_new_post(board.id, 0, Some(&target_ip), true);
        let (thread_id, _, _) =
            db::create_thread_with_optional_poll(&conn, board.id, None, &op, "", None, None)
                .context("create reported thread")?;
        let reply = sample_new_post(board.id, thread_id, Some(&target_ip), false);
        let reply_id = db::create_reply_with_thread_update(&conn, &reply, "", true, None)
            .context("create reported reply")?;
        drop(conn);

        let response = admin_ip_report(
            State(state.clone()),
            build_admin_jar(),
            admin_headers(),
            crate::test_support::connect_info(),
            Form(IpReportForm {
                post_id: reply_id,
                thread_id,
                board: "test".to_owned(),
                ip_hash: target_ip.clone(),
                reason: "Needs review".to_owned(),
                csrf: Some(admin_signed_csrf()),
            }),
        )
        .await
        .context("submit administrator IP report")?;

        let location = response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("administrator report redirect omitted a valid location")?;
        ensure!(
            location.contains("open=reports"),
            "administrator report redirect did not reopen reports"
        );
        ensure!(
            location.ends_with("#reports"),
            "administrator report redirect did not target the reports anchor"
        );

        let conn = state.db.get().context("get verification connection")?;
        let (reporter_hash, reason): (String, String) = conn
            .query_row(
                "SELECT reporter_hash, reason FROM reports WHERE post_id = ?1",
                rusqlite::params![reply_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("load saved administrator report")?;
        ensure!(
            reporter_hash == format!("admin:{admin_id}"),
            "administrator report stored the wrong reporter identity"
        );
        ensure!(
            reason.contains("Needs review"),
            "administrator report omitted its reason"
        );
        ensure!(
            reason.contains("Hashed IP:"),
            "administrator report omitted its hashed-IP label"
        );
        ensure!(
            reason.contains(&target_ip),
            "administrator report omitted the selected IP hash"
        );
        Ok(())
    }
}

// ─── GET /admin/mod-log ───────────────────────────────────────────────────────

#[derive(Deserialize)]
/// Query parameters accepted by the mod log request.
pub(crate) struct ModLogQuery {
    #[serde(default = "default_mod_log_page")]
    /// The page.
    page: i64,
}

/// Returns the default mod log page.
const fn default_mod_log_page() -> i64 {
    1
}

/// Handles the mod log page request.
pub(crate) async fn mod_log_page(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Query(params): Query<ModLogQuery>,
) -> Result<(CookieJar, Html<String>)> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    let (jar, csrf) = super::ensure_admin_csrf(
        jar,
        super::should_set_secure_cookie(&headers, secure_context),
    )?;
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
                current_theme.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}
