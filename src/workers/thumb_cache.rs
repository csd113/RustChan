//! Database-aware thumbnail and waveform cache eviction.

use anyhow::{Context as _, Result};
use std::path::PathBuf;
use tracing::warn;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Result of one database-aware thumbnail-cache eviction pass.
pub struct ThumbCacheEvictionReport {
    /// Bytes in all inspected thumbnail files before eviction.
    pub total_before_bytes: u64,
    /// Bytes remaining after all safe candidates were considered.
    pub total_after_bytes: u64,
    /// Unreferenced files removed.
    pub removed_files: u64,
    /// Bytes freed by removed files.
    pub removed_bytes: u64,
}

/// Sum cache bytes before filesystem mutation, rejecting unrepresentable totals.
fn thumbnail_cache_bytes(mut sizes: impl Iterator<Item = u64>) -> Result<u64> {
    sizes.try_fold(0_u64, |total, size| {
        total
            .checked_add(size)
            .context("thumbnail cache size overflow")
    })
}

/// Load every live thumbnail reference before considering cache eviction.
fn referenced_thumbnail_paths(
    conn: &rusqlite::Connection,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT thumb_path
         FROM posts
         WHERE thumb_path IS NOT NULL AND TRIM(thumb_path) != ''",
    )?;
    let referenced = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(referenced)
}

/// Walk every board's `thumbs/` subdirectory and evict unreferenced files.
///
/// Only files inside `{upload_dir}/{board}/thumbs/` are considered — original
/// uploads are never touched.  Deletion is best-effort: individual failures
/// are logged and skipped rather than aborting the whole pass.
///
/// # Errors
/// Returns an error if current thumbnail references cannot be loaded. In that
/// fail-closed case no filesystem deletion is attempted.
pub fn evict_thumb_cache(
    conn: &rusqlite::Connection,
    upload_dir: &str,
    max_bytes: u64,
) -> Result<ThumbCacheEvictionReport> {
    let referenced = referenced_thumbnail_paths(conn)?;

    // Collect (mtime_secs, path, size) for every file inside any thumbs/ dir.
    let mut files: Vec<(u64, PathBuf, u64, bool)> = Vec::new();
    let Ok(upload_root) = std::path::Path::new(upload_dir).canonicalize() else {
        return Ok(ThumbCacheEvictionReport::default());
    };
    let Ok(boards_iter) = std::fs::read_dir(upload_dir) else {
        return Ok(ThumbCacheEvictionReport::default());
    };
    for board_entry in boards_iter.flatten() {
        let Ok(board_metadata) = board_entry.path().symlink_metadata() else {
            continue;
        };
        if !board_metadata.is_dir() || board_metadata.file_type().is_symlink() {
            continue;
        }
        let thumbs_dir = board_entry.path().join("thumbs");
        let Ok(thumbs_metadata) = std::fs::symlink_metadata(&thumbs_dir) else {
            continue;
        };
        if !thumbs_metadata.is_dir() || thumbs_metadata.file_type().is_symlink() {
            continue;
        }
        let Ok(thumbs_iter) = std::fs::read_dir(&thumbs_dir) else {
            continue;
        };
        for entry in thumbs_iter.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.file_type().is_symlink() {
                continue;
            }
            let Ok(canonical_path) = path.canonicalize() else {
                continue;
            };
            if !canonical_path.starts_with(&upload_root) {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let is_referenced = canonical_path
                .strip_prefix(&upload_root)
                .ok()
                .and_then(std::path::Path::to_str)
                .is_some_and(|relative| referenced.contains(relative));
            files.push((mtime, canonical_path, meta.len(), is_referenced));
        }
    }

    // Validate the entire byte total before removing any cache file.
    let total = thumbnail_cache_bytes(files.iter().map(|(_, _, size, _)| *size))?;
    let mut report = ThumbCacheEvictionReport {
        total_before_bytes: total,
        total_after_bytes: total,
        ..ThumbCacheEvictionReport::default()
    };
    if total <= max_bytes {
        return Ok(report);
    }

    // Sort oldest-first so we delete the least-recently-used files first.
    files.sort_unstable_by_key(|(mtime, _, _, _)| *mtime);

    let mut remaining = total;
    let mut deleted = 0u64;
    let mut deleted_bytes = 0u64;
    for (_, path, size, is_referenced) in &files {
        if remaining <= max_bytes {
            break;
        }
        if *is_referenced {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                remaining = remaining.saturating_sub(*size);
                deleted = deleted
                    .checked_add(1)
                    .context("thumbnail eviction count overflow")?;
                // Dereference `size` for clarity.
                deleted_bytes = deleted_bytes
                    .checked_add(*size)
                    .context("thumbnail eviction size overflow")?;
            }
            Err(e) => {
                warn!(
                    target: concat!(env!("CARGO_CRATE_NAME"), "::workers"),
                    "evict_thumb_cache: failed to delete {:?}: {}",
                    path,
                    e
                );
            }
        }
    }
    if deleted > 0 {
        tracing::info!(target: "workers", files_removed = deleted, freed_kib = deleted_bytes / 1024, remaining_kib = remaining / 1024, limit_kib = max_bytes / 1024, "Thumbnail cache eviction complete");
    }
    report.total_after_bytes = remaining;
    report.removed_files = deleted;
    report.removed_bytes = deleted_bytes;
    Ok(report)
}
