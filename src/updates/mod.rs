//! Administrator-requested stable version checks. Installation is always manual.

/// Read-only admission checks for retained recovery evidence from older versions.
mod legacy;
/// Bounded discovery from the fixed official GitHub API.
mod release;

pub use legacy::check_legacy_state;
pub use release::{discover_official, Discovery, Release, RELEASES_URL};

use parking_lot::RwLock;
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Version of the running application.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default, Serialize)]
/// In-memory result of the latest administrator-requested check.
pub struct Status {
    /// Stable release comparison or a safe discovery error.
    pub discovery: Option<Discovery>,
    /// Time the last outbound check completed; no credentials or deployment paths.
    pub checked_at: Option<String>,
}

impl Status {
    /// A newer stable release, when the last check found one.
    #[must_use]
    pub const fn available_release(&self) -> Option<&Release> {
        match &self.discovery {
            Some(Discovery::Available(release)) => Some(release),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
/// Share one bounded check without blocking requests, backups, or normal startup.
pub struct CheckState {
    /// Only one outbound request at a time; repeated clicks reuse a recent result.
    last_attempt: Mutex<Option<Instant>>,
    /// Readers never wait for the outbound network request.
    status: RwLock<Status>,
}

impl CheckState {
    /// Read cached metadata without contacting GitHub or touching persistent state.
    #[must_use]
    pub fn snapshot(&self) -> Status {
        self.status.read().clone()
    }

    /// Perform one user-requested check, at most once per minute.
    ///
    /// # Errors
    /// Returns an error if another administrator's check is still running.
    pub async fn check(&self) -> anyhow::Result<()> {
        let mut last_attempt = self
            .last_attempt
            .try_lock()
            .map_err(|_error| anyhow::anyhow!("A release check is already running."))?;
        if last_attempt.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
            return Ok(());
        }
        let discovery = discover_official(VERSION).await;
        *self.status.write() = Status {
            discovery: Some(discovery),
            checked_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        *last_attempt = Some(Instant::now());
        drop(last_attempt);
        Ok(())
    }
}
