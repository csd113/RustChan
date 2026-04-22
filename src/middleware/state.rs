// These branches are clearer in this state module than the more compact Clippy-suggested form.
#![allow(clippy::single_match_else, clippy::option_if_let_else)]

// src/middleware/state.rs

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub struct AutoFullBackupSettingsSnapshot {
    pub interval_hours: u64,
    pub copies_to_keep: u64,
}

#[derive(Clone)]
pub struct AutoFullBackupSettings {
    interval_hours: std::sync::Arc<AtomicU64>,
    copies_to_keep: std::sync::Arc<AtomicU64>,
}

impl AutoFullBackupSettings {
    #[must_use]
    pub fn new(interval_hours: u64, copies_to_keep: u64) -> Self {
        Self {
            interval_hours: std::sync::Arc::new(AtomicU64::new(interval_hours)),
            copies_to_keep: std::sync::Arc::new(AtomicU64::new(copies_to_keep.max(1))),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> AutoFullBackupSettingsSnapshot {
        AutoFullBackupSettingsSnapshot {
            interval_hours: self.interval_hours.load(Ordering::Relaxed),
            copies_to_keep: self.copies_to_keep.load(Ordering::Relaxed),
        }
    }

    pub fn update(&self, interval_hours: u64, copies_to_keep: u64) {
        self.interval_hours.store(interval_hours, Ordering::Relaxed);
        self.copies_to_keep
            .store(copies_to_keep.max(1), Ordering::Relaxed);
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
pub struct AppState {
    pub db: crate::db::DbPool,
    pub ffmpeg_available: bool,
    pub ffmpeg_webp_available: bool,
    pub job_queue: std::sync::Arc<crate::workers::JobQueue>,
    pub backup_progress: std::sync::Arc<crate::middleware::BackupProgress>,
    pub auto_full_backup_settings: AutoFullBackupSettings,
    pub maintenance_gate: MaintenanceGate,
    pub chan_ledger: Option<std::sync::Arc<parking_lot::Mutex<crate::chan_net::ledger::TxLedger>>>,
    pub onion_address: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}

#[cfg(test)]
mod tests {
    use super::AutoFullBackupSettings;

    #[test]
    fn auto_full_backup_settings_clamps_copies_to_keep() {
        let settings = AutoFullBackupSettings::new(24, 0);
        assert_eq!(settings.snapshot().copies_to_keep, 1);

        settings.update(12, 0);
        let snapshot = settings.snapshot();
        assert_eq!(snapshot.interval_hours, 12);
        assert_eq!(snapshot.copies_to_keep, 1);
    }
}
