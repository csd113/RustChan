// handlers/admin/settings.rs
//
// Board settings, site settings, and maintenance (vacuum) handlers.
// All routes require a valid admin session cookie.

use crate::{
    banner,
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::AppState,
    models::{BannerScope, BannerTargetType, BoardAccessMode, BoardBannerMode},
    utils::crypto::hash_password,
};
use axum::{
    extract::{Form, Multipart, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{Html, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;

use super::{
    admin_panel_error_redirect_anchor, admin_panel_error_redirect_anchor_open,
    admin_panel_redirect_anchor, admin_panel_redirect_anchor_open, check_admin_csrf_jar,
    require_admin_post_origin_and_csrf, require_admin_session_sid, require_same_origin_request,
    SESSION_COOKIE,
};

/// Implements appearance handler support.
mod appearance;
/// Implements backup settings handler support.
mod backup_settings;
/// Implements banners handler support.
mod banners;
/// Implements board handler support.
mod board;
/// Implements maintenance handler support.
mod maintenance;
/// Implements site handler support.
mod site;
/// Implements themes handler support.
mod themes;

pub(crate) use appearance::*;
pub(crate) use backup_settings::*;
pub(crate) use banners::*;
pub(crate) use board::*;
pub(crate) use maintenance::*;
pub(crate) use site::*;
pub(crate) use themes::*;

/// Maximum permitted favicon upload bytes.
const MAX_FAVICON_UPLOAD_BYTES: usize = 5 * 1024 * 1024;
/// Maximum permitted banner upload bytes.
const MAX_BANNER_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Formats favicon upload error.
fn format_favicon_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Favicon upload failed.".to_owned())
}

/// Formats banner upload error.
fn format_banner_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Banner upload failed.".to_owned())
}

/// Performs the checkbox is on handler operation.
fn checkbox_is_on(value: Option<&str>) -> bool {
    value == Some("1")
        || value.is_some_and(|item| item.eq_ignore_ascii_case("on"))
        || value.is_some_and(|item| item.eq_ignore_ascii_case("true"))
}

/// Handles the read text field request.
async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> Result<String> {
    field
        .text()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))
}

/// Handles the read checkbox field request.
async fn read_checkbox_field(field: axum::extract::multipart::Field<'_>) -> Result<bool> {
    Ok(checkbox_is_on(Some(&read_text_field(field).await?)))
}

/// Handles the read limited upload bytes request.
async fn read_limited_upload_bytes(
    mut field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let next_chunk = field
            .chunk()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let Some(chunk) = next_chunk else {
            break;
        };
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
