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
use axum::response::IntoResponse as _;
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

mod appearance;
mod backup_settings;
mod banners;
mod board;
mod maintenance;
mod network;
mod runtime;
mod site;
mod themes;

pub(in crate::server) use appearance::*;
pub(in crate::server) use backup_settings::*;
pub(in crate::server) use banners::*;
pub(in crate::server) use board::*;
pub(in crate::server) use maintenance::*;
pub(in crate::server) use network::*;
pub(in crate::server) use runtime::*;
pub(in crate::server) use site::*;
pub(in crate::server) use themes::*;

/// Maximum permitted favicon upload bytes.
const MAX_FAVICON_UPLOAD_BYTES: usize = 5 * 1024 * 1024;
/// Maximum permitted banner upload bytes.
const MAX_BANNER_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

fn format_favicon_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Favicon upload failed.".to_owned())
}

fn format_banner_upload_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .filter(|msg| !msg.trim().is_empty() && !msg.starts_with("write "))
        .last()
        .unwrap_or_else(|| "Banner upload failed.".to_owned())
}

pub(super) fn checkbox_is_on(value: Option<&str>) -> bool {
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

// POST /admin/board/settings

/// Save the site NSFW default; existing visitor preferences retain priority.
pub(in crate::server) async fn update_visitor_defaults(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<std::collections::BTreeMap<String, String>>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.get("_csrf").map(String::as_str),
    )?;
    let hide = match form.get("default_hide_nsfw_boards").map(String::as_str) {
        Some("true") => true,
        Some("false") => false,
        _ => return Err(AppError::BadRequest("Choose an NSFW default.".into())),
    };
    let session = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let conn = state.db.get()?;
        require_admin_session_sid(&conn, session.as_deref())?;
        db::set_site_setting(
            &conn,
            "default_hide_nsfw_boards",
            if hide { "1" } else { "0" },
        )?;
        crate::templates::set_live_hide_nsfw_default(hide);
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(admin_panel_redirect_anchor(
        "Visitor default saved and applied live. Existing visitor preferences keep priority.",
        "site-settings",
    )
    .into_response())
}
