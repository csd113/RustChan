//! HTTP server, terminal console, and administration CLI.

/// Command-line argument parsing and administration commands.
pub mod cli;
/// Full-screen terminal console.
pub mod console;
// The nested module name matches the server layer layout and keeps the public path stable.
#[expect(
    clippy::module_inception,
    reason = "the nested name preserves the established crate::server::server module path"
)]
/// HTTP runtime implementation.
pub mod server;

use std::path::{Path, PathBuf};

pub use server::run_server;

// Re-export the global atomics so console/ (and any future module) can
// reference them as `crate::server::REQUEST_COUNT` etc. rather than the
// longer `crate::server::server::REQUEST_COUNT`.
pub use server::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT, SPINNER_TICK};

// Re-export cleanup so main.rs panic hook can call it without a long path.
pub use console::cleanup;

/// Apply the shared request boundary to a secondary listener.
pub async fn request_boundary_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    server::secondary_listener_request_boundary(request, next).await
}

/// Return a path's non-empty parent or the current-directory marker.
#[must_use]
pub fn parent_dir_or_current(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}
