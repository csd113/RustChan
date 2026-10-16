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
                phase: _,
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Running {
                    job_id,
                    started_at: *started_at,
                    phase,
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running {
                job_id: _,
                started_at: _,
                phase: _,
            }
            | DbMaintenanceJobStatus::Finished {
                job_id: _,
                report: _,
            }
            | DbMaintenanceJobStatus::Failed {
                job_id: _,
                finished_at: _,
                message: _,
            } => false,
        }
    }

    /// Records a successful report for the matching running job.
    #[must_use]
    pub fn mark_finished(&self, job_id: u64, report: crate::db::DbHealthReport) -> bool {
        let mut status = self.status.write();
        match &*status {
            DbMaintenanceJobStatus::Running {
                job_id: current_job_id,
                started_at: _,
                phase: _,
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Finished {
                    job_id,
                    report: Box::new(report),
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running {
                job_id: _,
                started_at: _,
                phase: _,
            }
            | DbMaintenanceJobStatus::Finished {
                job_id: _,
                report: _,
            }
            | DbMaintenanceJobStatus::Failed {
                job_id: _,
                finished_at: _,
                message: _,
            } => false,
        }
    }

    /// Records a failure for the matching running job.
    #[must_use]
    pub fn mark_failed(&self, job_id: u64, message: String) -> bool {
        let mut status = self.status.write();
        match &*status {
            DbMaintenanceJobStatus::Running {
                job_id: current_job_id,
                started_at: _,
                phase: _,
            } if *current_job_id == job_id => {
                *status = DbMaintenanceJobStatus::Failed {
                    job_id,
                    finished_at: chrono::Utc::now().timestamp(),
                    message,
                };
                true
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running {
                job_id: _,
                started_at: _,
                phase: _,
            }
            | DbMaintenanceJobStatus::Finished {
                job_id: _,
                report: _,
            }
            | DbMaintenanceJobStatus::Failed {
                job_id: _,
                finished_at: _,
                message: _,
            } => false,
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
            Self::Running {
                job_id,
                started_at: _,
                phase: _,
            }
            | Self::Finished { job_id, report: _ }
            | Self::Failed {
                job_id,
                finished_at: _,
                message: _,
            } => Some(*job_id),
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
#[expect(
    clippy::partial_pub_fields,
    reason = "the public runtime services remain available to embedders; the internal upload gate is restricted to trusted request handling"
)]
/// Shared runtime services and capability state used by request handlers.
pub struct AppState {
    /// Admit normal HTTP work only after initialization and supervisor observation.
    pub runtime_ready: Arc<AtomicBool>,
    /// Database connection pool.
    pub db: crate::db::DbPool,
    /// Whether the `FFmpeg` executable is available.
    pub ffmpeg_available: bool,
    /// Independent AV1 decoder and encoder capabilities.
    pub ffmpeg_av1: crate::media::ffmpeg::Av1Capabilities,
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
    /// Bound admitted HTTP requests and response streams without queuing.
    pub(crate) request_work_gate: super::WorkGate,
    /// Bound memory-hard authentication independently of media and search.
    pub(crate) password_work_gate: super::WorkGate,
    /// Bound anonymous CAPTCHA generation without blocking async workers.
    pub(crate) captcha_work_gate: super::WorkGate,
    /// Bound concurrent FTS counting and sorting without a waiter queue.
    pub(crate) search_work_gate: super::WorkGate,
    /// State of database maintenance jobs.
    pub db_maintenance_jobs: DbMaintenanceJobs,
    /// Current Tor onion address, when onion service is enabled.
    pub onion_address: Arc<tokio::sync::RwLock<Option<String>>>,
}
