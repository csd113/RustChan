/// Backup-operation progress shared with status endpoints.
mod backup_progress;
/// Cross-site request forgery token validation.
mod csrf;
/// Access coordination for backups, requests, and background writers.
pub(crate) mod data_access;
/// Trusted-proxy and client-address extraction.
mod ip;
/// Request-path normalization.
mod normalize;
/// Origin, referrer, and fetch-metadata checks for state-changing requests.
mod origin;
/// Concurrency admission for memory-hard authentication work.
mod work_gate;

/// Per-client request rate limiting.
mod rate_limit;
/// Bounded storage for browsing, action, and authentication budgets.
mod rate_table;
/// Shared application and maintenance state.
mod state;
/// Request transport metadata and cookie context.
mod transport;

pub use backup_progress::{backup_phase, BackupProgress};
pub use csrf::{validate_csrf, validate_signed_csrf};

pub use ip::{extract_ip, ClientIp};
pub use normalize::normalize_trailing_slash;
pub use rate_limit::rate_limit_middleware;

pub(crate) use origin::check_same_origin;
pub(crate) use state::MaintenanceGuard;
pub use state::{
    AppState, AutoFullBackupSettings, AutoFullBackupSettingsSnapshot, DbMaintenanceJobPhase,
    DbMaintenanceJobStatus, DbMaintenanceJobs, MaintenanceGate,
};
pub(crate) use state::{MediaUploadGate, MediaUploadGuard};
pub use transport::{RequestTransport, SecureCookieContext};

/// Returns whether a trusted proxy reported HTTPS for the request.
pub(crate) fn forwarded_proto_is_https(
    headers: &axum::http::HeaderMap,
    peer: Option<std::net::SocketAddr>,
    behind_proxy: bool,
) -> bool {
    ip::forwarded_proto_is_https(headers, peer, behind_proxy)
}

pub(crate) use rate_table::RateTable;
pub(crate) use work_gate::WorkGate;
