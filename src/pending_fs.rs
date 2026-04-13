use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;

pub const UPLOAD_FINALIZE_KIND: &str = "upload_finalize";
pub const FULL_RESTORE_SWAP_KIND: &str = "full_restore_swap";
pub const BOARD_RESTORE_SWAP_KIND: &str = "board_restore_swap";

#[derive(Clone)]
pub struct PendingFsOpInsert {
    pub id: String,
    pub kind: &'static str,
    pub payload_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadFinalizePayload {
    pub stage_dir: String,
    pub relative_paths: Vec<String>,
    pub primary_hash: Option<String>,
    pub primary_file_path: Option<String>,
    pub primary_thumb_path: Option<String>,
    pub primary_mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullRestoreSwapPayload {
    pub staged: String,
    pub live: String,
    pub previous: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardRestoreSwapPayload {
    pub staged: String,
    pub live: String,
    pub previous: String,
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Create parent directory {}", parent.display()))?;
    }
    Ok(())
}

fn move_stage_file(stage_dir: &Path, upload_dir: &Path, relative_path: &str) -> Result<()> {
    let source = stage_dir.join(relative_path);
    let target = upload_dir.join(relative_path);
    if !source.exists() {
        if target.exists() {
            return Ok(());
        }
        anyhow::bail!(
            "Pending staged file {} is missing and target {} does not exist",
            source.display(),
            target.display()
        );
    }

    ensure_parent_dir(&target)?;
    if target.exists() {
        std::fs::remove_file(&source).with_context(|| {
            format!("Remove already-finalized staged file {}", source.display())
        })?;
        return Ok(());
    }

    std::fs::rename(&source, &target)
        .with_context(|| format!("Move staged file {} into place", target.display()))
}

fn cleanup_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Remove directory {}", path.display()))?;
    } else {
        std::fs::remove_file(path).with_context(|| format!("Remove file {}", path.display()))?;
    }

    Ok(())
}

fn cleanup_empty_parent_dir(path: &Path, live: &Path) {
    if let Some(parent) = path.parent() {
        if parent != live
            && parent.exists()
            && parent
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_none())
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn finalize_swap(staged: &Path, live: &Path, previous: &Path) -> Result<()> {
    let staged_exists = staged.exists();
    let live_exists = live.exists();
    let previous_exists = previous.exists();

    if staged_exists && live_exists {
        if previous_exists {
            cleanup_path_if_exists(previous)?;
        }
        std::fs::rename(live, previous)
            .with_context(|| format!("Move live path {} aside", live.display()))?;
        ensure_parent_dir(live)?;
        if let Err(error) = std::fs::rename(staged, live) {
            let move_error =
                anyhow::anyhow!("Move staged path {} into place: {error}", live.display());
            if previous.exists() && !live.exists() {
                ensure_parent_dir(live)?;
                std::fs::rename(previous, live).with_context(|| {
                    format!(
                        "{move_error}; rollback live path {} from {}",
                        live.display(),
                        previous.display()
                    )
                })?;
            }
            return Err(move_error);
        }
    } else if staged_exists && !live_exists {
        ensure_parent_dir(live)?;
        std::fs::rename(staged, live)
            .with_context(|| format!("Move staged path {} into place", live.display()))?;
    }

    if previous.exists() {
        if let Err(error) = cleanup_path_if_exists(previous) {
            warn!(
                live = %live.display(),
                previous = %previous.display(),
                error = %error,
                "Restore swap completed but could not remove previous path"
            );
        }
    }

    cleanup_empty_parent_dir(staged, live);
    Ok(())
}

/// Finalize a staged upload by moving its files into the live upload tree and
/// refreshing deduplication metadata.
///
/// # Errors
/// Returns an error if any staged file cannot be finalized or the dedup row
/// cannot be written.
pub fn finalize_upload_payload(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    payload: &UploadFinalizePayload,
) -> Result<()> {
    let upload_root = Path::new(upload_dir);
    let stage_dir = Path::new(&payload.stage_dir);

    for relative_path in &payload.relative_paths {
        move_stage_file(stage_dir, upload_root, relative_path)?;
    }

    if let (Some(hash), Some(file_path), Some(thumb_path), Some(mime_type)) = (
        payload.primary_hash.as_deref(),
        payload.primary_file_path.as_deref(),
        payload.primary_thumb_path.as_deref(),
        payload.primary_mime_type.as_deref(),
    ) {
        crate::db::record_file_hash(conn, hash, file_path, thumb_path, mime_type)?;
    }

    cleanup_path_if_exists(stage_dir)?;
    Ok(())
}

/// Finalize a full-site restore upload-directory swap.
///
/// # Errors
/// Returns an error if the staged or backup directories cannot be moved or
/// cleaned up.
pub fn finalize_full_restore_payload(payload: &FullRestoreSwapPayload) -> Result<()> {
    finalize_swap(
        Path::new(&payload.staged),
        Path::new(&payload.live),
        Path::new(&payload.previous),
    )
}

/// Finalize a board-level restore directory swap.
///
/// # Errors
/// Returns an error if the staged or backup directories cannot be moved or
/// cleaned up.
pub fn finalize_board_restore_payload(payload: &BoardRestoreSwapPayload) -> Result<()> {
    finalize_swap(
        Path::new(&payload.staged),
        Path::new(&payload.live),
        Path::new(&payload.previous),
    )
}

/// Create a durable per-request upload staging directory under `.pending/`.
///
/// # Errors
/// Returns an error if the pending root or request-specific stage directory
/// cannot be created.
pub fn create_stage_root(upload_dir: &str, prefix: &str) -> Result<PathBuf> {
    let pending_root = Path::new(upload_dir).join(".pending");
    std::fs::create_dir_all(&pending_root)
        .with_context(|| format!("Create pending upload root {}", pending_root.display()))?;
    let stage_root = pending_root.join(format!("{prefix}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&stage_root)
        .with_context(|| format!("Create upload stage root {}", stage_root.display()))?;
    Ok(stage_root)
}

/// Reconcile any pending filesystem operations left behind by a crash.
///
/// # Errors
/// Returns an error if a pending operation cannot be decoded or completed, or
/// if orphaned upload staging directories cannot be cleaned up.
pub fn reconcile_pending_fs_ops(pool: &crate::db::DbPool, upload_dir: &str) -> Result<()> {
    let conn = pool
        .get()
        .context("Get DB connection for pending_fs_ops reconciliation failed")?;
    let pending_ops = crate::db::list_pending_fs_ops(&conn)?;
    drop(conn);
    let mut referenced_upload_stage_dirs = std::collections::HashSet::new();

    for op in pending_ops {
        let conn = pool
            .get()
            .context("Get DB connection for pending_fs_op application failed")?;
        match op.kind.as_str() {
            UPLOAD_FINALIZE_KIND => {
                let payload: UploadFinalizePayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse upload_finalize payload for {}", op.id))?;
                referenced_upload_stage_dirs.insert(payload.stage_dir.clone());
                finalize_upload_payload(&conn, upload_dir, &payload)?;
            }
            FULL_RESTORE_SWAP_KIND => {
                let payload: FullRestoreSwapPayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse full_restore_swap payload for {}", op.id))?;
                finalize_full_restore_payload(&payload)?;
            }
            BOARD_RESTORE_SWAP_KIND => {
                let payload: BoardRestoreSwapPayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse board_restore_swap payload for {}", op.id))?;
                finalize_board_restore_payload(&payload)?;
            }
            other => {
                anyhow::bail!("Unknown pending_fs_op kind {other:?} for {}", op.id);
            }
        }

        crate::db::delete_pending_fs_op(&conn, &op.id)?;
    }

    let pending_root = Path::new(upload_dir).join(".pending");
    if pending_root.exists() {
        for entry in std::fs::read_dir(&pending_root)
            .with_context(|| format!("Read pending upload directory {}", pending_root.display()))?
        {
            let entry = entry.with_context(|| {
                format!("Read pending upload entry in {}", pending_root.display())
            })?;
            let path = entry.path();
            if referenced_upload_stage_dirs.contains(&path.display().to_string()) {
                continue;
            }
            cleanup_path_if_exists(&path)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{finalize_board_restore_payload, BoardRestoreSwapPayload};

    fn create_dir_with_file(path: &std::path::Path, file_name: &str, contents: &str) {
        std::fs::create_dir_all(path).expect("create dir");
        std::fs::write(path.join(file_name), contents).expect("write file");
    }

    #[test]
    fn finalize_board_restore_payload_recovers_interrupted_swap() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let live = temp_dir.path().join("uploads").join("tech");
        let staged = temp_dir.path().join(".pending").join("tech-stage");
        let previous = temp_dir.path().join(".tech.restore-old");
        create_dir_with_file(&staged, "new.txt", "new");
        create_dir_with_file(&previous, "old.txt", "old");

        finalize_board_restore_payload(&BoardRestoreSwapPayload {
            staged: staged.display().to_string(),
            live: live.display().to_string(),
            previous: previous.display().to_string(),
        })
        .expect("finalize interrupted swap");

        assert_eq!(
            std::fs::read_to_string(live.join("new.txt")).expect("read live"),
            "new"
        );
        assert!(!previous.exists());
    }

    #[test]
    fn finalize_board_restore_payload_cleans_leftover_previous_path() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let live = temp_dir.path().join("uploads").join("tech");
        let staged = temp_dir.path().join(".pending").join("tech-stage");
        let previous = temp_dir.path().join(".tech.restore-old");
        create_dir_with_file(&live, "new.txt", "new");
        create_dir_with_file(&previous, "old.txt", "old");

        finalize_board_restore_payload(&BoardRestoreSwapPayload {
            staged: staged.display().to_string(),
            live: live.display().to_string(),
            previous: previous.display().to_string(),
        })
        .expect("cleanup completed swap");

        assert_eq!(
            std::fs::read_to_string(live.join("new.txt")).expect("read live"),
            "new"
        );
        assert!(!previous.exists());
        assert!(!staged.exists());
    }

    #[test]
    fn finalize_board_restore_payload_swaps_live_and_stage() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let live = temp_dir.path().join("uploads").join("tech");
        let staged = temp_dir.path().join(".pending").join("tech-stage");
        let previous = temp_dir.path().join(".tech.restore-old");
        create_dir_with_file(&live, "old.txt", "old");
        create_dir_with_file(&staged, "new.txt", "new");

        finalize_board_restore_payload(&BoardRestoreSwapPayload {
            staged: staged.display().to_string(),
            live: live.display().to_string(),
            previous: previous.display().to_string(),
        })
        .expect("swap live and stage");

        assert_eq!(
            std::fs::read_to_string(live.join("new.txt")).expect("read live"),
            "new"
        );
        assert!(!staged.exists());
        assert!(!previous.exists());
    }
}
