// Backup and restore subsystem for the admin panel.
// Covers full-site backups, board-level backups, streaming downloads,
// saved-backup restoration, and live board.json restore.

use crate::{
    banner,
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::AppState,
    models::{BackupInfo, BoardAccessMode},
    utils::crypto::{new_session_id, verify_password},
};
use axum::{
    extract::{Form, Multipart, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{Local, Utc};
use futures::stream::Stream;
use rusqlite::{backup::Backup, params};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::io::ReaderStream;

use super::{
    admin_panel_redirect_anchor_open, check_admin_csrf_jar, require_admin_post_origin_and_csrf,
    require_admin_session_sid, require_same_origin_request, should_set_secure_cookie,
    AdminPanelTarget, ADMIN_COOKIE_SAME_SITE, SESSION_COOKIE,
};

mod archive;
mod common;
mod create;
mod downloads;
mod http;
mod listing;
mod restore_board;
mod restore_full;
mod saved_backup;
mod types;
pub(crate) use saved_backup::BackupStorageMode;

use common::{
    copy_limited, create_staging_dir, extract_uploads_to_dir, log_backup_phase,
    log_backup_progress, read_limited_bytes, remap_body_quotelinks, remove_path_if_exists,
    render_restored_body_html, restore_safe_relative_path_under_prefix, validate_board_short_name,
    validate_restore_safe_entry_name, verify_full_backup_archive, BANNER_RESTORE_ENTRY_MAX_BYTES,
    BANNER_RESTORE_TOTAL_MAX_BYTES, BOARD_MANIFEST_MAX_BYTES, ZIP_ENTRY_MAX_BYTES,
};
pub(crate) use create::*;
pub(crate) use downloads::{
    backup_progress_json, delete_backup, download_backup, write_temp_board_download_token,
};
pub(crate) use http::backup_request_logging_middleware;
pub(crate) use listing::{invalidate_backup_list_cache, list_backup_files, BackupListKind};
pub(crate) use restore_board::{
    board_restore, extract_board_from_full_backup, restore_saved_board_backup,
};
pub(crate) use restore_full::{admin_restore, restore_saved_full_backup};
use types::board_backup_types;

/// Full backup restore section used by this handler.
const FULL_BACKUP_RESTORE_SECTION: &str = "full-backup-restore";
/// Board backup restore section used by this handler.
const BOARD_BACKUP_RESTORE_SECTION: &str = "board-backup-restore";
/// `SQLite` header used by this handler.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Deserialize)]
pub(crate) struct RestoreSavedForm {
    filename: String,
    #[serde(default, deserialize_with = "form_checkbox_bool")]
    restore_tor_hidden_service_keys: bool,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

fn form_checkbox_bool<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(form_checkbox_value_is_on(value.as_deref()))
}

fn form_checkbox_value_is_on(value: Option<&str>) -> bool {
    value == Some("1")
        || value.is_some_and(|item| item.eq_ignore_ascii_case("on"))
        || value.is_some_and(|item| item.eq_ignore_ascii_case("true"))
}

use archive::{
    canonicalize_restored_banner_dir, create_temp_board_backup_from_full_backup_path,
    create_temp_legacy_board_backup_from_saved_full_v4_path,
    create_temp_legacy_board_backup_from_v4_path, create_temp_legacy_full_backup_from_v4_path,
    create_temp_legacy_full_backup_from_v4_transfer_zip, parse_board_backup_manifest_from_zip,
    validate_full_restore_archive_layout,
};
use downloads::prune_stale_temp_board_downloads;
#[cfg(test)]
use downloads::{consume_temp_board_download_token, temp_board_download_token_path};
#[cfg(test)]
use http::admin_xhr_error_response;
use http::{
    is_xml_http_request, log_restore_upload_started, redirect_page_response,
    restore_auth_preflight, restore_error_redirect_target, restore_failure_response,
    restore_start_response, restore_success_redirect_target, restore_upload_parse_response,
    sanitize_backup_zip_filename, sanitize_board_short_value, sanitize_saved_backup_ref,
    stream_restore_upload_to_tempfile, validate_streamed_restore_upload, RestoreKind,
};
use listing::latest_saved_board_backup_filename as latest_board_backup_filename;
pub(crate) use listing::{
    enforce_full_backup_retention, latest_verified_full_backup_modified_time,
};
#[cfg(test)]
use listing::{latest_verified_full_backup_modified_time_in_dir, prune_full_backup_dir_to_limit};
#[cfg(test)]
use restore_board::execute_board_restore;
#[cfg(test)]
use restore_full::refresh_live_site_state_from_db;
use restore_full::restore_db_from_snapshot;

#[expect(
    clippy::too_many_lines,
    reason = "database snapshotting, archive creation, and temporary-file cleanup form one operation"
)]
pub(crate) async fn admin_backup(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Full backup download")?;
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let upload_dir = CONFIG.upload_dir.clone();
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
    let global_banner_dir = banner::backup_source_dir();
    let progress = std::sync::Arc::clone(&state.backup_progress);

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            let uploads_base = Path::new(&upload_dir);

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);

            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().simple().to_string();
            let temp_db = temp_dir.join(format!("chan_backup_{tmp_id}.db"));
            let temp_db_str = temp_db
                .to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path is non-UTF-8")))?
                .replace('\'', "''");

            conn.execute_batch(&format!("VACUUM INTO '{temp_db_str}'"))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("VACUUM INTO failed: {e}")))?;
            crate::config::restrict_private_file_permissions(&temp_db).map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Set private permissions on {}: {error}",
                    temp_db.display()
                ))
            })?;

            // Count files for progress bar before compressing.
            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            log_backup_phase(crate::middleware::backup_phase::COUNT_FILES);
            let favicon_file_count = count_files_in_dir(&global_favicon_dir);
            let banner_file_count = count_files_in_dir(&global_banner_dir);
            let file_count = count_files_in_dir(uploads_base)
                .saturating_add(favicon_file_count)
                .saturating_add(banner_file_count);
            let db_snapshot_size = std::fs::metadata(&temp_db)
                .map(|metadata| metadata.len())
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Stat DB snapshot: {e}")))?;
            let manifest = build_full_backup_manifest(
                &conn,
                db_snapshot_size,
                file_count
                    .saturating_sub(favicon_file_count)
                    .saturating_sub(banner_file_count),
                favicon_file_count,
                banner_file_count,
                false,
                0,
            )?;
            drop(conn);
            // +2 for backup.json and chan.db
            progress
                .files_total
                .store(file_count.saturating_add(2), Ordering::Relaxed);

            // MEM-FIX: write zip directly to a NamedTempFile instead of Vec<u8>.
            let zip_tmp = tempfile::NamedTempFile::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp zip: {e}")))?;
            let build_result = (|| -> Result<()> {
                let out_file =
                    std::io::BufWriter::new(zip_tmp.as_file().try_clone().map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Clone temp file handle: {e}"))
                    })?);
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                progress.reset(crate::middleware::backup_phase::COMPRESS);
                log_backup_phase(crate::middleware::backup_phase::COMPRESS);
                progress
                    .files_total
                    .store(file_count.saturating_add(2), Ordering::Relaxed);

                let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|e| {
                    AppError::Internal(anyhow::anyhow!("Serialize full backup manifest: {e}"))
                })?;
                zip.start_file(common::FULL_BACKUP_MANIFEST_NAME, opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip backup manifest: {e}")))?;
                zip.write_all(&manifest_json).map_err(|e| {
                    AppError::Internal(anyhow::anyhow!("Write backup manifest: {e}"))
                })?;

                // Database snapshot (streamed, not read into RAM)
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB entry: {e}")))?;
                let mut db_src = std::fs::File::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {e}")))?;
                let copied = std::io::copy(&mut db_src, &mut zip)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {e}")))?;
                drop(db_src);
                drop(std::fs::remove_file(&temp_db));
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
                log_backup_progress(&progress);

                // Upload files (streamed file-by-file via io::copy)
                if uploads_base.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
                }
                if global_favicon_dir.exists() {
                    add_dir_to_zip_with_prefix(
                        &mut zip,
                        &global_favicon_dir,
                        &global_favicon_dir,
                        "favicon",
                        opts,
                        &progress,
                    )?;
                }
                if global_banner_dir.exists() {
                    add_dir_to_zip_with_prefix(
                        &mut zip,
                        &global_banner_dir,
                        &global_banner_dir,
                        "banner",
                        opts,
                        &progress,
                    )?;
                }

                // Flush the BufWriter explicitly so I/O errors are not
                // silently swallowed by the implicit Drop-flush.
                let writer = zip
                    .finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {e}")))?;
                writer
                    .into_inner()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush zip writer: {e}")))?
                    .sync_all()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Sync zip file: {e}")))?;
                Ok(())
            })();

            if let Err(error) = build_result {
                drop(std::fs::remove_file(&temp_db));
                return Err(error);
            }

            if let Err(error) = common::verify_full_backup_zip(zip_tmp.path()) {
                drop(std::fs::remove_file(&temp_db));
                return Err(error);
            }

            let file_size = zip_tmp
                .as_file()
                .metadata()
                .map_or(0, |metadata| metadata.len());

            // Persist the temp file (prevents auto-delete on drop).
            // We delete it manually in the background after serving.
            let (_, tmp_path_obj) = zip_tmp.into_parts();
            let final_path = tmp_path_obj
                .keep()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Persist temp zip: {e}")))?;

            let ts = local_backup_timestamp_label();
            let fname = format!("rustchan-backup-{ts}.zip");
            tracing::info!(target: "admin", bytes = file_size, "Full backup downloaded");
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            log_backup_phase(crate::middleware::backup_phase::DONE);
            Ok((final_path, fname, file_size))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // MEM-FIX: Stream the zip file from disk in chunks — never load it all into heap.
    let file = tokio::fs::File::open(&tmp_path)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open backup for streaming: {e}")))?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    // Schedule temp-file cleanup after a generous window so even slow clients finish.
    let cleanup_path = tmp_path;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_mins(10)).await;
        drop(tokio::fs::remove_file(cleanup_path).await);
    });

    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

/// Count regular files (not directories) under `dir` recursively.
/// Used to initialise the progress bar's `files_total` before compression starts.
fn count_files_in_dir(dir: &Path) -> u64 {
    if crate::utils::fs_security::assert_dir_no_symlink(dir).is_err() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().fold(0u64, |acc, entry| {
        let p = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&p) else {
            return acc;
        };
        if metadata.file_type().is_symlink() {
            acc
        } else if metadata.file_type().is_dir() {
            acc + count_files_in_dir(&p)
        } else if metadata.file_type().is_file()
            && crate::utils::fs_security::assert_regular_file_no_symlink(&p).is_ok()
        {
            acc + 1
        } else {
            acc
        }
    })
}

/// Recursively add every file under `dir` into the zip as `uploads/{rel_path}`.
///
/// MEM-FIX: Uses `std::io::copy` with the zip writer directly, streaming each
/// file through a kernel buffer (~8 KiB) instead of reading the whole file
/// into a Vec<u8> first.  Peak RAM per file = `io::copy`'s 8 KiB stack buffer.
///
/// Progress tracking: increments `progress.files_done` and `progress.bytes_done`
/// after each file is written to the zip.
fn add_dir_to_zip<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    base: &Path,
    dir: &Path,
    opts: zip::write::SimpleFileOptions,
    progress: &crate::middleware::BackupProgress,
) -> Result<()> {
    add_dir_to_zip_with_prefix(zip, base, dir, "uploads", opts, progress)
}

pub(super) fn add_dir_to_zip_with_prefix<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    base: &Path,
    dir: &Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
    progress: &crate::middleware::BackupProgress,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("read_dir {}: {}", dir.display(), e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| AppError::Internal(anyhow::anyhow!("dir entry: {e}")))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("inspect {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            tracing::warn!(path = %path.display(), "skipping symlink during backup traversal");
            continue;
        }

        let relative = path
            .strip_prefix(base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("strip_prefix: {e}")))?;
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        let zip_path = format!("{prefix}/{rel_str}");

        if metadata.file_type().is_dir() {
            zip.add_directory(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip dir: {e}")))?;
            add_dir_to_zip_with_prefix(zip, base, &path, prefix, opts, progress)?;
        } else if metadata.file_type().is_file() {
            if crate::utils::fs_security::assert_regular_file_no_symlink(&path).is_err() {
                tracing::warn!(path = %path.display(), "skipping unsafe runtime file during backup");
                continue;
            }
            // MEM-FIX: open file, stream through io::copy — no Vec<u8> allocation.
            let mut src = std::fs::File::open(&path).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("open {}: {}", path.display(), e))
            })?;
            zip.start_file(&zip_path, zip_file_options_for_path(&path))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip file entry: {e}")))?;
            let copied = std::io::copy(&mut src, zip).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("copy {} to zip: {}", path.display(), e))
            })?;
            progress.files_done.fetch_add(1, Ordering::Relaxed);
            progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
            log_backup_progress(progress);
        }
    }
    Ok(())
}

fn zip_file_options_for_path(path: &Path) -> zip::write::SimpleFileOptions {
    let method = if should_store_without_recompress(path) {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    zip::write::SimpleFileOptions::default().compression_method(method)
}

fn should_store_without_recompress(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "7z" | "aac"
                    | "avif"
                    | "bz2"
                    | "flac"
                    | "gif"
                    | "gz"
                    | "heic"
                    | "heif"
                    | "jpeg"
                    | "jpg"
                    | "m4a"
                    | "m4v"
                    | "mkv"
                    | "mov"
                    | "mp3"
                    | "mp4"
                    | "ogg"
                    | "opus"
                    | "png"
                    | "rar"
                    | "tbz"
                    | "tbz2"
                    | "tgz"
                    | "wav"
                    | "webm"
                    | "webp"
                    | "xz"
                    | "zip"
                    | "zst"
            )
        })
}

/// rustchan-data/backups/full/
pub(crate) fn full_backup_dir() -> PathBuf {
    crate::config::full_backups_dir()
}

/// rustchan-data/backups/boards/
pub(crate) fn board_backup_dir() -> PathBuf {
    crate::config::board_backups_dir()
}

pub(super) fn local_backup_timestamp_label() -> String {
    Local::now().format("%Y%m%d_%H%M%S").to_string()
}

pub(crate) fn unique_backup_filename(dir: &Path, base_name: &str) -> String {
    let candidate = dir.join(base_name);
    if !candidate.exists() {
        return base_name.to_owned();
    }

    let stem = Path::new(base_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("backup");
    let ext = Path::new(base_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("zip");

    loop {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let candidate_name = format!("{stem}-{suffix}.{ext}");
        if !dir.join(&candidate_name).exists() {
            return candidate_name;
        }
    }
}

/// rustchan-data/runtime/tmp/board-downloads/
pub(crate) fn temp_board_download_dir() -> PathBuf {
    crate::config::runtime_temp_board_downloads_dir()
}

// Board-level backup / restore
#[derive(Deserialize)]
pub(crate) struct BoardBackupDownloadQuery {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Stream a board-level backup zip: manifest JSON + that board's upload files.
///
/// MEM-FIX: Same approach as `admin_backup` — build zip into a `NamedTempFile` on
/// disk, then stream the result in 64 KiB chunks.
pub(crate) async fn board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<BoardBackupDownloadQuery>,
    axum::extract::Path(board_short): axum::extract::Path<String>,
) -> Result<Response> {
    check_admin_csrf_jar(&jar, query.csrf.as_deref())?;

    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let safe_board = board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if safe_board.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let filename = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let safe_board = safe_board.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            conn.query_row(
                "SELECT 1 FROM boards WHERE short_name = ?1",
                params![safe_board],
                |_| Ok(()),
            )
            .map_err(|_error| AppError::NotFound(format!("Board '{safe_board}' not found")))?;

            latest_board_backup_filename(&safe_board).ok_or_else(|| {
                AppError::NotFound(format!(
                    "No saved backup found for /{safe_board}/. Create one from the admin panel first."
                ))
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let v4_root = crate::config::backups_dir().join(&filename);
    if v4_root.is_dir() {
        let (temp_zip, temp_name) =
            create_temp_legacy_board_backup_from_v4_path(&v4_root, Some(&safe_board))?;
        let mut temp_zip_guard = archive::TempZipCleanupGuard::new(temp_zip.clone());
        let download_token = new_session_id();
        write_temp_board_download_token(&temp_name, &download_token)?;
        let download_dir = temp_board_download_dir();
        crate::config::ensure_private_dir(&download_dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Create temp board download dir {}: {error}",
                download_dir.display()
            ))
        })?;
        let final_path = download_dir.join(&temp_name);
        std::fs::rename(&temp_zip, &final_path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Move temp board backup {}: {error}",
                final_path.display()
            ))
        })?;
        crate::config::restrict_private_file_permissions(&final_path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Set private permissions on {}: {error}",
                final_path.display()
            ))
        })?;
        temp_zip_guard.disarm();
        return Ok(Redirect::to(&format!(
            "/admin/backup/download/temp-board/{temp_name}?cleanup=1&token={download_token}"
        ))
        .into_response());
    }

    Ok(Redirect::to(&format!("/admin/backup/download/board/{filename}")).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        build_board_backup_manifest, consume_temp_board_download_token,
        create_temp_board_backup_from_full_backup_path, execute_board_restore, full_backup_dir,
        invalidate_backup_list_cache, latest_verified_full_backup_modified_time,
        latest_verified_full_backup_modified_time_in_dir, refresh_live_site_state_from_db,
        render_restored_body_html, should_store_without_recompress, temp_board_download_dir,
        temp_board_download_token_path, validate_full_restore_archive_layout,
        write_temp_board_download_token, BackupListKind, RestoreKind,
    };
    use crate::error::AppError;
    use crate::models::BackupBoardSummary;
    use anyhow::{bail, ensure, Context as _, Result as TestResult};
    use axum::{
        body::{to_bytes, Body},
        extract::Form,
        http::{header, HeaderMap, HeaderValue, Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use axum_extra::extract::cookie::{Cookie, CookieJar};
    use rusqlite::params;
    use std::io::{Cursor, Write as _};
    use std::path::{Path, PathBuf};
    use tower::ServiceExt as _;

    fn zip_with_entries(entries: &[(&str, &[u8])]) -> TestResult<zip::ZipArchive<Cursor<Vec<u8>>>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            for (name, body) in entries {
                writer
                    .start_file(*name, options)
                    .with_context(|| format!("start ZIP entry {name}"))?;
                writer
                    .write_all(body)
                    .with_context(|| format!("write ZIP entry {name}"))?;
            }
            writer.finish().context("finish ZIP archive")?;
        }
        cursor.set_position(0);
        zip::ZipArchive::new(cursor).context("parse ZIP archive")
    }

    #[test]
    fn local_backup_timestamp_label_is_filename_safe_and_sortable() {
        let label = super::local_backup_timestamp_label();

        assert_eq!(label.len(), "YYYYMMDD_HHMMSS".len());
        assert_eq!(label.as_bytes().get(8), Some(&b'_'));
        assert!(label
            .chars()
            .enumerate()
            .all(|(idx, ch)| idx == 8 && ch == '_' || ch.is_ascii_digit()));
    }

    async fn echo_restore_saved_form(Form(form): Form<super::RestoreSavedForm>) -> String {
        form.restore_tor_hidden_service_keys.to_string()
    }

    #[tokio::test]
    async fn restore_saved_form_accepts_checked_browser_checkbox_value() -> TestResult<()> {
        let app = Router::new().route("/parse", post(echo_restore_saved_form));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded;charset=UTF-8",
                    )
                    .body(Body::from(
                        "_csrf=test&filename=backup.zip&restore_tor_hidden_service_keys=1",
                    ))
                    .context("build restore form request")?,
            )
            .await
            .context("send restore form request")?;

        ensure!(response.status() == StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read response body")?;
        ensure!(&body[..] == b"true");
        Ok(())
    }

    fn sample_post(board_id: i64, thread_id: i64, body: &str, is_op: bool) -> crate::db::NewPost {
        crate::db::NewPost {
            thread_id,
            board_id,
            name: "anon".into(),
            tripcode: None,
            subject: None,
            body: body.into(),
            body_html: render_restored_body_html(body),
            ip_hash: Some("hash".into()),
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".into(),
            is_op,
        }
    }

    struct PathCleanup(PathBuf);

    impl Drop for PathCleanup {
        fn drop(&mut self) {
            match std::fs::metadata(&self.0) {
                Ok(metadata) if metadata.is_dir() => {
                    drop(std::fs::remove_dir_all(&self.0));
                }
                Ok(_) => {
                    drop(std::fs::remove_file(&self.0));
                }
                Err(_) => {}
            }
        }
    }

    fn install_admin_session(state: &crate::middleware::AppState) -> TestResult<()> {
        let conn = state.db.get().context("get database connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("hunter2").context("hash admin password")?;
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).context("create test admin")?;
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .context("create test admin session")?;
        Ok(())
    }

    fn admin_signed_csrf() -> String {
        crate::utils::crypto::make_scoped_csrf_form_token(
            "csrf123",
            &crate::config::CONFIG.cookie_secret,
            "session123",
        )
    }

    fn admin_cookie_jar() -> CookieJar {
        CookieJar::new()
            .add(Cookie::new("csrf_token", "csrf123"))
            .add(Cookie::new(super::super::SESSION_COOKIE, "session123"))
    }

    fn unique_zip_name(prefix: &str) -> String {
        format!("{prefix}-{}.zip", uuid::Uuid::new_v4().simple())
    }

    fn admin_form_post(uri: &str, body: String) -> TestResult<Request<Body>> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::HOST, "localhost")
            .header(header::ORIGIN, "http://localhost")
            .header(
                header::COOKIE,
                "csrf_token=csrf123; chan_admin_session=session123",
            )
            .extension(crate::test_support::connect_info())
            .body(Body::from(body))
            .context("build admin form request")
    }

    async fn response_body_string(response: axum::response::Response) -> TestResult<String> {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read response body")?;
        String::from_utf8(body.to_vec()).context("decode UTF-8 response body")
    }

    #[tokio::test]
    async fn board_backup_get_requires_admin_csrf() -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let suffix = unique.get(..7).unwrap_or(&unique);
        let board_short = format!("b{suffix}");
        {
            let conn = state.db.get().context("get database connection")?;
            crate::db::create_board(&conn, &board_short, "Board", "", false)
                .context("create board")?;
        }
        let app = Router::new()
            .route("/admin/board/backup/{board}", get(super::board_backup))
            .with_state(state);
        let cookie = "csrf_token=csrf123; chan_admin_session=session123";

        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/admin/board/backup/{board_short}"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .context("build rejected board-backup request")?,
            )
            .await
            .context("send rejected board-backup request")?;

        ensure!(rejected.status() == StatusCode::FORBIDDEN);

        let accepted = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/admin/board/backup/{board_short}?_csrf={}",
                        admin_signed_csrf()
                    ))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .context("build accepted board-backup request")?,
            )
            .await
            .context("send accepted board-backup request")?;

        ensure!(accepted.status() == StatusCode::NOT_FOUND);
        Ok(())
    }

    fn extract_location_query_param(location: &str, key: &str) -> Option<String> {
        let (_, query) = location.split_once('?')?;
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then(|| value.to_owned())
        })
    }

    #[tokio::test]
    async fn admin_xhr_bad_request_returns_handled_json_error() -> TestResult<()> {
        let response = super::admin_xhr_error_response(&AppError::BadRequest("bad restore".into()));

        ensure!(response.status() == StatusCode::OK);
        ensure!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok())
                == Some(StatusCode::BAD_REQUEST.as_str())
        );

        let body = response_body_string(response).await?;
        ensure!(body.contains("bad restore"));
        Ok(())
    }

    #[tokio::test]
    async fn restore_upload_parse_xhr_returns_handled_json_error() -> TestResult<()> {
        let response = super::restore_upload_parse_response(
            RestoreKind::Full,
            true,
            &"missing multipart field",
        );

        ensure!(response.status() == StatusCode::OK);
        ensure!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok())
                == Some(StatusCode::BAD_REQUEST.as_str())
        );

        let body = response_body_string(response).await?;
        ensure!(body.contains("Upload parsing failed"));
        ensure!(body.contains("missing multipart field"));
        Ok(())
    }

    #[test]
    fn full_restore_success_redirect_target_reopens_full_backup_section() {
        let target = super::restore_success_redirect_target(RestoreKind::Full, None);

        assert_eq!(
            target,
            "/admin/panel?restored=1&open=full-backup-restore#full-backup-restore"
        );
    }

    #[test]
    fn board_restore_success_redirect_target_keeps_board_anchor() {
        let target = super::restore_success_redirect_target(RestoreKind::Board, Some("tech"));

        assert_eq!(
            target,
            "/admin/panel?flash=Board+%2Ftech%2F+restored.&open=board-backup-restore#board-backup-tech"
        );
    }

    #[tokio::test]
    async fn saved_backup_delete_is_blocked_during_active_maintenance_without_mutation(
    ) -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let backup_ref = format!("delete-gate-{}", uuid::Uuid::new_v4().simple());
        let backup_root = crate::config::backups_dir().join(&backup_ref);
        let sentinel_path = backup_root.join("sentinel");
        let _backup_cleanup = PathCleanup(backup_root.clone());
        std::fs::create_dir_all(&backup_root).context("create saved backup")?;
        std::fs::write(&sentinel_path, b"protected backup").context("write backup sentinel")?;

        let app = Router::new()
            .route("/admin/backup/delete", post(super::delete_backup))
            .with_state(state.clone());
        let form_body = format!(
            "_csrf={}&kind=full&filename={backup_ref}",
            admin_signed_csrf()
        );

        let active_guard = state
            .maintenance_gate
            .try_begin("Full backup creation")
            .context("begin maintenance")?;
        let blocked = app
            .clone()
            .oneshot(admin_form_post("/admin/backup/delete", form_body.clone())?)
            .await
            .context("send blocked delete request")?;

        ensure!(blocked.status() == StatusCode::CONFLICT);
        ensure!(response_body_string(blocked)
            .await?
            .contains("already running"));
        let sentinel = std::fs::read(&sentinel_path).context("read protected backup sentinel")?;
        ensure!(sentinel == b"protected backup");

        drop(active_guard);
        let allowed = app
            .oneshot(admin_form_post("/admin/backup/delete", form_body)?)
            .await
            .context("send allowed delete request")?;

        ensure!(allowed.status() == StatusCode::SEE_OTHER);
        ensure!(!backup_root.exists());
        Ok(())
    }

    #[tokio::test]
    async fn saved_restore_routes_are_blocked_during_active_maintenance_without_mutation(
    ) -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let suffix = unique.get(..7).unwrap_or(&unique);
        let marker_board = format!("m{suffix}");
        {
            let conn = state.db.get().context("get database connection")?;
            crate::db::create_board(&conn, &marker_board, "Maintenance Marker", "", false)
                .context("create marker board")?;
        }

        std::fs::create_dir_all(full_backup_dir()).context("create full backup directory")?;
        std::fs::create_dir_all(super::board_backup_dir())
            .context("create board backup directory")?;
        let full_filename = unique_zip_name("saved-full-restore-gate");
        let board_filename = unique_zip_name("saved-board-restore-gate");
        let full_backup_path = full_backup_dir().join(&full_filename);
        let board_backup_path = super::board_backup_dir().join(&board_filename);
        let _full_backup_cleanup = PathCleanup(full_backup_path.clone());
        let _board_backup_cleanup = PathCleanup(board_backup_path.clone());
        std::fs::write(&full_backup_path, b"full backup marker")
            .context("write full backup marker")?;
        std::fs::write(&board_backup_path, b"board backup marker")
            .context("write board backup marker")?;

        let app = Router::new()
            .route(
                "/admin/backup/restore-saved",
                post(super::restore_saved_full_backup),
            )
            .route(
                "/admin/board/backup/restore-saved",
                post(super::restore_saved_board_backup),
            )
            .with_state(state.clone());
        let full_form_body = format!("_csrf={}&filename={full_filename}", admin_signed_csrf());
        let board_form_body = format!("_csrf={}&filename={board_filename}", admin_signed_csrf());

        let active_guard = state
            .maintenance_gate
            .try_begin("Full backup creation")
            .context("begin maintenance")?;
        for (route, form_body) in [
            ("/admin/backup/restore-saved", full_form_body.clone()),
            ("/admin/board/backup/restore-saved", board_form_body.clone()),
        ] {
            let blocked = app
                .clone()
                .oneshot(admin_form_post(route, form_body)?)
                .await
                .context("send blocked restore request")?;
            ensure!(blocked.status() == StatusCode::CONFLICT);
            ensure!(response_body_string(blocked)
                .await?
                .contains("already running"));
        }

        let full_marker = std::fs::read(&full_backup_path).context("read full backup marker")?;
        ensure!(full_marker == b"full backup marker");
        let board_marker = std::fs::read(&board_backup_path).context("read board backup marker")?;
        ensure!(board_marker == b"board backup marker");
        {
            let conn = state.db.get().context("get database connection")?;
            let marker_name: String = conn
                .query_row(
                    "SELECT name FROM boards WHERE short_name = ?1",
                    [&marker_board],
                    |row| row.get(0),
                )
                .context("query marker board")?;
            ensure!(marker_name == "Maintenance Marker");
        }

        drop(active_guard);
        for (route, form_body) in [
            ("/admin/backup/restore-saved", full_form_body),
            ("/admin/board/backup/restore-saved", board_form_body),
        ] {
            let control = app
                .clone()
                .oneshot(admin_form_post(route, form_body)?)
                .await
                .context("send ungated restore request")?;
            ensure!(control.status() == StatusCode::SEE_OTHER);
            let location = control
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("restore response has no valid redirect location")?;
            ensure!(location.contains("/admin/panel?restore_error="));
            ensure!(location.contains("Invalid+zip"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn extracted_board_restore_is_blocked_during_active_maintenance_without_mutation(
    ) -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let suffix = unique.get(..7).unwrap_or(&unique);
        let board_short = format!("e{suffix}");
        std::fs::create_dir_all(full_backup_dir()).context("create full backup directory")?;
        let filename = unique_zip_name("extract-board-restore-gate");
        let backup_path = full_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        write_sample_full_backup_zip_for_board_at(&backup_path, true, &board_short)?;
        let backup_before = std::fs::read(&backup_path).context("read original full backup")?;

        let upload_board_dir = PathBuf::from(&crate::config::CONFIG.upload_dir).join(&board_short);
        let upload_path = upload_board_dir.join("hello.txt");
        let _upload_cleanup = PathCleanup(upload_board_dir.clone());
        ensure!(!upload_board_dir.exists());

        let app = Router::new()
            .route(
                "/admin/backup/extract-board",
                post(super::extract_board_from_full_backup),
            )
            .with_state(state.clone());
        let form_body = format!(
            "filename={filename}&board_short={board_short}&action=restore&_csrf={}",
            admin_signed_csrf()
        );

        let active_guard = state
            .maintenance_gate
            .try_begin("Full backup creation")
            .context("begin maintenance")?;
        let blocked = app
            .clone()
            .oneshot(admin_form_post(
                "/admin/backup/extract-board",
                form_body.clone(),
            )?)
            .await
            .context("send blocked extracted-board restore request")?;

        ensure!(blocked.status() == StatusCode::CONFLICT);
        ensure!(response_body_string(blocked)
            .await?
            .contains("already running"));
        {
            let conn = state.db.get().context("get database connection")?;
            ensure!(crate::db::get_board_by_short(&conn, &board_short)?.is_none());
        }
        ensure!(!upload_board_dir.exists());
        ensure!(std::fs::read(&backup_path).context("read retained full backup")? == backup_before);

        drop(active_guard);
        let allowed = app
            .oneshot(admin_form_post("/admin/backup/extract-board", form_body)?)
            .await
            .context("send allowed extracted-board restore request")?;

        ensure!(allowed.status() == StatusCode::SEE_OTHER);
        let location = allowed
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("restore response has no valid redirect location")?;
        ensure!(location.contains(&format!("board-backup-{board_short}")));
        {
            let conn = state.db.get().context("get database connection")?;
            ensure!(crate::db::get_board_by_short(&conn, &board_short)?.is_some());
        }
        ensure!(std::fs::read(&upload_path).context("read restored upload")? == b"hello");
        Ok(())
    }

    #[tokio::test]
    async fn saved_full_restore_invalid_zip_redirects_back_to_full_backup_section() -> TestResult<()>
    {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        std::fs::create_dir_all(full_backup_dir()).context("create full backup directory")?;
        let filename = unique_zip_name("saved-full-restore-invalid");
        let backup_path = full_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        std::fs::write(&backup_path, b"not-a-zip").context("write invalid ZIP")?;

        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://localhost"));

        let response = super::restore_saved_full_backup(
            axum::extract::State(state),
            admin_cookie_jar(),
            headers,
            crate::middleware::SecureCookieContext::new(
                Some(crate::test_support::connect_info().0),
                false,
            ),
            Form(super::RestoreSavedForm {
                filename,
                restore_tor_hidden_service_keys: false,
                csrf: Some(admin_signed_csrf()),
            }),
        )
        .await
        .context("restore saved full backup")?;

        ensure!(response.status() == StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("restore response has no valid redirect location")?;
        ensure!(location.contains("/admin/panel?restore_error="));
        ensure!(location.contains("Invalid+zip"));
        ensure!(location.contains("open=full-backup-restore"));
        ensure!(location.contains("#full-backup-restore"));
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
    )]
    #[tokio::test]
    async fn saved_board_restore_success_redirects_back_to_restored_board_section() -> TestResult<()>
    {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let suffix = unique.get(..7).unwrap_or(&unique);
        let board_short = format!("b{suffix}");
        let thread_id = {
            let conn = state.db.get().context("get database connection")?;
            let board_id = crate::db::create_board(&conn, &board_short, "Restore Test", "", false)
                .context("create restore test board")?;
            let post = sample_post(board_id, 0, "restored board body", true);
            let (thread_id, _, _) = crate::db::create_thread_with_optional_poll(
                &conn,
                board_id,
                Some("restore test thread"),
                &post,
                "",
                None,
                None,
            )
            .context("create restore test thread")?;
            thread_id
        };

        std::fs::create_dir_all(super::board_backup_dir())
            .context("create board backup directory")?;
        let _upload_cleanup =
            PathCleanup(PathBuf::from(&crate::config::CONFIG.upload_dir).join(&board_short));

        let app = Router::new()
            .route(
                "/admin/board/backup/create",
                post(super::create_board_backup),
            )
            .route(
                "/admin/board/backup/restore-saved",
                post(super::restore_saved_board_backup),
            )
            .route("/{board}", get(crate::handlers::board::board_index))
            .route(
                "/{board}/thread/{id}",
                get(crate::handlers::thread::view_thread),
            )
            .with_state(state.clone());

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/board/backup/create")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "_csrf={}&board_short={board_short}",
                        admin_signed_csrf()
                    )))
                    .context("build board-backup create request")?,
            )
            .await
            .context("send board-backup create request")?;

        ensure!(create_response.status() == StatusCode::SEE_OTHER);
        let create_location = create_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("create response has no valid redirect location")?;
        ensure!(create_location.contains("open=board-backup-restore"));
        ensure!(create_location.contains(&format!("#board-backup-{board_short}")));

        let filename = super::latest_board_backup_filename(&board_short)
            .context("created backup filename not found")?;
        let backup_path = crate::config::backups_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        ensure!(backup_path.exists());

        {
            let conn = state.db.get().context("get database connection")?;
            conn.execute_batch(&format!(
                "BEGIN; DELETE FROM boards WHERE short_name='{board_short}'; COMMIT;"
            ))
            .context("remove board before restore")?;
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/board/backup/restore-saved")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "_csrf={}&filename={filename}",
                        admin_signed_csrf()
                    )))
                    .context("build saved-board restore request")?,
            )
            .await
            .context("send saved-board restore request")?;

        ensure!(response.status() == StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("restore response has no valid redirect location")?;
        ensure!(location.contains("/admin/panel?"));
        ensure!(location.contains("open=board-backup-restore"));

        let board_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/{board_short}"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("build restored board-page request")?,
            )
            .await
            .context("send restored board-page request")?;
        ensure!(board_page.status() == StatusCode::OK);
        let board_body = to_bytes(board_page.into_body(), usize::MAX)
            .await
            .context("read restored board-page body")?;
        let board_body =
            String::from_utf8(board_body.to_vec()).context("decode restored board-page body")?;
        ensure!(board_body.contains("Restore Test"));
        ensure!(board_body.contains("restored board body"));

        let thread_page = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/{board_short}/thread/{thread_id}"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("build restored thread-page request")?,
            )
            .await
            .context("send restored thread-page request")?;
        ensure!(thread_page.status() == StatusCode::OK);
        let thread_body = to_bytes(thread_page.into_body(), usize::MAX)
            .await
            .context("read restored thread-page body")?;
        let thread_body =
            String::from_utf8(thread_body.to_vec()).context("decode restored thread-page body")?;
        ensure!(thread_body.contains("restored board body"));
        Ok(())
    }

    #[tokio::test]
    async fn saved_board_restore_invalid_zip_redirects_back_to_board_restore_section(
    ) -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        std::fs::create_dir_all(super::board_backup_dir())
            .context("create board backup directory")?;
        let filename = unique_zip_name("saved-board-restore-invalid");
        let backup_path = super::board_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        std::fs::write(&backup_path, b"not-a-zip").context("write invalid ZIP")?;

        let response = super::restore_saved_board_backup(
            axum::extract::State(state),
            admin_cookie_jar(),
            {
                let mut headers = HeaderMap::new();
                headers.insert(header::HOST, HeaderValue::from_static("localhost"));
                headers.insert(header::ORIGIN, HeaderValue::from_static("http://localhost"));
                headers
            },
            crate::test_support::connect_info(),
            Form(super::RestoreSavedForm {
                filename,
                restore_tor_hidden_service_keys: false,
                csrf: Some(admin_signed_csrf()),
            }),
        )
        .await
        .context("restore saved board backup")?;

        ensure!(response.status() == StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("restore response has no valid redirect location")?;
        ensure!(location.contains("/admin/panel?restore_error="));
        ensure!(location.contains("Invalid+zip"));
        ensure!(location.contains("open=board-backup-restore"));
        ensure!(location.contains("#board-backup-restore"));
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
    )]
    #[tokio::test]
    async fn extract_board_from_full_backup_download_redirects_and_cleans_up_temp_file(
    ) -> TestResult<()> {
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        std::fs::create_dir_all(full_backup_dir()).context("create full backup directory")?;
        std::fs::create_dir_all(temp_board_download_dir())
            .context("create temporary board-download directory")?;
        let filename = unique_zip_name("extract-board-download");
        let backup_path = full_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        write_sample_full_backup_zip_at(&backup_path, true)?;

        let kind_segment: String = ['{', 'k', 'i', 'n', 'd', '}'].into_iter().collect();
        let filename_segment: String = ['{', 'f', 'i', 'l', 'e', 'n', 'a', 'm', 'e', '}']
            .into_iter()
            .collect();
        let app = Router::new()
            .route(
                "/admin/backup/extract-board",
                post(super::extract_board_from_full_backup),
            )
            .route(
                &[
                    "/admin/backup/download/",
                    &kind_segment,
                    "/",
                    &filename_segment,
                ]
                .concat(),
                get(super::download_backup),
            )
            .with_state(state.clone());

        let active_guard = state
            .maintenance_gate
            .try_begin("Full backup creation")
            .context("begin concurrent maintenance")?;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/backup/extract-board")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "filename={filename}&board_short=tech&action=download&_csrf={}",
                        admin_signed_csrf()
                    )))
                    .context("build board-extraction request")?,
            )
            .await
            .context("send board-extraction request")?;
        drop(active_guard);

        ensure!(response.status() == StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("extraction response has no valid redirect location")?;
        ensure!(location.starts_with("/admin/backup/download/temp-board/"));
        ensure!(location.contains("cleanup=1"));
        let token = extract_location_query_param(location, "token")
            .context("redirect location has no download token")?;
        let download_filename = location
            .split('/')
            .nth(5)
            .and_then(|segment| segment.split_once('?').map(|(name, _)| name))
            .context("redirect location has no download filename")?;

        let download_path = temp_board_download_dir().join(download_filename);
        ensure!(download_path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = std::fs::metadata(&download_path)
                .context("read temporary ZIP metadata")?
                .permissions()
                .mode()
                & 0o777;
            ensure!(mode == 0o600);
        }
        ensure!(
            temp_board_download_token_path(download_filename).exists(),
            "download token should be written before the download"
        );

        let download_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/admin/backup/download/temp-board/{download_filename}?cleanup=1&token={token}"
                    ))
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::empty())
                    .context("build temporary board-download request")?,
            )
            .await
            .context("send temporary board-download request")?;

        ensure!(download_response.status() == StatusCode::OK);
        let expected_content_disposition = format!("attachment; filename=\"{download_filename}\"");
        ensure!(
            download_response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok())
                == Some(expected_content_disposition.as_str())
        );
        let body = to_bytes(download_response.into_body(), usize::MAX)
            .await
            .context("read temporary board-download body")?;
        ensure!(!body.is_empty());
        ensure!(
            !download_path.exists(),
            "temp-board archive should be removed after cleanup stream is consumed"
        );
        ensure!(
            !temp_board_download_token_path(download_filename).exists(),
            "temp-board download token should be consumed"
        );

        let replay_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/admin/backup/download/temp-board/{download_filename}?cleanup=1&token={token}"
                    ))
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::empty())
                    .context("build replayed board-download request")?,
            )
            .await
            .context("send replayed board-download request")?;

        ensure!(replay_response.status() == StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn temp_board_download_rejects_token_without_admin_session() -> TestResult<()> {
        let state = crate::test_support::app_state();
        let filename = unique_zip_name("temp-token-only");
        let token = "token-only-test";
        let download_path = temp_board_download_dir().join(&filename);
        let token_path = temp_board_download_token_path(&filename);
        let _download_cleanup = PathCleanup(download_path.clone());
        let _token_cleanup = PathCleanup(token_path.clone());
        crate::config::write_private_file(&download_path, b"temporary board backup")
            .context("write temporary board download")?;
        write_temp_board_download_token(&filename, token).context("write download token")?;
        let kind_segment: String = ['{', 'k', 'i', 'n', 'd', '}'].into_iter().collect();
        let filename_segment: String = ['{', 'f', 'i', 'l', 'e', 'n', 'a', 'm', 'e', '}']
            .into_iter()
            .collect();
        let app = Router::new()
            .route(
                &[
                    "/admin/backup/download/",
                    &kind_segment,
                    "/",
                    &filename_segment,
                ]
                .concat(),
                get(super::download_backup),
            )
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/admin/backup/download/temp-board/{filename}?cleanup=1&token={token}"
                    ))
                    .body(Body::empty())
                    .context("build unauthenticated board-download request")?,
            )
            .await
            .context("send unauthenticated board-download request")?;

        ensure!(response.status() == StatusCode::FORBIDDEN);
        ensure!(download_path.exists());
        ensure!(
            token_path.exists(),
            "unauthenticated token attempts must not consume the one-time token"
        );
        Ok(())
    }

    #[test]
    fn board_restore_rejects_invalid_access_mode() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source board")?;
        let mut manifest = build_board_backup_manifest(&source_conn, "tech")?;
        manifest.board.access_mode = "definitely_not_valid".to_owned();

        let target_pool = crate::db::init_test_pool().context("create target database pool")?;
        let mut target_conn = target_pool
            .get()
            .context("get target database connection")?;
        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;
        let error = execute_board_restore(
            &mut target_conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test invalid access mode restore",
            "Test invalid access mode restore completed",
        )
        .err()
        .context("restore unexpectedly accepted invalid access mode")?;

        let AppError::BadRequest(message) = error else {
            bail!("expected BadRequest, got {error:?}");
        };
        ensure!(message.contains("invalid access mode"));
        Ok(())
    }

    #[test]
    fn board_restore_rejects_protected_board_without_password_hash() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source board")?;
        let mut manifest = build_board_backup_manifest(&source_conn, "tech")?;
        manifest.board.access_mode = "view_password".to_owned();
        manifest.board.access_password_hash.clear();

        let target_pool = crate::db::init_test_pool().context("create target database pool")?;
        let mut target_conn = target_pool
            .get()
            .context("get target database connection")?;
        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;
        let error = execute_board_restore(
            &mut target_conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test missing access hash restore",
            "Test missing access hash restore completed",
        )
        .err()
        .context("restore unexpectedly accepted protected board without password hash")?;

        let AppError::BadRequest(message) = error else {
            bail!("expected BadRequest, got {error:?}");
        };
        ensure!(message.contains("password hash"));
        Ok(())
    }

    #[test]
    fn board_backup_manifest_preserves_pdf_upload_setting() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        let board_id = crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source board")?;
        source_conn
            .execute(
                "UPDATE boards SET allow_pdf = 1 WHERE id = ?1",
                params![board_id],
            )
            .context("enable PDF uploads")?;

        let manifest = build_board_backup_manifest(&source_conn, "tech")?;

        ensure!(manifest.board.allow_pdf);
        Ok(())
    }

    #[test]
    fn board_restore_preserves_pdf_upload_setting() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        let board_id = crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source board")?;
        source_conn
            .execute(
                "UPDATE boards SET allow_pdf = 1 WHERE id = ?1",
                params![board_id],
            )
            .context("enable PDF uploads")?;
        let manifest = build_board_backup_manifest(&source_conn, "tech")?;

        let target_pool = crate::db::init_test_pool().context("create target database pool")?;
        let mut target_conn = target_pool
            .get()
            .context("get target database connection")?;
        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;
        execute_board_restore(
            &mut target_conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test PDF setting restore",
            "Test PDF setting restore completed",
        )
        .context("restore board")?;

        let restored = crate::db::get_board_by_short(&target_conn, "tech")
            .context("load restored board")?
            .context("restored board not found")?;
        ensure!(restored.allow_pdf);
        Ok(())
    }

    #[test]
    fn older_board_restore_manifests_default_pdf_uploads_off() -> TestResult<()> {
        let json = serde_json::json!({
            "version": 1,
            "board": {
                "id": 1,
                "short_name": "tech",
                "name": "Technology",
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
                "allow_self_delete": true,
                "allow_archive": true,
                "allow_video_embeds": true,
                "allow_captcha": false,
                "show_poster_ids": false,
                "collapse_greentext": false,
                "post_cooldown_secs": 0,
                "banner_mode": "inherit",
                "access_mode": "public",
                "access_password_hash": "",
                "created_at": 1_700_000_000
            },
            "threads": [],
            "posts": [],
            "polls": [],
            "poll_options": [],
            "poll_votes": [],
            "file_hashes": [],
            "banners": []
        });

        let manifest: super::types::board_backup_types::BoardBackupManifest =
            serde_json::from_value(json).context("deserialize legacy manifest")?;

        ensure!(!manifest.board.allow_pdf);
        Ok(())
    }

    #[test]
    fn full_restore_layout_accepts_full_backup_archive() -> TestResult<()> {
        let archive = zip_with_entries(&[("chan.db", b"SQLite format 3\0stub")])?;
        ensure!(validate_full_restore_archive_layout(&archive).is_ok());
        Ok(())
    }

    #[test]
    fn full_restore_layout_rejects_board_backup_archive_with_helpful_hint() -> TestResult<()> {
        let archive = zip_with_entries(&[("board.json", br#"{"version":1}"#)])?;
        let error = validate_full_restore_archive_layout(&archive)
            .err()
            .context("board backup was unexpectedly accepted as a full backup")?;
        let AppError::BadRequest(message) = error else {
            bail!("expected BadRequest, got {error}");
        };
        ensure!(message.contains("board backup"));
        ensure!(message.contains("Board restore"));
        Ok(())
    }

    #[test]
    fn refresh_live_site_state_from_db_updates_banner_caches() -> TestResult<()> {
        crate::templates::set_live_site_name("Before restore");
        crate::templates::set_live_site_subtitle("before subtitle");

        let pool = crate::db::init_test_pool().context("create test database pool")?;
        let conn = pool.get().context("get database connection")?;
        crate::db::set_site_setting(&conn, "site_name", "RestoredChan").context("set site name")?;
        crate::db::set_site_setting(&conn, "site_subtitle", "restored subtitle")
            .context("set site subtitle")?;

        refresh_live_site_state_from_db(&conn).context("refresh live site state")?;

        ensure!(&*crate::templates::live_site_name() == "RestoredChan");
        ensure!(&*crate::templates::live_site_subtitle() == "restored subtitle");
        Ok(())
    }

    #[test]
    fn temp_board_download_token_is_one_time_use() -> TestResult<()> {
        let filename = "rustchan-board-test-20990101_000000.zip";
        let token = "token-123";
        let token_path = temp_board_download_token_path(filename);
        drop(std::fs::remove_file(&token_path));

        write_temp_board_download_token(filename, token).context("write download token")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            let token_mode = std::fs::metadata(&token_path)
                .context("read token metadata")?
                .permissions()
                .mode()
                & 0o777;
            ensure!(token_mode == 0o600);
            let directory_mode = std::fs::metadata(temp_board_download_dir())
                .context("read download-directory metadata")?
                .permissions()
                .mode()
                & 0o777;
            ensure!(directory_mode == 0o700);
        }
        ensure!(consume_temp_board_download_token(filename, token)?);
        ensure!(!consume_temp_board_download_token(filename, token)?);
        Ok(())
    }

    #[test]
    fn already_compressed_media_is_stored_without_recompression() {
        assert!(should_store_without_recompress(Path::new(
            "uploads/mu/track.FLAC"
        )));
        assert!(should_store_without_recompress(Path::new(
            "uploads/mu/cover.webp"
        )));
        assert!(should_store_without_recompress(Path::new(
            "uploads/mu/video.webm"
        )));
        assert!(!should_store_without_recompress(Path::new("board.json")));
        assert!(!should_store_without_recompress(Path::new(
            "uploads/mu/readme.txt"
        )));
    }

    fn write_sample_full_backup_zip_at(zip_path: &Path, indexed_boards: bool) -> TestResult<()> {
        write_sample_full_backup_zip_for_board_at(zip_path, indexed_boards, "tech")
    }

    fn write_sample_full_backup_zip_for_board_at(
        zip_path: &Path,
        indexed_boards: bool,
        board_short: &str,
    ) -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let db_path = temp_dir.path().join("snapshot.db");

        let pool = crate::db::init_test_pool().context("create test database pool")?;
        {
            let conn = pool.get().context("get database connection")?;
            crate::db::create_board(&conn, board_short, "Technology", "", false)
                .context("create fixture board")?;
            let board = crate::db::get_board_by_short(&conn, board_short)
                .context("load fixture board")?
                .context("fixture board not found")?;
            let post = crate::db::NewPost {
                thread_id: 0,
                board_id: board.id,
                name: "anon".into(),
                tripcode: None,
                subject: Some("backup test".into()),
                body: "hello".into(),
                body_html: "hello".into(),
                ip_hash: Some("hash".into()),
                file_path: Some(format!("{board_short}/hello.txt")),
                file_name: Some("hello.txt".into()),
                file_size: Some(5),
                thumb_path: None,
                mime_type: Some("text/plain".into()),
                media_type: Some("other".into()),
                audio_file_path: None,
                audio_file_name: None,
                audio_file_size: None,
                audio_mime_type: None,
                deletion_token: "token".into(),
                is_op: true,
            };
            crate::db::create_thread_with_optional_poll(
                &conn,
                board.id,
                Some("backup test"),
                &post,
                "",
                None,
                None,
            )
            .context("create fixture thread")?;

            let db_path_str = db_path
                .to_str()
                .context("database path is not valid UTF-8")?
                .replace('\'', "''");
            conn.execute_batch(&format!("VACUUM INTO '{db_path_str}'"))
                .context("vacuum database into snapshot")?;
        }

        let manifest = super::common::FullBackupManifest {
            version: if indexed_boards { 2 } else { 1 },
            generated_at: 1_700_000_000,
            rustchan_version: "1.1.3".into(),
            db_bytes: std::fs::metadata(&db_path)
                .context("read database snapshot metadata")?
                .len(),
            upload_file_count: 1,
            favicon_file_count: 0,
            banner_file_count: 0,
            tor_hidden_service_keys_included: false,
            tor_hidden_service_key_file_count: 0,
            boards: if indexed_boards {
                vec![BackupBoardSummary {
                    short_name: board_short.to_owned(),
                    name: "Technology".into(),
                }]
            } else {
                Vec::new()
            },
        };
        let manifest_json = serde_json::to_vec(&manifest).context("serialize manifest")?;
        let db_bytes = std::fs::read(&db_path).context("read database snapshot")?;

        {
            let file = std::fs::File::create(zip_path).context("create backup ZIP")?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(super::common::FULL_BACKUP_MANIFEST_NAME, options)
                .context("start manifest ZIP entry")?;
            zip.write_all(&manifest_json)
                .context("write manifest ZIP entry")?;
            zip.start_file("chan.db", options)
                .context("start database ZIP entry")?;
            zip.write_all(&db_bytes)
                .context("write database ZIP entry")?;
            zip.start_file(format!("uploads/{board_short}/hello.txt"), options)
                .context("start upload ZIP entry")?;
            zip.write_all(b"hello").context("write upload ZIP entry")?;
            zip.finish().context("finish backup ZIP")?;
        }
        Ok(())
    }

    fn build_sample_full_backup_zip(indexed_boards: bool) -> TestResult<PathBuf> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("full.zip");
        write_sample_full_backup_zip_at(&zip_path, indexed_boards)?;
        let persisted = temp_dir.keep();
        Ok(persisted.join("full.zip"))
    }

    #[test]
    fn prune_full_backup_dir_to_limit_removes_oldest_saved_backups() -> TestResult<()> {
        let dir = tempfile::tempdir().context("create temporary directory")?;
        let oldest = dir.path().join("rustchan-backup-20260101_000000.zip");
        let middle = dir.path().join("rustchan-backup-20260102_000000.zip");
        let newest = dir.path().join("rustchan-backup-20260103_000000.zip");
        write_sample_full_backup_zip_at(&oldest, true)?;
        write_sample_full_backup_zip_at(&middle, true)?;
        write_sample_full_backup_zip_at(&newest, true)?;

        let removed = super::prune_full_backup_dir_to_limit(dir.path(), 2)?;

        ensure!(removed == vec!["rustchan-backup-20260101_000000.zip".to_owned()]);
        ensure!(!oldest.exists());
        ensure!(middle.exists());
        ensure!(newest.exists());
        Ok(())
    }

    #[test]
    fn latest_verified_full_backup_modified_time_ignores_newer_invalid_zip() -> TestResult<()> {
        let backup_dir = tempfile::tempdir().context("create temporary directory")?;
        let valid_path = backup_dir
            .path()
            .join("rustchan-backup-20990101_000001-valid.zip");
        let invalid_path = backup_dir
            .path()
            .join("rustchan-backup-20990101_000002-invalid.zip");

        write_sample_full_backup_zip_at(&valid_path, true)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&invalid_path, b"not a zip archive").context("write invalid ZIP")?;

        let modified = latest_verified_full_backup_modified_time_in_dir(backup_dir.path())
            .context("verified backup time not found")?;
        let valid_modified = std::fs::metadata(&valid_path)
            .context("read valid backup metadata")?
            .modified()
            .context("read valid backup modification time")?;
        let modified_epoch = modified
            .duration_since(std::time::UNIX_EPOCH)
            .context("verified backup time precedes Unix epoch")?
            .as_secs();
        let valid_modified_epoch = valid_modified
            .duration_since(std::time::UNIX_EPOCH)
            .context("valid backup time precedes Unix epoch")?
            .as_secs();

        ensure!(modified_epoch == valid_modified_epoch);
        Ok(())
    }

    #[test]
    fn latest_verified_full_backup_modified_time_prefers_verified_v4_completed_at_over_dir_mtime(
    ) -> TestResult<()> {
        struct CleanupGuard(Vec<PathBuf>);
        impl Drop for CleanupGuard {
            fn drop(&mut self) {
                for path in self.0.drain(..) {
                    drop(std::fs::remove_dir_all(path));
                }
            }
        }

        let backup_root = crate::handlers::admin::backup::saved_backup::backups_root_dir();
        std::fs::create_dir_all(&backup_root).context("create backup root")?;
        let older_completed_dir = backup_root.join("2099-01-01_000001_full-site-newer-mtime-test");
        let newer_dir_mtime_dir =
            backup_root.join("2099-01-01_000002_full-site-older-completed-test");
        let _cleanup = CleanupGuard(vec![
            older_completed_dir.clone(),
            newer_dir_mtime_dir.clone(),
        ]);
        crate::handlers::admin::backup::saved_backup::write_saved_v4_fixture_for_test(
            &older_completed_dir,
            crate::handlers::admin::backup::saved_backup::BackupScope::FullSite,
            crate::handlers::admin::backup::saved_backup::board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            4_102_444_800,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        crate::handlers::admin::backup::saved_backup::write_saved_v4_fixture_for_test(
            &newer_dir_mtime_dir,
            crate::handlers::admin::backup::saved_backup::BackupScope::FullSite,
            crate::handlers::admin::backup::saved_backup::board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            4_102_444_700,
        )?;
        invalidate_backup_list_cache(&full_backup_dir(), BackupListKind::Full);

        let modified = latest_verified_full_backup_modified_time()
            .context("verified v4 backup time not found")?;
        let modified_epoch = modified
            .duration_since(std::time::UNIX_EPOCH)
            .context("verified v4 backup time precedes Unix epoch")?
            .as_secs();

        ensure!(modified_epoch == 4_102_444_800);
        Ok(())
    }

    #[test]
    fn full_backup_can_extract_board_backup() -> TestResult<()> {
        let zip_path = build_sample_full_backup_zip(true)?;
        let (board_zip_path, filename) =
            create_temp_board_backup_from_full_backup_path(&zip_path, "tech")?;

        ensure!(filename.contains("from-full"));
        let manifest = super::common::verify_board_backup_zip(&board_zip_path)?;
        ensure!(manifest.board.short_name == "tech");

        let file = std::fs::File::open(&board_zip_path).context("open board ZIP")?;
        let mut archive = zip::ZipArchive::new(file).context("parse board ZIP")?;
        ensure!(archive.by_name("uploads/tech/hello.txt").is_ok());

        drop(std::fs::remove_file(board_zip_path));
        drop(std::fs::remove_file(zip_path));
        Ok(())
    }

    #[test]
    fn older_full_backup_without_board_index_still_extracts_board_backup() -> TestResult<()> {
        let zip_path = build_sample_full_backup_zip(false)?;
        let (board_zip_path, _) =
            create_temp_board_backup_from_full_backup_path(&zip_path, "tech")?;

        let manifest = super::common::verify_board_backup_zip(&board_zip_path)?;
        ensure!(manifest.board.short_name == "tech");

        drop(std::fs::remove_file(board_zip_path));
        drop(std::fs::remove_file(zip_path));
        Ok(())
    }

    #[test]
    fn board_restore_preserves_original_post_ids_when_they_are_free() -> TestResult<()> {
        let pool = crate::db::init_test_pool().context("create test database pool")?;
        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let mut conn = pool.get().context("get database connection")?;

        crate::db::create_board(&conn, "tech", "Technology", "", false).context("create board")?;
        let tech_board = crate::db::get_board_by_short(&conn, "tech")
            .context("load board")?
            .context("tech board not found")?;

        let (thread_id, op_post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            tech_board.id,
            Some("quoted thread"),
            &sample_post(tech_board.id, 0, "op body", true),
            "",
            None,
            None,
        )
        .context("create thread")?;
        let reply_body = format!(">>{op_post_id}\nreply body");
        let reply_post_id = crate::db::create_reply_with_thread_update(
            &conn,
            &sample_post(tech_board.id, thread_id, &reply_body, false),
            "",
            true,
            None,
        )
        .context("create reply")?;

        let manifest = build_board_backup_manifest(&conn, "tech")?;
        crate::db::delete_board(&conn, tech_board.id).context("delete board")?;

        crate::db::create_board(&conn, "b", "Random", "", false).context("create other board")?;
        let other_board = crate::db::get_board_by_short(&conn, "b")
            .context("load other board")?
            .context("other board not found")?;
        let (_, other_post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            other_board.id,
            Some("other thread"),
            &sample_post(other_board.id, 0, "other post", true),
            "",
            None,
            None,
        )
        .context("create other thread")?;
        ensure!(other_post_id > reply_post_id);

        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;

        execute_board_restore(
            &mut conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test board restore",
            "Test board restore completed",
        )
        .context("restore board")?;

        let restored_op = crate::db::get_post_on_board(&conn, "tech", op_post_id)
            .context("load restored OP")?
            .context("restored OP not found")?;
        let restored_reply = crate::db::get_post_on_board(&conn, "tech", reply_post_id)
            .context("load restored reply")?
            .context("restored reply not found")?;

        ensure!(restored_op.id == op_post_id);
        ensure!(restored_op.thread_id == thread_id);
        ensure!(restored_reply.id == reply_post_id);
        ensure!(restored_reply.thread_id == thread_id);
        ensure!(restored_reply.body == reply_body);
        ensure!(restored_reply
            .body_html
            .contains(&format!("data-pid=\"{op_post_id}\"")));
        ensure!(crate::db::get_post_on_board(&conn, "b", other_post_id)?.is_some());
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the ID-sequence regression keeps source padding, restore, and post-restore allocation checks in one scenario"
    )]
    #[test]
    fn board_restore_preserves_free_ids_above_target_sequence() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        crate::db::create_board(&source_conn, "pad", "Padding", "", false)
            .context("create padding board")?;
        let pad_board = crate::db::get_board_by_short(&source_conn, "pad")
            .context("load padding board")?
            .context("padding board not found")?;
        for idx in 0..5 {
            crate::db::create_thread_with_optional_poll(
                &source_conn,
                pad_board.id,
                Some("padding"),
                &sample_post(pad_board.id, 0, &format!("padding {idx}"), true),
                "",
                None,
                None,
            )
            .context("create padding thread")?;
        }
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source tech board")?;
        let source_tech_board = crate::db::get_board_by_short(&source_conn, "tech")
            .context("load source tech board")?
            .context("source tech board not found")?;
        let (source_thread_id, source_op_id, _) = crate::db::create_thread_with_optional_poll(
            &source_conn,
            source_tech_board.id,
            Some("high ids"),
            &sample_post(source_tech_board.id, 0, "source op", true),
            "",
            None,
            None,
        )
        .context("create source thread")?;
        let source_reply_id = crate::db::create_reply_with_thread_update(
            &source_conn,
            &sample_post(
                source_tech_board.id,
                source_thread_id,
                &format!(">>{source_op_id}\nsource reply"),
                false,
            ),
            "",
            true,
            None,
        )
        .context("create source reply")?;
        ensure!(source_op_id > 5);

        let manifest = build_board_backup_manifest(&source_conn, "tech")?;

        let target_pool = crate::db::init_test_pool().context("create target database pool")?;
        let mut target_conn = target_pool
            .get()
            .context("get target database connection")?;
        crate::db::create_board(&target_conn, "b", "Random", "", false)
            .context("create target board")?;
        let target_b = crate::db::get_board_by_short(&target_conn, "b")
            .context("load target board")?
            .context("target board not found")?;
        crate::db::create_thread_with_optional_poll(
            &target_conn,
            target_b.id,
            Some("low ids"),
            &sample_post(target_b.id, 0, "target op", true),
            "",
            None,
            None,
        )
        .context("create target thread")?;

        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;
        execute_board_restore(
            &mut target_conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test board restore high ids",
            "Test board restore high ids completed",
        )
        .context("restore board with high IDs")?;

        let restored_op = crate::db::get_post_on_board(&target_conn, "tech", source_op_id)
            .context("load restored OP")?
            .context("restored OP not found")?;
        let restored_reply = crate::db::get_post_on_board(&target_conn, "tech", source_reply_id)
            .context("load restored reply")?
            .context("restored reply not found")?;
        ensure!(restored_op.thread_id == source_thread_id);
        ensure!(restored_reply.thread_id == source_thread_id);

        let restored_board = crate::db::get_board_by_short(&target_conn, "tech")
            .context("load restored board")?
            .context("restored board not found")?;
        let new_post_id = crate::db::create_reply_with_thread_update(
            &target_conn,
            &sample_post(
                restored_board.id,
                source_thread_id,
                &format!(">>{source_op_id}\nafter restore"),
                false,
            ),
            "",
            true,
            None,
        )
        .context("create post after restore")?;
        ensure!(new_post_id > source_reply_id);
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
    )]
    #[test]
    fn board_restore_fallback_remaps_same_board_crosslinks_when_ids_collide() -> TestResult<()> {
        let source_pool = crate::db::init_test_pool().context("create source database pool")?;
        let source_conn = source_pool
            .get()
            .context("get source database connection")?;
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .context("create source tech board")?;
        let source_board = crate::db::get_board_by_short(&source_conn, "tech")
            .context("load source tech board")?
            .context("source tech board not found")?;
        let (source_thread_id, source_op_id, _) = crate::db::create_thread_with_optional_poll(
            &source_conn,
            source_board.id,
            Some("crosslinks"),
            &sample_post(source_board.id, 0, "source op", true),
            "",
            None,
            None,
        )
        .context("create source thread")?;
        let source_reply_id = crate::db::create_reply_with_thread_update(
            &source_conn,
            &sample_post(
                source_board.id,
                source_thread_id,
                &format!(">>{source_op_id}\n>>>/tech/{source_op_id}\n>>>/b/{source_op_id}"),
                false,
            ),
            "",
            true,
            None,
        )
        .context("create source reply")?;
        ensure!(source_op_id == 1);
        ensure!(source_reply_id == 2);

        let manifest = build_board_backup_manifest(&source_conn, "tech")?;

        let target_pool = crate::db::init_test_pool().context("create target database pool")?;
        let mut target_conn = target_pool
            .get()
            .context("get target database connection")?;
        crate::db::create_board(&target_conn, "b", "Random", "", false)
            .context("create target board")?;
        let target_board = crate::db::get_board_by_short(&target_conn, "b")
            .context("load target board")?
            .context("target board not found")?;
        let (existing_thread_id, existing_op_id, _) = crate::db::create_thread_with_optional_poll(
            &target_conn,
            target_board.id,
            Some("existing"),
            &sample_post(target_board.id, 0, "existing op", true),
            "",
            None,
            None,
        )
        .context("create existing thread")?;
        let existing_reply_id = crate::db::create_reply_with_thread_update(
            &target_conn,
            &sample_post(target_board.id, existing_thread_id, "existing reply", false),
            "",
            true,
            None,
        )
        .context("create existing reply")?;
        ensure!((existing_op_id, existing_reply_id) == (1, 2));

        let upload_dir = tempfile::tempdir().context("create upload directory")?;
        let upload_dir_str = upload_dir
            .path()
            .to_str()
            .context("upload directory path is not valid UTF-8")?;
        execute_board_restore(
            &mut target_conn,
            upload_dir_str,
            manifest,
            |_| Ok(()),
            "Test board restore remap",
            "Test board restore remap completed",
        )
        .context("restore board with ID collisions")?;

        let restored_board_id: i64 = target_conn
            .query_row(
                "SELECT id FROM boards WHERE short_name = 'tech'",
                [],
                |row| row.get(0),
            )
            .context("load restored board ID")?;
        let restored_op_id: i64 = target_conn
            .query_row(
                "SELECT id FROM posts WHERE board_id = ?1 AND is_op = 1",
                params![restored_board_id],
                |row| row.get(0),
            )
            .context("load restored OP ID")?;
        let (restored_reply_id, restored_body, restored_body_html): (i64, String, String) =
            target_conn
                .query_row(
                    "SELECT id, body, body_html
                     FROM posts
                     WHERE board_id = ?1 AND is_op = 0",
                    params![restored_board_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .context("load restored reply")?;

        ensure!(restored_op_id > source_op_id);
        ensure!(restored_reply_id > source_reply_id);
        ensure!(restored_body.contains(&format!(">>{restored_op_id}")));
        ensure!(restored_body.contains(&format!(">>>/tech/{restored_op_id}")));
        ensure!(restored_body.contains(">>>/b/1"));
        ensure!(
            restored_body_html.contains(&format!("data-pid=\"{restored_op_id}\"")),
            "same-board quotelink should point at remapped post id"
        );
        ensure!(
            restored_body_html.contains(&format!("/tech/post/{restored_op_id}")),
            "same-board crosslink should point at remapped post id"
        );
        ensure!(restored_body_html.contains("/b/post/1"));
        Ok(())
    }
}
