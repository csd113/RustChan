//! Dedicated storage visibility and secret rotation; never returns secret values.

use super::{BTreeMap, Environment};
use anyhow::{ensure, Context as _};

/// Authenticated operator state with no existing secret or secret-derived hash.
#[derive(Debug)]
pub struct ManagementState {
    /// Effective data root selected by the launcher.
    pub data_dir: String,
    /// Immutable startup source for the service data root.
    pub data_source: String,
    /// Immutable startup source for the database path.
    pub database_source: String,
    /// Immutable startup source for the media root.
    pub uploads_source: String,
    /// Effective running SQLite file.
    pub database: String,
    /// File-selected SQLite path for next startup without overrides.
    pub saved_database: String,
    /// Environment-effective next-start SQLite path.
    pub next_database: String,
    /// Effective running board-media root.
    pub uploads: String,
    /// File-selected uploads root without overrides.
    pub saved_uploads: String,
    /// Environment-effective next-start uploads root.
    pub next_uploads: String,
    /// Whether an externally managed cookie-secret override is present.
    pub secret_override: bool,
    /// Whether the saved secret differs from the active secret.
    pub secret_pending: bool,
    /// Whether the file contains configured secret material (no value is returned).
    pub secret_saved: bool,
}

/// Read path state after administrator authorization without exposing secrets.
///
/// # Errors
/// Returns unreadable or malformed settings errors.
pub fn snapshot() -> anyhow::Result<ManagementState> {
    let content = std::fs::read_to_string(super::super::settings_file_path())?;
    let raw: toml::Value = toml::from_str(&content)?;
    let file = super::resolve_file(&content, &Environment::Values(&BTreeMap::new()))?;
    let next = super::resolve_file(&content, &Environment::Process)?;
    Ok(ManagementState {
        data_dir: super::super::data_dir().display().to_string(),
        data_source: if super::super::DATA_DIR_OVERRIDE.get().is_some() {
            "CLI --data-dir; external service launcher"
        } else {
            "Default binary-relative data root; external service launcher"
        }
        .into(),
        database_source: super::CONFIG
            .network_sources
            .get("database_path")
            .cloned()
            .unwrap_or_else(|| "Startup path/default".into()),
        uploads_source: super::CONFIG
            .network_sources
            .get("upload_dir")
            .cloned()
            .unwrap_or_else(|| "Startup path/default".into()),
        database: super::CONFIG.database_path.clone(),
        saved_database: file.database_path,
        next_database: next.database_path,
        uploads: super::CONFIG.upload_dir.clone(),
        saved_uploads: file.upload_dir,
        next_uploads: next.upload_dir,
        secret_override: std::env::var_os("CHAN_COOKIE_SECRET").is_some(),
        secret_saved: raw
            .get("cookie_secret")
            .and_then(toml::Value::as_str)
            .is_some_and(|v| !v.is_empty()),
        secret_pending: raw
            .get("cookie_secret")
            .and_then(toml::Value::as_str)
            .is_some_and(|value| value != super::CONFIG.cookie_secret),
    })
}

/// Stage fresh OS-random secret material; the running secret remains immutable.
///
/// # Errors
/// Fails before mutation for environment overrides, invalid files or randomness/write errors.
pub fn rotate_secret() -> anyhow::Result<()> {
    use rand_core::TryRng as _;
    ensure!(std::env::var_os("CHAN_COOKIE_SECRET").is_none(), "CHAN_COOKIE_SECRET is externally managed; rotate it in the service environment and restart");
    let mut bytes = [0u8; 32];
    getrandom::SysRng
        .try_fill_bytes(&mut bytes)
        .context("OS randomness unavailable; no changes saved")?;
    let updates = BTreeMap::from([(
        "cookie_secret".to_owned(),
        Some(toml::Value::String(hex::encode(bytes))),
    )]);
    let _guard = super::SETTINGS_WRITE_LOCK.lock();
    super::save_root_at(
        &super::super::settings_file_path(),
        &updates,
        &Environment::Process,
        |_| Ok(()),
    )
}

/// Capture path provenance before file edits, using the same Unicode environment fallback.
pub(in crate::config) fn startup_sources(
    settings: &super::super::SettingsFile,
    environment: &Environment<'_>,
) -> BTreeMap<&'static str, String> {
    [
        ("database_path", "CHAN_DB", settings.database_path.is_some()),
        ("upload_dir", "CHAN_UPLOADS", settings.upload_dir.is_some()),
    ]
    .into_iter()
    .map(|(key, name, saved)| {
        let source = if environment.var(name).is_ok() {
            format!("Environment: {name} overrides settings.toml/default")
        } else if environment.var_os(name).is_some() {
            format!("Environment: {name} present but non-Unicode; startup file/default fallback")
        } else if saved {
            "settings.toml at startup".to_owned()
        } else {
            "Default under active data directory at startup".to_owned()
        };
        (key, source)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_provenance_uses_actual_environment_and_file_precedence() -> anyhow::Result<()> {
        let settings = super::super::super::parse_settings_file_str(
            "database_path = '/file/chan.db'\nupload_dir = '/file/boards'\n",
        )?;
        let values = BTreeMap::from([("CHAN_DB".to_owned(), "/service/chan.db".to_owned())]);
        let sources = startup_sources(&settings, &Environment::Values(&values));
        anyhow::ensure!(sources
            .get("database_path")
            .is_some_and(|source| source.contains("CHAN_DB")));
        anyhow::ensure!(sources
            .get("upload_dir")
            .is_some_and(|source| source == "settings.toml at startup"));
        Ok(())
    }
}
