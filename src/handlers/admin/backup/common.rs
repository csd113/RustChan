// src/handlers/admin/backup/common.rs

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
/// Variants supported by the Tor hidden service keys availability workflow.
pub(super) enum TorHiddenServiceKeysAvailability {
    /// Represents the skipped case.
    Skipped,
    /// Represents the available case.
    Available(PathBuf),
}

/// Resolves Tor hidden service keys availability.
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

    std::fs::read_dir(&dir).map_err(|error| {
        AppError::BadRequest(format!(
            "{unavailable_message} The configured identity directory {} could not be read: {error}",
            dir.display()
        ))
    })?;

    Ok(TorHiddenServiceKeysAvailability::Available(dir))
}

/// Resolves Tor hidden service keys restore target.
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
    /// The version.
    pub version: u32,
    /// The generated timestamp.
    pub generated_at: i64,
    /// The rustchan version.
    pub rustchan_version: String,
    /// The database size in bytes.
    pub db_bytes: u64,
    /// The number of upload files.
    pub upload_file_count: u64,
    /// The number of favicon files.
    pub favicon_file_count: u64,
    #[serde(default)]
    /// The number of banner files.
    pub banner_file_count: u64,
    #[serde(default)]
    /// Whether the Tor hidden service keys included setting is active.
    pub tor_hidden_service_keys_included: bool,
    #[serde(default)]
    /// The number of Tor hidden service key files.
    pub tor_hidden_service_key_file_count: u64,
    #[serde(default)]
    /// The boards collection.
    pub boards: Vec<BackupBoardSummary>,
}

/// Performs the log backup phase handler operation.
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

/// Performs the log backup progress handler operation.
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

    let percent = done.saturating_mul(100) / total.max(1);
    let prev_done = done.saturating_sub(1);
    let prev_percent = prev_done.saturating_mul(100) / total.max(1);
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

/// Performs the validated media upload relative path handler operation.
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
                    let abs = pos + rel;
                    let after = abs + needle.len();
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

/// Renders restored body HTML.
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

/// Creates staging dir.
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
    copy_limited(reader, &mut bytes, max_bytes).map_err(|error| {
        AppError::BadRequest(format!("{label} exceeds safe size limit: {error}"))
    })?;
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
        let name = entry.name().to_owned();
        let Some(rel_path) = restore_safe_relative_path_under_prefix(&name, "uploads/")? else {
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
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write {}: {e}", target.display())))?;
    }
    Ok(())
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
            _ => {
                return Err(AppError::BadRequest(format!(
                    "Backup contains suspicious path '{name}'"
                )));
            }
        }
    }
    Ok(())
}

/// Restores safe relative path under prefix.
pub(super) fn restore_safe_relative_path_under_prefix(
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
        let name = entry.name().to_owned();
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
) -> Result<super::types::board_backup_types::BoardBackupManifest> {
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
        validate_restore_safe_entry_name(entry.name())?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::{
        copy_limited_with_total_budget, extract_uploads_to_dir, remap_body_quotelinks,
        validate_board_short_name, validate_restore_safe_entry_name, validate_restored_media_path,
        validate_restored_media_path_for_board, verify_board_backup_zip, verify_full_backup_zip,
        FullBackupManifest, FULL_BACKUP_MANIFEST_NAME,
    };
    use anyhow::{ensure, Context as _, Result};
    use serde_json::json;
    use std::io::Write as _;
    use std::path::Path;

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) -> Result<()> {
        let file = std::fs::File::create(path).context("create ZIP file")?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            zip.start_file(name, options)
                .with_context(|| format!("start ZIP entry {name}"))?;
            zip.write_all(bytes)
                .with_context(|| format!("write ZIP entry {name}"))?;
        }
        zip.finish().context("finish ZIP archive")?;
        Ok(())
    }

    fn partial_sqlite_db_bytes_for_test() -> Result<Vec<u8>> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let db_path = temp_dir.path().join("partial.sqlite3");
        let conn = rusqlite::Connection::open(&db_path).context("open SQLite database")?;
        conn.execute("CREATE TABLE boards (id INTEGER PRIMARY KEY)", [])
            .context("create partial boards table")?;
        drop(conn);
        std::fs::read(db_path).context("read partial SQLite database")
    }

    #[test]
    fn validate_board_short_name_rejects_path_traversal() {
        assert!(validate_board_short_name("test").is_ok());
        assert!(validate_board_short_name("../bad").is_err());
        assert!(validate_board_short_name("waytoolong").is_err());
    }

    #[test]
    fn remap_body_quotelinks_updates_same_board_crosslinks() {
        let remapped = remap_body_quotelinks(
            "reply >>12 and >>>/tech/12 but not >>>/b/12",
            "tech",
            &[("12".into(), "77".into())],
        );

        assert!(remapped.contains(">>77"));
        assert!(remapped.contains(">>>/tech/77"));
        assert!(remapped.contains(">>>/b/12"));
    }

    #[test]
    fn remap_body_quotelinks_preserves_longer_numeric_suffixes() {
        let remapped = remap_body_quotelinks(
            ">>12 >>123 >>>/tech/12 >>>/tech/123",
            "tech",
            &[("12".into(), "88".into())],
        );

        assert!(remapped.contains(">>88"));
        assert!(remapped.contains(">>123"));
        assert!(remapped.contains(">>>/tech/88"));
        assert!(remapped.contains(">>>/tech/123"));
    }

    #[test]
    fn extract_uploads_to_dir_rejects_suspicious_entries() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("uploads.zip");
        {
            let file = std::fs::File::create(&zip_path).context("create ZIP file")?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("uploads/test/ok.txt", options)
                .context("start valid ZIP entry")?;
            std::io::Write::write_all(&mut zip, b"ok").context("write valid ZIP entry")?;
            zip.start_file("uploads/../../escape.txt", options)
                .context("start suspicious ZIP entry")?;
            std::io::Write::write_all(&mut zip, b"bad").context("write suspicious ZIP entry")?;
            zip.finish().context("finish ZIP archive")?;
        }

        let file = std::fs::File::open(&zip_path).context("open ZIP file")?;
        let mut archive = zip::ZipArchive::new(file).context("parse ZIP archive")?;
        let dest = temp_dir.path().join("dest");
        std::fs::create_dir_all(&dest).context("create extraction destination")?;

        let error = extract_uploads_to_dir(&mut archive, &dest)
            .err()
            .context("suspicious ZIP traversal was unexpectedly accepted")?;

        ensure!(dest.join("test/ok.txt").exists());
        ensure!(!dest.join("escape.txt").exists());
        ensure!(error.to_string().contains("suspicious path"));
        Ok(())
    }

    #[test]
    fn restore_extraction_budget_rejects_archive_that_exceeds_total_limit() -> Result<()> {
        let mut reader = std::io::Cursor::new(vec![b'x'; 6]);
        let mut writer = Vec::new();
        let mut total_written = 0;

        let error = copy_limited_with_total_budget(
            &mut reader,
            &mut writer,
            16,
            &mut total_written,
            5,
            "Test restore archive",
        )
        .err()
        .context("oversized extracted archive was unexpectedly accepted")?;

        ensure!(error.to_string().contains("extracted restore budget"));
        Ok(())
    }

    #[test]
    fn extract_uploads_to_dir_accepts_valid_archive_within_budget() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("uploads-ok.zip");
        {
            let file = std::fs::File::create(&zip_path).context("create ZIP file")?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("uploads/test/ok.txt", options)
                .context("start valid ZIP entry")?;
            std::io::Write::write_all(&mut zip, b"ok").context("write valid ZIP entry")?;
            zip.finish().context("finish ZIP archive")?;
        }

        let file = std::fs::File::open(&zip_path).context("open ZIP file")?;
        let mut archive = zip::ZipArchive::new(file).context("parse ZIP archive")?;
        let dest = temp_dir.path().join("dest");
        std::fs::create_dir_all(&dest).context("create extraction destination")?;

        extract_uploads_to_dir(&mut archive, &dest)?;
        let extracted =
            std::fs::read(dest.join("test/ok.txt")).context("read extracted valid file")?;

        ensure!(extracted == b"ok");
        Ok(())
    }

    #[test]
    fn restore_entry_validation_rejects_platform_specific_traversal() {
        for name in [
            "../escape.txt",
            "uploads/../escape.txt",
            "uploads\\board\\file.txt",
            "C:/Windows/system.ini",
            "uploads/C:/Windows/system.ini",
            "uploads//board/file.txt",
            "uploads/./board/file.txt",
            "/uploads/board/file.txt",
            "\\uploads\\board\\file.txt",
        ] {
            assert!(
                validate_restore_safe_entry_name(name).is_err(),
                "accepted suspicious path {name:?}"
            );
        }

        assert!(validate_restore_safe_entry_name("uploads/board/file.txt").is_ok());
    }

    #[test]
    fn restored_media_path_validation_requires_safe_board_scoped_paths() -> Result<()> {
        ensure!(validate_restored_media_path("tech/thumbs/doc.svg", "test media path")? == "tech");
        ensure!(
            validate_restored_media_path("../tech/doc.pdf", "test media path").is_err(),
            "parent traversal must be rejected"
        );
        ensure!(
            validate_restored_media_path("tech", "test media path").is_err(),
            "board-only path must be rejected"
        );
        ensure!(
            validate_restored_media_path_for_board("b/doc.pdf", "tech", "board restore path")
                .is_err(),
            "cross-board media path must be rejected for board restores"
        );
        Ok(())
    }

    #[test]
    fn verify_full_backup_zip_accepts_manifest_backed_archive() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("full.zip");
        let db_bytes = super::super::saved_backup::valid_db_snapshot_for_test()?;
        let manifest = FullBackupManifest {
            version: 1,
            generated_at: 1_700_000_000,
            rustchan_version: "1.1.3".into(),
            db_bytes: u64::try_from(db_bytes.len()).context("convert database size")?,
            upload_file_count: 1,
            favicon_file_count: 1,
            banner_file_count: 0,
            tor_hidden_service_keys_included: false,
            tor_hidden_service_key_file_count: 0,
            boards: vec![crate::models::BackupBoardSummary {
                short_name: "b".into(),
                name: "Random".into(),
            }],
        };
        let manifest_json = serde_json::to_vec(&manifest).context("serialize manifest")?;
        write_zip(
            &zip_path,
            &[
                (FULL_BACKUP_MANIFEST_NAME, &manifest_json),
                ("chan.db", db_bytes.as_slice()),
                ("uploads/b/test.webp", b"img"),
                ("favicon/favicon-32x32.png", b"icon"),
            ],
        )?;

        let verified = verify_full_backup_zip(&zip_path)?;
        ensure!(verified.upload_file_count == 1);
        ensure!(verified.favicon_file_count == 1);
        ensure!(verified.boards.len() == 1);
        Ok(())
    }

    #[test]
    fn verify_full_backup_zip_rejects_structurally_invalid_database() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("invalid-db.zip");
        let db_bytes = partial_sqlite_db_bytes_for_test()?;
        let manifest = FullBackupManifest {
            version: 3,
            generated_at: 1_700_000_000,
            rustchan_version: "1.4.0".into(),
            db_bytes: u64::try_from(db_bytes.len()).context("convert database size")?,
            upload_file_count: 0,
            favicon_file_count: 0,
            banner_file_count: 0,
            tor_hidden_service_keys_included: false,
            tor_hidden_service_key_file_count: 0,
            boards: Vec::new(),
        };
        let manifest_json = serde_json::to_vec(&manifest).context("serialize manifest")?;
        write_zip(
            &zip_path,
            &[
                (FULL_BACKUP_MANIFEST_NAME, &manifest_json),
                ("chan.db", db_bytes.as_slice()),
            ],
        )?;

        let error = verify_full_backup_zip(&zip_path)
            .err()
            .context("structurally invalid database was unexpectedly accepted")?;
        ensure!(
            error
                .to_string()
                .contains("does not match the RustChan 1.4.0 database baseline"),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    #[test]
    fn verify_full_backup_zip_rejects_missing_manifest() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("full.zip");
        write_zip(&zip_path, &[("chan.db", b"SQLite format 3\0rest of db")])?;

        let error = verify_full_backup_zip(&zip_path)
            .err()
            .context("backup without manifest was unexpectedly accepted")?;

        ensure!(error.to_string().contains("missing backup.json"));
        Ok(())
    }

    #[test]
    fn verify_full_backup_zip_defaults_legacy_tor_metadata_to_not_included() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("legacy-full.zip");
        let db_bytes = super::super::saved_backup::valid_db_snapshot_for_test()?;
        let manifest = json!({
            "version": 2,
            "generated_at": 1_700_000_000_i64,
            "rustchan_version": "1.1.3",
            "db_bytes": u64::try_from(db_bytes.len()).context("convert database size")?,
            "upload_file_count": 1_u64,
            "favicon_file_count": 0_u64,
            "banner_file_count": 0_u64,
            "boards": []
        });
        let manifest_bytes = serde_json::to_vec(&manifest).context("serialize manifest")?;
        write_zip(
            &zip_path,
            &[
                (FULL_BACKUP_MANIFEST_NAME, &manifest_bytes),
                ("chan.db", db_bytes.as_slice()),
                ("uploads/tech/file.txt", b"ok"),
            ],
        )?;

        let verified = verify_full_backup_zip(&zip_path)?;
        ensure!(!verified.tor_hidden_service_keys_included);
        ensure!(verified.tor_hidden_service_key_file_count == 0);
        Ok(())
    }

    #[test]
    fn verify_full_backup_zip_rejects_tor_manifest_mismatch() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("tor-mismatch.zip");
        let db_bytes = super::super::saved_backup::valid_db_snapshot_for_test()?;
        let manifest = FullBackupManifest {
            version: 3,
            generated_at: 1_700_000_000,
            rustchan_version: "1.1.3".into(),
            db_bytes: u64::try_from(db_bytes.len()).context("convert database size")?,
            upload_file_count: 0,
            favicon_file_count: 0,
            banner_file_count: 0,
            tor_hidden_service_keys_included: true,
            tor_hidden_service_key_file_count: 1,
            boards: Vec::new(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).context("serialize manifest")?;
        write_zip(
            &zip_path,
            &[
                (FULL_BACKUP_MANIFEST_NAME, &manifest_bytes),
                ("chan.db", db_bytes.as_slice()),
            ],
        )?;

        let error = verify_full_backup_zip(&zip_path)
            .err()
            .context("mismatched Tor metadata was unexpectedly accepted")?;
        ensure!(error.to_string().contains("manifest Tor key count 1"));
        Ok(())
    }

    #[test]
    fn verify_board_backup_zip_rejects_suspicious_entries() -> Result<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("board.zip");
        let manifest = json!({
            "version": 1,
            "board": {
                "id": 1,
                "short_name": "b",
                "name": "Random",
                "description": "",
                "nsfw": false,
                "max_threads": 100,
                "max_archived_threads": 150,
                "bump_limit": 300,
                "allow_images": true,
                "allow_video": true,
                "allow_audio": true,
                "allow_any_files": false,
                "allow_tripcodes": true,
                "edit_window_secs": 300,
                "allow_editing": true,
                "allow_archive": true,
                "allow_video_embeds": true,
                "allow_captcha": false,
                "show_poster_ids": false,
                "collapse_greentext": true,
                "post_cooldown_secs": 0,
                "created_at": 1_700_000_000
            },
            "threads": [],
            "posts": [],
            "polls": [],
            "poll_options": [],
            "poll_votes": [],
            "file_hashes": []
        });
        let manifest_json = serde_json::to_vec(&manifest).context("serialize board manifest")?;
        write_zip(
            &zip_path,
            &[
                ("board.json", &manifest_json),
                ("uploads/../../escape.txt", b"bad"),
            ],
        )?;

        ensure!(verify_board_backup_zip(&zip_path).is_err());
        Ok(())
    }
}
