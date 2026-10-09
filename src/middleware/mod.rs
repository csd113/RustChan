/// Backup-operation progress shared with status endpoints.
mod backup_progress;
/// Cross-site request forgery token validation.
mod csrf;
/// Trusted-proxy and client-address extraction.
mod ip;
/// Request-path normalization.
mod normalize;
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

/// Non-queueing work allowance shared across clones of one application state.
#[derive(Clone, Debug)]
pub(crate) struct WorkGate {
    /// Owned permits remain with blocking work after request cancellation.
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
}

/// Bounded fixed-window storage for hashed visitor identities.
#[derive(Debug, Default)]
pub(crate) struct RateTable {
    /// Serializes only admission and amortized expiration, never application work.
    state: parking_lot::Mutex<rate_table::TableState>,
}
