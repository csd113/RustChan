// src/middleware/state.rs

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
    pub maintenance_gate: MaintenanceGate,
    pub chan_ledger: Option<std::sync::Arc<parking_lot::Mutex<crate::chan_net::ledger::TxLedger>>>,
    pub onion_address: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}
