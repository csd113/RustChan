//! Authenticated, CSRF-protected version checks. Downloads and installation are manual.

use super::{require_admin_post_origin_and_csrf, require_admin_session_sid, SESSION_COOKIE};
use crate::{
    error::{AppError, Result},
    middleware::AppState,
};
use axum::{
    extract::{Form, State},
    http::HeaderMap,
    response::{IntoResponse as _, Response},
    Json,
};
use axum_extra::extract::cookie::CookieJar;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
/// Version checks accept only the signed administrator CSRF token.
pub(in crate::server) struct CheckForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Authorize before reading cached metadata or contacting the official API.
pub(super) async fn authorize(state: &AppState, jar: &CookieJar) -> Result<i64> {
    let session = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let pool = state.db.clone();
    crate::middleware::data_access::spawn_blocking(move || {
        let conn = pool.get()?;
        require_admin_session_sid(&conn, session.as_deref())
    })
    .await
    .map_err(|error| AppError::Internal(error.into()))?
}

/// Read cached check results without triggering network access.
pub(in crate::server) async fn status(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    authorize(&state, &jar).await.map(|_completed| ())?;
    Ok(Json(state.version_check.snapshot()).into_response())
}

/// Check stable releases after authentication and origin/token validation.
pub(in crate::server) async fn check(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    context: crate::middleware::SecureCookieContext,
    Form(form): Form<CheckForm>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(&jar, &headers, context, form.csrf.as_deref())?;
    authorize(&state, &jar).await.map(|_completed| ())?;
    state
        .version_check
        .check()
        .await
        .map_err(|_error| AppError::DbBusy)?;
    let status = state.version_check.snapshot();
    let response = match status.discovery {
        Some(crate::updates::Discovery::UnableToCheck(_)) => {
            super::admin_panel_error_redirect_anchor(
                "Could not check GitHub. You can still open the official releases page.",
                "software-updates",
            )
        }
        _ => super::admin_panel_redirect_anchor("Release check completed.", "software-updates"),
    };
    Ok(response.into_response())
}
