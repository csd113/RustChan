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
    utils::{
        crypto::{hash_ip, new_csrf_token, sha256_hex, verify_password, verify_pow},
        sanitize::validate_subject,
    },
};
use axum::{
    extract::{Form, Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{atomic::AtomicU64, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};
use time::Duration;

const PREVIEW_REPLIES: i64 = 3;
const THREADS_PER_PAGE: i64 = 10;
pub const USER_THEME_COOKIE: &str = "rustchan_theme";
pub const NSFW_CONSENT_COOKIE: &str = "rustchan_nsfw_ok";
pub const VISITOR_ID_COOKIE: &str = "rustchan_visitor_id";
pub(crate) const ADMIN_SESSION_COOKIE: &str = "chan_admin_session";
const BOARD_ACCESS_COOKIE_PREFIX: &str = "rustchan_board_access_";
const BOARD_ACCESS_COOKIE_TTL_DAYS: i64 = 30;
const BOARD_UNLOCK_FAIL_LIMIT: u32 = 5;
const BOARD_UNLOCK_FAIL_WINDOW_SECS: u64 = 900;
const HTML_CACHE_CONTROL: &str = "private, no-cache, must-revalidate";

static BOARD_UNLOCK_FAILS: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);
static BOARD_UNLOCK_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

pub struct BoardAccessContext {
    pub board: Board,
    pub is_admin: bool,
    pub can_view: bool,
    pub can_post: bool,
}

type CatalogRenderData = (
    Board,
    Vec<crate::models::Thread>,
    HashSet<i64>,
    usize,
    String,
);

fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

pub fn current_theme_from_jar(jar: &CookieJar) -> Option<String> {
    jar.get(USER_THEME_COOKIE)
        .and_then(|cookie| crate::templates::normalize_theme_slug(cookie.value()))
}

pub fn check_csrf_jar(jar: &CookieJar, form_token: Option<&str>) -> Result<()> {
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if validate_csrf(csrf_cookie.as_deref(), form_token.unwrap_or("")) {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
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
        || !board.access_mode.requires_post_password()
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
        HeaderValue::from_static(HTML_CACHE_CONTROL),
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

fn safe_return_to(path: &str) -> &str {
    if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") {
        path
    } else {
        "/"
    }
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

pub async fn index(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let nsfw_consent = has_nsfw_consent(&jar);

    let admin_session = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let (board_stats, site_data, is_admin) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Vec<crate::models::BoardStats>, crate::models::SiteStats, bool)> {
            let conn = pool.get()?;
            let boards = db::get_all_boards_with_stats(&conn)?;
            let site_data = db::get_site_stats(&conn).unwrap_or_default();
            let is_admin = admin_session
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            Ok((boards, site_data, is_admin))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Read the onion address from AppState (populated by the Arti task on startup).
    let onion_address: Option<String> = if crate::config::CONFIG.enable_tor_support {
        state.onion_address.read().await.clone()
    } else {
        None
    };
    let nsfw_prompt_board = params
        .get("nsfw")
        .and_then(|short| board_stats.iter().find(|s| s.board.short_name == *short))
        .map(|s| &s.board);

    if nsfw_consent {
        if let Some(board) = nsfw_prompt_board {
            let redirect_to = if board.access_mode.requires_view_password() {
                format!("/{}/unlock", board.short_name)
            } else {
                format!("/{}/catalog", board.short_name)
            };
            return Ok((jar, Redirect::to(&redirect_to)).into_response());
        }
    }

    Ok((
        jar,
        Html(templates::index_page(
            &board_stats,
            &site_data,
            &csrf,
            onion_address.as_deref(),
            current_theme.as_deref(),
            nsfw_prompt_board,
            nsfw_consent,
            is_admin,
        )),
    )
        .into_response())
}

// ─── GET /:board/ — board index ───────────────────────────────────────────────

#[allow(clippy::arithmetic_side_effects)]
pub async fn board_index(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
    let return_to = if page > 1 {
        format!("/{board_short}?page={page}")
    } else {
        format!("/{board_short}")
    };

    let access_context = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let admin_session_id = admin_session_id.clone();
        let board_short = board_short.clone();
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

    if !access_context.can_view {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            None,
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    let page_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        move || -> Result<(String, render::BoardPageData)> {
            let conn = pool.get()?;
            let page_data = render::load_board_page_data(
                &conn,
                &board_short,
                page,
                THREADS_PER_PAGE,
                PREVIEW_REPLIES,
                admin_session_id.as_deref(),
            )?;
            Ok((render::board_page_etag_signature(&page_data), page_data))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let can_post = access_context.can_post;
    let (page_sig, page_data) = page_data;
    let admin_tag = if page_data.is_admin { "-a" } else { "" };
    let post_tag = if can_post { "-p1" } else { "-p0" };
    let greentext_tag = if page_data.board.collapse_greentext {
        "-cg1"
    } else {
        "-cg0"
    };
    let etag = format!(
        "\"{}-{}-{page}{admin_tag}{post_tag}{greentext_tag}\"",
        page_data.pagination.total, page_sig
    );

    // 3.2: Return 304 Not Modified when the client's cached version is current.
    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if client_etag == etag {
        // StatusCode::NOT_MODIFIED and Body::empty() are always valid constants;
        // this builder call is infallible.
        let mut resp = axum::http::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        resp.headers_mut().insert(
            "etag",
            axum::http::HeaderValue::from_str(&etag)
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("\"0\"")),
        );
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(HTML_CACHE_CONTROL),
        );
        return Ok((jar, resp).into_response());
    }

    let html =
        render::render_board_page(&page_data, &csrf, None, current_theme.as_deref(), can_post);
    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(HTML_CACHE_CONTROL),
    );
    Ok((jar, resp).into_response())
}

// ─── POST /:board/ — create new thread ───────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn create_thread(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
    multipart: Multipart,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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

    if !access_context.can_post {
        let redirect_to = unlock_redirect_url(&board_short, &format!("/{board_short}"));
        return Ok(Redirect::to(&redirect_to).into_response());
    }

    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    let form = parse_post_multipart(multipart, csrf_cookie.as_deref()).await?;

    if !form.csrf_verified {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let raw_body = form.body;

    let upload_dir = CONFIG.upload_dir.clone();
    let thumb_size = CONFIG.thumb_size;
    let max_image_size = CONFIG.max_image_size;
    let max_video_size = CONFIG.max_video_size;
    let max_audio_size = CONFIG.max_audio_size;
    let ffmpeg_available = state.ffmpeg_available;
    let ffmpeg_webp_available = state.ffmpeg_webp_available;
    let cookie_secret = CONFIG.cookie_secret.clone();
    let file_data = form.file;
    let audio_file_data = form.audio_file;
    let image_file_data = form.image_file;
    let name_val = form.name;
    let subject_val = form.subject;
    let del_token_val = form.deletion_token;
    let submission_token = form.submission_token;
    let poll_question = form.poll_question;
    let poll_options = form.poll_options;
    let poll_duration = form.poll_duration_secs;
    let pow_nonce = form.pow_nonce;

    // Also extract csrf_token before spawn_blocking so the ban page appeal form works.
    let ban_csrf_token = csrf_cookie.clone().unwrap_or_default();

    // Clones kept outside the closure so we can re-render the board page inline on error.
    let admin_session_err = admin_session_id.clone();
    let csrf_for_error = csrf_cookie.clone().unwrap_or_default();

    let board_short_err = board_short.clone();
    let identity_key = identity_key(&client_ip, &jar);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let job_queue = state.job_queue.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let ip_hash = hash_ip(&identity_key, &cookie_secret);
            if let Some(reason) = db::is_banned(&conn, &ip_hash)? {
                return Err(AppError::BannedUser {
                    reason: if reason.is_empty() {
                        "No reason given".to_string()
                    } else {
                        reason
                    },
                    csrf_token: ban_csrf_token,
                });
            }
            if let Some(existing) =
                db::get_post_submission(&conn, &submission_token, &ip_hash, board.id)?
            {
                return Ok(format!(
                    "/{}/thread/{}#p{}",
                    board_short, existing.thread_id, existing.post_id
                ));
            }

            // Verify admin session — admins bypass the per-board cooldown entirely.
            let is_admin = posting::is_admin_session(&conn, admin_session_id.as_deref());

            // Per-board post cooldown — the SOLE post rate control.
            // post_cooldown_secs = 0 means no cooldown at all; admins always bypass it.
            if board.post_cooldown_secs > 0 && !is_admin {
                let elapsed = db::get_seconds_since_last_post(&conn, board.id, &ip_hash)?;
                if let Some(secs) = elapsed {
                    let remaining = board.post_cooldown_secs.saturating_sub(secs);
                    if remaining > 0 {
                        return Err(AppError::BadRequest(format!("Please wait {remaining} more second{} before posting again.", if remaining == 1 { "" } else { "s" })));
                    }
                }
            }

            // PoW CAPTCHA — verified only when the board has it enabled
            if board.allow_captcha && !verify_pow(&board_short, &pow_nonce) {
                return Err(AppError::BadRequest(
                    "CAPTCHA verification failed. Please wait for the solver to complete before posting.".into()
                ));
            }

            let filters = posting::load_word_filters(&conn)?;
            let (name, tripcode) = posting::resolve_post_identity(&name_val, board.allow_tripcodes);
            let subject = validate_subject(&subject_val);

            let board_allows_media = board.allow_images
                || board.allow_video
                || board.allow_audio
                || (crate::config::CONFIG.enable_any_file_uploads_feature && board.allow_any_files);
            let has_file = file_data.is_some() || audio_file_data.is_some() || image_file_data.is_some();
            let (body_text, body_html) =
                posting::build_post_body(
                    &raw_body,
                    has_file,
                    board_allows_media,
                    board.collapse_greentext,
                    &filters,
                )?;

            let uploads = posting::process_uploads(
                image_file_data,
                file_data,
                audio_file_data,
                &board,
                &conn,
                &posting::UploadConfig {
                    upload_dir: &upload_dir,
                    thumb_size,
                    max_image_size,
                    max_video_size,
                    max_audio_size,
                    ffmpeg_available,
                    ffmpeg_webp_available,
                },
            )?;

            let deletion_token = posting::resolve_deletion_token(&del_token_val);

            // Thread creation and OP post insertion are now
            // wrapped in a single transaction via create_thread_with_op.
            // Previously, a crash between the two calls left an orphaned thread.
            let new_post = posting::build_new_post(
                0,
                board.id,
                name,
                tripcode,
                subject.clone(),
                body_text.clone(),
                body_html,
                ip_hash.clone(),
                &uploads,
                deletion_token,
                true,
            );
            let q = poll_question.trim().to_string();
            let pending_upload_op = posting::build_pending_upload_op(&uploads)?;
            let valid_opts: Vec<String> = poll_options
                .iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .collect();
            let poll_insert = if !q.is_empty() && valid_opts.len() >= 2 {
                let secs = poll_duration.ok_or_else(|| {
                    AppError::BadRequest("A duration is required when creating a poll.".into())
                })?;
                let secs = secs.clamp(60, 30 * 24 * 3600); // clamp 1 min..30 days
                let expires_at = chrono::Utc::now().timestamp().saturating_add(secs);
                Some(db::threads::PollInsert {
                    question: &q,
                    options: &valid_opts,
                    expires_at,
                })
            } else {
                None
            };
            let create_result = db::create_thread_with_optional_poll(
                &conn,
                board.id,
                subject.as_deref(),
                &new_post,
                &submission_token,
                poll_insert.as_ref(),
                pending_upload_op.as_ref(),
            );
            let (thread_id, post_id, _) = match create_result {
                Ok(ids) => ids,
                Err(error) => {
                    uploads.rollback_new_files(&conn, &upload_dir)?;
                    return Err(error.into());
                }
            };
            posting::finalize_pending_uploads(&conn, &upload_dir, &uploads);

            // ── Background jobs ───────────────────────────────────────────────
            // 1 & 2. Media post-processing + spam check (shared helper)
            crate::handlers::enqueue_post_jobs(
                &job_queue,
                &conn,
                post_id,
                &ip_hash,
                body_text.len(),
                uploads.primary.as_ref(),
                &board.short_name,
            );

            // 3. Thread pruning — now async so HTTP response returns immediately.
            let max_threads = board.max_threads;
            let _ = job_queue.enqueue(&crate::workers::Job::ThreadPrune {
                board_id: board.id,
                board_short: board.short_name.clone(),
                max_threads,
                max_archived_threads: board.max_archived_threads,
                allow_archive: board.allow_archive,
            });

            tracing::info!(
                target: "board",
                board = %board.short_name,
                thread_id = thread_id,
                "Created new thread"
            );
            Ok(format!("/{}/thread/{thread_id}", board.short_name))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    // BadRequest → re-render the board page with an inline error banner so the
    // user sees the message in context without being sent to a separate error page.
    let redirect_url = match result {
        Ok(url) => url,
        Err(AppError::BadRequest(msg)) => {
            let board_short_render = board_short_err.clone();
            let pool = state.db.clone();
            let current_theme = current_theme.clone();
            let html = tokio::task::spawn_blocking(move || -> Result<String> {
                let conn = pool.get()?;
                let page_data = render::load_board_page_data(
                    &conn,
                    &board_short_render,
                    1,
                    THREADS_PER_PAGE,
                    PREVIEW_REPLIES,
                    admin_session_err.as_deref(),
                )?;
                Ok(render::render_board_page(
                    &page_data,
                    &csrf_for_error,
                    Some(&msg),
                    current_theme.as_deref(),
                    true,
                ))
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

            let mut resp = Html(html).into_response();
            *resp.status_mut() = axum::http::StatusCode::UNPROCESSABLE_ENTITY;
            return Ok(resp);
        }
        Err(e) => return Err(e),
    };

    if is_xml_http_request(&req_headers) {
        let mut resp = Response::new(axum::body::Body::empty());
        *resp.status_mut() = StatusCode::NO_CONTENT;
        resp.headers_mut().insert(
            "x-rustchan-redirect",
            HeaderValue::from_str(&redirect_url)
                .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?,
        );
        return Ok(resp);
    }

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── GET /:board/catalog ──────────────────────────────────────────────────────

pub async fn catalog(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: HeaderMap,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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

    if !access_context.can_view {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &format!("/{board_short}/catalog"),
            None,
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    // Add ETag caching to the catalog. Previously every request
    // fetched up to 200 full thread rows and re-rendered the entire page
    // regardless of whether anything changed. The ETag is derived from the
    // most-recently-bumped thread, mirroring the board index handler.
    let catalog_data = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        let admin_session_id = admin_session_id.clone();
        let viewer_key = viewer_key.clone();
        move || -> Result<CatalogRenderData> {
            let conn = pool.get()?;
            let access_context =
                load_board_access_context(&conn, &board_short, admin_session_id.as_deref(), None)?;
            let board = access_context.board;
            let all_threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let prefs = db::get_preferences_for_board(&conn, &viewer_key, board.id)?;
            let (threads, hidden_threads, pinned_ids) = split_catalog_threads(all_threads, &prefs);
            let catalog_sig = threads
                .iter()
                .map(|thread| {
                    format!(
                        "{}:{}:{}:{}",
                        thread.id,
                        thread.bumped_at,
                        i32::from(thread.sticky),
                        i32::from(thread.archived)
                    )
                })
                .collect::<Vec<_>>()
                .join("|");
            let mut pref_sig_parts = prefs
                .iter()
                .map(|(thread_id, pref)| {
                    format!(
                        "{thread_id}:{}:{}",
                        i32::from(pref.pinned),
                        i32::from(pref.hidden)
                    )
                })
                .collect::<Vec<_>>();
            pref_sig_parts.sort();
            let pref_sig = pref_sig_parts.join("|");
            let etag_signature = format!("{catalog_sig}-{pref_sig}");
            Ok((
                board,
                threads,
                pinned_ids,
                hidden_threads.len(),
                etag_signature,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let can_post = access_context.can_post;
    let (board, threads, pinned_ids, hidden_count, etag_signature) = catalog_data;
    let admin_tag = if access_context.is_admin { "-a" } else { "" };
    let post_tag = if can_post { "-p1" } else { "-p0" };
    let greentext_tag = if board.collapse_greentext {
        "-cg1"
    } else {
        "-cg0"
    };
    let etag = format!("\"{etag_signature}-catalog{admin_tag}{post_tag}{greentext_tag}\"");

    let client_etag = req_headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if client_etag == etag {
        // StatusCode::NOT_MODIFIED and Body::empty() are always valid constants;
        // this builder call is infallible.
        let mut resp = axum::http::Response::builder()
            .status(axum::http::StatusCode::NOT_MODIFIED)
            .body(axum::body::Body::empty())
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        resp.headers_mut().insert(
            "etag",
            axum::http::HeaderValue::from_str(&etag)
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("\"0\"")),
        );
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(HTML_CACHE_CONTROL),
        );
        return Ok((jar, resp).into_response());
    }

    let all_boards = crate::templates::live_boards();
    let html = templates::catalog_page(
        &board,
        &threads,
        &pinned_ids,
        hidden_count,
        false,
        &csrf,
        all_boards.as_slice(),
        access_context.is_admin,
        current_theme.as_deref(),
        board.collapse_greentext,
        can_post,
    );
    let mut resp = Html(html).into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&etag) {
        resp.headers_mut().insert("etag", v);
    }
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(HTML_CACHE_CONTROL),
    );
    Ok((jar, resp).into_response())
}

pub async fn hidden_threads(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let viewer_key = viewer_preference_key(&client_ip, &jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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

    if !access_context.can_view {
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &format!("/{board_short}/hidden"),
            None,
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let all_threads = db::get_threads_for_board(&conn, board.id, 200, 0)?;
            let prefs = db::get_preferences_for_board(&conn, &viewer_key, board.id)?;
            let (_visible, hidden_threads, pinned_ids) = split_catalog_threads(all_threads, &prefs);

            let all_boards = crate::templates::live_boards();
            Ok(templates::catalog_page(
                &board,
                &hidden_threads,
                &pinned_ids,
                hidden_threads.len(),
                true,
                &csrf,
                all_boards.as_slice(),
                access_context.is_admin,
                current_theme.as_deref(),
                board.collapse_greentext,
                access_context.can_post,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── GET /:board/archive ──────────────────────────────────────────────────────

pub async fn board_archive(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
) -> Result<Response> {
    const ARCHIVE_PER_PAGE: i64 = 20;
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    let page: i64 = params
        .get("page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
        .max(1);
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

    if !access_context.can_view {
        let return_to = if page > 1 {
            format!("/{board_short}/archive?page={page}")
        } else {
            format!("/{board_short}/archive")
        };
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            None,
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            if !board.allow_archive {
                return Err(AppError::NotFound(format!(
                    "/{board_short}/ does not have an archive."
                )));
            }

            let total = db::count_archived_threads_for_board(&conn, board.id)?;
            let pagination = Pagination::new(page, ARCHIVE_PER_PAGE, total);
            let threads = db::get_archived_threads_for_board(
                &conn,
                board.id,
                ARCHIVE_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = crate::templates::live_boards();
            Ok(templates::archive_page(
                &board,
                &threads,
                &pagination,
                &csrf_clone,
                all_boards.as_slice(),
                current_theme.as_deref(),
                board.collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── GET /:board/search ───────────────────────────────────────────────────────

pub async fn search(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(q): Query<SearchQuery>,
    jar: CookieJar,
) -> Result<Response> {
    const SEARCH_PER_PAGE: i64 = 20;
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);

    // Cap query length to prevent excessively large LIKE pattern scans.
    let query_str: String = q.q.trim().chars().take(SEARCH_QUERY_MAX_CHARS).collect();
    let page = q.page.max(1);
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

    if !access_context.can_view {
        let mut return_to = format!(
            "/{board_short}/search?q={}",
            crate::templates::urlencoding_simple(&query_str)
        );
        if page > 1 {
            return_to.push_str(&format!("&page={page}"));
        }
        let html = render_board_unlock_html(
            &access_context.board,
            &csrf,
            &return_to,
            None,
            current_theme.as_deref(),
        );
        return Ok(board_access_required_response(jar, html));
    }

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;

            let total = db::count_search_results(&conn, board.id, &query_str)?;
            let pagination = Pagination::new(page, SEARCH_PER_PAGE, total);
            let posts = db::search_posts(
                &conn,
                board.id,
                &query_str,
                SEARCH_PER_PAGE,
                pagination.offset(),
            )?;

            let all_boards = crate::templates::live_boards();
            Ok(templates::search_page(
                &board,
                &query_str,
                &posts,
                &pagination,
                &csrf_clone,
                all_boards.as_slice(),
                current_theme.as_deref(),
                board.collapse_greentext,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

// ─── CSRF cookie helper ───────────────────────────────────────────────────────

/// Ensure the CSRF token cookie is set. Returns (`updated_jar`, `token_string`).
pub fn ensure_csrf(jar: CookieJar) -> (CookieJar, String) {
    let mut jar = jar;
    if jar.get(VISITOR_ID_COOKIE).is_none() {
        let mut visitor_cookie = Cookie::new(VISITOR_ID_COOKIE, new_csrf_token());
        visitor_cookie.set_http_only(false);
        visitor_cookie.set_same_site(SameSite::Lax);
        visitor_cookie.set_path("/");
        visitor_cookie.set_secure(CONFIG.https_cookies);
        visitor_cookie.set_max_age(Duration::days(365));
        jar = jar.add(visitor_cookie);
    }

    if let Some(cookie) = jar.get("csrf_token") {
        let token = cookie.value().to_string();
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
    // set Secure flag based on config (true when behind proxy / HTTPS)
    cookie.set_secure(CONFIG.https_cookies);
    (
        jar.add(cookie),
        crate::utils::crypto::make_csrf_form_token(&token, &CONFIG.cookie_secret),
    )
}

pub async fn set_theme(
    Path(theme): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Result<Response> {
    let theme = crate::templates::normalize_theme_slug(&theme)
        .ok_or_else(|| AppError::BadRequest("Unknown theme.".into()))?;

    let mut cookie = Cookie::new(USER_THEME_COOKIE, theme.to_string());
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(365));
    let jar = jar.add(cookie);

    if headers
        .get("x-rustchan-background")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == "1")
    {
        return Ok((jar, axum::http::StatusCode::NO_CONTENT).into_response());
    }

    let redirect_to = params
        .get("return_to")
        .map(String::as_str)
        .map(safe_return_to)
        .or_else(|| {
            headers
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or("/");
    Ok((jar, Redirect::to(redirect_to)).into_response())
}

pub async fn serve_theme_css(
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
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        css,
    )
        .into_response())
}

#[derive(serde::Deserialize)]
pub struct NsfwConsentForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub return_to: Option<String>,
}

pub async fn accept_nsfw(jar: CookieJar, Form(form): Form<NsfwConsentForm>) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let mut cookie = Cookie::new(NSFW_CONSENT_COOKIE, "1");
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(365));

    let redirect_to = form.return_to.as_deref().map_or("/", safe_return_to);
    Ok((jar.add(cookie), Redirect::to(redirect_to)).into_response())
}

#[derive(serde::Deserialize)]
pub struct BoardUnlockQuery {
    pub return_to: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct BoardUnlockForm {
    pub password: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn board_unlock_page(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    Query(query): Query<BoardUnlockQuery>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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

    let return_to = query
        .return_to
        .as_deref()
        .map(safe_return_to)
        .map(str::to_string)
        .unwrap_or_else(|| board_unlock_default_return_to(&access_context.board));

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

pub async fn unlock_board_access(
    State(state): State<AppState>,
    Path(board_short): Path<String>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<BoardUnlockForm>,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    check_csrf_jar(&jar, form.csrf.as_deref())?;
    let (jar, csrf) = ensure_csrf(jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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

    let return_to = form
        .return_to
        .as_deref()
        .map(safe_return_to)
        .map(str::to_string)
        .unwrap_or_else(|| board_unlock_default_return_to(&access_context.board));

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

    if access_context.board.access_mode.requires_post_password()
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
    let mut cookie = Cookie::new(cookie_name, cookie_value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::days(BOARD_ACCESS_COOKIE_TTL_DAYS));
    Ok((jar.add(cookie), Redirect::to(&return_to)).into_response())
}

#[derive(serde::Deserialize)]
pub struct ThreadPreferenceForm {
    pub thread_id: i64,
    pub board: String,
    pub action: String,
    pub return_to: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn update_thread_preference(
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
        .map(|cookie| cookie.value().to_string());
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
        |path| safe_return_to(path).to_string(),
    );
    Ok(Redirect::to(&redirect_to).into_response())
}

// ─── POST /report ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct ReportForm {
    pub post_id: i64,
    #[allow(dead_code)]
    pub thread_id: i64,
    pub board: String,
    pub reason: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn file_report(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<ReportForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = form
        .reason
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    let post_id = form.post_id;
    let board_raw = form
        .board
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_raw);

    let board_raw_closure = board_raw.clone();
    let db_thread_id = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<i64> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_raw_closure,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Err(AppError::Forbidden(
                    "This board requires a password.".into(),
                ));
            }
            let board = access_context.board;
            // Verify post exists and belongs to this board to prevent spoofed reports.
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.board_id != board.id {
                return Err(AppError::BadRequest(
                    "Post does not belong to this board.".into(),
                ));
            }
            // Use the DB's thread_id for the redirect — not the user-submitted value.
            let authoritative_thread_id = post.thread_id;
            db::file_report(&conn, post_id, &reason, &ip_hash)?;
            Ok(authoritative_thread_id)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Redirect back to the thread using the DB-resolved IDs.
    // `board_raw` is already sanitised to alphanumeric earlier in this handler.
    Ok(Redirect::to(&format!(
        "/{board_raw}/thread/{db_thread_id}#p{}",
        form.post_id
    ))
    .into_response())
}

// ─── GET /boards/{*media_path} — serve media with mp4→webm redirect ──────────
//

// ─── Content-Type helper for board media ─────────────────────────────────────

/// Return the correct `Content-Type` value for a board media file based solely
/// on its extension.  Used to override whatever `mime_guess` / `ServeFile`
/// produces, because some builds of `mime_guess` do not include `.webp`,
/// `.svg`, or audio formats in their database and fall back to
/// `application/octet-stream`, which causes browsers to download the file
/// rather than display or play it inline.
fn media_content_type(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ico") => Some("image/x-icon"),
        Some("webp") => Some("image/webp"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("bmp") => Some("image/bmp"),
        Some("tiff" | "tif") => Some("image/tiff"),
        // SVG is intentionally omitted: serving SVG inline allows stored XSS via
        // embedded <script> tags. SVGs are not accepted as uploads (detect_mime_type
        // rejects image/svg+xml) so this arm would never match, but the explicit
        // absence here documents the security decision.
        Some("webm") => Some("video/webm"),
        Some("mp4") => Some("video/mp4"),
        Some("mp3") => Some("audio/mpeg"),
        Some("ogg") => Some("audio/ogg"),
        Some("flac") => Some("audio/flac"),
        Some("wav") => Some("audio/wav"),
        Some("m4a") => Some("audio/mp4"),
        Some("aac") => Some("audio/aac"),
        _ => None,
    }
}

// Replaces the former nest_service(ServeDir) so we can intercept stale .mp4

// links (created before the background transcoder replaced them with .webm)
// and issue a permanent redirect. All other paths are served via ServeFile.

pub async fn serve_board_media(
    State(state): State<AppState>,
    Path(media_path): Path<String>,
    jar: CookieJar,
    req: axum::extract::Request,
) -> Response {
    use axum::http::header::CACHE_CONTROL;
    use axum::http::StatusCode;
    use std::path::PathBuf;
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    // Reject path-traversal attempts and absolute-path escapes.
    if media_path.contains("..") || media_path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let Some(board_short) = media_path.split('/').next().filter(|part| !part.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, board_short);
    let access_context = match tokio::task::spawn_blocking({
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
    {
        Ok(Ok(context)) => context,
        Ok(Err(AppError::NotFound(_))) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(_)) | Err(_) => return StatusCode::FORBIDDEN.into_response(),
    };

    if !access_context.can_view {
        return StatusCode::FORBIDDEN.into_response();
    }

    let base = PathBuf::from(&CONFIG.upload_dir);
    let target = base.join(&media_path);
    let has_version = req
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part.starts_with("v=")));
    let is_board_favicon = std::path::Path::new(&media_path)
        .components()
        .nth(1)
        .is_some_and(|part| part.as_os_str() == "_favicon");

    // Verify the resolved path is still inside the upload directory.
    // This catches any edge cases that slip past the string checks above
    // (e.g. symlinks, exotic percent-encoding handled by the OS).
    if !target.starts_with(&base) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    if target.exists() {
        // File present — forward the real request (with Range, ETag, etc.) to
        // ServeFile so it can respond with 206 Partial Content when needed.
        // iOS Safari requires Range request support to play video — dropping
        // the request headers caused it to receive 200 instead of 206 and
        // refuse playback on videos it tried to stream in chunks.
        let req = req.map(|_| axum::body::Body::empty());
        ServeFile::new(&target).oneshot(req).await.map_or_else(
            |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            |resp| {
                use axum::http::header::{
                    HeaderValue, CONTENT_DISPOSITION, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS,
                };
                let mut resp = resp.map(axum::body::Body::new);
                if is_board_favicon {
                    resp.headers_mut().insert(
                        CACHE_CONTROL,
                        HeaderValue::from_static(board_media_cache_control(has_version)),
                    );
                }
                if let Some(ct) = media_content_type(&target) {
                    resp.headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
                } else {
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/octet-stream"),
                    );
                    resp.headers_mut()
                        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
                    let filename = target
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("download.bin")
                        .replace(['\\', '"'], "_");
                    if let Ok(value) =
                        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    {
                        resp.headers_mut().insert(CONTENT_DISPOSITION, value);
                    }
                }
                resp.into_response()
            },
        )
    } else if std::path::Path::new(&media_path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        // MP4 was transcoded away — redirect permanently to the .webm sibling.
        let webm_path_str = format!("{}.webm", &media_path[..media_path.len().saturating_sub(4)]);
        let webm_abs = base.join(&webm_path_str);
        if webm_abs.exists() {
            Redirect::permanent(&format!("/boards/{webm_path_str}")).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

const fn board_media_cache_control(has_version: bool) -> &'static str {
    if has_version {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache, must-revalidate"
    }
}

// ─── GET /api/post/{board}/{post_id} ──────────────────────────────────────────
//
// Lightweight JSON endpoint for cross-board quotelink hover previews.
//
// `post_id` is the **global** post ID (the AUTOINCREMENT primary key of the
// `posts` table).  The board name is used only to validate ownership — a link
// like >>>/tech/12345 will 404 if post 12345 actually lives on /b/, preventing
// cross-board information leakage.
//
// Response on success:
//   { "html": "<div class=\"post …\">…</div>", "thread_id": 42 }
// The `thread_id` field lets the client update the link's href to the canonical
// /{board}/thread/{thread_id}#p{post_id} URL after the first hover.
//
// Response on failure: 404 { "error": "not found" }

pub async fn api_post_preview(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> crate::error::Result<Option<(String, i64)>> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Ok(None);
            }

            // Fetch the post, validating it belongs to this board.
            let board = access_context.board;
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            match post {
                None => Ok(None),
                Some(p) => {
                    let thread_id = p.thread_id;
                    let html = crate::templates::render_post(
                        &p,
                        &board_short,
                        "",
                        crate::templates::thread::RenderPostOpts {
                            show_delete: false,
                            is_admin: false,
                            show_media: true,
                            allow_editing: false, // no edit link in read-only preview
                            show_poster_ids: false,
                            collapse_greentext: board.collapse_greentext,
                            thread_state: None,
                            thread_op_id: None,
                        },
                        0, // no edit window
                    );
                    Ok(Some((html, thread_id)))
                }
            }
        }
    })
    .await;

    let json_ct = [(header::CONTENT_TYPE, "application/json")];

    match result {
        Ok(Ok(Some((html, thread_id)))) => {
            let body =
                serde_json::to_string(&serde_json::json!({ "html": html, "thread_id": thread_id }))
                    .unwrap_or_else(|_| r#"{"html":"","thread_id":0}"#.to_string());
            (axum::http::StatusCode::OK, json_ct, body).into_response()
        }
        Ok(Ok(None)) => {
            let body = r#"{"error":"not found"}"#.to_string();
            (axum::http::StatusCode::NOT_FOUND, json_ct, body).into_response()
        }
        _ => {
            let body = r#"{"error":"internal error"}"#.to_string();
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, json_ct, body).into_response()
        }
    }
}

// ─── GET /{board}/post/{post_id} ──────────────────────────────────────────────
//
// Canonical redirect for `>>>/board/N` links.  Resolves the global post ID to
// its containing thread and issues a 302 to /{board}/thread/{thread_id}#p{post_id}.
//
// Users clicking a cross-board quotelink land here on the first click; after
// the first hover preview the JS upgrades the href in-place so subsequent
// clicks go directly to the thread anchor without a server round-trip.

pub async fn redirect_to_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    use axum::response::Redirect;

    let board_short_for_url = board_short.clone();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<(Option<i64>, bool)> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Ok((None, true));
            }
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            Ok((post.map(|p| p.thread_id), false))
        }
    })
    .await;

    if let Ok(Ok((Some(thread_id), _))) = result {
        let url = format!("/{board_short_for_url}/thread/{thread_id}#p{post_id}");
        Redirect::to(&url).into_response()
    } else if let Ok(Ok((None, true))) = result {
        Redirect::to(&unlock_redirect_url(
            &board_short_for_url,
            &format!("/{board_short_for_url}/post/{post_id}"),
        ))
        .into_response()
    } else {
        // Post not found or wrong board — render the error page template
        // so the user gets a readable message instead of a blank HTTP 404.
        // This is the fallback path when JavaScript is disabled or when
        // a user manually navigates to a quotelink URL after a board
        // restore that assigned new IDs to the restored posts.
        let html = crate::templates::error_page(
            404,
            &format!("Post #{post_id} not found. It may have been deleted or the board was restored from a backup."),
        );
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::response::Html(html),
        )
            .into_response()
    }
}

// ─── POST /appeal ─────────────────────────────────────────────────────────────
// Banned users submit a brief appeal message here.
// Appeals appear in the admin panel under // ban appeals.

#[derive(serde::Deserialize)]
pub struct AppealForm {
    pub reason: String,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn submit_appeal(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> impl axum::response::IntoResponse {
    use axum::response::Html;

    if check_csrf_jar(&jar, form.csrf.as_deref()).is_err() {
        return Html(crate::templates::error_page(403, "CSRF token mismatch.")).into_response();
    }

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return Html(crate::templates::error_page(
            400,
            "Appeal message cannot be empty.",
        ))
        .into_response();
    }

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<db::BanAppealSubmission> {
            let conn = pool.get()?;
            Ok(db::file_ban_appeal(&conn, &ip_hash, &reason)?)
        }
    })
    .await;

    let msg = match result {
        Ok(Ok(db::BanAppealSubmission::Filed)) => {
            "Your appeal has been submitted. An admin will review it."
        }
        Ok(Ok(db::BanAppealSubmission::AlreadyFiled)) => {
            "You have already filed an appeal in the last 24 hours."
        }
        Ok(Ok(db::BanAppealSubmission::NotBanned)) => "Your IP is not currently banned.",
        _ => "An error occurred. Please try again.",
    };

    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Appeal Submitted</title>
<link rel="stylesheet" href="{stylesheet_href}">
</head><body><div class="page-box error-page">
<h1>appeal submitted</h1>
<p>{msg}</p>
<p><a href="/">return home</a></p>
</div></body></html>"#,
        stylesheet_href = crate::templates::static_asset_url("/static/style.css"),
        msg = crate::utils::sanitize::escape_html(msg)
    );
    Html(html).into_response()
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    #[test]
    fn protected_board_without_password_hash_fails_closed() {
        let board = crate::models::Board {
            access_mode: crate::models::BoardAccessMode::ViewPassword,
            access_password_hash: String::new(),
            ..crate::test_fixtures::sample_board()
        };
        assert!(!super::can_view_board(&board, false, None));
        assert!(!super::can_post_to_board(&board, false, None));
    }

    #[tokio::test]
    async fn search_returns_results_without_500() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            let board_id =
                crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
            let post = crate::db::NewPost {
                thread_id: 0,
                board_id,
                name: "anon".to_string(),
                tripcode: None,
                subject: Some("subject".to_string()),
                body: "rust search body".to_string(),
                body_html: "rust search body".to_string(),
                ip_hash: None,
                file_path: None,
                file_name: None,
                file_size: None,
                thumb_path: None,
                mime_type: None,
                media_type: None,
                audio_file_path: None,
                audio_file_name: None,
                audio_file_size: None,
                audio_mime_type: None,
                deletion_token: "token".to_string(),
                is_op: true,
            };
            crate::db::create_thread_with_optional_poll(
                &conn, board_id, None, &post, "", None, None,
            )
            .expect("create thread");
        }

        let router = Router::new()
            .route("/{board}/search", get(super::search))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/test/search?q=rust")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("rust search body"));
    }

    #[tokio::test]
    async fn search_without_q_param_returns_empty_results_page() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/{board}/search", get(super::search))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/test/search")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("no results found."));
    }

    #[tokio::test]
    async fn locked_board_search_returns_forbidden_unlock_page() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "slock", "Secret", "", false).expect("create board");
            let password_hash =
                crate::utils::crypto::hash_password("swordfish").expect("hash password");
            conn.execute(
                "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'slock'",
                rusqlite::params!["view_password", password_hash],
            )
            .expect("update board access");
        }

        let router = Router::new()
            .route("/{board}/search", get(super::search))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/slock/search?q=rust")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(super::HTML_CACHE_CONTROL)
        );
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("action=\"/slock/unlock\""));
    }

    #[tokio::test]
    async fn create_thread_accepts_valid_multipart_submission() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/{board}", post(super::create_thread))
            .with_state(state.clone());
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "hello world")],
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("location header");
        assert!(location.starts_with("/test/thread/"));
    }

    #[tokio::test]
    async fn create_thread_xhr_returns_explicit_redirect_header() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        }

        let router = Router::new()
            .route("/{board}", post(super::create_thread))
            .with_state(state);
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "hello xhr")],
            None,
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .header("X-Requested-With", "XMLHttpRequest")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let redirect = response
            .headers()
            .get("x-rustchan-redirect")
            .and_then(|value| value.to_str().ok())
            .expect("xhr redirect header");
        assert!(redirect.starts_with("/test/thread/"));
    }

    #[tokio::test]
    async fn create_thread_rejects_uploads_on_upload_disabled_board() {
        let state = crate::test_support::app_state();
        {
            let mut conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
            crate::db::update_board_settings(
                &mut conn,
                1,
                "Test",
                "",
                false,
                500,
                100,
                150,
                false,
                false,
                false,
                false,
                true,
                0,
                false,
                true,
                false,
                false,
                false,
                false,
                0,
                "",
                crate::models::BoardAccessMode::Public,
                "",
            )
            .expect("update board settings");
        }

        let router = Router::new()
            .route("/{board}", post(super::create_thread))
            .with_state(state);
        let (boundary, body) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "file attempt")],
            Some(("file", "image.png", b"\x89PNG\r\n\x1a\n", "image/png")),
        );

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn view_locked_catalog_renders_unlock_page() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
            let password_hash =
                crate::utils::crypto::hash_password("swordfish").expect("hash password");
            conn.execute(
                "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
                rusqlite::params!["view_password", password_hash],
            )
            .expect("update board access");
        }

        let router = Router::new()
            .route("/{board}/catalog", get(super::catalog))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/secret/catalog")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("password protected board"));
        assert!(body.contains("action=\"/secret/unlock\""));
    }

    #[tokio::test]
    async fn unlock_board_access_sets_cookie_and_redirects() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
            let password_hash =
                crate::utils::crypto::hash_password("swordfish").expect("hash password");
            conn.execute(
                "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
                rusqlite::params!["view_password", password_hash],
            )
            .expect("update board access");
        }

        let router = Router::new()
            .route("/{board}/unlock", post(super::unlock_board_access))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/secret/unlock")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(
                        "password=swordfish&return_to=%2Fsecret%2Fcatalog&_csrf=csrf123",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/secret/catalog")
        );
        let set_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(&super::board_access_cookie_name("secret")))
            .expect("board access cookie");
        assert!(set_cookie.contains("HttpOnly"));
    }

    #[tokio::test]
    async fn unlock_board_access_rate_limits_repeated_failures() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "srate", "Secret", "", false).expect("create board");
            let password_hash =
                crate::utils::crypto::hash_password("swordfish").expect("hash password");
            conn.execute(
                "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'srate'",
                rusqlite::params!["view_password", password_hash],
            )
            .expect("update board access");
        }

        let router = Router::new()
            .route("/{board}/unlock", post(super::unlock_board_access))
            .with_state(state);

        for _ in 0..(super::BOARD_UNLOCK_FAIL_LIMIT - 1) {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/srate/unlock")
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .header(header::COOKIE, "csrf_token=csrf123")
                        .extension(crate::test_support::connect_info())
                        .body(Body::from(
                            "password=wrong&return_to=%2Fsrate%2Fcatalog&_csrf=csrf123",
                        ))
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/srate/unlock")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(
                        "password=wrong&return_to=%2Fsrate%2Fcatalog&_csrf=csrf123",
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response.headers().contains_key(header::RETRY_AFTER),
            "rate-limited unlock should advertise retry timing"
        );
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("Too many incorrect board password attempts."));
    }

    #[tokio::test]
    async fn locked_board_media_requires_unlock() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
            let password_hash =
                crate::utils::crypto::hash_password("swordfish").expect("hash password");
            conn.execute(
                "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
                rusqlite::params!["view_password", password_hash],
            )
            .expect("update board access");
        }

        let router = Router::new()
            .route("/boards/{*media_path}", get(super::serve_board_media))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/boards/secret/thumbs/example.webp")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn submit_appeal_is_rate_limited_to_one_open_window() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("db connection");
            crate::db::add_ban(
                &conn,
                &crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret),
                "test ban",
                None,
            )
            .expect("add ban");
        }

        let router = Router::new()
            .route("/appeal", post(super::submit_appeal))
            .with_state(state);
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/appeal")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from("reason=please+unban&_csrf=csrf123"))
                .expect("request")
        };

        let first = router
            .clone()
            .oneshot(request())
            .await
            .expect("first appeal");
        let first_body = String::from_utf8(
            to_bytes(first.into_body(), usize::MAX)
                .await
                .expect("first body")
                .to_vec(),
        )
        .expect("first body utf8");
        assert!(first_body.contains("appeal has been submitted"));

        let second = router.oneshot(request()).await.expect("second appeal");
        let second_body = String::from_utf8(
            to_bytes(second.into_body(), usize::MAX)
                .await
                .expect("second body")
                .to_vec(),
        )
        .expect("second body utf8");
        assert!(second_body.contains("already filed an appeal"));
    }
}
