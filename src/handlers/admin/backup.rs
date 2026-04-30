// The closure keeps the call-site shape aligned with the surrounding combinator chain.
#![allow(
    clippy::redundant_closure_for_method_calls,
    clippy::needless_pass_by_value,
    clippy::significant_drop_in_scrutinee,
    clippy::redundant_pub_crate,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]

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
    extract::{Form, FromRequest, Multipart, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::Utc;
use futures::stream::Stream;
use rusqlite::{backup::Backup, params, OptionalExtension};
use serde::Deserialize;
use serde_json;
use std::collections::HashMap;
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};
use time;
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;
use tracing::warn;

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
mod types;

use common::{
    copy_limited, create_staging_dir, extract_uploads_to_dir, log_backup_phase,
    log_backup_progress, read_limited_bytes, remap_body_quotelinks, remove_path_if_exists,
    render_restored_body_html, restore_safe_relative_path_under_prefix, validate_board_short_name,
    validate_restore_safe_entry_name, verify_full_backup_archive, BANNER_RESTORE_ENTRY_MAX_BYTES,
    BANNER_RESTORE_TOTAL_MAX_BYTES, BOARD_MANIFEST_MAX_BYTES, ZIP_ENTRY_MAX_BYTES,
};
pub use create::*;
pub use downloads::{
    backup_progress_json, delete_backup, download_backup, write_temp_board_download_token,
};
pub use http::backup_request_logging_middleware;
pub use listing::{invalidate_backup_list_cache, list_backup_files, BackupListKind};
pub use restore_board::{
    board_restore, extract_board_from_full_backup, restore_saved_board_backup,
};
pub use restore_full::{admin_restore, restore_saved_full_backup};
use types::board_backup_types;

const FULL_BACKUP_RESTORE_SECTION: &str = "full-backup-restore";
const BOARD_BACKUP_RESTORE_SECTION: &str = "board-backup-restore";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[derive(Deserialize)]
pub struct RestoreSavedForm {
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
    parse_board_backup_manifest_from_zip, validate_full_restore_archive_layout,
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
    sanitize_backup_zip_filename, sanitize_board_short_value, stream_restore_upload_to_tempfile,
    validate_streamed_restore_upload, RestoreKind,
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

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
pub async fn admin_backup(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Full backup download")?;
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
    let global_banner_dir = crate::banner::backup_source_dir();
    let progress = state.backup_progress.clone();

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let uploads_base = std::path::Path::new(&upload_dir);

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
            let manifest = create::build_full_backup_manifest(
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

                // ── Database snapshot (streamed, not read into RAM) ────────
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB entry: {e}")))?;
                let mut db_src = std::fs::File::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {e}")))?;
                let copied = std::io::copy(&mut db_src, &mut zip)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {e}")))?;
                drop(db_src);
                let _ = std::fs::remove_file(&temp_db);
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
                log_backup_progress(&progress);

                // ── Upload files (streamed file-by-file via io::copy) ──────
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
                let _ = std::fs::remove_file(&temp_db);
                return Err(error);
            }

            if let Err(error) = common::verify_full_backup_zip(zip_tmp.path()) {
                let _ = std::fs::remove_file(&temp_db);
                return Err(error);
            }

            let file_size = zip_tmp.as_file().metadata().map(|m| m.len()).unwrap_or(0);

            // Persist the temp file (prevents auto-delete on drop).
            // We delete it manually in the background after serving.
            let (_, tmp_path_obj) = zip_tmp.into_parts();
            let final_path = tmp_path_obj
                .keep()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Persist temp zip: {e}")))?;

            let ts = Utc::now().format("%Y%m%d_%H%M%S");
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
        tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        let _ = tokio::fs::remove_file(cleanup_path).await;
    });

    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

/// Count regular files (not directories) under `dir` recursively.
/// Used to initialise the progress bar's `files_total` before compression starts.
fn count_files_in_dir(dir: &std::path::Path) -> u64 {
    if !dir.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().fold(0u64, |acc, entry| {
        let p = entry.path();
        if p.is_dir() {
            acc + count_files_in_dir(&p)
        } else if p.is_file() {
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
    base: &std::path::Path,
    dir: &std::path::Path,
    opts: zip::write::SimpleFileOptions,
    progress: &crate::middleware::BackupProgress,
) -> Result<()> {
    add_dir_to_zip_with_prefix(zip, base, dir, "uploads", opts, progress)
}

pub(super) fn add_dir_to_zip_with_prefix<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    base: &std::path::Path,
    dir: &std::path::Path,
    prefix: &str,
    opts: zip::write::SimpleFileOptions,
    progress: &crate::middleware::BackupProgress,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("read_dir {}: {}", dir.display(), e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| AppError::Internal(anyhow::anyhow!("dir entry: {e}")))?;
        let path = entry.path();

        let relative = path
            .strip_prefix(base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("strip_prefix: {e}")))?;
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        let zip_path = format!("{prefix}/{rel_str}");

        if path.is_dir() {
            zip.add_directory(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip dir: {e}")))?;
            add_dir_to_zip_with_prefix(zip, base, &path, prefix, opts, progress)?;
        } else if path.is_file() {
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
pub fn full_backup_dir() -> PathBuf {
    crate::config::full_backups_dir()
}

/// rustchan-data/backups/boards/
pub fn board_backup_dir() -> PathBuf {
    crate::config::board_backups_dir()
}

pub fn unique_backup_filename(dir: &Path, base_name: &str) -> String {
    let candidate = dir.join(base_name);
    if !candidate.exists() {
        return base_name.to_string();
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
pub fn temp_board_download_dir() -> PathBuf {
    crate::config::runtime_temp_board_downloads_dir()
}

// ─── Board-level backup / restore ─────────────────────────────────────────────

/// Stream a board-level backup zip: manifest JSON + that board's upload files.
///
/// MEM-FIX: Same approach as `admin_backup` — build zip into a `NamedTempFile` on
/// disk, then stream the result in 64 KiB chunks.
#[allow(clippy::too_many_lines)]
pub async fn board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(board_short): axum::extract::Path<String>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            conn.query_row(
                "SELECT 1 FROM boards WHERE short_name = ?1",
                params![safe_board],
                |_| Ok(()),
            )
            .map_err(|_| AppError::NotFound(format!("Board '{safe_board}' not found")))?;

            latest_board_backup_filename(&safe_board).ok_or_else(|| {
                AppError::NotFound(format!(
                    "No saved backup found for /{safe_board}/. Create one from the admin panel first."
                ))
            })
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/admin/backup/download/board/{filename}")).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        build_board_backup_manifest, consume_temp_board_download_token,
        create_temp_board_backup_from_full_backup_path, execute_board_restore, full_backup_dir,
        latest_verified_full_backup_modified_time_in_dir, refresh_live_site_state_from_db,
        render_restored_body_html, should_store_without_recompress, temp_board_download_dir,
        temp_board_download_token_path, validate_full_restore_archive_layout,
        write_temp_board_download_token, RestoreKind,
    };
    use crate::error::AppError;
    use crate::models::BackupBoardSummary;
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
    use std::path::Path;
    use tower::ServiceExt as _;

    fn zip_with_entries(entries: &[(&str, &[u8])]) -> zip::ZipArchive<Cursor<Vec<u8>>> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default();
            for (name, body) in entries {
                writer.start_file(*name, options).expect("start file");
                writer.write_all(body).expect("write file");
            }
            writer.finish().expect("finish zip");
        }
        cursor.set_position(0);
        zip::ZipArchive::new(cursor).expect("zip archive")
    }

    async fn echo_restore_saved_form(Form(form): Form<super::RestoreSavedForm>) -> String {
        form.restore_tor_hidden_service_keys.to_string()
    }

    #[tokio::test]
    async fn restore_saved_form_accepts_checked_browser_checkbox_value() {
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
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"true");
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

    struct PathCleanup(std::path::PathBuf);

    impl Drop for PathCleanup {
        fn drop(&mut self) {
            match std::fs::metadata(&self.0) {
                Ok(metadata) if metadata.is_dir() => {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
                Ok(_) => {
                    let _ = std::fs::remove_file(&self.0);
                }
                Err(_) => {}
            }
        }
    }

    fn install_admin_session(state: &crate::middleware::AppState) {
        let conn = state.db.get().expect("db connection");
        let password_hash = crate::utils::crypto::hash_password("hunter2").expect("hash password");
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .expect("create session");
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

    fn latest_zip_name(dir: &Path, prefix: &str) -> Option<String> {
        let mut entries = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_zip = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
                let matches_prefix = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix));
                (is_zip && matches_prefix).then_some(path)
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries.pop().and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
    }

    fn extract_location_query_param(location: &str, key: &str) -> Option<String> {
        location.split_once('?').and_then(|(_, query)| {
            query.split('&').find_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                (name == key).then(|| value.to_string())
            })
        })
    }

    #[tokio::test]
    async fn admin_xhr_bad_request_returns_handled_json_error() {
        let response = super::admin_xhr_error_response(&AppError::BadRequest("bad restore".into()));

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::BAD_REQUEST.as_str())
        );

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("bad restore"));
    }

    #[tokio::test]
    async fn restore_upload_parse_xhr_returns_handled_json_error() {
        let response = super::restore_upload_parse_response(
            RestoreKind::Full,
            true,
            &"missing multipart field",
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-rustchan-error-status")
                .and_then(|value| value.to_str().ok()),
            Some(StatusCode::BAD_REQUEST.as_str())
        );

        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body")
                .to_vec(),
        )
        .expect("utf8 body");
        assert!(body.contains("Upload parsing failed"));
        assert!(body.contains("missing multipart field"));
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
    async fn saved_full_restore_invalid_zip_redirects_back_to_full_backup_section() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        std::fs::create_dir_all(super::full_backup_dir()).expect("create full backup dir");
        let filename = unique_zip_name("saved-full-restore-invalid");
        let backup_path = super::full_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        std::fs::write(&backup_path, b"not-a-zip").expect("write invalid zip");

        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://localhost"));

        let response = super::restore_saved_full_backup(
            axum::extract::State(state),
            admin_cookie_jar(),
            headers,
            crate::test_support::connect_info(),
            axum::extract::Form(super::RestoreSavedForm {
                filename,
                restore_tor_hidden_service_keys: false,
                csrf: Some(admin_signed_csrf()),
            }),
        )
        .await
        .expect("restore response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect location");
        assert!(location.contains("/admin/panel?restore_error="));
        assert!(location.contains("Invalid+zip"));
        assert!(location.contains("open=full-backup-restore"));
        assert!(location.contains("#full-backup-restore"));
    }

    #[tokio::test]
    async fn saved_board_restore_success_redirects_back_to_restored_board_section() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        let board_short = format!("b{}", &uuid::Uuid::new_v4().simple().to_string()[..7]);
        let thread_id = {
            let conn = state.db.get().expect("db connection");
            let board_id = crate::db::create_board(&conn, &board_short, "Restore Test", "", false)
                .expect("create board");
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
            .expect("create thread");
            thread_id
        };

        std::fs::create_dir_all(super::board_backup_dir()).expect("create board backup dir");
        let _upload_cleanup = PathCleanup(
            std::path::PathBuf::from(&crate::config::CONFIG.upload_dir).join(&board_short),
        );

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
                    .expect("request"),
            )
            .await
            .expect("create response");

        assert_eq!(create_response.status(), StatusCode::SEE_OTHER);
        let create_location = create_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("create redirect location");
        assert!(create_location.contains("open=board-backup-restore"));
        assert!(create_location.contains(&format!("#board-backup-{board_short}")));

        let filename = latest_zip_name(
            super::board_backup_dir().as_path(),
            &format!("rustchan-board-{board_short}-"),
        )
        .expect("created backup filename");
        let backup_path = super::board_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        assert!(backup_path.exists());

        {
            let conn = state.db.get().expect("db connection");
            conn.execute_batch(&format!(
                "BEGIN; DELETE FROM boards WHERE short_name='{board_short}'; COMMIT;"
            ))
            .expect("mutate board");
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
                    .expect("request"),
            )
            .await
            .expect("restore response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect location");
        assert!(location.contains("/admin/panel?"));
        assert!(location.contains("open=board-backup-restore"));

        let board_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/{board_short}"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("board page response");
        assert_eq!(board_page.status(), StatusCode::OK);
        let board_body = to_bytes(board_page.into_body(), usize::MAX)
            .await
            .expect("board body");
        let board_body = String::from_utf8(board_body.to_vec()).expect("utf8 board body");
        assert!(board_body.contains("Restore Test"));
        assert!(board_body.contains("restored board body"));

        let thread_page = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/{board_short}/thread/{thread_id}"))
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("thread page response");
        assert_eq!(thread_page.status(), StatusCode::OK);
        let thread_body = to_bytes(thread_page.into_body(), usize::MAX)
            .await
            .expect("thread body");
        let thread_body = String::from_utf8(thread_body.to_vec()).expect("utf8 thread body");
        assert!(thread_body.contains("restored board body"));
    }

    #[tokio::test]
    async fn saved_board_restore_invalid_zip_redirects_back_to_board_restore_section() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        std::fs::create_dir_all(super::board_backup_dir()).expect("create board backup dir");
        let filename = unique_zip_name("saved-board-restore-invalid");
        let backup_path = super::board_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        std::fs::write(&backup_path, b"not-a-zip").expect("write invalid zip");

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
            axum::extract::Form(super::RestoreSavedForm {
                filename,
                restore_tor_hidden_service_keys: false,
                csrf: Some(admin_signed_csrf()),
            }),
        )
        .await
        .expect("restore response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect location");
        assert!(location.contains("/admin/panel?restore_error="));
        assert!(location.contains("Invalid+zip"));
        assert!(location.contains("open=board-backup-restore"));
        assert!(location.contains("#board-backup-restore"));
    }

    #[tokio::test]
    async fn extract_board_from_full_backup_download_redirects_and_cleans_up_temp_file() {
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        std::fs::create_dir_all(full_backup_dir()).expect("create full backup dir");
        std::fs::create_dir_all(temp_board_download_dir()).expect("create temp board download dir");
        let filename = unique_zip_name("extract-board-download");
        let backup_path = full_backup_dir().join(&filename);
        let _backup_cleanup = PathCleanup(backup_path.clone());
        write_sample_full_backup_zip_at(&backup_path, true);

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
                    .expect("request"),
            )
            .await
            .expect("extract response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("redirect location");
        assert!(location.starts_with("/admin/backup/download/temp-board/"));
        assert!(location.contains("cleanup=1"));
        let token = extract_location_query_param(location, "token").expect("download token");
        let download_filename = location
            .split('/')
            .nth(5)
            .and_then(|segment| segment.split_once('?').map(|(name, _)| name))
            .expect("download filename");

        let download_path = temp_board_download_dir().join(download_filename);
        assert!(download_path.exists());
        assert!(
            temp_board_download_token_path(download_filename).exists(),
            "download token should be written before the download"
        );

        let download_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/admin/backup/download/temp-board/{download_filename}?cleanup=1&token={token}"
                    ))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("download response");

        assert_eq!(download_response.status(), StatusCode::OK);
        let expected_content_disposition = format!("attachment; filename=\"{download_filename}\"");
        assert_eq!(
            download_response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some(expected_content_disposition.as_str())
        );
        let body = to_bytes(download_response.into_body(), usize::MAX)
            .await
            .expect("download body");
        assert!(!body.is_empty());
        assert!(
            !download_path.exists(),
            "temp-board archive should be removed after cleanup stream is consumed"
        );
        assert!(
            !temp_board_download_token_path(download_filename).exists(),
            "temp-board download token should be consumed"
        );
    }

    #[test]
    fn board_restore_rejects_invalid_access_mode() {
        let source_pool = crate::db::init_test_pool().expect("source pool");
        let source_conn = source_pool.get().expect("source conn");
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .expect("create source board");
        let mut manifest = build_board_backup_manifest(&source_conn, "tech").expect("manifest");
        manifest.board.access_mode = "definitely_not_valid".to_string();

        let target_pool = crate::db::init_test_pool().expect("target pool");
        let mut target_conn = target_pool.get().expect("target conn");
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let error = execute_board_restore(
            &mut target_conn,
            upload_dir.path().to_str().expect("upload dir path"),
            manifest,
            |_| Ok(()),
            "Test invalid access mode restore",
            "Test invalid access mode restore completed",
        )
        .expect_err("restore should reject invalid access mode");

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("invalid access mode"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn board_restore_rejects_protected_board_without_password_hash() {
        let source_pool = crate::db::init_test_pool().expect("source pool");
        let source_conn = source_pool.get().expect("source conn");
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .expect("create source board");
        let mut manifest = build_board_backup_manifest(&source_conn, "tech").expect("manifest");
        manifest.board.access_mode = "view_password".to_string();
        manifest.board.access_password_hash.clear();

        let target_pool = crate::db::init_test_pool().expect("target pool");
        let mut target_conn = target_pool.get().expect("target conn");
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let error = execute_board_restore(
            &mut target_conn,
            upload_dir.path().to_str().expect("upload dir path"),
            manifest,
            |_| Ok(()),
            "Test missing access hash restore",
            "Test missing access hash restore completed",
        )
        .expect_err("restore should reject protected board without password hash");

        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("password hash"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_layout_accepts_full_backup_archive() {
        let archive = zip_with_entries(&[("chan.db", b"SQLite format 3\0stub")]);
        assert!(validate_full_restore_archive_layout(&archive).is_ok());
    }

    #[test]
    fn full_restore_layout_rejects_board_backup_archive_with_helpful_hint() {
        let archive = zip_with_entries(&[("board.json", br#"{"version":1}"#)]);
        let error = validate_full_restore_archive_layout(&archive).expect_err("should fail");
        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("board backup"));
                assert!(message.contains("Board restore"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn refresh_live_site_state_from_db_updates_banner_caches() {
        crate::templates::set_live_site_name("Before restore");
        crate::templates::set_live_site_subtitle("before subtitle");

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db conn");
        crate::db::set_site_setting(&conn, "site_name", "RestoredChan").expect("set site name");
        crate::db::set_site_setting(&conn, "site_subtitle", "restored subtitle")
            .expect("set subtitle");

        refresh_live_site_state_from_db(&conn).expect("refresh live site state");

        assert_eq!(&*crate::templates::live_site_name(), "RestoredChan");
        assert_eq!(
            &*crate::templates::live_site_subtitle(),
            "restored subtitle"
        );
    }

    #[test]
    fn temp_board_download_token_is_one_time_use() {
        let filename = "rustchan-board-test-20990101_000000.zip";
        let token = "token-123";
        let token_path = temp_board_download_token_path(filename);
        let _ = std::fs::remove_file(&token_path);

        write_temp_board_download_token(filename, token).expect("write token");
        assert!(consume_temp_board_download_token(filename, token).expect("consume token"));
        assert!(!consume_temp_board_download_token(filename, token).expect("token removed"));
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

    fn write_sample_full_backup_zip_at(zip_path: &std::path::Path, indexed_boards: bool) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("snapshot.db");

        let pool = crate::db::init_test_pool().expect("test pool");
        {
            let conn = pool.get().expect("db conn");
            crate::db::create_board(&conn, "tech", "Technology", "", false).expect("create board");
            let board = crate::db::get_board_by_short(&conn, "tech")
                .expect("get board")
                .expect("board exists");
            let post = crate::db::NewPost {
                thread_id: 0,
                board_id: board.id,
                name: "anon".into(),
                tripcode: None,
                subject: Some("backup test".into()),
                body: "hello".into(),
                body_html: "hello".into(),
                ip_hash: Some("hash".into()),
                file_path: Some("tech/hello.txt".into()),
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
            .expect("create thread");

            let db_path_str = db_path.to_str().expect("db path").replace('\'', "''");
            conn.execute_batch(&format!("VACUUM INTO '{db_path_str}'"))
                .expect("vacuum into snapshot");
        }

        let manifest = super::common::FullBackupManifest {
            version: if indexed_boards { 2 } else { 1 },
            generated_at: 1_700_000_000,
            rustchan_version: "1.1.3".into(),
            db_bytes: std::fs::metadata(&db_path).expect("db meta").len(),
            upload_file_count: 1,
            favicon_file_count: 0,
            banner_file_count: 0,
            tor_hidden_service_keys_included: false,
            tor_hidden_service_key_file_count: 0,
            boards: if indexed_boards {
                vec![BackupBoardSummary {
                    short_name: "tech".into(),
                    name: "Technology".into(),
                }]
            } else {
                Vec::new()
            },
        };
        let manifest_json = serde_json::to_vec(&manifest).expect("manifest json");
        let db_bytes = std::fs::read(&db_path).expect("read db");

        {
            let file = std::fs::File::create(zip_path).expect("zip file");
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(super::common::FULL_BACKUP_MANIFEST_NAME, options)
                .expect("start manifest");
            zip.write_all(&manifest_json).expect("write manifest");
            zip.start_file("chan.db", options).expect("start db");
            zip.write_all(&db_bytes).expect("write db");
            zip.start_file("uploads/tech/hello.txt", options)
                .expect("start upload");
            zip.write_all(b"hello").expect("write upload");
            zip.finish().expect("finish zip");
        }
    }

    fn build_sample_full_backup_zip(indexed_boards: bool) -> std::path::PathBuf {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("full.zip");
        write_sample_full_backup_zip_at(&zip_path, indexed_boards);
        let persisted = temp_dir.keep();
        persisted.join("full.zip")
    }

    #[test]
    fn prune_full_backup_dir_to_limit_removes_oldest_saved_backups() {
        let dir = tempfile::tempdir().expect("tempdir");
        let oldest = dir.path().join("rustchan-backup-20260101_000000.zip");
        let middle = dir.path().join("rustchan-backup-20260102_000000.zip");
        let newest = dir.path().join("rustchan-backup-20260103_000000.zip");
        write_sample_full_backup_zip_at(&oldest, true);
        write_sample_full_backup_zip_at(&middle, true);
        write_sample_full_backup_zip_at(&newest, true);

        let removed = super::prune_full_backup_dir_to_limit(dir.path(), 2).expect("prune");

        assert_eq!(
            removed,
            vec!["rustchan-backup-20260101_000000.zip".to_string()]
        );
        assert!(!oldest.exists());
        assert!(middle.exists());
        assert!(newest.exists());
    }

    #[test]
    fn latest_verified_full_backup_modified_time_ignores_newer_invalid_zip() {
        let backup_dir = tempfile::tempdir().expect("tempdir");
        let valid_path = backup_dir
            .path()
            .join("rustchan-backup-20990101_000001-valid.zip");
        let invalid_path = backup_dir
            .path()
            .join("rustchan-backup-20990101_000002-invalid.zip");

        write_sample_full_backup_zip_at(&valid_path, true);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&invalid_path, b"not a zip archive").expect("write invalid zip");

        let modified = latest_verified_full_backup_modified_time_in_dir(backup_dir.path())
            .expect("verified backup time");
        let valid_modified = std::fs::metadata(&valid_path)
            .expect("valid metadata")
            .modified()
            .expect("valid mtime");

        assert_eq!(modified, valid_modified);
    }

    #[test]
    fn full_backup_can_extract_board_backup() {
        let zip_path = build_sample_full_backup_zip(true);
        let (board_zip_path, filename) =
            create_temp_board_backup_from_full_backup_path(&zip_path, "tech")
                .expect("extract board backup");

        assert!(filename.contains("from-full"));
        let manifest =
            super::common::verify_board_backup_zip(&board_zip_path).expect("verify board zip");
        assert_eq!(manifest.board.short_name, "tech");

        let file = std::fs::File::open(&board_zip_path).expect("open board zip");
        let mut archive = zip::ZipArchive::new(file).expect("archive");
        assert!(archive.by_name("uploads/tech/hello.txt").is_ok());

        let _ = std::fs::remove_file(board_zip_path);
        let _ = std::fs::remove_file(zip_path);
    }

    #[test]
    fn older_full_backup_without_board_index_still_extracts_board_backup() {
        let zip_path = build_sample_full_backup_zip(false);
        let (board_zip_path, _) = create_temp_board_backup_from_full_backup_path(&zip_path, "tech")
            .expect("extract board backup from legacy full backup");

        let manifest =
            super::common::verify_board_backup_zip(&board_zip_path).expect("verify board zip");
        assert_eq!(manifest.board.short_name, "tech");

        let _ = std::fs::remove_file(board_zip_path);
        let _ = std::fs::remove_file(zip_path);
    }

    #[test]
    fn board_restore_preserves_original_post_ids_when_they_are_free() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let upload_dir = tempfile::tempdir().expect("upload dir");
        let mut conn = pool.get().expect("db conn");

        crate::db::create_board(&conn, "tech", "Technology", "", false).expect("create board");
        let tech_board = crate::db::get_board_by_short(&conn, "tech")
            .expect("load board")
            .expect("tech board");

        let (thread_id, op_post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            tech_board.id,
            Some("quoted thread"),
            &sample_post(tech_board.id, 0, "op body", true),
            "",
            None,
            None,
        )
        .expect("create thread");
        let reply_body = format!(">>{op_post_id}\nreply body");
        let reply_post_id = crate::db::create_reply_with_thread_update(
            &conn,
            &sample_post(tech_board.id, thread_id, &reply_body, false),
            "",
            true,
            None,
        )
        .expect("create reply");

        let manifest = build_board_backup_manifest(&conn, "tech").expect("build manifest");
        crate::db::delete_board(&conn, tech_board.id).expect("delete board");

        crate::db::create_board(&conn, "b", "Random", "", false).expect("create other board");
        let other_board = crate::db::get_board_by_short(&conn, "b")
            .expect("load other board")
            .expect("other board");
        let (_, other_post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn,
            other_board.id,
            Some("other thread"),
            &sample_post(other_board.id, 0, "other post", true),
            "",
            None,
            None,
        )
        .expect("create other thread");
        assert!(other_post_id > reply_post_id);

        execute_board_restore(
            &mut conn,
            upload_dir.path().to_str().expect("upload dir path"),
            manifest,
            |_| Ok(()),
            "Test board restore",
            "Test board restore completed",
        )
        .expect("restore board");

        let restored_op = crate::db::get_post_on_board(&conn, "tech", op_post_id)
            .expect("load restored op")
            .expect("restored op exists");
        let restored_reply = crate::db::get_post_on_board(&conn, "tech", reply_post_id)
            .expect("load restored reply")
            .expect("restored reply exists");

        assert_eq!(restored_op.id, op_post_id);
        assert_eq!(restored_op.thread_id, thread_id);
        assert_eq!(restored_reply.id, reply_post_id);
        assert_eq!(restored_reply.thread_id, thread_id);
        assert_eq!(restored_reply.body, reply_body);
        assert!(restored_reply
            .body_html
            .contains(&format!("data-pid=\"{op_post_id}\"")));
        assert!(crate::db::get_post_on_board(&conn, "b", other_post_id)
            .expect("load other post")
            .is_some());
    }

    #[test]
    fn board_restore_preserves_free_ids_above_target_sequence() {
        let source_pool = crate::db::init_test_pool().expect("source pool");
        let source_conn = source_pool.get().expect("source conn");
        crate::db::create_board(&source_conn, "pad", "Padding", "", false).expect("create pad");
        let pad_board = crate::db::get_board_by_short(&source_conn, "pad")
            .expect("load pad")
            .expect("pad board");
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
            .expect("create padding thread");
        }
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .expect("create tech");
        let source_tech_board = crate::db::get_board_by_short(&source_conn, "tech")
            .expect("load source tech")
            .expect("source tech board");
        let (source_thread_id, source_op_id, _) = crate::db::create_thread_with_optional_poll(
            &source_conn,
            source_tech_board.id,
            Some("high ids"),
            &sample_post(source_tech_board.id, 0, "source op", true),
            "",
            None,
            None,
        )
        .expect("create source thread");
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
        .expect("create source reply");
        assert!(source_op_id > 5);

        let manifest = build_board_backup_manifest(&source_conn, "tech").expect("build manifest");

        let target_pool = crate::db::init_test_pool().expect("target pool");
        let mut target_conn = target_pool.get().expect("target conn");
        crate::db::create_board(&target_conn, "b", "Random", "", false).expect("create target b");
        let target_b = crate::db::get_board_by_short(&target_conn, "b")
            .expect("load target b")
            .expect("target b board");
        crate::db::create_thread_with_optional_poll(
            &target_conn,
            target_b.id,
            Some("low ids"),
            &sample_post(target_b.id, 0, "target op", true),
            "",
            None,
            None,
        )
        .expect("create target thread");

        let upload_dir = tempfile::tempdir().expect("upload dir");
        execute_board_restore(
            &mut target_conn,
            upload_dir.path().to_str().expect("upload dir path"),
            manifest,
            |_| Ok(()),
            "Test board restore high ids",
            "Test board restore high ids completed",
        )
        .expect("restore board with high ids");

        let restored_op = crate::db::get_post_on_board(&target_conn, "tech", source_op_id)
            .expect("load restored op")
            .expect("restored op exists");
        let restored_reply = crate::db::get_post_on_board(&target_conn, "tech", source_reply_id)
            .expect("load restored reply")
            .expect("restored reply exists");
        assert_eq!(restored_op.thread_id, source_thread_id);
        assert_eq!(restored_reply.thread_id, source_thread_id);

        let restored_board = crate::db::get_board_by_short(&target_conn, "tech")
            .expect("load restored board")
            .expect("restored board");
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
        .expect("create post after restore");
        assert!(new_post_id > source_reply_id);
    }

    #[test]
    fn board_restore_fallback_remaps_same_board_crosslinks_when_ids_collide() {
        let source_pool = crate::db::init_test_pool().expect("source pool");
        let source_conn = source_pool.get().expect("source conn");
        crate::db::create_board(&source_conn, "tech", "Technology", "", false)
            .expect("create source tech");
        let source_board = crate::db::get_board_by_short(&source_conn, "tech")
            .expect("load source tech")
            .expect("source tech board");
        let (source_thread_id, source_op_id, _) = crate::db::create_thread_with_optional_poll(
            &source_conn,
            source_board.id,
            Some("crosslinks"),
            &sample_post(source_board.id, 0, "source op", true),
            "",
            None,
            None,
        )
        .expect("create source thread");
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
        .expect("create source reply");
        assert_eq!(source_op_id, 1);
        assert_eq!(source_reply_id, 2);

        let manifest = build_board_backup_manifest(&source_conn, "tech").expect("build manifest");

        let target_pool = crate::db::init_test_pool().expect("target pool");
        let mut target_conn = target_pool.get().expect("target conn");
        crate::db::create_board(&target_conn, "b", "Random", "", false).expect("create b");
        let target_board = crate::db::get_board_by_short(&target_conn, "b")
            .expect("load b")
            .expect("target board");
        let (existing_thread_id, existing_op_id, _) = crate::db::create_thread_with_optional_poll(
            &target_conn,
            target_board.id,
            Some("existing"),
            &sample_post(target_board.id, 0, "existing op", true),
            "",
            None,
            None,
        )
        .expect("create existing thread");
        let existing_reply_id = crate::db::create_reply_with_thread_update(
            &target_conn,
            &sample_post(target_board.id, existing_thread_id, "existing reply", false),
            "",
            true,
            None,
        )
        .expect("create existing reply");
        assert_eq!((existing_op_id, existing_reply_id), (1, 2));

        let upload_dir = tempfile::tempdir().expect("upload dir");
        execute_board_restore(
            &mut target_conn,
            upload_dir.path().to_str().expect("upload dir path"),
            manifest,
            |_| Ok(()),
            "Test board restore remap",
            "Test board restore remap completed",
        )
        .expect("restore board with collisions");

        let restored_board_id: i64 = target_conn
            .query_row(
                "SELECT id FROM boards WHERE short_name = 'tech'",
                [],
                |row| row.get(0),
            )
            .expect("load restored board id");
        let restored_op_id: i64 = target_conn
            .query_row(
                "SELECT id FROM posts WHERE board_id = ?1 AND is_op = 1",
                params![restored_board_id],
                |row| row.get(0),
            )
            .expect("load restored op id");
        let (restored_reply_id, restored_body, restored_body_html): (i64, String, String) =
            target_conn
                .query_row(
                    "SELECT id, body, body_html
                     FROM posts
                     WHERE board_id = ?1 AND is_op = 0",
                    params![restored_board_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("load restored reply");

        assert!(restored_op_id > source_op_id);
        assert!(restored_reply_id > source_reply_id);
        assert!(restored_body.contains(&format!(">>{restored_op_id}")));
        assert!(restored_body.contains(&format!(">>>/tech/{restored_op_id}")));
        assert!(restored_body.contains(">>>/b/1"));
        assert!(
            restored_body_html.contains(&format!("data-pid=\"{restored_op_id}\"")),
            "same-board quotelink should point at remapped post id"
        );
        assert!(
            restored_body_html.contains(&format!("/tech/post/{restored_op_id}")),
            "same-board crosslink should point at remapped post id"
        );
        assert!(restored_body_html.contains("/b/post/1"));
    }
}
