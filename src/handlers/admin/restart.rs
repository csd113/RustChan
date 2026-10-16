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
    updates::authorize(&state, &jar)
        .await
        .map(|_completed_value| ())?;
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
            drop(tokio::spawn(async move {
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
            }));
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
        drop(tokio::spawn(async move {
            let _guard = guard;
            // Permit the 303 response to reach the browser before closing listeners.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            cancel.cancel();
            cancel.cancelled().await;
            tokio::time::sleep(crate::restart::SHUTDOWN_TIMEOUT).await;
        }));
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
