//! Administrator-only POST restarts with closed forms and existing CSRF/origin protections.

use super::{require_admin_post_origin_and_csrf, updates};
use crate::{
    error::{AppError, Result},
    middleware::AppState,
    restart::View,
};
use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse as _, Response},
    Json,
};
use axum_extra::extract::cookie::CookieJar;

/// CSRF is the only accepted form parameter; service/command/path parameters are rejected.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct RestartForm {
    /// Session-scoped signed CSRF token.
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Derive pending state from actual saved/active settings, after authorization.
pub(super) async fn snapshot(native_status: Option<&crate::updates::Status>) -> View {
    let managed = crate::updates::managed();
    let local = if crate::restart::container_restart_enabled() {
        tokio::task::spawn_blocking(crate::restart::container_status)
            .await
            .ok()
            .and_then(std::result::Result::ok)
    } else {
        None
    };
    let status = if managed {
        native_status
    } else {
        local.as_ref()
    };
    let pending = tokio::task::spawn_blocking(crate::config::admin::restart_pending)
        .await
        .ok()
        .and_then(std::result::Result::ok);
    let in_progress = status.is_some_and(|s| s.phase.active());
    let supported = (managed || crate::restart::container_restart_enabled()) && pending.is_some();
    let message = if pending.is_none() {
        "Cannot safely read configuration state. Restart is disabled."
    } else if let Some(status) = status
        .filter(|s| s.operation == crate::updates::Operation::SettingsRestart || s.phase.active())
    {
        &status.message
    } else if pending == Some(true) {
        "Settings saved. Restart RustChan to apply these changes."
    } else {
        "Saved startup settings are active. No restart is required."
    };
    View {
        pending: pending.unwrap_or(true) || in_progress,
        supported,
        in_progress,
        message: if !supported && pending == Some(true) {
            format!("{message} This standalone deployment has no configured restart supervisor. Use the supplied managed systemd service or container restart policy.")
        } else {
            message.to_owned()
        },
        instance: *crate::restart::INSTANCE,
    }
}

/// Administrator-only bounded progress endpoint for reconnect polling.
pub(in crate::server) async fn status(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    updates::authorize(&state, &jar).await?;
    let status = updates::snapshot().await;
    Ok(Json(snapshot(Some(&status)).await).into_response())
}

/// Request the single predefined restart operation; merely saving never reaches this route.
pub(in crate::server) async fn restart(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<RestartForm>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let administrator = updates::authorize(&state, &jar).await?;
    let guard = state.maintenance_gate.try_begin("Settings restart")?;
    let pending = tokio::task::spawn_blocking(crate::config::admin::restart_pending)
        .await
        .map_err(|error| AppError::Internal(error.into()))??;
    if !pending {
        return Err(AppError::BadRequest(
            "No saved settings require a restart.".into(),
        ));
    }
    if crate::updates::managed() {
        let result = crate::updates::request(&crate::updates::Request::Restart {
            instance: *crate::restart::INSTANCE,
            administrator,
        })
        .await;
        let acknowledged_rejection = result.as_ref().is_ok_and(|reply| reply.error.is_some());
        if !acknowledged_rejection {
            let cancel = state.job_queue.cancel.clone();
            tokio::spawn(async move {
                let _guard = guard;
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if crate::updates::request(&crate::updates::Request::Status)
                        .await
                        .is_ok_and(|reply| {
                            reply.error.is_none() && reply.ready && !reply.status.phase.active()
                        })
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
        }
        match result {
            Ok(reply) if reply.error.is_none() => {},
            Ok(_) => return Ok(outcome(&headers, Some(false), "Restart rejected. Another operation may be running, or configuration/service preflight failed. Review the saved state and retry.")),
            Err(_) => return Ok(outcome(&headers, None, "Restart acknowledgment timed out. Check restart status before retrying; the supervisor may have accepted it.")),
        }
    } else if crate::restart::container_restart_enabled() {
        tokio::task::spawn_blocking(move || crate::restart::request_container(administrator))
            .await
            .map_err(|error| AppError::Internal(error.into()))??;
        let cancel = state.job_queue.cancel.clone();
        tokio::spawn(async move {
            let _guard = guard;
            // Permit the 303 response to reach the browser before closing listeners.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            cancel.cancel();
            cancel.cancelled().await;
            tokio::time::sleep(crate::restart::SHUTDOWN_TIMEOUT).await;
        });
    } else {
        return Err(AppError::BadRequest(
            "This deployment has no configured restart supervisor.".into(),
        ));
    }
    Ok(outcome(&headers, Some(true), "Restart accepted. RustChan is reconnecting; this page will show the verified result when it returns."))
}

/// AJAX receives the acknowledgment before shutdown; ordinary forms retain POST/redirect/GET.
fn outcome(headers: &HeaderMap, accepted: Option<bool>, message: &str) -> Response {
    if headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim() == "application/json")
        })
    {
        return (
            if accepted == Some(false) {
                axum::http::StatusCode::CONFLICT
            } else {
                axum::http::StatusCode::ACCEPTED
            },
            Json(serde_json::json!({ "accepted": accepted, "message": message })),
        )
            .into_response();
    }
    if accepted == Some(true) {
        super::admin_panel_redirect_anchor(message, "settings-restart").into_response()
    } else {
        super::admin_panel_error_redirect_anchor(message, "settings-restart").into_response()
    }
}

#[cfg(test)]
/// Real route extraction/authentication/CSRF tests with no active process supervisor.
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    /// Enhanced requests receive a bounded acknowledgment without following a redirect into downtime.
    #[tokio::test]
    async fn enhanced_acknowledgment_preserves_acceptance_and_rejection() -> anyhow::Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            axum::http::HeaderValue::from_static("application/json"),
        );
        for (accepted, expected) in [
            (Some(true), StatusCode::ACCEPTED),
            (None, StatusCode::ACCEPTED),
            (Some(false), StatusCode::CONFLICT),
        ] {
            let response = outcome(&headers, accepted, "Restart state");
            anyhow::ensure!(
                response.status() == expected && !response.headers().contains_key("location"),
                "JSON acknowledgment must not redirect"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
            let body: serde_json::Value = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                body.get("accepted") == Some(&serde_json::json!(accepted)),
                "uncertain acceptance must remain explicit"
            );
        }
        let native = outcome(&HeaderMap::new(), Some(true), "Restart accepted");
        anyhow::ensure!(
            native.status() == StatusCode::SEE_OTHER && native.headers().contains_key("location"),
            "ordinary forms retain POST/redirect/GET"
        );
        Ok(())
    }

    /// Exercise the production route handlers with isolated application/database fixtures.
    #[tokio::test]
    async fn restart_routes_reject_anonymous_users_methods_csrf_and_arbitrary_fields(
    ) -> anyhow::Result<()> {
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/admin/restart", post(restart))
            .route("/admin/restart/status", get(status))
            .with_state(state);
        for (method, route, body, expected) in [
            ("GET", "/admin/restart", "", StatusCode::METHOD_NOT_ALLOWED),
            ("GET", "/admin/restart/status", "", StatusCode::FORBIDDEN),
            (
                "POST",
                "/admin/restart",
                "_csrf=invalid",
                StatusCode::FORBIDDEN,
            ),
            (
                "POST",
                "/admin/restart",
                "command=systemctl&service=other",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(route)
                .header("host", "localhost")
                .header("origin", "http://localhost")
                .header("content-type", "application/x-www-form-urlencoded")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))?;
            let response = app.clone().oneshot(request).await?;
            anyhow::ensure!(
                response.status() == expected,
                "{method} {route} must reject with {expected}"
            );
        }
        Ok(())
    }
    /// A valid signature is insufficient without a full administrator session.
    #[tokio::test]
    async fn signed_csrf_does_not_authorize_a_non_admin_restart() -> anyhow::Result<()> {
        let state = crate::test_support::app_state();
        let session = "not-an-admin-session";
        let token = crate::utils::crypto::make_scoped_csrf_form_token(
            "raw",
            &crate::config::CONFIG.cookie_secret,
            session,
        );
        let jar = CookieJar::new()
            .add(axum_extra::extract::cookie::Cookie::new(
                "csrf_token",
                "raw",
            ))
            .add(axum_extra::extract::cookie::Cookie::new(
                super::super::SESSION_COOKIE,
                session,
            ));
        let mut headers = HeaderMap::new();
        headers.insert("host", axum::http::HeaderValue::from_static("localhost"));
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost"),
        );
        let result = restart(
            State(state),
            jar,
            headers,
            crate::test_support::connect_info(),
            Form(RestartForm { csrf: Some(token) }),
        )
        .await;
        anyhow::ensure!(
            matches!(result, Err(AppError::Forbidden(_))),
            "valid CSRF cannot substitute for admin authorization"
        );
        Ok(())
    }
}
