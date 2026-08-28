// src/middleware/state.rs

use crate::error::AppError;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

#[derive(Clone, Debug)]
/// Point-in-time automatic full-backup configuration.
pub struct AutoFullBackupSettingsSnapshot {
    /// Hours between automatic full backups.
    pub interval_hours: u64,
    /// Number of recent automatic backups to retain.
    pub copies_to_keep: u64,
    /// Whether backups include Tor hidden-service keys.
    pub include_tor_hidden_service_keys: bool,
    /// Storage format used for automatic backups.
    pub storage_mode: String,
    /// Maximum size of each split ZIP part, in bytes.
    pub split_zip_part_size: u64,
}

#[derive(Clone, Debug)]
/// Concurrently readable automatic full-backup configuration.
pub struct AutoFullBackupSettings {
    /// Hours between automatic full backups.
    interval_hours: Arc<AtomicU64>,
    /// Number of recent automatic backups to retain.
    copies_to_keep: Arc<AtomicU64>,
    /// Whether backups include Tor hidden-service keys.
    include_tor_hidden_service_keys: Arc<AtomicBool>,
    /// Storage format used for automatic backups.
    storage_mode: Arc<parking_lot::RwLock<String>>,
    /// Maximum size of each split ZIP part, in bytes.
    split_zip_part_size: Arc<AtomicU64>,
}

impl AutoFullBackupSettings {
    /// Creates shared automatic-backup settings.
    #[must_use]
    pub fn new(
        interval_hours: u64,
        copies_to_keep: u64,
        include_tor_hidden_service_keys: bool,
        storage_mode: impl Into<String>,
        split_zip_part_size: u64,
    ) -> Self {
        Self {
            interval_hours: Arc::new(AtomicU64::new(interval_hours)),
            copies_to_keep: Arc::new(AtomicU64::new(copies_to_keep.max(1))),
            include_tor_hidden_service_keys: Arc::new(AtomicBool::new(
                include_tor_hidden_service_keys,
            )),
            storage_mode: Arc::new(parking_lot::RwLock::new(storage_mode.into())),
            split_zip_part_size: Arc::new(AtomicU64::new(split_zip_part_size)),
        }
    }

    /// Returns a consistent point-in-time copy of the current settings.
    #[must_use]
    pub fn snapshot(&self) -> AutoFullBackupSettingsSnapshot {
        AutoFullBackupSettingsSnapshot {
            interval_hours: self.interval_hours.load(Ordering::Relaxed),
            copies_to_keep: self.copies_to_keep.load(Ordering::Relaxed),
            include_tor_hidden_service_keys: self
                .include_tor_hidden_service_keys
                .load(Ordering::Relaxed),
            storage_mode: self.storage_mode.read().clone(),
            split_zip_part_size: self.split_zip_part_size.load(Ordering::Relaxed),
        }
    }

    /// Replaces the automatic-backup settings exposed to running workers.
    pub fn update(
        &self,
        interval_hours: u64,
        copies_to_keep: u64,
        include_tor_hidden_service_keys: bool,
        storage_mode: impl Into<String>,
        split_zip_part_size: u64,
    ) {
        self.interval_hours.store(interval_hours, Ordering::Relaxed);
        self.copies_to_keep
            .store(copies_to_keep.max(1), Ordering::Relaxed);
        self.include_tor_hidden_service_keys
            .store(include_tor_hidden_service_keys, Ordering::Relaxed);
        *self.storage_mode.write() = storage_mode.into();
        self.split_zip_part_size
            .store(split_zip_part_size, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug)]
/// Serializes maintenance operations that must not overlap.
pub struct MaintenanceGate {
    /// Single permit held for the duration of an active operation.
    semaphore: Arc<tokio::sync::Semaphore>,
    /// Human-readable label for the active operation.
    active_label: Arc<parking_lot::RwLock<Option<String>>>,
}

impl MaintenanceGate {
    /// Creates an idle maintenance gate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
            active_label: Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    /// Begins a labeled maintenance operation or reports the conflicting operation.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Conflict`] when another maintenance operation already
    /// owns the gate.
    pub fn try_begin(&self, label: &str) -> Result<MaintenanceGuard, AppError> {
        let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
            let current = self
                .active_label
                .read()
                .clone()
                .unwrap_or_else(|| "another maintenance operation".to_owned());
            return Err(AppError::Conflict(format!(
                "{current} is already running. Try again after it finishes."
            )));
        };

        *self.active_label.write() = Some(label.to_owned());
        Ok(MaintenanceGuard {
            _permit: permit,
            active_label: Arc::clone(&self.active_label),
        })
    }

    /// Returns whether a maintenance operation currently owns the gate.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.semaphore.available_permits() == 0
    }

    /// Returns the active operation label, if any.
    #[must_use]
    pub fn active_label(&self) -> Option<String> {
        self.active_label.read().clone()
    }
}

impl Default for MaintenanceGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
/// Limits concurrent attacker-controlled media parsing and processing.
#[expect(
    clippy::redundant_pub_crate,
    reason = "this type is intentionally re-exported from the private state module"
)]
pub(crate) struct MediaUploadGate {
    /// Single permit held while a public media upload is validated and processed.
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl MediaUploadGate {
    /// Creates a gate that permits one resource-intensive media upload at a time.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Begins media processing or returns a retryable overload response.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::DbBusy`] while another media upload owns the gate.
    pub(crate) fn try_begin(&self) -> Result<MediaUploadGuard, AppError> {
        let permit = Arc::clone(&self.semaphore)
            .try_acquire_owned()
            .map_err(|_error| AppError::DbBusy)?;
        Ok(MediaUploadGuard { _permit: permit })
    }
}

impl Default for MediaUploadGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
/// Serializes authenticated federation snapshot import and poll processing.
#[expect(
    clippy::redundant_pub_crate,
    reason = "this type is intentionally re-exported from the private state module"
)]
pub(crate) struct ChanImportGate {
    /// Single permit held while a snapshot is fetched, parsed, and committed.
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl ChanImportGate {
    /// Creates a gate that permits one snapshot import or poll operation at a time.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Begins snapshot processing or returns a retryable overload response.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::DbBusy`] while another import or poll owns the gate.
    pub(crate) fn try_begin(&self) -> Result<ChanImportGuard, AppError> {
        let permit = Arc::clone(&self.semaphore)
            .try_acquire_owned()
            .map_err(|_error| AppError::DbBusy)?;
        Ok(ChanImportGuard {
            _permit: Arc::new(permit),
        })
    }
}

impl Default for ChanImportGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
/// Permit held for the duration of authenticated snapshot processing.
///
/// Clones keep the permit alive in detached blocking work if its request future
/// is cancelled while decompression, JSON parsing, or database writes continue.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the guard crosses the private state-module boundary and blocking tasks"
)]
pub(crate) struct ChanImportGuard {
    /// Shared owned permit released after the final guard clone is dropped.
    _permit: Arc<tokio::sync::OwnedSemaphorePermit>,
}

#[derive(Debug)]
/// Permit held for the duration of one public media-processing operation.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the inferred guard type crosses the private state-module boundary"
)]
pub(crate) struct MediaUploadGuard {
    /// Owned permit released when the guard is dropped.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Clone, Debug)]
/// Observable state of the current or most recent database maintenance job.
pub enum DbMaintenanceJobStatus {
    /// No database maintenance job has started.
    Idle,
    /// A database maintenance job is currently running.
    Running {
        /// Monotonically assigned job identifier.
        job_id: u64,
        /// Unix timestamp when the job started.
        started_at: i64,
        /// Current stage of the job.
        phase: DbMaintenanceJobPhase,
    },
    /// A database maintenance job completed successfully.
    Finished {
        /// Identifier of the completed job.
        job_id: u64,
        /// Database health report produced by the job.
        report: Box<crate::db::DbHealthReport>,
    },
    /// A database maintenance job failed.
    Failed {
        /// Identifier of the failed job.
        job_id: u64,
        /// Unix timestamp when the job failed.
        finished_at: i64,
        /// Human-readable failure description.
        message: String,
    },
}

#[derive(Clone, Copy, Debug)]
/// Execution stage of a running database maintenance job.
pub enum DbMaintenanceJobPhase {
    /// The job has been accepted and is initializing.
    Starting,
    /// The pre-repair database backup is being created.
    Backup,
    /// Database checks and repairs are running.
    Repair,
}

#[derive(Clone, Debug)]
/// Concurrent state tracker for database maintenance jobs.
pub struct DbMaintenanceJobs {
    /// Identifier to assign to the next job.
    next_job_id: Arc<AtomicU64>,
    /// Current or most recent job status.
    status: Arc<parking_lot::RwLock<DbMaintenanceJobStatus>>,
}

impl DbMaintenanceJobs {
    /// Creates an idle job tracker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_job_id: Arc::new(AtomicU64::new(1)),
            status: Arc::new(parking_lot::RwLock::new(DbMaintenanceJobStatus::Idle)),
        }
    }

    /// Starts a new job and returns its identifier.
    #[must_use]
    pub fn mark_running(&self) -> u64 {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        *self.status.write() = DbMaintenanceJobStatus::Running {
            job_id,
            started_at: chrono::Utc::now().timestamp(),
            phase: DbMaintenanceJobPhase::Starting,
        };
        job_id
    }

    /// Changes the phase of the matching running job.
    #[must_use]
    pub fn mark_phase(&self, job_id: u64, phase: DbMaintenanceJobPhase) -> bool {
        let mut status = self.status.write();
        match &*status {
            DbMaintenanceJobStatus::Running {
                job_id: current_job_id,
                started_at,
                ..
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Running {
                    job_id,
                    started_at: *started_at,
                    phase,
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => false,
        }
    }

    /// Records a successful report for the matching running job.
    #[must_use]
    pub fn mark_finished(&self, job_id: u64, report: crate::db::DbHealthReport) -> bool {
        let mut status = self.status.write();
        match &*status {
            DbMaintenanceJobStatus::Running {
                job_id: current_job_id,
                ..
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Finished {
                    job_id,
                    report: Box::new(report),
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => false,
        }
    }

    /// Records a failure for the matching running job.
    #[must_use]
    pub fn mark_failed(&self, job_id: u64, message: String) -> bool {
        let mut status = self.status.write();
        match &*status {
            DbMaintenanceJobStatus::Running {
                job_id: current_job_id,
                ..
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Failed {
                    job_id,
                    finished_at: chrono::Utc::now().timestamp(),
                    message,
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => false,
        }
    }

    /// Returns a clone of the current or most recent job status.
    #[must_use]
    pub fn snapshot(&self) -> DbMaintenanceJobStatus {
        self.status.read().clone()
    }
}

impl Default for DbMaintenanceJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl DbMaintenanceJobStatus {
    /// Returns the job identifier for every non-idle state.
    #[must_use]
    pub const fn job_id(&self) -> Option<u64> {
        match self {
            Self::Idle => None,
            Self::Running { job_id, .. }
            | Self::Finished { job_id, .. }
            | Self::Failed { job_id, .. } => Some(*job_id),
        }
    }
}

/// Permit that keeps a maintenance operation active until dropped.
#[derive(Debug)]
pub struct MaintenanceGuard {
    /// Owned semaphore permit released when the guard is dropped.
    _permit: tokio::sync::OwnedSemaphorePermit,
    /// Shared operation label cleared when the guard is dropped.
    active_label: Arc<parking_lot::RwLock<Option<String>>>,
}

impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        *self.active_label.write() = None;
    }
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent runtime capability flags are shared across handlers and workers"
)]
/// Shared runtime services and capability state used by request handlers.
pub struct AppState {
    /// Database connection pool.
    pub db: crate::db::DbPool,
    /// Whether the `FFmpeg` executable is available.
    pub ffmpeg_available: bool,
    /// Whether the `FFprobe` executable is available.
    pub ffprobe_available: bool,
    /// Whether `FFmpeg` supports `WebP` output.
    pub ffmpeg_webp_available: bool,
    /// Whether `FFmpeg` supports `VP9` output.
    pub ffmpeg_vp9_available: bool,
    /// Whether a usable `VP9` encoder is available.
    pub ffmpeg_vp9_encoder_available: bool,
    /// Whether `FFmpeg` supports `Opus` output.
    pub ffmpeg_opus_available: bool,
    /// Name of the available PDF thumbnail renderer.
    pub pdf_thumbnail_renderer: Option<&'static str>,
    /// Background media-processing job queue.
    pub job_queue: Arc<crate::workers::JobQueue>,
    /// Progress counters for the active backup.
    pub backup_progress: Arc<crate::middleware::BackupProgress>,
    /// Live automatic full-backup settings.
    pub auto_full_backup_settings: AutoFullBackupSettings,
    /// Gate preventing overlapping maintenance operations.
    pub maintenance_gate: MaintenanceGate,
    /// Gate limiting concurrent public media parsing and processing.
    pub(crate) media_upload_gate: MediaUploadGate,
    /// Gate serializing authenticated federation snapshot imports and polls.
    pub(crate) chan_import_gate: ChanImportGate,
    /// State of database maintenance jobs.
    pub db_maintenance_jobs: DbMaintenanceJobs,
    /// Optional transaction ledger for federation operations.
    pub chan_ledger: Option<Arc<parking_lot::Mutex<crate::chan_net::ledger::TxLedger>>>,
    /// Current Tor onion address, when onion service is enabled.
    pub onion_address: Arc<tokio::sync::RwLock<Option<String>>>,
}

#[cfg(test)]
mod tests {
    use super::{
        AutoFullBackupSettings, ChanImportGate, DbMaintenanceJobPhase, DbMaintenanceJobStatus,
        DbMaintenanceJobs, MediaUploadGate,
    };

    #[test]
    fn media_upload_gate_rejects_overlap_and_recovers_after_drop() {
        let gate = MediaUploadGate::new();
        let first = gate.try_begin();
        assert!(first.is_ok(), "first media upload should acquire the gate");
        assert!(
            matches!(gate.try_begin(), Err(crate::error::AppError::DbBusy)),
            "overlapping media upload should receive retryable backpressure"
        );
        drop(first);
        assert!(
            gate.try_begin().is_ok(),
            "media gate should reopen after the active upload finishes"
        );
    }

    #[test]
    fn chan_import_gate_rejects_overlap_until_last_guard_clone_drops() {
        let gate = ChanImportGate::new();
        let first = gate.try_begin().ok();
        assert!(
            first.is_some(),
            "first snapshot operation should acquire the gate"
        );
        let blocking_guard = first.clone();
        drop(first);

        assert!(
            matches!(gate.try_begin(), Err(crate::error::AppError::DbBusy)),
            "overlapping snapshot processing should receive retryable backpressure"
        );
        drop(blocking_guard);
        assert!(
            gate.try_begin().is_ok(),
            "snapshot gate should reopen after the last guard clone drops"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn media_upload_gate_permit_outlives_cancelled_join_waiter() -> anyhow::Result<()> {
        let gate = MediaUploadGate::new();
        let guard = gate.try_begin().map_err(|error| anyhow::anyhow!(error))?;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let blocking_work = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let _guard = guard;
            started_tx
                .send(())
                .map_err(|()| anyhow::anyhow!("test waiter dropped before work started"))?;
            release_rx
                .blocking_recv()
                .map_err(|error| anyhow::anyhow!("test release channel closed: {error}"))?;
            Ok(())
        });
        started_rx.await?;

        blocking_work.abort();
        anyhow::ensure!(
            matches!(gate.try_begin(), Err(crate::error::AppError::DbBusy)),
            "cancelling the join waiter released a permit while blocking work remained active"
        );

        release_tx
            .send(())
            .map_err(|()| anyhow::anyhow!("blocking work ended before test release"))?;
        blocking_work.await??;
        anyhow::ensure!(
            gate.try_begin().is_ok(),
            "media gate did not reopen after blocking work completed"
        );
        Ok(())
    }

    #[test]
    fn auto_full_backup_settings_clamps_copies_to_keep() {
        let settings = AutoFullBackupSettings::new(24, 0, false, "directory", 4);
        assert_eq!(
            settings.snapshot().copies_to_keep,
            1,
            "initial retention should be clamped to one copy"
        );

        settings.update(12, 0, true, "split_zip", 8);
        let snapshot = settings.snapshot();
        assert_eq!(
            snapshot.interval_hours, 12,
            "updated interval should be visible in a snapshot"
        );
        assert_eq!(
            snapshot.copies_to_keep, 1,
            "updated retention should remain clamped to one copy"
        );
        assert!(
            snapshot.include_tor_hidden_service_keys,
            "updated Tor-key inclusion should be visible in a snapshot"
        );
        assert_eq!(
            snapshot.storage_mode, "split_zip",
            "updated storage mode should be visible in a snapshot"
        );
        assert_eq!(
            snapshot.split_zip_part_size, 8,
            "updated split size should be visible in a snapshot"
        );
    }

    #[test]
    fn db_maintenance_jobs_start_idle_without_job_identity() {
        let jobs = DbMaintenanceJobs::new();

        assert!(
            matches!(jobs.snapshot(), DbMaintenanceJobStatus::Idle),
            "a new maintenance tracker should start idle"
        );
        assert_eq!(
            jobs.snapshot().job_id(),
            None,
            "idle state should not expose a job identifier"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions provide precise regression failures after fallible test-database setup"
    )]
    fn db_maintenance_jobs_assign_monotonic_job_ids_and_preserve_them_in_terminal_states(
    ) -> anyhow::Result<()> {
        let jobs = DbMaintenanceJobs::new();

        let first_job_id = jobs.mark_running();
        let first_status = jobs.snapshot();
        assert!(
            matches!(
                first_status,
                DbMaintenanceJobStatus::Running { job_id, .. } if job_id == first_job_id
            ),
            "the first running state should retain its assigned identifier"
        );

        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        assert!(
            jobs.mark_finished(first_job_id, crate::db::check_db_health(&conn)),
            "the matching running job should accept a completion report"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Finished { job_id, .. } if job_id == first_job_id
            ),
            "the finished state should retain the first job identifier"
        );

        let second_job_id = jobs.mark_running();
        assert!(
            second_job_id > first_job_id,
            "job identifiers should increase monotonically"
        );
        assert!(
            jobs.mark_failed(second_job_id, "simulated failure".to_owned()),
            "the matching running job should accept a failure"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Failed { job_id, .. } if job_id == second_job_id
            ),
            "the failed state should retain the second job identifier"
        );

        Ok(())
    }

    #[test]
    fn db_maintenance_jobs_phase_updates_only_matching_running_job() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(
            !jobs.mark_phase(first_job_id, DbMaintenanceJobPhase::Backup),
            "a stale job identifier must not change the active phase"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Running {
                    job_id,
                    phase: DbMaintenanceJobPhase::Starting,
                    ..
                } if job_id == second_job_id
            ),
            "a stale phase update should leave the newer job at its starting phase"
        );

        assert!(
            jobs.mark_phase(second_job_id, DbMaintenanceJobPhase::Repair),
            "the active job identifier should permit a phase update"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Running {
                    job_id,
                    phase: DbMaintenanceJobPhase::Repair,
                    ..
                } if job_id == second_job_id
            ),
            "the matching phase update should preserve the job identity"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions provide precise regression failures after fallible test-database setup"
    )]
    fn db_maintenance_jobs_finished_updates_only_matching_running_job() -> anyhow::Result<()> {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let report = crate::db::check_db_health(&conn);

        assert!(
            !jobs.mark_finished(first_job_id, report.clone()),
            "a stale job identifier must not finish the active job"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Running { job_id, .. } if job_id == second_job_id
            ),
            "a stale completion should leave the newer job running"
        );

        assert!(
            jobs.mark_finished(second_job_id, report),
            "the active job identifier should accept completion"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Finished { job_id, .. } if job_id == second_job_id
            ),
            "the matching completion should retain the active job identifier"
        );

        Ok(())
    }

    #[test]
    fn db_maintenance_jobs_failed_updates_only_matching_running_job() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(
            !jobs.mark_failed(first_job_id, "stale failure".to_owned()),
            "a stale job identifier must not fail the active job"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Running { job_id, .. } if job_id == second_job_id
            ),
            "a stale failure should leave the newer job running"
        );

        assert!(
            jobs.mark_failed(second_job_id, "current failure".to_owned()),
            "the active job identifier should accept a failure"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Failed {
                    job_id,
                    message,
                    ..
                } if job_id == second_job_id && message == "current failure"
            ),
            "the matching failure should preserve its job identifier and message"
        );
    }

    #[test]
    fn db_maintenance_jobs_stale_failure_cannot_overwrite_newer_run() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(
            !jobs.mark_failed(first_job_id, "join error".to_owned()),
            "a stale asynchronous failure must not overwrite a newer run"
        );
        assert!(
            matches!(
                jobs.snapshot(),
                DbMaintenanceJobStatus::Running {
                    job_id,
                    phase: DbMaintenanceJobPhase::Starting,
                    ..
                } if job_id == second_job_id
            ),
            "the newer job should remain running after a stale failure"
        );
    }
}
