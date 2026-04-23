// src/middleware/mod.rs

mod backup_progress;
mod csrf;
mod ip;
mod normalize;
mod rate_limit;
mod state;

pub use backup_progress::{backup_phase, BackupProgress};
pub use csrf::validate_csrf;

// Public re-exports here match the module layout and keep paths stable for callers.
#[allow(clippy::redundant_pub_crate)]
pub(crate) use ip::forwarded_proto_is_https;
pub use ip::{extract_ip, ClientIp};
pub use normalize::normalize_trailing_slash;
pub use rate_limit::rate_limit_middleware;
pub use state::{
    AppState, AutoFullBackupSettings, AutoFullBackupSettingsSnapshot, DbMaintenanceJobStatus,
    DbMaintenanceJobs, MaintenanceGate,
};
