//! Exclusive operation ownership and durable restore recovery evidence.

use crate::error::{AppError, Result};
use crate::handlers::admin::SESSION_COOKIE;
use crate::middleware::AppState;
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Held by the actual blocking operation, even after request cancellation.
pub(super) struct BackupOperation {
    access: crate::middleware::data_access::DataAccess,
    _exclusive: tokio::sync::OwnedRwLockWriteGuard<()>,
    _maintenance: crate::middleware::MaintenanceGuard,
}

impl Drop for BackupOperation {
    fn drop(&mut self) {
        // A failed restore may have committed its database before filesystem
        // completion. Never reopen admission merely because its request ended.
        if journal_path().try_exists().unwrap_or(true) {
            self.access.block();
        }
    }
}

/// Authenticate before closing admission, then drain all live-state users.
pub(super) async fn begin_operation(
    state: &AppState,
    jar: &CookieJar,
    label: &str,
) -> Result<BackupOperation> {
    let maintenance = reserve_operation(state, jar, label).await?;
    drain_operation(state, maintenance).await
}

/// Reserve maintenance during upload without blocking normal live traffic.
pub(super) async fn reserve_operation(
    state: &AppState,
    jar: &CookieJar,
    label: &str,
) -> Result<crate::middleware::MaintenanceGuard> {
    let access = state.data_access.try_read().ok_or_else(|| {
        AppError::Conflict("Live data is unavailable during maintenance or recovery.".into())
    })?;
    let session = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let pool = state.db.clone();
    crate::middleware::data_access::spawn_blocking(move || -> Result<()> {
        let _access = access;
        let conn = pool.get()?;
        super::require_admin_session_sid(&conn, session.as_deref()).map(|_admin| ())
    })
    .await
    .map_err(|error| AppError::Internal(error.into()))??;
    state.maintenance_gate.try_begin(label)
}

/// Close admission only when prepared input is ready for the actual operation.
pub(super) async fn drain_operation(
    state: &AppState,
    maintenance: crate::middleware::MaintenanceGuard,
) -> Result<BackupOperation> {
    let exclusive = state.data_access.exclusive().await.ok_or_else(|| {
        AppError::Conflict(
            "Could not drain live operations; retry after current work finishes.".into(),
        )
    })?;
    Ok(BackupOperation {
        access: state.data_access.clone(),
        _exclusive: exclusive,
        _maintenance: maintenance,
    })
}

/// Recovery record lives outside the database being replaced.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreJournal {
    pending_id: String,
    snapshot: PathBuf,
}

fn journal_path() -> PathBuf {
    crate::config::runtime_dir().join("restore-recovery.json")
}

/// Persist recovery evidence before any live database mutation.
pub(super) fn begin_restore(pending_id: &str, snapshot: &Path) -> Result<()> {
    std::fs::File::open(snapshot)?.sync_all()?;
    sync_parent(snapshot)?;
    let path = journal_path();
    if path.try_exists().map_err(anyhow::Error::from)? {
        return Err(AppError::Conflict(
            "An interrupted restore requires recovery before retrying.".into(),
        ));
    }
    let bytes = serde_json::to_vec(&RestoreJournal {
        pending_id: pending_id.to_owned(),
        snapshot: snapshot.to_path_buf(),
    })
    .map_err(anyhow::Error::from)?;
    if let Some(parent) = path.parent() {
        crate::config::ensure_private_dir(parent)?;
    }
    let mut file = super::recovery_files::open_private_file_new(&path)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.sync_all()?;
    sync_parent(&path)
}

/// Remove the recovery marker only after database and filesystem agree.
pub(super) fn finish_restore() -> Result<()> {
    let path = journal_path();
    std::fs::remove_file(&path).map_err(anyhow::Error::from)?;
    sync_parent(&path)
}

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(anyhow::Error::from)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Make staged payloads durable before committing their database journal.
pub(super) fn sync_staged_tree(root: &Path) -> Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut directories = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            directories.push(path.clone());
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else {
            crate::utils::fs_security::assert_regular_file_no_symlink(&path)?;
            std::fs::File::open(&path)?.sync_all()?;
        }
    }
    #[cfg(unix)]
    for directory in directories.into_iter().rev() {
        std::fs::File::open(directory)?.sync_all()?;
    }
    sync_parent(root)
}

/// Reconcile interrupted restores before startup admits requests or workers.
pub(in crate::server) fn reconcile_startup(
    pool: &super::db::DbPool,
    uploads: &str,
) -> anyhow::Result<()> {
    let path = journal_path();
    let journal = if path.try_exists()? {
        crate::utils::fs_security::assert_regular_file_no_symlink(&path)?;
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() <= 16 * 1024,
            "restore recovery record is too large"
        );
        let journal: RestoreJournal = serde_json::from_slice(&std::fs::read(&path)?)?;
        let _validated_id = uuid::Uuid::parse_str(&journal.pending_id)?;
        Some(journal)
    } else {
        None
    };
    let conn = pool.get()?;
    let pending = super::db::list_pending_fs_ops(&conn)?;
    let restoring = pending.iter().any(|op| {
        matches!(
            op.kind.as_str(),
            crate::pending_fs::FULL_RESTORE_SWAP_KIND | crate::pending_fs::BOARD_RESTORE_SWAP_KIND
        )
    });
    if let Some(journal) = &journal {
        anyhow::ensure!(
            pending.iter().any(|op| op.id == journal.pending_id
                && matches!(
                    op.kind.as_str(),
                    crate::pending_fs::FULL_RESTORE_SWAP_KIND
                        | crate::pending_fs::BOARD_RESTORE_SWAP_KIND
                )),
            "Interrupted restore requires manual recovery; retained database snapshot: {}",
            journal.snapshot.display()
        );
    }
    drop(conn);
    match crate::pending_fs::reconcile_pending_fs_ops(pool, uploads) {
        Ok(()) => {}
        Err(error) if restoring || journal.is_some() => return Err(error),
        Err(error) => {
            tracing::warn!(%error, "startup filesystem reconciliation left quarantined operations for retry");
        }
    }
    if restoring || journal.is_some() {
        let recovered_conn = pool.get()?;
        super::db::verify_database_schema(&recovered_conn)?;
    }
    if journal.is_some() {
        finish_restore()
            .map_err(|error| anyhow::anyhow!("Clear recovered restore journal: {error}"))?;
        // Retain the private safety snapshot for operator inspection after an
        // interrupted restore; recovery never trusts its path for deletion.
    }
    Ok(())
}
