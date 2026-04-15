#![allow(
    clippy::too_many_lines,
    clippy::semicolon_if_nothing_returned,
    clippy::option_if_let_else,
    clippy::uninlined_format_args,
    clippy::useless_let_if_seq,
    clippy::assigning_clones
)]

// handlers/admin/settings.rs
//
// Board settings, site settings, and maintenance (vacuum) handlers.
// All routes require a valid admin session cookie.

use crate::{
    banner,
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::board::ensure_csrf,
    middleware::AppState,
    models::{BannerScope, BoardAccessMode, BoardBannerMode},
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
const MAX_BANNER_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

fn format_favicon_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(std::string::ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Favicon upload failed.".to_string())
}

fn format_banner_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(std::string::ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Banner upload failed.".to_string())
}

fn checkbox_is_on(value: Option<&str>) -> bool {
    value == Some("1")
        || value.is_some_and(|item| item.eq_ignore_ascii_case("on"))
        || value.is_some_and(|item| item.eq_ignore_ascii_case("true"))
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
    banner_mode: Option<String>,
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
    let banner_mode =
        BoardBannerMode::from_db_str(form.banner_mode.as_deref().unwrap_or("inherit"))
            .ok_or_else(|| AppError::BadRequest("Invalid board banner mode.".into()))?;

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
                banner_mode,
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
    pub banner_rotation_interval_minutes: Option<String>,
    pub banner_external_links_enabled: Option<String>,
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
    let is_banner_settings_only = form.site_name.is_none()
        && form.site_subtitle.is_none()
        && form.default_theme.is_none()
        && (form.banner_rotation_interval_minutes.is_some()
            || form.banner_external_links_enabled.is_some());
    let banner_rotation_interval_minutes = form
        .banner_rotation_interval_minutes
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 43_200);
    let banner_external_links_enabled =
        checkbox_is_on(form.banner_external_links_enabled.as_deref());

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            // Save the custom site name (trimmed, max 64 chars).
            let new_name = form.site_name.as_deref().map_or_else(
                || db::get_site_name(&conn),
                |value| value.trim().chars().take(64).collect::<String>(),
            );
            db::set_site_setting(&conn, "site_name", &new_name)?;
            // Update the in-memory live name so all pages reflect it immediately.
            crate::templates::set_live_site_name(&new_name);
            tracing::info!(target: "admin", "Site name updated");

            // Save the custom subtitle.
            let new_subtitle = form.site_subtitle.as_deref().map_or_else(
                || db::get_site_subtitle(&conn),
                |value| value.trim().chars().take(128).collect::<String>(),
            );
            db::set_site_setting(&conn, "site_subtitle", &new_subtitle)?;
            crate::templates::set_live_site_subtitle(&new_subtitle);
            tracing::info!(target: "admin", "Site subtitle updated");

            // Persist both values back to settings.toml so they survive a
            // server restart without requiring a manual file edit.
            crate::config::update_settings_file_site_names(&new_name, &new_subtitle);
            tracing::info!(target: "admin", "settings.toml updated");

            // Save the default theme slug (validated against allowed values).
            let new_theme = if let Some(value) = form.default_theme.as_deref() {
                let candidate = db::sanitize_theme_slug(value);
                if candidate.is_empty() {
                    crate::theme::HARD_DEFAULT_THEME.to_string()
                } else if db::get_theme(&conn, &candidate)?.is_some_and(|theme| theme.enabled) {
                    candidate
                } else {
                    crate::theme::HARD_DEFAULT_THEME.to_string()
                }
            } else {
                db::get_default_user_theme(&conn)
            };
            db::set_site_setting(&conn, "default_theme", &new_theme)?;
            db::sync_live_theme_state(&conn)?;
            tracing::info!(target: "admin", "Default theme updated");

            db::set_site_setting(
                &conn,
                "banner_rotation_interval_minutes",
                &banner_rotation_interval_minutes.to_string(),
            )?;
            db::set_site_setting(
                &conn,
                "banner_external_links_enabled",
                if banner_external_links_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            tracing::info!(target: "admin", "Banner settings updated");

            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if is_banner_settings_only {
        Ok(super::admin_panel_redirect_anchor_open(
            "Banner settings saved.",
            "board-banners",
            "board-banners",
        )
        .into_response())
    } else {
        Ok(Redirect::to("/admin/panel?settings_saved=1").into_response())
    }
}

struct ParsedBannerUpload {
    csrf: Option<String>,
    board_id: Option<i64>,
    target_type: String,
    target_value: Option<String>,
    target_board_value: Option<String>,
    target_thread_value: Option<String>,
    target_external_url: Option<String>,
    show_on_index: bool,
    show_on_catalog: bool,
    enabled: bool,
    banner_bytes: Vec<u8>,
}

async fn parse_banner_upload(mut multipart: Multipart) -> Result<ParsedBannerUpload> {
    let mut csrf = None;
    let mut board_id = None;
    let mut target_type = String::from("none");
    let mut target_value = None;
    let mut target_board_value = None;
    let mut target_thread_value = None;
    let mut target_external_url = None;
    let mut show_on_index = true;
    let mut show_on_catalog = true;
    let mut enabled = true;
    let mut banner_bytes = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("board_id") => board_id = read_text_field(field).await?.trim().parse::<i64>().ok(),
            Some("target_type") => target_type = read_text_field(field).await?,
            Some("target_value") => target_value = Some(read_text_field(field).await?),
            Some("target_board_value") => target_board_value = Some(read_text_field(field).await?),
            Some("target_thread_value") => {
                target_thread_value = Some(read_text_field(field).await?)
            }
            Some("target_external_url") => {
                target_external_url = Some(read_text_field(field).await?)
            }
            Some("show_on_index") => {
                show_on_index = checkbox_is_on(Some(&read_text_field(field).await?))
            }
            Some("show_on_catalog") => {
                show_on_catalog = checkbox_is_on(Some(&read_text_field(field).await?))
            }
            Some("enabled") => enabled = checkbox_is_on(Some(&read_text_field(field).await?)),
            Some("banner") => {
                let bytes = read_limited_upload_bytes(field, MAX_BANNER_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    banner_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    Ok(ParsedBannerUpload {
        csrf,
        board_id,
        target_type,
        target_value,
        target_board_value,
        target_thread_value,
        target_external_url,
        show_on_index,
        show_on_catalog,
        enabled,
        banner_bytes: banner_bytes
            .ok_or_else(|| AppError::BadRequest("No banner file uploaded.".into()))?,
    })
}

#[derive(Deserialize)]
pub struct BannerMetaForm {
    pub banner_id: i64,
    pub target_type: String,
    pub target_value: Option<String>,
    pub target_board_value: Option<String>,
    pub target_thread_value: Option<String>,
    pub target_external_url: Option<String>,
    pub enabled: Option<String>,
    pub show_on_index: Option<String>,
    pub show_on_catalog: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteBannerForm {
    pub banner_id: i64,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct MoveBannerForm {
    pub banner_id: i64,
    pub direction: String,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct ClearBoardBannerForm {
    pub board_id: i64,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

async fn board_anchor_from_id(state: &AppState, board_id: i64) -> Result<String> {
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board_short = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get::<_, String>(0),
            )?;
            Ok(format!("board-{board_short}"))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}

async fn upload_banner_for_scope(
    state: AppState,
    session_id: Option<String>,
    scope: BannerScope,
    board_id: Option<i64>,
    parsed: ParsedBannerUpload,
) -> Result<String> {
    tokio::task::spawn_blocking(move || -> Result<String> {
        let conn = state.db.get()?;
        super::require_admin_session_sid(&conn, session_id.as_deref())?;
        let allow_external_links = db::get_banner_external_links_enabled(&conn);
        let selected_target_value = banner::select_banner_target_value(
            &parsed.target_type,
            parsed.target_value.as_deref(),
            parsed.target_board_value.as_deref(),
            parsed.target_thread_value.as_deref(),
            parsed.target_external_url.as_deref(),
        );
        let (target_type, target_value) = banner::parse_banner_target(
            &parsed.target_type,
            &selected_target_value,
            allow_external_links,
        )?;

        let board_short = if scope == BannerScope::Board {
            let id = board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
            Some(conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get::<_, String>(0),
            )?)
        } else {
            None
        };

        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let draft_asset = crate::models::BannerAsset {
            id: 0,
            scope,
            board_id,
            board_short: board_short.clone(),
            storage_key: storage_key.clone(),
            width: 0,
            height: 0,
            file_size: 0,
            enabled: parsed.enabled,
            sort_order: 1,
            target_type,
            target_value: target_value.clone(),
            show_on_index: parsed.show_on_index,
            show_on_catalog: parsed.show_on_catalog,
            created_at: chrono::Utc::now().timestamp(),
        };
        let (width, height, file_size) =
            banner::write_banner_asset(&draft_asset, &parsed.banner_bytes)?;

        let result = (|| -> Result<String> {
            let sort_order = db::next_banner_sort_order(&conn, scope, board_id)?;
            let banner_id = db::insert_banner_asset(
                &conn,
                scope,
                board_id,
                &storage_key,
                i64::from(width),
                i64::from(height),
                i64::try_from(file_size)
                    .map_err(|_| AppError::BadRequest("Banner file size is too large.".into()))?,
                parsed.enabled,
                sort_order,
                target_type,
                &target_value,
                if scope == BannerScope::Home {
                    false
                } else {
                    parsed.show_on_index
                },
                if scope == BannerScope::Home {
                    false
                } else {
                    parsed.show_on_catalog
                },
            )?;
            if scope == BannerScope::Board {
                let board_id =
                    board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
                conn.execute(
                    "UPDATE boards SET banner_mode = 'override' WHERE id = ?1",
                    rusqlite::params![board_id],
                )?;
            }
            let anchor = match scope {
                BannerScope::Global => "global-banners".to_string(),
                BannerScope::Home => "home-banners".to_string(),
                BannerScope::Board => {
                    format!("board-{}", board_short.as_deref().unwrap_or_default())
                }
            };
            tracing::info!(
                target: "admin",
                banner_id,
                scope = %scope,
                "Banner uploaded"
            );
            Ok(anchor)
        })();

        if result.is_err() {
            let _ = banner::delete_banner_asset_file(&draft_asset);
        }
        result
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}

pub async fn upload_global_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers)?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_csrf_jar(&jar, parsed.csrf.as_deref())?;
    match upload_banner_for_scope(state, session_id, BannerScope::Global, None, parsed).await {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Global banner uploaded.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "global-banners",
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            "global-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn upload_home_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers)?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_csrf_jar(&jar, parsed.csrf.as_deref())?;
    match upload_banner_for_scope(state, session_id, BannerScope::Home, None, parsed).await {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Home page banner uploaded.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "home-banners",
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            "home-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn upload_board_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers)?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_csrf_jar(&jar, parsed.csrf.as_deref())?;
    let board_id = parsed
        .board_id
        .ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
    let board_anchor = board_anchor_from_id(&state, board_id).await?;
    match upload_banner_for_scope(
        state,
        session_id,
        BannerScope::Board,
        Some(board_id),
        parsed,
    )
    .await
    {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Board banner saved.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            &board_anchor,
            &board_anchor,
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            &board_anchor,
            &board_anchor,
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn update_banner_meta(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BannerMetaForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = db::get_banner_asset(&conn, form.banner_id)?
                .ok_or_else(|| AppError::BadRequest("Banner not found.".into()))?;
            let selected_target_value = banner::select_banner_target_value(
                &form.target_type,
                form.target_value.as_deref(),
                form.target_board_value.as_deref(),
                form.target_thread_value.as_deref(),
                form.target_external_url.as_deref(),
            );
            let (target_type, target_value) = banner::parse_banner_target(
                &form.target_type,
                &selected_target_value,
                db::get_banner_external_links_enabled(&conn),
            )?;
            db::update_banner_asset_meta(
                &conn,
                form.banner_id,
                checkbox_is_on(form.enabled.as_deref()),
                target_type,
                &target_value,
                if asset.scope == BannerScope::Home {
                    false
                } else {
                    checkbox_is_on(form.show_on_index.as_deref())
                },
                if asset.scope == BannerScope::Home {
                    false
                } else {
                    checkbox_is_on(form.show_on_catalog.as_deref())
                },
            )?;
            Ok(match asset.scope {
                BannerScope::Global => "global-banners".to_string(),
                BannerScope::Home => "home-banners".to_string(),
                BannerScope::Board => format!("board-{}", asset.board_short.unwrap_or_default()),
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    match result {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Banner settings saved.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "board-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn delete_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    let anchor = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = db::delete_banner_asset(&conn, form.banner_id)?;
            banner::delete_banner_asset_file(&asset)?;
            Ok(match asset.scope {
                BannerScope::Global => "global-banners".to_string(),
                BannerScope::Home => "home-banners".to_string(),
                BannerScope::Board => format!("board-{}", asset.board_short.unwrap_or_default()),
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor_open(
        "Banner deleted.",
        &anchor,
        banner::banner_open_section(&anchor),
    )
    .into_response())
}

pub async fn move_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<MoveBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    let move_up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => {
            return Err(AppError::BadRequest(
                "Invalid banner move direction.".into(),
            ))
        }
    };
    let anchor = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = db::get_banner_asset(&conn, form.banner_id)?
                .ok_or_else(|| AppError::BadRequest("Banner not found.".into()))?;
            db::move_banner_asset(&mut conn, form.banner_id, move_up)?;
            Ok(match asset.scope {
                BannerScope::Global => "global-banners".to_string(),
                BannerScope::Home => "home-banners".to_string(),
                BannerScope::Board => format!("board-{}", asset.board_short.unwrap_or_default()),
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor_open(
        "Banner order updated.",
        &anchor,
        banner::banner_open_section(&anchor),
    )
    .into_response())
}

pub async fn clear_board_banner_override(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ClearBoardBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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
            let assets = db::delete_board_banner_assets(&conn, form.board_id)?;
            for asset in &assets {
                banner::delete_banner_asset_file(asset)?;
            }
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor(
        &format!("Board /{board_short}/ banner override cleared."),
        &format!("board-{board_short}"),
    )
    .into_response())
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
