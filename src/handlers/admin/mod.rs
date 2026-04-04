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
// Secure=true when CHAN_HTTPS_COOKIES=true (default: enabled for proxy or direct TLS).
//
// + All admin handlers now wrap DB and file I/O in
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
    http::{header, HeaderMap, Uri},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};
use std::net::IpAddr;

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

fn require_same_origin_request(headers: &HeaderMap) -> Result<()> {
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_string())
        .ok_or_else(|| AppError::Forbidden("Missing Host header.".into()))?;

    let Some(source) = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|value| value.to_str().ok())
    else {
        // Some browsers omit Origin/Referer on same-origin multipart uploads,
        // especially on localhost. The admin endpoints already require a valid
        // CSRF token, so treat missing headers as an allowed fallback.
        return Ok(());
    };
    if source.eq_ignore_ascii_case("null") {
        // Firefox and some privacy-restricted/local contexts can send
        // `Origin: null` on multipart form submissions from localhost. The
        // admin endpoints still require a valid session + CSRF token, so
        // treat this the same as a missing Origin/Referer fallback.
        return Ok(());
    }
    let source_uri = source
        .parse::<Uri>()
        .map_err(|_| AppError::Forbidden("Invalid Origin/Referer header.".into()))?;
    let source_host = source_uri
        .authority()
        .map(axum::http::uri::Authority::host)
        .ok_or_else(|| AppError::Forbidden("Origin/Referer header has no authority.".into()))?;

    if hosts_match_for_same_origin(source_host, &request_host) {
        Ok(())
    } else {
        tracing::warn!(
            target: "admin",
            request_host = %request_host,
            source_host = %source_host,
            source = %source,
            "Admin same-origin validation rejected request"
        );
        Err(AppError::Forbidden("Origin/Referer host mismatch.".into()))
    }
}

fn hosts_match_for_same_origin(source_host: &str, request_host: &str) -> bool {
    if source_host.eq_ignore_ascii_case(request_host) {
        return true;
    }

    is_loopback_alias(source_host) && is_loopback_alias(request_host)
}

fn is_loopback_alias(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn encode_query_component(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push_str("%20"),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

pub(super) fn should_set_secure_cookie(headers: &HeaderMap) -> bool {
    if !CONFIG.https_cookies {
        return false;
    }

    if CONFIG.tls.enabled {
        return true;
    }

    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .next()
                .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
        })
}

fn admin_panel_redirect_with_status(
    message: &str,
    is_error: bool,
    anchor: Option<&str>,
) -> Redirect {
    let key = if is_error { "flash_error" } else { "flash" };
    let mut url = format!("/admin/panel?{key}={}", encode_query_component(message));
    if let Some(anchor) = anchor.filter(|value| !value.is_empty()) {
        url.push('#');
        url.push_str(anchor);
    }
    Redirect::to(&url)
}

pub(super) fn admin_panel_redirect(message: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, None)
}

pub(super) fn admin_panel_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, Some(anchor))
}

pub(super) fn admin_panel_error_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, true, Some(anchor))
}

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

/// Query params accepted by GET /admin/panel.
/// All fields are optional — missing = no flash message.
#[derive(Deserialize, Default)]
pub struct AdminPanelQuery {
    pub flash: Option<String>,
    pub flash_error: Option<String>,
    pub backup_created: Option<String>,
    pub backup_deleted: Option<String>,
    pub restored: Option<String>,
    /// Set by `board_restore` on success: the `short_name` of the restored board.
    pub board_restored: Option<String>,
    /// Set by `board_restore` / `restore_saved_board_backup` on failure.
    pub restore_error: Option<String>,
    /// Set by `update_site_settings` on success.
    pub settings_saved: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct LiveLogQuery {
    pub bytes: Option<usize>,
}

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<AdminPanelQuery>,
) -> Result<(CookieJar, Html<String>)> {
    // Move auth check and all DB calls into spawn_blocking.
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let (jar, csrf) = ensure_csrf(jar);
    let csrf_clone = csrf.clone();

    // Build the flash message from query params before entering spawn_blocking.
    let flash: Option<(bool, String)> = if let Some(err) = params.flash_error {
        Some((true, err))
    } else if let Some(msg) = params.flash {
        Some((false, msg))
    } else if let Some(err) = params.restore_error {
        Some((true, format!("Restore failed: {err}")))
    } else if let Some(board) = params.board_restored {
        Some((false, format!("Board /{board}/ restored successfully.")))
    } else if params.backup_created.is_some() {
        Some((false, "Backup saved on the server.".to_string()))
    } else if params.backup_deleted.is_some() {
        Some((false, "Backup deleted.".to_string()))
    } else if params.restored.is_some() {
        Some((false, "Restore completed successfully.".to_string()))
    } else if params.settings_saved.is_some() {
        Some((false, "Site settings saved.".to_string()))
    } else {
        None
    };

    // Read onion address before entering spawn_blocking — await is not allowed
    // inside the synchronous closure.
    let onion_address_val: Option<String> = if CONFIG.enable_tor_support {
        state.onion_address.read().await.clone()
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

            // Onion address resolved before spawn_blocking (see above).
            let tor_address: Option<String> = onion_address_val;
            let flash_ref = flash.as_ref().map(|(is_err, msg)| (*is_err, msg.as_str()));

            Ok(crate::templates::admin_panel_page(
                &boards,
                &bans,
                &filters,
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

pub async fn admin_live_log(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<LiveLogQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
    let max_bytes = params.bytes.unwrap_or(65_536).clamp(4_096, 262_144);

    let payload = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let logs_dir = crate::config::logs_dir();

            let Some(path) = latest_log_file(&logs_dir) else {
                return Ok(
                    serde_json::json!({
                        "filename": "no log file",
                        "content": "No live log file found yet.",
                        "truncated": false
                    })
                    .to_string(),
                );
            };

            let (content, truncated) = read_log_tail(&path, max_bytes)?;
            Ok(
                serde_json::json!({
                    "filename": path.file_name().and_then(|name| name.to_str()).unwrap_or("current log"),
                    "content": content,
                    "truncated": truncated
                })
                .to_string(),
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((
        [(header::CONTENT_TYPE, "application/json".to_string())],
        payload,
    )
        .into_response())
}

fn latest_log_file(logs_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut files = std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("log"))
        .collect::<Vec<_>>();
    files.sort();
    files.pop()
}

fn read_log_tail(path: &std::path::Path, max_bytes: usize) -> Result<(String, bool)> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open log: {e}")))?;
    let len = file
        .metadata()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Log metadata: {e}")))?
        .len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Seek log: {e}")))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Read log: {e}")))?;

    let text = String::from_utf8_lossy(&buf).into_owned();
    let truncated = start > 0;
    let content = if truncated {
        match text.find('\n') {
            Some(pos) if pos + 1 < text.len() => text[pos + 1..].to_string(),
            _ => text,
        }
    } else {
        text
    };
    Ok((content, truncated))
}

#[cfg(test)]
mod tests {
    use super::{hosts_match_for_same_origin, latest_log_file, read_log_tail};

    #[test]
    fn same_origin_accepts_exact_host_match() {
        assert!(hosts_match_for_same_origin("example.com", "example.com"));
    }

    #[test]
    fn same_origin_accepts_loopback_aliases() {
        assert!(hosts_match_for_same_origin("localhost", "127.0.0.1"));
        assert!(hosts_match_for_same_origin("127.0.0.1", "localhost"));
        assert!(hosts_match_for_same_origin("::1", "localhost"));
        assert!(hosts_match_for_same_origin("127.0.0.1", "::1"));
    }

    #[test]
    fn same_origin_rejects_different_non_loopback_hosts() {
        assert!(!hosts_match_for_same_origin("example.com", "127.0.0.1"));
        assert!(!hosts_match_for_same_origin("evil.test", "localhost"));
    }

    #[test]
    fn null_origin_is_handled_by_caller_fallback() {
        assert!(!hosts_match_for_same_origin("null", "localhost"));
    }

    #[test]
    fn picks_latest_log_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("rustchan.2026-04-01.log"), "old").expect("old");
        std::fs::write(dir.path().join("rustchan.2026-04-02.log"), "new").expect("new");
        let latest = latest_log_file(dir.path()).expect("latest");
        assert_eq!(
            latest.file_name().and_then(|name| name.to_str()),
            Some("rustchan.2026-04-02.log")
        );
    }

    #[test]
    fn reads_tail_of_log_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rustchan.2026-04-02.log");
        std::fs::write(&path, "line1\nline2\nline3\n").expect("write");
        let (content, truncated) = read_log_tail(&path, 8).expect("tail");
        assert!(truncated);
        assert!(content.contains("line3"));
    }
}
