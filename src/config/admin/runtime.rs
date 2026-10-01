//! Restart-required runtime settings organized by operator task.

use super::{BTreeMap, Config, Environment, InputKind, SettingDefinition, SettingField};
use anyhow::{ensure, Context as _};

/// A group of related process-start settings, each saved and validated together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSection {
    /// Built-in onion service and identity selection.
    Tor,
    /// Upload defaults, external tools, caches and reconciliation budgets.
    Media,
    /// Recurring database schedules and size warnings.
    Maintenance,
    /// Advanced executor and database resource sizing.
    System,
    /// Native HTTPS and certificate sources.
    Https,
    /// Failed-attempt and signed-access policies.
    Access,
    /// Public index paging and reply previews.
    Display,
    /// Logging filter.
    Logging,
    /// Request deadlines.
    Timeouts,
}

impl RuntimeSection {
    /// Stable section name used by form routes and navigation.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Tor => "tor",
            Self::Media => "media",
            Self::Maintenance => "schedules",
            Self::System => "system",
            Self::Https => "https",
            Self::Access => "access",
            Self::Display => "display",
            Self::Logging => "logging",
            Self::Timeouts => "timeouts",
        }
    }

    /// Human-readable section heading.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tor => "Tor",
            Self::Media => "Media",
            Self::Maintenance => "Maintenance schedules",
            Self::System => "Advanced system settings",
            Self::Https => "HTTPS & Certificates",
            Self::Access => "Access policies",
            Self::Display => "Public index display",
            Self::Logging => "Logging",
            Self::Timeouts => "Request deadlines",
        }
    }

    /// Settings-file controls in this section.
    #[must_use]
    pub const fn definitions(self) -> &'static [SettingDefinition] {
        match self {
            Self::Tor => TOR_SETTINGS,
            Self::Media => MEDIA_SETTINGS,
            Self::Maintenance => MAINTENANCE_SETTINGS,
            Self::System => SYSTEM_SETTINGS,
            Self::Https => super::certificates::SETTINGS,
            Self::Access => super::super::operator::ACCESS_SETTINGS,
            Self::Display => super::super::operator::DISPLAY_SETTINGS,
            Self::Logging => super::super::operator::LOG_SETTINGS,
            Self::Timeouts => super::super::operator::TIMEOUT_SETTINGS,
        }
    }

    /// Route segment (maintenance retains a separate anchor from existing tools).
    #[must_use]
    pub const fn route_key(self) -> &'static str {
        match self {
            Self::Maintenance => "maintenance",
            _ => self.key(),
        }
    }
}

/// Complete set of restart-required root-setting groups.
pub const SECTIONS: &[RuntimeSection] = &[
    RuntimeSection::Tor,
    RuntimeSection::Media,
    RuntimeSection::Maintenance,
    RuntimeSection::System,
    RuntimeSection::Https,
    RuntimeSection::Access,
    RuntimeSection::Display,
    RuntimeSection::Logging,
    RuntimeSection::Timeouts,
];

/// Enumerate definitions for startup provenance without duplicating the registry.
pub(super) fn all_definitions() -> impl Iterator<Item = &'static SettingDefinition> {
    SECTIONS.iter().flat_map(|s| s.definitions())
}

/// Read saved and active values for an authenticated section.
///
/// # Errors
/// Returns an error when settings cannot be read or parsed safely.
pub fn section_snapshot(section: RuntimeSection) -> anyhow::Result<Vec<SettingField>> {
    let content = std::fs::read_to_string(super::super::settings_file_path())
        .context("read settings file")?;
    super::snapshot_from(&content, &super::CONFIG, section.definitions())
}

/// Save a complete related section; immutable running state remains unchanged.
///
/// # Errors
/// Rejects invalid fields, related-state conflicts, unusable new tools and failed persistence.
pub fn save_section(
    section: RuntimeSection,
    form: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if section == RuntimeSection::Https {
        return super::certificates::save(form);
    }
    let updates = super::parse_settings_form(section.definitions(), form)?;
    let _guard = super::SETTINGS_WRITE_LOCK.lock();
    let path = super::super::settings_file_path();
    let before = std::fs::read_to_string(&path).context("read settings file")?;
    let empty = BTreeMap::new();
    let previous = super::resolve_file(&before, &Environment::Values(&empty))?;
    let after = super::rewrite_root_settings(&before, &updates)?;
    let saved = super::resolve_file(&after, &Environment::Values(&empty))?;
    validate_section(section, &saved)?;
    if section == RuntimeSection::Media
        && (previous.ffmpeg_path != saved.ffmpeg_path || saved.require_ffmpeg)
    {
        probe_tool("ffmpeg", &saved.ffmpeg_path)?;
    }
    super::save_root_at(&path, &updates, &Environment::Process, |candidate| {
        validate_section(section, candidate)
    })
}

/// Validate related section values without changing paths, workers or listeners.
fn validate_section(section: RuntimeSection, config: &Config) -> anyhow::Result<()> {
    super::validate_network(config)?;
    for definition in section.definitions() {
        if let InputKind::Number(min, max) = definition.kind {
            let value: u64 = (definition.value)(config)
                .parse()
                .context("invalid resolved number")?;
            ensure!(
                (min..=max).contains(&value),
                "{} must be between {min} and {max}",
                definition.label
            );
        }
    }
    if section == RuntimeSection::Tor {
        config
            .tor_service_nickname
            .parse::<tor_hsservice::HsNickname>()
            .context("invalid Tor service nickname")?;
    }
    Ok(())
}

/// Permit only the documented PATH fallback or an absolute regular executable.
fn validate_tool_path(tool: &str, path: &str) -> anyhow::Result<()> {
    if path == tool {
        return Ok(());
    }
    let candidate = std::path::Path::new(path);
    ensure!(
        candidate.is_absolute()
            && !candidate
                .components()
                .any(|c| c == std::path::Component::ParentDir),
        "{tool} must use PATH lookup ({tool}) or an absolute path without parent traversal"
    );
    ensure!(
        !path.chars().any(char::is_control),
        "invalid executable path"
    );
    ensure!(
        std::fs::metadata(candidate).is_ok_and(|m| m.is_file()),
        "{tool} executable is not a regular file"
    );
    Ok(())
}

/// Probe a validated executable without a shell and with bounded time/output.
fn probe_tool(tool: &str, path: &str) -> anyhow::Result<()> {
    validate_tool_path(tool, path)?;
    let output = crate::media::process::run_std_command_with_timeout(
        std::process::Command::new(path).arg("-version"),
        std::time::Duration::from_secs(5),
        "media-tool probe timed out",
        || format!("could not execute {tool}"),
        || format!("could not read {tool} probe output"),
    )?;
    ensure!(output.status.success(), "{tool} version probe failed");
    ensure!(
        String::from_utf8_lossy(&output.stdout).starts_with(&format!("{tool} version")),
        "executable did not identify itself as {tool}"
    );
    Ok(())
}

/// Dedicated tor controls.
static TOR_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { key: "enable_tor_support", label: "Built-in onion service", environment: "CHAN_TOR_SUPPORT", kind: InputKind::Boolean, value: |c| c.enable_tor_support.to_string(), help: "Enable the built-in Arti onion service. Disabling it removes onion access after restart." },
    SettingDefinition { key: "tor_only", label: "Tor-only listener binding", environment: "CHAN_TOR_ONLY", kind: InputKind::Boolean, value: |c| c.tor_only.to_string(), help: "Requires Tor support and forces the primary listener to loopback. Review current clearnet access before restarting. Cannot use ACME." },
    SettingDefinition { key: "tor_bootstrap_timeout_secs", label: "Tor bootstrap timeout (seconds)", environment: "CHAN_TOR_BOOTSTRAP_TIMEOUT", kind: InputKind::Number(1, 3600), value: |c| c.tor_bootstrap_timeout_secs.to_string(), help: "Deadline per bootstrap attempt; increase on censored networks. Default: 120 seconds." },
    SettingDefinition { key: "tor_max_concurrent_streams", label: "Concurrent Tor streams", environment: "CHAN_TOR_MAX_STREAMS", kind: InputKind::Number(1, 65536), value: |c| c.tor_max_concurrent_streams.to_string(), help: "Each stream holds a file descriptor. Excess streams are dropped. Default: 512." },
    SettingDefinition { key: "tor_service_nickname", label: "Onion-service identity nickname", environment: "CHAN_TOR_NICKNAME", kind: InputKind::Text, value: |c| c.tor_service_nickname.clone(), help: "Changing this selects a different identity in the Tor keystore and can change the onion address. Back up the identity first." },
];

/// Dedicated media controls.
static MEDIA_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { key: "max_image_size_mb", label: "New-board image upload default (MiB)", environment: "CHAN_MAX_IMAGE_MB", kind: InputKind::Number(1, 100), value: |c| (c.max_image_size / (1024 * 1024)).to_string(), help: "Upload limit for newly created boards. Existing boards retain their own caps. New-board PDFs also derive their cap from this default." },
    SettingDefinition { key: "max_video_size_mb", label: "New-board video upload default (MiB)", environment: "CHAN_MAX_VIDEO_MB", kind: InputKind::Number(1, 2048), value: |c| (c.max_video_size / (1024 * 1024)).to_string(), help: "Upload limit for newly created boards; edit existing per-board caps under Boards." },
    SettingDefinition { key: "max_audio_size_mb", label: "New-board audio upload default (MiB)", environment: "CHAN_MAX_AUDIO_MB", kind: InputKind::Number(1, 512), value: |c| (c.max_audio_size / (1024 * 1024)).to_string(), help: "Upload limit for newly created boards; existing boards retain their stored caps." },
    SettingDefinition { key: "enable_any_file_uploads_feature", label: "Master arbitrary-file upload gate", environment: "CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE", kind: InputKind::Boolean, value: |c| c.enable_any_file_uploads_feature.to_string(), help: "Both this global gate and the per-board arbitrary-file checkbox must be enabled. This does not change individual board preferences." },
    SettingDefinition { key: "require_ffmpeg", label: "Require FFmpeg at startup", environment: "CHAN_REQUIRE_FFMPEG", kind: InputKind::Boolean, value: |c| c.require_ffmpeg.to_string(), help: "Missing FFmpeg becomes a startup error. Internal Rust image, metadata, PDF and common-audio processing does not need FFmpeg." },
    SettingDefinition { key: "ffmpeg_path", label: "FFmpeg executable", environment: "CHAN_FFMPEG_PATH", kind: InputKind::Text, value: |c| c.ffmpeg_path.clone(), help: "Use ffmpeg for PATH lookup, or an absolute executable path. New tool paths are checked with a bounded version probe before saving." },
    SettingDefinition { key: "thumb_size", label: "Generated thumbnail dimension (pixels)", environment: "CHAN_THUMB_SIZE", kind: InputKind::Number(16, 4096), value: |c| c.thumb_size.to_string(), help: "Applies to thumbnails generated after restart. Existing thumbnails are not regenerated." },
    SettingDefinition { key: "job_queue_capacity", label: "Background queue capacity (pending jobs)", environment: "CHAN_JOB_QUEUE_CAPACITY", kind: InputKind::Number(0, 1_000_000), value: |c| c.job_queue_capacity.to_string(), help: "0 is unlimited. Once full, the queue drops new media jobs with a warning. Current queue pressure is shown in Site Health." },
    SettingDefinition { key: "waveform_cache_max_mb", label: "Thumbnail/waveform cache budget (MiB)", environment: "CHAN_WAVEFORM_CACHE_MAX_MB", kind: InputKind::Number(0, 1_048_576), value: |c| (c.waveform_cache_max_bytes / (1024 * 1024)).to_string(), help: "Oldest cache files are evicted by a background task when the budget is exceeded. 0 disables eviction." },
    SettingDefinition { key: "archive_before_prune", label: "Always archive overflow threads", environment: "CHAN_ARCHIVE_BEFORE_PRUNE", kind: InputKind::Boolean, value: |c| c.archive_before_prune.to_string(), help: "Overrides per-board overflow deletion/archive policy when enabled. Boards still retain their stored allow_archive preference." },
    SettingDefinition { key: "media_reconcile_repair_enabled", label: "Permit safe reconciliation repairs", environment: "CHAN_MEDIA_RECONCILE_REPAIR_ENABLED", kind: InputKind::Boolean, value: |c| c.media_reconcile_repair_enabled.to_string(), help: "Separate repair permission from periodic audits. Only bounded, verified safe repairs are scheduled." },
    SettingDefinition { key: "media_reconcile_interval_hours", label: "Reconciliation audit interval (hours)", environment: "CHAN_MEDIA_RECONCILE_INTERVAL_HOURS", kind: InputKind::Number(0, 8760), value: |c| c.media_reconcile_interval_hours.to_string(), help: "0 disables periodic audit passes. Manual maintenance remains available." },
    SettingDefinition { key: "media_reconcile_files_per_pass", label: "Filesystem entries per audit pass", environment: "CHAN_MEDIA_RECONCILE_FILES_PER_PASS", kind: InputKind::Number(0, 1_000_000), value: |c| c.media_reconcile_files_per_pass.to_string(), help: "Bound the filesystem work performed by each reconciliation pass." },
    SettingDefinition { key: "media_reconcile_database_rows_per_pass", label: "Reference rows per audit pass", environment: "CHAN_MEDIA_RECONCILE_DATABASE_ROWS_PER_PASS", kind: InputKind::Number(0, 10_000_000), value: |c| c.media_reconcile_database_rows_per_pass.to_string(), help: "Bound the authoritative-reference snapshot used by each pass." },
    SettingDefinition { key: "media_reconcile_hash_bytes_per_pass", label: "Bytes hashed per audit pass", environment: "CHAN_MEDIA_RECONCILE_HASH_BYTES_PER_PASS", kind: InputKind::Number(0, 1_099_511_627_776), value: |c| c.media_reconcile_hash_bytes_per_pass.to_string(), help: "I/O budget in bytes. Default: 67108864 bytes (64 MiB)." },
    SettingDefinition { key: "media_reconcile_repairs_per_pass", label: "Repair attempts per audit pass", environment: "CHAN_MEDIA_RECONCILE_REPAIRS_PER_PASS", kind: InputKind::Number(0, 1_000_000), value: |c| c.media_reconcile_repairs_per_pass.to_string(), help: "Used when repair permission is enabled; caps repair work per pass." },
];

/// Dedicated maintenance controls.
static MAINTENANCE_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { key: "wal_checkpoint_interval_secs", label: "WAL checkpoint interval (seconds)", environment: "CHAN_WAL_CHECKPOINT_SECS", kind: InputKind::Number(0, 31_536_000), value: |c| c.wal_checkpoint_interval.to_string(), help: "0 disables the recurring checkpoint. Existing manual database actions remain available." },
    SettingDefinition { key: "auto_vacuum_interval_hours", label: "Automatic VACUUM interval (hours)", environment: "CHAN_AUTO_VACUUM_HOURS", kind: InputKind::Number(0, 8760), value: |c| c.auto_vacuum_interval_hours.to_string(), help: "0 disables recurring VACUUM. Manual VACUUM is available in database maintenance." },
    SettingDefinition { key: "poll_cleanup_interval_hours", label: "Expired poll-vote cleanup interval (hours)", environment: "CHAN_POLL_CLEANUP_HOURS", kind: InputKind::Number(0, 8760), value: |c| c.poll_cleanup_interval_hours.to_string(), help: "0 disables periodic cleanup. Default: 72 hours." },
    SettingDefinition { key: "db_warn_threshold_mb", label: "Database size warning threshold (MiB)", environment: "CHAN_DB_WARN_THRESHOLD_MB", kind: InputKind::Number(0, 1_048_576), value: |c| (c.db_warn_threshold_bytes / (1024 * 1024)).to_string(), help: "0 disables the admin warning. This warns about size; it does not cap or delete database contents." },
];

/// Dedicated system controls.
static SYSTEM_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { key: "blocking_threads", label: "Tokio blocking-pool size (threads)", environment: "CHAN_BLOCKING_THREADS", kind: InputKind::Number(0, 4096), value: |c| c.blocking_threads.to_string(), help: "0 selects logical CPUs × 4 (16 if CPU detection is unavailable). Active and saved state show the resolved count. Restart required." },
    SettingDefinition { key: "db_pool_size", label: "SQLite connection-pool size", environment: "CHAN_DB_POOL_SIZE", kind: InputKind::Number(1, 128), value: |c| c.db_pool_size.to_string(), help: "Default: 8 connections. Each connection has roughly 32 MiB of page cache; increasing this can substantially raise memory use." },
];

#[cfg(test)]
mod tests {
    use super::{probe_tool, validate_section, validate_tool_path, RuntimeSection, SECTIONS};
    use crate::config::admin::{
        parse_settings_form, resolve_file, save_root_at, BTreeMap, Environment, NETWORK_SETTINGS,
    };
    use anyhow::ensure;

    /// Build a full form with file defaults, retaining automatic blocking sizing.
    fn form_for(section: RuntimeSection) -> anyhow::Result<BTreeMap<String, String>> {
        let config = resolve_file(
            "enable_tor_support = false\n",
            &Environment::Values(&BTreeMap::new()),
        )?;
        Ok(section
            .definitions()
            .iter()
            .map(|field| {
                (
                    field.key.to_owned(),
                    if field.key == "blocking_threads" {
                        "0".to_owned()
                    } else if field.key == "log_filter" {
                        String::new()
                    } else {
                        (field.value)(&config)
                    },
                )
            })
            .collect())
    }

    #[test]
    fn registry_covers_inventoried_settings_and_new_operator_choices_without_duplicates(
    ) -> anyhow::Result<()> {
        let mut keys = std::collections::BTreeSet::new();
        for field in NETWORK_SETTINGS.iter().chain(super::all_definitions()) {
            ensure!(
                keys.insert(field.key),
                "duplicate runtime control {}",
                field.key
            );
        }
        ensure!(
            keys.len() == 62,
            "registry must cover 38 inventoried root controls, 12 TLS leaves and 12 new operator choices"
        );
        let config = resolve_file("", &Environment::Values(&BTreeMap::new()))?;
        ensure!(
            config.network_sources.len() == keys.len() + 2
                && keys
                    .iter()
                    .all(|key| config.network_sources.contains_key(key))
                && ["database_path", "upload_dir"]
                    .iter()
                    .all(|key| config.network_sources.contains_key(key)),
            "every control and both managed storage paths need captured startup provenance"
        );
        Ok(())
    }

    #[test]
    fn every_group_roundtrips_through_the_actual_loader_without_mutating_runtime_state(
    ) -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        for section in SECTIONS.iter().filter(|s| **s != RuntimeSection::Https) {
            let before = "# operator comment\nenable_tor_support = false\n";
            std::fs::write(&path, before)?;
            let active = resolve_file(before, &Environment::Values(&BTreeMap::new()))?;
            let mut form = form_for(*section)?;
            let (key, replacement) = match section {
                RuntimeSection::Tor => ("tor_bootstrap_timeout_secs", "300"),
                RuntimeSection::Media => ("thumb_size", "512"),
                RuntimeSection::Maintenance => ("db_warn_threshold_mb", "3000"),
                RuntimeSection::System => ("db_pool_size", "12"),
                RuntimeSection::Https => anyhow::bail!("TLS has dedicated tests"),
                RuntimeSection::Access => ("admin_login_fail_limit", "7"),
                RuntimeSection::Display => ("index_threads_per_page", "12"),
                RuntimeSection::Logging => ("log_filter", "warn"),
                RuntimeSection::Timeouts => ("read_timeout_secs", "45"),
            };
            form.insert(key.to_owned(), replacement.to_owned());
            let updates = parse_settings_form(section.definitions(), &form)?;
            save_root_at(
                &path,
                &updates,
                &Environment::Values(&BTreeMap::new()),
                |config| validate_section(*section, config),
            )?;
            let after = std::fs::read_to_string(&path)?;
            let saved = resolve_file(&after, &Environment::Values(&BTreeMap::new()))?;
            ensure!(
                after.contains("# operator comment"),
                "each save must retain unrelated comments"
            );
            for field in section.definitions() {
                if field.key != "blocking_threads" {
                    ensure!(
                        (field.value)(&saved)
                            == *form
                                .get(field.key)
                                .ok_or_else(|| anyhow::anyhow!("missing field"))?,
                        "runtime does not read saved {}",
                        field.key
                    );
                }
            }
            let field = section
                .definitions()
                .iter()
                .find(|f| f.key == key)
                .ok_or_else(|| anyhow::anyhow!("missing control"))?;
            ensure!(
                (field.value)(&active) != replacement,
                "saved change must not alter the running configuration"
            );
            if *section == RuntimeSection::System {
                ensure!(
                    after.contains("blocking_threads = 0"),
                    "automatic sizing must remain automatic"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn invalid_groups_and_effective_overrides_leave_files_untouched() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        let before = "enable_tor_support = false\n";
        std::fs::write(&path, before)?;
        for (section, key, invalid) in [
            (RuntimeSection::Tor, "tor_only", "true"),
            (RuntimeSection::Tor, "tor_service_nickname", "invalid/name"),
            (RuntimeSection::Media, "thumb_size", "0"),
            (
                RuntimeSection::Maintenance,
                "auto_vacuum_interval_hours",
                "8761",
            ),
            (RuntimeSection::System, "db_pool_size", "0"),
        ] {
            let mut form = form_for(section)?;
            form.insert(key.to_owned(), invalid.to_owned());
            let result = parse_settings_form(section.definitions(), &form).and_then(|updates| {
                save_root_at(
                    &path,
                    &updates,
                    &Environment::Values(&BTreeMap::new()),
                    |config| validate_section(section, config),
                )
            });
            ensure!(result.is_err(), "invalid {key} must be rejected");
            ensure!(
                std::fs::read_to_string(&path)? == before,
                "no invalid group may partially save"
            );
        }
        let updates = parse_settings_form(
            RuntimeSection::System.definitions(),
            &form_for(RuntimeSection::System)?,
        )?;
        let overrides = BTreeMap::from([("CHAN_DB_POOL_SIZE".to_owned(), "0".to_owned())]);
        ensure!(
            save_root_at(
                &path,
                &updates,
                &Environment::Values(&overrides),
                |config| validate_section(RuntimeSection::System, config)
            )
            .is_err(),
            "bad effective environment must fail before mutation"
        );
        ensure!(
            std::fs::read_to_string(path)? == before,
            "bad effective override must preserve bytes"
        );
        Ok(())
    }

    #[test]
    fn tool_paths_reject_arguments_relative_paths_and_traversal_before_execution(
    ) -> anyhow::Result<()> {
        for path in [
            "ffmpeg -version",
            "ffmpeg; touch marker",
            "./ffmpeg",
            "/tmp/../bin/ffmpeg",
            "\nffmpeg",
        ] {
            ensure!(
                validate_tool_path("ffmpeg", path).is_err(),
                "unsafe executable input must be rejected: {path}"
            );
        }
        validate_tool_path("ffmpeg", "ffmpeg")?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn tool_probe_checks_identity_and_exit_status_with_disposable_executables() -> anyhow::Result<()>
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("probe");
        let executable = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("fixture path must be UTF-8"))?;
        for (script, succeeds) in [
            ("#!/bin/sh\nprintf 'ffmpeg version fixture\\n'\n", true),
            ("#!/bin/sh\nprintf 'unrelated tool\\n'\n", false),
            ("#!/bin/sh\nexit 1\n", false),
        ] {
            std::fs::write(&path, script)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            ensure!(
                probe_tool("ffmpeg", executable).is_ok() == succeeds,
                "probe must verify tool identity and success status"
            );
        }
        Ok(())
    }
}
