//! Durable update journal.
//!
//! Once activation may have begun, recovery ALWAYS
//! stops the application and restores both the immutable database snapshot and
//! configuration before starting the old executable. No reverse migrations.
use super::release::Discovery;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Durable transaction phases; activation intent is persisted before filesystem mutation.
pub enum Phase {
    #[default]
    /// No installation is active.
    Idle,
    /// Download in progress; ordinary application writes remain permitted.
    Downloading,
    /// Package verification in progress before outage.
    Verifying,
    /// Service shutdown intent is durable.
    Stopping,
    /// Final stopped-service backup is being verified.
    BackingUp,
    /// Verified release is staged, with the old deployment still selected.
    Staged,
    /// Activation intent is durable; recovery must restore the complete old state.
    Activating,
    /// The new version may start its migrations.
    Restarting,
    /// New deployment is awaiting bounded readiness validation.
    HealthChecking,
    /// Complete known-good state is being restored while the service is stopped.
    RollingBack,
    /// Restoration completed; previous application is being checked.
    RestartingPrevious,
    /// Preparation failed before activation; previous application is restarting.
    ResumingPrevious,
    /// New version passed health/schema checks and committed durably.
    Succeeded,
    /// Previous version and persistent state restored and health verified.
    RolledBack,
    /// Update failed before activation; old deployment retained.
    Failed,
    /// Recovery failed; admission stays closed and snapshots remain retained.
    FailedManualIntervention,
}
impl Phase {
    /// Whether a durable transaction is in progress.
    pub(crate) const fn active(self) -> bool {
        matches!(
            self,
            Self::Downloading
                | Self::Verifying
                | Self::Stopping
                | Self::BackingUp
                | Self::Staged
                | Self::Activating
                | Self::Restarting
                | Self::HealthChecking
                | Self::RollingBack
                | Self::RestartingPrevious
                | Self::ResumingPrevious
        )
    }
    /// Whether accepting HTTP mutations could create a mixed update state.
    #[must_use]
    pub const fn blocks_writes(self) -> bool {
        self.active() && !matches!(self, Self::Downloading | Self::Verifying)
    }
    #[cfg(unix)]
    /// Whether activation may have changed binary or persistent state.
    pub(super) const fn needs_restore(self) -> bool {
        matches!(
            self,
            Self::Activating
                | Self::Restarting
                | Self::HealthChecking
                | Self::RollingBack
                | Self::RestartingPrevious
        )
    }
    #[cfg(unix)]
    /// Authorize startup only in a phase that has safe persistent state.
    pub(super) const fn may_start(self) -> bool {
        matches!(
            self,
            Self::Idle
                | Self::Downloading
                | Self::Verifying
                | Self::Succeeded
                | Self::RolledBack
                | Self::Failed
                | Self::Restarting
                | Self::HealthChecking
                | Self::RestartingPrevious
                | Self::ResumingPrevious
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Verified updater-owned pre-upgrade snapshot metadata exposed to administrators.
pub struct BackupInfo {
    /// Opaque snapshot or official release identifier.
    pub id: String,
    /// Snapshot publication timestamp.
    pub created_at: String,
    /// Known-good version retained for rollback.
    pub previous_version: String,
    /// Attempted signed application version.
    pub target_version: String,
    /// Recorded old release schema preserved in the snapshot.
    pub previous_schema: String,
    /// Expected migrated release schema.
    pub target_schema: String,
    /// Expected verified byte length.
    pub size: u64,
    /// Digest of the SQLite-safe snapshot.
    pub database_sha256: String,
    /// Digest of the saved configuration.
    pub configuration_sha256: String,
    /// Whether the mandatory snapshot passed verification.
    pub verified: bool,
    /// Snapshot origin, restricted to `pre_upgrade`.
    pub reason: String,
    #[serde(default)]
    /// Digest binding the stopped-service persistent-file inventory.
    /// SHA-256 binding the complete stopped-service persistent inventory.
    pub persistent_sha256: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Durable administrator-only discovery, transaction and retained snapshot state.
pub struct Status {
    /// Durable installation or recovery phase.
    pub phase: Phase,
    /// Currently selected immutable version.
    pub installed: String,
    /// Most recent stable release discovery result.
    pub discovery: Option<Discovery>,
    /// Short-lived one-use opaque authorization issued by the updater.
    pub approval: Option<String>,
    #[serde(default)]
    /// Release check and approval issuance timestamp.
    pub checked_at: Option<String>,
    /// Opaque durable transaction identifier.
    pub job: Option<String>,
    /// Authenticated full administrator initiating the update.
    pub administrator: Option<i64>,
    /// Known-good version retained for rollback.
    pub previous_version: Option<String>,
    /// Attempted signed application version.
    pub target_version: Option<String>,
    /// Sanitized current progress or final result.
    pub message: String,
    /// Snapshot protecting the current update transaction.
    pub backup: Option<BackupInfo>,
    /// Retained verified pre-upgrade snapshot history.
    pub backups: Vec<BackupInfo>,
    /// Timestamp of the last durable phase change.
    pub updated_at: String,
}

impl Status {
    /// A persisted discovery remains available only while newer than this application.
    /// Successful upgrades retain the discovery journal, so enum presence alone is insufficient.
    #[must_use]
    pub fn available_release(&self) -> Option<&super::Release> {
        let Some(Discovery::Available(candidate)) = &self.discovery else {
            return None;
        };
        let current = super::release::stable_version(super::VERSION).ok()?;
        let version = super::release::stable_version(&candidate.version).ok()?;
        (version > current).then_some(candidate.as_ref())
    }
}

#[cfg(unix)]
/// Filesystem transaction implementation for managed native deployments.
pub(super) mod native {
    use super::super::snapshot;
    use super::{BackupInfo, Discovery, Phase, Status};
    use crate::updates::release::{self, Manifest};
    use anyhow::Context as _;
    use rusqlite::{Connection, OpenFlags};
    use serde::Deserialize;
    use std::fs::{self, File};
    use std::io::{Read as _, Write as _};
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use uuid::Uuid;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    /// Root-owned fixed native deployment layout and trust configuration.
    pub(in crate::updates) struct Config {
        /// Protected immutable version root and active pointer.
        pub install_dir: PathBuf,
        /// Private updater journal, locks and rollback snapshots.
        pub state_dir: PathBuf,
        /// Fixed application data root containing chan.db, boards and runtime.
        pub data_dir: PathBuf,
        /// Must be exactly `data_dir/settings.toml`.
        pub settings_path: PathBuf,
        /// Loopback application HTTP port for bounded readiness checks.
        pub health_port: u16,
        /// Only local UID authorized to send web operations.
        pub web_uid: u32,
        /// Root-owned independently trusted `Ed25519` public key file.
        pub public_key: PathBuf,
        /// Bounded retained pre-upgrade snapshot count, at least two.
        pub retention: usize,
    }

    #[derive(Debug)]
    /// Operator-configured filesystem transaction engine.
    pub(in crate::updates) struct Engine {
        /// Validated fixed deployment configuration.
        pub config: Config,
        /// Trusted 32-byte `Ed25519` verification key.
        pub key: Vec<u8>,
    }

    /// Bounded artifact fetch seam for offline transaction fault tests.
    trait ArtifactSource {
        /// Fetch bounded artifact bytes only from allowlisted HTTPS release sources.
        fn fetch(&self, manifest: &Manifest) -> anyhow::Result<Vec<u8>>;
    }
    /// Fetch artifacts only from the official allowlisted release location.
    struct OfficialSource;
    impl ArtifactSource for OfficialSource {
        fn fetch(&self, manifest: &Manifest) -> anyhow::Result<Vec<u8>> {
            release::fetch(
                &release::asset_url(&manifest.version, &manifest.filename)?,
                manifest.size,
            )
        }
    }

    /// Fixed service lifecycle seam for transaction and recovery tests.
    pub(in crate::updates) trait Service {
        /// Validate configuration, package compatibility, free space and service authority.
        fn preflight(&self) -> anyhow::Result<()> {
            Ok(())
        }
        /// Stop the fixed application service before consistent snapshot or restore.
        fn stop(&self) -> anyhow::Result<()>;
        /// Start the selected release only after persistent state is safe.
        fn start(&self) -> anyhow::Result<()>;
        /// Bound readiness and require the expected running application version.
        fn health(&self, version: &str, schema: &str) -> anyhow::Result<()>;
    }

    /// Publish exact bytes via same-filesystem rename after file and directory fsync.
    pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        let parent = path.parent().context("missing managed parent directory")?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(bytes)?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
        sync_dir(parent)
    }
    /// Persist directory-entry changes across reboot.
    fn sync_dir(path: &Path) -> anyhow::Result<()> {
        File::open(path)?.sync_all()?;
        Ok(())
    }

    /// Reject links and special files before a managed filesystem operation.
    fn plain(path: &Path, directory: bool) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            !metadata.file_type().is_symlink()
                && if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                },
            "managed path has an unexpected type"
        );
        Ok(())
    }
    /// Reject traversal and symlink ancestors in fixed managed paths.
    fn no_symlink_ancestors(path: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "managed paths must be absolute without traversal"
        );
        for ancestor in path.ancestors() {
            plain(ancestor, ancestor != path || path.is_dir())?;
        }
        Ok(())
    }

    /// Directory ownership protects names, not merely file contents.
    ///
    /// Root-owned
    /// sticky shared temp parents are safe because another UID cannot rename
    /// their protected child; every other writable parent is refused.
    pub(in crate::updates) fn protected_ancestors(path: &Path, web_uid: u32) -> anyhow::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        no_symlink_ancestors(path)?;
        let owner = fs::symlink_metadata(path)?.uid();
        for ancestor in path.ancestors() {
            let metadata = fs::symlink_metadata(ancestor)?;
            let sticky_root =
                metadata.is_dir() && metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            anyhow::ensure!(
                metadata.uid() != web_uid
                    && (metadata.uid() == 0 || metadata.uid() == owner)
                    && (metadata.mode() & 0o022 == 0 || sticky_root),
                "protected path has an untrusted or writable ancestor"
            );
        }
        Ok(())
    }

    impl Engine {
        /// Verify managed layout, permissions and rollback retention constraints.
        pub(in crate::updates) fn validate(&self) -> anyhow::Result<()> {
            for path in [
                &self.config.install_dir,
                &self.config.state_dir,
                &self.config.data_dir,
                &self.config.settings_path,
                &self.config.public_key,
            ] {
                no_symlink_ancestors(path)?;
            }
            anyhow::ensure!(
                self.config.retention >= 2
                    && self.config.retention <= 20
                    && self.key.len() == 32
                    && self.config.health_port != 0,
                "invalid updater configuration"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                for path in [&self.config.install_dir, &self.config.state_dir] {
                    protected_ancestors(path, self.config.web_uid)?;
                    let metadata = fs::metadata(path)?;
                    anyhow::ensure!(
                        metadata.uid() != self.config.web_uid && metadata.mode() & 0o022 == 0,
                        "web account must not own or write updater installation/state"
                    );
                }
            }
            plain(&self.config.install_dir.join("versions"), true)?;
            protected_ancestors(&self.config.state_dir.join("backups"), self.config.web_uid)?;
            anyhow::ensure!(
                self.config.web_uid != 0,
                "the RustChan web account must not be root"
            );
            anyhow::ensure!(
                self.config.settings_path == self.config.data_dir.join("settings.toml"),
                "managed configuration must be data/settings.toml"
            );
            self.current_version()?;
            Ok(())
        }
        /// Validate the relative active link and immutable executable ownership.
        pub(in crate::updates) fn current_version(&self) -> anyhow::Result<String> {
            let pointer = fs::read_link(self.config.install_dir.join("current"))?;
            let version = pointer
                .strip_prefix("versions")?
                .to_str()
                .context("invalid active version")?;
            let parsed = release::stable_version(version)?;
            anyhow::ensure!(
                pointer == Path::new("versions").join(parsed.to_string()),
                "active installation pointer is outside versions"
            );
            plain(&self.config.install_dir.join(&pointer), true)?;
            plain(
                &self.config.install_dir.join(pointer).join("rustchan-cli"),
                false,
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                let owner = fs::metadata(&self.config.install_dir)?.uid();
                for path in [
                    self.config.install_dir.join("versions"),
                    self.config
                        .install_dir
                        .join("versions")
                        .join(parsed.to_string()),
                    self.config
                        .install_dir
                        .join("versions")
                        .join(parsed.to_string())
                        .join("rustchan-cli"),
                ] {
                    let metadata = fs::symlink_metadata(path)?;
                    anyhow::ensure!(
                        metadata.uid() != self.config.web_uid
                            && (metadata.uid() == 0 || metadata.uid() == owner)
                            && metadata.mode() & 0o7022 == 0
                            && metadata.mode() & 0o005 == 0o005
                            && (metadata.is_dir() || metadata.nlink() == 1),
                        "versioned program ownership or access permissions are unsafe"
                    );
                }
            }
            Ok(parsed.to_string())
        }
        /// Persisted transaction state.
        pub(in crate::updates) fn status(&self) -> anyhow::Result<Status> {
            let path = self.config.state_dir.join("status.json");
            if !path.exists() {
                return Ok(Status {
                    installed: self.current_version()?,
                    ..Status::default()
                });
            }
            plain(&path, false)?;
            let bytes = read_limited(&path, 512 * 1024)?;
            let status: Status = serde_json::from_slice(&bytes)?;
            for value in [&status.job, &status.approval].into_iter().flatten() {
                Uuid::parse_str(value)?;
            }
            for value in [&status.previous_version, &status.target_version]
                .into_iter()
                .flatten()
            {
                release::stable_version(value)?;
            }
            for backup in status.backup.iter().chain(&status.backups) {
                Uuid::parse_str(&backup.id)?;
                release::stable_version(&backup.previous_version)?;
                release::stable_version(&backup.target_version)?;
                release::stable_version(&backup.previous_schema)?;
                release::stable_version(&backup.target_schema)?;
            }
            Ok(status)
        }
        /// Serialize updater snapshot operations with an OS-owned file lock.
        fn coordination_lock(&self) -> anyhow::Result<File> {
            // Protected updater-owned lock: web maintenance is quiesced by service stop.
            let file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.config.state_dir.join("snapshot.lock"))?;
            file.try_lock().context("backup coordination failed")?;
            Ok(file)
        }
        /// Reject concurrent transactions; the OS releases the lock on process death.
        pub(in crate::updates) fn lock(&self) -> anyhow::Result<File> {
            let file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.config.state_dir.join("update.lock"))?;
            file.try_lock()
                .context("another update operation is running")?;
            Ok(file) // OS releases the lock even on crash; no stale PID guessing.
        }
        /// Fsync the durable phase and sanitized administrator-visible result.
        pub(in crate::updates) fn save(
            &self,
            status: &mut Status,
            phase: Phase,
            message: &str,
        ) -> anyhow::Result<()> {
            status.phase = phase;
            status.message = message.into();
            status.updated_at = chrono::Utc::now().to_rfc3339();
            tracing::info!(?phase, job = ?status.job, administrator = ?status.administrator, source = ?status.previous_version, target = ?status.target_version, "update transaction");
            atomic_write(
                &self.config.state_dir.join("status.json"),
                &serde_json::to_vec(status)?,
            )
        }
        /// Discover releases and issue a fresh expiring approval without service outage.
        pub(in crate::updates) fn check(&self) -> anyhow::Result<Status> {
            let _lock = self.lock()?;
            let mut status = self.status()?;
            anyhow::ensure!(
                !status.phase.active() && status.phase != Phase::FailedManualIntervention,
                "resolve the current update before checking again"
            );
            status.installed = self.current_version()?;
            status.discovery = Some(release::discover_for_schema(
                &status.installed,
                Some(&self.key),
                Some(&verify_database(&self.config.data_dir.join("chan.db"))?),
            ));
            status.checked_at = Some(chrono::Utc::now().to_rfc3339());
            status.approval = match &status.discovery {
                Some(Discovery::Available(release)) if release.compatible => {
                    Some(Uuid::new_v4().to_string())
                }
                _ => None,
            };
            if status.job.is_some() {
                atomic_write(
                    &self.config.state_dir.join("status.json"),
                    &serde_json::to_vec(&status)?,
                )?;
            } else {
                let phase = status.phase;
                self.save(&mut status, phase, "Release check completed.")?;
            }
            Ok(status)
        }
        /// Consume the matching fresh one-use approval before starting any worker.
        pub(in crate::updates) fn approve(
            &self,
            approval: &str,
            administrator: i64,
        ) -> anyhow::Result<(Status, Manifest)> {
            let mut status = self.status()?;
            anyhow::ensure!(
                !status.phase.active()
                    && status.phase != Phase::FailedManualIntervention
                    && status.approval.as_deref() == Some(approval)
                    && administrator > 0,
                "update approval is stale, consumed, or conflicting"
            );
            Uuid::parse_str(approval)?;
            let issued = chrono::DateTime::parse_from_rfc3339(
                status
                    .checked_at
                    .as_deref()
                    .context("missing approval timestamp")?,
            )?;
            let age = chrono::Utc::now().signed_duration_since(issued);
            anyhow::ensure!(
                (0..=600).contains(&age.num_seconds()),
                "update approval expired; check releases again"
            );
            let Some(Discovery::Available(release)) = &status.discovery else {
                anyhow::bail!("no approved release");
            };
            let manifest = release.manifest.clone().context("release is unverified")?;
            let current = self.current_version()?;
            anyhow::ensure!(
                release::stable_version(&manifest.version)? > release::stable_version(&current)?,
                "downgrades and reinstallation are disabled"
            );
            anyhow::ensure!(
                Some(manifest.target.as_str()) == release::platform_target()
                    && cfg!(target_os = "linux"),
                "incompatible installation target"
            );
            status.previous_version = Some(current);
            status.target_version = Some(manifest.version.clone());
            status.administrator = Some(administrator);
            status.job = Some(Uuid::new_v4().to_string());
            status.approval = None;
            status.backup = None;
            self.save(
                &mut status,
                Phase::Downloading,
                "Downloading the approved release.",
            )?;
            Ok((status, manifest))
        }
        /// Install only the approved manifest using the official artifact source.
        pub(in crate::updates) fn install(
            &self,
            status: &mut Status,
            manifest: &Manifest,
            service: &impl Service,
        ) -> anyhow::Result<()> {
            self.install_with_source(status, manifest, service, &OfficialSource)
        }
        /// Run installation and automatic complete rollback on activation failure.
        fn install_with_source(
            &self,
            status: &mut Status,
            manifest: &Manifest,
            service: &impl Service,
            source: &impl ArtifactSource,
        ) -> anyhow::Result<()> {
            let coordination = self.coordination_lock();
            let outcome = match &coordination {
                Ok(_) => self.install_inner(status, manifest, service, source),
                Err(error) => Err(anyhow::anyhow!("backup coordination failed: {error}")),
            };
            if let Err(error) = outcome {
                tracing::error!(error = %error, "update failed");
                if status.phase.needs_restore() || status.phase == Phase::Succeeded {
                    self.rollback(
                        status,
                        service,
                        &format!("{} Previous software, database, configuration and persistent files restored.", public_failure(&error)),
                    )?;
                    self.retain_terminal(status);
                    return Ok(());
                }
                if matches!(
                    status.phase,
                    Phase::Stopping | Phase::BackingUp | Phase::Staged | Phase::ResumingPrevious
                ) {
                    self.save(
                        status,
                        Phase::ResumingPrevious,
                        "Update preparation failed; restarting previous software.",
                    )?;
                    if let Err(restart_error) = service.start().and_then(|()| {
                        service.health(
                            status
                                .previous_version
                                .as_deref()
                                .context("missing previous version")?,
                            &status.backup.as_ref().map_or_else(
                                || crate::db::baseline_schema_version().to_owned(),
                                |b| b.previous_schema.clone(),
                            ),
                        )
                    }) {
                        tracing::error!(error = %restart_error, "previous version restart failed");
                        return self.save(status, Phase::FailedManualIntervention, "Preparation failed and the previous service could not restart. Operator intervention required.");
                    }
                }
                self.save(status, Phase::Failed, public_failure(&error))?;
                self.retain_terminal(status);
            }
            Ok(())
        }
        /// Stage verified bytes, snapshot stopped state, activate and health-check before commit.
        fn install_inner(
            &self,
            status: &mut Status,
            manifest: &Manifest,
            service: &impl Service,
            source: &impl ArtifactSource,
        ) -> anyhow::Result<()> {
            self.preflight(manifest)?;
            service.preflight()?;
            service.health(
                status
                    .previous_version
                    .as_deref()
                    .context("missing previous version")?,
                &verify_database(&self.config.data_dir.join("chan.db"))?,
            )?;
            let archive = source.fetch(manifest)?;
            self.save(
                status,
                Phase::Verifying,
                "Verifying release integrity and archive structure.",
            )?;
            let stage = tempfile::Builder::new()
                .prefix(".update-stage-")
                .tempdir_in(self.config.install_dir.join("versions"))?;
            stage_archive(&archive, manifest, stage.path())?;
            self.preflight(manifest)?;
            // Verify a live snapshot before any outage. Refresh after shutdown so
            // rollback includes every write acknowledged by the old application.
            self.snapshot(status, manifest, false)?;
            self.save(
                status,
                Phase::Stopping,
                "Stopping RustChan for the final consistent backup.",
            )?;
            service.stop()?;
            self.save(
                status,
                Phase::BackingUp,
                "Verifying the final database and configuration backup.",
            )?;
            self.snapshot(status, manifest, true)?;
            let version_dir = self
                .config
                .install_dir
                .join("versions")
                .join(&manifest.version);
            if version_dir.exists() {
                // A previous failed attempt may already have fully staged the
                // identical signed program. Reuse only its exact layout/hash,
                // allowing a normal administrator to retry without host access.
                verify_staged_install(&version_dir, manifest, self.config.web_uid)?;
            } else {
                let stage_path = stage.keep();
                fs::rename(&stage_path, &version_dir)?;
                sync_dir(&self.config.install_dir.join("versions"))?;
            }
            self.save(
                status,
                Phase::Staged,
                "New version staged; verified backup retained.",
            )?;
            // Journal intent BEFORE switching: a crash on either side restores
            // the old DB/config and pointer, idempotently, on daemon restart.
            self.save(status, Phase::Activating, "Activating the new release.")?;
            self.activate(&manifest.version)?;
            self.save(
                status,
                Phase::Restarting,
                "Starting the new release and its transactional migrations.",
            )?;
            service.start()?;
            self.save(
                status,
                Phase::HealthChecking,
                "Checking the new version, database schema and persistent directories.",
            )?;
            service.health(&manifest.version, &manifest.schema)?;
            anyhow::ensure!(
                verify_database(&self.config.data_dir.join("chan.db"))? == manifest.schema,
                "installed database schema mismatch"
            );
            snapshot::estimated_bytes(&self.config.data_dir)?;
            status.installed.clone_from(&manifest.version);
            self.save(
                status,
                Phase::Succeeded,
                "RustChan updated successfully after health verification.",
            )?;
            self.retain_terminal(status);
            Ok(())
        }
        /// Validate configuration, package compatibility, free space and service authority.
        fn preflight(&self, manifest: &Manifest) -> anyhow::Result<()> {
            self.validate()?;
            let existing = self
                .config
                .install_dir
                .join("versions")
                .join(&manifest.version);
            if existing.exists() {
                verify_staged_install(&existing, manifest, self.config.web_uid)?;
            }

            for path in [
                &self.config.data_dir.join("."),
                &self.config.data_dir.join("chan.db"),
            ] {
                no_symlink_ancestors(path)?;
            }
            let schema = verify_database(&self.config.data_dir.join("chan.db"))?;
            anyhow::ensure!(
                release::schema_compatible(&schema, manifest)?,
                "database schema is not compatible with this release"
            );
            let database_bytes = fs::metadata(self.config.data_dir.join("chan.db"))?.len();
            let config_bytes = fs::metadata(&self.config.settings_path)?.len();
            let wal_path = self.config.data_dir.join("chan.db-wal");
            let wal_bytes = if wal_path.exists() {
                plain(&wal_path, false)?;
                fs::metadata(&wal_path)?.len()
            } else {
                0
            };
            let required_space = manifest
                .size
                .saturating_add(manifest.executable_size)
                .saturating_mul(2)
                .saturating_add(database_bytes.saturating_add(wal_bytes).saturating_mul(6))
                .saturating_add(config_bytes.saturating_mul(4))
                .saturating_add(
                    snapshot::estimated_bytes(&self.config.data_dir)?.saturating_mul(3),
                );
            for path in [
                &self.config.install_dir,
                &self.config.state_dir,
                &self.config.data_dir,
            ] {
                let disk = rustix::fs::statvfs(path)?;
                anyhow::ensure!(
                    disk.f_bavail.saturating_mul(disk.f_frsize)
                        > required_space.saturating_add(64 * 1024 * 1024),
                    "insufficient free disk space for a safe update"
                );
                tempfile::NamedTempFile::new_in(path)?;
            }
            Ok(())
        }
        /// Authenticated snapshots of startup-mutable persistent files.
        fn snapshot(
            &self,
            status: &mut Status,
            manifest: &Manifest,
            stopped: bool,
        ) -> anyhow::Result<()> {
            let id = status.job.as_deref().context("missing update job")?;
            let backups = self.config.state_dir.join("backups");
            let stage = tempfile::Builder::new()
                .prefix(".update-backup-")
                .tempdir_in(&backups)?;
            let conn = Connection::open_with_flags(
                self.config.data_dir.join("chan.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            conn.busy_timeout(Duration::from_secs(10))?;
            let db_path = stage.path().join("database.sqlite3");
            conn.execute(
                "VACUUM INTO ?",
                [db_path.to_str().context("invalid backup path")?],
            )?;
            drop(conn);
            let schema = verify_database(&db_path)?;
            let settings = read_limited(&self.config.settings_path, 4 * 1024 * 1024)?;
            let text = std::str::from_utf8(&settings)
                .map_err(|_| anyhow::anyhow!("configuration backup is not valid UTF-8"))?;
            crate::config::validate_update_settings(text, &self.config.data_dir)?;
            atomic_write(&stage.path().join("settings.toml"), &settings)?;
            File::open(&db_path)?.sync_all()?;
            let (persistent_sha256, persistent_bytes) = if stopped {
                let (digest, size) = snapshot::create(&self.config.data_dir, stage.path())?;
                (Some(digest), size)
            } else {
                (None, 0)
            };
            let info = BackupInfo {
                id: id.into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                previous_version: status
                    .previous_version
                    .clone()
                    .context("missing previous version")?,
                target_version: manifest.version.clone(),
                previous_schema: schema,
                target_schema: manifest.schema.clone(),
                size: fs::metadata(&db_path)?.len()
                    + u64::try_from(settings.len())?
                    + persistent_bytes,
                database_sha256: hash_file(&db_path)?,
                configuration_sha256: release::digest(&settings),
                verified: true,
                reason: "pre_upgrade".into(),
                persistent_sha256,
            };
            atomic_write(
                &stage.path().join("metadata.json"),
                &serde_json::to_vec(&info)?,
            )?;
            sync_dir(stage.path())?;
            let destination = backups.join(id);
            // Only pre-activation refreshes this backup. Recovery never needs a
            // backup while in BackingUp; the old DB has not been migrated yet.
            if destination.exists() {
                plain(&destination, true)?;
                fs::remove_dir_all(&destination)?;
            }
            let stage_path = stage.keep();
            fs::rename(stage_path, destination)?;
            sync_dir(&backups)?;
            status.backup = Some(info.clone());
            status.backups.retain(|b| b.id != id);
            status.backups.insert(0, info);
            Ok(())
        }
        /// Atomically replace only the current relative link to a validated version directory.
        fn activate(&self, version: &str) -> anyhow::Result<()> {
            let parsed = release::stable_version(version)?;
            let target = Path::new("versions").join(parsed.to_string());
            plain(&self.config.install_dir.join(&target), true)?;
            plain(
                &self.config.install_dir.join(&target).join("rustchan-cli"),
                false,
            )?;
            #[cfg(unix)]
            {
                let tmp = self
                    .config
                    .install_dir
                    .join(format!(".current-{}", Uuid::new_v4()));
                std::os::unix::fs::symlink(target, &tmp)?;
                fs::rename(tmp, self.config.install_dir.join("current"))?;
                sync_dir(&self.config.install_dir)?;
                Ok(())
            }
            #[cfg(not(unix))]
            {
                anyhow::bail!("atomic activation requires Unix");
            }
        }
        /// Authenticate all snapshot components before replaying complete old state.
        fn restore(&self, status: &Status) -> anyhow::Result<String> {
            use std::os::unix::fs::MetadataExt as _;
            let backup = status
                .backup
                .as_ref()
                .context("missing verified rollback backup")?;
            Uuid::parse_str(&backup.id)?;
            let directory = self.config.state_dir.join("backups").join(&backup.id);
            no_symlink_ancestors(&directory)?;
            let database = directory.join("database.sqlite3");
            let settings = directory.join("settings.toml");
            plain(&database, false)?;
            plain(&settings, false)?;
            anyhow::ensure!(
                backup.verified
                    && hash_file(&database)? == backup.database_sha256
                    && hash_file(&settings)? == backup.configuration_sha256
                    && verify_database(&database)? == backup.previous_schema,
                "rollback backup failed verification"
            );
            snapshot::verify_saved(
                &directory,
                backup
                    .persistent_sha256
                    .as_deref()
                    .context("missing persistent rollback inventory")?,
            )?;
            no_symlink_ancestors(&self.config.data_dir)?;
            no_symlink_ancestors(&self.config.settings_path)?;
            for suffix in ["wal", "shm"] {
                let sidecar = self.config.data_dir.join(format!("chan.db-{suffix}"));
                match fs::remove_file(sidecar) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            // Stream into a same-filesystem temporary file then rename/fsync.
            let live = self.config.data_dir.join("chan.db");
            let mut stage =
                tempfile::NamedTempFile::new_in(live.parent().context("missing db parent")?)?;
            std::io::copy(&mut File::open(database)?, &mut stage)?;
            stage.as_file().sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                stage
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            let owner = fs::metadata(&self.config.data_dir)?;
            std::os::unix::fs::chown(stage.path(), Some(owner.uid()), Some(owner.gid()))?;
            stage.as_file().sync_all()?;
            stage.persist(&live).map_err(|e| e.error)?;
            sync_dir(live.parent().context("missing db parent")?)?;
            atomic_write(
                &self.config.settings_path,
                &read_limited(&settings, 4 * 1024 * 1024)?,
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(
                    &self.config.settings_path,
                    fs::Permissions::from_mode(0o600),
                )?;
                File::open(&self.config.settings_path)?.sync_all()?;
            }
            snapshot::restore(
                &directory,
                &self.config.data_dir,
                backup
                    .persistent_sha256
                    .as_deref()
                    .context("missing persistent rollback inventory")?,
            )?;
            std::os::unix::fs::chown(
                &self.config.settings_path,
                Some(owner.uid()),
                Some(owner.gid()),
            )?;
            File::open(&self.config.settings_path)?.sync_all()?;
            Ok(backup.previous_schema.clone())
        }
        /// Stop the service, restore old persistent state/version and verify health.
        fn rollback(
            &self,
            status: &mut Status,
            service: &impl Service,
            message: &str,
        ) -> anyhow::Result<()> {
            self.save(
                status,
                Phase::RollingBack,
                "Restoring the verified pre-upgrade database, configuration, and executable.",
            )?;
            let result = (|| {
                service.stop()?;
                let schema = self.restore(status)?;
                let previous = status
                    .previous_version
                    .clone()
                    .context("missing previous version")?;
                self.activate(&previous)?;
                self.save(
                    status,
                    Phase::RestartingPrevious,
                    "Health-checking the restored previous version.",
                )?;
                service.start()?;
                service.health(&previous, &schema)?;
                anyhow::ensure!(
                    verify_database(&self.config.data_dir.join("chan.db"))? == schema,
                    "restored schema mismatch"
                );
                status.installed = previous;
                Ok::<_, anyhow::Error>(())
            })();
            match result {
                Ok(()) => self.save(status, Phase::RolledBack, message),
                Err(error) => {
                    tracing::error!(error = %error, "automatic rollback failed");
                    self.save(status, Phase::FailedManualIntervention, "Upgrade and automatic rollback failed. RustChan is stopped or unhealthy; operator intervention required. The rollback backup is retained.")
                }
            }
        }
        /// Replay interrupted activation or resume previous software before opening admission.
        pub(in crate::updates) fn recover(&self, service: &impl Service) -> anyhow::Result<()> {
            let _lock = self.lock()?;
            let mut status = self.status()?;
            self.reconcile_backups(&mut status)?;
            if !status.phase.active() {
                self.retain_terminal(&mut status);
                self.clean_staging()?;
                return Ok(());
            }
            let _coordination = self.coordination_lock()?;
            if status.phase.needs_restore() {
                self.rollback(
                    &mut status,
                    service,
                    "Interrupted update recovered by restoring previous software and database.",
                )?;
                self.retain_terminal(&mut status);
                return self.clean_staging();
            }
            if matches!(
                status.phase,
                Phase::Stopping | Phase::BackingUp | Phase::Staged | Phase::ResumingPrevious
            ) {
                let previous = status
                    .previous_version
                    .clone()
                    .context("missing previous version")?;
                self.activate(&previous)?;
                self.save(
                    &mut status,
                    Phase::ResumingPrevious,
                    "Restarting previous version after interrupted preparation.",
                )?;
                if let Err(error) = service.start().and_then(|()| {
                    service.health(
                        &previous,
                        &status.backup.as_ref().map_or_else(
                            || crate::db::baseline_schema_version().to_owned(),
                            |b| b.previous_schema.clone(),
                        ),
                    )
                }) {
                    tracing::error!(error = %error, "preparation recovery failed");
                    return self.save(&mut status, Phase::FailedManualIntervention, "Interrupted preparation could not recover the previous service; operator intervention required.");
                }
            }
            self.save(
                &mut status,
                Phase::Failed,
                "Interrupted update stopped before activation; previous version retained.",
            )?;
            self.retain_terminal(&mut status);
            self.clean_staging()
        }
        /// Authenticate orphan snapshot metadata before adding it to retained history.
        fn reconcile_backups(&self, status: &mut Status) -> anyhow::Result<()> {
            let backups = self.config.state_dir.join("backups");
            for entry in fs::read_dir(&backups)?.take(1000) {
                let entry = entry?;
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if Uuid::parse_str(&id).is_err() {
                    continue;
                }
                no_symlink_ancestors(&entry.path())?;
                let info: BackupInfo = serde_json::from_slice(&read_limited(
                    &entry.path().join("metadata.json"),
                    16 * 1024,
                )?)?;
                anyhow::ensure!(
                    info.id == id && info.reason == "pre_upgrade" && info.verified,
                    "invalid managed backup metadata"
                );
                // Already journaled immutable snapshots are reauthenticated during
                // restore. Avoid hashing every retained media tree on ordinary boot.
                if status.backups.iter().any(|backup| backup == &info) {
                    continue;
                }
                release::stable_version(&info.previous_version)?;
                release::stable_version(&info.target_version)?;
                chrono::DateTime::parse_from_rfc3339(&info.created_at)?;
                anyhow::ensure!(
                    hash_file(&entry.path().join("database.sqlite3"))? == info.database_sha256
                        && hash_file(&entry.path().join("settings.toml"))?
                            == info.configuration_sha256,
                    "orphan backup failed verification"
                );
                if let Some(hash) = &info.persistent_sha256 {
                    snapshot::verify_saved(&entry.path(), hash)?;
                }
                status.backups.retain(|backup| backup.id != id);
                if status.backup.as_ref().is_some_and(|backup| backup.id == id) {
                    status.backup = Some(info.clone());
                }
                status.backups.push(info);
            }
            status
                .backups
                .sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Ok(())
        }
        /// Apply retention only after a safe terminal state; failures retain extra snapshots.
        fn retain_terminal(&self, status: &mut Status) {
            if !status.phase.active() && status.phase != Phase::FailedManualIntervention {
                if let Err(error) = self.prune(status) {
                    tracing::warn!(error = %error, "update retention failed; retained extra backups");
                }
            }
        }
        /// Remove only validated updater-generated temporary directories.
        fn clean_staging(&self) -> anyhow::Result<()> {
            for (parent, prefix) in [
                (self.config.install_dir.join("versions"), ".update-stage-"),
                (self.config.state_dir.join("backups"), ".update-backup-"),
            ] {
                for entry in fs::read_dir(&parent)?.take(1000) {
                    let entry = entry?;
                    if entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(prefix))
                    {
                        no_symlink_ancestors(&entry.path())?;
                        fs::remove_dir_all(entry.path())?;
                    }
                }
                sync_dir(&parent)?;
            }
            Ok(())
        }

        /// Retain the active rollback snapshot and immediately previous executable.
        fn prune(&self, status: &mut Status) -> anyhow::Result<()> {
            let protected = status.backup.as_ref().map(|backup| backup.id.as_str());
            let remove: Vec<_> = status
                .backups
                .iter()
                .skip(self.config.retention)
                .filter(|b| Some(b.id.as_str()) != protected)
                .map(|b| b.id.clone())
                .collect();
            for id in &remove {
                Uuid::parse_str(id)?;
                let path = self.config.state_dir.join("backups").join(id);
                no_symlink_ancestors(&path)?;
                fs::remove_dir_all(path)?;
            }
            status.backups.retain(|b| !remove.contains(&b.id));
            if status.phase == Phase::Succeeded {
                let current = self.current_version()?;
                for entry in fs::read_dir(self.config.install_dir.join("versions"))?.take(1000) {
                    let entry = entry?;
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    if release::stable_version(name).is_ok()
                        && name != current
                        && Some(name) != status.previous_version.as_deref()
                    {
                        no_symlink_ancestors(&entry.path())?;
                        fs::remove_dir_all(entry.path())?;
                    }
                }
                sync_dir(&self.config.install_dir.join("versions"))?;
            }
            let phase = status.phase;
            let message = status.message.clone();
            self.save(status, phase, &message)
        }
    }

    /// Map internal errors to bounded actionable operator messages without secrets or paths.
    fn public_failure(error: &anyhow::Error) -> &'static str {
        let detail = error.to_string();
        if detail.contains("health")
            || detail.contains("readiness")
            || detail.contains("version mismatch")
        {
            "Update failed: restarted application did not pass readiness/version validation."
        } else if detail.contains("migration")
            || detail.contains("start")
            || detail.contains("service")
        {
            "Update failed: application migration or service startup failed."
        } else if detail.contains("space") {
            "Update blocked: insufficient disk space for staging and a verified backup."
        } else if detail.contains("backup")
            || detail.contains("database")
            || detail.contains("coordination")
        {
            "Update blocked: database backup/preflight failed or another backup/restore is running."
        } else if detail.contains("checksum")
            || detail.contains("archive")
            || detail.contains("architecture")
        {
            "Update blocked: release integrity, archive layout or target verification failed."
        } else if detail.contains("configuration") {
            "Update blocked: configuration could not be safely preserved."
        } else {
            "Update stopped before activation. The previous version is retained. Check updater logs for details."
        }
    }

    /// Read regular managed files with a strict byte limit.
    fn read_limited(path: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
        plain(path, false)?;
        let mut bytes = Vec::new();
        File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= limit,
            "managed file exceeds size limit"
        );
        Ok(bytes)
    }
    /// Stream a regular file digest without buffering the entire file.
    fn hash_file(path: &Path) -> anyhow::Result<String> {
        use sha2::{Digest as _, Sha256};
        plain(path, false)?;
        let mut reader = File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(buffer.get(..count).context("invalid file read count")?);
        }
        Ok(hex::encode(hash.finalize()))
    }
    /// Read and validate the single semantic schema-version row.
    fn read_schema(conn: &Connection) -> anyhow::Result<String> {
        let schema: String = conn.query_row(
            "SELECT CAST(version AS TEXT) FROM schema_version",
            [],
            |row| row.get(0),
        )?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))?;
        anyhow::ensure!(count == 1, "invalid database schema metadata");
        release::stable_version(&schema)?;
        Ok(schema)
    }

    /// Require `SQLite` integrity, foreign keys and valid schema metadata.
    fn verify_database(path: &Path) -> anyhow::Result<String> {
        plain(path, false)?;
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        anyhow::ensure!(integrity == "ok", "database backup integrity check failed");
        let mut foreign_keys = conn.prepare("PRAGMA foreign_key_check")?;
        anyhow::ensure!(
            foreign_keys.query([])?.next()?.is_none(),
            "database backup foreign key check failed"
        );
        read_schema(&conn)
    }

    /// Reuse only an exact previously staged executable with matching hashes and ownership.
    fn verify_staged_install(
        directory: &Path,
        manifest: &Manifest,
        web_uid: u32,
    ) -> anyhow::Result<()> {
        no_symlink_ancestors(directory)?;
        let entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            entries.len() == 1
                && entries
                    .first()
                    .is_some_and(|entry| entry.file_name() == "rustchan-cli"),
            "existing staged release has unexpected layout"
        );
        let binary = directory.join("rustchan-cli");
        plain(&binary, false)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let install = directory
                .parent()
                .and_then(Path::parent)
                .context("missing staged installation root")?;
            let owner = fs::metadata(install)?.uid();
            for path in [directory, binary.as_path()] {
                let metadata = fs::metadata(path)?;
                anyhow::ensure!(
                    metadata.uid() != web_uid
                        && (metadata.uid() == 0 || metadata.uid() == owner)
                        && metadata.mode() & 0o7022 == 0
                        && metadata.mode() & 0o005 == 0o005
                        && (metadata.is_dir() || metadata.nlink() == 1),
                    "staged release permissions are unsafe"
                );
            }
        }
        anyhow::ensure!(
            fs::metadata(&binary)?.len() == manifest.executable_size
                && hash_file(&binary)? == manifest.executable_sha256,
            "existing staged release checksum mismatch"
        );
        verify_executable(&binary, &manifest.target)
    }

    /// Require a 64-bit little-endian executable ELF with the expected CPU machine.
    fn verify_executable(path: &Path, target: &str) -> anyhow::Result<()> {
        let mut header = [0u8; 64];
        File::open(path)?.read_exact(&mut header)?;
        let machine = match target {
            "x86_64-unknown-linux-gnu" => 62,
            "aarch64-unknown-linux-gnu" => 183,
            _ => anyhow::bail!("unsupported executable target"),
        };
        anyhow::ensure!(
            &header[..7] == b"\x7fELF\x02\x01\x01"
                && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
                && u16::from_le_bytes([header[18], header[19]]) == machine,
            "release executable architecture or format mismatch"
        );
        Ok(())
    }

    /// Bound decompressed tar data, including padding after its last visible entry.
    struct ExpandedReader<R> {
        /// Underlying decompressor being bounded.
        inner: R,
        /// Remaining permitted expanded bytes.
        remaining: u64,
    }
    impl<R: std::io::Read> std::io::Read for ExpandedReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if buffer.is_empty() {
                return Ok(0);
            }
            if self.remaining == 0 {
                let mut extra = [0u8; 1];
                if self.inner.read(&mut extra)? != 0 {
                    return Err(std::io::Error::other(
                        "release archive exceeds expansion limit",
                    ));
                }
                return Ok(0);
            }
            let count = usize::try_from(self.remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = self.inner.read(
                buffer
                    .get_mut(..count)
                    .ok_or_else(|| std::io::Error::other("invalid expanded read count"))?,
            )?;
            self.remaining -= u64::try_from(read).map_err(std::io::Error::other)?;
            Ok(read)
        }
    }

    /// Reject non-exact raw tar layouts before creating the isolated staged executable.
    fn stage_archive(bytes: &[u8], manifest: &Manifest, destination: &Path) -> anyhow::Result<()> {
        anyhow::ensure!(
            u64::try_from(bytes.len())? == manifest.size
                && release::digest(bytes) == manifest.sha256,
            "artifact checksum or size mismatch"
        );
        let decoder = flate2::read::MultiGzDecoder::new(bytes);
        let mut archive = tar::Archive::new(ExpandedReader {
            inner: decoder,
            remaining: manifest.executable_size.saturating_add(64 * 1024),
        });
        let mut count = 0;
        for entry in archive.entries()?.raw(true) {
            let mut entry = entry?;
            anyhow::ensure!(
                entry.header().entry_type().is_file()
                    && entry.path_bytes().as_ref() == b"rustchan-cli"
                    && count == 0
                    && entry.size() == manifest.executable_size,
                "unexpected release archive layout or traversal attempt"
            );
            let mut executable = File::options()
                .write(true)
                .create_new(true)
                .open(destination.join("rustchan-cli"))?;
            std::io::copy(&mut entry, &mut executable)?;
            executable.sync_all()?;
            count += 1;
        }
        let mut remainder = archive.into_inner();
        let mut padding = [0u8; 16 * 1024];
        loop {
            let count = remainder.read(&mut padding)?;
            if count == 0 {
                break;
            }
            anyhow::ensure!(
                padding
                    .get(..count)
                    .context("invalid archive padding length")?
                    .iter()
                    .all(|byte| *byte == 0),
                "unexpected trailing release archive contents"
            );
        }
        anyhow::ensure!(
            count == 1
                && hash_file(&destination.join("rustchan-cli"))? == manifest.executable_sha256,
            "release executable verification failed"
        );
        verify_executable(&destination.join("rustchan-cli"), &manifest.target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(
                destination.join("rustchan-cli"),
                fs::Permissions::from_mode(0o755),
            )?;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
            File::open(destination.join("rustchan-cli"))?.sync_all()?;
        }
        sync_dir(destination)
    }

    #[cfg(test)]
    mod tests {
        include!("transaction_tests.rs");
    }
}
