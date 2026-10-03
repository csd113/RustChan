//! Application-state reporting for existing live settings and startup-only storage.

use super::{BTreeMap, InputKind, SettingDefinition, SettingField};
use anyhow::Context as _;

/// The sixteen pre-existing runtime controls, with their startup getters.
pub static SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "forum_name", label: "Site name", help: "Database-owned after seeding; settings.toml and CHAN_FORUM_NAME initialize an unconfigured site.", environment: "CHAN_FORUM_NAME", kind: InputKind::Text, value: |c| c.forum_name.clone() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "site_subtitle", label: "Homepage subtitle", help: "Database-owned after seeding.", environment: "CHAN_SITE_SUBTITLE", kind: InputKind::Text, value: |c| c.initial_site_subtitle.clone() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "homepage_new_thread_badges_enabled", label: "Homepage new-thread badges", help: "Database-owned after seeding; the legacy combined notification default remains a startup alias.", environment: "CHAN_HOMEPAGE_NEW_THREAD_BADGES", kind: InputKind::Boolean, value: |c| c.initial_homepage_new_thread_badges_enabled.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "homepage_new_reply_badges_enabled", label: "Homepage new-reply badges", help: "Database-owned after seeding.", environment: "CHAN_HOMEPAGE_NEW_REPLY_BADGES", kind: InputKind::Boolean, value: |c| c.initial_homepage_new_reply_badges_enabled.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "thread_new_reply_badges_enabled", label: "Thread new-reply badges", help: "Database-owned after seeding.", environment: "CHAN_THREAD_NEW_REPLY_BADGES", kind: InputKind::Boolean, value: |c| c.initial_thread_new_reply_badges_enabled.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "default_theme", label: "Default visitor theme", help: "Database-owned catalog default. Per-board and visitor selections keep precedence.", environment: "CHAN_DEFAULT_THEME", kind: InputKind::Text, value: |c| c.initial_default_theme.clone() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "enabled_builtin_themes", label: "Enabled built-in themes", help: "Database theme catalog is authoritative after seeding; custom themes are managed in Appearance.", environment: "", kind: InputKind::List, value: |c| c.initial_enabled_builtin_themes.join(", ") },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "ffmpeg_timeout_secs", label: "FFmpeg job timeout (seconds)", help: "Saved with Media settings in one database transaction. CHAN_FFMPEG_TIMEOUT_SECS overrides on restart. Applies live to new subprocess calls.", environment: "CHAN_FFMPEG_TIMEOUT_SECS", kind: InputKind::Number(30,86400), value: |c| c.ffmpeg_timeout_secs.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "media_auto_prune_enabled", label: "Automatic post-media pruning", help: "Database-owned; saving may immediately run pruning. Global upload gates remain separate.", environment: "CHAN_MEDIA_AUTO_PRUNE_ENABLED", kind: InputKind::Boolean, value: |c| c.initial_media_auto_prune_enabled.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "media_max_active_content_size_bytes", label: "Active post-media cap (bytes)", help: "Database-owned; 0 is unset. Full-size media pruning retains thumbnails where possible.", environment: "CHAN_MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES", kind: InputKind::Number(0, u64::MAX), value: |c| c.initial_media_max_active_content_size_bytes.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "backup_directory", label: "Saved backup directory", help: "File-owned, restart required; existing backups are not relocated.", environment: "CHAN_BACKUP_DIRECTORY", kind: InputKind::Text, value: |c| c.backup_directory.clone().unwrap_or_else(|| super::super::data_dir().join("backups")).display().to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "auto_full_backup_interval_hours", label: "Automatic backup interval (hours)", help: "File-owned shared live scheduler; environment wins on restart.", environment: "CHAN_AUTO_FULL_BACKUP_HOURS", kind: InputKind::Number(0,8760), value: |c| c.auto_full_backup_interval_hours.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "auto_full_backup_copies_to_keep", label: "Automatic backup retention (copies)", help: "File-owned shared live scheduler; environment wins on restart.", environment: "CHAN_AUTO_FULL_BACKUP_COPIES", kind: InputKind::Number(1,1000), value: |c| c.auto_full_backup_copies_to_keep.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "auto_full_backup_include_tor_hidden_service_keys", label: "Include Tor identities in automatic backups", help: "File-owned shared live scheduler; protect archives containing private onion identities.", environment: "CHAN_AUTO_FULL_BACKUP_INCLUDE_TOR_KEYS", kind: InputKind::Boolean, value: |c| c.auto_full_backup_include_tor_hidden_service_keys.to_string() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "auto_full_backup_storage_mode", label: "Automatic backup format", help: "File-owned shared live scheduler; directory or split_zip.", environment: "CHAN_AUTO_FULL_BACKUP_STORAGE_MODE", kind: InputKind::Text, value: |c| c.auto_full_backup_storage_mode.clone() },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Live, key: "auto_full_backup_split_zip_part_size_gib", label: "Automatic ZIP part size (GiB)", help: "File-owned shared live scheduler; applies to subsequent backups.", environment: "CHAN_AUTO_FULL_BACKUP_SPLIT_ZIP_PART_SIZE_GIB", kind: InputKind::Number(1,64), value: |c| (c.auto_full_backup_split_zip_part_size_bytes / (1024*1024*1024)).to_string() },
];

/// Report actual live values rather than immutable startup defaults.
///
/// # Errors
/// Returns malformed/unreadable settings or missing authoritative live state.
pub fn snapshot(
    live: &BTreeMap<String, String>,
    saved_timeout: Option<&str>,
) -> anyhow::Result<Vec<SettingField>> {
    let content = std::fs::read_to_string(super::super::settings_file_path())?;
    let mut fields = super::snapshot_from(&content, &super::CONFIG, SETTINGS)?;
    for field in &mut fields {
        let key = field.definition.key;
        field.active = live
            .get(key)
            .cloned()
            .with_context(|| format!("missing live setting {key}"))?;
        if matches!(
            key,
            "forum_name"
                | "site_subtitle"
                | "homepage_new_thread_badges_enabled"
                | "homepage_new_reply_badges_enabled"
                | "thread_new_reply_badges_enabled"
                | "default_theme"
                | "enabled_builtin_themes"
                | "media_auto_prune_enabled"
                | "media_max_active_content_size_bytes"
        ) {
            field.saved.clone_from(&field.active);
            field.next_start.clone_from(&field.active);
            field.source = "Database (authoritative after initial seeding; startup inputs do not override an existing value)".into();
            field.pending = false;
            field.overridden = false;
        } else if key == "ffmpeg_timeout_secs" {
            if let Some(saved) = &saved_timeout {
                (*saved).clone_into(&mut field.saved);
            }
            let overridden = std::env::var(field.definition.environment)
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .is_some();
            if !overridden {
                field.next_start.clone_from(&field.saved);
            }
            field.source = if overridden {
                "Database/live; CHAN_FFMPEG_TIMEOUT_SECS takes priority on restart"
            } else if saved_timeout.is_some() {
                "Database/live (settings.toml is the initial seed)"
            } else {
                "Startup file/environment/default; live atomic timeout"
            }
            .into();
            field.pending = field.definition.application == super::ApplicationMode::Restart
                && field.active != field.next_start;
            field.overridden = field.saved != field.next_start;
        } else {
            field.pending = field.definition.application == super::ApplicationMode::Restart
                && field.active != field.next_start;
            field.source = backup_source(field.definition, &super::Environment::Process);
        }
    }
    Ok(fields)
}

/// Explain parser fallbacks instead of claiming malformed overrides are effective.
fn backup_source(definition: &SettingDefinition, environment: &super::Environment<'_>) -> String {
    if environment.var_os(definition.environment).is_none() {
        "Settings.toml / live backup store".into()
    } else if definition.key == "backup_directory"
        || super::environment_override_is_valid(definition, environment)
    {
        format!(
            "Live store / settings.toml; {} overrides on restart",
            definition.environment
        )
    } else {
        format!(
            "Live store / settings.toml; {} present but invalid; file/default fallback on restart",
            definition.environment
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::ensure;

    #[test]
    fn malformed_backup_overrides_report_the_actual_file_fallback() -> anyhow::Result<()> {
        for (key, value) in [
            ("auto_full_backup_copies_to_keep", "broken"),
            ("auto_full_backup_storage_mode", "unsupported"),
        ] {
            let definition = SETTINGS
                .iter()
                .find(|field| field.key == key)
                .context("missing backup setting")?;
            let values = BTreeMap::from([(definition.environment.to_owned(), value.to_owned())]);
            ensure!(
                backup_source(definition, &super::super::Environment::Values(&values))
                    .contains("present but invalid")
            );
        }
        Ok(())
    }
}
