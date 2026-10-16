use super::{
    board_access_cookie_days, board_access_cookie_from_jar, board_access_cookie_name,
    board_access_ok_response, board_access_rate_limited_response, board_access_required_response,
    board_unlock_attempt_key, board_unlock_default_return_to, board_unlock_rate_limit_message,
    board_unlock_retry_after_secs, check_csrf_jar, clear_board_unlock_failures,
    current_theme_from_jar, db, expected_board_access_cookie_value, header,
    load_board_access_context, new_csrf_token, record_board_unlock_failure,
    render_board_unlock_html, safe_return_to, sha256_hex, should_set_public_secure_cookie,
    templates, user_preferences_from_jar, verify_password, viewer_preference_key, AppError,
    AppState, BoardAccessContext, Cookie, CookieJar, Duration, Form, HeaderMap, HeaderValue, Path,
    Query, Redirect, Response, Result, SameSite, SecureCookieContext, State, StatusCode,
    ADMIN_SESSION_COOKIE, CONFIG, NSFW_CONSENT_COOKIE, USER_ACTIVITY_BADGES_COOKIE,
    USER_HIDE_NSFW_COOKIE, USER_PREFERRED_VIEW_COOKIE, USER_THEME_COOKIE, USER_VIDEO_AUDIO_COOKIE,
    VISITOR_ID_COOKIE,
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
pub(in crate::server::handlers) fn self_action_window_secs() -> i64 {
    CONFIG.operator.self_action_window_secs
}

/// Bound self-action grants without extending a malformed restored timestamp on overflow.
pub(in crate::server::handlers) fn self_action_expiry(created_at: i64) -> Result<i64> {
    created_at
        .checked_add(self_action_window_secs())
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("self-action expiry timestamp overflow")))
}
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
pub(in crate::server::handlers) struct OwnedPostGrant {
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
pub(super) struct BoardActivityMarker {
    pub board_id: i64,
    pub seen_thread_created_at: i64,
    pub seen_thread_id: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ThreadActivityMarker {
    pub thread_id: i64,
    pub seen_reply_count: i64,
    pub updated_at: i64,
}

/// Thread activity marker limit used by this handler.
pub(super) const THREAD_ACTIVITY_MARKER_LIMIT: usize = THREAD_ACTIVITY_COOKIE_MAX;

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
        let _previous_value = grants.pop();
    }
    grants
}

pub(in crate::server::handlers) fn owned_post_grants_from_jar(
    jar: &CookieJar,
) -> Vec<OwnedPostGrant> {
    jar.get(OWNED_POSTS_COOKIE)
        .map(Cookie::value)
        .map(parse_owned_posts_cookie)
        .unwrap_or_default()
}

pub(in crate::server::handlers) fn owned_post_grant_from_jar(
    jar: &CookieJar,
    board_short: &str,
    post_id: i64,
) -> Option<OwnedPostGrant> {
    owned_post_grants_from_jar(jar)
        .into_iter()
        .find(|grant| grant.post_id == post_id && grant.board_short == board_short)
}

pub(in crate::server::handlers) fn remember_owned_post_until_with_secure(
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

pub(in crate::server::handlers) fn forget_owned_post_with_secure(
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
            let _previous_value = markers.insert(marker.board_id, marker);
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
            let _previous_value = markers.insert(marker.thread_id, marker);
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

pub(super) fn board_activity_markers_from_jar(
    jar: &CookieJar,
) -> HashMap<i64, BoardActivityMarker> {
    jar.get(BOARD_ACTIVITY_COOKIE)
        .map(Cookie::value)
        .map(parse_board_activity_cookie)
        .unwrap_or_default()
}

pub(super) fn thread_activity_markers_from_jar(
    jar: &CookieJar,
) -> HashMap<i64, ThreadActivityMarker> {
    jar.get(THREAD_ACTIVITY_COOKIE)
        .map(Cookie::value)
        .map(parse_thread_activity_cookie)
        .unwrap_or_default()
}

pub(in crate::server::handlers) fn remember_board_activity(
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

pub(super) fn prune_board_activity_markers(
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

pub(in crate::server::handlers) fn remember_thread_activity(
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

pub(super) fn remember_visible_thread_activity<I>(jar: CookieJar, visible_threads: I) -> CookieJar
where
    I: IntoIterator<Item = (i64, i64)>,
{
    let now = now_ts();
    let mut markers = thread_activity_markers_from_jar(&jar);
    for (thread_id, seen_reply_count) in visible_threads {
        if thread_id <= 0 {
            continue;
        }
        let _previous_value = markers.insert(
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
pub(in crate::server::handlers) fn ensure_csrf_with_secure(
    jar: CookieJar,
    secure: bool,
) -> (CookieJar, String) {
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
pub(in crate::server) fn ensure_csrf_for_request(
    jar: CookieJar,
    headers: &HeaderMap,
    context: SecureCookieContext,
) -> (CookieJar, String) {
    ensure_csrf_with_secure(jar, should_set_public_secure_cookie(headers, context))
}

#[derive(serde::Deserialize)]
pub(in crate::server) struct ThemePreferenceQuery {
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(in crate::server) async fn set_theme(
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

    let mut cookie = Cookie::new(USER_THEME_COOKIE, theme);
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
pub(in crate::server) struct UserPreferencesForm {
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

pub(in crate::server) async fn set_user_preferences(
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

pub(in crate::server) async fn serve_theme_css(
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
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        css,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
pub(in crate::server) struct NsfwConsentForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub return_to: Option<String>,
}

pub(in crate::server) async fn accept_nsfw(
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
pub(in crate::server) struct BoardUnlockQuery {
    pub return_to: Option<String>,
}

#[derive(serde::Deserialize)]
pub(in crate::server) struct BoardUnlockForm {
    pub password: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(in crate::server) async fn board_unlock_page(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(query): Query<BoardUnlockQuery>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: SecureCookieContext,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
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
pub(in crate::server) async fn unlock_board_access(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: SecureCookieContext,
    Form(form): Form<BoardUnlockForm>,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    check_csrf_jar(&jar, form.csrf.as_deref())?;
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

    let permit = state.password_work_gate.try_begin()?;
    if record_board_unlock_failure(&attempt_key) > super::board_password_fail_limit() {
        return Err(AppError::DbBusy);
    }
    let password_hash = access_context.board.access_password_hash.clone();
    let password_valid_result = tokio::task::spawn_blocking(move || {
        let _password_permit = permit;
        verify_password(&password, &password_hash)
    })
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
    cookie.set_max_age(Duration::days(board_access_cookie_days()));
    cookie
}

#[derive(serde::Deserialize)]
pub(in crate::server) struct ThreadPreferenceForm {
    pub thread_id: i64,
    pub board: String,
    pub action: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

/// Validated personal thread action; none of these changes global moderation state.
#[derive(Clone, Copy)]
enum ThreadPreferenceAction {
    /// Set the personal pin.
    Pin,
    /// Clear the personal pin.
    Unpin,
    /// Hide from active board listings.
    Hide,
    /// Restore to active board listings.
    Unhide,
}

impl ThreadPreferenceAction {
    /// Reject unknown actions before entering a write transaction.
    fn parse(value: &str) -> Result<Self> {
        match value {
            "pin" => Ok(Self::Pin),
            "unpin" => Ok(Self::Unpin),
            "hide" => Ok(Self::Hide),
            "unhide" => Ok(Self::Unhide),
            _ => Err(AppError::BadRequest("Unknown thread action.".into())),
        }
    }
}

pub(in crate::server) async fn update_thread_preference(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: SecureCookieContext,
    Form(form): Form<ThreadPreferenceForm>,
) -> Result<Response> {
    super::check_menu_action_csrf(&jar, &req_headers, peer, form.csrf.as_deref())?;

    if form.board != board_short || !super::valid_action_board(&board_short) {
        return Err(AppError::BadRequest("Board mismatch.".into()));
    }
    if form.thread_id <= 0 {
        return Err(AppError::BadRequest("Invalid thread ID.".into()));
    }

    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let action = ThreadPreferenceAction::parse(&form.action)?;
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
            // Keep access/target validation, both flags, and default-row cleanup
            // atomic with archiving, deletion, and another preference action.
            let tx = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let access_context = load_board_access_context(
                &tx,
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
            let thread = db::get_thread(&tx, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            if thread.board_id != board.id {
                return Err(AppError::NotFound("Thread not found.".into()));
            }
            if thread.archived
                && matches!(
                    action,
                    ThreadPreferenceAction::Pin | ThreadPreferenceAction::Hide
                )
            {
                return Err(AppError::Conflict("This thread is archived.".into()));
            }

            match action {
                ThreadPreferenceAction::Pin => {
                    db::set_thread_pinned(&tx, &viewer_key, thread.id, true)?;
                }
                ThreadPreferenceAction::Unpin => {
                    db::set_thread_pinned(&tx, &viewer_key, thread.id, false)?;
                }
                ThreadPreferenceAction::Hide => {
                    db::set_thread_hidden(&tx, &viewer_key, thread.id, true)?;
                }
                ThreadPreferenceAction::Unhide => {
                    db::set_thread_hidden(&tx, &viewer_key, thread.id, false)?;
                }
            }
            tx.commit()?;
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
