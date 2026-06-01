// These branches are clearer in this state module than the more compact Clippy-suggested form.
#![allow(
    clippy::option_if_let_else,
    clippy::map_unwrap_or,
    clippy::needless_pass_by_value,
    clippy::assigning_clones,
    clippy::useless_let_if_seq
)]

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
    middleware::validate_signed_csrf,
    middleware::AppState,
    models::BackupInfo,
    utils::crypto::{make_scoped_csrf_form_token, new_csrf_token},
};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, Uri},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{Seek as _, SeekFrom};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

// ─── Shared constant ──────────────────────────────────────────────────────────

const SESSION_COOKIE: &str = "chan_admin_session";
const ADMIN_COOKIE_SAME_SITE: SameSite = SameSite::Lax;
const ADMIN_BOOTSTRAP_TTL_SECS: u64 = 120;
const MISSING_ORIGIN_REFERER: &str = "Missing Origin/Referer header.";

static ADMIN_SESSION_BOOTSTRAPS: LazyLock<DashMap<String, (String, u64)>> =
    LazyLock::new(DashMap::new);

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
    let name = db::get_admin_name_by_id(conn, admin_id)?.unwrap_or_else(|| "unknown".to_owned());
    Ok((admin_id, name))
}

/// Check CSRF using the cookie jar. Returns error on mismatch.
/// Verify admin session from a session ID string.
/// For use inside `spawn_blocking` closures where we have an open connection.
pub(in crate::handlers) fn require_admin_session_sid(
    conn: &rusqlite::Connection,
    session_id: Option<&str>,
) -> Result<i64> {
    let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session = db::get_session(conn, sid)?
        .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;
    Ok(session.admin_id)
}

pub(super) fn require_same_origin_request(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Result<()> {
    let request_authority = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .ok_or_else(|| AppError::Forbidden("Missing Host header.".into()))?;
    let request_scheme = request_scheme_for_same_origin(headers, peer, request_authority.host());
    let request_port = request_authority
        .port_u16()
        .unwrap_or(if request_scheme == "https" { 443 } else { 80 });

    // Browsers and HTTPS tunnels can omit Origin in legitimate same-origin
    // admin form posts. We accept two narrow fallbacks instead of broadly
    // allowing headerless requests:
    //   1. Origin: null with a same-origin Referer (seen in some tunnel/webview flows)
    //   2. Missing Origin/Referer with Sec-Fetch-Site: same-origin
    // Cross-site and malformed cases still fail closed below.
    let Some(source) = effective_same_origin_source(headers, request_authority.host()) else {
        if request_has_same_origin_fetch_metadata(headers) {
            return Ok(());
        }
        return Err(AppError::Forbidden(MISSING_ORIGIN_REFERER.into()));
    };
    if source.eq_ignore_ascii_case("null") {
        if is_loopback_alias(request_authority.host()) {
            return Ok(());
        }
        return Err(AppError::Forbidden(
            "Origin/Referer header must be same-origin.".into(),
        ));
    }
    let source_uri = source
        .parse::<Uri>()
        .map_err(|_error| AppError::Forbidden("Invalid Origin/Referer header.".into()))?;
    let source_scheme = source_uri
        .scheme_str()
        .ok_or_else(|| AppError::Forbidden("Origin/Referer header has no scheme.".into()))?;
    let source_authority = source_uri
        .authority()
        .ok_or_else(|| AppError::Forbidden("Origin/Referer header has no authority.".into()))?;
    if source_authority.as_str().contains('@') {
        return Err(AppError::Forbidden(
            "Origin/Referer header contains invalid authority.".into(),
        ));
    }
    let source_port = source_authority.port_u16().unwrap_or_else(|| {
        if source_scheme.eq_ignore_ascii_case("https") {
            443
        } else {
            80
        }
    });

    if source_scheme.eq_ignore_ascii_case(request_scheme)
        && hosts_match_for_same_origin(source_authority.host(), request_authority.host())
        && source_port == request_port
    {
        return Ok(());
    }

    tracing::warn!(
        target: "admin",
        request_scheme,
        request_host = %request_authority.host(),
        request_port,
        source_scheme,
        source_host = %source_authority.host(),
        source_port,
        source = %source,
        "Admin same-origin validation rejected request"
    );
    Err(AppError::Forbidden(
        "Origin/Referer origin mismatch.".into(),
    ))
}

fn effective_same_origin_source<'a>(headers: &'a HeaderMap, request_host: &str) -> Option<&'a str> {
    let origin = header_value_trimmed(headers, header::ORIGIN);
    let referer = header_value_trimmed(headers, header::REFERER);

    match origin {
        Some(origin) if !origin.eq_ignore_ascii_case("null") => Some(origin),
        Some(origin) if is_loopback_alias(request_host) => Some(origin),
        Some(_) | None => referer,
    }
}

fn header_value_trimmed(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn request_has_same_origin_fetch_metadata(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
}

pub(super) fn check_admin_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    if admin_csrf_is_valid(jar, form_token) {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

pub(super) fn admin_csrf_is_valid(jar: &CookieJar, form_token: Option<&str>) -> bool {
    let csrf_cookie = jar
        .get("csrf_token")
        .map(axum_extra::extract::cookie::Cookie::value);
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(axum_extra::extract::cookie::Cookie::value);
    validate_signed_csrf(csrf_cookie, session_id, form_token.unwrap_or(""))
}

pub(in crate::handlers) fn require_same_origin_or_valid_csrf(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    csrf_valid: bool,
) -> Result<()> {
    match require_same_origin_request(headers, peer) {
        Ok(()) => Ok(()),
        Err(AppError::Forbidden(message)) if message == MISSING_ORIGIN_REFERER && csrf_valid => {
            tracing::debug!(
                target: "admin",
                "Admin POST accepted without Origin/Referer because signed CSRF token was valid"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub(in crate::handlers) fn require_admin_post_origin_and_csrf(
    jar: &CookieJar,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    form_token: Option<&str>,
) -> Result<()> {
    let csrf_valid = admin_csrf_is_valid(jar, form_token);
    require_same_origin_or_valid_csrf(headers, peer, csrf_valid)?;
    if csrf_valid {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

fn admin_csrf_cookie(raw_token: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new("csrf_token", raw_token);
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(secure);
    cookie
}

pub(super) fn refresh_admin_csrf_cookie(jar: CookieJar, secure: bool) -> CookieJar {
    let cookie = admin_csrf_cookie(new_csrf_token(), secure);
    jar.add(cookie)
}

pub(super) fn ensure_admin_csrf(jar: CookieJar, secure: bool) -> Result<(CookieJar, String)> {
    let raw = jar
        .get("csrf_token")
        .map(axum_extra::extract::cookie::Cookie::value)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut jar = jar;
    let raw = if let Some(raw) = raw {
        raw
    } else {
        let raw = new_csrf_token();
        jar = jar.add(admin_csrf_cookie(raw.clone(), secure));
        raw
    };
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(axum_extra::extract::cookie::Cookie::value)
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session_id = session_id.to_owned();
    Ok((
        jar,
        make_scoped_csrf_form_token(&raw, &CONFIG.cookie_secret, &session_id),
    ))
}

pub(super) use crate::utils::redirect::encode_query_component;

pub(in crate::handlers) fn should_set_secure_cookie(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
) -> bool {
    should_set_secure_cookie_with_config(
        headers,
        context,
        CONFIG.https_cookies,
        CONFIG.behind_proxy,
    )
}

fn should_set_secure_cookie_with_config(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
    https_cookies: bool,
    behind_proxy: bool,
) -> bool {
    https_cookies
        && (context.direct_https
            || crate::middleware::forwarded_proto_is_https(headers, context.peer, behind_proxy))
}

fn request_scheme_for_same_origin(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    request_host: &str,
) -> &'static str {
    request_scheme_for_same_origin_with_config(
        headers,
        peer,
        request_host,
        CONFIG.behind_proxy,
        CONFIG.tls.enabled,
        CONFIG.tls.port,
    )
}

fn request_scheme_for_same_origin_with_config(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
    request_host: &str,
    behind_proxy: bool,
    tls_enabled: bool,
    tls_port: u16,
) -> &'static str {
    if crate::middleware::forwarded_proto_is_https(headers, peer, behind_proxy)
        || request_origin_uses_https(headers)
        || (tls_enabled
            && !is_onion_host(request_host)
            && host_header_uses_https_port_with_config(headers, tls_port))
    {
        "https"
    } else {
        "http"
    }
}

fn host_header_uses_https_port_with_config(headers: &HeaderMap, tls_port: u16) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };

    match authority.port_u16() {
        Some(port) => port == tls_port,
        None => tls_port == 443,
    }
}

fn request_origin_uses_https(headers: &HeaderMap) -> bool {
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_owned());

    let Some(request_host) = request_host.as_deref() else {
        return false;
    };
    let Some(source) = effective_same_origin_source(headers, request_host) else {
        return false;
    };

    let Ok(source_uri) = source.parse::<Uri>() else {
        return false;
    };

    if source_uri.scheme_str() != Some("https") {
        return false;
    }

    let Some(source_host) = source_uri.authority().map(axum::http::uri::Authority::host) else {
        return false;
    };

    hosts_match_for_same_origin(source_host, request_host)
}

fn hosts_match_for_same_origin(source_host: &str, request_host: &str) -> bool {
    let source_host = normalize_same_origin_host(source_host);
    let request_host = normalize_same_origin_host(request_host);

    if source_host.eq_ignore_ascii_case(request_host) {
        return true;
    }

    is_loopback_alias(source_host) && is_loopback_alias(request_host)
}

fn normalize_same_origin_host(host: &str) -> &str {
    let Some(inner) = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return host;
    };

    if inner.parse::<std::net::Ipv6Addr>().is_ok() {
        inner
    } else {
        host
    }
}

fn is_onion_host(host: &str) -> bool {
    let host = normalize_same_origin_host(host);
    let Some((label, suffix)) = host.rsplit_once('.') else {
        return false;
    };

    !label.is_empty() && suffix.eq_ignore_ascii_case("onion")
}

fn is_loopback_alias(host: &str) -> bool {
    let host = normalize_same_origin_host(host);

    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn admin_panel_redirect_with_status(
    message: &str,
    is_error: bool,
    target: AdminPanelTarget<'_>,
) -> Redirect {
    let key = if is_error { "flash_error" } else { "flash" };
    let mut url = format!("/admin/panel?{key}={}", encode_query_component(message));
    if let Some(section) = target.open_section_value() {
        url.push_str("&open=");
        url.push_str(&encode_query_component(section));
    }
    if let Some(anchor) = target.anchor_value() {
        url.push('#');
        url.push_str(anchor);
    }
    Redirect::to(&url)
}

#[derive(Clone, Debug, Default)]
pub(super) struct AdminPanelTarget<'a> {
    anchor: Option<Cow<'a, str>>,
    open_section: Option<Cow<'a, str>>,
}

impl<'a> AdminPanelTarget<'a> {
    pub(super) const fn none() -> Self {
        Self {
            anchor: None,
            open_section: None,
        }
    }

    pub(super) const fn anchor(anchor: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Borrowed(anchor)),
            open_section: None,
        }
    }

    pub(super) const fn anchor_open(anchor: &'a str, open_section: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Borrowed(anchor)),
            open_section: Some(Cow::Borrowed(open_section)),
        }
    }

    pub(super) const fn owned_anchor_open(anchor: String, open_section: &'a str) -> Self {
        Self {
            anchor: Some(Cow::Owned(anchor)),
            open_section: Some(Cow::Borrowed(open_section)),
        }
    }

    pub(super) fn anchor_value(&self) -> Option<&str> {
        self.anchor.as_deref().filter(|value| !value.is_empty())
    }

    pub(super) fn open_section_value(&self) -> Option<&str> {
        self.open_section
            .as_deref()
            .filter(|value| !value.is_empty())
    }
}

pub(super) fn admin_panel_redirect(message: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, AdminPanelTarget::none())
}

pub(super) fn admin_panel_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, false, AdminPanelTarget::anchor(anchor))
}

pub(super) fn admin_panel_redirect_anchor_open(
    message: &str,
    anchor: &str,
    open_section: &str,
) -> Redirect {
    admin_panel_redirect_with_status(
        message,
        false,
        AdminPanelTarget::anchor_open(anchor, open_section),
    )
}

pub(super) fn admin_panel_error_redirect_anchor(message: &str, anchor: &str) -> Redirect {
    admin_panel_redirect_with_status(message, true, AdminPanelTarget::anchor(anchor))
}

pub(super) fn admin_panel_error_redirect_anchor_open(
    message: &str,
    anchor: &str,
    open_section: &str,
) -> Redirect {
    admin_panel_redirect_with_status(
        message,
        true,
        AdminPanelTarget::anchor_open(anchor, open_section),
    )
}

// ─── GET /admin/panel ─────────────────────────────────────────────────────────

/// Query params accepted by GET /admin/panel.
/// All fields are optional — missing = no flash message.
#[derive(Deserialize, Default)]
pub struct AdminPanelQuery {
    pub flash: Option<String>,
    pub flash_error: Option<String>,
    pub open: Option<String>,
    pub bootstrap: Option<String>,
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

#[expect(clippy::struct_excessive_bools)]
struct AdminPanelSnapshot {
    boards: Vec<crate::models::Board>,
    bans: Vec<crate::models::Ban>,
    filters: Vec<crate::models::WordFilter>,
    reports: Vec<crate::models::ReportWithContext>,
    appeals: Vec<crate::models::BanAppeal>,
    site_name: String,
    site_subtitle: String,
    homepage_new_thread_badges_enabled: bool,
    homepage_new_reply_badges_enabled: bool,
    thread_new_reply_badges_enabled: bool,
    default_theme: String,
    banner_rotation_interval_minutes: i64,
    banner_external_links_enabled: bool,
    auto_full_backup_interval_hours: u64,
    auto_full_backup_copies_to_keep: u64,
    auto_full_backup_include_tor_hidden_service_keys: bool,
    auto_full_backup_storage_mode: String,
    auto_full_backup_split_zip_part_size_bytes: u64,
    themes: Vec<crate::models::Theme>,
    global_banners: Vec<crate::models::BannerAsset>,
    home_banners: Vec<crate::models::BannerAsset>,
    board_banners: Vec<crate::models::BannerAsset>,
    full_backups: Vec<crate::models::BackupInfo>,
    board_backups: Vec<crate::models::BackupInfo>,
    db_size_bytes: i64,
    db_size_warning: bool,
    setup_status: crate::templates::AdminPanelSetupStatus,
    ffmpeg_timeout_secs: u64,
    media_auto_prune_enabled: bool,
    media_max_active_content_size_bytes: u64,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
    ffmpeg_vp9_available: bool,
    ffmpeg_vp9_encoder_available: bool,
    ffmpeg_opus_available: bool,
    pdf_thumbnail_renderer: Option<String>,
    backup_summary: BackupSummary,
    site_health: SiteHealthSnapshot,
    dashboard: AdminDashboardSummary,
}

#[derive(Clone)]
struct BackupSummary {
    warning: Option<String>,
    status_line: String,
}

struct OverviewDomainData {
    backup_summary: BackupSummary,
}

#[derive(Clone)]
struct AdminDashboardSummary {
    version: String,
    build: String,
    setup_status: String,
    setup_detail: String,
    setup_state: crate::templates::AdminDashboardState,
    site_title: String,
    public_url: String,
    db_status: String,
    db_detail: String,
    db_state: crate::templates::AdminDashboardState,
    backup_status: String,
    backup_detail: String,
    backup_state: crate::templates::AdminDashboardState,
    storage_status: String,
    storage_detail: String,
    storage_state: crate::templates::AdminDashboardState,
    tor_status: String,
    tor_detail: String,
    tor_state: crate::templates::AdminDashboardState,
    dependency_status: String,
    dependency_detail: String,
    dependency_state: crate::templates::AdminDashboardState,
    job_status: String,
    job_detail: String,
    job_state: crate::templates::AdminDashboardState,
    board_count: String,
    thread_count: String,
    post_count: String,
    recent_activity: String,
    media_summary: String,
    report_status: String,
    report_detail: String,
    report_state: crate::templates::AdminDashboardState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DashboardActivitySnapshot {
    board_count: usize,
    active_threads: Option<i64>,
    total_threads: Option<i64>,
    total_posts: Option<i64>,
    posts_24h: Option<i64>,
    posts_7d: Option<i64>,
    upload_posts: Option<i64>,
    total_images: Option<i64>,
    total_videos: Option<i64>,
    total_audio: Option<i64>,
    active_bytes: Option<i64>,
    recent_reports_7d: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DashboardThreadCounts {
    active: i64,
    total: i64,
}

struct DashboardSummaryInputs<'a> {
    activity: &'a DashboardActivitySnapshot,
    moderation: &'a ModerationDomainData,
    appearance: &'a AppearanceDomainData,
    backup_summary: &'a BackupSummary,
    maintenance: &'a MaintenanceDomainData,
    setup_status: crate::templates::AdminPanelSetupStatus,
    site_health: &'a SiteHealthSnapshot,
    tor_address: Option<&'a str>,
}

struct SiteHealthSnapshot {
    server_status: String,
    database_integrity_status: String,
    last_successful_backup: String,
    next_scheduled_backup: String,
    data_dir_usage: String,
    upload_dir_size: String,
    tor_status: String,
    running_jobs: i64,
    queued_jobs: i64,
    recent_completed_jobs: i64,
    failed_jobs: i64,
    backup_jobs: String,
    restore_jobs: String,
    recent_warnings: String,
}

#[derive(Serialize)]
struct SiteHealthJobsSnapshot {
    #[serde(rename = "running_jobs")]
    running: i64,
    #[serde(rename = "queued_jobs")]
    queued: i64,
    #[serde(rename = "recent_completed_jobs")]
    recent_completed: i64,
    #[serde(rename = "failed_jobs")]
    failed: i64,
    #[serde(rename = "backup_jobs")]
    backup: String,
    #[serde(rename = "restore_jobs")]
    restore: String,
    #[serde(rename = "recent_failed_job_details")]
    recent_failed: Vec<SiteHealthJobDetail>,
    #[serde(rename = "recent_completed_job_details")]
    recent_completed_details: Vec<SiteHealthJobDetail>,
}

#[derive(Clone, Serialize)]
struct SiteHealthJobDetail {
    id: i64,
    #[serde(rename = "type")]
    job_type: String,
    name: String,
    post_id: Option<i64>,
    post_url: Option<String>,
    status: String,
    attempts: i64,
    error: Option<String>,
    updated_at: String,
}

struct BoardsDomainData {
    boards: Vec<crate::models::Board>,
}

struct ModerationDomainData {
    bans: Vec<crate::models::Ban>,
    filters: Vec<crate::models::WordFilter>,
    reports: Vec<crate::models::ReportWithContext>,
    appeals: Vec<crate::models::BanAppeal>,
}

#[expect(clippy::struct_excessive_bools)]
struct AppearanceDomainData {
    site_name: String,
    site_subtitle: String,
    homepage_new_thread_badges_enabled: bool,
    homepage_new_reply_badges_enabled: bool,
    thread_new_reply_badges_enabled: bool,
    default_theme: String,
    banner_rotation_interval_minutes: i64,
    banner_external_links_enabled: bool,
    themes: Vec<crate::models::Theme>,
    global_banners: Vec<crate::models::BannerAsset>,
    home_banners: Vec<crate::models::BannerAsset>,
    board_banners: Vec<crate::models::BannerAsset>,
}

struct BackupsDomainData {
    full_backups: Vec<BackupInfo>,
    board_backups: Vec<BackupInfo>,
}

#[expect(clippy::struct_excessive_bools)]
// This is a flat snapshot of independent maintenance capability flags read from app state.
struct MaintenanceDomainData {
    db_size_bytes: i64,
    db_size_warning: bool,
    ffmpeg_timeout_secs: u64,
    media_auto_prune_enabled: bool,
    media_max_active_content_size_bytes: u64,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
    ffmpeg_vp9_available: bool,
    ffmpeg_vp9_encoder_available: bool,
    ffmpeg_opus_available: bool,
    pdf_thumbnail_renderer: Option<String>,
}

fn load_overview_domain_data(full_backups: &[BackupInfo]) -> OverviewDomainData {
    OverviewDomainData {
        backup_summary: build_backup_summary(full_backups),
    }
}

fn load_site_health_snapshot(
    conn: &rusqlite::Connection,
    state: &AppState,
    full_backups: &[BackupInfo],
    auto_full_backup_settings: &crate::middleware::AutoFullBackupSettingsSnapshot,
    _onion_address_val: Option<&str>,
) -> SiteHealthSnapshot {
    let server_status = conn
        .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
        .ok()
        .filter(|value| *value == 1)
        .map_or_else(|| "degraded".to_owned(), |_| "ready".to_owned());
    let database_integrity_status = db_integrity_status(&state.db_maintenance_jobs.snapshot());
    let last_successful_backup = full_backups
        .iter()
        .find(|backup| backup.verified)
        .map_or_else(|| "none saved".to_owned(), format_backup_time);
    let next_scheduled_backup =
        next_scheduled_backup_label(full_backups, auto_full_backup_settings.interval_hours);
    let data_dir_usage = safe_dir_size_label(&crate::config::data_dir());
    let upload_dir_size = safe_dir_size_label(Path::new(&CONFIG.upload_dir));
    let jobs = load_site_health_jobs_snapshot(conn, state);
    let recent_warnings = recent_warning_lines().unwrap_or_else(|| "not available".to_owned());

    SiteHealthSnapshot {
        server_status,
        database_integrity_status,
        last_successful_backup,
        next_scheduled_backup,
        data_dir_usage,
        upload_dir_size,
        tor_status: if CONFIG.enable_tor_support {
            "enabled".to_owned()
        } else {
            "disabled".to_owned()
        },
        running_jobs: jobs.running,
        queued_jobs: jobs.queued,
        recent_completed_jobs: jobs.recent_completed,
        failed_jobs: jobs.failed,
        backup_jobs: jobs.backup,
        restore_jobs: jobs.restore,
        recent_warnings,
    }
}

fn load_dashboard_activity_snapshot(
    conn: &rusqlite::Connection,
    board_count: usize,
) -> DashboardActivitySnapshot {
    let site_stats = db::get_site_stats(conn).ok();
    let thread_counts = dashboard_thread_counts(conn);

    DashboardActivitySnapshot {
        board_count,
        active_threads: thread_counts.map(|counts| counts.active),
        total_threads: thread_counts.map(|counts| counts.total),
        total_posts: site_stats.as_ref().map(|stats| stats.total_posts),
        posts_24h: dashboard_recent_count(conn, "posts", 24 * 60 * 60),
        posts_7d: dashboard_recent_count(conn, "posts", 7 * 24 * 60 * 60),
        upload_posts: optional_count_query(
            conn,
            "SELECT COUNT(*) FROM posts
             WHERE file_path IS NOT NULL OR audio_file_path IS NOT NULL",
        ),
        total_images: site_stats.as_ref().map(|stats| stats.total_images),
        total_videos: site_stats.as_ref().map(|stats| stats.total_videos),
        total_audio: site_stats.as_ref().map(|stats| stats.total_audio),
        active_bytes: site_stats.map(|stats| stats.active_bytes),
        recent_reports_7d: dashboard_recent_count(conn, "reports", 7 * 24 * 60 * 60),
    }
}

fn dashboard_thread_counts(conn: &rusqlite::Connection) -> Option<DashboardThreadCounts> {
    conn.query_row(
        "SELECT
             COALESCE(SUM(CASE WHEN archived = 0 THEN 1 ELSE 0 END), 0),
             COUNT(*)
         FROM threads",
        [],
        |row| {
            Ok(DashboardThreadCounts {
                active: row.get(0)?,
                total: row.get(1)?,
            })
        },
    )
    .ok()
}

fn dashboard_recent_count(
    conn: &rusqlite::Connection,
    table_name: &str,
    window_secs: i64,
) -> Option<i64> {
    if !matches!(table_name, "posts" | "reports") {
        return None;
    }
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table_name} WHERE created_at >= unixepoch() - ?1"),
        rusqlite::params![window_secs.max(0)],
        |row| row.get(0),
    )
    .ok()
}

fn optional_count_query(conn: &rusqlite::Connection, query: &str) -> Option<i64> {
    conn.query_row(query, [], |row| row.get(0)).ok()
}

fn load_site_health_jobs_snapshot(
    conn: &rusqlite::Connection,
    state: &AppState,
) -> SiteHealthJobsSnapshot {
    let job_summary =
        db::background_job_summary(conn).unwrap_or_else(|_| db::BackgroundJobSummary {
            running: 0,
            queued: state.job_queue.pending_count(),
            recent_completed: 0,
            failed: 0,
        });
    let recent_failed = load_site_health_job_details(conn, "failed");
    let recent_completed_details = load_site_health_job_details(conn, "done");
    SiteHealthJobsSnapshot {
        running: job_summary.running,
        queued: job_summary.queued,
        recent_completed: job_summary.recent_completed,
        failed: job_summary.failed,
        backup: backup_jobs_label(state.backup_progress.as_ref()),
        restore: "not available".to_owned(),
        recent_failed,
        recent_completed_details,
    }
}

fn load_site_health_job_details(
    conn: &rusqlite::Connection,
    status: &str,
) -> Vec<SiteHealthJobDetail> {
    // background_jobs has no stable log-entry foreign key, so Site Health shows
    // bounded inline job details instead of guessing at admin log links.
    db::recent_background_jobs(conn, status, 10)
        .unwrap_or_default()
        .into_iter()
        .map(|job| site_health_job_detail(conn, job))
        .collect()
}

fn site_health_job_detail(
    conn: &rusqlite::Connection,
    job: db::RecentBackgroundJob,
) -> SiteHealthJobDetail {
    let post_id = job_post_id(&job.payload);
    let post_url = post_id.and_then(|id| post_url_for_job(conn, id));
    SiteHealthJobDetail {
        id: job.id,
        name: background_job_display_name(&job.job_type).to_owned(),
        job_type: job.job_type,
        post_id,
        post_url,
        status: job.status,
        attempts: job.attempts,
        error: job
            .last_error
            .as_deref()
            .and_then(sanitized_job_error_snippet),
        updated_at: fmt_epoch(job.updated_at),
    }
}

fn job_post_id(payload: &str) -> Option<i64> {
    let value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    value.get("d")?.get("post_id")?.as_i64()
}

fn post_url_for_job(conn: &rusqlite::Connection, post_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT b.short_name, p.thread_id
         FROM posts p
         JOIN boards b ON b.id = p.board_id
         WHERE p.id = ?1
         LIMIT 1",
        rusqlite::params![post_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )
    .ok()
    .map(|(board_short, thread_id)| format!("/{board_short}/thread/{thread_id}#p{post_id}"))
}

fn background_job_display_name(job_type: &str) -> &str {
    match job_type {
        "video_transcode" => "Video transcode",
        "audio_waveform" => "Audio waveform",
        "thread_prune" => "Thread prune",
        "spam_check" => "Spam check",
        _ => "Background job",
    }
}

fn sanitized_job_error_snippet(error: &str) -> Option<String> {
    let mut redacted = String::new();
    for token in error.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let safe_token = if token.starts_with('/')
            || token.starts_with("~/")
            || lower.contains("/users/")
            || lower.contains("token=")
            || lower.contains("secret=")
            || lower.contains("password=")
            || lower.contains("cookie=")
            || lower.contains("authorization:")
        {
            "[redacted]"
        } else {
            token
        };
        if !redacted.is_empty() {
            redacted.push(' ');
        }
        redacted.push_str(safe_token);
        if redacted.chars().count() >= 180 {
            break;
        }
    }

    let snippet: String = redacted.chars().take(180).collect();
    let snippet = snippet.trim();
    if snippet.is_empty() {
        None
    } else if redacted.chars().count() > 180 || error.chars().count() > snippet.chars().count() {
        Some(format!("{snippet}..."))
    } else {
        Some(snippet.to_owned())
    }
}

fn format_backup_time(backup: &BackupInfo) -> String {
    backup
        .modified_epoch
        .map_or_else(|| backup.filename.clone(), fmt_epoch)
}

fn next_scheduled_backup_label(full_backups: &[BackupInfo], interval_hours: u64) -> String {
    if interval_hours == 0 {
        return "not scheduled".to_owned();
    }
    let Some(latest_verified) = full_backups.iter().find(|backup| backup.verified) else {
        return "after first scheduler check".to_owned();
    };
    let Some(modified_epoch) = latest_verified.modified_epoch else {
        return "unknown".to_owned();
    };
    let interval_secs = i64::try_from(interval_hours.saturating_mul(3600)).unwrap_or(i64::MAX);
    fmt_epoch(modified_epoch.saturating_add(interval_secs))
}

fn fmt_epoch(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0).map_or_else(
        || "unknown".to_owned(),
        |datetime| datetime.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

fn db_integrity_status(status: &crate::middleware::DbMaintenanceJobStatus) -> String {
    match status {
        crate::middleware::DbMaintenanceJobStatus::Finished { report, .. } => {
            if report.after.as_ref().unwrap_or(&report.before).ok() {
                "passed at last check".to_owned()
            } else {
                "failed at last check".to_owned()
            }
        }
        crate::middleware::DbMaintenanceJobStatus::Running { .. } => "check running".to_owned(),
        crate::middleware::DbMaintenanceJobStatus::Failed { .. } => "last check failed".to_owned(),
        crate::middleware::DbMaintenanceJobStatus::Idle => "not checked".to_owned(),
    }
}

fn backup_jobs_label(progress: &crate::middleware::BackupProgress) -> String {
    use std::sync::atomic::Ordering;
    match progress.phase.load(Ordering::Relaxed) {
        crate::middleware::backup_phase::IDLE => "idle".to_owned(),
        crate::middleware::backup_phase::SNAPSHOT_DB => "snapshotting database".to_owned(),
        crate::middleware::backup_phase::COUNT_FILES => "counting files".to_owned(),
        crate::middleware::backup_phase::COMPRESS => {
            let done = progress.files_done.load(Ordering::Relaxed);
            let total = progress.files_total.load(Ordering::Relaxed);
            if total == 0 {
                "compressing".to_owned()
            } else {
                format!("compressing ({done}/{total} files)")
            }
        }
        crate::middleware::backup_phase::DONE => "last run complete".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn safe_dir_size_label(path: &Path) -> String {
    safe_dir_size(path).map_or_else(
        || "unknown".to_owned(),
        |bytes| {
            let display_bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
            crate::utils::files::format_file_size(display_bytes)
        },
    )
}

fn safe_dir_size(root: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(root).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    if metadata.is_file() {
        return Some(metadata.len());
    }
    if !metadata.is_dir() {
        return Some(0);
    }

    let mut total = 0_u64;
    let mut pending = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = pending.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&entry_path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push_back(entry_path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Some(total)
}

fn recent_warning_lines() -> Option<String> {
    let log_path = latest_log_file(&crate::config::logs_dir())?;
    let buf = std::fs::read(log_path).ok()?;
    let start = buf.len().saturating_sub(65_536);
    let buf = String::from_utf8_lossy(buf.get(start..).unwrap_or_default()).into_owned();
    let warnings: Vec<&str> = buf
        .lines()
        .rev()
        .filter(|line| {
            line.contains("WARN")
                || line.contains("ERROR")
                || line.contains("warn")
                || line.contains("error")
        })
        .take(5)
        .collect();
    if warnings.is_empty() {
        Some("none in recent log tail".to_owned())
    } else {
        Some(warnings.into_iter().rev().collect::<Vec<_>>().join("\n"))
    }
}

fn load_boards_domain_data(conn: &rusqlite::Connection) -> Result<BoardsDomainData> {
    Ok(BoardsDomainData {
        boards: db::get_all_boards(conn)?,
    })
}

fn load_moderation_domain_data(conn: &rusqlite::Connection) -> Result<ModerationDomainData> {
    Ok(ModerationDomainData {
        bans: db::list_bans(conn)?,
        filters: db::get_word_filters(conn)?,
        reports: db::get_open_reports(conn)?,
        appeals: db::get_open_ban_appeals(conn)?,
    })
}

fn load_appearance_domain_data(
    conn: &rusqlite::Connection,
    boards: &[crate::models::Board],
) -> Result<AppearanceDomainData> {
    let themes = db::load_themes(conn)?;
    let global_banners =
        db::list_banner_assets_for_scope(conn, crate::models::BannerScope::Global)?;
    let home_banners = db::list_banner_assets_for_scope(conn, crate::models::BannerScope::Home)?;
    let mut board_banners = Vec::new();
    for board in boards {
        board_banners.extend(db::list_banner_assets_for_board(conn, board.id)?);
    }

    Ok(AppearanceDomainData {
        site_name: db::get_site_name(conn),
        site_subtitle: db::get_site_subtitle(conn),
        homepage_new_thread_badges_enabled: db::get_homepage_new_thread_badges_enabled(conn),
        homepage_new_reply_badges_enabled: db::get_homepage_new_reply_badges_enabled(conn),
        thread_new_reply_badges_enabled: db::get_thread_new_reply_badges_enabled(conn),
        default_theme: db::get_default_user_theme(conn),
        banner_rotation_interval_minutes: db::get_banner_rotation_interval_minutes(conn),
        banner_external_links_enabled: db::get_banner_external_links_enabled(conn),
        themes,
        global_banners,
        home_banners,
        board_banners,
    })
}

fn load_backups_domain_data() -> BackupsDomainData {
    BackupsDomainData {
        full_backups: list_backup_files(&full_backup_dir(), BackupListKind::Full),
        board_backups: list_backup_files(&board_backup_dir(), BackupListKind::Board),
    }
}

fn load_maintenance_domain_data(
    conn: &rusqlite::Connection,
    state: &AppState,
) -> MaintenanceDomainData {
    let db_size_bytes = db::get_db_size_bytes(conn).unwrap_or(0);
    let db_size_warning = if CONFIG.db_warn_threshold_bytes > 0 {
        let file_size = std::fs::metadata(&CONFIG.database_path)
            .map_or_else(|_| db_size_bytes.cast_unsigned(), |m| m.len());
        file_size >= CONFIG.db_warn_threshold_bytes
    } else {
        false
    };

    MaintenanceDomainData {
        db_size_bytes,
        db_size_warning,
        ffmpeg_timeout_secs: crate::config::ffmpeg_timeout_secs(),
        media_auto_prune_enabled: db::get_media_auto_prune_enabled(conn),
        media_max_active_content_size_bytes: db::get_media_max_active_content_size_bytes(conn),
        ffmpeg_available: state.ffmpeg_available,
        ffprobe_available: state.ffprobe_available,
        ffmpeg_webp_available: state.ffmpeg_webp_available,
        ffmpeg_vp9_available: state.ffmpeg_vp9_available,
        ffmpeg_vp9_encoder_available: state.ffmpeg_vp9_encoder_available,
        ffmpeg_opus_available: state.ffmpeg_opus_available,
        pdf_thumbnail_renderer: state.pdf_thumbnail_renderer.map(str::to_owned),
    }
}

fn load_admin_panel_snapshot(
    conn: &rusqlite::Connection,
    state: &AppState,
    onion_address_val: Option<String>,
    auto_full_backup_settings: crate::middleware::AutoFullBackupSettingsSnapshot,
) -> Result<(AdminPanelSnapshot, Option<String>)> {
    let boards_domain = load_boards_domain_data(conn)?;
    let moderation_domain = load_moderation_domain_data(conn)?;
    let appearance_domain = load_appearance_domain_data(conn, &boards_domain.boards)?;
    let backups_domain = load_backups_domain_data();
    let overview_domain = load_overview_domain_data(&backups_domain.full_backups);
    let maintenance_domain = load_maintenance_domain_data(conn, state);
    let setup_state = db::setup_state(conn)?;
    let setup_status = admin_panel_setup_status(setup_state);
    let site_health = load_site_health_snapshot(
        conn,
        state,
        &backups_domain.full_backups,
        &auto_full_backup_settings,
        onion_address_val.as_deref(),
    );
    let dashboard_activity = load_dashboard_activity_snapshot(conn, boards_domain.boards.len());
    let dashboard = build_admin_dashboard_summary(DashboardSummaryInputs {
        activity: &dashboard_activity,
        moderation: &moderation_domain,
        appearance: &appearance_domain,
        backup_summary: &overview_domain.backup_summary,
        maintenance: &maintenance_domain,
        setup_status,
        site_health: &site_health,
        tor_address: onion_address_val.as_deref(),
    });
    Ok((
        AdminPanelSnapshot {
            boards: boards_domain.boards,
            bans: moderation_domain.bans,
            filters: moderation_domain.filters,
            reports: moderation_domain.reports,
            appeals: moderation_domain.appeals,
            site_name: appearance_domain.site_name,
            site_subtitle: appearance_domain.site_subtitle,
            homepage_new_thread_badges_enabled: appearance_domain
                .homepage_new_thread_badges_enabled,
            homepage_new_reply_badges_enabled: appearance_domain.homepage_new_reply_badges_enabled,
            thread_new_reply_badges_enabled: appearance_domain.thread_new_reply_badges_enabled,
            default_theme: appearance_domain.default_theme,
            banner_rotation_interval_minutes: appearance_domain.banner_rotation_interval_minutes,
            banner_external_links_enabled: appearance_domain.banner_external_links_enabled,
            auto_full_backup_interval_hours: auto_full_backup_settings.interval_hours,
            auto_full_backup_copies_to_keep: auto_full_backup_settings.copies_to_keep,
            auto_full_backup_include_tor_hidden_service_keys: auto_full_backup_settings
                .include_tor_hidden_service_keys,
            auto_full_backup_storage_mode: auto_full_backup_settings.storage_mode,
            auto_full_backup_split_zip_part_size_bytes: auto_full_backup_settings
                .split_zip_part_size,
            themes: appearance_domain.themes,
            global_banners: appearance_domain.global_banners,
            home_banners: appearance_domain.home_banners,
            board_banners: appearance_domain.board_banners,
            full_backups: backups_domain.full_backups,
            board_backups: backups_domain.board_backups,
            db_size_bytes: maintenance_domain.db_size_bytes,
            db_size_warning: maintenance_domain.db_size_warning,
            setup_status,
            ffmpeg_timeout_secs: maintenance_domain.ffmpeg_timeout_secs,
            media_auto_prune_enabled: maintenance_domain.media_auto_prune_enabled,
            media_max_active_content_size_bytes: maintenance_domain
                .media_max_active_content_size_bytes,
            ffmpeg_available: maintenance_domain.ffmpeg_available,
            ffprobe_available: maintenance_domain.ffprobe_available,
            ffmpeg_webp_available: maintenance_domain.ffmpeg_webp_available,
            ffmpeg_vp9_available: maintenance_domain.ffmpeg_vp9_available,
            ffmpeg_vp9_encoder_available: maintenance_domain.ffmpeg_vp9_encoder_available,
            ffmpeg_opus_available: maintenance_domain.ffmpeg_opus_available,
            pdf_thumbnail_renderer: maintenance_domain.pdf_thumbnail_renderer,
            backup_summary: overview_domain.backup_summary,
            site_health,
            dashboard,
        },
        onion_address_val,
    ))
}

const fn admin_panel_setup_status(
    setup_state: db::SetupState,
) -> crate::templates::AdminPanelSetupStatus {
    if setup_state.reopened {
        crate::templates::AdminPanelSetupStatus::Reopened
    } else if setup_state.completed {
        crate::templates::AdminPanelSetupStatus::Complete
    } else if setup_state.is_available() {
        crate::templates::AdminPanelSetupStatus::Available
    } else {
        crate::templates::AdminPanelSetupStatus::Initialized
    }
}

fn build_backup_summary(full_backups: &[BackupInfo]) -> BackupSummary {
    const BACKUP_WARN_AFTER_HOURS: i64 = 72;

    let Some(latest) = full_backups.first() else {
        return BackupSummary {
            warning: Some(
                "No saved full backup found. Create and download a verified full backup before relying on this node.".to_owned(),
            ),
            status_line: "Latest full backup: none saved.".to_owned(),
        };
    };

    let now = chrono::Utc::now().timestamp();
    let age_hours = latest
        .modified_epoch
        .map(|ts| now.saturating_sub(ts).max(0) / 3600);
    let age_text = age_hours
        .map(|hours| format!("{hours}h ago"))
        .unwrap_or_else(|| "unknown age".to_owned());
    let status_line = format!(
        "Latest full backup: {} ({age_text}) — {}.",
        latest.filename, latest.verification_note
    );

    let warning = if !latest.verified {
        Some(format!(
            "Latest full backup '{}' failed verification: {}",
            latest.filename, latest.verification_note
        ))
    } else if age_hours.is_some_and(|hours| hours >= BACKUP_WARN_AFTER_HOURS) {
        Some(format!(
            "Latest verified full backup '{}' is older than {BACKUP_WARN_AFTER_HOURS} hours ({age_text}).",
            latest.filename
        ))
    } else {
        None
    };

    BackupSummary {
        warning,
        status_line,
    }
}

fn build_admin_dashboard_summary(inputs: DashboardSummaryInputs<'_>) -> AdminDashboardSummary {
    let (setup_status, setup_detail, setup_state) = dashboard_setup_status(inputs.setup_status);
    let (db_status, db_detail, db_state) = dashboard_database_status(inputs.site_health);
    let (backup_status, backup_detail, backup_state) =
        dashboard_backup_status(inputs.backup_summary);
    let (storage_status, storage_detail, storage_state) =
        dashboard_storage_status(inputs.activity, inputs.maintenance, inputs.site_health);
    let (tor_status, tor_detail, tor_state) = dashboard_tor_status(inputs.tor_address);
    let (dependency_status, dependency_detail, dependency_state) =
        dashboard_dependency_status(inputs.maintenance);
    let (job_status, job_detail, job_state) = dashboard_job_status(inputs.site_health);
    let (report_status, report_detail, report_state) =
        dashboard_report_status(inputs.activity, inputs.moderation);

    AdminDashboardSummary {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        build: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        setup_status,
        setup_detail,
        setup_state,
        site_title: inputs.appearance.site_name.clone(),
        public_url: public_url_label(),
        db_status,
        db_detail,
        db_state,
        backup_status,
        backup_detail,
        backup_state,
        storage_status,
        storage_detail,
        storage_state,
        tor_status,
        tor_detail,
        tor_state,
        dependency_status,
        dependency_detail,
        dependency_state,
        job_status,
        job_detail,
        job_state,
        board_count: count_label(inputs.activity.board_count, "board", "boards"),
        thread_count: thread_count_label(inputs.activity),
        post_count: optional_count_label(inputs.activity.total_posts, "post", "posts"),
        recent_activity: recent_activity_label(inputs.activity),
        media_summary: media_summary_label(inputs.activity),
        report_status,
        report_detail,
        report_state,
    }
}

fn dashboard_setup_status(
    status: crate::templates::AdminPanelSetupStatus,
) -> (String, String, crate::templates::AdminDashboardState) {
    match status {
        crate::templates::AdminPanelSetupStatus::Reopened => (
            "reopened".to_owned(),
            "Setup wizard is admin-only and currently reopened.".to_owned(),
            crate::templates::AdminDashboardState::Warning,
        ),
        crate::templates::AdminPanelSetupStatus::Complete => (
            "complete".to_owned(),
            "Public setup routes are blocked.".to_owned(),
            crate::templates::AdminDashboardState::Ok,
        ),
        crate::templates::AdminPanelSetupStatus::Available => (
            "available".to_owned(),
            "First-run setup still needs to be completed.".to_owned(),
            crate::templates::AdminDashboardState::ActionNeeded,
        ),
        crate::templates::AdminPanelSetupStatus::Initialized => (
            "initialized".to_owned(),
            "Durable runtime state exists and public setup is blocked.".to_owned(),
            crate::templates::AdminDashboardState::Ok,
        ),
    }
}

fn dashboard_database_status(
    site_health: &SiteHealthSnapshot,
) -> (String, String, crate::templates::AdminDashboardState) {
    let state = if site_health.server_status != "ready"
        || site_health.database_integrity_status.contains("failed")
    {
        crate::templates::AdminDashboardState::ActionNeeded
    } else if site_health.database_integrity_status.contains("running") {
        crate::templates::AdminDashboardState::Warning
    } else if site_health.database_integrity_status == "not checked" {
        crate::templates::AdminDashboardState::Unknown
    } else {
        crate::templates::AdminDashboardState::Ok
    };
    (
        site_health.server_status.clone(),
        format!("Integrity: {}.", site_health.database_integrity_status),
        state,
    )
}

fn dashboard_backup_status(
    backup_summary: &BackupSummary,
) -> (String, String, crate::templates::AdminDashboardState) {
    let Some(warning) = backup_summary.warning.as_deref() else {
        return (
            "current".to_owned(),
            backup_summary.status_line.clone(),
            crate::templates::AdminDashboardState::Ok,
        );
    };

    let (status, state) = if warning.starts_with("No saved full backup") {
        (
            "no full backup",
            crate::templates::AdminDashboardState::ActionNeeded,
        )
    } else if warning.contains("failed verification") {
        (
            "verification failed",
            crate::templates::AdminDashboardState::ActionNeeded,
        )
    } else {
        (
            "stale backup",
            crate::templates::AdminDashboardState::Warning,
        )
    };
    (status.to_owned(), warning.to_owned(), state)
}

fn dashboard_storage_status(
    activity: &DashboardActivitySnapshot,
    maintenance: &MaintenanceDomainData,
    site_health: &SiteHealthSnapshot,
) -> (String, String, crate::templates::AdminDashboardState) {
    let active_media = activity.active_bytes.map_or_else(
        || "unknown".to_owned(),
        crate::utils::files::format_file_size,
    );
    let storage_known =
        site_health.data_dir_usage != "unknown" && site_health.upload_dir_size != "unknown";
    let over_prune_limit = activity.active_bytes.is_some_and(|bytes| {
        maintenance.media_max_active_content_size_bytes > 0
            && u64::try_from(bytes)
                .is_ok_and(|bytes| bytes >= maintenance.media_max_active_content_size_bytes)
    });
    let state = if over_prune_limit {
        crate::templates::AdminDashboardState::Warning
    } else if storage_known {
        crate::templates::AdminDashboardState::Ok
    } else {
        crate::templates::AdminDashboardState::Unknown
    };
    let status = if over_prune_limit {
        "above prune threshold".to_owned()
    } else {
        format!("uploads {}", site_health.upload_dir_size)
    };
    (
        status,
        format!(
            "Data directory {}; active media {}.",
            site_health.data_dir_usage, active_media
        ),
        state,
    )
}

fn dashboard_tor_status(
    tor_address: Option<&str>,
) -> (String, String, crate::templates::AdminDashboardState) {
    if !CONFIG.enable_tor_support {
        return (
            "disabled".to_owned(),
            "Tor support is disabled in configuration.".to_owned(),
            crate::templates::AdminDashboardState::Disabled,
        );
    }
    if tor_address.is_some() {
        (
            "onion ready".to_owned(),
            "Tor support is enabled and an onion address is available.".to_owned(),
            crate::templates::AdminDashboardState::Ok,
        )
    } else {
        (
            "enabled, address pending".to_owned(),
            "Tor support is enabled but no onion address is currently available.".to_owned(),
            crate::templates::AdminDashboardState::Warning,
        )
    }
}

fn dashboard_dependency_status(
    maintenance: &MaintenanceDomainData,
) -> (String, String, crate::templates::AdminDashboardState) {
    let ffmpeg = detection_word(maintenance.ffmpeg_available);
    let ffprobe = detection_word(maintenance.ffprobe_available);
    let state = if maintenance.ffmpeg_available && maintenance.ffprobe_available {
        crate::templates::AdminDashboardState::Ok
    } else if CONFIG.require_ffmpeg {
        crate::templates::AdminDashboardState::ActionNeeded
    } else {
        crate::templates::AdminDashboardState::Warning
    };
    let status = if maintenance.ffmpeg_available && maintenance.ffprobe_available {
        "ready"
    } else if CONFIG.require_ffmpeg {
        "required tool missing"
    } else {
        "limited"
    };
    (
        status.to_owned(),
        format!(
            "ffmpeg {ffmpeg}; ffprobe {ffprobe}; WebP {}; VP9 {}; Opus {}.",
            detection_word(maintenance.ffmpeg_webp_available),
            detection_word(maintenance.ffmpeg_vp9_encoder_available),
            detection_word(maintenance.ffmpeg_opus_available)
        ),
        state,
    )
}

fn dashboard_job_status(
    site_health: &SiteHealthSnapshot,
) -> (String, String, crate::templates::AdminDashboardState) {
    let state = if site_health.failed_jobs > 0 {
        crate::templates::AdminDashboardState::ActionNeeded
    } else if site_health.running_jobs > 0
        || site_health.queued_jobs > 0
        || site_health.backup_jobs != "idle"
    {
        crate::templates::AdminDashboardState::Warning
    } else if site_health.backup_jobs == "unknown" {
        crate::templates::AdminDashboardState::Unknown
    } else {
        crate::templates::AdminDashboardState::Ok
    };
    let status = if site_health.failed_jobs > 0 {
        format!("{} failed", site_health.failed_jobs)
    } else if site_health.running_jobs > 0 || site_health.queued_jobs > 0 {
        format!(
            "{} running / {} queued",
            site_health.running_jobs, site_health.queued_jobs
        )
    } else {
        "idle".to_owned()
    };
    (
        status,
        format!(
            "Recently completed {}; backup job {}; restore jobs {}.",
            site_health.recent_completed_jobs, site_health.backup_jobs, site_health.restore_jobs
        ),
        state,
    )
}

fn dashboard_report_status(
    activity: &DashboardActivitySnapshot,
    moderation: &ModerationDomainData,
) -> (String, String, crate::templates::AdminDashboardState) {
    let open_reports = moderation.reports.len();
    let open_appeals = moderation.appeals.len();
    let recent_reports = activity.recent_reports_7d;
    let state = if open_reports > 0 || open_appeals > 0 {
        crate::templates::AdminDashboardState::ActionNeeded
    } else if recent_reports.is_some_and(|count| count > 0) {
        crate::templates::AdminDashboardState::Warning
    } else if recent_reports.is_some() {
        crate::templates::AdminDashboardState::Ok
    } else {
        crate::templates::AdminDashboardState::Unknown
    };
    let status = if open_reports == 0 {
        "no open reports".to_owned()
    } else {
        count_label(open_reports, "open report", "open reports")
    };
    let recent = recent_reports.map_or_else(|| "unknown".to_owned(), |count| count.to_string());
    (
        status,
        format!("{recent} reports in 7d; {open_appeals} open appeals."),
        state,
    )
}

fn public_url_label() -> String {
    let Some(host) = CONFIG.public_hosts.first().filter(|host| !host.is_empty()) else {
        return "not configured".to_owned();
    };
    let scheme = if CONFIG.tls.enabled { "https" } else { "http" };
    format!("{scheme}://{host}")
}

fn count_label(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

fn optional_count_label(count: Option<i64>, singular: &str, plural: &str) -> String {
    match count {
        Some(1) => format!("1 {singular}"),
        Some(count) => format!("{count} {plural}"),
        None => "unknown".to_owned(),
    }
}

fn thread_count_label(activity: &DashboardActivitySnapshot) -> String {
    match (activity.active_threads, activity.total_threads) {
        (Some(active), Some(total)) => format!("{active} active / {total} total"),
        _ => "unknown".to_owned(),
    }
}

fn recent_activity_label(activity: &DashboardActivitySnapshot) -> String {
    match (activity.posts_24h, activity.posts_7d) {
        (Some(day), Some(week)) => format!("{day} posts in 24h; {week} in 7d"),
        _ => "recent post activity unknown".to_owned(),
    }
}

fn media_summary_label(activity: &DashboardActivitySnapshot) -> String {
    match (
        activity.upload_posts,
        activity.total_images,
        activity.total_videos,
        activity.total_audio,
        activity.active_bytes,
    ) {
        (Some(upload_posts), Some(images), Some(videos), Some(audio), Some(active_bytes)) => {
            let active = crate::utils::files::format_file_size(active_bytes);
            format!(
                "{upload_posts} upload posts; {images} images, {videos} video, {audio} audio; {active} active"
            )
        }
        _ => "media summary unknown".to_owned(),
    }
}

fn render_admin_panel_from_snapshot(
    snapshot: AdminPanelSnapshot,
    csrf_token: &str,
    tor_address: Option<String>,
    flash: Option<(bool, String)>,
    open_section: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    let diagnostics_text = build_diagnostics_text(&snapshot, tor_address.as_deref());
    let flash_ref = flash
        .as_ref()
        .map(|(is_error, message)| crate::templates::AdminPanelFlash {
            is_error: *is_error,
            message,
        });
    let view = crate::templates::AdminPanelViewModel {
        csrf_token,
        boards: &snapshot.boards,
        current_theme,
        dashboard: build_dashboard_view(&snapshot.dashboard),
        moderation: crate::templates::AdminPanelModerationView {
            bans: &snapshot.bans,
            filters: &snapshot.filters,
            reports: &snapshot.reports,
            appeals: &snapshot.appeals,
        },
        appearance: crate::templates::AdminPanelAppearanceView {
            site_name: &snapshot.site_name,
            site_subtitle: &snapshot.site_subtitle,
            homepage_new_thread_badges_enabled: snapshot.homepage_new_thread_badges_enabled,
            homepage_new_reply_badges_enabled: snapshot.homepage_new_reply_badges_enabled,
            thread_new_reply_badges_enabled: snapshot.thread_new_reply_badges_enabled,
            default_theme: &snapshot.default_theme,
            banner_rotation_interval_minutes: snapshot.banner_rotation_interval_minutes,
            banner_external_links_enabled: snapshot.banner_external_links_enabled,
            themes: &snapshot.themes,
            global_banners: &snapshot.global_banners,
            home_banners: &snapshot.home_banners,
            board_banners: &snapshot.board_banners,
        },
        site_health: build_site_health_view(&snapshot, tor_address.as_deref(), &diagnostics_text),
        backups: crate::templates::AdminPanelBackupsView {
            full_backups: &snapshot.full_backups,
            board_backups: &snapshot.board_backups,
            backup_status_line: &snapshot.backup_summary.status_line,
            backup_warning: snapshot.backup_summary.warning.as_deref(),
            auto_full_backup_interval_hours: snapshot.auto_full_backup_interval_hours,
            auto_full_backup_copies_to_keep: snapshot.auto_full_backup_copies_to_keep,
            auto_full_backup_include_tor_hidden_service_keys: snapshot
                .auto_full_backup_include_tor_hidden_service_keys,
            auto_full_backup_storage_mode: &snapshot.auto_full_backup_storage_mode,
            auto_full_backup_split_zip_part_size_gib:
                crate::handlers::admin::backup::split_zip_part_size_gib(
                    snapshot.auto_full_backup_split_zip_part_size_bytes,
                ),
            tor_hidden_service_key_backup_available:
                crate::config::configured_tor_hidden_service_keys_dir().is_some(),
        },
        maintenance: crate::templates::AdminPanelMaintenanceView {
            db_size_bytes: snapshot.db_size_bytes,
            db_size_warning: snapshot.db_size_warning,
            setup_status: snapshot.setup_status,
            ffmpeg_timeout_secs: snapshot.ffmpeg_timeout_secs,
            media_auto_prune_enabled: snapshot.media_auto_prune_enabled,
            media_max_active_content_size_bytes: snapshot.media_max_active_content_size_bytes,
            media_detection: crate::templates::AdminMediaDetectionView {
                ffmpeg: if snapshot.ffmpeg_available {
                    crate::templates::AdminDetectionStatus::Detected
                } else {
                    crate::templates::AdminDetectionStatus::Missing
                },
                ffprobe: if snapshot.ffprobe_available {
                    crate::templates::AdminDetectionStatus::Detected
                } else {
                    crate::templates::AdminDetectionStatus::Missing
                },
                webp_encoder: if snapshot.ffmpeg_webp_available {
                    crate::templates::AdminDetectionStatus::Detected
                } else {
                    crate::templates::AdminDetectionStatus::Missing
                },
                vp9_pipeline: if snapshot.ffmpeg_vp9_available {
                    crate::templates::AdminDetectionStatus::Detected
                } else {
                    crate::templates::AdminDetectionStatus::Missing
                },
                pdf_thumbnail_renderer: snapshot.pdf_thumbnail_renderer.clone(),
            },
        },
        tor_address: tor_address.as_deref(),
        flash: flash_ref,
        open_section,
    };
    crate::templates::admin_panel_page(&view)
}

fn build_dashboard_view(
    dashboard: &AdminDashboardSummary,
) -> crate::templates::AdminPanelDashboardView<'_> {
    crate::templates::AdminPanelDashboardView {
        version: &dashboard.version,
        build: &dashboard.build,
        setup_status: &dashboard.setup_status,
        setup_detail: &dashboard.setup_detail,
        setup_state: dashboard.setup_state,
        site_title: &dashboard.site_title,
        public_url: &dashboard.public_url,
        db_status: &dashboard.db_status,
        db_detail: &dashboard.db_detail,
        db_state: dashboard.db_state,
        backup_status: &dashboard.backup_status,
        backup_detail: &dashboard.backup_detail,
        backup_state: dashboard.backup_state,
        storage_status: &dashboard.storage_status,
        storage_detail: &dashboard.storage_detail,
        storage_state: dashboard.storage_state,
        tor_status: &dashboard.tor_status,
        tor_detail: &dashboard.tor_detail,
        tor_state: dashboard.tor_state,
        dependency_status: &dashboard.dependency_status,
        dependency_detail: &dashboard.dependency_detail,
        dependency_state: dashboard.dependency_state,
        job_status: &dashboard.job_status,
        job_detail: &dashboard.job_detail,
        job_state: dashboard.job_state,
        board_count: &dashboard.board_count,
        thread_count: &dashboard.thread_count,
        post_count: &dashboard.post_count,
        recent_activity: &dashboard.recent_activity,
        media_summary: &dashboard.media_summary,
        report_status: &dashboard.report_status,
        report_detail: &dashboard.report_detail,
        report_state: dashboard.report_state,
    }
}

fn build_site_health_view<'a>(
    snapshot: &'a AdminPanelSnapshot,
    tor_address: Option<&'a str>,
    diagnostics_text: &'a str,
) -> crate::templates::AdminPanelSiteHealthView<'a> {
    crate::templates::AdminPanelSiteHealthView {
        server_status: &snapshot.site_health.server_status,
        rustchan_version: env!("CARGO_PKG_VERSION"),
        database_integrity_status: &snapshot.site_health.database_integrity_status,
        last_successful_backup: &snapshot.site_health.last_successful_backup,
        next_scheduled_backup: &snapshot.site_health.next_scheduled_backup,
        data_dir_usage: &snapshot.site_health.data_dir_usage,
        upload_dir_size: &snapshot.site_health.upload_dir_size,
        tor_status: &snapshot.site_health.tor_status,
        tor_onion_address: tor_address,
        dependency_summary: crate::templates::AdminSiteHealthDependencySummary {
            ffmpeg: detection_status(snapshot.ffmpeg_available),
            ffprobe: detection_status(snapshot.ffprobe_available),
            webp: detection_status(snapshot.ffmpeg_webp_available),
            vp9: detection_status(snapshot.ffmpeg_vp9_encoder_available),
            opus: detection_status(snapshot.ffmpeg_opus_available),
        },
        running_jobs: snapshot.site_health.running_jobs,
        queued_jobs: snapshot.site_health.queued_jobs,
        recent_completed_jobs: snapshot.site_health.recent_completed_jobs,
        failed_jobs: snapshot.site_health.failed_jobs,
        backup_jobs: &snapshot.site_health.backup_jobs,
        restore_jobs: &snapshot.site_health.restore_jobs,
        diagnostics_text,
    }
}

const fn detection_status(detected: bool) -> crate::templates::AdminDetectionStatus {
    if detected {
        crate::templates::AdminDetectionStatus::Detected
    } else {
        crate::templates::AdminDetectionStatus::Missing
    }
}

const fn detection_word(detected: bool) -> &'static str {
    if detected {
        "found"
    } else {
        "missing"
    }
}

fn build_diagnostics_text(snapshot: &AdminPanelSnapshot, tor_address: Option<&str>) -> String {
    let tor_enabled = if CONFIG.enable_tor_support {
        "yes"
    } else {
        "no"
    };
    let tls_enabled = if CONFIG.tls.enabled { "yes" } else { "no" };
    let reverse_proxy = if CONFIG.behind_proxy { "yes" } else { "no" };
    let tor_detail = tor_address.unwrap_or("not available");
    format!(
        "RustChan version: {version}\n\
         OS: {os}-{arch}\n\
         SQLite: {sqlite}\n\
         ffmpeg: {ffmpeg}\n\
         ffprobe: {ffprobe}\n\
         Tor enabled: {tor_enabled} ({tor_detail})\n\
         TLS enabled: {tls_enabled}\n\
         Reverse proxy: {reverse_proxy}\n\
         Data directory: configured\n\
         Main log directory: configured\n\
         Dependency log: configured\n\
         Recent warnings:\n{warnings}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        sqlite = rusqlite::version(),
        ffmpeg = detection_word(snapshot.ffmpeg_available),
        ffprobe = detection_word(snapshot.ffprobe_available),
        warnings = indent_diagnostics_block(&snapshot.site_health.recent_warnings),
    )
}

fn indent_diagnostics_block(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Query(params): Query<AdminPanelQuery>,
) -> Result<(CookieJar, Html<String>)> {
    // Move auth check and all DB calls into spawn_blocking.
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let cookie_secure = should_set_secure_cookie(&headers, secure_context);
    let mut session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let mut jar = jar;
    if session_id.is_none() {
        if let Some(bootstrap_token) = params.bootstrap.as_deref() {
            if let Some(bootstrapped_session_id) = consume_admin_session_bootstrap(bootstrap_token)
            {
                let mut cookie = axum_extra::extract::cookie::Cookie::new(
                    SESSION_COOKIE,
                    bootstrapped_session_id.clone(),
                );
                cookie.set_http_only(true);
                cookie.set_same_site(ADMIN_COOKIE_SAME_SITE);
                cookie.set_path("/");
                cookie.set_secure(cookie_secure);
                cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));
                jar = jar.add(cookie);
                session_id = Some(bootstrapped_session_id);
            }
        }
    }
    let (jar, csrf) = ensure_admin_csrf(jar, cookie_secure)?;
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
        Some((false, "Backup saved on the server.".to_owned()))
    } else if params.backup_deleted.is_some() {
        Some((false, "Backup deleted.".to_owned()))
    } else if params.restored.is_some() {
        Some((false, "Restore completed successfully.".to_owned()))
    } else if params.settings_saved.is_some() {
        Some((false, "Site settings saved.".to_owned()))
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
    let auto_full_backup_settings = state.auto_full_backup_settings.snapshot();
    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let open_section = params.open.clone();
        move || -> Result<String> {
            let conn = pool.get()?;

            // Auth check inside blocking task
            let sid = session_id.ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
            db::get_session(&conn, &sid)?
                .ok_or_else(|| AppError::Forbidden("Session expired or invalid.".into()))?;

            let (snapshot, tor_address) = load_admin_panel_snapshot(
                &conn,
                &state,
                onion_address_val,
                auto_full_backup_settings,
            )?;
            Ok(render_admin_panel_from_snapshot(
                snapshot,
                &csrf_clone,
                tor_address,
                flash,
                open_section.as_deref(),
                current_theme.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)))
}

pub async fn admin_site_health_jobs(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());

    let jobs = tokio::task::spawn_blocking({
        let state = state.clone();
        move || -> Result<SiteHealthJobsSnapshot> {
            let conn = state.db.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(load_site_health_jobs_snapshot(&conn, &state))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let payload =
        serde_json::to_string(&jobs).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_owned(),
            ),
            (
                header::CACHE_CONTROL,
                "private, no-cache, no-store, must-revalidate, no-transform".to_owned(),
            ),
            (header::PRAGMA, "no-cache".to_owned()),
            (header::EXPIRES, "0".to_owned()),
            (header::VARY, "Cookie".to_owned()),
        ],
        payload,
    )
        .into_response())
}

pub async fn admin_live_log(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<LiveLogQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
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
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_owned(),
            ),
            (
                header::CACHE_CONTROL,
                "private, no-cache, no-store, must-revalidate, no-transform".to_owned(),
            ),
            (header::PRAGMA, "no-cache".to_owned()),
            (header::EXPIRES, "0".to_owned()),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                "no".to_owned(),
            ),
            (header::VARY, "Cookie".to_owned()),
        ],
        payload,
    )
        .into_response())
}

fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(logs_dir).ok()?.flatten() {
        let path = entry.path();
        if !crate::logging::is_main_log_file(&path) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().ok()?;
        if latest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            latest = Some((modified, path));
        }
    }
    latest.map(|(_, path)| path)
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

    let buf =
        std::fs::read(path).map_err(|e| AppError::Internal(anyhow::anyhow!("Read log: {e}")))?;
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    let text = String::from_utf8_lossy(buf.get(start..).unwrap_or_default()).into_owned();
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

fn admin_bootstrap_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn create_admin_session_bootstrap(session_id: &str) -> String {
    let token = crate::utils::crypto::new_session_id();
    let expires_at = admin_bootstrap_now_secs().saturating_add(ADMIN_BOOTSTRAP_TTL_SECS);
    ADMIN_SESSION_BOOTSTRAPS.insert(token.clone(), (session_id.to_owned(), expires_at));
    token
}

pub(super) fn consume_admin_session_bootstrap(token: &str) -> Option<String> {
    let now = admin_bootstrap_now_secs();
    ADMIN_SESSION_BOOTSTRAPS.retain(|_, (_, expires_at)| *expires_at > now);

    let (session_id, expires_at) = ADMIN_SESSION_BOOTSTRAPS.remove(token)?.1;
    (expires_at > now).then_some(session_id)
}

#[cfg(test)]
mod tests {
    use super::{
        admin_live_log, admin_site_health_jobs, consume_admin_session_bootstrap,
        create_admin_session_bootstrap, dashboard_backup_status, dashboard_recent_count,
        dashboard_thread_counts, host_header_uses_https_port_with_config,
        hosts_match_for_same_origin, latest_log_file, load_dashboard_activity_snapshot,
        optional_count_query, read_log_tail, request_origin_uses_https,
        request_scheme_for_same_origin_with_config, require_same_origin_or_valid_csrf,
        require_same_origin_request, should_set_secure_cookie_with_config, BackupSummary,
        LiveLogQuery, SESSION_COOKIE,
    };
    use crate::error::AppError;
    use crate::middleware::SecureCookieContext;
    use axum::{
        body::to_bytes,
        extract::{Query, State},
        http::{header, HeaderMap, HeaderValue, StatusCode},
    };
    use axum_extra::extract::cookie::{Cookie, CookieJar};

    const TEST_ONION_HOST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion";
    const TEST_ONION_ORIGIN: &str =
        "http://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion";
    const TEST_ONION_HTTPS_ORIGIN: &str =
        "https://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion";

    #[test]
    fn dashboard_count_helpers_fail_closed_when_schema_is_missing() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");

        assert_eq!(
            optional_count_query(&conn, "SELECT COUNT(*) FROM posts"),
            None
        );
        assert_eq!(dashboard_thread_counts(&conn), None);
        assert_eq!(dashboard_recent_count(&conn, "posts", 24 * 60 * 60), None);
        assert_eq!(
            dashboard_recent_count(&conn, "sqlite_master", 24 * 60 * 60),
            None
        );
    }

    #[test]
    fn dashboard_activity_snapshot_counts_existing_systems() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "dash", "Dashboard", "", false).expect("board");
        let now = chrono::Utc::now().timestamp();

        conn.execute(
            "INSERT INTO threads (board_id, subject, created_at, bumped_at, archived)
             VALUES (?1, 'active', ?2, ?2, 0)",
            rusqlite::params![board_id, now],
        )
        .expect("active thread");
        let active_thread_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO posts
             (thread_id, board_id, body, body_html, file_path, file_name, file_size,
              thumb_path, mime_type, media_type, audio_file_path, audio_file_name,
              audio_file_size, audio_mime_type, created_at, deletion_token, is_op)
             VALUES
             (?1, ?2, 'active body', 'active body', 'dash/image.webp', 'image.webp', 11,
              'dash/thumb.webp', 'image/webp', 'image', 'dash/audio.ogg', 'audio.ogg',
              7, 'audio/ogg', ?3, 'delete-active', 1)",
            rusqlite::params![active_thread_id, board_id, now],
        )
        .expect("active post");
        let active_post_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO threads (board_id, subject, created_at, bumped_at, archived)
             VALUES (?1, 'archived', ?2, ?2, 1)",
            rusqlite::params![board_id, now - (8 * 24 * 60 * 60)],
        )
        .expect("archived thread");
        let archived_thread_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO posts
             (thread_id, board_id, body, body_html, file_path, file_name, file_size,
              thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
             VALUES
             (?1, ?2, 'archived body', 'archived body', 'dash/video.webm', 'video.webm',
              1000, 'dash/video-thumb.webp', 'video/webm', 'video', ?3, 'delete-archived', 1)",
            rusqlite::params![archived_thread_id, board_id, now - (8 * 24 * 60 * 60)],
        )
        .expect("archived post");

        conn.execute(
            "INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash, created_at)
             VALUES (?1, ?2, ?3, 'needs review', 'reporter', ?4)",
            rusqlite::params![active_post_id, active_thread_id, board_id, now],
        )
        .expect("report");

        let activity = load_dashboard_activity_snapshot(&conn, 1);

        assert_eq!(activity.board_count, 1);
        assert_eq!(activity.active_threads, Some(1));
        assert_eq!(activity.total_threads, Some(2));
        assert_eq!(activity.total_posts, Some(2));
        assert_eq!(activity.posts_24h, Some(1));
        assert_eq!(activity.posts_7d, Some(1));
        assert_eq!(activity.upload_posts, Some(2));
        assert_eq!(activity.total_images, Some(1));
        assert_eq!(activity.total_videos, Some(1));
        assert_eq!(activity.total_audio, Some(1));
        assert_eq!(activity.active_bytes, Some(18));
        assert_eq!(activity.recent_reports_7d, Some(1));
    }

    #[test]
    fn dashboard_backup_status_classifies_warning_and_ok_states() {
        let missing = BackupSummary {
            warning: Some(
                "No saved full backup found. Create and download a verified full backup before relying on this node."
                    .to_owned(),
            ),
            status_line: "Latest full backup: none saved.".to_owned(),
        };
        let stale = BackupSummary {
            warning: Some(
                "Latest verified full backup 'backup.zip' is older than 72 hours (96h ago)."
                    .to_owned(),
            ),
            status_line: "Latest full backup: backup.zip (96h ago) - verified.".to_owned(),
        };
        let ok = BackupSummary {
            warning: None,
            status_line: "Latest full backup: backup.zip (1h ago) - verified.".to_owned(),
        };

        assert_eq!(
            dashboard_backup_status(&missing).2,
            crate::templates::AdminDashboardState::ActionNeeded
        );
        assert_eq!(
            dashboard_backup_status(&stale).2,
            crate::templates::AdminDashboardState::Warning
        );
        assert_eq!(
            dashboard_backup_status(&ok).2,
            crate::templates::AdminDashboardState::Ok
        );
    }

    fn same_origin_headers(host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_str(host).expect("host header"),
        );
        headers
    }

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
    fn null_origin_is_not_considered_same_origin() {
        assert!(!hosts_match_for_same_origin("null", "localhost"));
    }

    #[test]
    fn same_origin_request_accepts_loopback_aliases_with_matching_port() {
        let mut headers = same_origin_headers("127.0.0.1:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:8080"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());

        let mut headers = same_origin_headers("[::1]:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_accepts_ipv6_loopback_bracket_format() {
        let mut headers = same_origin_headers("[::1]:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://[::1]:8080"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_accepts_referer_when_origin_is_missing() {
        let mut headers = same_origin_headers("localhost:8080");
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("http://127.0.0.1:8080/admin"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_accepts_valid_onion_http_origin() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(header::ORIGIN, HeaderValue::from_static(TEST_ONION_ORIGIN));

        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_accepts_valid_onion_http_referer() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(
            header::REFERER,
            HeaderValue::from_static(concat!(
                "http://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion",
                "/admin/panel"
            )),
        );

        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_rejects_spoofed_onion_origin() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static(
                "http://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbm2dqd.onion",
            ),
        );

        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_request_keeps_onion_http_when_tls_uses_default_https_port() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(header::ORIGIN, HeaderValue::from_static(TEST_ONION_ORIGIN));

        assert_eq!(
            request_scheme_for_same_origin_with_config(
                &headers,
                None,
                TEST_ONION_HOST,
                false,
                true,
                443,
            ),
            "http"
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_preserves_default_https_port_for_clearnet_hosts() {
        let headers = same_origin_headers("example.test");

        assert_eq!(
            request_scheme_for_same_origin_with_config(
                &headers,
                None,
                "example.test",
                false,
                true,
                443,
            ),
            "https"
        );
    }

    #[test]
    fn same_origin_request_accepts_missing_origin_and_referer_with_same_origin_fetch_metadata() {
        let mut headers = same_origin_headers("demo.serveo.net");
        headers.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_rejects_missing_origin_and_referer_with_cross_site_fetch_metadata() {
        let mut headers = same_origin_headers("demo.serveo.net");
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_or_valid_csrf_accepts_headerless_post_with_valid_csrf() {
        let headers = same_origin_headers("demo.serveo.net");
        assert!(require_same_origin_or_valid_csrf(&headers, None, true).is_ok());
    }

    #[test]
    fn same_origin_or_valid_csrf_rejects_headerless_post_with_invalid_csrf() {
        let headers = same_origin_headers("demo.serveo.net");
        assert!(require_same_origin_or_valid_csrf(&headers, None, false).is_err());
    }

    #[test]
    fn same_origin_or_valid_csrf_rejects_cross_origin_post_with_valid_csrf() {
        let mut headers = same_origin_headers("demo.serveo.net");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.test"),
        );
        assert!(require_same_origin_or_valid_csrf(&headers, None, true).is_err());
    }

    #[test]
    fn same_origin_request_accepts_null_origin_with_same_origin_referer_on_https_tunnel() {
        let mut headers = same_origin_headers("demo.serveo.net");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://demo.serveo.net/admin"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn same_origin_request_accepts_same_host_https_origin_on_https_tunnel() {
        let mut headers = same_origin_headers("rustchan.serveousercontent.com");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://rustchan.serveousercontent.com"),
        );
        assert!(require_same_origin_request(&headers, None).is_ok());
    }

    #[test]
    fn admin_post_csrf_accepts_scoped_token_on_https_tunnel_host() {
        let mut headers = same_origin_headers("rustchan.serveousercontent.com");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://rustchan.serveousercontent.com"),
        );
        let token = crate::utils::crypto::make_scoped_csrf_form_token(
            "csrf123",
            &crate::config::CONFIG.cookie_secret,
            "session123",
        );
        let jar = CookieJar::new()
            .add(Cookie::new("csrf_token", "csrf123"))
            .add(Cookie::new(SESSION_COOKIE, "session123"));

        assert!(
            super::require_admin_post_origin_and_csrf(&jar, &headers, None, Some(&token)).is_ok()
        );
        assert!(
            super::require_admin_post_origin_and_csrf(&jar, &headers, None, Some("bad")).is_err()
        );
        assert!(super::require_admin_post_origin_and_csrf(&jar, &headers, None, None).is_err());
    }

    #[test]
    fn same_origin_request_rejects_null_origin_for_non_loopback_targets() {
        let mut headers = same_origin_headers("192.168.1.20:8080");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(require_same_origin_request(&headers, None).is_err());

        let mut headers = same_origin_headers("board-admin-exampleonion123.onion");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_request_rejects_default_https_origin_with_explicit_http_port() {
        let mut headers = same_origin_headers("example.test:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://example.test"),
        );
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_request_rejects_port_mismatch_even_for_loopback_aliases() {
        let mut headers = same_origin_headers("localhost:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:3000"),
        );
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_request_does_not_treat_private_lan_ips_as_loopback_aliases() {
        assert!(!hosts_match_for_same_origin("192.168.1.20", "127.0.0.1"));
        assert!(!hosts_match_for_same_origin("10.0.0.5", "localhost"));
        assert!(!hosts_match_for_same_origin("172.16.0.8", "::1"));
    }

    #[test]
    fn same_origin_request_rejects_loopback_lookalike_hostnames() {
        assert!(!hosts_match_for_same_origin(
            "127.0.0.1.evil.com",
            "127.0.0.1"
        ));
        assert!(!hosts_match_for_same_origin(
            "localhost.evil.com",
            "localhost"
        ));
        assert!(!hosts_match_for_same_origin("::1.evil.com", "::1"));
        assert!(!hosts_match_for_same_origin("localhost.", "localhost"));
    }

    #[test]
    fn same_origin_request_rejects_weird_loopback_encodings() {
        assert!(!hosts_match_for_same_origin("%5B::1%5D", "::1"));
        assert!(!hosts_match_for_same_origin("127.000.000.001", "127.0.0.1"));
        assert!(!hosts_match_for_same_origin("2130706433", "127.0.0.1"));
        assert!(!hosts_match_for_same_origin("0x7f000001", "127.0.0.1"));
    }

    #[test]
    fn same_origin_request_rejects_malformed_bracketed_loopback_forms() {
        assert!(!hosts_match_for_same_origin("[::1", "::1"));
        assert!(!hosts_match_for_same_origin("::1]", "::1"));
        assert!(!hosts_match_for_same_origin("[127.0.0.1]", "127.0.0.1"));
        assert!(!hosts_match_for_same_origin("[localhost]", "localhost"));
        assert!(!hosts_match_for_same_origin("[[::1]]", "::1"));
    }

    #[test]
    fn same_origin_request_rejects_userinfo_bypass_shapes() {
        let mut headers = same_origin_headers("127.0.0.1:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1@evil.com:8080"),
        );
        assert!(require_same_origin_request(&headers, None).is_err());

        let mut headers = same_origin_headers("127.0.0.1:8080");
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.com@127.0.0.1:8080"),
        );
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn same_origin_request_rejects_non_loopback_null_origin_lookalikes() {
        let mut headers = same_origin_headers("localhost.evil.com:8080");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(require_same_origin_request(&headers, None).is_err());

        let mut headers = same_origin_headers("192.168.1.20:8080");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(require_same_origin_request(&headers, None).is_err());

        let mut headers = same_origin_headers("examplehiddenservice.onion");
        headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
        assert!(require_same_origin_request(&headers, None).is_err());
    }

    #[test]
    fn https_host_port_marks_request_secure() {
        let mut headers = HeaderMap::new();
        let host = format!("example.test:{}", crate::config::CONFIG.tls.port);
        headers.insert(
            header::HOST,
            HeaderValue::from_str(&host).expect("host header"),
        );
        assert!(host_header_uses_https_port_with_config(
            &headers,
            crate::config::CONFIG.tls.port
        ));
    }

    #[test]
    fn http_host_port_does_not_mark_request_secure() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.test:8080"));
        assert!(!host_header_uses_https_port_with_config(
            &headers,
            crate::config::CONFIG.tls.port
        ));
    }

    #[test]
    fn https_origin_marks_tunneled_request_secure() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("demo.serveo.net"));
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://demo.serveo.net/admin"),
        );
        assert!(request_origin_uses_https(&headers));
    }

    #[test]
    fn mismatched_https_origin_does_not_mark_request_secure() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("demo.serveo.net"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!request_origin_uses_https(&headers));
    }

    #[test]
    fn secure_cookie_decision_ignores_spoofed_https_origin_on_plain_http() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:8080"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://localhost:8080"),
        );
        let context =
            SecureCookieContext::new(Some("127.0.0.1:41000".parse().expect("peer")), false);

        assert!(!should_set_secure_cookie_with_config(
            &headers, context, true, false,
        ));
    }

    #[test]
    fn secure_cookie_decision_ignores_spoofed_https_host_port_on_plain_http() {
        let mut headers = HeaderMap::new();
        let host = format!("example.test:{}", crate::config::CONFIG.tls.port);
        headers.insert(
            header::HOST,
            HeaderValue::from_str(&host).expect("host header"),
        );
        let context =
            SecureCookieContext::new(Some("127.0.0.1:41000".parse().expect("peer")), false);

        assert!(!should_set_secure_cookie_with_config(
            &headers, context, true, false,
        ));
    }

    #[test]
    fn secure_cookie_decision_keeps_onion_plain_http_cookie_insecure() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(header::ORIGIN, HeaderValue::from_static(TEST_ONION_ORIGIN));
        let context =
            SecureCookieContext::new(Some("127.0.0.1:41000".parse().expect("peer")), false);

        assert!(!should_set_secure_cookie_with_config(
            &headers, context, true, false,
        ));
    }

    #[test]
    fn secure_cookie_decision_marks_direct_onion_https_cookie_secure() {
        let mut headers = same_origin_headers(TEST_ONION_HOST);
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static(TEST_ONION_HTTPS_ORIGIN),
        );
        let context =
            SecureCookieContext::new(Some("127.0.0.1:41000".parse().expect("peer")), true);

        assert!(should_set_secure_cookie_with_config(
            &headers, context, true, false,
        ));
    }

    #[test]
    fn secure_cookie_decision_requires_trusted_proxy_for_forwarded_proto() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:8080"));
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let trusted =
            SecureCookieContext::new(Some("127.0.0.1:41000".parse().expect("peer")), false);
        let untrusted =
            SecureCookieContext::new(Some("203.0.113.10:41000".parse().expect("peer")), false);

        assert!(!should_set_secure_cookie_with_config(
            &headers, trusted, true, false,
        ));
        assert!(!should_set_secure_cookie_with_config(
            &headers, untrusted, true, true,
        ));
        assert!(should_set_secure_cookie_with_config(
            &headers, trusted, true, true,
        ));
    }

    #[test]
    fn secure_cookie_decision_marks_direct_https_secure() {
        let headers = HeaderMap::new();
        let context = SecureCookieContext::new(None, true);

        assert!(should_set_secure_cookie_with_config(
            &headers, context, true, false,
        ));
        assert!(!should_set_secure_cookie_with_config(
            &headers, context, false, false,
        ));
    }

    #[test]
    fn admin_session_bootstrap_is_one_time() {
        let token = create_admin_session_bootstrap("session-123");
        assert_eq!(
            consume_admin_session_bootstrap(&token).as_deref(),
            Some("session-123")
        );
        assert!(consume_admin_session_bootstrap(&token).is_none());
    }

    #[test]
    fn picks_latest_log_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("rustchan.2026-04-01.log"), "old").expect("old");
        std::fs::write(dir.path().join("rustchan.2026-04-02.log"), "new").expect("new");
        std::fs::write(
            dir.path().join(crate::logging::DEPENDENCY_LOG_FILE_NAME),
            "deps",
        )
        .expect("deps");
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

    fn install_admin_session(state: &crate::middleware::AppState) {
        let conn = state.db.get().expect("db connection");
        let password_hash = crate::utils::crypto::hash_password("hunter2").expect("hash password");
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .expect("create session");
    }

    #[tokio::test]
    async fn live_log_requires_admin_auth() {
        let state = crate::test_support::app_state();
        let error = admin_live_log(
            State(state),
            CookieJar::new(),
            Query(LiveLogQuery { bytes: None }),
        )
        .await
        .expect_err("missing session should fail");

        match error {
            AppError::Forbidden(message) => assert_eq!(message, "Not logged in."),
            other => panic!("expected forbidden error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn site_health_jobs_requires_admin_auth() {
        let state = crate::test_support::app_state();
        let error = admin_site_health_jobs(State(state), CookieJar::new())
            .await
            .expect_err("missing session should fail");

        match error {
            AppError::Forbidden(message) => assert_eq!(message, "Not logged in."),
            other => panic!("expected forbidden error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn site_health_jobs_returns_no_store_json_body() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let (expected_post_id, expected_post_url);
        {
            let conn = state.db.get().expect("db connection");
            let board_id =
                crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
            conn.execute(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'job thread')",
                rusqlite::params![board_id],
            )
            .expect("insert thread");
            let thread_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, body, body_html, deletion_token, is_op)
                 VALUES (?1, ?2, 'job body', 'job body', 'delete-token', 1)",
                rusqlite::params![thread_id, board_id],
            )
            .expect("insert post");
            let post_id = conn.last_insert_rowid();
            expected_post_id = post_id;
            expected_post_url = format!("/test/thread/{thread_id}#p{post_id}");
            let failed_payload = serde_json::json!({
                "t": "SpamCheck",
                "d": {
                    "post_id": post_id,
                    "ip_hash": "hash",
                    "body_len": 8
                }
            })
            .to_string();
            conn.execute(
                "INSERT INTO background_jobs
                 (job_type, payload, status, attempts, last_error, updated_at)
                 VALUES
                 ('spam_check', ?1, 'failed', 3, ?2, unixepoch()),
                 ('thread_prune', '{}', 'done', 1, NULL, unixepoch())",
                rusqlite::params![
                    failed_payload,
                    "failed reading /Users/example/private.txt with token=abc123 ".repeat(8)
                ],
            )
            .expect("insert background jobs");
        }
        let response = admin_site_health_jobs(
            State(state),
            CookieJar::new().add(Cookie::new(SESSION_COOKIE, "session123")),
        )
        .await
        .expect("handler response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json; charset=utf-8"))
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static(
                "private, no-cache, no-store, must-revalidate, no-transform"
            ))
        );
        assert_eq!(
            response.headers().get(header::VARY),
            Some(&HeaderValue::from_static("Cookie"))
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(
            payload
                .get("backup_jobs")
                .and_then(serde_json::Value::as_str),
            Some("idle")
        );
        assert!(payload.get("running_jobs").is_some());
        assert!(payload.get("queued_jobs").is_some());
        assert!(payload.get("recent_failed_job_details").is_some());
        assert!(payload.get("recent_completed_job_details").is_some());
        assert!(payload.get("thumbnail_transcode_jobs").is_none());
        assert!(payload.get("repair_vacuum_jobs").is_none());
        let failed_job = payload
            .get("recent_failed_job_details")
            .and_then(serde_json::Value::as_array)
            .and_then(|jobs| jobs.first())
            .expect("failed job detail");
        assert_eq!(failed_job["name"], "Spam check");
        assert_eq!(failed_job["attempts"], 3);
        assert_eq!(failed_job["post_id"], expected_post_id);
        assert_eq!(failed_job["post_url"], expected_post_url);
        let error = failed_job["error"].as_str().expect("error snippet");
        assert!(error.contains("[redacted]"));
        assert!(!error.contains("/Users/example"));
        assert!(!error.contains("abc123"));
        assert!(error.chars().count() <= 183);
    }

    #[tokio::test]
    async fn live_log_returns_no_store_headers_and_json_body() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let response = admin_live_log(
            State(state),
            CookieJar::new().add(Cookie::new(SESSION_COOKIE, "session123")),
            Query(LiveLogQuery { bytes: None }),
        )
        .await
        .expect("handler response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json; charset=utf-8"))
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static(
                "private, no-cache, no-store, must-revalidate, no-transform"
            ))
        );
        assert_eq!(
            response.headers().get(header::PRAGMA),
            Some(&HeaderValue::from_static("no-cache"))
        );
        assert_eq!(
            response.headers().get(header::EXPIRES),
            Some(&HeaderValue::from_static("0"))
        );
        assert_eq!(
            response
                .headers()
                .get(header::HeaderName::from_static("x-accel-buffering")),
            Some(&HeaderValue::from_static("no"))
        );
        assert_eq!(
            response.headers().get(header::VARY),
            Some(&HeaderValue::from_static("Cookie"))
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("json payload");
        assert_eq!(
            payload.get("filename").and_then(serde_json::Value::as_str),
            Some("no log file")
        );
        assert_eq!(
            payload
                .get("truncated")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            payload.get("content").and_then(serde_json::Value::as_str),
            Some("No live log file found yet.")
        );
    }
}
