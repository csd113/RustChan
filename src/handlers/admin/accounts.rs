//! Authenticated account management; secrets are never returned or logged.

use super::{require_admin_post_origin_and_csrf, require_admin_session_sid, SESSION_COOKIE};
use crate::{
    db,
    error::{AppError, Result},
    middleware::AppState,
};
use axum::{
    extract::{Form, Path, State},
    http::HeaderMap,
    response::{IntoResponse as _, Response},
};
use axum_extra::extract::cookie::CookieJar;

#[derive(serde::Deserialize)]
pub(in crate::server) struct AccountForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    username: String,
    current_password: String,
    new_password: String,
    confirm_password: String,
}

/// Authorize, reauthenticate and transactionally create or reset an account.
pub(in crate::server) async fn update_account(
    State(state): State<AppState>,
    Path(action): Path<db::AccountAction>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    crate::middleware::ClientIp(client): crate::middleware::ClientIp,
    Form(form): Form<AccountForm>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let session = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let key = super::auth::login_ip_key(&client);
    if super::auth::is_login_locked(&key) {
        return Err(AppError::BadRequest(
            "Reauthentication locked; retry after the failed-login window.".into(),
        ));
    }
    let password_permit = state.password_work_gate.try_begin()?;
    tokio::task::spawn_blocking(move || -> Result<Response> {
        let _password_permit = password_permit;
        let mut conn = state.db.get()?;
        require_admin_session_sid(&conn, session.as_deref()).map(|_completed_value| ())?;
        if super::auth::record_login_fail(&key) > crate::config::CONFIG.operator.admin_login_fail_limit { return Err(AppError::DbBusy); }
        if form.new_password != form.confirm_password { return Ok(super::admin_panel_error_redirect_anchor("Passwords did not match. No account changes saved.", "accounts").into_response()); }
        let result = db::manage_account(&mut conn, session.as_deref().unwrap_or_default(), action, form.username.trim(), &form.current_password, &form.new_password);
        match result {
            Ok(true) => { super::auth::clear_login_fails(&key); Ok(super::admin_panel_redirect_anchor("Account saved. Password changes revoke all sessions for that account; sign in again if you changed your own password.", "accounts").into_response()) },
            Ok(false) => {
                Ok(super::admin_panel_error_redirect_anchor("Current password did not verify. No account changes saved.", "accounts").into_response())
            }
            Err(error) => Ok(super::admin_panel_error_redirect_anchor(&format!("No account changes saved: {error}"), "accounts").into_response()),
        }
    }).await.map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}

/// Stage secret rotation after origin/CSRF/session checks and current-password verification.
pub(in crate::server) async fn rotate_secret(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    crate::middleware::ClientIp(client): crate::middleware::ClientIp,
    Form(form): Form<std::collections::BTreeMap<String, String>>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.get("_csrf").map(String::as_str),
    )?;
    let session = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let key = super::auth::login_ip_key(&client);
    if super::auth::is_login_locked(&key) {
        return Err(AppError::BadRequest(
            "Reauthentication locked; retry after the failed-login window.".into(),
        ));
    }
    let password_permit = state.password_work_gate.try_begin()?;
    tokio::task::spawn_blocking(move || -> Result<Response> {
        let _password_permit = password_permit;
        let conn = state.db.get()?;
        let actor = require_admin_session_sid(&conn, session.as_deref())?;
        if form.keys().any(|k| !matches!(k.as_str(), "_csrf" | "current_password" | "confirmation")) || form.get("confirmation").map(String::as_str) != Some("ROTATE") { return Ok(super::admin_panel_error_redirect_anchor("No secret changes saved. Type ROTATE to confirm the restart and identity consequences.", "storage").into_response()); }
        let password = form.get("current_password").map_or("", String::as_str);
        let actor = db::get_admin_name_by_id(&conn, actor)?.ok_or_else(|| AppError::Forbidden("Administrator missing".into()))?;
        let user = db::get_admin_by_username(&conn, &actor)?.ok_or_else(|| AppError::Forbidden("Administrator missing".into()))?;
        if super::auth::record_login_fail(&key) > crate::config::CONFIG.operator.admin_login_fail_limit { return Err(AppError::DbBusy); }
        if password.len() > 1024 || !crate::utils::crypto::verify_password(password, &user.password_hash)? {
            return Ok(super::admin_panel_error_redirect_anchor("Current password did not verify. No secret changes saved.", "storage").into_response());
        }
        super::auth::clear_login_fails(&key);
        let rotation = crate::config::admin::management::rotate_secret();
        match rotation {
            Ok(()) => Ok(super::admin_panel_redirect_anchor("Fresh secret staged. Restart required; sessions and signed grants will be invalidated, and IP ban identities will change.", "storage").into_response()),
            Err(error) => Ok(super::admin_panel_error_redirect_anchor(&format!("No secret changes saved: {error}"), "storage").into_response()),
        }
    }).await.map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}
