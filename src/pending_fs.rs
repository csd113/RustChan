use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use tracing::warn;

pub const UPLOAD_FINALIZE_KIND: &str = "upload_finalize";
pub const DELETE_FILES_KIND: &str = "delete_files";
pub const FULL_RESTORE_SWAP_KIND: &str = "full_restore_swap";
pub const BOARD_RESTORE_SWAP_KIND: &str = "board_restore_swap";

#[cfg(test)]
static PRIVATE_PERMISSION_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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
pub struct DeleteFilesPayload {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullRestoreSwapPayload {
    pub staged: String,
    pub live: String,
    pub previous: String,
    #[serde(default)]
    pub additional_swaps: Vec<RestorePathSwapPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestorePathSwapPayload {
    pub staged: String,
    pub live: String,
    pub previous: String,
    #[serde(default)]
    pub restrict_private_permissions: bool,
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

fn safe_relative_path(relative_path: &str, context: &str) -> Result<PathBuf> {
    let rel = Path::new(relative_path);
    if relative_path.trim().is_empty() || rel.is_absolute() {
        anyhow::bail!("{context} path {relative_path:?} must be relative");
    }

    let mut normalized = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                anyhow::bail!("{context} path {relative_path:?} contains unsafe components");
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        anyhow::bail!("{context} path {relative_path:?} is empty");
    }
    Ok(normalized)
}

fn validate_upload_finalize_payload(
    upload_dir: &Path,
    payload: &UploadFinalizePayload,
) -> Result<()> {
    let upload_root = validated_restore_path(upload_dir)?;
    let pending_root = upload_root.join(".pending");
    let stage_dir = validated_restore_path(Path::new(&payload.stage_dir))?;
    if stage_dir == pending_root || !stage_dir.starts_with(&pending_root) {
        anyhow::bail!(
            "Upload finalize stage {} is outside {}",
            stage_dir.display(),
            pending_root.display()
        );
    }

    for relative_path in &payload.relative_paths {
        let rel = safe_relative_path(relative_path, "Upload finalize")?;
        let target = upload_root.join(&rel);
        if !target.starts_with(&upload_root) {
            anyhow::bail!(
                "Upload finalize target {} escapes {}",
                target.display(),
                upload_root.display()
            );
        }
    }

    for path in [
        payload.primary_file_path.as_deref(),
        payload.primary_thumb_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let rel = safe_relative_path(path, "Upload finalize metadata")?;
        let target = upload_root.join(&rel);
        if !target.starts_with(&upload_root) {
            anyhow::bail!(
                "Upload finalize metadata target {} escapes {}",
                target.display(),
                upload_root.display()
            );
        }
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

fn absolute_path_without_parent_traversal(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("Resolve current directory for restore path validation failed")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!(
                    "Restore swap path {} contains parent traversal",
                    path.display()
                );
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("Restore swap path is empty");
    }
    Ok(normalized)
}

fn reject_existing_symlink(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Restore swap path {} cannot be a symlink", path.display());
        }
    }
    Ok(())
}

fn validated_restore_path(path: &Path) -> Result<PathBuf> {
    let path = absolute_path_without_parent_traversal(path)?;
    reject_existing_symlink(&path)?;
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("Canonicalize restore swap path {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Restore swap path {} has no parent", path.display()))?;
    reject_existing_symlink(parent)?;
    let parent = parent.canonicalize().with_context(|| {
        format!(
            "Canonicalize restore swap parent directory {}",
            parent.display()
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        anyhow::anyhow!(
            "Restore swap path {} has no final component",
            path.display()
        )
    })?;
    Ok(parent.join(file_name))
}

fn validated_restore_path_allow_missing_parent(path: &Path) -> Result<PathBuf> {
    let path = absolute_path_without_parent_traversal(path)?;
    reject_existing_symlink(&path)?;
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("Canonicalize restore swap path {}", path.display()));
    }
    let Some(parent) = path.parent() else {
        anyhow::bail!("Restore swap path {} has no parent", path.display());
    };
    reject_existing_symlink(parent)?;
    if parent.exists() {
        let parent = parent.canonicalize().with_context(|| {
            format!(
                "Canonicalize restore swap parent directory {}",
                parent.display()
            )
        })?;
        let file_name = path.file_name().ok_or_else(|| {
            anyhow::anyhow!(
                "Restore swap path {} has no final component",
                path.display()
            )
        })?;
        return Ok(parent.join(file_name));
    }
    let mut missing_components = Vec::new();
    let mut existing_ancestor = path.as_path();
    while !existing_ancestor.exists() {
        let Some(file_name) = existing_ancestor.file_name() else {
            anyhow::bail!(
                "Restore swap path {} has no existing ancestor",
                path.display()
            );
        };
        missing_components.push(file_name.to_os_string());
        existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "Restore swap path {} has no existing ancestor",
                path.display()
            )
        })?;
    }
    reject_existing_symlink(existing_ancestor)?;
    let mut normalized = existing_ancestor.canonicalize().with_context(|| {
        format!(
            "Canonicalize restore swap ancestor directory {}",
            existing_ancestor.display()
        )
    })?;
    for component in missing_components.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn expected_restore_path_name(live: &Path, label: &str) -> Result<String> {
    let live_name = live
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Restore live path {} has no file name", live.display()))?;
    Ok(format!(".{live_name}.{label}."))
}

fn validate_restore_generated_name(path: &Path, live: &Path, label: &str) -> Result<()> {
    let expected_prefix = expected_restore_path_name(live, label)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Restore swap path {} has no UTF-8 file name",
                path.display()
            )
        })?;
    let Some(suffix) = file_name.strip_prefix(&expected_prefix) else {
        anyhow::bail!(
            "Restore swap path {} does not match expected {label} name for {}",
            path.display(),
            live.display()
        );
    };
    if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!(
            "Restore swap path {} has an invalid generated suffix",
            path.display()
        );
    }
    Ok(())
}

fn validate_restore_swap_paths(
    swap: &RestorePathSwapPayload,
    allowed_live: &Path,
    require_private_permissions: bool,
) -> Result<()> {
    if require_private_permissions && !swap.restrict_private_permissions {
        anyhow::bail!("Restore swap for private runtime material must restrict permissions");
    }

    let allowed_live = validated_restore_path(allowed_live)?;
    let live = validated_restore_path(Path::new(&swap.live))?;
    if live != allowed_live {
        anyhow::bail!(
            "Restore swap live path {} is not an allowed restore target",
            live.display()
        );
    }

    let live_parent = live
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Restore live path {} has no parent", live.display()))?;
    let staged = validated_restore_path(Path::new(&swap.staged))?;
    let previous = validated_restore_path(Path::new(&swap.previous))?;
    if staged.parent() != Some(live_parent) {
        anyhow::bail!(
            "Restore swap staged path {} is outside the expected restore staging area",
            staged.display()
        );
    }
    if previous.parent() != Some(live_parent) {
        anyhow::bail!(
            "Restore swap previous path {} is outside the expected restore backup area",
            previous.display()
        );
    }
    validate_restore_generated_name(&staged, &live, "restore-stage")?;
    validate_restore_generated_name(&previous, &live, "restore-old")?;
    Ok(())
}

fn validate_full_restore_payload_paths(
    payload: &FullRestoreSwapPayload,
    upload_dir: &Path,
    tor_hidden_service_keys_dir: Option<&Path>,
) -> Result<()> {
    let primary_swap = RestorePathSwapPayload {
        staged: payload.staged.clone(),
        live: payload.live.clone(),
        previous: payload.previous.clone(),
        restrict_private_permissions: false,
    };
    validate_restore_swap_paths(&primary_swap, upload_dir, false)?;

    for swap in &payload.additional_swaps {
        let Some(tor_hidden_service_keys_dir) = tor_hidden_service_keys_dir else {
            anyhow::bail!("Full restore payload contains an unsupported additional swap");
        };
        validate_restore_swap_paths(swap, tor_hidden_service_keys_dir, true)?;
    }
    Ok(())
}

fn validate_board_short_component(path: &Path) -> Result<String> {
    let short = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow::anyhow!("Board restore path {} has no board name", path.display())
        })?;
    if short.is_empty()
        || short.len() > 8
        || !short.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        anyhow::bail!("Board restore target name {short:?} is invalid");
    }
    Ok(short.to_string())
}

fn validate_generated_suffix(file_name: &str, expected_prefix: &str) -> Result<()> {
    let Some(suffix) = file_name.strip_prefix(expected_prefix) else {
        anyhow::bail!("Board restore swap path {file_name:?} does not match expected prefix");
    };
    if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("Board restore swap path {file_name:?} has an invalid generated suffix");
    }
    Ok(())
}

fn validate_board_restore_payload_paths(
    payload: &BoardRestoreSwapPayload,
    upload_dir: &Path,
) -> Result<()> {
    let upload_root = validated_restore_path(upload_dir)?;
    let upload_parent = upload_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Upload root {} has no parent", upload_root.display()))?;
    let live = validated_restore_path(Path::new(&payload.live))?;
    if live.parent() != Some(upload_root.as_path()) {
        anyhow::bail!(
            "Board restore live path {} is not an immediate child of {}",
            live.display(),
            upload_root.display()
        );
    }
    let board_short = validate_board_short_component(&live)?;

    let staged = validated_restore_path_allow_missing_parent(Path::new(&payload.staged))?;
    if staged.file_name().and_then(|name| name.to_str()) != Some(board_short.as_str()) {
        anyhow::bail!("Board restore staged path does not target /{board_short}/");
    }
    let staged_parent = staged.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Board restore staged path {} has no parent",
            staged.display()
        )
    })?;
    if staged_parent.parent() != Some(upload_parent) {
        anyhow::bail!(
            "Board restore staged path {} is outside the expected upload staging area",
            staged.display()
        );
    }
    let staged_parent_name = staged_parent
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Board restore staged parent has no UTF-8 name"))?;
    let upload_name = upload_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Upload root {} has no name", upload_root.display()))?;
    validate_generated_suffix(
        staged_parent_name,
        &format!(".{upload_name}.board-restore-stage."),
    )?;

    let previous = validated_restore_path(Path::new(&payload.previous))?;
    if previous.parent() != Some(upload_root.as_path()) {
        anyhow::bail!(
            "Board restore previous path {} is outside {}",
            previous.display(),
            upload_root.display()
        );
    }
    let previous_name = previous
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Board restore previous path has no UTF-8 name"))?;
    validate_generated_suffix(previous_name, &format!(".{board_short}.restore-old."))?;
    Ok(())
}

/// Finalize a delete-files pending op by removing the listed paths and, if the
/// cleanup succeeds, clearing the durable operation record.
///
/// # Errors
/// Returns an error if any file removal fails. Missing files are treated as
/// already-cleaned and do not fail the operation.
pub fn finalize_delete_files_payload(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    pending_op_id: Option<&str>,
    paths: &[String],
) -> Result<()> {
    let mut cleanup_errors = Vec::new();

    for path in paths {
        if let Err(error) = crate::utils::files::delete_file_checked(upload_dir, path) {
            cleanup_errors.push(anyhow::anyhow!(error));
        }
    }

    if cleanup_errors.is_empty() {
        if let Some(op_id) = pending_op_id {
            if let Err(error) = crate::db::delete_pending_fs_op(conn, op_id) {
                warn!(
                    op_id = %op_id,
                    error = %error,
                    "deleted files but could not clear pending delete op"
                );
            }
        }
        Ok(())
    } else {
        let detail = cleanup_errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("Delete cleanup incomplete: {detail}");
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

fn restrict_private_path_permissions(path: &Path) -> Result<()> {
    #[cfg(test)]
    {
        let private_permission_failure = PRIVATE_PERMISSION_FAILURE
            .lock()
            .expect("private permission failure mutex")
            .clone();
        if let Some(message) = private_permission_failure {
            anyhow::bail!("{message}");
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::metadata(path)
            .with_context(|| format!("Inspect private path {}", path.display()))?;
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("Set private permissions on {}", path.display()))?;

        if metadata.is_dir() {
            for entry in std::fs::read_dir(path)
                .with_context(|| format!("Read private path {}", path.display()))?
            {
                let entry = entry
                    .with_context(|| format!("Read private path entry under {}", path.display()))?;
                restrict_private_path_permissions(&entry.path())?;
            }
        }
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
/// Configure a test-only failure injected during private permission repair.
///
/// # Panics
/// Panics if the test failure mutex is poisoned.
pub fn set_private_permission_failure_for_test(message: Option<String>) {
    *PRIVATE_PERMISSION_FAILURE
        .lock()
        .expect("private permission failure mutex") = message;
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
    validate_upload_finalize_payload(upload_root, payload)?;

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
pub fn finalize_full_restore_payload(
    payload: &FullRestoreSwapPayload,
    upload_dir: &Path,
    tor_hidden_service_keys_dir: Option<&Path>,
) -> Result<()> {
    validate_full_restore_payload_paths(payload, upload_dir, tor_hidden_service_keys_dir)?;
    let primary_swap = RestorePathSwapPayload {
        staged: payload.staged.clone(),
        live: payload.live.clone(),
        previous: payload.previous.clone(),
        restrict_private_permissions: false,
    };
    for swap in std::iter::once(&primary_swap).chain(payload.additional_swaps.iter()) {
        finalize_swap(
            Path::new(&swap.staged),
            Path::new(&swap.live),
            Path::new(&swap.previous),
        )?;
        if swap.restrict_private_permissions {
            restrict_private_path_permissions(Path::new(&swap.live))?;
        }
    }
    Ok(())
}

/// Finalize a board-level restore directory swap.
///
/// # Errors
/// Returns an error if the staged or backup directories cannot be moved or
/// cleaned up.
pub fn finalize_board_restore_payload(
    payload: &BoardRestoreSwapPayload,
    upload_dir: &Path,
) -> Result<()> {
    validate_board_restore_payload_paths(payload, upload_dir)?;
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
            DELETE_FILES_KIND => {
                let payload: DeleteFilesPayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse delete_files payload for {}", op.id))?;
                finalize_delete_files_payload(&conn, upload_dir, Some(&op.id), &payload.paths)?;
            }
            FULL_RESTORE_SWAP_KIND => {
                let payload: FullRestoreSwapPayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse full_restore_swap payload for {}", op.id))?;
                let tor_hidden_service_keys_dir =
                    crate::config::configured_tor_hidden_service_keys_dir();
                finalize_full_restore_payload(
                    &payload,
                    Path::new(upload_dir),
                    tor_hidden_service_keys_dir.as_deref(),
                )?;
            }
            BOARD_RESTORE_SWAP_KIND => {
                let payload: BoardRestoreSwapPayload = serde_json::from_str(&op.payload_json)
                    .with_context(|| format!("Parse board_restore_swap payload for {}", op.id))?;
                finalize_board_restore_payload(&payload, Path::new(upload_dir))?;
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
    use super::{
        finalize_board_restore_payload, finalize_delete_files_payload,
        finalize_full_restore_payload, finalize_upload_payload, BoardRestoreSwapPayload,
        DeleteFilesPayload, FullRestoreSwapPayload, RestorePathSwapPayload, UploadFinalizePayload,
        DELETE_FILES_KIND, FULL_RESTORE_SWAP_KIND,
    };
    use crate::db::{init_test_pool, insert_pending_fs_op};

    fn create_dir_with_file(path: &std::path::Path, file_name: &str, contents: &str) {
        std::fs::create_dir_all(path).expect("create dir");
        std::fs::write(path.join(file_name), contents).expect("write file");
    }

    fn generated_restore_path(live: &std::path::Path, label: &str) -> std::path::PathBuf {
        let parent = live.parent().expect("live parent");
        let name = live
            .file_name()
            .and_then(|name| name.to_str())
            .expect("live name");
        parent.join(format!(".{name}.{label}.0123456789abcdef0123456789abcdef"))
    }

    fn full_restore_payload_for_live(live: &std::path::Path) -> FullRestoreSwapPayload {
        FullRestoreSwapPayload {
            staged: generated_restore_path(live, "restore-stage")
                .display()
                .to_string(),
            live: live.display().to_string(),
            previous: generated_restore_path(live, "restore-old")
                .display()
                .to_string(),
            additional_swaps: Vec::new(),
        }
    }

    fn board_restore_payload_for_live(live: &std::path::Path) -> BoardRestoreSwapPayload {
        let upload_root = live.parent().expect("board live parent");
        let upload_parent = upload_root.parent().expect("upload parent");
        let upload_name = upload_root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("upload name");
        let board_short = live
            .file_name()
            .and_then(|name| name.to_str())
            .expect("board short");
        BoardRestoreSwapPayload {
            staged: upload_parent
                .join(format!(
                    ".{upload_name}.board-restore-stage.0123456789abcdef0123456789abcdef"
                ))
                .join(board_short)
                .display()
                .to_string(),
            live: live.display().to_string(),
            previous: upload_root
                .join(format!(
                    ".{board_short}.restore-old.0123456789abcdef0123456789abcdef"
                ))
                .display()
                .to_string(),
        }
    }

    fn tor_swap_for_live(live: &std::path::Path) -> RestorePathSwapPayload {
        RestorePathSwapPayload {
            staged: generated_restore_path(live, "restore-stage")
                .display()
                .to_string(),
            live: live.display().to_string(),
            previous: generated_restore_path(live, "restore-old")
                .display()
                .to_string(),
            restrict_private_permissions: true,
        }
    }

    #[test]
    fn finalize_delete_files_payload_removes_files_and_clears_pending_op() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let board_dir = upload_dir.join("tech");
        let thumb_dir = board_dir.join("thumbs");
        std::fs::create_dir_all(&thumb_dir).expect("create thumb dir");
        std::fs::write(board_dir.join("file.webp"), "file").expect("write file");
        std::fs::write(thumb_dir.join("file.webp"), "thumb").expect("write thumb");

        let pool = init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");

        let payload = DeleteFilesPayload {
            paths: vec![
                "tech/file.webp".to_string(),
                "tech/thumbs/file.webp".to_string(),
            ],
        };
        let op = crate::pending_fs::PendingFsOpInsert {
            id: "delete-files-op".to_string(),
            kind: DELETE_FILES_KIND,
            payload_json: serde_json::to_string(&payload).expect("serialize payload"),
        };
        insert_pending_fs_op(&conn, &op).expect("insert pending op");

        finalize_delete_files_payload(
            &conn,
            upload_dir.to_str().expect("utf8 upload dir"),
            Some(&op.id),
            &payload.paths,
        )
        .expect("delete cleanup");

        assert!(!board_dir.join("file.webp").exists());
        assert!(!thumb_dir.join("file.webp").exists());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pending_fs_ops", [], |row| row
                .get::<_, i64>(0))
                .expect("pending op count"),
            0
        );

        finalize_delete_files_payload(
            &conn,
            upload_dir.to_str().expect("utf8 upload dir"),
            None,
            &payload.paths,
        )
        .expect("retry cleanup should be idempotent");
    }

    #[test]
    fn finalize_upload_payload_rejects_traversal_relative_path_before_mutation() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let stage_dir = upload_dir.join(".pending").join("stage");
        let sentinel = temp_dir.path().join("sentinel.txt");
        std::fs::create_dir_all(&stage_dir).expect("create stage dir");
        std::fs::write(&sentinel, "keep").expect("write sentinel");

        let pool = init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let payload = UploadFinalizePayload {
            stage_dir: stage_dir.display().to_string(),
            relative_paths: vec!["../sentinel.txt".to_string()],
            primary_hash: None,
            primary_file_path: Some("../sentinel.txt".to_string()),
            primary_thumb_path: None,
            primary_mime_type: None,
        };

        let error = finalize_upload_payload(
            &conn,
            upload_dir.to_str().expect("utf8 upload dir"),
            &payload,
        )
        .expect_err("traversal upload payload rejected");

        assert!(error.to_string().contains("unsafe components"));
        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("read sentinel"),
            "keep"
        );
    }

    #[test]
    fn finalize_upload_payload_rejects_stage_dir_outside_pending_root() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let stage_dir = temp_dir.path().join("outside-stage");
        std::fs::create_dir_all(upload_dir.join(".pending")).expect("create pending root");
        std::fs::create_dir_all(&stage_dir).expect("create outside stage");

        let pool = init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let payload = UploadFinalizePayload {
            stage_dir: stage_dir.display().to_string(),
            relative_paths: vec!["tech/file.webp".to_string()],
            primary_hash: None,
            primary_file_path: Some("tech/file.webp".to_string()),
            primary_thumb_path: None,
            primary_mime_type: None,
        };

        let error = finalize_upload_payload(
            &conn,
            upload_dir.to_str().expect("utf8 upload dir"),
            &payload,
        )
        .expect_err("outside stage rejected");

        assert!(error.to_string().contains("outside"));
        assert!(stage_dir.exists());
    }

    #[test]
    fn finalize_board_restore_payload_rejects_arbitrary_live_path_before_mutation() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let sentinel_dir = temp_dir.path().join("sentinel");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");
        create_dir_with_file(&sentinel_dir, "keep.txt", "keep");
        let payload = BoardRestoreSwapPayload {
            staged: temp_dir
                .path()
                .join(".uploads.board-restore-stage.0123456789abcdef0123456789abcdef")
                .join("sentinel")
                .display()
                .to_string(),
            live: sentinel_dir.display().to_string(),
            previous: upload_dir
                .join(".sentinel.restore-old.0123456789abcdef0123456789abcdef")
                .display()
                .to_string(),
        };

        let error = finalize_board_restore_payload(&payload, &upload_dir)
            .expect_err("arbitrary board restore live rejected");

        assert!(error.to_string().contains("not an immediate child"));
        assert_eq!(
            std::fs::read_to_string(sentinel_dir.join("keep.txt")).expect("read sentinel"),
            "keep"
        );
    }

    #[test]
    fn finalize_board_restore_payload_recovers_interrupted_swap() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let live = temp_dir.path().join("uploads").join("tech");
        let payload = board_restore_payload_for_live(&live);
        let staged = std::path::Path::new(&payload.staged);
        let previous = std::path::Path::new(&payload.previous);
        create_dir_with_file(staged, "new.txt", "new");
        create_dir_with_file(previous, "old.txt", "old");

        finalize_board_restore_payload(&payload, &temp_dir.path().join("uploads"))
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
        let payload = board_restore_payload_for_live(&live);
        let staged = std::path::Path::new(&payload.staged);
        let previous = std::path::Path::new(&payload.previous);
        create_dir_with_file(&live, "new.txt", "new");
        create_dir_with_file(previous, "old.txt", "old");

        finalize_board_restore_payload(&payload, &temp_dir.path().join("uploads"))
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
        let payload = board_restore_payload_for_live(&live);
        let staged = std::path::Path::new(&payload.staged);
        let previous = std::path::Path::new(&payload.previous);
        create_dir_with_file(&live, "old.txt", "old");
        create_dir_with_file(staged, "new.txt", "new");

        finalize_board_restore_payload(&payload, &temp_dir.path().join("uploads"))
            .expect("swap live and stage");

        assert_eq!(
            std::fs::read_to_string(live.join("new.txt")).expect("read live"),
            "new"
        );
        assert!(!staged.exists());
        assert!(!previous.exists());
    }

    #[test]
    fn finalize_full_restore_payload_without_additional_swaps_still_succeeds() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let live = temp_dir.path().join("uploads");
        let payload = full_restore_payload_for_live(&live);
        create_dir_with_file(&live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");

        finalize_full_restore_payload(&payload, &live, None).expect("finalize full restore");

        assert_eq!(
            std::fs::read_to_string(live.join("new.txt")).expect("read live"),
            "new"
        );
        assert!(!std::path::Path::new(&payload.staged).exists());
        assert!(!std::path::Path::new(&payload.previous).exists());
    }

    #[test]
    fn finalize_full_restore_payload_with_valid_tor_swap_succeeds() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let mut payload = full_restore_payload_for_live(&upload_live);
        let tor_swap = tor_swap_for_live(&tor_live);
        payload.additional_swaps.push(tor_swap.clone());

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");
        create_dir_with_file(&tor_live, "hs_ed25519_secret_key", "old-secret");
        create_dir_with_file(
            std::path::Path::new(&tor_swap.staged),
            "hs_ed25519_secret_key",
            "new-secret",
        );

        finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect("finalize full restore with tor swap");

        assert_eq!(
            std::fs::read_to_string(upload_live.join("new.txt")).expect("read upload"),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(tor_live.join("hs_ed25519_secret_key")).expect("read tor key"),
            "new-secret"
        );
    }

    #[test]
    fn finalize_full_restore_rejects_tor_swap_without_private_permissions_flag() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let mut payload = full_restore_payload_for_live(&upload_live);
        let mut tor_swap = tor_swap_for_live(&tor_live);
        tor_swap.restrict_private_permissions = false;
        payload.additional_swaps.push(tor_swap);

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");
        create_dir_with_file(&tor_live, "hs_ed25519_secret_key", "old-secret");
        create_dir_with_file(&tor_live, "hs_ed25519_public_key", "old-public");

        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("tor swap without private permissions rejected");

        assert!(error.to_string().contains("must restrict permissions"));
        assert_eq!(
            std::fs::read_to_string(tor_live.join("hs_ed25519_secret_key")).expect("read tor key"),
            "old-secret"
        );
    }

    #[test]
    fn finalize_full_restore_rejects_arbitrary_additional_live_before_mutation() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let victim_live = temp_dir.path().join("victim");
        let mut payload = full_restore_payload_for_live(&upload_live);
        let mut malicious_swap = tor_swap_for_live(&victim_live);
        malicious_swap.live = victim_live.display().to_string();
        payload.additional_swaps.push(malicious_swap);

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");
        create_dir_with_file(&victim_live, "keep.txt", "keep");

        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("arbitrary additional live rejected");

        assert!(error.to_string().contains("not an allowed restore target"));
        assert_eq!(
            std::fs::read_to_string(upload_live.join("old.txt")).expect("read upload"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(victim_live.join("keep.txt")).expect("read victim"),
            "keep"
        );
    }

    #[test]
    fn finalize_full_restore_rejects_parent_traversal_in_additional_swap() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let mut payload = full_restore_payload_for_live(&upload_live);
        let mut malicious_swap = tor_swap_for_live(&tor_live);
        malicious_swap.staged = temp_dir
            .path()
            .join(".keys.restore-stage.0123456789abcdef0123456789abcdef")
            .join("..")
            .join("escape")
            .display()
            .to_string();
        payload.additional_swaps.push(malicious_swap);

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");

        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("parent traversal rejected");

        assert!(error.to_string().contains("parent traversal"));
        assert_eq!(
            std::fs::read_to_string(upload_live.join("old.txt")).expect("read upload"),
            "old"
        );
    }

    #[test]
    fn finalize_full_restore_rejects_additional_swap_with_wrong_live_root() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let other_live = temp_dir.path().join("other-keys");
        let mut payload = full_restore_payload_for_live(&upload_live);
        payload
            .additional_swaps
            .push(tor_swap_for_live(&other_live));

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");

        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("wrong additional live root rejected");

        assert!(error.to_string().contains("not an allowed restore target"));
        assert!(std::path::Path::new(&payload.staged).exists());
    }

    #[test]
    fn finalize_full_restore_rejects_additional_staged_or_previous_outside_expected_area() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let tor_live = temp_dir.path().join("keys");
        let outside = temp_dir.path().join("outside");
        std::fs::create_dir_all(&outside).expect("outside dir");
        let mut payload = full_restore_payload_for_live(&upload_live);
        let mut malicious_swap = tor_swap_for_live(&tor_live);
        malicious_swap.staged = outside
            .join(".keys.restore-stage.0123456789abcdef0123456789abcdef")
            .display()
            .to_string();
        payload.additional_swaps.push(malicious_swap);

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");

        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("outside staged path rejected");

        assert!(error
            .to_string()
            .contains("outside the expected restore staging area"));
        assert!(std::path::Path::new(&payload.staged).exists());

        let mut payload = full_restore_payload_for_live(&upload_live);
        let mut malicious_swap = tor_swap_for_live(&tor_live);
        malicious_swap.previous = outside
            .join(".keys.restore-old.0123456789abcdef0123456789abcdef")
            .display()
            .to_string();
        payload.additional_swaps.push(malicious_swap);
        let error = finalize_full_restore_payload(&payload, &upload_live, Some(&tor_live))
            .expect_err("outside previous path rejected");

        assert!(error
            .to_string()
            .contains("outside the expected restore backup area"));
    }

    #[test]
    fn reconcile_rejects_malicious_additional_swap_and_keeps_pending_op() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let victim_live = temp_dir.path().join("victim");
        let mut payload = full_restore_payload_for_live(&upload_live);
        payload
            .additional_swaps
            .push(tor_swap_for_live(&victim_live));

        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");
        create_dir_with_file(&victim_live, "keep.txt", "keep");

        let pool = init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let op = crate::pending_fs::PendingFsOpInsert {
            id: "malicious-full-restore".to_string(),
            kind: FULL_RESTORE_SWAP_KIND,
            payload_json: serde_json::to_string(&payload).expect("payload json"),
        };
        insert_pending_fs_op(&conn, &op).expect("insert pending op");
        drop(conn);

        let error =
            crate::pending_fs::reconcile_pending_fs_ops(&pool, upload_live.to_str().expect("utf8"))
                .expect_err("malicious additional swap rejected");

        assert!(!error.to_string().is_empty());
        let conn = pool.get().expect("db connection");
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pending_fs_ops", [], |row| row
                .get::<_, i64>(0))
                .expect("pending op count"),
            1
        );
        assert_eq!(
            std::fs::read_to_string(upload_live.join("old.txt")).expect("read upload"),
            "old"
        );
        assert_eq!(
            std::fs::read_to_string(victim_live.join("keep.txt")).expect("read victim"),
            "keep"
        );
    }

    #[test]
    fn old_full_restore_payload_without_additional_swaps_deserializes_and_finalizes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_live = temp_dir.path().join("uploads");
        let payload = full_restore_payload_for_live(&upload_live);
        create_dir_with_file(&upload_live, "old.txt", "old");
        create_dir_with_file(std::path::Path::new(&payload.staged), "new.txt", "new");
        let legacy_json = serde_json::json!({
            "staged": payload.staged,
            "live": payload.live,
            "previous": payload.previous,
        });
        let payload: FullRestoreSwapPayload =
            serde_json::from_value(legacy_json).expect("legacy full restore payload");

        finalize_full_restore_payload(&payload, &upload_live, None)
            .expect("legacy full restore payload finalizes");

        assert_eq!(
            std::fs::read_to_string(upload_live.join("new.txt")).expect("read upload"),
            "new"
        );
    }
}
