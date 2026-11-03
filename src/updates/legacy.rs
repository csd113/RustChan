//! Preserve admission safety for interrupted older updates without executing an updater.

use anyhow::{ensure, Context as _};
use parking_lot::Mutex;
use serde::Deserialize;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use uuid::Uuid;

/// Hold existing legacy leases until process exit; never create or rewrite old state.
static ADMISSION: LazyLock<Mutex<Option<Admission>>> = LazyLock::new(|| Mutex::new(None));

/// Existing data binding and lock handles preventing an older writer from returning.
struct Admission {
    /// Canonical data binding retained for repeated initialization.
    data: PathBuf,
    /// Shared stable leases retained until process exit.
    _locks: Vec<File>,
}

#[derive(Deserialize)]
/// Only the phase controls admission; unrelated historical fields remain untouched.
struct Journal {
    /// Persisted phase, checked against the closed historical contract.
    phase: String,
    #[serde(rename = "operation")]
    /// Old records without an operation predate settings-only restarts.
    _operation: Option<LegacyOperation>,
    #[serde(flatten)]
    /// Known display fields are ignored without executing or modifying them.
    metadata: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
/// Closed historical transaction kinds.
enum LegacyOperation {
    /// Software and persistent-state update.
    Update,
    /// Configuration-only recovery; database and media are not rolled back.
    SettingsRestart,
}

#[derive(Deserialize)]
/// Read-only older executable-retention completion marker.
struct Origin {
    /// Historical contract format, independent of package version.
    format: u32,
    /// Persisted phase, checked against the closed historical contract.
    phase: String,
    #[serde(flatten)]
    /// Known display fields are ignored without executing or modifying them.
    metadata: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Historical kernel process identity; never used for PID-only exit inference.
struct ProcessIdentity {
    /// Positive kernel process identifier.
    pid: i32,
    #[serde(rename = "start_tick")]
    /// Start identity remains type-checked without probing a potentially reused PID.
    _start_tick: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// Durable older writer-exit evidence.
struct Lifetime {
    /// Historical contract format, independent of package version.
    format: u32,
    /// Unique record identity, which must match its filename.
    id: Uuid,
    /// Kernel boot that admitted this writer.
    boot: Uuid,
    /// Fixed older writer role.
    role: String,
    /// Persisted phase, checked against the closed historical contract.
    phase: String,
    /// Valid creating process identity.
    parent: ProcessIdentity,
    /// Running records must identify their guardian.
    guardian: Option<ProcessIdentity>,
    /// Optional admitted application or media child.
    child: Option<ProcessIdentity>,
}

/// Validate a historical private path without changing ownership or permissions.
fn private_path(path: &Path, data: &Path) -> anyhow::Result<()> {
    crate::utils::fs_security::reject_symlink_components(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::metadata(data)?;
        ensure!(
            metadata.uid() == owner.uid() && metadata.mode().trailing_zeros() >= 6,
            "legacy recovery state has unsafe ownership or permissions"
        );
    }
    #[cfg(not(unix))]
    let _data = data;
    ensure!(
        metadata.is_file() || metadata.is_dir(),
        "legacy recovery state has an invalid type"
    );
    Ok(())
}

/// Open an existing stable lease, excluding active older monitors and writers.
fn lease(path: &Path, data: &Path) -> anyhow::Result<File> {
    private_path(path, data)?;
    crate::utils::fs_security::assert_regular_file_no_symlink(path)?;
    let mut options = File::options();
    let _options = options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _flags = options.custom_flags(i32::try_from(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits(),
        )?);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "legacy lease must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = file.metadata()?;
        let named = std::fs::symlink_metadata(path)?;
        ensure!(
            opened.nlink() == 1 && opened.dev() == named.dev() && opened.ino() == named.ino(),
            "legacy lease changed while opening"
        );
    }
    // Old roots require exclusive admission. One shared acquisition excludes
    // them while allowing compatible new writers; lifetime proof excludes old
    // orphan writers. Re-locking would retain the exclusive lock on Windows.
    file.try_lock_shared().context(
        "An older RustChan process still owns the data. Stop it normally before upgrading.",
    )?;
    Ok(file)
}

/// Read a small historical record with the same link and private-file checks as settings.
fn read<T: serde::de::DeserializeOwned>(path: &Path, data: &Path, limit: u64) -> anyhow::Result<T> {
    private_path(path, data)?;
    Ok(serde_json::from_slice(
        &crate::utils::fs_security::read_bounded_regular_file(path, limit)?,
    )?)
}

/// Unknown, active, and failed-recovery journals require operator recovery before writes.
fn terminal_journal(path: &Path, data: &Path) -> anyhow::Result<()> {
    if exists(path)? {
        let journal: Journal = read(path, data, 512 * 1024)?;
        ensure!(
            journal.metadata.keys().all(|key| matches!(
                key.as_str(),
                "installed"
                    | "discovery"
                    | "approval"
                    | "checked_at"
                    | "job"
                    | "administrator"
                    | "previous_version"
                    | "target_version"
                    | "message"
                    | "backup"
                    | "backups"
                    | "updated_at"
                    | "restart_instance"
            )),
            "Unknown older update journal fields; preserve recovery evidence."
        );
        ensure!(matches!(journal.phase.as_str(), "idle" | "succeeded" | "rolled_back" | "failed"),
            "An older update or settings restart is unfinished. Preserve its state and backups; use the matching older version to complete recovery before upgrading.");
    }
    Ok(())
}

/// A dangling symlink is unsafe existing state, not an absent journal.
fn exists(path: &Path) -> anyhow::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Read the actual local Linux boot identity; PID absence never proves a changed boot.
#[cfg(target_os = "linux")]
fn kernel_boot() -> anyhow::Result<Uuid> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let boot = Uuid::parse_str(boot.trim())?;
    ensure!(!boot.is_nil(), "invalid kernel boot identity");
    Ok(boot)
}

/// Inspect only old admission records, retaining locks without invoking recovery actions.
fn admit(data: &Path) -> anyhow::Result<Admission> {
    let mut locks = Vec::new();
    let legacy = data.join(".software-updates");
    if exists(&legacy)? {
        private_path(&legacy, data)?;
        locks.push(lease(&legacy.join("admission.lock"), data)?);
        locks.push(lease(&legacy.join("data-writers.lock"), data)?);
        let origin_path = legacy.join("origin.json");
        if exists(&origin_path)? {
            let origin: Origin = read(&origin_path, data, 4096)?;
            ensure!(origin.format == 1 && origin.phase == "ready"
                && origin.metadata.keys().all(|key| matches!(key.as_str(), "program" | "version" | "sha256")),
                "Older executable retention did not complete; preserve its state and recover the old installation first.");
        }
        terminal_journal(&legacy.join("status.json"), data)?;
        #[cfg(target_os = "linux")]
        let boot = Some(kernel_boot()?);
        #[cfg(not(target_os = "linux"))]
        let boot: Option<Uuid> = None;
        let writers = legacy.join("writers");
        private_path(&writers, data)?;
        for (count, item) in std::fs::read_dir(writers)?.enumerate() {
            ensure!(
                count < 16_384,
                "legacy writer inventory exceeds safety bounds"
            );
            let path = item?.path();
            let record: Lifetime = read(&path, data, 4096)?;
            ensure!(
                record.format == 1
                    && !record.id.is_nil()
                    && !record.boot.is_nil()
                    && path.file_name().is_some_and(
                        |name| name == std::ffi::OsStr::new(&format!("{}.json", record.id))
                    )
                    && matches!(
                        record.role.as_str(),
                        "controller" | "server" | "administrator" | "media"
                    )
                    && matches!(record.phase.as_str(), "launching" | "running" | "stopped")
                    && record.parent.pid > 0_i32
                    && record
                        .guardian
                        .as_ref()
                        .is_none_or(|process| process.pid > 0_i32)
                    && record
                        .child
                        .as_ref()
                        .is_none_or(|process| process.pid > 0_i32)
                    && (record.phase != "running" || record.guardian.is_some()),
                "Invalid older writer record; preserve recovery evidence."
            );
            ensure!(record.phase == "stopped" || boot.is_some_and(|boot| boot != record.boot),
                "An older writer's exit is uncertain. Keep recovery evidence and restart the host before upgrading.");
        }
    }
    terminal_journal(&data.join("runtime/settings-restart/status.json"), data)?;
    // Keep both shared leases through validation and every admitted writer's lifetime.
    Ok(Admission {
        data: data.to_owned(),
        _locks: locks,
    })
}

#[cfg(test)]
#[path = "tests/legacy_admission.rs"]
mod tests;

/// Exclude older writers and unfinished recovery before any configuration or database writes.
///
/// # Errors
/// Rejects obsolete internal invocation, unsafe recovery evidence, or active older processes.
pub fn check_legacy_state() -> anyhow::Result<()> {
    ensure!(std::env::var_os("RUSTCHAN_INTERNAL_WRITER").is_none(), "The old internal updater launcher is unsupported. Stop the old launcher and start RustChan directly.");
    ensure!(
        !std::env::var("RUSTCHAN_MANAGED").is_ok_and(|value| value == "1"),
        "Stop the old external updater and complete or recover any unfinished transaction. Remove RUSTCHAN_MANAGED from the launcher, then start RustChan directly with the same data directory."
    );
    let data = crate::config::data_dir();
    let mut admission = ADMISSION.lock();
    if let Some(existing) = admission.as_ref() {
        ensure!(
            existing.data == data,
            "legacy admission is bound to another data directory"
        );
        return Ok(());
    }
    *admission = Some(admit(&data)?);
    drop(admission);
    Ok(())
}
