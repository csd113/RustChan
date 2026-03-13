// handlers/admin/mod.rs
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

pub mod auth;
pub use auth::*;

pub mod backup;
pub use backup::*;

pub mod content;
pub use content::*;

pub mod moderation;
pub use moderation::*;

pub mod settings;
pub use settings::*;

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
};
use axum::{
    extract::{Query, State},
    response::Html,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

// ─── Shared constant ──────────────────────────────────────────────────────────

const SESSION_COOKIE: &str = "chan_admin_session";

// ─── Shared form type used by auth and backup ─────────────────────────────────

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub return_to: Option<String>,
}

// ─── Shared session helpers (used by all sub-modules) ────────────────────────

/// Verify admin session and also return the admin's username.
/// For use inside `spawn_blocking` closures.
fn require_admin_session_with_name(
    conn: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<(i64, String)> {
    let admin_id = require_admin_session_sid(conn, session_id)?;
    let name = db::get_admin_name_by_id(conn, admin_id)?.unwrap_or_else(|| "unknown".to_string());
    Ok((admin_id, name))
}

/// Check CSRF using the cookie jar. Returns error on mismatch.
fn check_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let cookie_token = jar.get("csrf_token").map(|c| c.value().to_string());
    if crate::middleware::validate_csrf(cookie_token.as_deref(), form_token.unwrap_or("")) {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

/// Verify admin session from a session ID string.
/// For use inside `spawn_blocking` closures where we have an open connection.
fn require_admin_session_sid(conn: &rusqlite::Connection, session_id: Option<&str>) -> Result<i64> {
    let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session = db::get_session(conn, sid)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;
    Ok(session.admin_id)
}

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

/// Query params accepted by GET /admin/panel.
/// All fields are optional — missing = no flash message.
#[derive(Deserialize, Default)]
pub struct AdminPanelQuery {
    /// Set by `board_restore` on success: the `short_name` of the restored board.
    pub board_restored: Option<String>,
    /// Set by `board_restore` / `restore_saved_board_backup` on failure.
    pub restore_error: Option<String>,
    /// Set by `update_site_settings` on success.
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
        Some((true, format!("Restore failed: {err}")))
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

            // 1.8: Compute whether the DB file size exceeds the configured
            // warning threshold. Uses the on-disk file size (via fs::metadata)
            // rather than SQLite's PRAGMA page_count estimate for accuracy,
            // falling back to the pragma value if the path is unavailable.
            let db_size_warning = if CONFIG.db_warn_threshold_bytes > 0 {
                let file_size = std::fs::metadata(&CONFIG.database_path)
                    .map_or_else(|_| db_size_bytes.cast_unsigned(), |m| m.len());
                file_size >= CONFIG.db_warn_threshold_bytes
            } else {
                false
            };

            // Read the tor onion address from the hostname file if tor is enabled.
            let tor_address: Option<String> = if CONFIG.enable_tor_support {
                let data_dir = std::path::PathBuf::from(&CONFIG.database_path)
                    .parent()
                    .map_or_else(
                        || std::path::PathBuf::from("."),
                        std::path::Path::to_path_buf,
                    );
                let hostname_path = data_dir.join("tor_hidden_service").join("hostname");
                std::fs::read_to_string(&hostname_path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                None
            };

            let flash_ref = flash.as_ref().map(|(is_err, msg)| (*is_err, msg.as_str()));

            Ok(crate::templates::admin_panel_page(
                &boards,
                &bans,
                &filters,
                collapse_greentext,
                &csrf_clone,
                &full_backups,
                &board_backups_list,
                db_size_bytes,
                db_size_warning,
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
