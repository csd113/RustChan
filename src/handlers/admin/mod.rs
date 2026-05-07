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
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use serde::Deserialize;
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
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
    let name = db::get_admin_name_by_id(conn, admin_id)?.unwrap_or_else(|| "unknown".to_string());
    Ok((admin_id, name))
}

/// Check CSRF using the cookie jar. Returns error on mismatch.
/// Verify admin session from a session ID string.
/// For use inside `spawn_blocking` closures where we have an open connection.
fn require_admin_session_sid(conn: &rusqlite::Connection, session_id: Option<&str>) -> Result<i64> {
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
    let request_scheme =
        if crate::middleware::forwarded_proto_is_https(headers, peer, CONFIG.behind_proxy)
            || (CONFIG.tls.enabled && host_header_uses_https_port(headers))
            || request_origin_uses_https(headers)
        {
            "https"
        } else {
            "http"
        };
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
        .map_err(|_| AppError::Forbidden("Invalid Origin/Referer header.".into()))?;
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

pub(super) fn require_same_origin_or_valid_csrf(
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

pub(super) fn require_admin_post_origin_and_csrf(
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

fn admin_csrf_cookie(raw_token: String) -> Cookie<'static> {
    let mut cookie = Cookie::new("csrf_token", raw_token);
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie
}

pub(super) fn refresh_admin_csrf_cookie(jar: CookieJar) -> CookieJar {
    let cookie = admin_csrf_cookie(new_csrf_token());
    jar.add(cookie)
}

pub(super) fn ensure_admin_csrf(jar: CookieJar) -> Result<(CookieJar, String)> {
    let raw = jar
        .get("csrf_token")
        .map(axum_extra::extract::cookie::Cookie::value)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut jar = jar;
    let raw = if let Some(raw) = raw {
        raw
    } else {
        let raw = new_csrf_token();
        jar = jar.add(admin_csrf_cookie(raw.clone()));
        raw
    };
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(axum_extra::extract::cookie::Cookie::value)
        .ok_or_else(|| AppError::Forbidden("Not logged in.".into()))?;
    let session_id = session_id.to_string();
    Ok((
        jar,
        make_scoped_csrf_form_token(&raw, &CONFIG.cookie_secret, &session_id),
    ))
}

pub(super) use crate::utils::redirect::encode_query_component;

pub(in crate::handlers) fn should_set_secure_cookie(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> bool {
    if !CONFIG.https_cookies {
        return false;
    }

    if crate::middleware::forwarded_proto_is_https(headers, peer, CONFIG.behind_proxy) {
        return true;
    }

    if !CONFIG.tls.enabled {
        return request_origin_uses_https(headers);
    }

    host_header_uses_https_port(headers) || request_origin_uses_https(headers)
}

fn host_header_uses_https_port(headers: &HeaderMap) -> bool {
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
        Some(port) => port == CONFIG.tls.port,
        None => CONFIG.tls.port == 443,
    }
}

fn request_origin_uses_https(headers: &HeaderMap) -> bool {
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_string());

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

#[allow(clippy::struct_excessive_bools)]
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
    themes: Vec<crate::models::Theme>,
    global_banners: Vec<crate::models::BannerAsset>,
    home_banners: Vec<crate::models::BannerAsset>,
    board_banners: Vec<crate::models::BannerAsset>,
    full_backups: Vec<crate::models::BackupInfo>,
    board_backups: Vec<crate::models::BackupInfo>,
    db_size_bytes: i64,
    db_size_warning: bool,
    ffmpeg_timeout_secs: u64,
    media_auto_prune_enabled: bool,
    media_max_active_content_size_bytes: u64,
    ffmpeg_available: bool,
    ffprobe_available: bool,
    ffmpeg_webp_available: bool,
    ffmpeg_vp9_available: bool,
    pdf_thumbnail_renderer: Option<String>,
    backup_summary: BackupSummary,
}

#[derive(Clone)]
struct BackupSummary {
    warning: Option<String>,
    status_line: String,
}

struct OverviewDomainData {
    backup_summary: BackupSummary,
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

#[allow(clippy::struct_excessive_bools)]
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

#[allow(clippy::struct_excessive_bools)]
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
    pdf_thumbnail_renderer: Option<String>,
}

fn load_overview_domain_data(full_backups: &[BackupInfo]) -> OverviewDomainData {
    OverviewDomainData {
        backup_summary: build_backup_summary(full_backups),
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
        pdf_thumbnail_renderer: state.pdf_thumbnail_renderer.map(str::to_string),
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
            themes: appearance_domain.themes,
            global_banners: appearance_domain.global_banners,
            home_banners: appearance_domain.home_banners,
            board_banners: appearance_domain.board_banners,
            full_backups: backups_domain.full_backups,
            board_backups: backups_domain.board_backups,
            db_size_bytes: maintenance_domain.db_size_bytes,
            db_size_warning: maintenance_domain.db_size_warning,
            ffmpeg_timeout_secs: maintenance_domain.ffmpeg_timeout_secs,
            media_auto_prune_enabled: maintenance_domain.media_auto_prune_enabled,
            media_max_active_content_size_bytes: maintenance_domain
                .media_max_active_content_size_bytes,
            ffmpeg_available: maintenance_domain.ffmpeg_available,
            ffprobe_available: maintenance_domain.ffprobe_available,
            ffmpeg_webp_available: maintenance_domain.ffmpeg_webp_available,
            ffmpeg_vp9_available: maintenance_domain.ffmpeg_vp9_available,
            pdf_thumbnail_renderer: maintenance_domain.pdf_thumbnail_renderer,
            backup_summary: overview_domain.backup_summary,
        },
        onion_address_val,
    ))
}

fn build_backup_summary(full_backups: &[BackupInfo]) -> BackupSummary {
    const BACKUP_WARN_AFTER_HOURS: i64 = 72;

    let Some(latest) = full_backups.first() else {
        return BackupSummary {
            warning: Some(
                "No saved full backup found. Create and download a verified full backup before relying on this node."
                    .to_string(),
            ),
            status_line: "Latest full backup: none saved.".to_string(),
        };
    };

    let now = chrono::Utc::now().timestamp();
    let age_hours = latest
        .modified_epoch
        .map(|ts| now.saturating_sub(ts).max(0) / 3600);
    let age_text = age_hours
        .map(|hours| format!("{hours}h ago"))
        .unwrap_or_else(|| "unknown age".to_string());
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

fn render_admin_panel_from_snapshot(
    snapshot: AdminPanelSnapshot,
    csrf_token: &str,
    tor_address: Option<String>,
    flash: Option<(bool, String)>,
    open_section: Option<&str>,
) -> String {
    let flash_ref = flash
        .as_ref()
        .map(|(is_error, message)| crate::templates::AdminPanelFlash {
            is_error: *is_error,
            message,
        });
    let view = crate::templates::AdminPanelViewModel {
        csrf_token,
        boards: &snapshot.boards,
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
        backups: crate::templates::AdminPanelBackupsView {
            full_backups: &snapshot.full_backups,
            board_backups: &snapshot.board_backups,
            backup_status_line: &snapshot.backup_summary.status_line,
            backup_warning: snapshot.backup_summary.warning.as_deref(),
            auto_full_backup_interval_hours: snapshot.auto_full_backup_interval_hours,
            auto_full_backup_copies_to_keep: snapshot.auto_full_backup_copies_to_keep,
            auto_full_backup_include_tor_hidden_service_keys: snapshot
                .auto_full_backup_include_tor_hidden_service_keys,
            tor_hidden_service_key_backup_available:
                crate::config::configured_tor_hidden_service_keys_dir().is_some(),
        },
        maintenance: crate::templates::AdminPanelMaintenanceView {
            db_size_bytes: snapshot.db_size_bytes,
            db_size_warning: snapshot.db_size_warning,
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
                pdf_thumbnail_renderer: snapshot.pdf_thumbnail_renderer,
            },
        },
        tor_address: tor_address.as_deref(),
        flash: flash_ref,
        open_section,
    };
    crate::templates::admin_panel_page(&view)
}

pub async fn admin_panel(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Query(params): Query<AdminPanelQuery>,
) -> Result<(CookieJar, Html<String>)> {
    // Move auth check and all DB calls into spawn_blocking.
    let cookie_secure = should_set_secure_cookie(&headers, Some(peer));
    let mut session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_string());
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
    let (jar, csrf) = ensure_admin_csrf(jar)?;
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
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CACHE_CONTROL,
                "private, no-cache, no-store, must-revalidate, no-transform".to_string(),
            ),
            (header::PRAGMA, "no-cache".to_string()),
            (header::EXPIRES, "0".to_string()),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                "no".to_string(),
            ),
            (header::VARY, "Cookie".to_string()),
        ],
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

fn admin_bootstrap_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(super) fn create_admin_session_bootstrap(session_id: &str) -> String {
    let token = crate::utils::crypto::new_session_id();
    let expires_at = admin_bootstrap_now_secs().saturating_add(ADMIN_BOOTSTRAP_TTL_SECS);
    ADMIN_SESSION_BOOTSTRAPS.insert(token.clone(), (session_id.to_string(), expires_at));
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
        admin_live_log, consume_admin_session_bootstrap, create_admin_session_bootstrap,
        host_header_uses_https_port, hosts_match_for_same_origin, latest_log_file, read_log_tail,
        request_origin_uses_https, require_same_origin_or_valid_csrf, require_same_origin_request,
        LiveLogQuery, SESSION_COOKIE,
    };
    use crate::error::AppError;
    use axum::{
        body::to_bytes,
        extract::{Query, State},
        http::{header, HeaderMap, HeaderValue, StatusCode},
    };
    use axum_extra::extract::cookie::{Cookie, CookieJar};

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
        assert!(host_header_uses_https_port(&headers));
    }

    #[test]
    fn http_host_port_does_not_mark_request_secure() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.test:8080"));
        assert!(!host_header_uses_https_port(&headers));
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
