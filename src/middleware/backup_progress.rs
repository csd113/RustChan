/// Phase codes stored in `BackupProgress::phase`.
pub mod backup_phase {
    pub const IDLE: u64 = 0;
    pub const SNAPSHOT_DB: u64 = 1;
    pub const COUNT_FILES: u64 = 2;
    pub const COMPRESS: u64 = 3;
    pub const DONE: u64 = 5;
}

/// Shared atomic progress state for backup operations.
pub struct BackupProgress {
    pub phase: std::sync::atomic::AtomicU64,
    pub files_done: std::sync::atomic::AtomicU64,
    pub files_total: std::sync::atomic::AtomicU64,
    pub bytes_done: std::sync::atomic::AtomicU64,
    pub bytes_total: std::sync::atomic::AtomicU64,
}

impl BackupProgress {
    pub const fn new() -> Self {
        use std::sync::atomic::AtomicU64;
        Self {
            phase: AtomicU64::new(backup_phase::IDLE),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
        }
    }

    pub fn reset(&self, phase: u64) {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        self.files_done.store(0, Relaxed);
        self.files_total.store(0, Relaxed);
        self.bytes_done.store(0, Relaxed);
        self.bytes_total.store(0, Relaxed);
        self.phase.store(phase, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{backup_phase, BackupProgress};

    #[test]
    fn backup_progress_initial_phase_is_idle() {
        use std::sync::atomic::Ordering::Acquire;
        let bp = BackupProgress::new();
        assert_eq!(bp.phase.load(Acquire), backup_phase::IDLE);
    }

    #[test]
    fn backup_progress_reset_clears_counters() {
        use std::sync::atomic::Ordering::{Acquire, Relaxed};
        let bp = BackupProgress::new();
        bp.files_done.store(10, Relaxed);
        bp.files_total.store(20, Relaxed);
        bp.bytes_done.store(1024, Relaxed);
        bp.bytes_total.store(2048, Relaxed);

        bp.reset(backup_phase::COMPRESS);

        assert_eq!(bp.phase.load(Acquire), backup_phase::COMPRESS);
        assert_eq!(bp.files_done.load(Relaxed), 0);
        assert_eq!(bp.files_total.load(Relaxed), 0);
        assert_eq!(bp.bytes_done.load(Relaxed), 0);
        assert_eq!(bp.bytes_total.load(Relaxed), 0);
    }
}
