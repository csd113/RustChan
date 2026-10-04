//! Administrator-only software discovery and reauthenticated installation requests.

use super::{require_admin_post_origin_and_csrf, require_admin_session_sid, SESSION_COOKIE};
use crate::{
    db,
    error::{AppError, Result},
    middleware::AppState,
    updates::{self, Request, Status},
};
use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse as _, Response},
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use std::io::Read as _;

/// Closed check request; installation credentials cannot become paths or commands.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct CheckForm {
    /// Session-scoped form CSRF token.
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// One-use approval plus the current administrator's password.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct InstallForm {
    /// Session-scoped CSRF token.
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    /// Opaque updater-issued approval, consumed durably before installation starts.
    approval: String,
    /// Never serialized, returned or logged.
    current_password: String,
    /// Explicit acknowledgment of the restart.
    confirmation: String,
}

/// Authorize before reading any discovery metadata or contacting the updater.
pub(super) async fn authorize(state: &AppState, jar: &CookieJar) -> Result<i64> {
    let session = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        require_admin_session_sid(&conn, session.as_deref())
    })
    .await
    .map_err(|error| AppError::Internal(error.into()))?
}

/// Fetch persisted update state only after the caller has authorized the administrator.
pub(super) async fn snapshot() -> Status {
    if updates::managed() {
        return updates::request(&Request::Status).await.map_or_else(|_| Status {
            installed: updates::VERSION.to_owned(),
            message: "Update controller unavailable. Check RustChan's launch logs; installation is disabled.".to_owned(),
            ..Status::default()
        }, |reply| reply.status);
    }
    tokio::task::spawn_blocking(|| {
        let path = crate::config::runtime_dir().join("software-updates.json");
        let status = std::fs::File::open(path)
            .ok()
            .and_then(|file| {
                let mut bytes = Vec::new();
                file.take(512 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .ok()
                    .map(|_completed_value| ())?;
                (bytes.len() <= 512 * 1024).then_some(bytes)
            })
            .and_then(|bytes| serde_json::from_slice::<Status>(&bytes).ok());
        status.unwrap_or_else(|| Status {
            installed: updates::VERSION.to_owned(),
            ..Status::default()
        })
    })
    .await
    .unwrap_or_default()
}

/// Return status with admin-only cache policy inherited from the admin route group.
pub(in crate::server) async fn status(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    authorize(&state, &jar).await.map(|_completed_value| ())?;
    Ok(Json(snapshot().await).into_response())
}

/// Perform bounded official release discovery; no service state changes.
pub(in crate::server) async fn check(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<CheckForm>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    authorize(&state, &jar).await.map(|_completed_value| ())?;
    let _guard = state.maintenance_gate.try_begin("Software update check")?;
    let outcome = if updates::managed() {
        updates::request(&Request::Check).await.and_then(|reply| {
            anyhow::ensure!(reply.error.is_none(), "updater check failed");
            Ok(())
        })
    } else {
        tokio::task::spawn_blocking(|| -> anyhow::Result<()> {
            let status = Status {
                installed: updates::VERSION.to_owned(),
                discovery: Some(updates::discover_official(updates::VERSION)),
                checked_at: Some(chrono::Utc::now().to_rfc3339()),
                message: "Release check completed. Installation is deployment-managed.".to_owned(),
                ..Status::default()
            };
            crate::config::write_private_file(
                &crate::config::runtime_dir().join("software-updates.json"),
                &serde_json::to_vec(&status)?,
            )?;
            Ok(())
        })
        .await
        .map_err(|error| AppError::Internal(error.into()))?
    };
    Ok(if outcome.is_ok() {
        super::admin_panel_redirect_anchor("Release check completed.", "software-updates")
    } else {
        super::admin_panel_error_redirect_anchor(
            "Release check could not complete. Retry later or check updater logs.",
            "software-updates",
        )
    }
    .into_response())
}

/// Reauthenticate using existing login lockout before sending the closed Install operation.
pub(in crate::server) async fn install(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    crate::middleware::ClientIp(client): crate::middleware::ClientIp,
    Form(form): Form<InstallForm>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let session = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let key = super::auth::login_ip_key(&client);
    if super::auth::is_login_locked(&key) {
        return Err(AppError::Forbidden(
            "Reauthentication locked. Retry after the failed-login window.".to_owned(),
        ));
    }
    let maintenance_gate = state.maintenance_gate.clone();
    let approval = form.approval;
    let password = form.current_password;
    let confirmation = form.confirmation;
    let approval_valid = uuid::Uuid::parse_str(&approval).is_ok();
    let administrator = tokio::task::spawn_blocking(move || -> Result<i64> {
        let conn = state.db.get()?;
        let actor = require_admin_session_sid(&conn, session.as_deref())?;
        let name = db::get_admin_name_by_id(&conn, actor)?
            .ok_or_else(|| AppError::Forbidden("Administrator missing.".to_owned()))?;
        let user = db::get_admin_by_username(&conn, &name)?
            .ok_or_else(|| AppError::Forbidden("Administrator missing.".to_owned()))?;
        if password.len() > 1024
            || !crate::utils::crypto::verify_password(&password, &user.password_hash)?
        {
            let _failure_count = super::auth::record_login_fail(&key);
            return Err(AppError::Forbidden(
                "Current password did not verify. No update started.".to_owned(),
            ));
        }
        if !updates::managed() || confirmation != "INSTALL" || !approval_valid {
            return Err(AppError::BadRequest(
                "Native installation is unavailable or confirmation is invalid.".to_owned(),
            ));
        }
        Ok(actor)
    })
    .await
    .map_err(|error| AppError::Internal(error.into()))??;
    let guard = maintenance_gate.try_begin("Software update installation")?;
    let outcome = updates::request(&Request::Install {
        approval: approval.clone(),
        administrator,
    })
    .await;
    let acknowledged = outcome.is_ok();
    drop(tokio::spawn(async move {
        let _guard = guard;
        loop {
            if updates::request(&Request::Status)
                .await
                .is_ok_and(|reply| maintenance_finished(&reply.status, &approval, acknowledged))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }));
    let reply = outcome.map_err(|_error| {
        AppError::BadRequest(
            "Updater unavailable; reload Software Updates to check the persisted result."
                .to_owned(),
        )
    })?;
    if reply.error.is_some() {
        return Err(AppError::BadRequest(
            "Update could not start. Check releases again, then retry after resolving the reported issue.".to_owned(),
        ));
    }
    Ok(super::admin_panel_redirect_anchor("Update accepted. RustChan will restart; reload this page to see the verified final result.", "software-updates").into_response())
}

/// Keep existing backup/restore maintenance paused until a safe terminal result is known.
/// A lost install reply cannot prove rejection while the same approval remains outstanding.
fn maintenance_finished(status: &Status, approval: &str, acknowledged: bool) -> bool {
    !status.phase.active() && (acknowledged || status.approval.as_deref() != Some(approval))
}

#[cfg(test)]
/// Installation maintenance admission regression tests.
mod tests {
    use super::*;

    /// Lost replies and active work cannot prematurely release backup/restore serialization.
    #[test]
    fn maintenance_waits_for_consumed_approval_and_terminal_state() {
        let mut status = Status {
            approval: Some("pending".to_owned()),
            ..Status::default()
        };
        assert!(
            !maintenance_finished(&status, "pending", false),
            "lost reply with pending approval must hold maintenance"
        );
        status.approval = None;
        status.phase = updates::Phase::Downloading;
        assert!(
            !maintenance_finished(&status, "pending", true),
            "accepted download must serialize maintenance without blocking posts"
        );
        status.phase = updates::Phase::RolledBack;
        assert!(
            maintenance_finished(&status, "pending", false),
            "persisted complete rollback may release maintenance even after a lost reply"
        );
    }
}
