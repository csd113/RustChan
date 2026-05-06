// Route handlers use a shared module API and a deliberately broad import set
// so the request flow stays readable without a lot of plumbing noise.
#![allow(
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::redundant_pub_crate,
    clippy::significant_drop_tightening,
    clippy::format_push_string,
    clippy::redundant_clone,
    clippy::implicit_clone,
    clippy::map_unwrap_or,
    clippy::equatable_if_let
)]

// handlers/board.rs
//
// Handles:
//   GET  /                    — board list
//   GET  /:board/             — board index (thread list)
//   POST /:board/             — create new thread
//   GET  /:board/catalog      — catalog view
//   GET  /:board/search       — search results
//   POST /delete              — user post deletion

use crate::{
    config::CONFIG,
    db::{self},
    error::{AppError, Result},
    handlers::{parse_post_multipart, posting, render},
    middleware::{validate_csrf, AppState},
    models::{Board, Pagination, SearchQuery, SEARCH_QUERY_MAX_CHARS},
    templates,
    utils::crypto::{
        hash_ip, make_scoped_csrf_form_token, new_csrf_token, sha256_hex, verify_password,
    },
};
use axum::{
    extract::{ConnectInfo, Form, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{atomic::AtomicU64, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};
use time::Duration;

mod access_preferences;
mod catalog;
mod create_thread;
mod media;
mod pages;
mod reports;
#[cfg(test)]
mod tests;

pub use access_preferences::*;
pub use catalog::*;
pub use create_thread::*;
pub use media::*;
pub use pages::*;
pub use reports::*;

pub(crate) fn should_set_public_secure_cookie(
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> bool {
    crate::handlers::admin::should_set_secure_cookie(headers, peer)
}

const PREVIEW_REPLIES: i64 = 3;
const THREADS_PER_PAGE: i64 = 10;
pub const USER_THEME_COOKIE: &str = "rustchan_theme";
pub const USER_HIDE_NSFW_COOKIE: &str = "rustchan_hide_nsfw";
pub const USER_VIDEO_AUDIO_COOKIE: &str = "rustchan_video_audio";
pub const USER_PREFERRED_VIEW_COOKIE: &str = "rustchan_preferred_view";
pub const USER_ACTIVITY_BADGES_COOKIE: &str = "rustchan_activity_badges";
pub const NSFW_CONSENT_COOKIE: &str = "rustchan_nsfw_ok";
pub const VISITOR_ID_COOKIE: &str = "rustchan_visitor_id";
pub(crate) const ADMIN_SESSION_COOKIE: &str = "chan_admin_session";
const BOARD_ACCESS_COOKIE_PREFIX: &str = "rustchan_board_access_";
const BOARD_ACCESS_COOKIE_TTL_DAYS: i64 = 30;
const BOARD_UNLOCK_FAIL_LIMIT: u32 = 5;
const BOARD_UNLOCK_FAIL_WINDOW_SECS: u64 = 900;
const HTML_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_DYNAMIC_PUBLIC;
pub(crate) const X_RUSTCHAN_REDIRECT_HEADER: &str = "x-rustchan-redirect";

static BOARD_UNLOCK_FAILS: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);
static BOARD_UNLOCK_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

pub struct BoardAccessContext {
    pub board: Board,
    pub is_admin: bool,
    pub can_view: bool,
    pub can_post: bool,
}

pub(crate) enum BoardAccessRequirement {
    View,
    Post,
}

pub(crate) struct BoardAccessDenial {
    pub context: BoardAccessContext,
    pub return_to: String,
}

pub(crate) enum BoardAccessDecision {
    Allowed(BoardAccessContext),
    Denied(BoardAccessDenial),
}

type CatalogRenderData = (
    Board,
    Vec<crate::models::Thread>,
    HashSet<i64>,
    usize,
    String,
);

pub(crate) fn latest_visible_thread_marker_tuple(marker: Option<(i64, i64)>) -> (i64, i64) {
    marker.unwrap_or((0, 0))
}

pub(crate) fn thread_unread_counts(
    threads: &[crate::models::Thread],
    markers: &HashMap<i64, crate::handlers::board::ThreadActivityMarker>,
) -> HashMap<i64, i64> {
    threads
        .iter()
        .filter_map(|thread| {
            markers.get(&thread.id).and_then(|marker| {
                let unread = (thread.reply_count - marker.seen_reply_count).max(0);
                (unread > 0).then_some((thread.id, unread))
            })
        })
        .collect()
}

fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

#[derive(Serialize)]
struct XhrErrorPayload<'a> {
    error: &'a str,
}

fn json_response<T: Serialize>(status: StatusCode, payload: &T) -> Result<Response> {
    let body =
        serde_json::to_vec(payload).map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    Ok(response)
}

fn xhr_json_error_response(
    response_status: StatusCode,
    error_status: StatusCode,
    message: &str,
) -> Result<Response> {
    let mut response = json_response(response_status, &XhrErrorPayload { error: message })?;
    response.headers_mut().insert(
        "x-rustchan-error-status",
        HeaderValue::from_str(error_status.as_str())
            .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
    );
    Ok(response)
}

pub(crate) fn xhr_error_response(status: StatusCode, message: &str) -> Result<Response> {
    xhr_json_error_response(status, status, message)
}

// Browsers log `Failed to load resource` for XHR/fetch responses with 4xx/5xx
// statuses even when the page handles the JSON error inline. For validation
// errors that the UI already renders in-context, keep the transport successful
// and expose the original HTTP meaning via `X-Rustchan-Error-Status`.
pub(crate) fn xhr_handled_error_response(status: StatusCode, message: &str) -> Result<Response> {
    xhr_json_error_response(StatusCode::OK, status, message)
}

pub(crate) fn xhr_redirect_response(target: &str) -> Result<Response> {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response.headers_mut().insert(
        X_RUSTCHAN_REDIRECT_HEADER,
        HeaderValue::from_str(target)
            .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
    );
    Ok(response)
}

fn banned_page_redirect_url(reason: &str) -> String {
    let reason_for_url = reason.chars().take(256).collect::<String>();
    format!(
        "/banned?reason={}",
        crate::templates::urlencoding_simple(&reason_for_url)
    )
}

pub(crate) fn xhr_post_error_response(error: AppError) -> Result<Response> {
    match error {
        AppError::NotFound(message) => xhr_handled_error_response(StatusCode::NOT_FOUND, &message),
        AppError::BadRequest(message) => {
            xhr_handled_error_response(StatusCode::UNPROCESSABLE_ENTITY, &message)
        }
        AppError::Forbidden(message) => xhr_handled_error_response(StatusCode::FORBIDDEN, &message),
        AppError::BannedUser { reason, .. } => {
            xhr_redirect_response(&banned_page_redirect_url(&reason))
        }
        AppError::UploadTooLarge(message) => {
            xhr_handled_error_response(StatusCode::PAYLOAD_TOO_LARGE, &message)
        }
        AppError::InvalidMediaType(message) => {
            xhr_handled_error_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, &message)
        }
        AppError::Conflict(message) => xhr_handled_error_response(StatusCode::CONFLICT, &message),
        AppError::DbBusy => xhr_handled_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is temporarily busy. Please try again in a moment.",
        ),
        AppError::Internal(error) => {
            tracing::error!("Internal error during XHR post submission: {:?}", error);
            xhr_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "An internal error occurred.",
            )
        }
        AppError::Tls(message) => {
            tracing::error!("TLS error during XHR post submission: {message}");
            xhr_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "A TLS configuration error occurred.",
            )
        }
    }
}

#[derive(Deserialize)]
pub struct BannedPageQuery {
    pub reason: Option<String>,
}

pub fn current_theme_from_jar(jar: &CookieJar) -> Option<String> {
    jar.get(USER_THEME_COOKIE)
        .and_then(|cookie| crate::templates::normalize_theme_slug(cookie.value()))
}

pub fn user_preferences_from_jar(jar: &CookieJar) -> crate::templates::UserPreferences {
    let default_preferences = crate::templates::UserPreferences::default();
    crate::templates::UserPreferences {
        hide_nsfw_boards: jar
            .get(USER_HIDE_NSFW_COOKIE)
            .is_some_and(|cookie| cookie.value() == "1"),
        video_audio_muted: jar
            .get(USER_VIDEO_AUDIO_COOKIE)
            .is_some_and(|cookie| cookie.value() == "mute"),
        preferred_board_view: match jar.get(USER_PREFERRED_VIEW_COOKIE).map(Cookie::value) {
            Some("index") => crate::templates::PreferredBoardView::Index,
            Some("catalog") => crate::templates::PreferredBoardView::Catalog,
            _ => default_preferences.preferred_board_view,
        },
        show_activity_badges: jar
            .get(USER_ACTIVITY_BADGES_COOKIE)
            .map_or(default_preferences.show_activity_badges, |cookie| {
                cookie.value() != "0"
            }),
    }
}

pub fn check_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if validate_csrf(csrf_cookie.as_deref(), form_token.unwrap_or("")) {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

pub(crate) fn admin_scoped_csrf_token(
    jar: &CookieJar,
    admin_session_id: Option<&str>,
    is_admin: bool,
) -> Option<String> {
    if !is_admin {
        return None;
    }
    let raw = jar
        .get("csrf_token")
        .map(axum_extra::extract::cookie::Cookie::value)
        .filter(|value| !value.is_empty())?;
    let session_id = admin_session_id.filter(|value| !value.is_empty())?;
    Some(make_scoped_csrf_form_token(
        raw,
        &CONFIG.cookie_secret,
        session_id,
    ))
}

pub fn has_nsfw_consent(jar: &CookieJar) -> bool {
    jar.get(NSFW_CONSENT_COOKIE)
        .is_some_and(|cookie| cookie.value() == "1")
}

pub fn board_access_cookie_name(board_short: &str) -> String {
    format!("{BOARD_ACCESS_COOKIE_PREFIX}{board_short}")
}

pub fn board_access_cookie_from_jar(jar: &CookieJar, board_short: &str) -> Option<String> {
    let cookie_name = board_access_cookie_name(board_short);
    jar.get(cookie_name.as_str())
        .map(|cookie| cookie.value().to_string())
}

fn expected_board_access_cookie_value(board_short: &str, password_hash: &str) -> Option<String> {
    if password_hash.is_empty() {
        return None;
    }
    Some(sha256_hex(
        format!(
            "{}:board-access:{board_short}:{password_hash}",
            CONFIG.cookie_secret
        )
        .as_bytes(),
    ))
}

fn has_valid_board_access_cookie(
    board_short: &str,
    password_hash: &str,
    cookie_value: Option<&str>,
) -> bool {
    let Some(expected) = expected_board_access_cookie_value(board_short, password_hash) else {
        return false;
    };
    cookie_value.is_some_and(|value| value == expected)
}

pub fn can_view_board(board: &Board, is_admin: bool, access_cookie: Option<&str>) -> bool {
    is_admin
        || !board.access_mode.requires_view_password()
        || has_valid_board_access_cookie(
            &board.short_name,
            &board.access_password_hash,
            access_cookie,
        )
}

pub fn can_post_to_board(board: &Board, is_admin: bool, access_cookie: Option<&str>) -> bool {
    is_admin
        || !board.access_mode.requires_unlock_for_posting()
        || has_valid_board_access_cookie(
            &board.short_name,
            &board.access_password_hash,
            access_cookie,
        )
}

pub fn load_board_access_context(
    conn: &rusqlite::Connection,
    board_short: &str,
    admin_session_id: Option<&str>,
    access_cookie: Option<&str>,
) -> Result<BoardAccessContext> {
    let board = db::get_board_by_short(conn, board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
    let is_admin = posting::is_admin_session(conn, admin_session_id);
    Ok(BoardAccessContext {
        can_view: can_view_board(&board, is_admin, access_cookie),
        can_post: can_post_to_board(&board, is_admin, access_cookie),
        board,
        is_admin,
    })
}

pub(crate) async fn board_access_preflight(
    state: &AppState,
    board_short: &str,
    admin_session_id: Option<String>,
    access_cookie: Option<String>,
    requirement: BoardAccessRequirement,
    return_to: String,
) -> Result<BoardAccessDecision> {
    let access_context = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.to_string();
        move || -> Result<BoardAccessContext> {
            let conn = pool.get()?;
            load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let allowed = match requirement {
        BoardAccessRequirement::View => access_context.can_view,
        BoardAccessRequirement::Post => access_context.can_post,
    };

    if allowed {
        Ok(BoardAccessDecision::Allowed(access_context))
    } else {
        Ok(BoardAccessDecision::Denied(BoardAccessDenial {
            context: access_context,
            return_to,
        }))
    }
}

pub(crate) fn unlock_redirect_url(board_short: &str, return_to: &str) -> String {
    format!(
        "/{board_short}/unlock?return_to={}",
        crate::templates::urlencoding_simple(return_to)
    )
}

pub(crate) fn render_board_unlock_html(
    board: &Board,
    csrf_token: &str,
    return_to: &str,
    error: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    let boards = crate::templates::live_boards();
    crate::templates::board_access_page(
        board,
        csrf_token,
        boards.as_slice(),
        return_to,
        error,
        current_theme,
        board.collapse_greentext,
    )
}

pub(crate) fn board_access_denied_response(
    jar: CookieJar,
    denial: &BoardAccessDenial,
    csrf_token: &str,
    current_theme: Option<&str>,
) -> Response {
    let html = render_board_unlock_html(
        &denial.context.board,
        csrf_token,
        &denial.return_to,
        None,
        current_theme,
    );
    board_access_required_response(jar, html)
}

pub async fn banned_page(Query(query): Query<BannedPageQuery>, jar: CookieJar) -> Response {
    let (jar, csrf) = ensure_csrf(jar);
    let reason = query
        .reason
        .unwrap_or_else(|| "No reason given".to_string())
        .trim()
        .chars()
        .take(512)
        .collect::<String>();
    let reason = if reason.is_empty() {
        "No reason given".to_string()
    } else {
        reason
    };
    let html = crate::templates::ban_page(&reason, &csrf);
    (jar, Html(html)).into_response()
}

fn board_unlock_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn board_unlock_attempt_key(board_short: &str, client_ip: &str) -> String {
    sha256_hex(format!("{board_short}:{client_ip}").as_bytes())
}

fn prune_board_unlock_failures(now_secs: u64) {
    let last_cleanup = BOARD_UNLOCK_CLEANUP_SECS.load(std::sync::atomic::Ordering::Relaxed);
    if now_secs.saturating_sub(last_cleanup) < 60 {
        return;
    }
    if BOARD_UNLOCK_CLEANUP_SECS
        .compare_exchange(
            last_cleanup,
            now_secs,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        return;
    }
    BOARD_UNLOCK_FAILS.retain(|_, (_, window_start)| {
        now_secs.saturating_sub(*window_start) <= BOARD_UNLOCK_FAIL_WINDOW_SECS
    });
}

fn board_unlock_retry_after_secs(attempt_key: &str) -> Option<u64> {
    let now_secs = board_unlock_now_secs();
    prune_board_unlock_failures(now_secs);
    let (count, window_start) = *BOARD_UNLOCK_FAILS.get(attempt_key)?;
    let elapsed = now_secs.saturating_sub(window_start);
    if elapsed > BOARD_UNLOCK_FAIL_WINDOW_SECS {
        BOARD_UNLOCK_FAILS.remove(attempt_key);
        return None;
    }
    if count < BOARD_UNLOCK_FAIL_LIMIT {
        return None;
    }
    Some((BOARD_UNLOCK_FAIL_WINDOW_SECS.saturating_sub(elapsed)).max(1))
}

fn record_board_unlock_failure(attempt_key: &str) {
    let now_secs = board_unlock_now_secs();
    prune_board_unlock_failures(now_secs);
    let mut entry = BOARD_UNLOCK_FAILS
        .entry(attempt_key.to_string())
        .or_insert((0, now_secs));
    let (count, window_start) = entry.value_mut();
    if now_secs.saturating_sub(*window_start) > BOARD_UNLOCK_FAIL_WINDOW_SECS {
        *count = 1;
        *window_start = now_secs;
    } else {
        *count = count.saturating_add(1);
    }
}

fn clear_board_unlock_failures(attempt_key: &str) {
    BOARD_UNLOCK_FAILS.remove(attempt_key);
}

fn board_unlock_rate_limit_message(retry_after_secs: u64) -> String {
    let minutes = retry_after_secs / 60;
    let seconds = retry_after_secs % 60;
    if minutes > 0 && seconds > 0 {
        format!(
            "Too many incorrect board password attempts. Try again in {minutes} minute{} and {seconds} second{}.",
            if minutes == 1 { "" } else { "s" },
            if seconds == 1 { "" } else { "s" }
        )
    } else if minutes > 0 {
        format!(
            "Too many incorrect board password attempts. Try again in {minutes} minute{}.",
            if minutes == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Too many incorrect board password attempts. Try again in {retry_after_secs} second{}.",
            if retry_after_secs == 1 { "" } else { "s" }
        )
    }
}

fn board_unlock_default_return_to(board: &Board) -> String {
    if board.access_mode.requires_view_password() {
        format!("/{}/catalog", board.short_name)
    } else {
        format!("/{}", board.short_name)
    }
}

fn board_access_page_response(
    jar: CookieJar,
    html: String,
    status: StatusCode,
    retry_after_secs: Option<u64>,
) -> Response {
    let mut resp = Html(html).into_response();
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    );
    if let Some(retry_after_secs) = retry_after_secs {
        if let Ok(retry_after) = HeaderValue::from_str(&retry_after_secs.to_string()) {
            resp.headers_mut().insert(header::RETRY_AFTER, retry_after);
        }
    }
    (jar, resp).into_response()
}

pub(crate) fn board_access_required_response(jar: CookieJar, html: String) -> Response {
    board_access_page_response(jar, html, StatusCode::FORBIDDEN, None)
}

pub(crate) fn board_access_ok_response(jar: CookieJar, html: String) -> Response {
    board_access_page_response(jar, html, StatusCode::OK, None)
}

pub(crate) fn board_access_rate_limited_response(
    jar: CookieJar,
    html: String,
    retry_after_secs: u64,
) -> Response {
    board_access_page_response(
        jar,
        html,
        StatusCode::TOO_MANY_REQUESTS,
        Some(retry_after_secs),
    )
}

fn safe_return_to(path: Option<&str>, fallback: &str) -> String {
    crate::utils::redirect::strict_safe_internal_path_or(path, fallback).to_string()
}

pub fn identity_key(client_ip: &str, jar: &CookieJar) -> String {
    if client_ip.starts_with("tor:") {
        return client_ip.to_string();
    }

    if client_ip == "127.0.0.1" || client_ip == "::1" || client_ip == "unknown" {
        if let Some(visitor_id) = jar.get(VISITOR_ID_COOKIE).map(Cookie::value) {
            if !visitor_id.is_empty() {
                return format!("visitor:{visitor_id}");
            }
        }
    }

    client_ip.to_string()
}

fn viewer_preference_key(client_ip: &str, jar: &CookieJar) -> String {
    hash_ip(&identity_key(client_ip, jar), &CONFIG.cookie_secret)
}

fn split_catalog_threads(
    threads: Vec<crate::models::Thread>,
    prefs: &HashMap<i64, db::UserThreadPreference>,
) -> (
    Vec<crate::models::Thread>,
    Vec<crate::models::Thread>,
    HashSet<i64>,
) {
    let mut visible = Vec::new();
    let mut hidden = Vec::new();
    let mut pinned_ids = HashSet::new();

    for thread in threads {
        if let Some(pref) = prefs.get(&thread.id) {
            if pref.pinned {
                pinned_ids.insert(thread.id);
            }
            if pref.hidden {
                hidden.push(thread);
                continue;
            }
        }
        visible.push(thread);
    }

    let sort_threads = |items: &mut Vec<crate::models::Thread>| {
        items.sort_by(|a, b| {
            let a_pinned = pinned_ids.contains(&a.id);
            let b_pinned = pinned_ids.contains(&b.id);
            b_pinned
                .cmp(&a_pinned)
                .then_with(|| b.sticky.cmp(&a.sticky))
                .then_with(|| b.bumped_at.cmp(&a.bumped_at))
        });
    };

    sort_threads(&mut visible);
    sort_threads(&mut hidden);
    (visible, hidden, pinned_ids)
}

// ─── GET / — board list ───────────────────────────────────────────────────────
