use crate::{
    config::CONFIG,
    error::{AppError, Result},
};
use std::io::Seek;
use std::path::{Path, PathBuf};
use tracing::warn;

pub(super) const ZIP_ENTRY_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

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

pub(super) fn swap_staged_path_into_place(
    staged: &Path,
    live: &Path,
    backup: &Path,
) -> Result<bool> {
    let had_live = live.exists();
    if had_live {
        std::fs::rename(live, backup).map_err(|e| {
            AppError::Internal(anyhow::anyhow!(
                "Move live path {} aside: {e}",
                live.display()
            ))
        })?;
    }

    if let Err(e) = std::fs::rename(staged, live) {
        if had_live {
            let _ = std::fs::rename(backup, live);
        }
        return Err(AppError::Internal(anyhow::anyhow!(
            "Move staged path {} into place: {e}",
            live.display()
        )));
    }

    Ok(had_live)
}

pub(super) fn rollback_swapped_path(live: &Path, backup: &Path, had_live: bool) -> Result<()> {
    remove_path_if_exists(live)?;
    if had_live {
        std::fs::rename(backup, live).map_err(|e| {
            AppError::Internal(anyhow::anyhow!(
                "Restore backup path {}: {e}",
                live.display()
            ))
        })?;
    }
    Ok(())
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
