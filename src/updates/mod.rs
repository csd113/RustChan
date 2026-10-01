//! Stable release discovery and the deliberately small local updater protocol.
//!
//! No request can specify a URL, path, command, or service name.
/// Restricted local IPC and fixed native service control.
mod daemon;
/// Official stable release discovery and cryptographic verification.
mod release;
#[cfg(unix)]
mod snapshot;
mod transaction;

pub use daemon::run;
pub use release::{discover, platform_target, Discovery, Release};
pub use transaction::{BackupInfo, Phase, Status};

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Fixed operator-installed Unix socket; HTTP cannot override this endpoint.
pub const SOCKET: &str = "/run/rustchan-updater/control.sock";
/// Version of this updater/application build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "operation", rename_all = "snake_case")]
/// Closed operations accepted from the authorized local web identity.
pub enum Request {
    /// Return durable update state without modifying deployment state.
    Status,
    /// Check only the official release repository.
    Check,
    /// Consume a one-use approval and launch the fixed installation transaction.
    Install {
        /// One-use approval from the last compatible release check.
        approval: String,
        /// Authenticated full administrator requesting installation.
        administrator: i64,
    },
    /// Authorize startup only after updater recovery and current-version validation.
    Ready {
        /// Running package version requesting startup admission.
        version: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Updater state and recovery admission returned over bounded local IPC.
pub struct Reply {
    /// Persisted transaction state.
    pub status: Status,
    /// Sanitized operation failure, absent on success.
    pub error: Option<String>,
    /// Whether updater recovery currently allows the requested admission.
    pub ready: bool,
}

/// The deployment supplies this environment variable; the web settings editor
/// cannot select arbitrary socket endpoints.
#[must_use]
pub fn managed() -> bool {
    cfg!(target_os = "linux")
        && std::env::var("RUSTCHAN_MANAGED").is_ok_and(|value| value == "1")
        && !container_managed()
}

#[must_use]
/// Detect deployments whose images must remain managed by deployment tools.
pub fn container_managed() -> bool {
    std::env::var("RUSTCHAN_CONTAINER").is_ok_and(|value| value == "1")
        || std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
}

/// Send a closed operation to the fixed native updater endpoint.
///
/// # Errors
/// Rejects unmanaged deployments, unavailable IPC, oversized replies or malformed protocol data.
pub async fn request(request: &Request) -> anyhow::Result<Reply> {
    anyhow::ensure!(
        managed(),
        "installation is managed by the deployment environment"
    );
    request_to(std::path::Path::new(SOCKET), request).await
}

/// The application constructor supplies only the fixed deployment socket.
///
/// Kept internal so temporary IPC sockets can test authorization without
/// changing global process environment or touching a host's actual updater.
pub(crate) async fn request_to(
    socket_path: &std::path::Path,
    request: &Request,
) -> anyhow::Result<Reply> {
    #[cfg(unix)]
    {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        tokio::time::timeout(
            Duration::from_secs(if matches!(request, Request::Check) {
                40
            } else {
                2
            }),
            async {
                let mut socket = tokio::net::UnixStream::connect(socket_path).await?;
                let data = serde_json::to_vec(request)?;
                anyhow::ensure!(data.len() < 4096, "updater request is too large");
                socket.write_all(&data).await?;
                socket.shutdown().await?;
                let mut data = Vec::new();
                socket.take(512 * 1024 + 1).read_to_end(&mut data).await?;
                anyhow::ensure!(data.len() <= 512 * 1024, "updater response is too large");
                serde_json::from_slice(&data).map_err(Into::into)
            },
        )
        .await?
    }
    #[cfg(not(unix))]
    {
        let _ = (socket_path, request);
        anyhow::bail!("self-update requires a managed Linux deployment")
    }
}

/// True only after the daemon has recovered and durably committed its terminal
/// journal, with no update/check worker holding the transaction lock.
pub async fn mutations_allowed(socket: Option<&std::path::Path>) -> bool {
    let Some(socket) = socket else {
        return true;
    };
    request_to(socket, &Request::Status)
        .await
        .is_ok_and(|reply| reply.error.is_none() && reply.ready)
}

/// Wait for recovered updater admission before database migration or filesystem reconciliation.
///
/// # Errors
/// Refuses managed startup if the updater is unavailable, recovering, or blocking this version.
pub async fn await_startup() -> anyhow::Result<()> {
    if !managed() {
        return Ok(());
    }
    for _ in 0..120 {
        if request(&Request::Ready {
            version: VERSION.to_owned(),
        })
        .await
        .is_ok_and(|reply| reply.error.is_none() && reply.ready)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("updater recovery has not authorized application startup")
}

#[cfg(all(test, unix))]
/// Fail-closed IPC admission and protocol boundary tests.
mod tests {
    use super::*;

    /// Unknown commands/fields and unavailable updater sockets cannot authorize mutation.
    #[tokio::test]
    async fn protocol_and_mutation_admission_fail_closed() -> anyhow::Result<()> {
        anyhow::ensure!(
            serde_json::from_str::<Request>(
                r#"{"operation":"install","approval":"x","administrator":1,"path":"/tmp"}"#
            )
            .is_err(),
            "paths must not enter the closed protocol"
        );
        anyhow::ensure!(
            serde_json::from_str::<Request>(r#"{"operation":"execute","command":"anything"}"#)
                .is_err(),
            "arbitrary commands must be rejected"
        );
        let dir = tempfile::tempdir()?;
        anyhow::ensure!(
            !mutations_allowed(Some(&dir.path().join("missing.sock"))).await,
            "IPC loss must close mutation admission"
        );
        anyhow::ensure!(
            !Phase::Downloading.blocks_writes() && !Phase::Verifying.blocks_writes(),
            "download and check must preserve normal writes"
        );
        anyhow::ensure!(
            Phase::Activating.blocks_writes() && Phase::RollingBack.blocks_writes(),
            "activation and restore must block mutation"
        );
        Ok(())
    }
}
