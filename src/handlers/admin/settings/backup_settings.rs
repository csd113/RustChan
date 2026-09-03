use super::{
    admin_panel_redirect_anchor, checkbox_is_on, require_admin_post_origin_and_csrf,
    require_admin_session_sid, AppError, AppState, CookieJar, Form, HeaderMap, Response, Result,
    State, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;

#[derive(Deserialize)]
pub(in crate::server) struct FullBackupSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub auto_full_backup_interval_hours: Option<String>,
    pub auto_full_backup_copies_to_keep: Option<String>,
    pub auto_full_backup_include_tor_hidden_service_keys: Option<String>,
    pub auto_full_backup_storage_mode: Option<String>,
    pub auto_full_backup_split_zip_part_size_gib: Option<String>,
}

struct ParsedFullBackupSettings {
    interval_hours: u64,
    copies_to_keep: u64,
    include_tor_hidden_service_keys: bool,
    storage_mode_value: &'static str,
    split_zip_part_size: u64,
    split_zip_part_size_gib: u64,
}

fn parse_full_backup_settings_form(
    form: &FullBackupSettingsForm,
) -> Result<ParsedFullBackupSettings> {
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
    let include_tor_hidden_service_keys = checkbox_is_on(
        form.auto_full_backup_include_tor_hidden_service_keys
            .as_deref(),
    );
    let storage_mode = crate::handlers::admin::backup::parse_backup_storage_mode_value(
        form.auto_full_backup_storage_mode.as_deref(),
    )?;
    let storage_mode_value = match storage_mode {
        crate::handlers::admin::BackupStorageMode::Directory => "directory",
        crate::handlers::admin::BackupStorageMode::SplitZip => "split_zip",
        _ => {
            return Err(AppError::BadRequest(
                "Unsupported automatic backup storage mode.".into(),
            ));
        }
    };
    let split_zip_part_size_gib = form
        .auto_full_backup_split_zip_part_size_gib
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok());
    let split_zip_part_size =
        crate::handlers::admin::backup::parse_split_zip_part_size_gib(split_zip_part_size_gib)?;
    let split_zip_part_size_gib =
        crate::handlers::admin::backup::split_zip_part_size_gib(split_zip_part_size);

    Ok(ParsedFullBackupSettings {
        interval_hours,
        copies_to_keep,
        include_tor_hidden_service_keys,
        storage_mode_value,
        split_zip_part_size,
        split_zip_part_size_gib,
    })
}

pub(in crate::server) async fn update_full_backup_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<FullBackupSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let settings = parse_full_backup_settings_form(&form)?;
    let interval_hours = settings.interval_hours;
    let copies_to_keep = settings.copies_to_keep;
    let include_tor_hidden_service_keys = settings.include_tor_hidden_service_keys;
    let storage_mode_value = settings.storage_mode_value;
    let split_zip_part_size = settings.split_zip_part_size;
    let split_zip_part_size_gib = settings.split_zip_part_size_gib;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let auto_backup_settings = state.auto_full_backup_settings.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            auto_backup_settings.update(
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
                storage_mode_value,
                split_zip_part_size,
            );
            crate::config::update_settings_file_auto_full_backup(
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
                storage_mode_value,
                split_zip_part_size_gib,
            );
            tracing::info!(
                target: "admin",
                interval_hours,
                copies_to_keep,
                include_tor_hidden_service_keys,
                storage_mode = storage_mode_value,
                split_zip_part_size,
                "Automatic full-backup settings updated"
            );
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(admin_panel_redirect_anchor(
        "Automatic full-backup settings saved.",
        "full-backup-restore",
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::{parse_full_backup_settings_form, FullBackupSettingsForm};
    use anyhow::ensure;

    #[test]
    fn automatic_backup_settings_parse_directory_output_mode() -> anyhow::Result<()> {
        let parsed = parse_full_backup_settings_form(&FullBackupSettingsForm {
            csrf: None,
            auto_full_backup_interval_hours: Some("12".to_owned()),
            auto_full_backup_copies_to_keep: Some("3".to_owned()),
            auto_full_backup_include_tor_hidden_service_keys: None,
            auto_full_backup_storage_mode: Some("directory".to_owned()),
            auto_full_backup_split_zip_part_size_gib: Some("8".to_owned()),
        })?;

        ensure!(
            parsed.interval_hours == 12,
            "interval was parsed incorrectly"
        );
        ensure!(
            parsed.copies_to_keep == 3,
            "retention was parsed incorrectly"
        );
        ensure!(
            !parsed.include_tor_hidden_service_keys,
            "an absent Tor-key checkbox was treated as enabled"
        );
        ensure!(
            parsed.storage_mode_value == "directory",
            "directory storage mode was parsed incorrectly"
        );
        ensure!(
            parsed.split_zip_part_size_gib == 8,
            "split ZIP display size was parsed incorrectly"
        );
        ensure!(
            parsed.split_zip_part_size == 8 * 1024 * 1024 * 1024,
            "split ZIP byte size was parsed incorrectly"
        );
        Ok(())
    }

    #[test]
    fn automatic_backup_settings_parse_split_zip_output_mode() -> anyhow::Result<()> {
        let parsed = parse_full_backup_settings_form(&FullBackupSettingsForm {
            csrf: None,
            auto_full_backup_interval_hours: Some("24".to_owned()),
            auto_full_backup_copies_to_keep: Some("5".to_owned()),
            auto_full_backup_include_tor_hidden_service_keys: Some("1".to_owned()),
            auto_full_backup_storage_mode: Some("split_zip".to_owned()),
            auto_full_backup_split_zip_part_size_gib: Some("2".to_owned()),
        })?;

        ensure!(
            parsed.storage_mode_value == "split_zip",
            "split ZIP storage mode was parsed incorrectly"
        );
        ensure!(
            parsed.split_zip_part_size_gib == 2,
            "split ZIP display size was parsed incorrectly"
        );
        ensure!(
            parsed.split_zip_part_size == 2 * 1024 * 1024 * 1024,
            "split ZIP byte size was parsed incorrectly"
        );
        ensure!(
            parsed.include_tor_hidden_service_keys,
            "the Tor-key checkbox was not parsed as enabled"
        );
        Ok(())
    }
}
