//! Stable release discovery and the deliberately small local updater protocol.
//!
//! No request can specify a URL, path, command, or service name.
#[cfg(target_os = "linux")]
/// Retained source-controller preparation before any candidate can be selected.
mod bootstrap;
#[cfg(target_os = "linux")]
/// Bounded kernel-authenticated control frames and close-on-exec lease transfers.
mod control;
#[cfg(target_os = "linux")]
/// Selected same-executable full update controller.
mod controller;
#[cfg(target_os = "linux")]
/// Dedicated main-thread process monitor for the one installed executable.
mod coordinator;
/// Restricted local IPC and fixed native service control.
mod daemon;
#[cfg(target_os = "linux")]
/// Same-binary ordinary-account process-tree ownership.
mod guardian;
#[cfg(target_os = "linux")]
/// Durable same-account writer admission and process-lifetime recovery.
mod lifecycle;
#[cfg(target_os = "linux")]
/// Ordinary foreground same-binary writer admission.
mod native;
/// Official stable release discovery and cryptographic verification.
mod release;
#[cfg(unix)]
/// Configuration transactions sharing the update control boundary.
mod restart;
#[cfg(unix)]
mod snapshot;
mod transaction;
/// Fixed official public verification identity included in source builds.
mod trust;

pub use daemon::run;
pub use release::{discover, platform_target, Discovery, Release};
pub use transaction::{BackupInfo, Operation, Phase, Status};

/// Verify native Linux release discovery with the embedded official public key.
/// Other platforms retain their platform-specific check-only discovery.
#[must_use]
pub fn discover_official(current: &str) -> Discovery {
    if !cfg!(target_os = "linux") {
        return discover(current, None);
    }
    match trust::official_public_key() {
        Ok(key) => discover(current, Some(&key)),
        Err(error) => {
            Discovery::UnableToCheck(format!("Invalid embedded release identity: {error}"))
        }
    }
}

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Fixed operator-installed Unix socket; HTTP cannot override this endpoint.
pub const SOCKET: &str = "/run/rustchan-updater/control.sock";
/// Version of this updater/application build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Handle a kernel-parent authenticated internal Linux role before configuration side effects.
///
/// # Errors
/// Rejects malformed, stale or unauthorized internal invocation context.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::missing_const_for_fn,
        reason = "the shared entry API invokes non-const kernel operations on Linux"
    )
)]
pub fn native_entry() -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        native::entry()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(false)
    }
}

/// Preserve the admitted data binding when executing a retained same-binary copy.
#[must_use]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::missing_const_for_fn,
        reason = "the shared data binding is populated by non-const Linux admission"
    )
)]
pub fn admitted_data_dir() -> Option<&'static std::path::Path> {
    #[cfg(target_os = "linux")]
    {
        native::data_dir()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Admit ordinary Linux application/CLI writers before database or filesystem initialization.
///
/// # Errors
/// Rejects concurrent recovery, uncertain descendant lifetime or unsafe data ownership.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::missing_const_for_fn,
        reason = "the shared launcher API performs non-const kernel admission on Linux"
    )
)]
pub fn supervise_native(administrator: bool) -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        native::before_startup(administrator)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _: bool = administrator;
        Ok(false)
    }
}

/// Bind normal application initialization to its admitted writer guardian.
///
/// # Errors
/// Rejects an unavailable or mismatched admitted parent.
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        clippy::unused_async,
        reason = "the shared startup API awaits an authenticated parent acknowledgment on Linux"
    )
)]
pub async fn writer_initialized() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        native::initialized().await
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Record a fully initialized process bound to its exact loaded settings.
    Started {
        /// Fresh process identity; replay cannot overwrite a restart's generation.
        instance: uuid::Uuid,
        /// Private SHA-256 binding of startup settings, not exposed in readiness.
        configuration: String,
    },
    /// Request only the fixed `RustChan` settings restart, once per running instance.
    Restart {
        /// Last ready process identity, issued by the application, never browser-selected.
        instance: uuid::Uuid,
        /// Authenticated full administrator.
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
    #[cfg(target_os = "linux")]
    if native::automatic() {
        return true;
    }
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
    #[cfg(target_os = "linux")]
    if native::automatic() {
        return controller::request(request).await;
    }
    anyhow::ensure!(
        managed(),
        "installation is managed by the deployment environment"
    );
    request_to(std::path::Path::new(SOCKET), request).await
}

/// All external requests stay closed during source trials, including GET side effects.
pub async fn requests_allowed() -> bool {
    #[cfg(target_os = "linux")]
    if native::automatic() {
        let Ok(context) = native::application_context() else {
            return false;
        };
        return tokio::task::spawn_blocking(move || {
            coordinator::call(&context, coordinator::Request::PublicAdmission).is_ok()
        })
        .await
        .is_ok_and(|allowed| allowed);
    }
    if managed() {
        return mutations_allowed(Some(std::path::Path::new(SOCKET))).await;
    }
    true
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
                let request_bytes = serde_json::to_vec(request)?;
                anyhow::ensure!(request_bytes.len() < 4096, "updater request is too large");
                socket.write_all(&request_bytes).await?;
                socket.shutdown().await?;
                let mut checked_data = Vec::new();
                socket
                    .take(512 * 1024 + 1)
                    .read_to_end(&mut checked_data)
                    .await
                    .map(|_bytes_read| ())?;
                anyhow::ensure!(
                    checked_data.len() <= 512 * 1024,
                    "updater response is too large"
                );
                serde_json::from_slice(&checked_data).map_err(Into::into)
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
    for _ in 0_i32..120_i32 {
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

#[cfg(test)]
#[cfg(unix)]
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
        for extra in ["command", "arguments", "service", "executable", "path"] {
            let value = serde_json::json!({"operation":"restart", "instance": uuid::Uuid::new_v4(), "administrator":1_i32, extra: "anything"});
            anyhow::ensure!(
                serde_json::from_value::<Request>(value).is_err(),
                "restart must reject arbitrary {extra}"
            );
        }
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
