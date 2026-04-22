#![allow(clippy::wildcard_imports)]

use super::*;

const BACKUP_LIST_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct BackupListCacheEntry {
    generated_at: Instant,
    dir_modified: Option<SystemTime>,
    files: Vec<BackupInfo>,
}

static BACKUP_LIST_CACHE: LazyLock<parking_lot::Mutex<HashMap<String, BackupListCacheEntry>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

pub(super) fn latest_saved_board_backup_filename(board_short: &str) -> Option<String> {
    let prefix = format!("rustchan-board-{board_short}-");
    let mut matches = list_backup_files(&board_backup_dir(), BackupListKind::Board)
        .into_iter()
        .filter(|info| info.filename.starts_with(&prefix));
    matches.next().map(|info| info.filename)
}

#[derive(Clone, Copy)]
pub enum BackupListKind {
    Full,
    Board,
}

fn backup_cache_key(dir: &Path, kind: BackupListKind) -> String {
    let kind = match kind {
        BackupListKind::Full => "full",
        BackupListKind::Board => "board",
    };
    format!("{kind}:{}", dir.display())
}

fn current_dir_modified(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}

pub fn invalidate_backup_list_cache(dir: &Path, kind: BackupListKind) {
    BACKUP_LIST_CACHE
        .lock()
        .remove(&backup_cache_key(dir, kind));
}

/// List `.zip` files in `dir`, newest-filename-first.
pub fn list_backup_files(dir: &std::path::Path, kind: BackupListKind) -> Vec<BackupInfo> {
    let cache_key = backup_cache_key(dir, kind);
    let dir_modified = current_dir_modified(dir);
    if let Some(entry) = BACKUP_LIST_CACHE.lock().get(&cache_key).cloned() {
        if entry.generated_at.elapsed() <= BACKUP_LIST_CACHE_TTL
            && entry.dir_modified == dir_modified
        {
            return entry.files;
        }
    }

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
                let modified = modified_epoch
                    .and_then(|secs| {
                        #[allow(deprecated)]
                        chrono::DateTime::<Utc>::from_timestamp(secs, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                    })
                    .unwrap_or_default();
                let (verification, boards) = match kind {
                    BackupListKind::Full => match common::verify_full_backup_zip(&path) {
                        Ok(manifest) => (
                            Ok(format!("verified v{} backup", manifest.version)),
                            manifest.boards,
                        ),
                        Err(error) => (Err(error), Vec::new()),
                    },
                    BackupListKind::Board => match common::verify_board_backup_zip(&path) {
                        Ok(manifest) => (
                            Ok(format!(
                                "verified board /{}/ backup",
                                manifest.board.short_name
                            )),
                            vec![crate::models::BackupBoardSummary {
                                short_name: manifest.board.short_name,
                                name: manifest.board.name,
                            }],
                        ),
                        Err(error) => (Err(error), Vec::new()),
                    },
                };
                files.push(BackupInfo {
                    filename: name,
                    size_bytes: meta.len(),
                    modified,
                    modified_epoch,
                    verified: verification.is_ok(),
                    verification_note: verification.unwrap_or_else(|error| error.to_string()),
                    boards,
                });
            }
        }
    }
    files.sort_by(|a, b| b.filename.cmp(&a.filename));
    BACKUP_LIST_CACHE.lock().insert(
        cache_key,
        BackupListCacheEntry {
            generated_at: Instant::now(),
            dir_modified,
            files: files.clone(),
        },
    );
    files
}

pub(super) fn prune_full_backup_dir_to_limit(dir: &Path, keep_limit: usize) -> Result<Vec<String>> {
    let keep_limit = keep_limit.max(1);
    let mut backups = list_backup_files(dir, BackupListKind::Full);
    if backups.len() <= keep_limit {
        return Ok(Vec::new());
    }

    let to_remove = backups.split_off(keep_limit);
    let mut removed = Vec::with_capacity(to_remove.len());
    for backup in to_remove {
        let path = dir.join(&backup.filename);
        if !path.exists() {
            continue;
        }
        std::fs::remove_file(&path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Delete retained full backup '{}': {error}",
                backup.filename
            ))
        })?;
        removed.push(backup.filename);
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
    let mut candidates = Vec::new();
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("zip") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        candidates.push((modified, path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    for (modified, path) in candidates {
        if common::verify_full_backup_zip(&path).is_ok() {
            return Some(modified);
        }
    }
    None
}

pub(crate) fn latest_verified_full_backup_modified_time() -> Option<SystemTime> {
    latest_verified_full_backup_modified_time_in_dir(&full_backup_dir())
}
