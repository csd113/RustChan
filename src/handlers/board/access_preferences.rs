// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use axum::http::Uri;
use std::net::IpAddr;

const OWNED_POSTS_COOKIE: &str = "rustchan_owned_posts";
const OWNED_POSTS_COOKIE_MAX: usize = 16;
pub(crate) const SELF_DELETE_WINDOW_SECS: i64 = 60;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnedPostGrant {
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

fn owned_posts_cookie_signature(payload_hex: &str) -> String {
    crate::utils::crypto::sha256_hex(
        format!("{}:owned-posts:{payload_hex}", CONFIG.cookie_secret).as_bytes(),
    )
}

fn parse_owned_posts_cookie(value: &str) -> Vec<OwnedPostGrant> {
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
    Some(format!("{payload_hex}.{signature}"))
}

fn owned_posts_cookie(grants: &[OwnedPostGrant]) -> Option<Cookie<'static>> {
    let value = owned_posts_cookie_value(grants)?;
    let mut cookie = Cookie::new(OWNED_POSTS_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(CONFIG.https_cookies);
    cookie.set_max_age(Duration::minutes(5));
    Some(cookie)
}

pub fn owned_post_grants_from_jar(jar: &CookieJar) -> Vec<OwnedPostGrant> {
    jar.get(OWNED_POSTS_COOKIE)
        .map(Cookie::value)
        .map(parse_owned_posts_cookie)
        .unwrap_or_default()
}

pub fn owned_post_grant_from_jar(
    jar: &CookieJar,
    board_short: &str,
    post_id: i64,
) -> Option<OwnedPostGrant> {
    owned_post_grants_from_jar(jar)
        .into_iter()
        .find(|grant| grant.post_id == post_id && grant.board_short == board_short)
}

pub fn remember_owned_post(
    jar: CookieJar,
    board_short: &str,
    thread_id: i64,
    post_id: i64,
    deletion_token: &str,
) -> CookieJar {
    let now = chrono::Utc::now().timestamp();
    let mut grants = owned_post_grants_from_jar(&jar)
        .into_iter()
        .filter(|grant| grant.post_id != post_id)
        .collect::<Vec<_>>();
    grants.push(OwnedPostGrant {
        post_id,
        thread_id,
        board_short: board_short.to_string(),
        deletion_token: deletion_token.to_string(),
        expires_at: now + SELF_DELETE_WINDOW_SECS,
    });
    grants.sort_by(|a, b| {
        b.expires_at
            .cmp(&a.expires_at)
            .then_with(|| b.post_id.cmp(&a.post_id))
    });
    grants.truncate(OWNED_POSTS_COOKIE_MAX);

    if let Some(cookie) = owned_posts_cookie(&grants) {
        jar.add(cookie)
    } else {
        jar.remove(Cookie::from(OWNED_POSTS_COOKIE))
    }
}

pub fn forget_owned_post(jar: CookieJar, board_short: &str, post_id: i64) -> CookieJar {
    let grants = owned_post_grants_from_jar(&jar)
        .into_iter()
        .filter(|grant| !(grant.post_id == post_id && grant.board_short == board_short))
        .collect::<Vec<_>>();
    if let Some(cookie) = owned_posts_cookie(&grants) {
        jar.add(cookie)
    } else {
        jar.remove(Cookie::from(OWNED_POSTS_COOKIE))
    }
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
        .map(|value| safe_return_to(Some(value.as_str()), "/"))
        .or_else(|| safe_referer_return_to(&headers))
        .unwrap_or_else(|| "/".to_string());
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
        .then(|| path_and_query.to_string())
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

    let redirect_to = safe_return_to(form.return_to.as_deref(), "/");
    Ok((jar.add(cookie), Redirect::to(&redirect_to)).into_response())
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
        |path| safe_return_to(Some(path), &format!("/{board_short}/catalog")),
    );
    Ok(Redirect::to(&redirect_to).into_response())
}

// ─── POST /report ─────────────────────────────────────────────────────────────
