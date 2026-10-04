//! Fixed settings restart coordination. The web process never launches a replacement.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::{LazyLock, OnceLock},
};
use uuid::Uuid;

/// Per-process identity, independent of configuration secrets.
pub static INSTANCE: LazyLock<Uuid> = LazyLock::new(Uuid::new_v4);
/// Exact bytes captured before immutable configuration and logging initialization.
static STARTUP_SETTINGS: OnceLock<Vec<u8>> = OnceLock::new();
/// Deadline for completing initialization, within a supervisor restart attempt.
pub const STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Maximum HTTP request drain before forced connection closure.
pub const HTTP_DRAIN: std::time::Duration = std::time::Duration::from_secs(10);
/// Total application shutdown budget, leaving supervisor time for final process cleanup.
pub const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// File bounds for settings and private recovery payloads.
const MAX_SETTINGS: u64 = 4 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Private binding between process identity and exact loaded configuration.
pub(crate) struct Running {
    /// Identity observed only after all listeners and database initialization succeed.
    pub instance: Uuid,
    /// Internal configuration binding; never exposed in public readiness or admin status.
    pub digest: String,
}

#[derive(Debug)]
/// Fixed filesystem configuration payloads used by the deployment's supervisor.
pub(crate) struct Store {
    /// Trusted supervisor-owned payload root, or the container's private runtime root.
    pub directory: PathBuf,
    /// Fixed application settings path; never supplied by HTTP.
    pub settings: PathBuf,
}
impl Store {
    /// Validate the root before publishing private recovery files.
    pub(crate) fn prepare(&self) -> anyhow::Result<()> {
        let mut existing = self.directory.as_path();
        while !existing.try_exists()? {
            existing = existing
                .parent()
                .ok_or_else(|| anyhow::anyhow!("restart store has no existing parent"))?;
        }
        crate::utils::fs_security::reject_symlink_components(existing)?;
        crate::config::ensure_private_dir(&self.directory)?;
        crate::utils::fs_security::reject_symlink_components(&self.directory)
    }
    /// Read a bounded regular file without following links or accepting hardlinks.
    pub(crate) fn read(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
        crate::utils::fs_security::reject_symlink_components(path)?;
        crate::utils::fs_security::assert_regular_file_no_symlink(path)?;
        let mut options = fs::File::options();
        let _read_options = options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let _platform_options = options.custom_flags(i32::try_from(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits(),
            )?);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(metadata.is_file(), "restart payload must be a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            anyhow::ensure!(
                metadata.nlink() == 1,
                "restart payload must not be hardlinked"
            );
        }
        let mut bytes = Vec::new();
        file.take(
            limit
                .checked_add(1)
                .context("restart byte limit overflow")?,
        )
        .read_to_end(&mut bytes)
        .map(|_bytes_read| ())?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= limit,
            "restart payload exceeds limit"
        );
        Ok(bytes)
    }
    /// Read and type-check the complete configuration before restart or recovery.
    pub(crate) fn candidate(&self) -> anyhow::Result<Vec<u8>> {
        let bytes = Self::read(&self.settings, MAX_SETTINGS)?;
        crate::config::admin::validate_restart_candidate(
            std::str::from_utf8(&bytes)?,
            &crate::config::Environment::Process,
        )?;
        Ok(bytes)
    }
    /// Persist private fsynced bytes by atomic replacement.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.prepare()?;
        let path = self.directory.join(name);
        if fs::symlink_metadata(&path).is_ok() {
            crate::utils::fs_security::assert_regular_file_no_symlink(&path)?;
        }
        crate::config::admin::atomic_replace(&path, std::str::from_utf8(bytes)?)
    }
    /// Read the last fully initialized application observation.
    pub(crate) fn running(&self) -> anyhow::Result<Running> {
        Ok(serde_json::from_slice(&Self::read(
            &self.directory.join("running.json"),
            4096,
        )?)?)
    }
    /// Record readiness without overwriting rollback bytes during an active trial.
    pub(crate) fn observe(&self, instance: Uuid, digest: &str, commit: bool) -> anyhow::Result<()> {
        let bytes = self.candidate()?;
        anyhow::ensure!(
            configuration_digest(&bytes) == digest,
            "settings changed since process initialization"
        );
        if commit {
            self.write("known-good.toml", &bytes)?;
        }
        self.write(
            "running.json",
            &serde_json::to_vec(&Running {
                instance,
                digest: digest.to_owned(),
            })?,
        )
    }
    /// Restore only verified configuration bytes; database/media are never reverted.
    pub(crate) fn restore(&self) -> anyhow::Result<()> {
        let bytes = Self::read(&self.directory.join("known-good.toml"), MAX_SETTINGS)?;
        crate::config::admin::validate_restart_candidate(
            std::str::from_utf8(&bytes)?,
            &crate::config::Environment::Process,
        )?;
        crate::config::admin::atomic_replace(&self.settings, std::str::from_utf8(&bytes)?)?;
        Ok(())
    }
}

/// Bind private startup/recovery records to exact configuration bytes.
pub(crate) fn configuration_digest(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Only the shipped container deployment explicitly opts into exit-based restarts.
/// Merely detecting a container is insufficient to assume an automatic restart policy.
#[must_use]
pub fn container_restart_enabled() -> bool {
    crate::updates::container_managed()
        && std::env::var("RUSTCHAN_RESTART_ON_EXIT").is_ok_and(|value| value == "1")
}

/// Fixed private container payload store.
fn container_store() -> Store {
    Store {
        directory: crate::config::runtime_dir().join("settings-restart"),
        settings: crate::config::data_dir().join("settings.toml"),
    }
}

/// Read container state; absence means no restart has been requested.
fn container_status_at(store: &Store) -> anyhow::Result<crate::updates::Status> {
    let path = store.directory.join("status.json");
    if !path.try_exists()? {
        return Ok(crate::updates::Status::default());
    }
    Ok(serde_json::from_slice(&Store::read(&path, 512 * 1024)?)?)
}

/// Persist container transaction phases using the same status contract as native control.
fn save_container(
    store: &Store,
    status: &mut crate::updates::Status,
    phase: crate::updates::Phase,
    message: &str,
) -> anyhow::Result<()> {
    status.phase = phase;
    message.clone_into(&mut status.message);
    status.updated_at = chrono::Utc::now().to_rfc3339();
    store.write("status.json", &serde_json::to_vec(status)?)
}

/// Recover an unverified container trial before CONFIG or logging reads the file.
///
/// # Errors
/// Rejects corrupt state, unsafe recovery paths, and failed configuration restoration.
pub fn prepare_startup() -> anyhow::Result<()> {
    let settings = crate::config::data_dir().join("settings.toml");
    if container_restart_enabled() {
        let store = container_store();
        if store.directory.try_exists()? {
            let _lease = crate::config::admin::settings_lease(&settings)?;
            let mut status = container_status_at(&store)?;
            prepare_container_trial(&store, &mut status)?;
        }
    }
    if settings.try_exists()? {
        let bytes = Store::read(&settings, MAX_SETTINGS)?;
        STARTUP_SETTINGS.set(bytes).map_err(|rejected_settings| {
            drop(rejected_settings); // Never expose settings secrets in diagnostics.
            anyhow::anyhow!("startup settings already captured")
        })?;
    }
    Ok(())
}

/// Select a single trial or restore the known-good file after interrupted/failed startup.
fn prepare_container_trial(
    store: &Store,
    status: &mut crate::updates::Status,
) -> anyhow::Result<()> {
    use crate::updates::Phase;
    match status.phase {
        Phase::Stopping => save_container(
            store,
            status,
            Phase::Restarting,
            "Starting RustChan with saved settings.",
        ),
        Phase::Restarting | Phase::HealthChecking => {
            save_container(
                store,
                status,
                Phase::RollingBack,
                "Startup did not verify; restoring the last healthy configuration.",
            )?;
            store.restore()?;
            save_container(
                store,
                status,
                Phase::RestartingPrevious,
                "Starting RustChan with the previous configuration.",
            )
        }
        Phase::RollingBack => {
            store.restore()?;
            save_container(
                store,
                status,
                Phase::RestartingPrevious,
                "Recovering the previous configuration.",
            )
        }
        Phase::RestartingPrevious | Phase::FailedManualIntervention => {
            save_container(
                store,
                status,
                Phase::FailedManualIntervention,
                "Previous configuration did not become healthy. Operator recovery is required.",
            )?;
            anyhow::bail!("configuration recovery requires operator intervention")
        }
        Phase::Idle
        | Phase::Downloading
        | Phase::Verifying
        | Phase::BackingUp
        | Phase::Activating
        | Phase::Staged
        | Phase::ResumingPrevious
        | Phase::Succeeded
        | Phase::RolledBack
        | Phase::Failed => Ok(()),
    }
}

/// Capture the configuration binding when library callers start the server directly.
///
/// # Errors
/// Rejects unreadable, oversized or unsafe settings files.
pub fn startup_digest() -> anyhow::Result<String> {
    if let Some(bytes) = STARTUP_SETTINGS.get() {
        return Ok(configuration_digest(bytes));
    }
    let bytes = Store::read(
        &crate::config::data_dir().join("settings.toml"),
        MAX_SETTINGS,
    )?;
    Ok(configuration_digest(&bytes))
}

/// Notify the controller after all listeners bind and the database reaches readiness.
///
/// # Errors
/// Rejects unavailable control, failed health probes and configuration/generation mismatches.
pub async fn started(digest: String) -> anyhow::Result<()> {
    crate::updates::writer_initialized().await?;
    if crate::updates::managed() {
        // Establish a web-owned lock inode before the privileged controller uses it.
        if !crate::config::data_dir()
            .join(".settings.lock")
            .try_exists()?
        {
            drop(crate::config::admin::settings_lease(
                &crate::config::data_dir().join("settings.toml"),
            )?);
        }
        let reply = crate::updates::request(&crate::updates::Request::Started {
            instance: *INSTANCE,
            configuration: digest,
        })
        .await?;
        anyhow::ensure!(
            reply.error.is_none(),
            "supervisor rejected configuration readiness"
        );
    } else if container_restart_enabled() {
        verify_container_health().await?;
        tokio::task::spawn_blocking(move || {
            let store = container_store();
            let _lease = crate::config::admin::settings_lease(&store.settings)?;
            let mut status = container_status_at(&store)?;
            if status.phase.active() {
                anyhow::ensure!(status.restart_instance != Some(*INSTANCE), "old process cannot verify a restart");
                let expected_path = if status.phase == crate::updates::Phase::RestartingPrevious { "known-good.toml" } else { "candidate.sha256" };
                let expected = Store::read(&store.directory.join(expected_path), MAX_SETTINGS)?;
                let expected_digest = if expected_path == "known-good.toml" { configuration_digest(&expected) } else { String::from_utf8(expected)? };
                anyhow::ensure!(digest == expected_digest, "replacement loaded an unrelated configuration");
            }
            store.observe(*INSTANCE, &digest, false)?;
            if status.phase.active() {
                let recovered = status.phase == crate::updates::Phase::RestartingPrevious;
                save_container(&store, &mut status, if recovered { crate::updates::Phase::RolledBack } else { crate::updates::Phase::Succeeded }, if recovered { "Settings startup failed. Previous configuration restored and readiness verified." } else { "RustChan restarted; saved settings passed readiness verification." })?;
            }
            store.observe(*INSTANCE, &digest, true)?;
            Ok::<_, anyhow::Error>(())
        }).await??;
    }
    Ok(())
}

/// Use the container health-check's fixed loopback port, never a browser-supplied URL.
async fn verify_container_health() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(2))
        .build()?;
    let port = crate::config::CONFIG.port;
    let deadline = tokio::time::Instant::now()
        .checked_add(STARTUP_TIMEOUT)
        .context("startup deadline overflow")?;
    while tokio::time::Instant::now() < deadline {
        let checked = async {
            let mut response = client
                .get(format!("http://127.0.0.1:{port}/readyz"))
                .send()
                .await?
                .error_for_status()?;
            anyhow::ensure!(
                response
                    .content_length()
                    .is_none_or(|length| length <= 4096),
                "health response exceeds limit"
            );
            let observed_instance = response
                .headers()
                .get("x-rustchan-instance")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| Uuid::parse_str(value).ok());
            let mut bytes = Vec::new();
            loop {
                let next = response.chunk().await?;
                let Some(chunk) = next else {
                    break;
                };
                anyhow::ensure!(
                    bytes.len().saturating_add(chunk.len()) <= 4096,
                    "health response exceeds limit"
                );
                bytes.extend_from_slice(&chunk);
            }
            let health: serde_json::Value = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                health.get("status").and_then(serde_json::Value::as_str) == Some("ready")
                    && observed_instance == Some(*INSTANCE)
                    && health.get("version").and_then(serde_json::Value::as_str)
                        == Some(crate::updates::VERSION),
                "container replacement not ready"
            );
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if checked.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    anyhow::bail!("container failed bounded readiness verification")
}

/// Read durable container restart progress after administrator authentication.
pub(crate) fn container_status() -> anyhow::Result<crate::updates::Status> {
    container_status_at(&container_store())
}

/// Durably accept exactly one container restart for this healthy process instance.
///
/// # Errors
/// Rejects duplicate operations, invalid configuration and missing verified startup state.
pub fn request_container(administrator: i64) -> anyhow::Result<()> {
    let store = container_store();
    let _lease = crate::config::admin::settings_lease(&store.settings)?;
    let mut status = container_status_at(&store)?;
    anyhow::ensure!(!status.phase.active(), "restart already in progress");
    anyhow::ensure!(
        crate::config::admin::restart_pending()?,
        "no saved settings require a restart"
    );
    let running = store.running()?;
    anyhow::ensure!(
        running.instance == *INSTANCE,
        "process has not reached verified readiness"
    );
    let candidate = store.candidate()?;
    store.write(
        "candidate.sha256",
        configuration_digest(&candidate).as_bytes(),
    )?;
    Store::read(&store.directory.join("known-good.toml"), MAX_SETTINGS)
        .map(|_operation_summary| ())?;
    status.operation = crate::updates::Operation::SettingsRestart;
    status.job = Some(Uuid::new_v4().to_string());
    status.administrator = Some(administrator);
    status.restart_instance = Some(*INSTANCE);
    save_container(
        &store,
        &mut status,
        crate::updates::Phase::Stopping,
        "Restart accepted. RustChan is draining requests.",
    )
}

/// Secret-free administrator restart state. The server derives every value.
#[derive(Debug, Serialize)]
pub struct View {
    /// Saved effective startup settings differ, or a trial still awaits verification.
    pub pending: bool,
    /// Deployment explicitly supports the fixed restart operation.
    pub supported: bool,
    /// Restart/update owns the service transaction.
    pub in_progress: bool,
    /// Sanitized server/controller progress or deployment guidance.
    pub message: String,
    /// Current process identity used by reconnect polling, never accepted from HTTP.
    pub instance: Uuid,
}

#[cfg(test)]
/// Container startup/recovery tests against fixed disposable paths, never real restarts.
mod tests {
    use super::*;

    /// Reject impossible over-read limits without panicking on addition.
    #[test]
    fn bounded_restart_read_rejects_limit_overflow() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let payload = temporary.path().canonicalize()?.join("payload");
        fs::write(&payload, b"bounded content")?;
        let overflow_error = Store::read(&payload, u64::MAX)
            .err()
            .context("overflowed read limit was accepted")?;
        anyhow::ensure!(
            overflow_error
                .to_string()
                .contains("restart byte limit overflow"),
            "read limit must be rejected by the checked byte bound: {overflow_error:#}"
        );
        anyhow::ensure!(
            Store::read(&payload, 15)? == b"bounded content",
            "valid bounded reads must preserve bytes"
        );
        Ok(())
    }

    /// Build private payloads for one disposable configuration restart.
    fn fixture() -> anyhow::Result<(tempfile::TempDir, Store, crate::updates::Status)> {
        let temp = tempfile::tempdir()?;
        let store = Store {
            directory: temp.path().canonicalize()?.join("controller"),
            settings: temp.path().canonicalize()?.join("settings.toml"),
        };
        fs::write(
            &store.settings,
            "enable_tor_support = false\nrate_limit_gets = 60\n",
        )?;
        let good = store.candidate()?;
        store.observe(Uuid::new_v4(), &configuration_digest(&good), true)?;
        fs::write(
            &store.settings,
            "enable_tor_support = false\nrate_limit_gets = 90\n",
        )?;
        let status = crate::updates::Status {
            operation: crate::updates::Operation::SettingsRestart,
            phase: crate::updates::Phase::Stopping,
            restart_instance: Some(Uuid::new_v4()),
            ..crate::updates::Status::default()
        };
        Ok((temp, store, status))
    }

    /// Trial must retain the old configuration until the new instance verifies readiness.
    #[test]
    fn container_trial_preserves_known_good_and_recovers_failed_startup() -> anyhow::Result<()> {
        let (_temp, store, mut status) = fixture()?;
        prepare_container_trial(&store, &mut status)?;
        anyhow::ensure!(
            status.phase == crate::updates::Phase::Restarting,
            "first replacement must get one trial"
        );
        let candidate = store.candidate()?;
        store.observe(Uuid::new_v4(), &configuration_digest(&candidate), false)?;
        let good = Store::read(&store.directory.join("known-good.toml"), MAX_SETTINGS)?;
        anyhow::ensure!(
            good != candidate,
            "unverified startup must not overwrite known-good config"
        );
        prepare_container_trial(&store, &mut status)?;
        anyhow::ensure!(
            status.phase == crate::updates::Phase::RestartingPrevious
                && fs::read(&store.settings)? == good,
            "next container boot must restore healthy configuration"
        );
        Ok(())
    }

    /// Corrupt payloads and symlinks must fail before changing the saved configuration.
    #[test]
    fn corrupt_recovery_never_publishes_invalid_settings() -> anyhow::Result<()> {
        let (_temp, store, _status) = fixture()?;
        let before = fs::read(&store.settings)?;
        store.write("known-good.toml", b"port = 0")?;
        anyhow::ensure!(
            store.restore().is_err(),
            "invalid rollback payload must be rejected"
        );
        anyhow::ensure!(
            fs::read(&store.settings)? == before,
            "failed recovery cannot partially replace config"
        );
        Ok(())
    }
    #[cfg(unix)]
    /// A runtime symlink cannot redirect privileged rollback or snapshot writes.
    #[test]
    fn restart_payload_rejects_symlink_and_hardlink() -> anyhow::Result<()> {
        let (temp, store, _status) = fixture()?;
        let link = temp.path().canonicalize()?.join("link");
        std::os::unix::fs::symlink(&store.settings, &link)?;
        anyhow::ensure!(
            Store::read(&link, MAX_SETTINGS).is_err(),
            "symlink payload must reject"
        );
        fs::remove_file(&link)?;
        fs::hard_link(&store.settings, &link)?;
        anyhow::ensure!(
            Store::read(&link, MAX_SETTINGS).is_err(),
            "hardlink payload must reject"
        );
        Ok(())
    }
}
