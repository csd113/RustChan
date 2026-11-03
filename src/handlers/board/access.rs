//! Board password policy, access grants, and unlock requests.

use super::access_preferences::ensure_csrf_for_request;
use super::{
    check_csrf_jar, current_theme_from_jar, safe_return_to, should_set_public_secure_cookie,
    ADMIN_SESSION_COOKIE,
};
use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::posting,
    middleware::{AppState, SecureCookieContext},
    models::Board,
    templates,
    utils::crypto::{sha256_hex, verify_password},
};
use axum::{
    extract::{Form, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use time::Duration;

/// Board access cookie prefix used by this handler.
const BOARD_ACCESS_COOKIE_PREFIX: &str = "rustchan_board_access_";
/// Board access cookie TTL days used by this handler.
fn board_access_cookie_days() -> i64 {
    CONFIG.operator.board_access_cookie_days
}
/// Board unlock fail limit used by this handler.
fn board_password_fail_limit() -> u32 {
    CONFIG.operator.board_password_fail_limit
}
/// Board unlock fail window secs used by this handler.
fn board_password_fail_window_secs() -> u64 {
    CONFIG.operator.board_password_fail_window_secs
}

/// Bounded board-password failure state, independent of public visitor cookies.
static BOARD_UNLOCK_FAILS: LazyLock<crate::middleware::RateTable> =
    LazyLock::new(crate::middleware::RateTable::default);

pub(in crate::server::handlers) struct BoardAccessContext {
    pub board: Board,
    pub is_admin: bool,
    pub can_view: bool,
    pub can_post: bool,
}

pub(in crate::server::handlers) enum BoardAccessRequirement {
    View,
    Post,
}

pub(in crate::server::handlers) struct BoardAccessDenial {
    pub context: BoardAccessContext,
    pub return_to: String,
}

pub(in crate::server::handlers) enum BoardAccessDecision {
    Allowed(BoardAccessContext),
    Denied(BoardAccessDenial),
}

fn board_access_cookie_name(board_short: &str) -> String {
    format!("{BOARD_ACCESS_COOKIE_PREFIX}{board_short}")
}

pub(in crate::server::handlers) fn board_access_cookie_from_jar(
    jar: &CookieJar,
    board_short: &str,
) -> Option<String> {
    let cookie_name = board_access_cookie_name(board_short);
    jar.get(cookie_name.as_str())
        .map(|cookie| cookie.value().to_owned())
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

pub(super) fn can_view_board(board: &Board, is_admin: bool, access_cookie: Option<&str>) -> bool {
    is_admin
        || !board.access_mode.requires_view_password()
        || has_valid_board_access_cookie(
            &board.short_name,
            &board.access_password_hash,
            access_cookie,
        )
}

fn can_post_to_board(board: &Board, is_admin: bool, access_cookie: Option<&str>) -> bool {
    is_admin
        || !board.access_mode.requires_unlock_for_posting()
        || has_valid_board_access_cookie(
            &board.short_name,
            &board.access_password_hash,
            access_cookie,
        )
}

pub(in crate::server::handlers) fn load_board_access_context(
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

pub(in crate::server::handlers) async fn board_access_preflight(
    state: &AppState,
    board_short: &str,
    admin_session_id: Option<String>,
    access_cookie: Option<String>,
    requirement: BoardAccessRequirement,
    return_to: String,
) -> Result<BoardAccessDecision> {
    let access_context = crate::middleware::data_access::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.to_owned();
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

pub(in crate::server::handlers) fn unlock_redirect_url(
    board_short: &str,
    return_to: &str,
) -> String {
    format!(
        "/{board_short}/unlock?return_to={}",
        templates::urlencoding_simple(return_to)
    )
}

pub(in crate::server::handlers) fn render_board_unlock_html(
    board: &Board,
    csrf_token: &str,
    return_to: &str,
    error: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    let boards = templates::live_boards();
    templates::board_access_page(
        board,
        csrf_token,
        boards.as_ref(),
        return_to,
        error,
        current_theme,
        board.collapse_greentext,
    )
}

pub(in crate::server::handlers) fn board_access_denied_response(
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

fn board_unlock_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn board_unlock_attempt_key(board_short: &str, client_ip: &str) -> String {
    sha256_hex(format!("{board_short}:{client_ip}").as_bytes())
}

/// Return the remaining lockout or saturation interval.
fn board_unlock_retry_after_secs(attempt_key: &str) -> Option<u64> {
    BOARD_UNLOCK_FAILS.retry_after(
        attempt_key,
        board_unlock_now_secs(),
        board_password_fail_limit(),
    )
}

/// Record one failed password attempt without unbounded retained identity growth.
fn record_board_unlock_failure(attempt_key: &str) -> u32 {
    BOARD_UNLOCK_FAILS.record(
        attempt_key,
        board_unlock_now_secs(),
        board_password_fail_window_secs(),
    )
}

/// Remove an authenticated client's failure counter.
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
    drop(resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE),
    ));
    if let Some(retry_after_secs) = retry_after_secs {
        if let Ok(retry_after) = HeaderValue::from_str(&retry_after_secs.to_string()) {
            drop(resp.headers_mut().insert(header::RETRY_AFTER, retry_after));
        }
    }
    (jar, resp).into_response()
}

pub(in crate::server::handlers) fn board_access_required_response(
    jar: CookieJar,
    html: String,
) -> Response {
    board_access_page_response(jar, html, StatusCode::FORBIDDEN, None)
}

fn board_access_ok_response(jar: CookieJar, html: String) -> Response {
    board_access_page_response(jar, html, StatusCode::OK, None)
}

fn board_access_rate_limited_response(
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
    let access_context = crate::middleware::data_access::spawn_blocking({
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
    let access_context = crate::middleware::data_access::spawn_blocking({
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
    if record_board_unlock_failure(&attempt_key) > board_password_fail_limit() {
        return Err(AppError::DbBusy);
    }
    let password_hash = access_context.board.access_password_hash.clone();
    let password_valid_result = crate::middleware::data_access::spawn_blocking(move || {
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
