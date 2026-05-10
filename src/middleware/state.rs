// These branches are clearer in this state module than the more compact Clippy-suggested form.
#![allow(clippy::single_match_else, clippy::option_if_let_else)]

// src/middleware/state.rs

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone)]
pub struct AutoFullBackupSettingsSnapshot {
    pub interval_hours: u64,
    pub copies_to_keep: u64,
    pub include_tor_hidden_service_keys: bool,
    pub storage_mode: String,
    pub split_zip_part_size: u64,
}

#[derive(Clone)]
pub struct AutoFullBackupSettings {
    interval_hours: std::sync::Arc<AtomicU64>,
    copies_to_keep: std::sync::Arc<AtomicU64>,
    include_tor_hidden_service_keys: std::sync::Arc<AtomicBool>,
    storage_mode: std::sync::Arc<parking_lot::RwLock<String>>,
    split_zip_part_size: std::sync::Arc<AtomicU64>,
}

impl AutoFullBackupSettings {
    #[must_use]
    pub fn new(
        interval_hours: u64,
        copies_to_keep: u64,
        include_tor_hidden_service_keys: bool,
        storage_mode: impl Into<String>,
        split_zip_part_size: u64,
    ) -> Self {
        Self {
            interval_hours: std::sync::Arc::new(AtomicU64::new(interval_hours)),
            copies_to_keep: std::sync::Arc::new(AtomicU64::new(copies_to_keep.max(1))),
            include_tor_hidden_service_keys: std::sync::Arc::new(AtomicBool::new(
                include_tor_hidden_service_keys,
            )),
            storage_mode: std::sync::Arc::new(parking_lot::RwLock::new(storage_mode.into())),
            split_zip_part_size: std::sync::Arc::new(AtomicU64::new(split_zip_part_size)),
        }
    }

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

#[derive(Clone)]
pub struct MaintenanceGate {
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    active_label: std::sync::Arc<parking_lot::RwLock<Option<String>>>,
}

impl MaintenanceGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            active_label: std::sync::Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    pub fn try_begin(
        &self,
        label: &str,
    ) -> std::result::Result<MaintenanceGuard, crate::error::AppError> {
        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => {
                *self.active_label.write() = Some(label.to_string());
                Ok(MaintenanceGuard {
                    _permit: permit,
                    active_label: self.active_label.clone(),
                })
            }
            Err(_) => {
                let current = self
                    .active_label
                    .read()
                    .clone()
                    .unwrap_or_else(|| "another maintenance operation".to_string());
                Err(crate::error::AppError::Conflict(format!(
                    "{current} is already running. Try again after it finishes."
                )))
            }
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.semaphore.available_permits() == 0
    }

    #[must_use]
    pub fn active_label(&self) -> Option<String> {
        self.active_label.read().clone()
    }
}

#[derive(Clone)]
pub enum DbMaintenanceJobStatus {
    Idle,
    Running {
        job_id: u64,
        started_at: i64,
        phase: DbMaintenanceJobPhase,
    },
    Finished {
        job_id: u64,
        report: Box<crate::db::DbHealthReport>,
    },
    Failed {
        job_id: u64,
        finished_at: i64,
        message: String,
    },
}

#[derive(Clone, Copy)]
pub enum DbMaintenanceJobPhase {
    Starting,
    Backup,
    Repair,
}

#[derive(Clone)]
pub struct DbMaintenanceJobs {
    next_job_id: std::sync::Arc<AtomicU64>,
    status: std::sync::Arc<parking_lot::RwLock<DbMaintenanceJobStatus>>,
}

impl DbMaintenanceJobs {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_job_id: std::sync::Arc::new(AtomicU64::new(1)),
            status: std::sync::Arc::new(parking_lot::RwLock::new(DbMaintenanceJobStatus::Idle)),
        }
    }

    pub fn mark_running(&self) -> u64 {
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        *self.status.write() = DbMaintenanceJobStatus::Running {
            job_id,
            started_at: chrono::Utc::now().timestamp(),
            phase: DbMaintenanceJobPhase::Starting,
        };
        job_id
    }

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

    #[must_use]
    pub fn snapshot(&self) -> DbMaintenanceJobStatus {
        self.status.read().clone()
    }
}

impl DbMaintenanceJobStatus {
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

pub struct MaintenanceGuard {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active_label: std::sync::Arc<parking_lot::RwLock<Option<String>>>,
}

impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        *self.active_label.write() = None;
    }
}

#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
// These booleans are independent runtime capability toggles shared across handlers.
pub struct AppState {
    pub db: crate::db::DbPool,
    pub ffmpeg_available: bool,
    pub ffprobe_available: bool,
    pub ffmpeg_webp_available: bool,
    pub ffmpeg_vp9_available: bool,
    pub pdf_thumbnail_renderer: Option<&'static str>,
    pub job_queue: std::sync::Arc<crate::workers::JobQueue>,
    pub backup_progress: std::sync::Arc<crate::middleware::BackupProgress>,
    pub auto_full_backup_settings: AutoFullBackupSettings,
    pub maintenance_gate: MaintenanceGate,
    pub db_maintenance_jobs: DbMaintenanceJobs,
    pub chan_ledger: Option<std::sync::Arc<parking_lot::Mutex<crate::chan_net::ledger::TxLedger>>>,
    pub onion_address: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}

#[cfg(test)]
mod tests {
    use super::{
        AutoFullBackupSettings, DbMaintenanceJobPhase, DbMaintenanceJobStatus, DbMaintenanceJobs,
    };

    #[test]
    fn auto_full_backup_settings_clamps_copies_to_keep() {
        let settings = AutoFullBackupSettings::new(24, 0, false, "directory", 4);
        assert_eq!(settings.snapshot().copies_to_keep, 1);

        settings.update(12, 0, true, "split_zip", 8);
        let snapshot = settings.snapshot();
        assert_eq!(snapshot.interval_hours, 12);
        assert_eq!(snapshot.copies_to_keep, 1);
        assert!(snapshot.include_tor_hidden_service_keys);
        assert_eq!(snapshot.storage_mode, "split_zip");
        assert_eq!(snapshot.split_zip_part_size, 8);
    }

    #[test]
    fn db_maintenance_jobs_start_idle_without_job_identity() {
        let jobs = DbMaintenanceJobs::new();

        assert!(matches!(jobs.snapshot(), DbMaintenanceJobStatus::Idle));
        assert_eq!(jobs.snapshot().job_id(), None);
    }

    #[test]
    fn db_maintenance_jobs_assign_monotonic_job_ids_and_preserve_them_in_terminal_states() {
        let jobs = DbMaintenanceJobs::new();

        let first_job_id = jobs.mark_running();
        let first_status = jobs.snapshot();
        match first_status {
            DbMaintenanceJobStatus::Running { job_id, .. } => assert_eq!(job_id, first_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }

        let conn = crate::db::init_test_pool()
            .expect("test pool")
            .get()
            .expect("db connection");
        assert!(jobs.mark_finished(first_job_id, crate::db::check_db_health(&conn)));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Finished { job_id, .. } => assert_eq!(job_id, first_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected finished status"),
        }

        let second_job_id = jobs.mark_running();
        assert!(second_job_id > first_job_id);
        assert!(jobs.mark_failed(second_job_id, "simulated failure".to_string()));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Failed { job_id, .. } => assert_eq!(job_id, second_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Finished { .. } => panic!("expected failed status"),
        }
    }

    #[test]
    fn db_maintenance_jobs_phase_updates_only_matching_running_job() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(!jobs.mark_phase(first_job_id, DbMaintenanceJobPhase::Backup));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Running { job_id, phase, .. } => {
                assert_eq!(job_id, second_job_id);
                assert!(matches!(phase, DbMaintenanceJobPhase::Starting));
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }

        assert!(jobs.mark_phase(second_job_id, DbMaintenanceJobPhase::Repair));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Running { job_id, phase, .. } => {
                assert_eq!(job_id, second_job_id);
                assert!(matches!(phase, DbMaintenanceJobPhase::Repair));
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }
    }

    #[test]
    fn db_maintenance_jobs_finished_updates_only_matching_running_job() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();
        let conn = crate::db::init_test_pool()
            .expect("test pool")
            .get()
            .expect("db connection");
        let report = crate::db::check_db_health(&conn);

        assert!(!jobs.mark_finished(first_job_id, report.clone()));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Running { job_id, .. } => assert_eq!(job_id, second_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }

        assert!(jobs.mark_finished(second_job_id, report));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Finished { job_id, .. } => assert_eq!(job_id, second_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected finished status"),
        }
    }

    #[test]
    fn db_maintenance_jobs_failed_updates_only_matching_running_job() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(!jobs.mark_failed(first_job_id, "stale failure".to_string()));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Running { job_id, .. } => assert_eq!(job_id, second_job_id),
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }

        assert!(jobs.mark_failed(second_job_id, "current failure".to_string()));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Failed {
                job_id, message, ..
            } => {
                assert_eq!(job_id, second_job_id);
                assert_eq!(message, "current failure");
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Running { .. }
            | DbMaintenanceJobStatus::Finished { .. } => panic!("expected failed status"),
        }
    }

    #[test]
    fn db_maintenance_jobs_stale_failure_cannot_overwrite_newer_run() {
        let jobs = DbMaintenanceJobs::new();
        let first_job_id = jobs.mark_running();
        let second_job_id = jobs.mark_running();

        assert!(!jobs.mark_failed(first_job_id, "join error".to_string()));
        match jobs.snapshot() {
            DbMaintenanceJobStatus::Running { job_id, phase, .. } => {
                assert_eq!(job_id, second_job_id);
                assert!(matches!(phase, DbMaintenanceJobPhase::Starting));
            }
            DbMaintenanceJobStatus::Idle
            | DbMaintenanceJobStatus::Finished { .. }
            | DbMaintenanceJobStatus::Failed { .. } => panic!("expected running status"),
        }
    }
}
