// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#![allow(
    clippy::too_many_lines,
    clippy::semicolon_if_nothing_returned,
    clippy::option_if_let_else,
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
    models::{BannerScope, BannerTargetType, BoardAccessMode, BoardBannerMode},
    utils::crypto::hash_password,
};
use axum::{
    extract::{Form, Multipart, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;

use super::{
    admin_panel_error_redirect_anchor, admin_panel_error_redirect_anchor_open,
    admin_panel_redirect_anchor, admin_panel_redirect_anchor_open, check_csrf_jar,
    require_admin_session_sid, require_same_origin_request, SESSION_COOKIE,
};

mod appearance;
mod backup_settings;
mod banners;
mod board;
mod maintenance;
mod site;
mod themes;

pub use appearance::*;
pub use backup_settings::*;
pub use banners::*;
pub use board::*;
pub use maintenance::*;
pub use site::*;
pub use themes::*;

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

async fn read_checkbox_field(field: axum::extract::multipart::Field<'_>) -> Result<bool> {
    Ok(checkbox_is_on(Some(&read_text_field(field).await?)))
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
