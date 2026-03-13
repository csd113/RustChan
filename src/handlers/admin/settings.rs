// handlers/admin/settings.rs
//
// Board settings, site settings, and maintenance (vacuum) handlers.
// All routes require a valid admin session cookie.

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
};
use axum::{
    extract::{Form, Query, State},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use tracing::info;

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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn update_board_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardSettingsForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
            info!("Admin updated settings for board id={board_id}");
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/site/settings ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SiteSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    /// Checkbox: present = "1", absent = not submitted (treat as false)
    pub collapse_greentext: Option<String>,
    /// Custom site name (replaces [ `RustChan` ] on home page and footer).
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
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            const VALID_THEMES: &[&str] = &[
                "terminal",
                "aero",
                "dorfic",
                "fluorogrid",
                "neoncubicle",
                "chanclassic",
            ];
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let val = if form.collapse_greentext.as_deref() == Some("1") {
                "1"
            } else {
                "0"
            };
            db::set_site_setting(&conn, "collapse_greentext", val)?;
            info!("Admin updated site setting: collapse_greentext={val}");

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

            // Persist both values back to settings.toml so they survive a
            // server restart without requiring a manual file edit.
            crate::config::update_settings_file_site_names(&new_name, &new_subtitle);
            info!("settings.toml updated with new site_name and site_subtitle");

            // Save the default theme slug (validated against allowed values).
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
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn admin_vacuum(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<VacuumForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

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

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

/// Query params accepted by GET /admin/panel.
/// All fields are optional — missing = no flash message.
#[allow(dead_code)]
#[derive(Deserialize, Default)]
pub struct AdminPanelQuery {
    /// Set by `board_restore` on success: the `short_name` of the restored board.
    pub board_restored: Option<String>,
    /// Set by `board_restore` / `restore_saved_board_backup` on failure.
    pub restore_error: Option<String>,
    /// Set by `update_site_settings` on success.
    pub settings_saved: Option<String>,
}

#[allow(dead_code)]
pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<AdminPanelQuery>,
) -> Result<(CookieJar, Html<String>)> {
    // FIX[HIGH-3]: Move auth check and all DB calls into spawn_blocking.
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
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
            let full_backups = super::list_backup_files(&super::full_backup_dir());
            let board_backups_list = super::list_backup_files(&super::board_backup_dir());

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
