// Routes:
//   GET  /                    — board list
//   GET  /:board/             — board index (thread list)
//   POST /:board/             — create new thread
//   GET  /:board/catalog      — catalog view
//   GET  /:board/search       — search results
//   POST /delete              — user post deletion

/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers::board");

use crate::{
    config::CONFIG,
    db::{self},
    error::{AppError, Result},
    handlers::{is_xml_http_request, parse_post_multipart, posting, render, PostFormData},
    middleware::{validate_csrf, validate_signed_csrf, AppState, SecureCookieContext},
    models::{Board, Pagination, SearchQuery, SEARCH_QUERY_MAX_CHARS},
    templates,
    utils::crypto::{hash_ip, make_scoped_csrf_form_token, new_csrf_token, sha256_hex},
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use time::Duration;

mod access;
mod access_preferences;
mod catalog;
mod create_thread;
mod media;
mod pages;
mod reports;

pub(in crate::server) use access::*;
pub(in crate::server) use access_preferences::*;
pub(in crate::server) use catalog::*;
pub(in crate::server) use create_thread::*;
pub(in crate::server) use media::*;
pub(in crate::server) use pages::*;
pub(in crate::server) use reports::*;

pub(super) fn should_set_public_secure_cookie(
    headers: &HeaderMap,
    context: SecureCookieContext,
) -> bool {
    crate::handlers::admin::should_set_secure_cookie(headers, context)
}

/// Preview replies used by this handler.
fn index_reply_previews() -> i64 {
    CONFIG.operator.index_reply_previews
}
/// Threads per page used by this handler.
fn index_threads_per_page() -> i64 {
    CONFIG.operator.index_threads_per_page
}
/// User theme cookie used by this handler.
const USER_THEME_COOKIE: &str = "rustchan_theme";
/// User hide NSFW cookie used by this handler.
const USER_HIDE_NSFW_COOKIE: &str = "rustchan_hide_nsfw";
/// User video audio cookie used by this handler.
const USER_VIDEO_AUDIO_COOKIE: &str = "rustchan_video_audio";
/// User preferred view cookie used by this handler.
const USER_PREFERRED_VIEW_COOKIE: &str = "rustchan_preferred_view";
/// User activity badges cookie used by this handler.
const USER_ACTIVITY_BADGES_COOKIE: &str = "rustchan_activity_badges";
/// NSFW consent cookie used by this handler.
const NSFW_CONSENT_COOKIE: &str = "rustchan_nsfw_ok";
/// Visitor ID cookie used by this handler.
const VISITOR_ID_COOKIE: &str = "rustchan_visitor_id";
/// Admin session cookie used by this handler.
pub(in crate::server) const ADMIN_SESSION_COOKIE: &str = "chan_admin_session";
/// HTML cache control used by this handler.
const HTML_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_DYNAMIC_PUBLIC;
/// X rustchan redirect header used by this handler.
const X_RUSTCHAN_REDIRECT_HEADER: &str = "x-rustchan-redirect";

type CatalogRenderData = (
    Board,
    Vec<crate::models::Thread>,
    HashSet<i64>,
    usize,
    String,
);

pub(super) fn latest_visible_thread_marker_tuple(marker: Option<(i64, i64)>) -> (i64, i64) {
    marker.unwrap_or((0, 0))
}

pub(super) const fn activity_html_cache_control(activity_markers_enabled: bool) -> &'static str {
    if activity_markers_enabled {
        crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE
    } else {
        HTML_CACHE_CONTROL
    }
}

/// Personal thread state must never be stored in a shared cache by IP identity.
pub(super) const fn personal_html_cache_control(activity_markers_enabled: bool) -> &'static str {
    if activity_markers_enabled {
        crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE
    } else {
        crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE
    }
}

fn thread_unread_counts(
    threads: &[crate::models::Thread],
    markers: &HashMap<i64, ThreadActivityMarker>,
) -> HashMap<i64, i64> {
    threads
        .iter()
        .filter_map(|thread| {
            let marker = markers.get(&thread.id)?;
            let unread = thread
                .reply_count
                .saturating_sub(marker.seen_reply_count)
                .max(0);
            (unread > 0).then_some((thread.id, unread))
        })
        .collect()
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
    drop(response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    ));
    Ok(response)
}

fn xhr_json_error_response(
    response_status: StatusCode,
    error_status: StatusCode,
    message: &str,
) -> Result<Response> {
    let mut response = json_response(response_status, &XhrErrorPayload { error: message })?;
    drop(
        response.headers_mut().insert(
            "x-rustchan-error-status",
            HeaderValue::from_str(error_status.as_str())
                .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
        ),
    );
    if error_status == StatusCode::SERVICE_UNAVAILABLE {
        drop(
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1")),
        );
    }
    Ok(response)
}

pub(super) fn xhr_error_response(status: StatusCode, message: &str) -> Result<Response> {
    xhr_json_error_response(status, status, message)
}

// Browsers log `Failed to load resource` for XHR/fetch responses with 4xx/5xx
// statuses even when the page handles the JSON error inline. For validation
// errors that the UI already renders in-context, keep the transport successful
// and expose the original HTTP meaning via `X-Rustchan-Error-Status`.
pub(super) fn xhr_handled_error_response(status: StatusCode, message: &str) -> Result<Response> {
    xhr_json_error_response(StatusCode::OK, status, message)
}

pub(super) fn xhr_redirect_response(target: &str) -> Result<Response> {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    drop(
        response.headers_mut().insert(
            X_RUSTCHAN_REDIRECT_HEADER,
            HeaderValue::from_str(target)
                .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
        ),
    );
    Ok(response)
}

fn banned_page_redirect_url(reason: &str) -> String {
    let reason_for_url = reason.chars().take(256).collect::<String>();
    format!(
        "/banned?reason={}",
        templates::urlencoding_simple(&reason_for_url)
    )
}

pub(super) fn xhr_post_error_response(error: AppError) -> Result<Response> {
    match error {
        AppError::NotFound(message) => xhr_handled_error_response(StatusCode::NOT_FOUND, &message),
        AppError::BadRequest(message) => {
            xhr_handled_error_response(StatusCode::UNPROCESSABLE_ENTITY, &message)
        }
        AppError::Forbidden(message) => xhr_handled_error_response(StatusCode::FORBIDDEN, &message),
        AppError::BannedUser {
            reason,
            csrf_token: _,
        } => xhr_redirect_response(&banned_page_redirect_url(&reason)),
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
            tracing::error!(target: LOG_TARGET, "Internal error during XHR post submission: {:?}", error);
            xhr_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "An internal error occurred.",
            )
        }
        AppError::Tls(message) => {
            tracing::error!(target: LOG_TARGET, "TLS error during XHR post submission: {message}");
            xhr_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "A TLS configuration error occurred.",
            )
        }
    }
}

pub(super) fn handled_post_error_status(
    error: AppError,
) -> std::result::Result<(StatusCode, String), AppError> {
    match error {
        AppError::BadRequest(message) => Ok((StatusCode::UNPROCESSABLE_ENTITY, message)),
        AppError::UploadTooLarge(message) => Ok((StatusCode::PAYLOAD_TOO_LARGE, message)),
        AppError::InvalidMediaType(message) => Ok((StatusCode::UNSUPPORTED_MEDIA_TYPE, message)),
        other @ (AppError::NotFound(_)
        | AppError::Forbidden(_)
        | AppError::BannedUser {
            reason: _,
            csrf_token: _,
        }
        | AppError::Conflict(_)
        | AppError::DbBusy
        | AppError::Internal(_)
        | AppError::Tls(_)) => Err(other),
    }
}

#[derive(Deserialize)]
pub(in crate::server) struct BannedPageQuery {
    pub reason: Option<String>,
}

pub(super) fn current_theme_from_jar(jar: &CookieJar) -> Option<String> {
    let cookie = jar.get(USER_THEME_COOKIE)?;
    templates::normalize_theme_slug(cookie.value())
}

pub(in crate::server) fn user_preferences_from_jar(jar: &CookieJar) -> templates::UserPreferences {
    let default_preferences = templates::UserPreferences::default();
    templates::UserPreferences {
        hide_nsfw_boards: jar
            .get(USER_HIDE_NSFW_COOKIE)
            .map_or(default_preferences.hide_nsfw_boards, |cookie| {
                cookie.value() == "1"
            }),
        video_audio_muted: jar
            .get(USER_VIDEO_AUDIO_COOKIE)
            .is_some_and(|cookie| cookie.value() == "mute"),
        preferred_board_view: match jar.get(USER_PREFERRED_VIEW_COOKIE).map(Cookie::value) {
            Some("index") => templates::PreferredBoardView::Index,
            Some("catalog") => templates::PreferredBoardView::Catalog,
            _ => default_preferences.preferred_board_view,
        },
        show_activity_badges: jar
            .get(USER_ACTIVITY_BADGES_COOKIE)
            .map_or(default_preferences.show_activity_badges, |cookie| {
                cookie.value() != "0"
            }),
    }
}

pub(super) fn check_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_owned());
    let admin_session = jar.get("chan_admin_session").map(Cookie::value);
    let form_token = form_token.unwrap_or("");
    if validate_csrf(csrf_cookie.as_deref(), form_token)
        || validate_signed_csrf(csrf_cookie.as_deref(), admin_session, form_token)
    {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

/// Bind menu actions to the visitor cookie and reject cross-site signed-token replay.
/// Cookie-disabled browsers may use a signed token only with same-origin evidence.
pub(super) fn check_menu_action_csrf(
    jar: &CookieJar,
    headers: &HeaderMap,
    peer: SecureCookieContext,
    form_token: Option<&str>,
) -> Result<()> {
    check_csrf_jar(jar, form_token)?;
    let token = form_token.unwrap_or("");
    let cookie = jar.get("csrf_token").map(Cookie::value);
    let raw = token
        .rsplit_once('.')
        .map_or(token, |(raw, _signature)| raw);
    let cookie_bound = cookie.is_some_and(|cookie| cookie == token || cookie == raw);
    if cookie.is_some() && !cookie_bound {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }
    let fetch_site = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok());
    if fetch_site.is_some_and(|value| value.eq_ignore_ascii_case("cross-site")) {
        return Err(AppError::Forbidden("Cross-site action denied.".into()));
    }
    let null_origin_without_referer = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        == Some("null")
        && !headers.contains_key(header::REFERER);
    if null_origin_without_referer {
        // The site's no-referrer policy makes native form POSTs send Origin:
        // null in supported browsers. Fetch metadata or a bound Strict cookie
        // distinguishes these from a cross-site signed-token replay.
        if cookie_bound || fetch_site.is_some_and(|value| value.eq_ignore_ascii_case("same-origin"))
        {
            return Ok(());
        }
        return Err(AppError::Forbidden("Cross-site action denied.".into()));
    }
    crate::handlers::admin::require_same_origin_or_valid_csrf(headers, peer.peer, cookie_bound)
}

pub(super) fn admin_scoped_csrf_token(
    jar: &CookieJar,
    admin_session_id: Option<&str>,
    is_admin: bool,
) -> Option<String> {
    if !is_admin {
        return None;
    }
    let raw = jar
        .get("csrf_token")
        .map(Cookie::value)
        .filter(|value| !value.is_empty())?;
    let session_id = admin_session_id.filter(|value| !value.is_empty())?;
    Some(make_scoped_csrf_form_token(
        raw,
        &CONFIG.cookie_secret,
        session_id,
    ))
}

fn has_nsfw_consent(jar: &CookieJar) -> bool {
    jar.get(NSFW_CONSENT_COOKIE)
        .is_some_and(|cookie| cookie.value() == "1")
}

/// Normalize a stored ban reason for display.
fn normalized_ban_reason(reason: String) -> String {
    if reason.is_empty() {
        "No reason given".to_owned()
    } else {
        reason
    }
}

/// Return the current ban reason for an actor identity, if the identity is
/// banned.
///
/// This is the single source of truth for the ban policy shared by the public
/// participation routes. A banned identity is refused with
/// [`AppError::BannedUser`], which renders the standalone ban notice (and its
/// appeal form) instead of performing the action. Reading remains allowed, and
/// the appeal route stays available so a banned identity can ask for review.
pub(super) fn actor_ban_reason(
    conn: &rusqlite::Connection,
    ip_hash: &str,
) -> Result<Option<String>> {
    Ok(db::is_banned(conn, ip_hash)?.map(normalized_ban_reason))
}

/// Refuse a banned actor's participation request with the ban notice.
///
/// `ban_csrf_token` is the request's CSRF token, embedded so the rendered
/// appeal form can submit. Callers must have validated CSRF before calling
/// this, otherwise an attacker could use the ban page as an oracle.
pub(super) fn ensure_actor_not_banned(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    ban_csrf_token: String,
) -> Result<()> {
    if let Some(reason) = actor_ban_reason(conn, ip_hash)? {
        return Err(AppError::BannedUser {
            reason,
            csrf_token: ban_csrf_token,
        });
    }
    Ok(())
}

pub(in crate::server) async fn banned_page(
    Query(query): Query<BannedPageQuery>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: SecureCookieContext,
) -> Response {
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let reason = query
        .reason
        .unwrap_or_else(|| "No reason given".to_owned())
        .trim()
        .chars()
        .take(512)
        .collect::<String>();
    let reason = if reason.is_empty() {
        "No reason given".to_owned()
    } else {
        reason
    };
    let theme = current_theme_from_jar(&jar);
    let html = templates::ban_page_with_theme(&reason, &csrf, theme.as_deref(), None);
    (jar, Html(html)).into_response()
}

fn safe_return_to(path: Option<&str>, fallback: &str) -> String {
    crate::utils::redirect::strict_safe_internal_path_or(path, fallback).to_owned()
}

/// Accept only a complete board identifier, never a lossily sanitized target.
pub(super) fn valid_action_board(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

pub(super) fn identity_key(client_ip: &str, jar: &CookieJar) -> String {
    if client_ip.starts_with("tor:") {
        return client_ip.to_owned();
    }

    if client_ip == "127.0.0.1" || client_ip == "::1" || client_ip == "unknown" {
        if let Some(visitor_id) = jar.get(VISITOR_ID_COOKIE).map(Cookie::value) {
            if !visitor_id.is_empty() {
                return format!("visitor:{visitor_id}");
            }
        }
    }

    client_ip.to_owned()
}

fn viewer_preference_key(client_ip: &str, jar: &CookieJar) -> String {
    hash_ip(&identity_key(client_ip, jar), &CONFIG.cookie_secret)
}

// GET / — board list
