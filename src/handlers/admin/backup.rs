// Backup and restore subsystem for the admin panel.
// Covers full-site backups, board-level backups, streaming downloads,
// saved-backup restoration, and live board.json restore.

/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers::admin::backup");

use crate::{
    banner,
    config::CONFIG,
    db,
    error::{AppError, Result},
    handlers::is_xml_http_request,
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
mod board_manifest;
mod create;
mod downloads;
mod http;
mod listing;
mod restore_board;
mod restore_full;
mod safety;
mod storage;
pub(in crate::server) use storage::BackupStorageMode;

pub(in crate::server) use create::*;
pub(in crate::server) use downloads::{backup_progress_json, delete_backup, download_backup};
pub(in crate::server) use http::backup_request_logging_middleware;
pub(in crate::server) use listing::{list_backup_files, BackupListKind};
pub(in crate::server) use restore_board::{
    board_restore, extract_board_from_full_backup, restore_saved_board_backup,
};
pub(in crate::server) use restore_full::{admin_restore, restore_saved_full_backup};
use safety::{
    copy_limited, create_staging_dir, extract_uploads_to_dir, log_backup_phase,
    log_backup_progress, read_limited_bytes, remap_body_quotelinks, remove_path_if_exists,
    render_restored_body_html, validate_board_short_name, validate_entry_path_under_prefix,
    validate_restore_safe_entry_name, verify_full_backup_archive, BANNER_RESTORE_ENTRY_MAX_BYTES,
    BANNER_RESTORE_TOTAL_MAX_BYTES, BOARD_MANIFEST_MAX_BYTES, ZIP_ENTRY_MAX_BYTES,
};

/// Full backup restore section used by this handler.
const FULL_BACKUP_RESTORE_SECTION: &str = "full-backup-restore";
/// Board backup restore section used by this handler.
const BOARD_BACKUP_RESTORE_SECTION: &str = "board-backup-restore";
/// `SQLite` header used by this handler.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Deserialize)]
pub(in crate::server) struct RestoreSavedForm {
    filename: String,
    #[serde(default, deserialize_with = "deserialize_form_checkbox")]
    restore_tor_hidden_service_keys: bool,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

fn deserialize_form_checkbox<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(super::settings::checkbox_is_on(value.as_deref()))
}

use archive::{
    canonicalize_restored_banner_dir, convert_transfer_zip_to_full_restore_archive,
    extract_board_archive_from_full_backup, extract_board_archive_from_saved_full_backup,
    parse_board_backup_manifest_from_zip, prepare_saved_board_restore_archive,
    prepare_saved_full_restore_archive, validate_full_restore_archive_layout,
};
use downloads::{prune_stale_temp_board_downloads, write_temp_board_download_token};
use http::{
    log_restore_upload_started, redirect_page_response, restore_auth_preflight,
    restore_error_redirect_target, restore_failure_response, restore_start_response,
    restore_success_redirect_target, restore_upload_parse_response, sanitize_board_short_value,
    stream_restore_upload_to_tempfile, validate_backup_zip_filename,
    validate_saved_backup_reference, validate_streamed_restore_upload, RestoreKind,
};
pub(in crate::server) use listing::latest_verified_full_backup_modified_time;
use listing::{
    enforce_full_backup_retention, invalidate_backup_list_cache, latest_board_backup_reference,
};
use restore_full::restore_db_from_snapshot;

#[expect(
    clippy::too_many_lines,
    reason = "database snapshotting, archive creation, and temporary-file cleanup form one operation"
)]
pub(in crate::server) async fn admin_backup(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
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
                zip.start_file(safety::FULL_BACKUP_MANIFEST_NAME, opts)
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
                let _files_before_snapshot = progress.files_done.fetch_add(1, Ordering::Relaxed);
                let _bytes_before_snapshot =
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

            if let Err(error) = safety::verify_full_backup_zip(zip_tmp.path()) {
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
    drop(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_mins(10)).await;
        drop(tokio::fs::remove_file(cleanup_path).await);
    }));

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
            acc.saturating_add(count_files_in_dir(&p))
        } else if metadata.file_type().is_file()
            && crate::utils::fs_security::assert_regular_file_no_symlink(&p).is_ok()
        {
            acc.saturating_add(1)
        } else {
            acc
        }
    })
}

/// Recursively add every file under `dir` into the zip as `uploads/{rel_path}`.
///
/// MEM-FIX: Uses `std::io::copy` with the zip writer directly, streaming each
/// file through a kernel buffer (~8 KiB) instead of reading the whole file
/// into a `Vec<u8>` first.  Peak RAM per file = `io::copy`'s 8 KiB stack buffer.
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
            tracing::warn!(target: LOG_TARGET, path = %path.display(), "skipping symlink during backup traversal");
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
                tracing::warn!(target: LOG_TARGET, path = %path.display(), "skipping unsafe runtime file during backup");
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
            let _files_before_archive_entry = progress.files_done.fetch_add(1, Ordering::Relaxed);
            let _bytes_before_archive_entry =
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

/// Legacy full ZIP directory within the configured backup root.
pub(in crate::server) fn full_backup_dir() -> PathBuf {
    crate::config::full_backups_dir()
}

/// Legacy board ZIP directory within the configured backup root.
pub(super) fn board_backup_dir() -> PathBuf {
    crate::config::board_backups_dir()
}

pub(super) fn local_backup_timestamp_label() -> String {
    Local::now().format("%Y%m%d_%H%M%S").to_string()
}

fn unique_backup_filename(dir: &Path, base_name: &str) -> String {
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

/// rustchan-data/runtime/tmp/board-downloads/.
fn temp_board_download_dir() -> PathBuf {
    crate::config::runtime_temp_board_downloads_dir()
}

// Board-level backup / restore
#[derive(Deserialize)]
pub(in crate::server) struct BoardBackupDownloadQuery {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Stream a board-level backup zip: manifest JSON + that board's upload files.
///
/// MEM-FIX: Same approach as `admin_backup` — build zip into a `NamedTempFile` on
/// disk, then stream the result in 64 KiB chunks.
pub(in crate::server) async fn board_backup(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
            conn.query_row(
                "SELECT 1 FROM boards WHERE short_name = ?1",
                params![safe_board],
                |_| Ok(()),
            )
            .map_err(|_error| AppError::NotFound(format!("Board '{safe_board}' not found")))?;

            latest_board_backup_reference(&safe_board).ok_or_else(|| {
                AppError::NotFound(format!(
                    "No saved backup found for /{safe_board}/. Create one from the admin panel first."
                ))
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let saved_root = crate::config::backups_dir().join(&filename);
    if saved_root.is_dir() {
        let (temp_zip, temp_name) =
            prepare_saved_board_restore_archive(&saved_root, Some(&safe_board))?;
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
