/// Phase codes stored in `BackupProgress::phase`.
pub mod backup_phase {
    /// No backup operation is running.
    pub const IDLE: u64 = 0;
    /// The database snapshot is being created.
    pub const SNAPSHOT_DB: u64 = 1;
    /// Files are being counted before compression.
    pub const COUNT_FILES: u64 = 2;
    /// Backup contents are being compressed.
    pub const COMPRESS: u64 = 3;
    /// The backup operation completed.
    pub const DONE: u64 = 5;
}

/// Shared atomic progress state for backup operations.
#[derive(Debug)]
pub struct BackupProgress {
    /// Current backup phase code.
    pub phase: std::sync::atomic::AtomicU64,
    /// Number of files processed so far.
    pub files_done: std::sync::atomic::AtomicU64,
    /// Total number of files to process.
    pub files_total: std::sync::atomic::AtomicU64,
    /// Number of bytes processed so far.
    pub bytes_done: std::sync::atomic::AtomicU64,
    /// Total number of bytes to process.
    pub bytes_total: std::sync::atomic::AtomicU64,
}

impl BackupProgress {
    /// Creates idle backup progress with all counters set to zero.
    #[must_use]
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

    /// Clears all counters and publishes a new backup phase.
    pub fn reset(&self, phase: u64) {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        self.files_done.store(0, Relaxed);
        self.files_total.store(0, Relaxed);
        self.bytes_done.store(0, Relaxed);
        self.bytes_total.store(0, Relaxed);
        self.phase.store(phase, Release);
    }
}

impl Default for BackupProgress {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{backup_phase, BackupProgress};

    #[test]
    fn backup_progress_initial_phase_is_idle() {
        use std::sync::atomic::Ordering::Acquire;
        let bp = BackupProgress::new();
        assert_eq!(
            bp.phase.load(Acquire),
            backup_phase::IDLE,
            "new backup progress should start idle"
        );
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

        assert_eq!(
            bp.phase.load(Acquire),
            backup_phase::COMPRESS,
            "reset should publish the requested phase"
        );
        assert_eq!(
            bp.files_done.load(Relaxed),
            0,
            "reset should clear completed files"
        );
        assert_eq!(
            bp.files_total.load(Relaxed),
            0,
            "reset should clear the total file count"
        );
        assert_eq!(
            bp.bytes_done.load(Relaxed),
            0,
            "reset should clear completed bytes"
        );
        assert_eq!(
            bp.bytes_total.load(Relaxed),
            0,
            "reset should clear the total byte count"
        );
    }
}
