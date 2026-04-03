// src/handlers/admin/backup/common.rs

use crate::{
    config::CONFIG,
    error::{AppError, Result},
    middleware::{backup_phase, BackupProgress},
};
use std::io::Seek;
use std::path::{Path, PathBuf};
use tracing::warn;

pub(super) const ZIP_ENTRY_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub(super) const BOARD_MANIFEST_MAX_BYTES: u64 = 64 * 1024 * 1024;

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

#[allow(clippy::arithmetic_side_effects)]
pub(super) fn remap_body_quotelinks(body: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return body.to_string();
    }

    let mut result = body.to_string();
    for (old, new) in pairs {
        let needle = format!(">>{old}");
        let mut out = String::with_capacity(result.len());
        let mut pos = 0;
        let bytes = result.as_bytes();
        while pos < bytes.len() {
            match result[pos..].find(&needle) {
                None => {
                    out.push_str(&result[pos..]);
                    break;
                }
                Some(rel) => {
                    let abs = pos + rel;
                    let after = abs + needle.len();
                    let next_is_digit = bytes.get(after).is_some_and(u8::is_ascii_digit);
                    out.push_str(&result[pos..abs]);
                    if next_is_digit {
                        out.push_str(&needle);
                    } else {
                        out.push_str(">>");
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

pub(super) fn render_restored_body_html(body: &str) -> String {
    let escaped = crate::utils::sanitize::escape_html(body);
    crate::utils::sanitize::render_post_body(&escaped)
}

#[allow(clippy::arithmetic_side_effects)]
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
        total += n as u64;
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

pub(super) fn extract_uploads_to_dir<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    destination_root: &Path,
) -> Result<()> {
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{i}]: {e}")))?;
        let name = entry.name().to_string();
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            warn!("Restore: skipping suspicious entry '{name}'");
            continue;
        }
        let Some(rel) = name.strip_prefix("uploads/") else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let rel_path = Path::new(rel);
        if rel_path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            warn!("Restore: skipping suspicious entry '{name}'");
            continue;
        }
        let target = destination_root.join(rel_path);
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
        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write {}: {e}", target.display())))?;
    }
    Ok(())
}

pub(super) fn db_dir() -> PathBuf {
    PathBuf::from(&CONFIG.database_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::{extract_uploads_to_dir, validate_board_short_name};

    #[test]
    fn validate_board_short_name_rejects_path_traversal() {
        assert!(validate_board_short_name("test").is_ok());
        assert!(validate_board_short_name("../bad").is_err());
        assert!(validate_board_short_name("waytoolong").is_err());
    }

    #[test]
    fn extract_uploads_to_dir_skips_suspicious_entries() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("uploads.zip");
        {
            let file = std::fs::File::create(&zip_path).expect("zip file");
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("uploads/test/ok.txt", options)
                .expect("start valid file");
            std::io::Write::write_all(&mut zip, b"ok").expect("write valid file");
            zip.start_file("uploads/../../escape.txt", options)
                .expect("start invalid file");
            std::io::Write::write_all(&mut zip, b"bad").expect("write invalid file");
            zip.finish().expect("finish zip");
        }

        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let dest = temp_dir.path().join("dest");
        std::fs::create_dir_all(&dest).expect("dest dir");

        extract_uploads_to_dir(&mut archive, &dest).expect("extract uploads");

        assert!(dest.join("test/ok.txt").exists());
        assert!(!dest.join("escape.txt").exists());
    }
}
