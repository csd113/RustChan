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
    extract::{Form, Multipart, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

const MAX_FAVICON_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

fn format_favicon_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(std::string::ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Favicon upload failed.".to_string())
}

async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> Result<String> {
    field
        .text()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))
}

async fn read_limited_upload_bytes(
    mut field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        if out.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::UploadTooLarge(format!(
                "File too large. Maximum upload size is {} MiB.",
                max_bytes / 1024 / 1024
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

// ─── POST /admin/board/settings ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardSettingsForm {
    board_id: i64,
    name: String,
    description: String,
    bump_limit: Option<String>,
    max_threads: Option<String>,
    max_archived_threads: Option<String>,
    nsfw: Option<String>,
    allow_images: Option<String>,
    allow_video: Option<String>,
    allow_audio: Option<String>,
    allow_any_files: Option<String>,
    allow_tripcodes: Option<String>,
    allow_editing: Option<String>,
    edit_window_secs: Option<String>,
    allow_archive: Option<String>,
    allow_video_embeds: Option<String>,
    allow_captcha: Option<String>,
    show_poster_ids: Option<String>,
    collapse_greentext: Option<String>,
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
    let max_archived_threads = form
        .max_archived_threads
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150)
        .clamp(1, 10_000);
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
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            db::update_board_settings(
                &mut conn,
                board_id,
                &name,
                &description,
                form.nsfw.as_deref() == Some("1"),
                bump_limit,
                max_threads,
                max_archived_threads,
                form.allow_images.as_deref() == Some("1"),
                form.allow_video.as_deref() == Some("1"),
                form.allow_audio.as_deref() == Some("1"),
                CONFIG.enable_any_file_uploads_feature
                    && form.allow_any_files.as_deref() == Some("1"),
                form.allow_tripcodes.as_deref() == Some("1"),
                edit_window_secs,
                form.allow_editing.as_deref() == Some("1"),
                form.allow_archive.as_deref() == Some("1"),
                form.allow_video_embeds.as_deref() == Some("1"),
                form.allow_captcha.as_deref() == Some("1"),
                form.show_poster_ids.as_deref() == Some("1"),
                form.collapse_greentext.as_deref() == Some("1"),
                post_cooldown_secs,
            )?;
            tracing::info!(
                target: "admin",
                board = %board_short,
                board_id = board_id,
                "Saved board settings"
            );
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect("Board settings saved.").into_response())
}

#[derive(Deserialize)]
pub struct ClearBoardFaviconForm {
    board_id: i64,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn clear_board_favicon_override(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ClearBoardFaviconForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let board_short = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![form.board_id],
                |row| row.get(0),
            )?;
            crate::favicon::clear_board_favicon(&board_short)?;
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect_anchor(
        &format!("Board /{board_short}/ favicon override cleared."),
        &format!("board-{board_short}"),
    )
    .into_response())
}

pub async fn update_site_favicon(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_same_origin_request(&headers)?;

    let mut csrf = None;
    let mut favicon_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("favicon") => {
                let bytes = read_limited_upload_bytes(field, MAX_FAVICON_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    favicon_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    super::check_csrf_jar(&jar, csrf.as_deref())?;
    let favicon_bytes =
        favicon_bytes.ok_or_else(|| AppError::BadRequest("No favicon file uploaded.".into()))?;

    let favicon_result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            crate::favicon::write_favicon_set(
                crate::favicon::FaviconScope::Global,
                &favicon_bytes,
            )?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match favicon_result {
        Ok(()) => Ok(super::admin_panel_redirect_anchor(
            "Global favicon updated.",
            "site-settings",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor(
            &format_favicon_upload_error(&error),
            "site-settings",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn update_board_favicon(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_same_origin_request(&headers)?;

    let mut csrf = None;
    let mut board_id = None;
    let mut favicon_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("board_id") => {
                board_id = read_text_field(field).await?.trim().parse::<i64>().ok();
            }
            Some("favicon") => {
                let bytes = read_limited_upload_bytes(field, MAX_FAVICON_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    favicon_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    super::check_csrf_jar(&jar, csrf.as_deref())?;
    let board_id = board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
    let favicon_bytes =
        favicon_bytes.ok_or_else(|| AppError::BadRequest("No favicon file uploaded.".into()))?;

    let favicon_result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            crate::favicon::write_favicon_set(
                crate::favicon::FaviconScope::Board(&board_short),
                &favicon_bytes,
            )?;
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match favicon_result {
        Ok(board_short) => Ok(super::admin_panel_redirect_anchor(
            &format!("Board /{board_short}/ favicon updated."),
            &format!("board-{board_short}"),
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor(
            &format_favicon_upload_error(&error),
            "site-settings",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

// ─── POST /admin/site/settings ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SiteSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
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
            tracing::info!(target: "admin", "Site name updated");

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
            tracing::info!(target: "admin", "Site subtitle updated");

            // Persist both values back to settings.toml so they survive a
            // server restart without requiring a manual file edit.
            crate::config::update_settings_file_site_names(&new_name, &new_subtitle);
            tracing::info!(target: "admin", "settings.toml updated");

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
            tracing::info!(target: "admin", "Default theme updated");

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

#[derive(Deserialize)]
pub struct DbMaintenanceForm {
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

            tracing::info!(
                target: "admin",
                before_bytes = size_before,
                after_bytes  = size_after,
                saved_bytes  = saved,
                "Admin ran VACUUM"
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

pub async fn admin_db_check(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DbMaintenanceForm>,
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

            let report = db::check_db_health(&conn);
            tracing::info!(
                target: "admin",
                ok = report.before_ok,
                before = report.before_check,
                "Admin ran database integrity check"
            );

            Ok(crate::templates::admin_db_health_result_page(
                &report,
                false,
                &csrf_clone,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

pub async fn admin_db_repair(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DbMaintenanceForm>,
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

            let report = db::attempt_db_repair(&conn);
            tracing::info!(
                target: "admin",
                before_ok = report.before_ok,
                after_ok = report.after_ok,
                steps = report.repair_steps.len(),
                "Admin ran database repair attempt"
            );

            Ok(crate::templates::admin_db_health_result_page(
                &report,
                true,
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
    // Move auth check and all DB calls into spawn_blocking.
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
