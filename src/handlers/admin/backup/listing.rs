// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use chrono::TimeZone;

const BACKUP_LIST_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct BackupListCacheEntry {
    generated_at: Instant,
    source_modified: Option<SystemTime>,
    files: Vec<BackupInfo>,
}

static BACKUP_LIST_CACHE: LazyLock<parking_lot::Mutex<HashMap<String, BackupListCacheEntry>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

pub(super) fn latest_saved_board_backup_filename(board_short: &str) -> Option<String> {
    list_backup_files(&board_backup_dir(), BackupListKind::Board)
        .into_iter()
        .find(|info| {
            info.boards
                .first()
                .is_some_and(|board| board.short_name == board_short)
        })
        .map(|info| info.backup_ref)
}

#[derive(Clone, Copy)]
pub enum BackupListKind {
    Full,
    Board,
}

fn backup_cache_key(kind: BackupListKind) -> String {
    match kind {
        BackupListKind::Full => "full".to_string(),
        BackupListKind::Board => "board".to_string(),
    }
}

fn current_dir_modified(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}

fn current_source_modified(dir: &Path) -> Option<SystemTime> {
    let mut modified = current_dir_modified(dir);
    let root_modified = current_dir_modified(&v4::backups_root_dir());
    if root_modified > modified {
        modified = root_modified;
    }
    modified
}

pub fn invalidate_backup_list_cache(_dir: &Path, kind: BackupListKind) {
    BACKUP_LIST_CACHE.lock().remove(&backup_cache_key(kind));
}

fn modified_string_from_epoch(epoch: Option<i64>) -> String {
    epoch
        .and_then(|secs| {
            Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        })
        .unwrap_or_default()
}

const fn metadata_scope_matches(kind: BackupListKind, scope: v4::BackupScope) -> bool {
    match kind {
        BackupListKind::Full => matches!(
            scope,
            v4::BackupScope::FullSite
                | v4::BackupScope::SelectedBoards
                | v4::BackupScope::PreMaintenance
        ),
        BackupListKind::Board => matches!(scope, v4::BackupScope::Board),
    }
}

fn scope_label(scope: v4::BackupScope) -> String {
    match scope {
        v4::BackupScope::FullSite => "Full site".to_string(),
        v4::BackupScope::Board => "Board".to_string(),
        v4::BackupScope::SelectedBoards => "Selected boards".to_string(),
        v4::BackupScope::PreMaintenance => "Pre-maintenance".to_string(),
    }
}

fn verify_v4_saved_backup(layout: &v4::SavedBackupLayout) -> Result<v4::VerifiedSavedV4Root> {
    let metadata = v4::load_metadata(&layout.metadata_path)?;
    let expected_scopes: &[v4::BackupScope] = match metadata.scope {
        v4::BackupScope::FullSite => &[v4::BackupScope::FullSite],
        v4::BackupScope::Board => &[v4::BackupScope::Board],
        v4::BackupScope::SelectedBoards => &[v4::BackupScope::SelectedBoards],
        v4::BackupScope::PreMaintenance => &[v4::BackupScope::PreMaintenance],
    };
    v4::verify_saved_v4_root(&layout.root_dir, expected_scopes)
}

fn list_v4_backups(kind: BackupListKind) -> Vec<BackupInfo> {
    let mut backups = Vec::new();
    for layout in v4::iter_saved_backup_layouts() {
        let verified = verify_v4_saved_backup(&layout);
        let (metadata, manifest, modified_epoch, verified_note) = match verified {
            Ok(verified) => {
                let note = format!(
                    "verified Backup v4 {}",
                    verified.metadata.storage_mode.display_name().to_lowercase()
                );
                (
                    verified.metadata,
                    verified.manifest,
                    Some(verified.completed_at),
                    note,
                )
            }
            Err(error) => {
                let Ok(metadata) = v4::load_metadata(&layout.metadata_path) else {
                    continue;
                };
                let Ok(manifest) = v4::load_manifest(&layout.manifest_path) else {
                    continue;
                };
                (
                    metadata.clone(),
                    manifest,
                    metadata
                        .completed_at
                        .filter(|completed_at| *completed_at >= metadata.created_at),
                    error.to_string(),
                )
            }
        };

        if !metadata_scope_matches(kind, metadata.scope) {
            continue;
        }

        backups.push(BackupInfo {
            backup_ref: layout.backup_ref.clone(),
            backup_id: metadata.backup_id.clone(),
            filename: metadata.backup_id.clone(),
            size_bytes: metadata.total_size_bytes,
            modified: modified_string_from_epoch(modified_epoch),
            modified_epoch,
            verified: verified_note.starts_with("verified "),
            verification_note: verified_note,
            scope: scope_label(metadata.scope),
            mode: metadata.storage_mode.display_name().to_string(),
            part_count: metadata.part_count,
            part_filenames: manifest
                .parts
                .iter()
                .map(|part| {
                    part.filename
                        .strip_prefix("parts/")
                        .unwrap_or(&part.filename)
                        .to_string()
                })
                .collect(),
            contains_tor_hidden_service_keys: metadata.includes_tor_keys,
            boards: metadata.included_boards.clone(),
            server_path: layout.root_dir.display().to_string(),
            manifest_path: layout.manifest_path.display().to_string(),
            downloadable_archive: metadata.storage_mode == v4::BackupStorageMode::SingleZip,
        });

        let _ = manifest;
    }
    backups
}

fn list_legacy_zip_backups(dir: &Path, kind: BackupListKind) -> Vec<BackupInfo> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("zip") {
                continue;
            }
            if let (Some(name), Ok(meta)) = (
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(ToString::to_string),
                std::fs::metadata(&path),
            ) {
                let modified_epoch = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs().cast_signed());
                let modified = modified_string_from_epoch(modified_epoch);
                let (verification, boards, contains_tor_hidden_service_keys, scope, mode) =
                    match kind {
                        BackupListKind::Full => match common::verify_full_backup_zip(&path) {
                            Ok(manifest) => (
                                Ok(format!("verified legacy v{} backup", manifest.version)),
                                manifest.boards,
                                manifest.tor_hidden_service_keys_included,
                                "Full site".to_string(),
                                "Legacy ZIP".to_string(),
                            ),
                            Err(error) => (
                                Err(error),
                                Vec::new(),
                                false,
                                "Full site".to_string(),
                                "Legacy ZIP".to_string(),
                            ),
                        },
                        BackupListKind::Board => match common::verify_board_backup_zip(&path) {
                            Ok(manifest) => (
                                Ok(format!(
                                    "verified legacy board /{}/ backup",
                                    manifest.board.short_name
                                )),
                                vec![crate::models::BackupBoardSummary {
                                    short_name: manifest.board.short_name,
                                    name: manifest.board.name,
                                }],
                                false,
                                "Board".to_string(),
                                "Legacy ZIP".to_string(),
                            ),
                            Err(error) => (
                                Err(error),
                                Vec::new(),
                                false,
                                "Board".to_string(),
                                "Legacy ZIP".to_string(),
                            ),
                        },
                    };
                files.push(BackupInfo {
                    backup_ref: name.clone(),
                    backup_id: name.clone(),
                    filename: name,
                    size_bytes: meta.len(),
                    modified,
                    modified_epoch,
                    verified: verification.is_ok(),
                    verification_note: verification.unwrap_or_else(|error| error.to_string()),
                    scope,
                    mode,
                    part_count: 1,
                    part_filenames: Vec::new(),
                    contains_tor_hidden_service_keys,
                    boards,
                    server_path: path.display().to_string(),
                    manifest_path: String::new(),
                    downloadable_archive: true,
                });
            }
        }
    }
    files
}

/// List saved backups for the requested kind, newest-first.
pub fn list_backup_files(dir: &std::path::Path, kind: BackupListKind) -> Vec<BackupInfo> {
    let cache_key = backup_cache_key(kind);
    let source_modified = current_source_modified(dir);
    if let Some(entry) = BACKUP_LIST_CACHE.lock().get(&cache_key).cloned() {
        if entry.generated_at.elapsed() <= BACKUP_LIST_CACHE_TTL
            && entry.source_modified == source_modified
        {
            return entry.files;
        }
    }

    let mut files = list_v4_backups(kind);
    files.extend(list_legacy_zip_backups(dir, kind));
    files.sort_by(|left, right| {
        right
            .modified_epoch
            .cmp(&left.modified_epoch)
            .then_with(|| right.backup_ref.cmp(&left.backup_ref))
    });

    BACKUP_LIST_CACHE.lock().insert(
        cache_key,
        BackupListCacheEntry {
            generated_at: Instant::now(),
            source_modified,
            files: files.clone(),
        },
    );
    files
}

pub(super) fn safe_saved_backup_dir_for_delete(path: &Path) -> Result<()> {
    let backup_root = v4::backups_root_dir();
    crate::utils::fs_security::assert_dir_no_symlink(path).map_err(|error| {
        AppError::BadRequest(format!(
            "Saved backup directory {} is unsafe to delete: {error}",
            path.display()
        ))
    })?;
    let canonical_root = backup_root.canonicalize().map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Canonicalize backup root {}: {error}",
            backup_root.display()
        ))
    })?;
    let canonical_path = crate::utils::fs_security::canonical_child_of(&backup_root, path)
        .map_err(|error| {
            AppError::BadRequest(format!(
                "Saved backup directory {} is outside the backup root: {error}",
                path.display()
            ))
        })?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return Err(AppError::BadRequest(format!(
            "Saved backup directory {} is not a direct child of the backup root.",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn prune_full_backup_dir_to_limit(dir: &Path, keep_limit: usize) -> Result<Vec<String>> {
    let keep_limit = keep_limit.max(1);
    let mut backups = list_backup_files(dir, BackupListKind::Full)
        .into_iter()
        .filter(|backup| backup.scope == "Full site" || backup.scope == "Selected boards")
        .collect::<Vec<_>>();
    if backups.len() <= keep_limit {
        return Ok(Vec::new());
    }

    let to_remove = backups.split_off(keep_limit);
    let mut removed = Vec::with_capacity(to_remove.len());
    for backup in to_remove {
        let path = PathBuf::from(&backup.server_path);
        if !path.exists() {
            continue;
        }
        if path.is_dir() {
            safe_saved_backup_dir_for_delete(&path)?;
            std::fs::remove_dir_all(&path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Delete retained saved backup '{}': {error}",
                    backup.backup_ref
                ))
            })?;
        } else {
            std::fs::remove_file(&path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Delete retained full backup '{}': {error}",
                    backup.backup_ref
                ))
            })?;
        }
        removed.push(backup.backup_ref);
    }

    if !removed.is_empty() {
        invalidate_backup_list_cache(dir, BackupListKind::Full);
    }

    Ok(removed)
}

pub(crate) fn enforce_full_backup_retention(copies_to_keep: u64) -> Result<Vec<String>> {
    prune_full_backup_dir_to_limit(&full_backup_dir(), copies_to_keep.max(1) as usize)
}

pub(super) fn latest_verified_full_backup_modified_time_in_dir(dir: &Path) -> Option<SystemTime> {
    let mut latest = None;
    let backups = if dir == full_backup_dir().as_path() {
        list_backup_files(dir, BackupListKind::Full)
    } else {
        list_legacy_zip_backups(dir, BackupListKind::Full)
    };
    for backup in backups {
        if !backup.verified {
            continue;
        }
        let candidate = backup.modified_epoch.and_then(|epoch| {
            std::time::UNIX_EPOCH.checked_add(Duration::from_secs(epoch.cast_unsigned()))
        })?;
        if latest.is_none_or(|current| candidate > current) {
            latest = Some(candidate);
        }
    }
    latest
}

pub(crate) fn latest_verified_full_backup_modified_time() -> Option<SystemTime> {
    latest_verified_full_backup_modified_time_in_dir(&full_backup_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_saved_backup_dir_for_delete_rejects_paths_outside_backup_root() {
        let backup_root = v4::backups_root_dir();
        std::fs::create_dir_all(&backup_root).expect("backup root");
        let data_dir = backup_root.parent().expect("backup root has parent");
        let outside = tempfile::Builder::new()
            .prefix("outside-backup-root-")
            .tempdir_in(data_dir)
            .expect("outside tempdir");
        let error = safe_saved_backup_dir_for_delete(outside.path())
            .expect_err("outside path should be rejected");
        assert!(error.to_string().contains("outside the backup root"));
    }
}
