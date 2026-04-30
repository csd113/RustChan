// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Deserialize)]
pub struct FullBackupSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub auto_full_backup_interval_hours: Option<String>,
    pub auto_full_backup_copies_to_keep: Option<String>,
    pub auto_full_backup_include_tor_hidden_service_keys: Option<String>,
}

pub async fn update_full_backup_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<FullBackupSettingsForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let interval_hours = form
        .auto_full_backup_interval_hours
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(CONFIG.auto_full_backup_interval_hours)
        .min(8_760);
    let copies_to_keep = form
        .auto_full_backup_copies_to_keep
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(CONFIG.auto_full_backup_copies_to_keep)
        .clamp(1, 1_000);
    let include_tor_hidden_service_keys = super::checkbox_is_on(
        form.auto_full_backup_include_tor_hidden_service_keys
            .as_deref(),
    );

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let auto_backup_settings = state.auto_full_backup_settings.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            auto_backup_settings.update(
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
            );
            crate::config::update_settings_file_auto_full_backup(
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
            );
            tracing::info!(
                target: "admin",
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
                "Automatic full-backup settings updated"
            );
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(super::admin_panel_redirect_anchor(
        "Automatic full-backup settings saved.",
        "full-backup-restore",
    )
    .into_response())
}
