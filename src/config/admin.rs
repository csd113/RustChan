//! Mutation-free configuration previews and validated, comment-preserving settings saves.

/// Retained live settings state.
pub mod application;
/// HTTPS and certificate-management validation.
pub mod certificates;
/// Storage state and secret rotation.
pub mod management;
/// Restart-required Tor, media, maintenance and process controls.
pub mod runtime;

use super::{Config, Environment, RateLimitPolicy, SettingsFile, CONFIG};
use anyhow::{ensure, Context as _};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

/// Serializes all in-process settings writers through read/validate/replace.
pub(super) static SETTINGS_WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Supported input shapes for network controls.
#[derive(Debug, Clone, Copy)]
pub enum InputKind {
    /// Boolean selection; cookie policy also permits automatic selection.
    Boolean,
    /// Bounded integer input.
    Number(u64, u64),
    /// Comma or newline separated validated list.
    List,
    /// Listener address; empty means compose the interface and primary port.
    Address,
    /// Request-counting policy selection.
    Policy,
    /// Validated nickname or executable path.
    Text,
    /// Optional string; blank removes the file value.
    OptionalText,
}

/// Definition shared by rendering, input validation and coverage checks.
#[derive(Debug)]
pub struct SettingDefinition {
    /// Authoritative settings-file key.
    pub key: &'static str,
    /// Human-readable label and units.
    pub label: &'static str,
    /// Contextual help and interactions.
    pub help: &'static str,
    /// Direct environment override.
    pub environment: &'static str,
    /// Input shape and limits.
    pub kind: InputKind,
    /// Getter for the active or resolved saved configuration.
    pub(in crate::config) value: fn(&Config) -> String,
}

/// Network settings; default request policy preserves the historical behavior.
pub static NETWORK_SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { key: "behind_proxy", label: "Trust reverse-proxy headers", help: "For Serveo or another reverse proxy. Forwarded visitor identity and HTTPS are accepted only from trusted peers. Configure the proxy to replace client-supplied forwarding headers.", environment: "CHAN_BEHIND_PROXY", kind: InputKind::Boolean, value: |c| c.behind_proxy.to_string() },
    SettingDefinition { key: "trusted_proxy_cidrs", label: "Trusted proxy networks (CIDRs)", help: "One CIDR per line or separated by commas. Trust only your proxy peers; a broad allowlist lets those peers supply visitor identity for bans and request counters.", environment: "CHAN_TRUSTED_PROXY_CIDRS", kind: InputKind::List, value: |c| c.trusted_proxy_cidrs.join(", ") },
    SettingDefinition { key: "public_hosts", label: "Public hostnames", help: "Bare hostnames or IP literals, without scheme, port or path. The first hostname is the displayed public entry point. Used by host checks and HTTPS redirects.", environment: "CHAN_PUBLIC_HOSTS", kind: InputKind::List, value: |c| c.public_hosts.join(", ") },
    SettingDefinition { key: "https_cookies", label: "Secure authentication cookies", help: "Automatic enables the policy for proxy mode or native HTTPS. Secure cookies are issued for native HTTPS or trusted forwarded HTTPS, preserving plain HTTP access behavior.", environment: "CHAN_HTTPS_COOKIES", kind: InputKind::Boolean, value: |c| c.https_cookies.to_string() },
    SettingDefinition { key: "port", label: "Primary HTTP port", help: "CHAN_PORT overrides this default. An explicit listen address supplies its own port. A launcher --port/-p overrides the final listener port, including CHAN_BIND.", environment: "CHAN_PORT", kind: InputKind::Number(1, 65535), value: |c| c.port.to_string() },
    SettingDefinition { key: "bind_addr", label: "Listen address (IP:port)", help: "Leave blank for 0.0.0.0 with the primary HTTP port. Use [::]:8080 for IPv6. CHAN_BIND overrides this field; CHAN_HOST overrides the saved or automatic interface. A launcher --port/-p overrides this address’s port. Tor-only forces loopback. Listener changes require a service restart.", environment: "CHAN_BIND", kind: InputKind::Address, value: |c| c.bind_addr.clone() },
    SettingDefinition { key: "rate_limit_gets", label: "Browsing request allowance (per visitor/window)", help: "Raise this if visitors frequently see ‘slow down’. Counters use the trusted forwarded identity or the direct peer. Posting cooldowns and password-attempt protection remain separate.", environment: "CHAN_RATE_GETS", kind: InputKind::Number(1, 1_000_000), value: |c| c.rate_limit_gets.to_string() },
    SettingDefinition { key: "rate_limit_window", label: "Browsing window (seconds)", help: "The historical counter rolls over when elapsed whole seconds are greater than this window. Changing it takes effect on restart.", environment: "CHAN_RATE_WINDOW", kind: InputKind::Number(1, 86400), value: |c| c.rate_limit_window.to_string() },
    SettingDefinition { key: "rate_limit_policy", label: "Requests counted by the browsing limiter", help: "Legacy counts all methods, including writes, API polling, favicons and banners. Reads counts GET/HEAD only; write-specific protections still apply. Both exempt /static/, /theme-css/, /boards/, admin live logs and backup progress.", environment: "CHAN_RATE_POLICY", kind: InputKind::Policy, value: |c| c.rate_limit_policy.as_str().to_owned() },
    SettingDefinition { key: "session_duration", label: "Administrator session lifetime (seconds)", help: "Applies to sessions created after restart. Existing sessions keep their stored expiry. Default: 28800 seconds (8 hours).", environment: "CHAN_SESSION_SECS", kind: InputKind::Number(60, 2_592_000), value: |c| c.session_duration.to_string() },
    SettingDefinition { key: "public_readiness_details", label: "Public detailed readiness", help: "Expose operational details at /readyz. /healthz remains minimal and public.", environment: "CHAN_PUBLIC_READINESS_DETAILS", kind: InputKind::Boolean, value: |c| c.public_readiness_details.to_string() },
    SettingDefinition { key: "public_metrics_enabled", label: "Public Prometheus metrics", help: "Expose unauthenticated /metrics. Use a trusted scrape path if enabled.", environment: "CHAN_PUBLIC_METRICS_ENABLED", kind: InputKind::Boolean, value: |c| c.public_metrics_enabled.to_string() },
];

/// One control with saved, effective and active state; never includes secrets.
#[derive(Debug)]
pub struct SettingField {
    /// Shared input definition.
    pub definition: &'static SettingDefinition,
    /// Editable file value (blank denotes automatic fallback).
    pub input: String,
    /// Actual running value.
    pub active: String,
    /// Resolved file value with environment overrides removed.
    pub saved: String,
    /// Running source or overriding operator input.
    pub source: String,
    /// Effective value after restart with current environment overrides.
    pub next_start: String,
    /// Effective next-start value differs from the running value.
    pub pending: bool,
    /// An override or derived policy prevents the file value taking effect.
    pub overridden: bool,
}

/// Read a secret-safe status snapshot after admin authorization.
///
/// # Errors
/// Returns an error when settings cannot be read, validated or persisted safely.
pub fn network_snapshot() -> anyhow::Result<Vec<SettingField>> {
    let content =
        std::fs::read_to_string(super::settings_file_path()).context("read settings file")?;
    snapshot_from(&content, &CONFIG, NETWORK_SETTINGS)
}

/// Look up a dotted setting without treating it as a literal TOML key.
pub(super) fn file_value<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(root, |value, component| value.get(component))
}

/// Build field state independently of process-global configuration.
fn snapshot_from(
    content: &str,
    active: &Config,
    definitions: &'static [SettingDefinition],
) -> anyhow::Result<Vec<SettingField>> {
    let raw: toml::Value = toml::from_str(content)
        .map_err(|_| anyhow::anyhow!("settings.toml is invalid; repair it before saving"))?;
    let saved = resolve_file(content, &Environment::Values(&BTreeMap::new()))?;
    let next = resolve_file(content, &Environment::Process)?;
    Ok(definitions
        .iter()
        .map(|definition| {
            let active_value = (definition.value)(active);
            let saved_value = (definition.value)(&saved);
            let input = match (definition.kind, file_value(&raw, definition.key)) {
                (InputKind::Number(..), Some(value)) => value.to_string(),
                (InputKind::Address | InputKind::Text | InputKind::OptionalText, Some(value)) => {
                    value.as_str().unwrap_or_default().to_owned()
                }
                (_, None) if definition.key == "blocking_threads" => "0".to_owned(),
                (InputKind::Address | InputKind::Boolean, None)
                    if definition.key == "bind_addr" || definition.key == "https_cookies" =>
                {
                    String::new()
                }
                (InputKind::OptionalText, None) => String::new(),
                _ => saved_value.clone(),
            };
            let source = setting_source(definition, active);
            let next_start = (definition.value)(&next);
            SettingField {
                definition,
                input,
                pending: active_value != next_start,
                overridden: saved_value != next_start,
                next_start,
                active: active_value,
                saved: saved_value,
                source,
            }
        })
        .collect())
}

/// Capture startup provenance before any admin edits change the file.
pub(super) fn startup_network_sources(
    settings: &SettingsFile,
    environment: &Environment<'_>,
    cli_port: Option<u16>,
) -> BTreeMap<&'static str, String> {
    NETWORK_SETTINGS
        .iter()
        .chain(runtime::all_definitions())
        .map(|definition| {
            let in_file = match definition.key {
                "behind_proxy" => settings.behind_proxy.is_some(),
                "trusted_proxy_cidrs" => settings.trusted_proxy_cidrs.is_some(),
                "public_hosts" => settings.public_hosts.is_some(),
                "https_cookies" => settings.https_cookies.is_some(),
                "port" => settings.port.is_some(),
                "bind_addr" => settings.bind_addr.is_some(),
                "rate_limit_gets" => settings.rate_limit_gets.is_some(),
                "rate_limit_window" => settings.rate_limit_window.is_some(),
                "rate_limit_policy" => settings.rate_limit_policy.is_some(),
                "session_duration" => settings.session_duration.is_some(),
                "public_readiness_details" => settings.public_readiness_details.is_some(),
                "public_metrics_enabled" => settings.public_metrics_enabled.is_some(),
                "enable_tor_support" => settings.enable_tor_support.is_some(),
                "tor_only" => settings.tor_only.is_some(),
                "tor_bootstrap_timeout_secs" => settings.tor_bootstrap_timeout_secs.is_some(),
                "tor_max_concurrent_streams" => settings.tor_max_concurrent_streams.is_some(),
                "tor_service_nickname" => settings.tor_service_nickname.is_some(),
                "max_image_size_mb" => settings.max_image_size_mb.is_some(),
                "max_video_size_mb" => settings.max_video_size_mb.is_some(),
                "max_audio_size_mb" => settings.max_audio_size_mb.is_some(),
                "enable_any_file_uploads_feature" => {
                    settings.enable_any_file_uploads_feature.is_some()
                }
                "require_ffmpeg" => settings.require_ffmpeg.is_some(),
                "ffmpeg_path" => settings.ffmpeg_path.is_some(),
                "ffprobe_path" => settings.ffprobe_path.is_some(),
                "thumb_size" => settings.thumb_size.is_some(),
                "job_queue_capacity" => settings.job_queue_capacity.is_some(),
                "waveform_cache_max_mb" => settings.waveform_cache_max_mb.is_some(),
                "archive_before_prune" => settings.archive_before_prune.is_some(),
                "media_reconcile_repair_enabled" => {
                    settings.media_reconcile_repair_enabled.is_some()
                }
                "media_reconcile_interval_hours" => {
                    settings.media_reconcile_interval_hours.is_some()
                }
                "media_reconcile_files_per_pass" => {
                    settings.media_reconcile_files_per_pass.is_some()
                }
                "media_reconcile_database_rows_per_pass" => {
                    settings.media_reconcile_database_rows_per_pass.is_some()
                }
                "media_reconcile_hash_bytes_per_pass" => {
                    settings.media_reconcile_hash_bytes_per_pass.is_some()
                }
                "media_reconcile_repairs_per_pass" => {
                    settings.media_reconcile_repairs_per_pass.is_some()
                }
                "wal_checkpoint_interval_secs" => settings.wal_checkpoint_interval_secs.is_some(),
                "auto_vacuum_interval_hours" => settings.auto_vacuum_interval_hours.is_some(),
                "poll_cleanup_interval_hours" => settings.poll_cleanup_interval_hours.is_some(),
                "db_warn_threshold_mb" => settings.db_warn_threshold_mb.is_some(),
                "blocking_threads" => settings.blocking_threads.is_some(),
                "db_pool_size" => settings.db_pool_size.is_some(),
                key if key.starts_with("tls.") => settings.tls_explicit_keys.contains(key),
                _ => settings.operator.contains(definition.key),
            };
            let source = if cli_port.is_some() && matches!(definition.key, "port" | "bind_addr") {
                if definition.key == "bind_addr" {
                    "CLI --port/-p overrides listener port; interface from CHAN_BIND / CHAN_HOST / file / default".to_owned()
                } else {
                    "CLI --port/-p (overrides file/environment port)".to_owned()
                }
            } else if environment.var_os(definition.environment).is_some()
                && !environment_override_is_valid(definition, environment)
            {
                format!(
                    "Environment: {} present but invalid; file/default fallback used",
                    definition.environment
                )
            } else if environment.var_os(definition.environment).is_some() {
                format!(
                    "Environment: {} (overrides saved value)",
                    definition.environment
                )
            } else if definition.key == "bind_addr" && environment.var_os("CHAN_HOST").is_some() {
                "Environment: CHAN_HOST overrides listener interface".to_owned()
            } else if in_file {
                "settings.toml at startup".to_owned()
            } else {
                "Default / derived at startup".to_owned()
            };
            (definition.key, source)
        })
        .collect()
}

/// Mirror loader parse fallback so invalid overrides are not reported as active.
pub(super) fn environment_override_is_valid(
    definition: &SettingDefinition,
    environment: &Environment<'_>,
) -> bool {
    let Ok(value) = environment.var(definition.environment) else {
        return false;
    };
    if definition.key == "auto_full_backup_storage_mode" {
        return matches!(value.as_str(), "directory" | "split_zip");
    }
    match definition.kind {
        InputKind::Number(..) if definition.key == "session_duration" => {
            value.parse::<i64>().is_ok()
        }
        InputKind::Number(..)
            if matches!(
                definition.key,
                "port"
                    | "max_image_size_mb"
                    | "max_video_size_mb"
                    | "max_audio_size_mb"
                    | "db_pool_size"
                    | "thumb_size"
                    | "rate_limit_gets"
                    | "admin_login_fail_limit"
                    | "board_password_fail_limit"
            ) =>
        {
            if definition.key == "port" {
                value.parse::<u16>().is_ok()
            } else {
                value.parse::<u32>().is_ok()
            }
        }
        InputKind::Number(..)
            if matches!(
                definition.key,
                "tor_max_concurrent_streams"
                    | "blocking_threads"
                    | "media_reconcile_files_per_pass"
                    | "media_reconcile_database_rows_per_pass"
                    | "media_reconcile_repairs_per_pass"
            ) =>
        {
            value.parse::<usize>().is_ok()
        }
        InputKind::Number(..) => value.parse::<u64>().is_ok(),
        InputKind::Policy => value.parse::<RateLimitPolicy>().is_ok(),
        InputKind::OptionalText if definition.key == "log_filter" => {
            tracing_subscriber::EnvFilter::try_new(value).is_ok()
        }
        _ => true,
    }
}

/// Explain immutable startup provenance and Tor's derived binding policy.
fn setting_source(definition: &SettingDefinition, active: &Config) -> String {
    let source = active
        .network_sources
        .get(definition.key)
        .cloned()
        .unwrap_or_else(|| "Default / derived at startup".to_owned());
    if definition.key == "bind_addr" && active.tor_only {
        format!("Tor-only forces loopback; {source}")
    } else {
        source
    }
}

/// Resolve file configuration without filesystem validation or secret exposure.
fn resolve_file(content: &str, environment: &Environment<'_>) -> anyhow::Result<Config> {
    let mut settings: SettingsFile = super::parse_settings_file_str(content)
        .map_err(|_| anyhow::anyhow!("settings.toml is invalid; repair it before saving"))?;
    // A preview does not use secrets. Avoid generating a fallback secret for a
    // manually authored file that omitted it; startup remains authoritative.
    settings.cookie_secret.get_or_insert_with(|| "0".repeat(64));
    Ok(Config::from_settings(settings, environment))
}

/// Parse every related network field before touching the file.
fn parse_settings_form(
    definitions: &[SettingDefinition],
    form: &BTreeMap<String, String>,
) -> anyhow::Result<BTreeMap<String, Option<toml::Value>>> {
    for key in form.keys() {
        ensure!(
            key == "_csrf" || definitions.iter().any(|field| field.key == key),
            "unknown runtime setting: {key}"
        );
    }
    definitions
        .iter()
        .map(|field| {
            let text = form
                .get(field.key)
                .with_context(|| format!("missing {}", field.label))?
                .trim();
            ensure!(
                text.len() <= 4096
                    && !text
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\r'),
                "{} contains invalid characters or is too long",
                field.label
            );
            let value = match field.kind {
                InputKind::Boolean if text.is_empty() && field.key == "https_cookies" => None,
                InputKind::Boolean => Some(toml::Value::Boolean(match text {
                    "true" => true,
                    "false" => false,
                    _ => anyhow::bail!("{} must be true or false", field.label),
                })),
                InputKind::Number(min, max) => {
                    let number: u64 = text
                        .parse()
                        .with_context(|| format!("{} must be a whole number", field.label))?;
                    ensure!(
                        (min..=max).contains(&number),
                        "{} must be between {min} and {max}",
                        field.label
                    );
                    Some(toml::Value::Integer(i64::try_from(number)?))
                }
                InputKind::List => {
                    let list = text
                        .split([',', '\n', '\r'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    ensure!(list.len() <= 128, "{} has too many entries", field.label);
                    for entry in &list {
                        if field.key == "trusted_proxy_cidrs" {
                            entry
                                .parse::<ipnet::IpNet>()
                                .with_context(|| format!("invalid trusted proxy CIDR: {entry}"))?;
                        } else {
                            ensure!(valid_public_host(entry), "invalid public hostname: {entry}");
                        }
                    }
                    Some(toml::Value::Array(
                        list.into_iter().map(toml::Value::String).collect(),
                    ))
                }
                InputKind::Address | InputKind::OptionalText if text.is_empty() => None,
                InputKind::Address => {
                    let address: std::net::SocketAddr = text
                        .parse()
                        .context("listen address must be an IP:port (use brackets for IPv6)")?;
                    ensure!(address.port() != 0, "listen port must not be zero");
                    Some(toml::Value::String(address.to_string()))
                }
                InputKind::Text | InputKind::OptionalText => {
                    ensure!(
                        !text.is_empty() && !text.chars().any(char::is_control),
                        "{} is required and must not contain control characters",
                        field.label
                    );
                    Some(toml::Value::String(text.to_owned()))
                }
                InputKind::Policy => {
                    text.parse::<RateLimitPolicy>()
                        .map_err(anyhow::Error::msg)?;
                    Some(toml::Value::String(text.to_owned()))
                }
            };
            Ok((field.key.to_owned(), value))
        })
        .collect()
}

/// Parse a complete related network form.
fn parse_network_form(
    form: &BTreeMap<String, String>,
) -> anyhow::Result<BTreeMap<String, Option<toml::Value>>> {
    parse_settings_form(NETWORK_SETTINGS, form)
}

/// Reject malformed DNS names while permitting IPv4 and IPv6 literals.
fn valid_public_host(value: &str) -> bool {
    let Some(host) = super::normalize_public_host(value) else {
        return false;
    };
    host.parse::<std::net::IpAddr>().is_ok()
        || (host.len() <= 253
            && host.split('.').all(|part| {
                !part.is_empty()
                    && part.len() <= 63
                    && !part.starts_with('-')
                    && !part.ends_with('-')
                    && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            }))
}

/// Validate listeners together with Tor and existing TLS settings, without probes.
fn validate_network(config: &Config) -> anyhow::Result<()> {
    config.operator.validate()?;
    ensure!(config.port != 0, "primary HTTP port must not be zero");
    let port = super::port_from_bind_addr(&config.bind_addr).context("invalid listen address")?;
    ensure!(port != 0, "listen port must not be zero");
    ensure!(
        !config.tor_only || config.enable_tor_support,
        "Tor-only requires Tor support"
    );
    ensure!(
        !config.tor_only || !config.tls.acme.enabled,
        "Tor-only cannot use ACME"
    );
    if config.tls.enabled {
        ensure!(
            config.tls.port != 0 && config.tls.port != port,
            "HTTPS port must be nonzero and differ from the primary listener"
        );
        if config.tls.redirect_http {
            ensure!(
                config.tls.http_port != 0
                    && config.tls.http_port != port
                    && config.tls.http_port != config.tls.port,
                "redirect port must be nonzero and differ from the HTTP and HTTPS ports"
            );
        }
    }
    ensure!(
        (1..=1_000_000).contains(&config.rate_limit_gets),
        "browsing allowance must be 1–1000000"
    );
    ensure!(
        (1..=86400).contains(&config.rate_limit_window),
        "browsing window must be 1–86400 seconds"
    );
    ensure!(
        (60..=2_592_000).contains(&config.session_duration),
        "session lifetime must be 60–2592000 seconds"
    );
    for cidr in &config.trusted_proxy_cidrs {
        cidr.parse::<ipnet::IpNet>()
            .context("invalid trusted proxy CIDR")?;
    }
    for host in &config.public_hosts {
        ensure!(valid_public_host(host), "invalid public hostname: {host}");
    }
    Ok(())
}

/// Validate saved and next-start effective configuration before atomic replacement.
///
/// # Errors
/// Returns an error when settings cannot be read, validated or persisted safely.
pub fn save_network(form: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let updates = parse_network_form(form)?;
    let _guard = SETTINGS_WRITE_LOCK.lock();
    save_network_at(
        &super::settings_file_path(),
        &updates,
        &Environment::Process,
    )
}

/// Path-explicit save used by isolated persistence tests.
fn save_network_at(
    path: &Path,
    updates: &BTreeMap<String, Option<toml::Value>>,
    environment: &Environment<'_>,
) -> anyhow::Result<()> {
    save_root_at(path, updates, environment, validate_network)
}

/// Validate both file and overridden effective state before replacing any bytes.
pub(super) fn save_root_at(
    path: &Path,
    updates: &BTreeMap<String, Option<toml::Value>>,
    environment: &Environment<'_>,
    validate: impl Fn(&Config) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "settings.toml must not be a symlink"
    );
    let before = std::fs::read_to_string(path).context("read settings.toml")?;
    let after = rewrite_root_settings(&before, updates)?;
    validate(&resolve_file(
        &after,
        &Environment::Values(&BTreeMap::new()),
    )?)?;
    validate(&resolve_file(&after, environment)?)?;
    atomic_replace(path, &after)
}

/// Coordinate setup's file update with its database commit under the writer lock.
/// Restore the original file if committing the already prepared DB transaction fails.
pub(super) fn save_root_and_commit_at(
    path: &Path,
    updates: &BTreeMap<String, Option<toml::Value>>,
    commit: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let before = std::fs::read_to_string(path).context("read setup settings before commit")?;
    save_root_at(path, updates, &Environment::Process, |config| {
        validate_network(config)?;
        config.operator.validate()
    })?;
    if let Err(error) = commit() {
        atomic_replace(path, &before).context("Setup database commit failed and settings rollback failed; stop the service and recover the verified backup before restarting")?;
        return Err(error).context("Setup database commit failed; original settings restored");
    }
    Ok(())
}

/// Replace exact TOML value spans, preserving comments, tables and multiline values.
pub(super) fn rewrite_root_settings(
    content: &str,
    updates: &BTreeMap<String, Option<toml::Value>>,
) -> anyhow::Result<String> {
    if updates.keys().any(|key| key.contains('.')) {
        return certificates::rewrite_tls(content, updates);
    }
    let spans: BTreeMap<String, toml::Spanned<toml::Value>> = toml::from_str(content)
        .map_err(|_| anyhow::anyhow!("settings.toml is invalid; no changes saved"))?;
    let mut expected: toml::Value =
        toml::from_str(content).map_err(|_| anyhow::anyhow!("invalid settings.toml"))?;
    let table = expected
        .as_table_mut()
        .context("settings root must be a table")?;
    let mut edits = Vec::new();
    let mut missing = String::new();
    for (key, value) in updates {
        if let Some(value) = value {
            table.insert(key.clone(), value.clone());
        } else {
            table.remove(key);
        }
        if let Some(span) = spans.get(key) {
            let range = span.span();
            if let Some(value) = value {
                if span.get_ref() != value {
                    edits.push((range, value.to_string()));
                }
            } else {
                let prefix = content.get(..range.start).context("invalid TOML span")?;
                let start = prefix.rfind('\n').map_or(0, |offset| offset + 1);
                edits.push((start..range.end, String::new()));
            }
        } else if let Some(value) = value {
            writeln!(missing, "{key} = {value}")?;
        }
    }
    if !missing.is_empty() {
        let mut offset = 0;
        let first_table = content
            .split_inclusive('\n')
            .find_map(|line| {
                let start = offset;
                offset += line.len();
                let within_value = spans
                    .values()
                    .any(|v| !v.get_ref().is_table() && v.span().contains(&start));
                (line.trim_start().starts_with('[') && !within_value).then_some(start)
            })
            .unwrap_or(content.len());
        if first_table > 0
            && !content
                .get(..first_table)
                .is_some_and(|s| s.ends_with('\n'))
        {
            missing.insert(0, '\n');
        }
        edits.push((first_table..first_table, missing));
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut output = content.to_owned();
    for (range, replacement) in edits {
        ensure!(
            output.get(range.clone()).is_some(),
            "invalid settings span; no changes saved"
        );
        output.replace_range(range, &replacement);
    }
    let actual: toml::Value = toml::from_str(&output)
        .map_err(|_| anyhow::anyhow!("settings rewrite failed verification; no changes saved"))?;
    ensure!(
        actual == expected,
        "settings rewrite changed unrelated values; no changes saved"
    );
    Ok(output)
}

/// Persist private, synced bytes before replacing the original settings file.
fn atomic_replace(path: &Path, content: &str) -> anyhow::Result<()> {
    let parent = path.parent().context("settings path needs a parent")?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".settings_")
        .suffix(".tmp")
        .tempfile_in(parent)
        .context("create temporary settings file")?;
    super::restrict_private_file_permissions(temporary.path())?;
    temporary
        .write_all(content.as_bytes())
        .context("write temporary settings file")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary settings file")?;
    temporary
        .persist(path)
        .map_err(|e| e.error)
        .context("atomically replace settings.toml")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_network_form, resolve_file, rewrite_root_settings, save_network_at, validate_network,
        BTreeMap, Environment, NETWORK_SETTINGS,
    };
    use anyhow::{ensure, Context as _};

    #[test]
    fn launcher_port_overrides_final_bind_and_is_validated_before_startup() -> anyhow::Result<()> {
        let values = BTreeMap::from([
            ("CHAN_BIND".to_owned(), "[::1]:8100".to_owned()),
            ("CHAN_PORT".to_owned(), "8200".to_owned()),
        ]);
        let content =
            "port = 8000\nenable_tor_support = false\n[tls]\nenabled = true\nport = 8443\n";
        let settings = super::super::parse_settings_file_str(content)?;
        let active = super::super::Config::from_settings_with_port(
            super::super::parse_settings_file_str(content)?,
            &Environment::Values(&values),
            Some(8300),
        );
        ensure!(
            active.port == 8300 && active.bind_addr == "[::1]:8300",
            "CLI must override both environment port sources and preserve IPv6 interface"
        );
        ensure!(
            active
                .network_sources
                .get("bind_addr")
                .is_some_and(|source| source.contains("CLI --port")),
            "CLI provenance must be visible"
        );
        validate_network(&active)?;
        let conflict = super::super::Config::from_settings_with_port(
            settings,
            &Environment::Values(&values),
            Some(8443),
        );
        ensure!(
            validate_network(&conflict).is_err(),
            "CLI-induced TLS conflict must fail before listeners start"
        );
        let file = resolve_file(
            "port = 8000\nenable_tor_support = false\n",
            &Environment::Values(&BTreeMap::new()),
        )?;
        ensure!(
            file.port == 8000,
            "saved preview must exclude launcher overrides"
        );
        Ok(())
    }

    #[test]
    fn failed_setup_commit_restores_exact_file_and_sql_transaction() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        let before = "# retained operator comment\nport = 8080\nenable_tor_support = false\n";
        std::fs::write(&path, before)?;
        let mut database = rusqlite::Connection::open_in_memory()?;
        database.execute_batch(
            "CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES ('original');",
        )?;
        let tx = database.transaction()?;
        tx.execute("UPDATE sample SET value='pending'", [])?;
        let changes = BTreeMap::from([("port".to_owned(), Some(toml::Value::Integer(9000)))]);
        let result = super::save_root_and_commit_at(&path, &changes, move || {
            drop(tx);
            anyhow::bail!("injected commit failure")
        });
        ensure!(result.is_err(), "commit failure must not report success");
        ensure!(
            std::fs::read_to_string(&path)? == before,
            "failed commit must restore exact settings bytes"
        );
        let value: String = database.query_row("SELECT value FROM sample", [], |row| row.get(0))?;
        ensure!(
            value == "original",
            "SQL mutation must roll back with settings"
        );
        Ok(())
    }

    /// Build a valid complete no-environment form without loading global CONFIG.
    fn valid_form() -> anyhow::Result<BTreeMap<String, String>> {
        let empty = BTreeMap::new();
        let config = resolve_file("enable_tor_support = false\n", &Environment::Values(&empty))?;
        Ok(NETWORK_SETTINGS
            .iter()
            .map(|field| {
                (
                    field.key.to_owned(),
                    if matches!(field.key, "bind_addr" | "https_cookies") {
                        String::new()
                    } else {
                        (field.value)(&config)
                    },
                )
            })
            .collect())
    }

    #[test]
    fn full_form_validates_lists_booleans_bounds_and_unknown_keys() -> anyhow::Result<()> {
        let form = valid_form()?;
        let valid = parse_network_form(&form)?;
        ensure!(
            valid.len() == NETWORK_SETTINGS.len(),
            "complete form must include every network field"
        );
        for (key, input) in [
            ("rate_limit_gets", "0"),
            ("rate_limit_window", "86401"),
            ("session_duration", "-1"),
            ("port", "65536"),
            ("behind_proxy", "yes"),
            ("trusted_proxy_cidrs", "127.0.0.1/32, bad"),
            ("public_hosts", "https://example.com"),
            ("public_hosts", "bad_.example.com"),
            ("bind_addr", "127.0.0.1:0"),
            ("rate_limit_policy", "none"),
        ] {
            let mut invalid = form.clone();
            invalid.insert(key.to_owned(), input.to_owned());
            ensure!(
                parse_network_form(&invalid).is_err(),
                "must reject {key}={input}"
            );
        }
        let mut invalid = form.clone();
        invalid.insert("cookie_secret".to_owned(), "replacement".to_owned());
        ensure!(
            parse_network_form(&invalid).is_err(),
            "network form must not accept secret rotation"
        );
        let mut missing = form;
        missing.remove("trusted_proxy_cidrs");
        ensure!(
            parse_network_form(&missing).is_err(),
            "partial form must not save"
        );
        Ok(())
    }

    #[test]
    fn span_rewrite_preserves_multiline_comments_prefix_keys_and_nested_tables(
    ) -> anyhow::Result<()> {
        let before = "# operator comment\nport = 8080 # keep this\nport_extra = 7\npublic_hosts = [\n  'old.example.com', # old\n]\nsite_subtitle = '''\n[not_a_table]\ntext\n'''\n[tls]\nport = 8443\n";
        let updates = BTreeMap::from([
            ("port".to_owned(), Some(toml::Value::Integer(9000))),
            (
                "public_hosts".to_owned(),
                Some(toml::Value::Array(vec![toml::Value::String(
                    "new.example.com".to_owned(),
                )])),
            ),
            (
                "rate_limit_window".to_owned(),
                Some(toml::Value::Integer(90)),
            ),
        ]);
        let after = rewrite_root_settings(before, &updates)?;
        ensure!(
            after.contains("port = 9000 # keep this"),
            "inline comment must survive"
        );
        ensure!(
            after.contains("port_extra = 7"),
            "prefix key must remain untouched"
        );
        ensure!(
            after.contains("[tls]\nport = 8443"),
            "nested same-name key must remain untouched"
        );
        ensure!(
            after.contains("[not_a_table]\ntext"),
            "multiline string must survive insertion"
        );
        ensure!(
            rewrite_root_settings("bad toml", &updates).is_err(),
            "invalid original file must fail closed"
        );
        Ok(())
    }

    #[test]
    fn automatic_fields_can_be_removed_without_changing_comments() -> anyhow::Result<()> {
        let before = "bind_addr = '127.0.0.1:9000' # interface\nhttps_cookies = false # policy\nport = 8080\n";
        let updates = BTreeMap::from([
            ("bind_addr".to_owned(), None),
            ("https_cookies".to_owned(), None),
        ]);
        let after = rewrite_root_settings(before, &updates)?;
        ensure!(
            after.contains("# interface") && after.contains("# policy"),
            "removing overrides must retain comments"
        );
        let empty = BTreeMap::new();
        let config = resolve_file(&after, &Environment::Values(&empty))?;
        ensure!(
            config.bind_addr == "0.0.0.0:8080",
            "automatic binding must compose the primary port"
        );
        Ok(())
    }

    #[test]
    fn environment_overrides_file_and_startup_provenance_survives_later_edits() -> anyhow::Result<()>
    {
        let overrides = BTreeMap::from([
            ("CHAN_RATE_GETS".to_owned(), "999".to_owned()),
            ("CHAN_BIND".to_owned(), "127.0.0.1:9100".to_owned()),
            ("CHAN_SESSION_SECS".to_owned(), "3600".to_owned()),
        ]);
        let config = resolve_file(
            "port = 9000\nrate_limit_gets = 120\nsession_duration = 7200\n",
            &Environment::Values(&overrides),
        )?;
        ensure!(
            config.rate_limit_gets == 999
                && config.bind_addr == "127.0.0.1:9100"
                && config.session_duration == 3600,
            "environment must override saved settings"
        );
        ensure!(
            config
                .network_sources
                .get("rate_limit_gets")
                .is_some_and(|s| s.contains("CHAN_RATE_GETS")),
            "startup source must record override names"
        );
        ensure!(
            config
                .network_sources
                .get("port")
                .is_some_and(|s| s.contains("settings.toml")),
            "file source must be captured at startup"
        );
        let defaults = resolve_file("", &Environment::Values(&BTreeMap::new()))?;
        ensure!(
            defaults.rate_limit_gets == 60
                && defaults.rate_limit_window == 60
                && defaults.rate_limit_policy == super::RateLimitPolicy::Legacy,
            "existing defaults must be preserved"
        );
        Ok(())
    }

    #[test]
    fn previously_environment_only_fields_are_actually_loaded_from_toml() -> anyhow::Result<()> {
        let config = resolve_file("database_path = '/tmp/disposable/chan.db'\nupload_dir = '/tmp/disposable/boards'\nthumb_size = 300\nmedia_reconcile_repair_enabled = true\nmedia_reconcile_interval_hours = 12\nmedia_reconcile_files_per_pass = 100\nmedia_reconcile_database_rows_per_pass = 200\nmedia_reconcile_hash_bytes_per_pass = 300\nmedia_reconcile_repairs_per_pass = 4\n", &Environment::Values(&BTreeMap::new()))?;
        ensure!(
            config.database_path == "/tmp/disposable/chan.db"
                && config.upload_dir == "/tmp/disposable/boards"
                && config.thumb_size == 300,
            "storage and thumbnail values must be loaded"
        );
        ensure!(
            config.media_reconcile_repair_enabled
                && config.media_reconcile_interval_hours == 12
                && config.media_reconcile_files_per_pass == 100
                && config.media_reconcile_database_rows_per_pass == 200
                && config.media_reconcile_hash_bytes_per_pass == 300
                && config.media_reconcile_repairs_per_pass == 4,
            "all reconciliation values must be loaded"
        );
        Ok(())
    }

    #[test]
    fn atomic_save_validates_before_mutation_and_does_not_change_active_config(
    ) -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        let before = "# preserved\nport = 8080\nrate_limit_gets = 60\nenable_tor_support = false\n[tls]\nenabled = true\nport = 8443\n";
        std::fs::write(&path, before)?;
        let mut form = valid_form()?;
        form.insert("port".to_owned(), "8443".to_owned());
        let invalid = parse_network_form(&form)?;
        let empty = BTreeMap::new();
        ensure!(
            save_network_at(&path, &invalid, &Environment::Values(&empty)).is_err(),
            "conflicting listener must fail"
        );
        ensure!(
            std::fs::read_to_string(&path)? == before,
            "failure must leave original bytes intact"
        );
        form.insert("port".to_owned(), "9000".to_owned());
        form.insert("rate_limit_gets".to_owned(), "120".to_owned());
        let valid = parse_network_form(&form)?;
        let active = resolve_file(before, &Environment::Values(&empty))?;
        save_network_at(&path, &valid, &Environment::Values(&empty))?;
        let after = std::fs::read_to_string(&path)?;
        let saved = resolve_file(&after, &Environment::Values(&empty))?;
        ensure!(
            saved.rate_limit_gets == 120 && saved.port == 9000,
            "saved values must reload"
        );
        ensure!(
            active.rate_limit_gets == 60 && active.port == 8080,
            "saving must not claim immutable values changed live"
        );
        ensure!(
            after.contains("# preserved") && after.contains("[tls]\nenabled = true\nport = 8443"),
            "unrelated configuration must survive"
        );
        let overrides = BTreeMap::from([("CHAN_BIND".to_owned(), "127.0.0.1:8443".to_owned())]);
        ensure!(
            save_network_at(&path, &valid, &Environment::Values(&overrides)).is_err(),
            "effective environment conflicts must fail before saving"
        );
        ensure!(
            std::fs::read_to_string(&path)? == after,
            "override failure must preserve bytes"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            ensure!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777 == 0o600,
                "settings must remain private"
            );
        }
        Ok(())
    }

    #[test]
    fn persistence_failure_and_invalid_source_do_not_create_partial_settings() -> anyhow::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing/settings.toml");
        let updates = parse_network_form(&valid_form()?)?;
        let empty = BTreeMap::new();
        ensure!(
            save_network_at(&missing, &updates, &Environment::Values(&empty)).is_err(),
            "missing source must fail honestly"
        );
        ensure!(
            !missing.exists(),
            "failed save must not create a partial file"
        );
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "port = broken")?;
        ensure!(
            save_network_at(&path, &updates, &Environment::Values(&empty)).is_err(),
            "malformed source must fail"
        );
        ensure!(
            std::fs::read_to_string(&path)? == "port = broken",
            "malformed file must not be overwritten"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn settings_symlink_is_rejected_before_mutation() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("target.toml");
        let link = dir.path().join("settings.toml");
        std::fs::write(&target, "port = 8080\n")?;
        std::os::unix::fs::symlink(&target, &link)?;
        let updates = parse_network_form(&valid_form()?)?;
        ensure!(
            save_network_at(&link, &updates, &Environment::Values(&BTreeMap::new())).is_err(),
            "symlink settings must be rejected"
        );
        ensure!(
            std::fs::read_to_string(target)? == "port = 8080\n",
            "symlink target must remain untouched"
        );
        Ok(())
    }

    #[test]
    fn invalid_related_tor_and_redirect_state_is_rejected_without_filesystem_probes(
    ) -> anyhow::Result<()> {
        let empty = BTreeMap::new();
        for content in [
            "tor_only = true\nenable_tor_support = false\n",
            "[tls]\nenabled = true\nredirect_http = true\n",
            "tor_only = true\n[tls.acme]\nenabled = true\n",
        ] {
            let config =
                resolve_file(content, &Environment::Values(&empty)).context("resolve fixture")?;
            ensure!(
                validate_network(&config).is_err(),
                "invalid related state must fail: {content}"
            );
        }
        Ok(())
    }
    #[test]
    fn invalid_environment_fallback_and_host_override_are_reported_truthfully() -> anyhow::Result<()>
    {
        let overrides = BTreeMap::from([
            ("CHAN_RATE_GETS".to_owned(), "invalid".to_owned()),
            ("CHAN_HOST".to_owned(), "127.0.0.2".to_owned()),
        ]);
        let config = resolve_file(
            "bind_addr = '127.0.0.1:9000'\nrate_limit_gets = 120\n",
            &Environment::Values(&overrides),
        )?;
        ensure!(
            config.rate_limit_gets == 120,
            "invalid environment number must retain file value"
        );
        ensure!(
            config
                .network_sources
                .get("rate_limit_gets")
                .is_some_and(|s| s.contains("fallback used")),
            "invalid override must not be presented as authoritative"
        );
        ensure!(
            config.bind_addr == "127.0.0.2:9000",
            "host environment must override the saved interface while retaining its port"
        );
        ensure!(
            config
                .network_sources
                .get("bind_addr")
                .is_some_and(|s| s.contains("CHAN_HOST")),
            "derived host override needs provenance"
        );
        Ok(())
    }
}
