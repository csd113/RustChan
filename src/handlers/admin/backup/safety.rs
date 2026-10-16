//! Backup integrity checks, bounded restore I/O, and shared safety primitives.

use crate::{
    error::{AppError, Result},
    middleware::{backup_phase, BackupProgress},
    models::BackupBoardSummary,
};
use serde::{Deserialize, Serialize};
use std::io::Seek;
use std::path::{Path, PathBuf};

/// ZIP entry max bytes used by this handler.
pub(super) const ZIP_ENTRY_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Board manifest max bytes used by this handler.
pub(super) const BOARD_MANIFEST_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Banner restore entry max bytes used by this handler.
pub(super) const BANNER_RESTORE_ENTRY_MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Banner restore total max bytes used by this handler.
pub(super) const BANNER_RESTORE_TOTAL_MAX_BYTES: u64 = 64 * 1024 * 1024;
// Keep restore writes within an application-level disk budget instead of relying
// only on the router's coarse 20 GiB body cap.
/// Restore upload max bytes used by this handler.
pub(super) const RESTORE_UPLOAD_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Restore total extracted max bytes used by this handler.
pub(super) const RESTORE_TOTAL_EXTRACTED_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Full backup manifest name used by this handler.
pub(super) const FULL_BACKUP_MANIFEST_NAME: &str = "backup.json";
/// Full backup Tor keys prefix used by this handler.
pub(super) const FULL_BACKUP_TOR_KEYS_PREFIX: &str = "tor/keys";
/// Full backup Tor keys entry prefix used by this handler.
pub(super) const FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX: &str = "tor/keys/";
/// `SQLite` header used by this handler.
const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TorHiddenServiceKeysAvailability {
    Skipped,
    Available(PathBuf),
}

pub(super) fn resolve_tor_hidden_service_keys_availability(
    requested: bool,
    configured_dir: Option<PathBuf>,
    unavailable_message: &str,
) -> Result<TorHiddenServiceKeysAvailability> {
    if !requested {
        return Ok(TorHiddenServiceKeysAvailability::Skipped);
    }

    let Some(dir) = configured_dir else {
        return Err(AppError::BadRequest(unavailable_message.to_owned()));
    };

    std::fs::read_dir(&dir)
        .map_err(|error| {
            AppError::BadRequest(format!(
            "{unavailable_message} The configured identity directory {} could not be read: {error}",
            dir.display()
        ))
        })
        .map(|_completed_value| ())?;

    Ok(TorHiddenServiceKeysAvailability::Available(dir))
}

pub(super) fn resolve_tor_hidden_service_keys_restore_target(
    requested: bool,
    configured_dir: Option<PathBuf>,
    unavailable_message: &str,
) -> Result<TorHiddenServiceKeysAvailability> {
    if !requested {
        return Ok(TorHiddenServiceKeysAvailability::Skipped);
    }

    let Some(dir) = configured_dir else {
        return Err(AppError::BadRequest(unavailable_message.to_owned()));
    };

    if let Ok(metadata) = std::fs::symlink_metadata(&dir) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(AppError::BadRequest(format!(
                "{unavailable_message} The configured identity path {} is not a directory.",
                dir.display()
            )));
        }
    }

    Ok(TorHiddenServiceKeysAvailability::Available(dir))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Manifest data for full backup.
pub(super) struct FullBackupManifest {
    pub version: u32,
    pub generated_at: i64,
    pub rustchan_version: String,
    pub db_bytes: u64,
    pub upload_file_count: u64,
    pub favicon_file_count: u64,
    #[serde(default)]
    pub banner_file_count: u64,
    #[serde(default)]
    pub tor_hidden_service_keys_included: bool,
    #[serde(default)]
    pub tor_hidden_service_key_file_count: u64,
    #[serde(default)]
    pub boards: Vec<BackupBoardSummary>,
}

pub(super) fn log_backup_phase(phase: u64) {
    let message = match phase {
        backup_phase::SNAPSHOT_DB => "Backup progress - snapshotting database",
        backup_phase::COUNT_FILES => "Backup progress - counting files",
        backup_phase::COMPRESS => "Backup progress - compressing files",
        backup_phase::DONE => "Backup progress - done",
        _ => return,
    };
    tracing::info!(target: "admin", "{message}");
}

pub(super) fn log_backup_progress(progress: &BackupProgress) {
    use std::sync::atomic::Ordering::Relaxed;

    let phase = progress.phase.load(Relaxed);
    if phase != backup_phase::COMPRESS {
        return;
    }

    let done = progress.files_done.load(Relaxed);
    let total = progress.files_total.load(Relaxed);
    if total == 0 || done == 0 {
        return;
    }

    let Some(percent) = done.saturating_mul(100).checked_div(total) else {
        return;
    };
    let prev_done = done.saturating_sub(1);
    let Some(prev_percent) = prev_done.saturating_mul(100).checked_div(total) else {
        return;
    };
    let should_log = total <= 50
        || done == 1
        || done == total
        || done.is_multiple_of(25)
        || (percent != prev_percent && percent.is_multiple_of(10));

    if should_log {
        tracing::info!(
            target: "admin",
            "Backup progress - compressing files: {done}/{total} ({percent}%)"
        );
    }
}

/// Validates board short name.
pub(super) fn validate_board_short_name(short_name: &str) -> Result<()> {
    let valid = !short_name.is_empty()
        && short_name.len() <= 8
        && short_name.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "Invalid board short name in backup manifest.".into(),
        ))
    }
}

fn validated_media_upload_relative_path(path: &str, context: &str) -> Result<Vec<String>> {
    validate_restore_safe_entry_name(path)?;
    let components = path.split('/').map(str::to_owned).collect::<Vec<_>>();
    if components.len() < 2 {
        return Err(AppError::BadRequest(format!(
            "{context} must include a board directory and file name."
        )));
    }
    Ok(components)
}

/// Validates restored media path.
pub(super) fn validate_restored_media_path(path: &str, context: &str) -> Result<String> {
    let components = validated_media_upload_relative_path(path, context)?;
    let board_short = components
        .first()
        .ok_or_else(|| AppError::BadRequest(format!("{context} is missing a board directory.")))?;
    validate_board_short_name(board_short)?;
    Ok(board_short.clone())
}

/// Validates restored media path for board.
pub(super) fn validate_restored_media_path_for_board(
    path: &str,
    expected_board_short: &str,
    context: &str,
) -> Result<()> {
    let board_short = validate_restored_media_path(path, context)?;
    if board_short != expected_board_short {
        return Err(AppError::BadRequest(format!(
            "{context} must stay within /{expected_board_short}/ uploads."
        )));
    }
    Ok(())
}

/// Remaps numeric references.
fn remap_numeric_references(body: &str, prefix: &str, pairs: &[(String, String)]) -> String {
    let mut result = body.to_owned();
    for (old, new) in pairs {
        let needle = format!("{prefix}{old}");
        let mut out = String::with_capacity(result.len());
        let mut pos = 0;
        let bytes = result.as_bytes();
        while pos < bytes.len() {
            let Some(remaining) = result.get(pos..) else {
                break;
            };
            match remaining.find(&needle) {
                None => {
                    out.push_str(remaining);
                    break;
                }
                Some(rel) => {
                    let abs = pos.saturating_add(rel);
                    let after = abs.saturating_add(needle.len());
                    let next_is_digit = bytes.get(after).is_some_and(u8::is_ascii_digit);
                    let Some(before_match) = remaining.get(..rel) else {
                        break;
                    };
                    out.push_str(before_match);
                    if next_is_digit {
                        out.push_str(&needle);
                    } else {
                        out.push_str(prefix);
                        out.push_str(new);
                    }
                    pos = after;
                }
            }
        }
        result = out;
    }
    result
}

/// Remaps body quotelinks.
pub(super) fn remap_body_quotelinks(
    body: &str,
    board_short: &str,
    pairs: &[(String, String)],
) -> String {
    if pairs.is_empty() {
        return body.to_owned();
    }

    let result = remap_numeric_references(body, ">>", pairs);
    let crosslink_prefix = format!(">>>/{board_short}/");
    remap_numeric_references(&result, &crosslink_prefix, pairs)
}

pub(super) fn render_restored_body_html(body: &str) -> String {
    let escaped = crate::utils::sanitize::escape_html(body);
    crate::utils::sanitize::render_post_body(&escaped, false)
}

/// Copies limited.
pub(super) fn copy_limited<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 65_536];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let n_u64 = u64::try_from(n).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Read size does not fit the byte counter: {error}"),
            )
        })?;
        total = total.checked_add(n_u64).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Decompressed byte count overflowed",
            )
        })?;
        if total > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Decompressed entry exceeds {} MiB limit — possible zip bomb",
                    max_bytes / 1024 / 1024
                ),
            ));
        }
        if let Some(slice) = buf.get(..n) {
            writer.write_all(slice)?;
        }
    }
    Ok(total)
}

/// Copies limited with total budget.
pub(super) fn copy_limited_with_total_budget<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
    total_written: &mut u64,
    total_budget: u64,
    label: &str,
) -> std::io::Result<u64> {
    let copied = copy_limited(reader, writer, max_bytes)?;
    *total_written = total_written.saturating_add(copied);
    if *total_written > total_budget {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{label} exceeds the {} MiB extracted restore budget",
                total_budget / 1024 / 1024
            ),
        ));
    }
    Ok(copied)
}

pub(super) fn create_staging_dir(base_path: &Path, label: &str) -> Result<PathBuf> {
    let parent = base_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let file_name = base_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(label);
    let staging = parent.join(format!(
        ".{file_name}.{label}.{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&staging)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create staging dir: {e}")))?;
    Ok(staging)
}

/// Reads limited bytes.
pub(super) fn read_limited_bytes<R: std::io::Read>(
    reader: &mut R,
    max_bytes: u64,
    label: &str,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    copy_limited(reader, &mut bytes, max_bytes)
        .map_err(|error| AppError::BadRequest(format!("{label} exceeds safe size limit: {error}")))
        .map(|_completed_value| ())?;
    Ok(bytes)
}

/// Removes path if exists.
pub(super) fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Remove dir {}: {e}", path.display())))
    } else {
        std::fs::remove_file(path)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Remove file {}: {e}", path.display())))
    }
}

/// Extracts uploads to dir.
pub(super) fn extract_uploads_to_dir<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    destination_root: &Path,
) -> Result<()> {
    let mut extracted_bytes = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{i}]: {e}")))?;
        let name = entry
            .name()
            .map_err(|error| AppError::BadRequest(format!("Invalid ZIP entry name: {error}")))?
            .into_owned();
        let Some(rel_path) = validate_entry_path_under_prefix(&name, "uploads/")? else {
            continue;
        };
        let target = destination_root.join(&rel_path);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("mkdir {}: {e}", target.display()))
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("mkdir parent {}: {e}", parent.display()))
            })?;
        }
        let mut out = std::fs::File::create(&target)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Create {}: {e}", target.display())))?;
        copy_limited_with_total_budget(
            &mut entry,
            &mut out,
            ZIP_ENTRY_MAX_BYTES,
            &mut extracted_bytes,
            RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
            "Restored uploads",
        )
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write {}: {e}", target.display())))
        .map(|_completed_value| ())?;
    }
    Ok(())
}

/// Scan ZIP names without decoding file contents, rejecting invalid encodings.
pub(super) fn zip_contains_entry<R: std::io::Read + Seek>(
    archive: &zip::ZipArchive<R>,
    expected: &str,
) -> Result<bool> {
    archive.file_names().try_fold(false, |found, name| {
        let name =
            name.map_err(|error| AppError::BadRequest(format!("Invalid ZIP entry name: {error}")))?;
        Ok(found || name == expected)
    })
}

/// Validates restore safe entry name.
pub(super) fn validate_restore_safe_entry_name(name: &str) -> Result<()> {
    let normalized = name.trim_end_matches('/');
    let suspicious = name.is_empty()
        || normalized.is_empty()
        || name.contains('\0')
        || name.contains('\\')
        || name.contains(':')
        || name.starts_with('/')
        || normalized
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");

    if suspicious {
        return Err(AppError::BadRequest(format!(
            "Backup contains suspicious path '{name}'"
        )));
    }
    for component in Path::new(name).components() {
        match component {
            std::path::Component::Normal(_) => {}
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir
            | std::path::Component::ParentDir => {
                return Err(AppError::BadRequest(format!(
                    "Backup contains suspicious path '{name}'"
                )));
            }
        }
    }
    Ok(())
}

/// Validates an archive entry and returns its relative path beneath the prefix.
pub(super) fn validate_entry_path_under_prefix(
    name: &str,
    prefix: &str,
) -> Result<Option<PathBuf>> {
    validate_restore_safe_entry_name(name)?;
    let Some(rel) = name.strip_prefix(prefix) else {
        return Ok(None);
    };
    let rel = rel.trim_end_matches('/');
    if rel.is_empty() {
        return Ok(None);
    }
    Ok(Some(rel.split('/').collect()))
}

/// Verifies full backup archive.
pub(super) fn verify_full_backup_archive<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<FullBackupManifest> {
    let manifest = read_full_backup_manifest_from_archive(archive)?;

    let mut db_entry = archive.by_name("chan.db").map_err(|_error| {
        AppError::BadRequest("Invalid full backup: zip must contain 'chan.db' at the root.".into())
    })?;
    let mut header = [0u8; 16];
    std::io::Read::read_exact(&mut db_entry, &mut header).map_err(|error| {
        AppError::BadRequest(format!("Invalid full backup database entry: {error}"))
    })?;
    if header.as_slice() != SQLITE_HEADER {
        return Err(AppError::BadRequest(
            "Invalid full backup: chan.db does not look like a SQLite database.".into(),
        ));
    }
    if db_entry.size() != manifest.db_bytes {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: manifest database size {} does not match archive size {}.",
            manifest.db_bytes,
            db_entry.size()
        )));
    }
    drop(db_entry);
    verify_full_backup_db_schema(archive, manifest.db_bytes)?;

    let mut upload_file_count = 0u64;
    let mut favicon_file_count = 0u64;
    let mut banner_file_count = 0u64;
    let mut tor_hidden_service_key_file_count = 0u64;
    for idx in 0..archive.len() {
        let entry = archive.by_index(idx).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read backup entry #{idx}: {error}"))
        })?;
        let name = entry
            .name()
            .map_err(|error| AppError::BadRequest(format!("Invalid ZIP entry name: {error}")))?
            .into_owned();
        validate_restore_safe_entry_name(&name)?;
        if entry.is_dir() {
            continue;
        }
        if name.starts_with("uploads/") {
            upload_file_count = upload_file_count.saturating_add(1);
        } else if name.starts_with("favicon/") {
            favicon_file_count = favicon_file_count.saturating_add(1);
        } else if name.starts_with("banner/") {
            banner_file_count = banner_file_count.saturating_add(1);
        } else if name.starts_with(FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX) {
            tor_hidden_service_key_file_count = tor_hidden_service_key_file_count.saturating_add(1);
        }
    }

    if upload_file_count != manifest.upload_file_count {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: manifest upload count {} does not match archive count {}.",
            manifest.upload_file_count, upload_file_count
        )));
    }
    if favicon_file_count != manifest.favicon_file_count {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: manifest favicon count {} does not match archive count {}.",
            manifest.favicon_file_count, favicon_file_count
        )));
    }
    if banner_file_count != manifest.banner_file_count {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: manifest banner count {} does not match archive count {}.",
            manifest.banner_file_count, banner_file_count
        )));
    }
    if tor_hidden_service_key_file_count != manifest.tor_hidden_service_key_file_count {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: manifest Tor key count {} does not match archive count {}.",
            manifest.tor_hidden_service_key_file_count, tor_hidden_service_key_file_count
        )));
    }
    if manifest.tor_hidden_service_keys_included != (tor_hidden_service_key_file_count > 0) {
        return Err(AppError::BadRequest(
            "Invalid full backup: manifest Tor key metadata does not match archive contents."
                .into(),
        ));
    }

    Ok(manifest)
}

/// Verifies full backup database schema.
fn verify_full_backup_db_schema<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    expected_db_bytes: u64,
) -> Result<()> {
    if expected_db_bytes > ZIP_ENTRY_MAX_BYTES {
        return Err(AppError::BadRequest(
            "Invalid full backup: chan.db exceeds the restore entry size limit.".into(),
        ));
    }
    let mut db_entry = archive.by_name("chan.db").map_err(|_error| {
        AppError::BadRequest("Invalid full backup: zip must contain 'chan.db' at the root.".into())
    })?;
    let mut temp_db = tempfile::NamedTempFile::new().map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Create temporary database validation file: {error}"
        ))
    })?;
    let copied = std::io::copy(&mut db_entry, temp_db.as_file_mut()).map_err(|error| {
        AppError::BadRequest(format!("Invalid full backup database entry: {error}"))
    })?;
    if copied != expected_db_bytes {
        return Err(AppError::BadRequest(format!(
            "Invalid full backup: copied database size {copied} does not match manifest size {expected_db_bytes}."
        )));
    }
    let conn = rusqlite::Connection::open(temp_db.path()).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid full backup: chan.db could not be opened as SQLite: {error}"
        ))
    })?;
    crate::db::normalize_database_schema_version(&conn).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid full backup: chan.db does not match the RustChan {} database baseline: {error}",
            crate::db::baseline_schema_version()
        ))
    })?;
    Ok(())
}

/// Verifies full backup ZIP.
pub(super) fn verify_full_backup_zip(path: &Path) -> Result<FullBackupManifest> {
    let file = std::fs::File::open(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Open backup {}: {error}", path.display()))
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| AppError::BadRequest(format!("Invalid zip backup: {error}")))?;
    verify_full_backup_archive(&mut archive)
}

/// Reads full backup manifest from archive.
pub(super) fn read_full_backup_manifest_from_archive<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<FullBackupManifest> {
    let mut entry = archive
        .by_name(FULL_BACKUP_MANIFEST_NAME)
        .map_err(|_error| {
            AppError::BadRequest(format!(
                "Invalid full backup: missing {FULL_BACKUP_MANIFEST_NAME}"
            ))
        })?;
    let bytes = read_limited_bytes(
        &mut entry,
        BOARD_MANIFEST_MAX_BYTES,
        FULL_BACKUP_MANIFEST_NAME,
    )?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid full backup manifest {FULL_BACKUP_MANIFEST_NAME}: {error}"
        ))
    })
}

/// Verifies board backup ZIP.
pub(super) fn verify_board_backup_zip(
    path: &Path,
) -> Result<super::board_manifest::BoardBackupManifest> {
    let file = std::fs::File::open(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Open backup {}: {error}", path.display()))
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| AppError::BadRequest(format!("Invalid zip backup: {error}")))?;
    let manifest = super::parse_board_backup_manifest_from_zip(&mut archive)?;
    validate_board_short_name(&manifest.board.short_name)?;
    for idx in 0..archive.len() {
        let entry = archive.by_index(idx).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read backup entry #{idx}: {error}"))
        })?;
        let name = entry
            .name()
            .map_err(|error| AppError::BadRequest(format!("Invalid ZIP entry name: {error}")))?;
        validate_restore_safe_entry_name(&name)?;
    }
    Ok(manifest)
}
