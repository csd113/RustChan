use super::{
    board_access_cookie_from_jar, board_access_cookie_name, board_access_ok_response,
    board_access_rate_limited_response, board_access_required_response, board_unlock_attempt_key,
    board_unlock_default_return_to, board_unlock_rate_limit_message, board_unlock_retry_after_secs,
    check_csrf_jar, clear_board_unlock_failures, current_theme_from_jar, db,
    expected_board_access_cookie_value, header, load_board_access_context, new_csrf_token,
    optional_connect_info_peer, record_board_unlock_failure, render_board_unlock_html,
    safe_return_to, sha256_hex, should_set_public_secure_cookie, templates,
    user_preferences_from_jar, verify_password, viewer_preference_key, AppError, AppState,
    BoardAccessContext, Cookie, CookieJar, Duration, Form, HeaderMap, HeaderValue,
    OptionalConnectInfoPeer, Path, Query, Redirect, Response, Result, SameSite,
    SecureCookieContext, State, StatusCode, ADMIN_SESSION_COOKIE, BOARD_ACCESS_COOKIE_TTL_DAYS,
    CONFIG, NSFW_CONSENT_COOKIE, USER_ACTIVITY_BADGES_COOKIE, USER_HIDE_NSFW_COOKIE,
    USER_PREFERRED_VIEW_COOKIE, USER_THEME_COOKIE, USER_VIDEO_AUDIO_COOKIE, VISITOR_ID_COOKIE,
};
use axum::http::Uri;
use axum::response::IntoResponse as _;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

/// Owned posts cookie used by this handler.
const OWNED_POSTS_COOKIE: &str = "rustchan_owned_posts";
/// Owned posts cookie max used by this handler.
const OWNED_POSTS_COOKIE_MAX: usize = 16;
/// Owned posts cookie max len used by this handler.
const OWNED_POSTS_COOKIE_MAX_LEN: usize = 3_800;
/// Self delete window secs used by this handler.
pub(crate) const SELF_DELETE_WINDOW_SECS: i64 = 60;
/// Board activity cookie used by this handler.
const BOARD_ACTIVITY_COOKIE: &str = "rustchan_board_activity";
/// Thread activity cookie used by this handler.
const THREAD_ACTIVITY_COOKIE: &str = "rustchan_thread_activity";
/// Activity cookie version used by this handler.
const ACTIVITY_COOKIE_VERSION: &str = "v1";
/// Board activity cookie max used by this handler.
const BOARD_ACTIVITY_COOKIE_MAX: usize = 64;
/// Thread activity cookie max used by this handler.
const THREAD_ACTIVITY_COOKIE_MAX: usize = 96;
/// Activity cookie max len used by this handler.
const ACTIVITY_COOKIE_MAX_LEN: usize = 3_800;
/// Activity cookie max age days used by this handler.
const ACTIVITY_COOKIE_MAX_AGE_DAYS: i64 = 180;
/// Board activity TTL secs used by this handler.
const BOARD_ACTIVITY_TTL_SECS: i64 = 180 * 24 * 60 * 60;
/// Thread activity TTL secs used by this handler.
const THREAD_ACTIVITY_TTL_SECS: i64 = 90 * 24 * 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct OwnedPostGrant {
    pub post_id: i64,
    pub thread_id: i64,
    pub board_short: String,
    pub deletion_token: String,
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize)]
struct OwnedPostsCookiePayload {
    grants: Vec<OwnedPostGrant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoardActivityMarker {
    pub board_id: i64,
    pub seen_thread_created_at: i64,
    pub seen_thread_id: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadActivityMarker {
    pub thread_id: i64,
    pub seen_reply_count: i64,
    pub updated_at: i64,
}

/// Thread activity marker limit used by this handler.
pub(crate) const THREAD_ACTIVITY_MARKER_LIMIT: usize = THREAD_ACTIVITY_COOKIE_MAX;

fn owned_posts_cookie_signature(payload_hex: &str) -> String {
    sha256_hex(format!("{}:owned-posts:{payload_hex}", CONFIG.cookie_secret).as_bytes())
}

fn parse_owned_posts_cookie(value: &str) -> Vec<OwnedPostGrant> {
    if value.len() > OWNED_POSTS_COOKIE_MAX_LEN {
        return Vec::new();
    }
    let Some((payload_hex, signature)) = value.split_once('.') else {
        return Vec::new();
    };
    if owned_posts_cookie_signature(payload_hex) != signature {
        return Vec::new();
    }

    let Ok(payload_bytes) = hex::decode(payload_hex) else {
        return Vec::new();
    };
    let Ok(payload) = serde_json::from_slice::<OwnedPostsCookiePayload>(&payload_bytes) else {
        return Vec::new();
    };
    let now = chrono::Utc::now().timestamp();
    payload
        .grants
        .into_iter()
        .filter(|grant| grant.expires_at > now)
        .collect()
}

fn owned_posts_cookie_value(grants: &[OwnedPostGrant]) -> Option<String> {
    if grants.is_empty() {
        return None;
    }
    let payload = OwnedPostsCookiePayload {
        grants: grants.to_vec(),
    };
    let payload_json = serde_json::to_vec(&payload).ok()?;
    let payload_hex = hex::encode(payload_json);
    let signature = owned_posts_cookie_signature(&payload_hex);
    let value = format!("{payload_hex}.{signature}");
    (value.len() <= OWNED_POSTS_COOKIE_MAX_LEN).then_some(value)
}

fn owned_posts_cookie(grants: &[OwnedPostGrant], secure: bool) -> Option<Cookie<'static>> {
    let value = owned_posts_cookie_value(grants)?;
    let mut cookie = Cookie::new(OWNED_POSTS_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(secure);
    cookie.set_max_age(Duration::minutes(5));
    Some(cookie)
}

fn prune_owned_post_grants_for_cookie(mut grants: Vec<OwnedPostGrant>) -> Vec<OwnedPostGrant> {
    grants.sort_by(|a, b| {
        b.expires_at
            .cmp(&a.expires_at)
            .then_with(|| b.post_id.cmp(&a.post_id))
    });
    grants.truncate(OWNED_POSTS_COOKIE_MAX);
    while owned_posts_cookie_value(&grants).is_none() && !grants.is_empty() {
        grants.pop();
    }
    grants
}

pub(crate) fn owned_post_grants_from_jar(jar: &CookieJar) -> Vec<OwnedPostGrant> {
    jar.get(OWNED_POSTS_COOKIE)
        .map(Cookie::value)
        .map(parse_owned_posts_cookie)
        .unwrap_or_default()
}

pub(crate) fn owned_post_grant_from_jar(
    jar: &CookieJar,
    board_short: &str,
    post_id: i64,
) -> Option<OwnedPostGrant> {
    owned_post_grants_from_jar(jar)
        .into_iter()
        .find(|grant| grant.post_id == post_id && grant.board_short == board_short)
}

#[cfg(test)]
pub(crate) fn remember_owned_post_until(
    jar: CookieJar,
    board_short: &str,
    thread_id: i64,
    post_id: i64,
    deletion_token: &str,
    expires_at: i64,
) -> CookieJar {
    remember_owned_post_until_with_secure(
        jar,
        board_short,
        thread_id,
        post_id,
        deletion_token,
        expires_at,
        CONFIG.https_cookies,
    )
}

pub(crate) fn remember_owned_post_until_with_secure(
    jar: CookieJar,
    board_short: &str,
    thread_id: i64,
    post_id: i64,
    deletion_token: &str,
    expires_at: i64,
    secure: bool,
) -> CookieJar {
    if expires_at <= chrono::Utc::now().timestamp() {
        return forget_owned_post_with_secure(jar, board_short, post_id, secure);
    }

    let mut grants = owned_post_grants_from_jar(&jar)
        .into_iter()
        .filter(|grant| grant.post_id != post_id)
        .collect::<Vec<_>>();
    grants.push(OwnedPostGrant {
        post_id,
        thread_id,
        board_short: board_short.to_owned(),
        deletion_token: deletion_token.to_owned(),
        expires_at,
    });
    let grants = prune_owned_post_grants_for_cookie(grants);

    if let Some(cookie) = owned_posts_cookie(&grants, secure) {
        jar.add(cookie)
    } else {
        jar.remove(Cookie::from(OWNED_POSTS_COOKIE))
    }
}

pub(crate) fn forget_owned_post_with_secure(
    jar: CookieJar,
    board_short: &str,
    post_id: i64,
    secure: bool,
) -> CookieJar {
    let grants = owned_post_grants_from_jar(&jar)
        .into_iter()
        .filter(|grant| !(grant.post_id == post_id && grant.board_short == board_short))
        .collect::<Vec<_>>();
    if let Some(cookie) = owned_posts_cookie(&grants, secure) {
        jar.add(cookie)
    } else {
        jar.remove(Cookie::from(OWNED_POSTS_COOKIE))
    }
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn parse_activity_cookie_entries(raw: &str) -> impl Iterator<Item = &str> {
    raw.strip_prefix(&format!("{ACTIVITY_COOKIE_VERSION}|"))
        .into_iter()
        .flat_map(|payload| payload.split('|'))
        .filter(|entry| !entry.is_empty())
}

fn parse_board_activity_marker(entry: &str, now: i64) -> Option<BoardActivityMarker> {
    let mut parts = entry.split('.');
    let board_id = parts.next()?.parse::<i64>().ok()?;
    let seen_thread_created_at = parts.next()?.parse::<i64>().ok()?;
    let seen_thread_id = parts.next()?.parse::<i64>().ok()?;
    let updated_at = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || board_id <= 0 || updated_at <= 0 {
        return None;
    }
    if now.saturating_sub(updated_at) > BOARD_ACTIVITY_TTL_SECS {
        return None;
    }
    Some(BoardActivityMarker {
        board_id,
        seen_thread_created_at: seen_thread_created_at.max(0),
        seen_thread_id: seen_thread_id.max(0),
        updated_at,
    })
}

fn parse_thread_activity_marker(entry: &str, now: i64) -> Option<ThreadActivityMarker> {
    let mut parts = entry.split('.');
    let thread_id = parts.next()?.parse::<i64>().ok()?;
    let seen_reply_count = parts.next()?.parse::<i64>().ok()?;
    let updated_at = parts.next()?.parse::<i64>().ok()?;
    if parts.next().is_some() || thread_id <= 0 || updated_at <= 0 {
        return None;
    }
    if now.saturating_sub(updated_at) > THREAD_ACTIVITY_TTL_SECS {
        return None;
    }
    Some(ThreadActivityMarker {
        thread_id,
        seen_reply_count: seen_reply_count.max(0),
        updated_at,
    })
}

fn parse_board_activity_cookie(value: &str) -> HashMap<i64, BoardActivityMarker> {
    if value.len() > ACTIVITY_COOKIE_MAX_LEN {
        return HashMap::new();
    }
    let now = now_ts();
    let mut markers = HashMap::new();
    for entry in parse_activity_cookie_entries(value) {
        if let Some(marker) = parse_board_activity_marker(entry, now) {
            markers.insert(marker.board_id, marker);
        }
    }
    markers
}

fn parse_thread_activity_cookie(value: &str) -> HashMap<i64, ThreadActivityMarker> {
    if value.len() > ACTIVITY_COOKIE_MAX_LEN {
        return HashMap::new();
    }
    let now = now_ts();
    let mut markers = HashMap::new();
    for entry in parse_activity_cookie_entries(value) {
        if let Some(marker) = parse_thread_activity_marker(entry, now) {
            markers.insert(marker.thread_id, marker);
        }
    }
    markers
}

fn board_activity_cookie(markers: &[BoardActivityMarker]) -> Option<Cookie<'static>> {
    if markers.is_empty() {
        return None;
    }
    let mut sorted = markers.to_vec();
    sorted.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.board_id.cmp(&a.board_id))
    });
    sorted.truncate(BOARD_ACTIVITY_COOKIE_MAX);
    let payload = sorted
        .iter()
        .map(|marker| {
            format!(
                "{}.{}.{}.{}",
                marker.board_id,
                marker.seen_thread_created_at,
                marker.seen_thread_id,
                marker.updated_at
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    let value = format!("{ACTIVITY_COOKIE_VERSION}|{payload}");
    if value.len() > ACTIVITY_COOKIE_MAX_LEN {
        return None;
    }
    let mut cookie = Cookie::new(BOARD_ACTIVITY_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(ACTIVITY_COOKIE_MAX_AGE_DAYS));
    Some(cookie)
}

fn thread_activity_cookie(markers: &[ThreadActivityMarker]) -> Option<Cookie<'static>> {
    if markers.is_empty() {
        return None;
    }
    let mut sorted = markers.to_vec();
    sorted.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.thread_id.cmp(&a.thread_id))
    });
    sorted.truncate(THREAD_ACTIVITY_COOKIE_MAX);
    let payload = sorted
        .iter()
        .map(|marker| {
            format!(
                "{}.{}.{}",
                marker.thread_id, marker.seen_reply_count, marker.updated_at
            )
        })
        .collect::<Vec<_>>()
        .join("|");
    let value = format!("{ACTIVITY_COOKIE_VERSION}|{payload}");
    if value.len() > ACTIVITY_COOKIE_MAX_LEN {
        return None;
    }
    let mut cookie = Cookie::new(THREAD_ACTIVITY_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(ACTIVITY_COOKIE_MAX_AGE_DAYS));
    Some(cookie)
}

fn activity_cookie_removal(name: &'static str) -> Cookie<'static> {
    let mut cookie = Cookie::from(name);
    cookie.set_path("/");
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(CONFIG.https_cookies);
    cookie
}

pub(crate) fn board_activity_markers_from_jar(
    jar: &CookieJar,
) -> HashMap<i64, BoardActivityMarker> {
    jar.get(BOARD_ACTIVITY_COOKIE)
        .map(Cookie::value)
        .map(parse_board_activity_cookie)
        .unwrap_or_default()
}

pub(crate) fn thread_activity_markers_from_jar(
    jar: &CookieJar,
) -> HashMap<i64, ThreadActivityMarker> {
    jar.get(THREAD_ACTIVITY_COOKIE)
        .map(Cookie::value)
        .map(parse_thread_activity_cookie)
        .unwrap_or_default()
}

pub(crate) fn remember_board_activity(
    jar: CookieJar,
    board_id: i64,
    seen_thread_created_at: i64,
    seen_thread_id: i64,
) -> CookieJar {
    if board_id <= 0 {
        return jar;
    }
    let mut markers = board_activity_markers_from_jar(&jar)
        .into_values()
        .filter(|marker| marker.board_id != board_id)
        .collect::<Vec<_>>();
    markers.push(BoardActivityMarker {
        board_id,
        seen_thread_created_at: seen_thread_created_at.max(0),
        seen_thread_id: seen_thread_id.max(0),
        updated_at: now_ts(),
    });
    if let Some(cookie) = board_activity_cookie(&markers) {
        jar.add(cookie)
    } else {
        jar.remove(activity_cookie_removal(BOARD_ACTIVITY_COOKIE))
    }
}

pub(crate) fn prune_board_activity_markers(
    jar: CookieJar,
    known_board_ids: &HashSet<i64>,
) -> CookieJar {
    let markers = board_activity_markers_from_jar(&jar)
        .into_values()
        .filter(|marker| known_board_ids.contains(&marker.board_id))
        .collect::<Vec<_>>();
    if let Some(cookie) = board_activity_cookie(&markers) {
        jar.add(cookie)
    } else {
        jar.remove(activity_cookie_removal(BOARD_ACTIVITY_COOKIE))
    }
}

pub(crate) fn remember_thread_activity(
    jar: CookieJar,
    thread_id: i64,
    seen_reply_count: i64,
) -> CookieJar {
    if thread_id <= 0 {
        return jar;
    }
    let mut markers = thread_activity_markers_from_jar(&jar)
        .into_values()
        .filter(|marker| marker.thread_id != thread_id)
        .collect::<Vec<_>>();
    markers.push(ThreadActivityMarker {
        thread_id,
        seen_reply_count: seen_reply_count.max(0),
        updated_at: now_ts(),
    });
    if let Some(cookie) = thread_activity_cookie(&markers) {
        jar.add(cookie)
    } else {
        jar.remove(activity_cookie_removal(THREAD_ACTIVITY_COOKIE))
    }
}

pub(crate) fn remember_visible_thread_activity<I>(jar: CookieJar, visible_threads: I) -> CookieJar
where
    I: IntoIterator<Item = (i64, i64)>,
{
    let now = now_ts();
    let mut markers = thread_activity_markers_from_jar(&jar);
    for (thread_id, seen_reply_count) in visible_threads {
        if thread_id <= 0 {
            continue;
        }
        markers.insert(
            thread_id,
            ThreadActivityMarker {
                thread_id,
                seen_reply_count: seen_reply_count.max(0),
                updated_at: now,
            },
        );
    }
    let markers = markers.into_values().collect::<Vec<_>>();
    if let Some(cookie) = thread_activity_cookie(&markers) {
        jar.add(cookie)
    } else {
        jar.remove(activity_cookie_removal(THREAD_ACTIVITY_COOKIE))
    }
}

// CSRF cookie helper
/// Ensure the CSRF token cookie is set. Returns (`updated_jar`, `token_string`).
pub(crate) fn ensure_csrf_with_secure(jar: CookieJar, secure: bool) -> (CookieJar, String) {
    let mut jar = jar;
    if jar.get(VISITOR_ID_COOKIE).is_none() {
        let mut visitor_cookie = Cookie::new(VISITOR_ID_COOKIE, new_csrf_token());
        visitor_cookie.set_http_only(false);
        visitor_cookie.set_same_site(SameSite::Lax);
        visitor_cookie.set_path("/");
        visitor_cookie.set_secure(secure);
        visitor_cookie.set_max_age(Duration::days(365));
        jar = jar.add(visitor_cookie);
    }

    if let Some(cookie) = jar.get("csrf_token") {
        let token = cookie.value().to_owned();
        if !token.is_empty() {
            return (
                jar,
                crate::utils::crypto::make_csrf_form_token(&token, &CONFIG.cookie_secret),
            );
        }
    }
    let token = new_csrf_token();
    let mut cookie = Cookie::new("csrf_token", token.clone());
    // http_only=false is intentional for the double-submit CSRF pattern —
    // the token must be readable by the page so forms can embed it.
    // XSS is mitigated by SameSite=Strict and thorough HTML escaping.
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    cookie.set_secure(secure);
    (
        jar.add(cookie),
        crate::utils::crypto::make_csrf_form_token(&token, &CONFIG.cookie_secret),
    )
}

/// Ensure the public CSRF token cookie is scoped for the current transport.
pub(crate) fn ensure_csrf_for_request(
    jar: CookieJar,
    headers: &HeaderMap,
    context: SecureCookieContext,
) -> (CookieJar, String) {
    ensure_csrf_with_secure(jar, should_set_public_secure_cookie(headers, context))
}

#[derive(serde::Deserialize)]
pub(crate) struct ThemePreferenceQuery {
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(crate) async fn set_theme(
    Path(theme): Path<String>,
    Query(params): Query<ThemePreferenceQuery>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: SecureCookieContext,
) -> Result<Response> {
    let theme = templates::normalize_theme_slug(&theme)
        .ok_or_else(|| AppError::BadRequest("Unknown theme.".into()))?;
    if params
        .csrf
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        check_csrf_jar(&jar, params.csrf.as_deref())?;
    }

    let mut cookie = Cookie::new(USER_THEME_COOKIE, theme.clone());
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(should_set_public_secure_cookie(&headers, secure_context));
    cookie.set_max_age(Duration::days(365));
    let jar = jar.add(cookie);

    if headers
        .get("x-rustchan-background")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "1")
    {
        return Ok((jar, StatusCode::NO_CONTENT).into_response());
    }

    let redirect_to = params
        .return_to
        .as_deref()
        .map(|value| safe_return_to(Some(value), "/"))
        .or_else(|| safe_referer_return_to(&headers))
        .unwrap_or_else(|| "/".to_owned());
    Ok((jar, Redirect::to(&redirect_to)).into_response())
}

#[derive(serde::Deserialize)]
pub(crate) struct UserPreferencesForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub preferences_form: Option<String>,
    pub return_to: Option<String>,
    pub theme: Option<String>,
    pub hide_nsfw_boards_present: Option<String>,
    pub hide_nsfw_boards: Option<String>,
    pub video_audio: Option<String>,
    pub preferred_board_view: Option<String>,
    pub show_activity_badges_present: Option<String>,
    pub show_activity_badges: Option<String>,
}

fn public_preference_cookie(name: &'static str, value: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, value);
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(secure);
    cookie.set_max_age(Duration::days(365));
    cookie
}

pub(crate) async fn set_user_preferences(
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: SecureCookieContext,
    Form(form): Form<UserPreferencesForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;
    let secure_cookie = should_set_public_secure_cookie(&headers, secure_context);

    let existing_preferences = user_preferences_from_jar(&jar);
    let existing_theme = current_theme_from_jar(&jar);
    let submitted_full_form = form.preferences_form.as_deref() == Some("1")
        || (form.theme.is_some()
            && form.video_audio.is_some()
            && form.preferred_board_view.is_some());

    let theme = form
        .theme
        .as_deref()
        .and_then(templates::normalize_theme_slug)
        .or(existing_theme);
    let video_audio = match form.video_audio.as_deref() {
        Some("mute") => "mute",
        Some("on") => "on",
        _ if existing_preferences.video_audio_muted => "mute",
        _ => "on",
    };
    let preferred_board_view = match form.preferred_board_view.as_deref() {
        Some("index") => "index",
        Some("catalog") => "catalog",
        _ if existing_preferences.preferred_board_view.is_catalog() => "catalog",
        _ => "index",
    };
    let update_hide_nsfw = submitted_full_form || form.hide_nsfw_boards_present.is_some();
    let hide_nsfw = if update_hide_nsfw {
        if form.hide_nsfw_boards.as_deref() == Some("1") {
            "1"
        } else {
            "0"
        }
    } else if existing_preferences.hide_nsfw_boards {
        "1"
    } else {
        "0"
    };
    let update_show_badges = submitted_full_form || form.show_activity_badges_present.is_some();
    let show_badges = if update_show_badges {
        if form.show_activity_badges.as_deref() == Some("1") {
            "1"
        } else {
            "0"
        }
    } else if existing_preferences.show_activity_badges {
        "1"
    } else {
        "0"
    };

    let jar = if let Some(theme) = theme {
        jar.add(public_preference_cookie(
            USER_THEME_COOKIE,
            theme,
            secure_cookie,
        ))
    } else {
        jar.remove(Cookie::from(USER_THEME_COOKIE))
    };
    let jar = jar
        .add(public_preference_cookie(
            USER_HIDE_NSFW_COOKIE,
            hide_nsfw.to_owned(),
            secure_cookie,
        ))
        .add(public_preference_cookie(
            USER_VIDEO_AUDIO_COOKIE,
            video_audio.to_owned(),
            secure_cookie,
        ))
        .add(public_preference_cookie(
            USER_PREFERRED_VIEW_COOKIE,
            preferred_board_view.to_owned(),
            secure_cookie,
        ))
        .add(public_preference_cookie(
            USER_ACTIVITY_BADGES_COOKIE,
            show_badges.to_owned(),
            secure_cookie,
        ));

    if headers
        .get("x-rustchan-background")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "1")
    {
        return Ok((jar, StatusCode::NO_CONTENT).into_response());
    }

    let redirect_to = safe_return_to(form.return_to.as_deref(), "/");
    Ok((jar, Redirect::to(&redirect_to)).into_response())
}

fn safe_referer_return_to(headers: &HeaderMap) -> Option<String> {
    let request_host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::uri::Authority>().ok())?;
    let referer = headers.get(header::REFERER)?.to_str().ok()?;
    let uri = referer.parse::<Uri>().ok()?;
    let referer_host = uri.authority()?;
    if !hosts_match_for_same_origin(referer_host.as_str(), request_host.as_str()) {
        return None;
    }
    let path_and_query = uri.path_and_query()?.as_str();
    crate::utils::redirect::is_strict_safe_internal_path(path_and_query)
        .then(|| path_and_query.to_owned())
}

fn hosts_match_for_same_origin(source_host: &str, request_host: &str) -> bool {
    if source_host.eq_ignore_ascii_case(request_host) {
        return true;
    }

    is_loopback_alias(source_host) && is_loopback_alias(request_host)
}

fn is_loopback_alias(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub(crate) async fn serve_theme_css(
    State(state): State<AppState>,
    Path(theme): Path<String>,
) -> Result<Response> {
    let css = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<Option<String>> {
            let conn = pool.get()?;
            db::theme_css_response(&conn, &theme).map_err(Into::into)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let Some(css) = css else {
        return Err(AppError::NotFound("Theme stylesheet not found.".into()));
    };

    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(crate::cache::CACHE_CONTROL_STATIC_SHORT),
            ),
        ],
        css,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
pub(crate) struct NsfwConsentForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub return_to: Option<String>,
}

pub(crate) async fn accept_nsfw(
    jar: CookieJar,
    Form(form): Form<NsfwConsentForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let mut cookie = Cookie::new(NSFW_CONSENT_COOKIE, "1");
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(365));

    let redirect_to = safe_return_to(form.return_to.as_deref(), "/");
    Ok((jar.add(cookie), Redirect::to(&redirect_to)).into_response())
}

#[derive(serde::Deserialize)]
pub(crate) struct BoardUnlockQuery {
    pub return_to: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct BoardUnlockForm {
    pub password: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(crate) async fn board_unlock_page(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(query): Query<BoardUnlockQuery>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: OptionalConnectInfoPeer,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, optional_connect_info_peer(peer));
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
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

    let default_return_to = board_unlock_default_return_to(&access_context.board);
    let return_to = query
        .return_to
        .as_deref()
        .map(|path| safe_return_to(Some(path), &default_return_to))
        .unwrap_or(default_return_to);

    if access_context.can_post {
        return Ok((jar, Redirect::to(&return_to)).into_response());
    }

    let attempt_key = board_unlock_attempt_key(&board_short, &client_ip);
    if let Some(retry_after_secs) = board_unlock_retry_after_secs(&attempt_key) {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            Some(&board_unlock_rate_limit_message(retry_after_secs)),
            current_theme.as_deref(),
        );
        return Ok(board_access_rate_limited_response(
            jar,
            html,
            retry_after_secs,
        ));
    }

    let html = render_board_unlock_html(
        &access_context.board,
        &csrf,
        &return_to,
        None,
        current_theme.as_deref(),
    );
    Ok(board_access_ok_response(jar, html))
}

#[expect(
    clippy::too_many_lines,
    reason = "rate limiting, credential checks, signed access grants, and redirect validation form one request"
)]
pub(crate) async fn unlock_board_access(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: OptionalConnectInfoPeer,
    Form(form): Form<BoardUnlockForm>,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    check_csrf_jar(&jar, form.csrf.as_deref())?;
    let peer = optional_connect_info_peer(peer);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let access_context = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let access_cookie = access_cookie.clone();
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

    let default_return_to = board_unlock_default_return_to(&access_context.board);
    let return_to = form
        .return_to
        .as_deref()
        .map(|path| safe_return_to(Some(path), &default_return_to))
        .unwrap_or(default_return_to);

    if access_context.can_post {
        return Ok((jar, Redirect::to(&return_to)).into_response());
    }

    let attempt_key = board_unlock_attempt_key(&board_short, &client_ip);
    if let Some(retry_after_secs) = board_unlock_retry_after_secs(&attempt_key) {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            Some(&board_unlock_rate_limit_message(retry_after_secs)),
            current_theme.as_deref(),
        );
        return Ok(board_access_rate_limited_response(
            jar,
            html,
            retry_after_secs,
        ));
    }

    let password = form.password;
    if password.chars().count() > 256 {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            Some("Board password must be 256 characters or fewer."),
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    if access_context.board.access_mode.is_password_protected()
        && access_context.board.access_password_hash.is_empty()
    {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            Some("This board is protected, but no password has been configured yet."),
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    let password_hash = access_context.board.access_password_hash.clone();
    let password_valid_result =
        tokio::task::spawn_blocking(move || verify_password(&password, &password_hash))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let password_valid = match password_valid_result {
        Ok(valid) => valid,
        Err(error) => {
            tracing::warn!(
                target: "board",
                board = %board_short,
                %error,
                "Board password hash is invalid"
            );
            let html = render_board_unlock_html(
                &access_context.board,
                &csrf,
                &return_to,
                Some("This board password is misconfigured. Please contact an administrator."),
                current_theme.as_deref(),
            );
            return Ok(board_access_required_response(jar, html));
        }
    };

    if !password_valid {
        record_board_unlock_failure(&attempt_key);
        if let Some(retry_after_secs) = board_unlock_retry_after_secs(&attempt_key) {
            let html = render_board_unlock_html(
                &access_context.board,
                &csrf,
                &return_to,
                Some(&board_unlock_rate_limit_message(retry_after_secs)),
                current_theme.as_deref(),
            );
            return Ok(board_access_rate_limited_response(
                jar,
                html,
                retry_after_secs,
            ));
        }
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            Some("Incorrect board password."),
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    clear_board_unlock_failures(&attempt_key);
    let cookie_name = board_access_cookie_name(&board_short);
    let cookie_value = expected_board_access_cookie_value(
        &board_short,
        &access_context.board.access_password_hash,
    )
    .ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "Missing board access password hash while creating unlock cookie"
        ))
    })?;
    let cookie = board_access_cookie(
        cookie_name,
        cookie_value,
        should_set_public_secure_cookie(&req_headers, peer),
    );
    Ok((jar.add(cookie), Redirect::to(&return_to)).into_response())
}

fn board_access_cookie(cookie_name: String, cookie_value: String, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(cookie_name, cookie_value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(secure);
    cookie.set_max_age(Duration::days(BOARD_ACCESS_COOKIE_TTL_DAYS));
    cookie
}

#[derive(serde::Deserialize)]
pub(crate) struct ThreadPreferenceForm {
    pub thread_id: i64,
    pub board: String,
    pub action: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(crate) async fn update_thread_preference(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<ThreadPreferenceForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let board_from_form = form
        .board
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if board_from_form != board_short {
        return Err(AppError::BadRequest("Board mismatch.".into()));
    }

    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let action = form.action.trim().to_ascii_lowercase();
    let thread_id = form.thread_id;
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Err(AppError::Forbidden(
                    "This board requires a password.".into(),
                ));
            }
            let board = access_context.board;
            let thread = db::get_thread(&conn, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            if thread.board_id != board.id || thread.archived {
                return Err(AppError::NotFound("Thread not found.".into()));
            }

            match action.as_str() {
                "pin" => db::set_thread_pinned(&conn, &viewer_key, thread.id, true)?,
                "unpin" => db::set_thread_pinned(&conn, &viewer_key, thread.id, false)?,
                "hide" => db::set_thread_hidden(&conn, &viewer_key, thread.id, true)?,
                "unhide" => db::set_thread_hidden(&conn, &viewer_key, thread.id, false)?,
                _ => return Err(AppError::BadRequest("Unknown thread action.".into())),
            }
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let redirect_to = form.return_to.as_deref().map_or_else(
        || format!("/{board_short}/catalog"),
        |path| safe_return_to(Some(path), &format!("/{board_short}/catalog")),
    );
    Ok(Redirect::to(&redirect_to).into_response())
}

// POST /report
#[cfg(test)]
mod tests {
    use super::{
        board_activity_markers_from_jar, ensure_csrf_with_secure, owned_post_grants_from_jar,
        owned_posts_cookie, owned_posts_cookie_value, prune_board_activity_markers,
        public_preference_cookie, remember_owned_post_until, set_user_preferences,
        thread_activity_markers_from_jar, OwnedPostGrant, UserPreferencesForm,
        BOARD_ACTIVITY_COOKIE, OWNED_POSTS_COOKIE_MAX_LEN, SELF_DELETE_WINDOW_SECS,
        THREAD_ACTIVITY_COOKIE, USER_HIDE_NSFW_COOKIE, VISITOR_ID_COOKIE,
    };
    use anyhow::{ensure, Context as _, Result as AnyResult};
    use axum::{
        extract::Form,
        http::{header, HeaderMap, HeaderValue},
    };
    use axum_extra::extract::cookie::{Cookie as TestCookie, CookieJar, SameSite};
    use std::collections::HashSet;

    #[test]
    fn owned_posts_cookie_is_host_only_and_scoped_for_same_site_posts() -> AnyResult<()> {
        let cookie = owned_posts_cookie(
            &[OwnedPostGrant {
                post_id: 42,
                thread_id: 7,
                board_short: "test".to_owned(),
                deletion_token: "token".to_owned(),
                expires_at: chrono::Utc::now().timestamp() + SELF_DELETE_WINDOW_SECS,
            }],
            true,
        )
        .context("create owned-posts cookie")?;

        ensure!(cookie.name() == "rustchan_owned_posts");
        ensure!(cookie.path() == Some("/"));
        ensure!(cookie.same_site() == Some(SameSite::Lax));
        ensure!(cookie.http_only() == Some(true));
        ensure!(cookie.secure() == Some(true));
        ensure!(cookie.domain().is_none(), "cookie should remain host-only");
        ensure!(cookie.max_age() == Some(time::Duration::minutes(5)));
        Ok(())
    }

    #[test]
    fn public_preference_cookie_is_durable_and_uses_the_supplied_transport() -> AnyResult<()> {
        let onion_cookie = public_preference_cookie(USER_HIDE_NSFW_COOKIE, "1".to_owned(), false);
        let https_cookie = public_preference_cookie(USER_HIDE_NSFW_COOKIE, "1".to_owned(), true);

        ensure!(onion_cookie.path() == Some("/"));
        ensure!(onion_cookie.same_site() == Some(SameSite::Lax));
        ensure!(onion_cookie.max_age().is_some());
        ensure!(!onion_cookie.secure().unwrap_or(false));
        ensure!(https_cookie.secure().unwrap_or(false));
        Ok(())
    }

    #[tokio::test]
    async fn user_preferences_remain_settable_on_plain_http_onion_transport() -> AnyResult<()> {
        let jar = CookieJar::new().add(TestCookie::new("csrf_token", "csrf123"));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            HeaderValue::from_static(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion",
            ),
        );
        let response = set_user_preferences(
            jar,
            headers,
            crate::middleware::SecureCookieContext::default(),
            Form(UserPreferencesForm {
                csrf: Some("csrf123".to_owned()),
                preferences_form: None,
                return_to: Some("/".to_owned()),
                theme: None,
                hide_nsfw_boards_present: Some("1".to_owned()),
                hide_nsfw_boards: Some("1".to_owned()),
                video_audio: None,
                preferred_board_view: None,
                show_activity_badges_present: None,
                show_activity_badges: None,
            }),
        )
        .await
        .context("set onion preference")?;

        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        ensure!(cookies
            .iter()
            .any(|cookie| cookie.starts_with("rustchan_hide_nsfw=1;")));
        ensure!(cookies.iter().all(|cookie| !cookie.contains("; Secure")));
        Ok(())
    }

    #[test]
    fn owned_posts_cookie_can_be_set_without_secure_for_plain_http_localhost() -> AnyResult<()> {
        let cookie = owned_posts_cookie(
            &[OwnedPostGrant {
                post_id: 42,
                thread_id: 7,
                board_short: "test".to_owned(),
                deletion_token: "token".to_owned(),
                expires_at: chrono::Utc::now().timestamp() + SELF_DELETE_WINDOW_SECS,
            }],
            false,
        )
        .context("create plain-HTTP owned-posts cookie")?;

        ensure!(cookie.secure() == Some(false));
        ensure!(cookie.http_only() == Some(true));
        ensure!(cookie.same_site() == Some(SameSite::Lax));
        Ok(())
    }

    #[test]
    fn board_access_cookie_secure_attribute_follows_request() {
        let insecure_cookie = super::board_access_cookie(
            "rustchan_board_access_test".to_owned(),
            "grant".to_owned(),
            false,
        );
        assert_eq!(insecure_cookie.name(), "rustchan_board_access_test");
        assert_eq!(insecure_cookie.path(), Some("/"));
        assert_eq!(insecure_cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(insecure_cookie.http_only(), Some(true));
        assert_eq!(insecure_cookie.secure(), Some(false));

        let secure_cookie = super::board_access_cookie(
            "rustchan_board_access_test".to_owned(),
            "grant".to_owned(),
            true,
        );
        assert_eq!(secure_cookie.secure(), Some(true));
    }

    #[test]
    fn ensure_csrf_with_secure_applies_transport_to_public_cookies() -> AnyResult<()> {
        let (insecure_jar, _) = ensure_csrf_with_secure(CookieJar::new(), false);
        let insecure_visitor = insecure_jar
            .get(VISITOR_ID_COOKIE)
            .context("find insecure visitor cookie")?;
        let insecure_csrf = insecure_jar
            .get("csrf_token")
            .context("find insecure CSRF cookie")?;
        ensure!(insecure_visitor.secure() == Some(false));
        ensure!(insecure_visitor.same_site() == Some(SameSite::Lax));
        ensure!(insecure_csrf.secure() == Some(false));
        ensure!(insecure_csrf.same_site() == Some(SameSite::Strict));

        let (secure_jar, _) = ensure_csrf_with_secure(CookieJar::new(), true);
        let secure_visitor = secure_jar
            .get(VISITOR_ID_COOKIE)
            .context("find secure visitor cookie")?;
        let secure_csrf = secure_jar
            .get("csrf_token")
            .context("find secure CSRF cookie")?;
        ensure!(secure_visitor.secure() == Some(true));
        ensure!(secure_csrf.secure() == Some(true));
        Ok(())
    }

    #[test]
    fn expired_owned_post_replay_does_not_mint_fresh_grant() {
        let jar = remember_owned_post_until(
            CookieJar::new(),
            "test",
            1,
            2,
            "token",
            chrono::Utc::now().timestamp() - 1,
        );

        assert!(owned_post_grants_from_jar(&jar).is_empty());
    }

    #[test]
    fn owned_posts_cookie_prunes_to_browser_safe_size() -> AnyResult<()> {
        let mut jar = CookieJar::new();
        let now = chrono::Utc::now().timestamp();
        for id in 1..=32 {
            jar = remember_owned_post_until(
                jar,
                "very-long-board-name-for-cookie-pressure",
                10_000 + id,
                id,
                &"x".repeat(64),
                now + id,
            );
        }

        let cookie = jar
            .get("rustchan_owned_posts")
            .context("find pruned owned-posts cookie")?;
        ensure!(cookie.value().len() <= OWNED_POSTS_COOKIE_MAX_LEN);

        let grants = owned_post_grants_from_jar(&jar);
        ensure!(!grants.is_empty());
        ensure!(grants.len() < 16);
        ensure!(grants.first().map(|grant| grant.post_id) == Some(32));
        ensure!(grants.iter().any(|grant| grant.post_id == 32));
        Ok(())
    }

    #[test]
    fn owned_posts_cookie_value_uses_cookie_safe_ascii() -> AnyResult<()> {
        let value = owned_posts_cookie_value(&[OwnedPostGrant {
            post_id: 42,
            thread_id: 7,
            board_short: "test".to_owned(),
            deletion_token: "token".to_owned(),
            expires_at: chrono::Utc::now().timestamp() + SELF_DELETE_WINDOW_SECS,
        }])
        .context("encode owned-posts cookie value")?;

        ensure!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'.'),
            "owned-post cookie value should stay in conservative Safari-safe bytes"
        );
        Ok(())
    }

    #[test]
    fn malformed_owned_posts_cookie_entries_are_ignored_safely() {
        for value in [
            "not-signed".to_owned(),
            "zz.bad-signature".to_owned(),
            "x".repeat(4_096),
        ] {
            let jar = CookieJar::new().add(axum_extra::extract::cookie::Cookie::new(
                "rustchan_owned_posts",
                value,
            ));

            assert!(owned_post_grants_from_jar(&jar).is_empty());
        }
    }

    #[test]
    fn owned_posts_cookie_pruning_keeps_newest_valid_grants() {
        let mut jar = CookieJar::new();
        let now = chrono::Utc::now().timestamp();
        for id in 1..=20 {
            jar = remember_owned_post_until(jar, "test", 100 + id, id, "token", now + id);
        }

        let grants = owned_post_grants_from_jar(&jar);

        assert_eq!(grants.len(), 16);
        assert!(grants.iter().any(|grant| grant.post_id == 20));
        assert!(grants.iter().any(|grant| grant.post_id == 5));
        assert!(!grants.iter().any(|grant| grant.post_id == 4));
    }

    #[test]
    fn malformed_activity_cookies_are_ignored_safely() {
        let jar = CookieJar::new()
            .add(axum_extra::extract::cookie::Cookie::new(
                BOARD_ACTIVITY_COOKIE,
                "v2|1.2.3.4",
            ))
            .add(axum_extra::extract::cookie::Cookie::new(
                THREAD_ACTIVITY_COOKIE,
                "v1|bad-entry",
            ));

        assert!(board_activity_markers_from_jar(&jar).is_empty());
        assert!(thread_activity_markers_from_jar(&jar).is_empty());
    }

    #[test]
    fn oversized_and_stale_activity_cookies_are_dropped() {
        let oversized = "x".repeat(4_096);
        let stale_ts = chrono::Utc::now().timestamp() - (200 * 24 * 60 * 60);
        let jar = CookieJar::new()
            .add(axum_extra::extract::cookie::Cookie::new(
                BOARD_ACTIVITY_COOKIE,
                oversized,
            ))
            .add(axum_extra::extract::cookie::Cookie::new(
                THREAD_ACTIVITY_COOKIE,
                format!("v1|7.3.{stale_ts}"),
            ));

        assert!(board_activity_markers_from_jar(&jar).is_empty());
        assert!(thread_activity_markers_from_jar(&jar).is_empty());
    }

    #[test]
    fn pruning_board_activity_cookie_removes_unknown_board_ids() {
        let now = chrono::Utc::now().timestamp();
        let jar = CookieJar::new().add(axum_extra::extract::cookie::Cookie::new(
            BOARD_ACTIVITY_COOKIE,
            format!("v1|1.10.20.{now}|2.10.21.{now}"),
        ));
        let pruned = prune_board_activity_markers(jar, &HashSet::from([2_i64]));
        let markers = board_activity_markers_from_jar(&pruned);

        assert_eq!(markers.len(), 1);
        assert!(markers.contains_key(&2));
        assert!(!markers.contains_key(&1));
    }
}
