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
    models::BoardAccessMode,
    utils::crypto::hash_password,
};
use axum::{
    extract::{Form, Multipart, State},
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
    default_theme: Option<String>,
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
    access_mode: Option<String>,
    access_password: Option<String>,
    clear_access_password: Option<String>,
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
    let access_mode = BoardAccessMode::from_db_str(form.access_mode.as_deref().unwrap_or("public"))
        .ok_or_else(|| AppError::BadRequest("Invalid board access mode.".into()))?;
    let access_password = form.access_password.clone().unwrap_or_default();
    if access_password.chars().count() > 256 {
        return Err(AppError::BadRequest(
            "Board password must be 256 characters or fewer.".into(),
        ));
    }
    let board_id = form.board_id;

    let board_short = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            let existing_password_hash: String = conn.query_row(
                "SELECT access_password_hash FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            let resolved_default_theme = form
                .default_theme
                .as_deref()
                .map(db::sanitize_theme_slug)
                .filter(|slug| {
                    slug.is_empty()
                        || db::get_theme(&conn, slug)
                            .ok()
                            .flatten()
                            .is_some_and(|theme| theme.enabled)
                })
                .unwrap_or_default();
            let access_password_hash = if access_password.is_empty() {
                if form.clear_access_password.as_deref() == Some("1") {
                    String::new()
                } else {
                    existing_password_hash
                }
            } else {
                hash_password(&access_password)?
            };
            if access_mode.requires_post_password() && access_password_hash.is_empty() {
                return Err(AppError::BadRequest(
                    "Protected boards require a password before they can be saved.".into(),
                ));
            }
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
                &resolved_default_theme,
                access_mode,
                &access_password_hash,
            )?;
            tracing::info!(
                target: "admin",
                board = %board_short,
                board_id = board_id,
                "Saved board settings"
            );
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let board_anchor = format!("board-{board_short}");
    Ok(super::admin_panel_redirect_anchor_open(
        "Board settings saved.",
        &board_anchor,
        &board_anchor,
    )
    .into_response())
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
    pub default_theme: Option<String>,
}

#[derive(Deserialize)]
pub struct FullBackupSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub auto_full_backup_interval_hours: Option<String>,
    pub auto_full_backup_copies_to_keep: Option<String>,
}

pub async fn update_site_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<SiteSettingsForm>,
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
                .map(db::sanitize_theme_slug)
                .unwrap_or_default();
            let new_theme = if new_theme.is_empty() {
                crate::theme::HARD_DEFAULT_THEME.to_string()
            } else if db::get_theme(&conn, &new_theme)?.is_some_and(|theme| theme.enabled) {
                new_theme
            } else {
                crate::theme::HARD_DEFAULT_THEME.to_string()
            };
            db::set_site_setting(&conn, "default_theme", &new_theme)?;
            db::sync_live_theme_state(&conn)?;
            tracing::info!(target: "admin", "Default theme updated");

            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?settings_saved=1").into_response())
}

pub async fn update_full_backup_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<FullBackupSettingsForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let interval_hours = form
        .auto_full_backup_interval_hours
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(CONFIG.auto_full_backup_interval_hours)
        .min(8_760);
    let copies_to_keep = form
        .auto_full_backup_copies_to_keep
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(CONFIG.auto_full_backup_copies_to_keep)
        .clamp(1, 1_000);

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let auto_backup_settings = state.auto_full_backup_settings.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            auto_backup_settings.update(interval_hours, copies_to_keep);
            crate::config::update_settings_file_auto_full_backup(interval_hours, copies_to_keep);
            tracing::info!(
                target: "admin",
                interval_hours,
                copies_to_keep,
                "Automatic full-backup settings updated"
            );
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(super::admin_panel_redirect_anchor(
        "Automatic full-backup settings saved.",
        "full-backup-restore",
    )
    .into_response())
}

#[derive(Deserialize)]
pub struct CreateThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub swatch_hex: Option<String>,
    pub custom_css: String,
    pub enabled: Option<String>,
}

pub async fn create_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateThemeForm>,
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
            let slug = db::sanitize_theme_slug(&form.slug);
            if slug.is_empty() {
                return Err(AppError::BadRequest("Theme slug is required.".into()));
            }
            if db::is_builtin_slug(&slug) {
                return Err(AppError::BadRequest(
                    "That slug is reserved by a built-in theme.".into(),
                ));
            }
            db::create_custom_theme(
                &conn,
                &slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or("")),
                &db::sanitize_theme_css(&form.custom_css),
                form.enabled.as_deref() == Some("1"),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme created.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
}

#[derive(Deserialize)]
pub struct UpdateThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub existing_slug: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub swatch_hex: Option<String>,
    pub custom_css: Option<String>,
    pub enabled: Option<String>,
}

pub async fn update_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UpdateThemeForm>,
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
            let existing_slug = db::sanitize_theme_slug(&form.existing_slug);
            let theme = db::get_theme(&conn, &existing_slug)?
                .ok_or_else(|| AppError::BadRequest("Theme not found.".into()))?;
            let mut new_slug = db::sanitize_theme_slug(&form.slug);
            if theme.is_builtin {
                new_slug = existing_slug.clone();
            }
            if new_slug.is_empty() {
                return Err(AppError::BadRequest("Theme slug is required.".into()));
            }
            let custom_css = form.custom_css.as_deref().map(db::sanitize_theme_css);
            db::update_theme(
                &conn,
                &existing_slug,
                &new_slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or("")),
                form.enabled.as_deref() == Some("1"),
                custom_css.as_deref(),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme updated.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
}

#[derive(Deserialize)]
pub struct DeleteThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub slug: String,
}

pub async fn delete_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteThemeForm>,
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
            let slug = db::sanitize_theme_slug(&form.slug);
            db::delete_custom_theme(&conn, &slug)?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme deleted.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
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
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
