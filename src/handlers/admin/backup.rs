// handlers/admin/backup.rs
//
// Backup and restore subsystem for the admin panel.
// Covers full-site backups, board-level backups, streaming downloads,
// saved-backup restoration, and live board.json restore.

use crate::{
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

mod common;
mod create;
mod types;

use common::{
    copy_limited, create_staging_dir, extract_uploads_to_dir, log_backup_phase,
    log_backup_progress, read_limited_bytes, remap_body_quotelinks, remove_path_if_exists,
    render_restored_body_html, validate_board_short_name, BOARD_MANIFEST_MAX_BYTES,
    ZIP_ENTRY_MAX_BYTES,
};
pub use create::*;
use types::board_backup_types;

const BACKUP_LIST_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct BackupListCacheEntry {
    generated_at: Instant,
    dir_modified: Option<SystemTime>,
    files: Vec<BackupInfo>,
}

static BACKUP_LIST_CACHE: LazyLock<parking_lot::Mutex<HashMap<String, BackupListCacheEntry>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

pub async fn backup_request_logging_middleware(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    if uri.path() == "/admin/backup/progress" {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let response = next.run(req).await;
    let status = response.status();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        method = %method,
        uri = %uri,
        status = status.as_u16(),
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        "Admin backup request completed"
    );

    response
}

// ─── URL query-string encoder ─────────────────────────────────────────────────
//
// `encode_q` was previously defined as an inner function inside two
// separate async handlers, with one implementation slightly less efficient than
// the other. Extracted here so both call sites share a single definition.
//
// Encodes `s` using percent-encoding, with spaces as `+` (form-URL encoding).
// Used only for error messages in redirect query strings — not for URL paths.
fn encode_q(s: &str) -> String {
    const fn nibble(n: u8) -> char {
        match n {
            0 => '0',
            1 => '1',
            2 => '2',
            3 => '3',
            4 => '4',
            5 => '5',
            6 => '6',
            7 => '7',
            8 => '8',
            9 => '9',
            10 => 'A',
            11 => 'B',
            12 => 'C',
            13 => 'D',
            14 => 'E',
            _ => 'F',
        }
    }
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            b => {
                out.push('%');
                out.push(nibble(b >> 4));
                out.push(nibble(b & 0xf));
            }
        }
    }
    out
}

fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

fn admin_xhr_error_response(error: &AppError) -> Response {
    let handled = match error {
        AppError::NotFound(message) => Some((StatusCode::NOT_FOUND, message.clone())),
        AppError::BadRequest(message) => Some((StatusCode::BAD_REQUEST, message.clone())),
        AppError::Forbidden(message) => Some((StatusCode::FORBIDDEN, message.clone())),
        AppError::BannedUser { reason, .. } => Some((
            StatusCode::FORBIDDEN,
            format!("You are banned. Reason: {reason}"),
        )),
        AppError::UploadTooLarge(message) => Some((StatusCode::PAYLOAD_TOO_LARGE, message.clone())),
        AppError::InvalidMediaType(message) => {
            Some((StatusCode::UNSUPPORTED_MEDIA_TYPE, message.clone()))
        }
        AppError::Conflict(message) => Some((StatusCode::CONFLICT, message.clone())),
        AppError::RateLimited => Some((
            StatusCode::TOO_MANY_REQUESTS,
            "You are posting too fast. Slow down.".to_string(),
        )),
        AppError::DbBusy => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is temporarily busy. Please try again in a moment.".to_string(),
        )),
        AppError::Internal(error) => {
            tracing::error!("Internal admin restore XHR error: {:?}", error);
            None
        }
        AppError::Api {
            status,
            detail,
            endpoint,
        } => {
            tracing::error!(
                status,
                endpoint = endpoint.as_deref().unwrap_or("unknown"),
                "API error during admin restore XHR request: {detail}",
            );
            None
        }
        AppError::Tls(message) => {
            tracing::error!("TLS admin restore XHR error: {message}");
            None
        }
    };

    if let Some((status, message)) = handled {
        return crate::handlers::board::xhr_handled_error_response(status, &message)
            .unwrap_or_else(|response_error| response_error.into_response());
    }

    let (status, message) = match error {
        AppError::Internal(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        AppError::Api { detail, .. } => (StatusCode::BAD_GATEWAY, detail.clone()),
        AppError::Tls(message) => (StatusCode::INTERNAL_SERVER_ERROR, message.clone()),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unexpected admin restore error.".to_string(),
        ),
    };

    crate::handlers::board::xhr_error_response(status, &message)
        .unwrap_or_else(|response_error| response_error.into_response())
}

fn validate_board_backup_access_settings(
    manifest: &mut board_backup_types::BoardBackupManifest,
) -> Result<()> {
    let access_mode =
        BoardAccessMode::from_db_str(&manifest.board.access_mode).ok_or_else(|| {
            AppError::BadRequest("Board backup contains an invalid access mode.".into())
        })?;
    manifest.board.access_mode = access_mode.as_str().to_string();

    if access_mode.requires_post_password() && manifest.board.access_password_hash.is_empty() {
        return Err(AppError::BadRequest(
            "Protected board backups must include a password hash.".into(),
        ));
    }

    if !manifest.board.access_password_hash.is_empty()
        && verify_password(
            "__rustchan_board_access_probe__",
            &manifest.board.access_password_hash,
        )
        .is_err()
    {
        return Err(AppError::BadRequest(
            "Board backup contains an invalid board password hash.".into(),
        ));
    }

    Ok(())
}

fn redirect_page_response(target: &str, message: &str) -> Response {
    let escaped_target = crate::utils::sanitize::escape_html(target);
    let escaped_message = crate::utils::sanitize::escape_html(message);
    let body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="0;url={escaped_target}">
<title>Redirecting</title>
</head>
<body>
<p>{escaped_message}</p>
<p><a href="{escaped_target}">Continue</a></p>
</body>
</html>"#
    );

    let mut resp = Response::new(axum::body::Body::from(body));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::HeaderName::from_static("refresh"),
        HeaderValue::from_str(&format!("0; url={target}"))
            .unwrap_or_else(|_| HeaderValue::from_static("0; url=/admin/panel")),
    );
    resp
}

#[derive(Clone, Copy)]
enum RestoreKind {
    Full,
    Board,
}

impl RestoreKind {
    const fn title(self) -> &'static str {
        match self {
            Self::Full => "Full restore",
            Self::Board => "Board restore",
        }
    }

    const fn route(self) -> &'static str {
        match self {
            Self::Full => "/admin/restore",
            Self::Board => "/admin/board/restore",
        }
    }

    const fn maintenance_label(self) -> &'static str {
        match self {
            Self::Full => "Full restore",
            Self::Board => "Board restore",
        }
    }

    const fn start_failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore could not start.",
            Self::Board => "Board restore could not start.",
        }
    }

    const fn upload_failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore upload failed.",
            Self::Board => "Board restore upload failed.",
        }
    }

    const fn failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore failed.",
            Self::Board => "Board restore failed.",
        }
    }
}

struct StreamedRestoreUpload {
    temp_file: tempfile::NamedTempFile,
    form_csrf: Option<String>,
    uploaded_filename: Option<String>,
    uploaded_content_type: Option<String>,
    uploaded_bytes: u64,
}

fn restore_error_redirect_target(message: &str) -> String {
    format!("/admin/panel?restore_error={}", encode_q(message))
}

fn restore_start_response(
    kind: RestoreKind,
    xhr_request: bool,
    error: &impl std::fmt::Display,
) -> Response {
    if xhr_request {
        return crate::handlers::board::xhr_handled_error_response(
            StatusCode::CONFLICT,
            &error.to_string(),
        )
        .unwrap_or_else(|response_error| response_error.into_response());
    }
    redirect_page_response(
        &restore_error_redirect_target(&error.to_string()),
        kind.start_failure_message(),
    )
}

fn restore_upload_parse_response(
    kind: RestoreKind,
    xhr_request: bool,
    error: &impl std::fmt::Display,
) -> Response {
    let message = format!("Upload parsing failed: {error}");
    if xhr_request {
        return crate::handlers::board::xhr_handled_error_response(
            StatusCode::BAD_REQUEST,
            &message,
        )
        .unwrap_or_else(|response_error| response_error.into_response());
    }
    redirect_page_response(
        &restore_error_redirect_target(&message),
        kind.upload_failure_message(),
    )
}

fn restore_failure_response(kind: RestoreKind, xhr_request: bool, error: &AppError) -> Response {
    if xhr_request {
        return admin_xhr_error_response(error);
    }
    redirect_page_response(
        &restore_error_redirect_target(&error.to_string()),
        kind.failure_message(),
    )
}

fn log_restore_upload_started(kind: RestoreKind, headers: &HeaderMap, jar: &CookieJar) {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        route = kind.route(),
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        has_session_cookie = jar.get(super::SESSION_COOKIE).is_some(),
        has_csrf_cookie = jar.get("csrf_token").is_some(),
        "{} upload started",
        kind.title()
    );
}

async fn restore_auth_preflight(
    state: &AppState,
    headers: &HeaderMap,
    jar: &CookieJar,
) -> Result<Option<String>> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(headers)?;

    {
        let pool = state.db.clone();
        let session_id_for_task = session_id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id_for_task.as_deref())?;
            Ok(())
        })
        .await
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Admin auth preflight task failed: {error}"))
        })??;
    }

    Ok(session_id)
}

async fn stream_restore_upload_to_tempfile(
    kind: RestoreKind,
    multipart: &mut Multipart,
) -> Result<StreamedRestoreUpload> {
    let mut temp_file: Option<tempfile::NamedTempFile> = None;
    let mut form_csrf: Option<String> = None;
    let mut uploaded_filename: Option<String> = None;
    let mut uploaded_content_type: Option<String> = None;
    let mut uploaded_bytes = 0u64;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("Multipart error: {error}")))?
    {
        let field_name = field.name().unwrap_or("<unnamed>").to_string();
        match field.name() {
            Some("_csrf") => {
                tracing::debug!(
                    target: "admin",
                    route = kind.route(),
                    field = "_csrf",
                    "{} received CSRF field",
                    kind.title()
                );
                form_csrf = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| AppError::BadRequest(error.to_string()))?,
                );
            }
            Some("backup_file") => {
                uploaded_filename = field.file_name().map(str::to_string);
                uploaded_content_type = field.content_type().map(str::to_string);
                tracing::info!(
                    target: "admin",
                    route = kind.route(),
                    field = field_name,
                    filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                    mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
                    "{} received backup file field",
                    kind.title()
                );
                let tmp = tempfile::NamedTempFile::new()
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Tempfile: {error}")))?;
                let std_clone = tmp
                    .as_file()
                    .try_clone()
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Clone fd: {error}")))?;
                let async_file = tokio::fs::File::from_std(std_clone);
                let mut writer = tokio::io::BufWriter::new(async_file);
                let mut field = field;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|error| AppError::BadRequest(error.to_string()))?
                {
                    uploaded_bytes = uploaded_bytes
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                    writer.write_all(&chunk).await.map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Write chunk: {error}"))
                    })?;
                }
                writer
                    .flush()
                    .await
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Flush: {error}")))?;
                temp_file = Some(tmp);
            }
            _ => {
                tracing::debug!(
                    target: "admin",
                    route = kind.route(),
                    field = field_name,
                    "{} ignored unexpected multipart field",
                    kind.title()
                );
                let _ = field.bytes().await;
            }
        }
    }

    let temp_file =
        temp_file.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;

    Ok(StreamedRestoreUpload {
        temp_file,
        form_csrf,
        uploaded_filename,
        uploaded_content_type,
        uploaded_bytes,
    })
}

fn validate_streamed_restore_upload(
    kind: RestoreKind,
    jar: &CookieJar,
    upload: &StreamedRestoreUpload,
) -> Result<u64> {
    let has_csrf_cookie = jar.get("csrf_token").is_some();
    if super::check_csrf_jar(jar, upload.form_csrf.as_deref()).is_err() {
        tracing::warn!(
            target: "admin",
            route = kind.route(),
            has_csrf_cookie,
            has_form_csrf = upload.form_csrf.is_some(),
            "{} failed CSRF validation",
            kind.title()
        );
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let file_size = upload
        .temp_file
        .as_file()
        .seek(std::io::SeekFrom::End(0))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Seek check: {error}")))?;
    if file_size == 0 {
        return Err(AppError::BadRequest(
            "Uploaded backup file is empty.".into(),
        ));
    }

    tracing::info!(
        target: "admin",
        route = kind.route(),
        filename = upload.uploaded_filename.as_deref().unwrap_or("<missing>"),
        mime = upload.uploaded_content_type.as_deref().unwrap_or("<missing>"),
        streamed_bytes = upload.uploaded_bytes,
        temp_file_size = file_size,
        "{} upload streamed to disk",
        kind.title()
    );

    Ok(file_size)
}

fn format_magic_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitize_backup_zip_filename(filename: &str) -> Result<String> {
    let safe_filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != filename
        || safe_filename.contains("..")
        || !Path::new(&safe_filename)
            .extension()
            .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    Ok(safe_filename)
}

fn sanitize_board_short_value(board_short: &str) -> Result<String> {
    let safe_board = board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if safe_board.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }
    Ok(safe_board)
}

pub(super) fn parse_board_backup_manifest_from_zip<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<board_backup_types::BoardBackupManifest> {
    if !archive.file_names().any(|name| name == "board.json") {
        return Err(AppError::BadRequest(
            "Invalid board backup: zip must contain 'board.json'. \
             (Did you upload a full-site backup instead?)"
                .into(),
        ));
    }

    let mut entry = archive
        .by_name("board.json")
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Read board.json: {error}")))?;
    let buf = read_limited_bytes(&mut entry, BOARD_MANIFEST_MAX_BYTES, "board.json manifest")?;
    serde_json::from_slice(&buf)
        .map_err(|error| AppError::BadRequest(format!("Invalid board.json: {error}")))
}

fn validate_full_restore_archive_layout<R: std::io::Read + std::io::Seek>(
    archive: &zip::ZipArchive<R>,
) -> Result<()> {
    if archive.file_names().any(|name| name == "chan.db") {
        return Ok(());
    }

    if archive.file_names().any(|name| name == "board.json") {
        return Err(AppError::BadRequest(
            "Invalid full backup: zip must contain 'chan.db' at the root. \
             This archive looks like a board backup; use Board restore instead."
                .into(),
        ));
    }

    Err(AppError::BadRequest(
        "Invalid full backup: zip must contain 'chan.db' at the root.".into(),
    ))
}

fn run_restore_db_quick_check(
    conn: &rusqlite::Connection,
    restore_label: &str,
    board_short: &str,
) -> Result<()> {
    let result: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "{restore_label}: run DB quick_check for /{board_short}/: {error}"
            ))
        })?;

    if result.eq_ignore_ascii_case("ok") {
        return Ok(());
    }

    Err(AppError::Internal(anyhow::anyhow!(
        "{restore_label}: live database integrity check failed before modifying /{board_short}/: \
         {result}. The live DB appears corrupted; board restore was aborted before deleting data."
    )))
}

fn map_board_restore_sqlite_error(
    restore_label: &str,
    board_short: &str,
    context: &str,
    error: rusqlite::Error,
) -> AppError {
    let message = error.to_string();
    if message.contains("database disk image is malformed")
        || matches!(
            error,
            rusqlite::Error::SqliteFailure(ref inner, _)
                if inner.code == rusqlite::ErrorCode::DatabaseCorrupt
                    || inner.code == rusqlite::ErrorCode::NotADatabase
        )
    {
        AppError::Internal(anyhow::anyhow!(
            "{restore_label}: {context} failed while replacing /{board_short}/: {message}. \
             The live database appears corrupted. Restore was aborted before the backup could be applied."
        ))
    } else {
        AppError::Internal(anyhow::anyhow!("{context}: {message}"))
    }
}

fn insert_returning_id<P>(
    conn: &rusqlite::Connection,
    sql: &str,
    params: P,
) -> std::result::Result<i64, rusqlite::Error>
where
    P: rusqlite::Params,
{
    conn.query_row(sql, params, |row| row.get(0))
}

fn can_reuse_row_ids<I>(conn: &rusqlite::Connection, table: &'static str, ids: I) -> Result<bool>
where
    I: IntoIterator<Item = i64>,
{
    debug_assert!(matches!(
        table,
        "threads" | "posts" | "polls" | "poll_options"
    ));
    let sql = format!("SELECT 1 FROM {table} WHERE id = ?1 LIMIT 1");
    let mut stmt = conn.prepare_cached(&sql).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Prepare {table} ID probe: {error}"))
    })?;
    for id in ids {
        let exists = stmt.exists(params![id]).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Probe {table} ID {id} during restore: {error}"
            ))
        })?;
        if exists {
            return Ok(false);
        }
    }
    Ok(true)
}

fn sync_autoincrement_sequence(
    conn: &rusqlite::Connection,
    table: &'static str,
    max_id: Option<i64>,
) -> Result<()> {
    debug_assert!(matches!(
        table,
        "threads" | "posts" | "polls" | "poll_options"
    ));
    let Some(max_id) = max_id else {
        return Ok(());
    };

    let current_seq: Option<i64> = conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name = ?1",
            params![table],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read sqlite_sequence for {table} during restore: {error}"
            ))
        })?;

    match current_seq {
        Some(seq) if seq >= max_id => Ok(()),
        Some(_) => {
            conn.execute(
                "UPDATE sqlite_sequence SET seq = ?2 WHERE name = ?1",
                params![table, max_id],
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Advance sqlite_sequence for {table} during restore: {error}"
                ))
            })?;
            Ok(())
        }
        None => {
            conn.execute(
                "INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)",
                params![table, max_id],
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Insert sqlite_sequence for {table} during restore: {error}"
                ))
            })?;
            Ok(())
        }
    }
}

fn insert_or_validate_restored_file_hash(
    conn: &rusqlite::Connection,
    file_hash: &board_backup_types::FileHashRow,
) -> Result<()> {
    match conn.execute(
        "INSERT INTO file_hashes
         (sha256, file_path, thumb_path, mime_type, created_at)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            file_hash.sha256,
            file_hash.file_path,
            file_hash.thumb_path,
            file_hash.mime_type,
            file_hash.created_at
        ],
    ) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(inner, _))
            if inner.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            let existing: Option<(String, String, String, i64)> = conn
                .query_row(
                    "SELECT file_path, thumb_path, mime_type, created_at
                     FROM file_hashes
                     WHERE sha256 = ?1",
                    params![file_hash.sha256],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!(
                        "Read existing file_hash {}: {error}",
                        file_hash.sha256
                    ))
                })?;

            let Some((file_path, thumb_path, mime_type, created_at)) = existing else {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "File hash {} hit a uniqueness error but could not be reloaded",
                    file_hash.sha256
                )));
            };

            if file_path == file_hash.file_path
                && thumb_path == file_hash.thumb_path
                && mime_type == file_hash.mime_type
                && created_at == file_hash.created_at
            {
                Ok(())
            } else {
                Err(AppError::Internal(anyhow::anyhow!(
                    "Restore file_hash collision for sha256 {}: existing row points to different media",
                    file_hash.sha256
                )))
            }
        }
        Err(error) => Err(AppError::Internal(anyhow::anyhow!(
            "Insert file_hash {}: {error}",
            file_hash.sha256
        ))),
    }
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn execute_board_restore<F>(
    conn: &mut rusqlite::Connection,
    upload_dir: &str,
    mut manifest: board_backup_types::BoardBackupManifest,
    mut extract_uploads: F,
    restore_label: &str,
    completion_log: &str,
) -> Result<String>
where
    F: FnMut(&Path) -> Result<()>,
{
    use std::collections::HashMap;

    let board_short = manifest.board.short_name.clone();
    validate_board_short_name(&board_short)?;
    validate_board_backup_access_settings(&mut manifest)?;
    let upload_root = PathBuf::from(upload_dir);
    let staged_upload_root = create_staging_dir(&upload_root, "board-restore-stage")?;
    extract_uploads(&staged_upload_root)?;
    let staged_board_dir = staged_upload_root.join(&board_short);
    std::fs::create_dir_all(&staged_board_dir)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Create staged board dir: {error}")))?;
    let live_board_dir = upload_root.join(&board_short);
    let previous_board_dir = upload_root.join(format!(
        ".{board_short}.restore-old.{}",
        uuid::Uuid::new_v4().simple()
    ));
    let pending_board_restore_id = uuid::Uuid::new_v4().to_string();
    let pending_board_restore_payload = crate::pending_fs::BoardRestoreSwapPayload {
        staged: staged_board_dir.display().to_string(),
        live: live_board_dir.display().to_string(),
        previous: previous_board_dir.display().to_string(),
    };
    let pending_board_restore_op = crate::pending_fs::PendingFsOpInsert {
        id: pending_board_restore_id.clone(),
        kind: crate::pending_fs::BOARD_RESTORE_SWAP_KIND,
        payload_json: serde_json::to_string(&pending_board_restore_payload).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Serialize board restore pending_fs_op payload: {error}"
            ))
        })?,
    };

    let existing_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM boards WHERE short_name = ?1",
            params![board_short],
            |row| row.get(0),
        )
        .ok();
    if existing_id.is_some() {
        run_restore_db_quick_check(conn, restore_label, &board_short)?;
    }
    let temp_dir = std::env::temp_dir();
    let db_snapshot = temp_dir.join(format!(
        "board_restore_live_before_{}.db",
        uuid::Uuid::new_v4().simple()
    ));
    let db_snapshot_str = db_snapshot
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Snapshot path is non-UTF-8")))?
        .replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{db_snapshot_str}'"))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Snapshot board DB: {error}")))?;
    conn.execute("BEGIN IMMEDIATE", [])
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Begin tx: {error}")))?;

    let restore_result = (|| -> Result<()> {
        let live_board_id: i64 = if let Some(existing_id) = existing_id {
            conn.execute(
                "DELETE FROM threads WHERE board_id = ?1",
                params![existing_id],
            )
            .map_err(|error| {
                map_board_restore_sqlite_error(restore_label, &board_short, "Clear threads", error)
            })?;
            conn.execute(
                "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                 max_threads=?4, max_archived_threads=?5, bump_limit=?6,
                 allow_images=?7, allow_video=?8, allow_audio=?9, allow_any_files=?10,
                allow_tripcodes=?11, edit_window_secs=?12, allow_editing=?13,
                 allow_archive=?14, allow_video_embeds=?15, allow_captcha=?16,
                 show_poster_ids=?17, collapse_greentext=?18, post_cooldown_secs=?19,
                 access_mode=?20, access_password_hash=?21
                 WHERE id=?22",
                params![
                    manifest.board.name,
                    manifest.board.description,
                    i64::from(manifest.board.nsfw),
                    manifest.board.max_threads,
                    manifest.board.max_archived_threads,
                    manifest.board.bump_limit,
                    i64::from(manifest.board.allow_images),
                    i64::from(manifest.board.allow_video),
                    i64::from(manifest.board.allow_audio),
                    i64::from(manifest.board.allow_any_files),
                    i64::from(manifest.board.allow_tripcodes),
                    manifest.board.edit_window_secs,
                    i64::from(manifest.board.allow_editing),
                    i64::from(manifest.board.allow_archive),
                    i64::from(manifest.board.allow_video_embeds),
                    i64::from(manifest.board.allow_captcha),
                    i64::from(manifest.board.show_poster_ids),
                    i64::from(manifest.board.collapse_greentext),
                    manifest.board.post_cooldown_secs,
                    manifest.board.access_mode,
                    manifest.board.access_password_hash,
                    existing_id,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Update board: {error}")))?;
            existing_id
        } else {
            insert_returning_id(
                conn,
                "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                 max_archived_threads, bump_limit, allow_images, allow_video, allow_audio, allow_any_files,
                 allow_tripcodes, edit_window_secs, allow_editing, allow_archive,
                 allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext,
                 post_cooldown_secs, access_mode, access_password_hash, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)
                 RETURNING id",
                params![
                    manifest.board.short_name,
                    manifest.board.name,
                    manifest.board.description,
                    i64::from(manifest.board.nsfw),
                    manifest.board.max_threads,
                    manifest.board.max_archived_threads,
                    manifest.board.bump_limit,
                    i64::from(manifest.board.allow_images),
                    i64::from(manifest.board.allow_video),
                    i64::from(manifest.board.allow_audio),
                    i64::from(manifest.board.allow_any_files),
                    i64::from(manifest.board.allow_tripcodes),
                    manifest.board.edit_window_secs,
                    i64::from(manifest.board.allow_editing),
                    i64::from(manifest.board.allow_archive),
                    i64::from(manifest.board.allow_video_embeds),
                    i64::from(manifest.board.allow_captcha),
                    i64::from(manifest.board.show_poster_ids),
                    i64::from(manifest.board.collapse_greentext),
                    manifest.board.post_cooldown_secs,
                    manifest.board.access_mode,
                    manifest.board.access_password_hash,
                    manifest.board.created_at,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Insert board: {error}")))?
        };

        let preserve_thread_ids = can_reuse_row_ids(
            conn,
            "threads",
            manifest.threads.iter().map(|thread| thread.id),
        )?;
        let preserve_post_ids =
            can_reuse_row_ids(conn, "posts", manifest.posts.iter().map(|post| post.id))?;
        let preserve_poll_ids =
            can_reuse_row_ids(conn, "polls", manifest.polls.iter().map(|poll| poll.id))?;
        let preserve_option_ids = can_reuse_row_ids(
            conn,
            "poll_options",
            manifest.poll_options.iter().map(|option| option.id),
        )?;

        let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
        for thread in &manifest.threads {
            let new_thread_id = if preserve_thread_ids {
                insert_returning_id(
                    conn,
                    "INSERT INTO threads (id, board_id, subject, created_at, bumped_at,
                     locked, sticky, archived, reply_count)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     RETURNING id",
                    params![
                        thread.id,
                        live_board_id,
                        thread.subject,
                        thread.created_at,
                        thread.bumped_at,
                        i64::from(thread.locked),
                        i64::from(thread.sticky),
                        i64::from(thread.archived),
                        thread.reply_count,
                    ],
                )
            } else {
                insert_returning_id(
                    conn,
                    "INSERT INTO threads (board_id, subject, created_at, bumped_at,
                     locked, sticky, archived, reply_count)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     RETURNING id",
                    params![
                        live_board_id,
                        thread.subject,
                        thread.created_at,
                        thread.bumped_at,
                        i64::from(thread.locked),
                        i64::from(thread.sticky),
                        i64::from(thread.archived),
                        thread.reply_count,
                    ],
                )
            }
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert thread {}: {error}", thread.id))
            })?;
            thread_id_map.insert(thread.id, new_thread_id);
        }
        if preserve_thread_ids {
            sync_autoincrement_sequence(
                conn,
                "threads",
                manifest.threads.iter().map(|thread| thread.id).max(),
            )?;
        }

        let mut post_id_map: HashMap<i64, i64> = HashMap::new();
        for post in &manifest.posts {
            let new_thread_id = *thread_id_map.get(&post.thread_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "Post {} refs unknown thread {}",
                    post.id,
                    post.thread_id
                ))
            })?;
            let new_post_id = if preserve_post_ids {
                insert_returning_id(
                    conn,
                    "INSERT INTO posts (id, thread_id, board_id, name, tripcode, subject,
                     body, body_html, ip_hash, file_path, file_name, file_size,
                     thumb_path, mime_type, media_type, created_at, deletion_token, is_op,
                     media_processing_state, media_processing_error)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
                     RETURNING id",
                    params![
                        post.id,
                        new_thread_id,
                        live_board_id,
                        post.name,
                        post.tripcode,
                        post.subject,
                        post.body,
                        render_restored_body_html(&post.body),
                        post.ip_hash,
                        post.file_path,
                        post.file_name,
                        post.file_size,
                        post.thumb_path,
                        post.mime_type,
                        post.media_type,
                        post.created_at,
                        post.deletion_token,
                        i64::from(post.is_op),
                        post.media_processing_state,
                        post.media_processing_error,
                    ],
                )
            } else {
                insert_returning_id(
                    conn,
                    "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                     body, body_html, ip_hash, file_path, file_name, file_size,
                     thumb_path, mime_type, media_type, created_at, deletion_token, is_op,
                     media_processing_state, media_processing_error)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
                     RETURNING id",
                    params![
                        new_thread_id,
                        live_board_id,
                        post.name,
                        post.tripcode,
                        post.subject,
                        post.body,
                        render_restored_body_html(&post.body),
                        post.ip_hash,
                        post.file_path,
                        post.file_name,
                        post.file_size,
                        post.thumb_path,
                        post.mime_type,
                        post.media_type,
                        post.created_at,
                        post.deletion_token,
                        i64::from(post.is_op),
                        post.media_processing_state,
                        post.media_processing_error,
                    ],
                )
            }
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert post {}: {error}", post.id))
            })?;
            post_id_map.insert(post.id, new_post_id);
        }
        if preserve_post_ids {
            sync_autoincrement_sequence(
                conn,
                "posts",
                manifest.posts.iter().map(|post| post.id).max(),
            )?;
        }

        let any_changed = post_id_map.iter().any(|(old, new)| old != new);
        if any_changed {
            let mut pairs: Vec<(String, String)> = post_id_map
                .iter()
                .filter(|(old, new)| old != new)
                .map(|(old, new)| (old.to_string(), new.to_string()))
                .collect();
            pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.0.cmp(&a.0)));

            for post in &manifest.posts {
                let Some(&new_post_id) = post_id_map.get(&post.id) else {
                    continue;
                };

                let new_body = remap_body_quotelinks(&post.body, &board_short, &pairs);
                let new_body_html = render_restored_body_html(&new_body);
                if new_body != post.body {
                    conn.execute(
                        "UPDATE posts SET body = ?1, body_html = ?2 WHERE id = ?3",
                        params![new_body, new_body_html, new_post_id],
                    )
                    .map_err(|error| {
                        AppError::Internal(anyhow::anyhow!(
                            "Fixup quotelinks for post {new_post_id}: {error}"
                        ))
                    })?;
                }
            }
        }

        let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
        for poll in &manifest.polls {
            let new_thread_id = *thread_id_map.get(&poll.thread_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "Poll {} refs unknown thread {}",
                    poll.id,
                    poll.thread_id
                ))
            })?;
            let new_poll_id = if preserve_poll_ids {
                insert_returning_id(
                    conn,
                    "INSERT INTO polls (id, thread_id, question, expires_at, created_at)
                     VALUES (?1,?2,?3,?4,?5)
                     RETURNING id",
                    params![
                        poll.id,
                        new_thread_id,
                        poll.question,
                        poll.expires_at,
                        poll.created_at
                    ],
                )
            } else {
                insert_returning_id(
                    conn,
                    "INSERT INTO polls (thread_id, question, expires_at, created_at)
                     VALUES (?1,?2,?3,?4)
                     RETURNING id",
                    params![
                        new_thread_id,
                        poll.question,
                        poll.expires_at,
                        poll.created_at
                    ],
                )
            }
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert poll {}: {error}", poll.id))
            })?;
            poll_id_map.insert(poll.id, new_poll_id);
        }
        if preserve_poll_ids {
            sync_autoincrement_sequence(
                conn,
                "polls",
                manifest.polls.iter().map(|poll| poll.id).max(),
            )?;
        }

        let mut option_id_map: HashMap<i64, i64> = HashMap::new();
        for option in &manifest.poll_options {
            let new_poll_id = *poll_id_map.get(&option.poll_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "Option {} refs unknown poll {}",
                    option.id,
                    option.poll_id
                ))
            })?;
            let new_option_id = if preserve_option_ids {
                insert_returning_id(
                    conn,
                    "INSERT INTO poll_options (id, poll_id, text, position)
                     VALUES (?1,?2,?3,?4)
                     RETURNING id",
                    params![option.id, new_poll_id, option.text, option.position],
                )
            } else {
                insert_returning_id(
                    conn,
                    "INSERT INTO poll_options (poll_id, text, position)
                     VALUES (?1,?2,?3)
                     RETURNING id",
                    params![new_poll_id, option.text, option.position],
                )
            }
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert option {}: {error}", option.id))
            })?;
            option_id_map.insert(option.id, new_option_id);
        }
        if preserve_option_ids {
            sync_autoincrement_sequence(
                conn,
                "poll_options",
                manifest.poll_options.iter().map(|option| option.id).max(),
            )?;
        }

        for vote in &manifest.poll_votes {
            let new_poll_id = *poll_id_map.get(&vote.poll_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "Vote {} refs unknown poll {}",
                    vote.id,
                    vote.poll_id
                ))
            })?;
            let new_option_id = *option_id_map.get(&vote.option_id).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!(
                    "Vote {} refs unknown option {}",
                    vote.id,
                    vote.option_id
                ))
            })?;
            conn.execute(
                "INSERT INTO poll_votes
                 (poll_id, option_id, ip_hash) VALUES (?1,?2,?3)",
                params![new_poll_id, new_option_id, vote.ip_hash],
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert vote {}: {error}", vote.id))
            })?;
        }

        for file_hash in &manifest.file_hashes {
            insert_or_validate_restored_file_hash(conn, file_hash)?;
        }

        db::insert_pending_fs_op(conn, &pending_board_restore_op)?;
        Ok(())
    })();

    match restore_result {
        Ok(()) => {
            conn.execute("COMMIT", [])
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Commit tx: {error}")))?;
            if let Err(error) =
                crate::pending_fs::finalize_board_restore_payload(&pending_board_restore_payload)
            {
                if let Err(restore_err) =
                    restore_db_from_snapshot(conn, &db_snapshot, restore_label)
                {
                    let _ = std::fs::remove_file(&db_snapshot);
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "{restore_label} filesystem swap failed: {error}; DB rollback error: {restore_err}"
                    )));
                }
                let _ = std::fs::remove_file(&db_snapshot);
                return Err(AppError::Internal(anyhow::anyhow!(
                    "{restore_label} filesystem swap failed: {error}"
                )));
            }
            db::delete_pending_fs_op(conn, &pending_board_restore_id)?;
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", []);
            let _ = remove_path_if_exists(&staged_upload_root);
            let _ = std::fs::remove_file(&db_snapshot);
            return Err(error);
        }
    }

    let _ = std::fs::remove_file(&db_snapshot);
    let _ = remove_path_if_exists(&staged_upload_root);

    tracing::info!(target: "admin", board = %board_short, "{completion_log}");
    if let Ok(boards) = db::get_all_boards(conn) {
        crate::templates::set_live_boards(boards);
    }
    Ok(board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect())
}

// Module-level constant so it can be referenced inside closures and
// loops without triggering the "item after statements" clippy lint.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

fn restore_db_from_snapshot(
    live_conn: &mut rusqlite::Connection,
    snapshot_path: &Path,
    context: &str,
) -> Result<()> {
    let src = rusqlite::Connection::open(snapshot_path).map_err(|restore_err| {
        AppError::Internal(anyhow::anyhow!(
            "{context}: open DB rollback snapshot {}: {restore_err}",
            snapshot_path.display()
        ))
    })?;
    let backup = Backup::new(&src, live_conn).map_err(|restore_err| {
        AppError::Internal(anyhow::anyhow!("{context}: rollback init: {restore_err}"))
    })?;
    backup
        .run_to_completion(100, std::time::Duration::from_millis(0), None)
        .map_err(|restore_err| {
            AppError::Internal(anyhow::anyhow!("{context}: rollback copy: {restore_err}"))
        })?;
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_full_restore<R: std::io::Read + std::io::Seek>(
    live_conn: &mut rusqlite::Connection,
    admin_id: i64,
    upload_dir: &str,
    archive: &mut zip::ZipArchive<R>,
    restore_label: &str,
    completion_log: &str,
    suspicious_entry_log: &str,
    session_warning_log: &str,
) -> Result<String> {
    validate_full_restore_archive_layout(archive)?;

    let temp_dir = std::env::temp_dir();
    let tmp_id = uuid::Uuid::new_v4().simple().to_string();
    let temp_db = temp_dir.join(format!("chan_restore_{tmp_id}.db"));
    let upload_root = PathBuf::from(upload_dir);
    let staged_upload_root = create_staging_dir(&upload_root, "restore-stage")?;
    let live_global_favicon_dir = crate::favicon::global_backup_source_dir();
    let staged_global_favicon_dir = create_staging_dir(&live_global_favicon_dir, "restore-stage")?;
    let mut favicon_extracted = false;
    let previous_upload_root = upload_root.parent().map_or_else(
        || PathBuf::from(format!("{}.restore-old", upload_root.display())),
        |parent| {
            parent.join(format!(
                ".{}.restore-old.{}",
                upload_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("uploads"),
                uuid::Uuid::new_v4().simple()
            ))
        },
    );
    let db_snapshot = temp_dir.join(format!("chan_restore_live_before_{tmp_id}.db"));
    let mut db_extracted = false;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip[{index}]: {error}")))?;
        let name = entry.name().to_string();
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            warn!("{suspicious_entry_log}: skipping suspicious entry '{name}'");
            continue;
        }

        if name == "chan.db" {
            let mut out = std::fs::File::create(&temp_db)
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Create temp DB: {error}")))?;
            copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Write temp DB: {error}")))?;

            let mut header = [0u8; 16];
            {
                use std::io::Read;
                let mut file = std::fs::File::open(&temp_db).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Magic check open: {error}"))
                })?;
                if file.read_exact(&mut header).is_err() {
                    let _ = std::fs::remove_file(&temp_db);
                    return Err(AppError::BadRequest(
                        "Uploaded chan.db is not a valid SQLite database (file too small).".into(),
                    ));
                }
            }
            if &header != SQLITE_HEADER {
                let _ = std::fs::remove_file(&temp_db);
                return Err(AppError::BadRequest(
                    "Uploaded chan.db is not a valid SQLite database (invalid magic bytes).".into(),
                ));
            }
            db_extracted = true;
        } else if let Some(rel) = name.strip_prefix("uploads/") {
            if rel.is_empty() {
                continue;
            }
            let rel_path = Path::new(rel);
            if rel_path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
            {
                warn!("{suspicious_entry_log}: skipping suspicious entry '{name}'");
                continue;
            }
            let target = staged_upload_root.join(rel_path);
            if entry.is_dir() {
                std::fs::create_dir_all(&target).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("mkdir {}: {error}", target.display()))
                })?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("mkdir parent: {error}"))
                    })?;
                }
                let mut out = std::fs::File::create(&target).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Create {}: {error}", target.display()))
                })?;
                copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
                })?;
            }
        } else if let Some(rel) = name.strip_prefix("favicon/") {
            if rel.is_empty() {
                continue;
            }
            let rel_path = Path::new(rel);
            if rel_path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
            {
                warn!("{suspicious_entry_log}: skipping suspicious entry '{name}'");
                continue;
            }
            favicon_extracted = true;
            let target = staged_global_favicon_dir.join(rel_path);
            if entry.is_dir() {
                std::fs::create_dir_all(&target).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("mkdir {}: {error}", target.display()))
                })?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("mkdir parent: {error}"))
                    })?;
                }
                let mut out = std::fs::File::create(&target).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Create {}: {error}", target.display()))
                })?;
                copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
                })?;
            }
        }
    }

    if !db_extracted {
        return Err(AppError::Internal(anyhow::anyhow!(
            "chan.db was found in pre-flight but not extracted — corrupted zip?"
        )));
    }

    let db_snapshot_str = db_snapshot
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Snapshot path is non-UTF-8")))?
        .replace('\'', "''");
    live_conn
        .execute_batch(&format!("VACUUM INTO '{db_snapshot_str}'"))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Snapshot live DB: {error}")))?;

    let pending_restore_id = uuid::Uuid::new_v4().to_string();
    let pending_restore_payload = crate::pending_fs::FullRestoreSwapPayload {
        staged: staged_upload_root.display().to_string(),
        live: upload_root.display().to_string(),
        previous: previous_upload_root.display().to_string(),
    };
    let pending_restore_op = crate::pending_fs::PendingFsOpInsert {
        id: pending_restore_id.clone(),
        kind: crate::pending_fs::FULL_RESTORE_SWAP_KIND,
        payload_json: serde_json::to_string(&pending_restore_payload).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Serialize full restore pending_fs_op payload: {error}"
            ))
        })?,
    };

    let backup_result = (|| -> Result<()> {
        let src = rusqlite::Connection::open(&temp_db)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Open backup source: {error}")))?;
        db::ensure_pending_fs_ops_table(&src)?;
        db::insert_pending_fs_op(&src, &pending_restore_op)?;
        let backup = Backup::new(&src, live_conn)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Backup init: {error}")))?;
        backup
            .run_to_completion(100, std::time::Duration::from_millis(0), None)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Backup copy: {error}")))?;
        Ok(())
    })();

    if let Err(error) = backup_result {
        let restore_db_result = restore_db_from_snapshot(live_conn, &db_snapshot, restore_label);
        let _ = std::fs::remove_file(&temp_db);
        let _ = std::fs::remove_file(&db_snapshot);
        let _ = remove_path_if_exists(&staged_upload_root);
        let _ = remove_path_if_exists(&staged_global_favicon_dir);
        if let Err(restore_err) = restore_db_result {
            return Err(AppError::Internal(anyhow::anyhow!(
                "{restore_label} failed and rollback failed: {error}; rollback error: {restore_err}"
            )));
        }
        return Err(error);
    }

    if let Err(error) = crate::pending_fs::finalize_full_restore_payload(&pending_restore_payload) {
        let restore_db_result = restore_db_from_snapshot(live_conn, &db_snapshot, restore_label);
        let _ = std::fs::remove_file(&temp_db);
        let _ = std::fs::remove_file(&db_snapshot);
        let _ = remove_path_if_exists(&staged_global_favicon_dir);
        if let Err(restore_err) = restore_db_result {
            return Err(AppError::Internal(anyhow::anyhow!(
                "{restore_label} filesystem swap failed: {error}; DB rollback error: {restore_err}"
            )));
        }
        return Err(AppError::Internal(anyhow::anyhow!(
            "{restore_label} filesystem swap failed: {error}"
        )));
    }
    db::delete_pending_fs_op(live_conn, &pending_restore_id)?;

    if favicon_extracted {
        remove_path_if_exists(&live_global_favicon_dir)?;
        std::fs::rename(&staged_global_favicon_dir, &live_global_favicon_dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "{restore_label} global favicon swap failed: {error}"
            ))
        })?;
    } else {
        let _ = remove_path_if_exists(&staged_global_favicon_dir);
    }

    let _ = std::fs::remove_file(&temp_db);
    let _ = std::fs::remove_file(&db_snapshot);

    let fresh_sid = new_session_id();
    let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
    match db::create_session(live_conn, &fresh_sid, admin_id, expires_at) {
        Ok(()) => {
            tracing::info!(target: "admin", admin_id = admin_id, "{completion_log}");
            if let Ok(boards) = db::get_all_boards(live_conn) {
                crate::templates::set_live_boards(boards);
            }
            Ok(fresh_sid)
        }
        Err(error) => {
            warn!("{session_warning_log}: could not create session: {error}");
            Ok(String::new())
        }
    }
}

fn extract_sqlite_db_from_full_backup_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    temp_db: &Path,
) -> Result<()> {
    let mut db_entry = archive.by_name("chan.db").map_err(|_| {
        AppError::BadRequest("Invalid full backup: zip must contain 'chan.db' at the root.".into())
    })?;
    let mut out = std::fs::File::create(temp_db)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Create temp DB: {error}")))?;
    copy_limited(&mut db_entry, &mut out, ZIP_ENTRY_MAX_BYTES)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Write temp DB: {error}")))?;
    drop(out);

    let mut header = [0u8; 16];
    let mut file = std::fs::File::open(temp_db)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Open temp DB: {error}")))?;
    std::io::Read::read_exact(&mut file, &mut header).map_err(|error| {
        AppError::BadRequest(format!("Invalid full backup database entry: {error}"))
    })?;
    if header.as_slice() != SQLITE_HEADER {
        return Err(AppError::BadRequest(
            "Invalid full backup: chan.db does not look like a SQLite database.".into(),
        ));
    }
    Ok(())
}

fn copy_board_upload_entries_from_full_backup<R: std::io::Read + std::io::Seek, W: Write + Seek>(
    archive: &mut zip::ZipArchive<R>,
    zip: &mut zip::ZipWriter<W>,
    board_short: &str,
) -> Result<()> {
    let board_prefix = format!("uploads/{board_short}/");
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip[{index}]: {error}")))?;
        let name = entry.name().to_string();
        common::validate_restore_safe_entry_name(&name)?;
        if !name.starts_with(&board_prefix) {
            continue;
        }
        if entry.is_dir() {
            zip.add_directory(
                &name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip dir: {error}")))?;
            continue;
        }
        zip.start_file(&name, zip_file_options_for_path(Path::new(&name)))
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip file entry: {error}")))?;
        std::io::copy(&mut entry, zip)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Copy board upload: {error}")))?;
    }
    Ok(())
}

fn create_temp_board_backup_from_full_backup_path(
    full_backup_path: &Path,
    board_short: &str,
) -> Result<(PathBuf, String)> {
    prune_stale_temp_board_downloads();
    std::fs::create_dir_all(temp_board_download_dir()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create temp board backup dir: {error}"))
    })?;

    let zip_file = std::fs::File::open(full_backup_path)
        .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
        .map_err(|error| AppError::BadRequest(format!("Invalid zip: {error}")))?;
    validate_full_restore_archive_layout(&archive)?;
    let _ = common::read_full_backup_manifest_from_archive(&mut archive)?;

    let temp_db = std::env::temp_dir().join(format!(
        "full_backup_extract_{}_{}.db",
        board_short,
        uuid::Uuid::new_v4().simple()
    ));
    extract_sqlite_db_from_full_backup_archive(&mut archive, &temp_db)?;

    let manifest_result = (|| -> Result<board_backup_types::BoardBackupManifest> {
        let conn = rusqlite::Connection::open(&temp_db)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Open temp DB: {error}")))?;
        create::build_board_backup_manifest(&conn, board_short)
    })();
    let _ = std::fs::remove_file(&temp_db);
    let manifest = manifest_result?;
    let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Serialize board manifest: {error}"))
    })?;

    let backup_dir = temp_board_download_dir();
    let ts = Utc::now().format("%Y%m%d_%H%M%S");
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let filename = unique_backup_filename(
        &backup_dir,
        &format!("rustchan-board-{board_short}-from-full-{ts}-{nonce}.zip"),
    );
    let final_path = backup_dir.join(&filename);
    let tmp_path = backup_dir.join(format!("{filename}.tmp"));

    let write_result = create::write_board_backup_archive(&tmp_path, &manifest_json, None, |zip| {
        copy_board_upload_entries_from_full_backup(&mut archive, zip, board_short)
    });
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }

    if let Err(error) = common::verify_board_backup_zip(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error);
    }

    std::fs::rename(&tmp_path, &final_path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::Internal(anyhow::anyhow!("Rename extracted board backup: {error}"))
    })?;

    Ok((final_path, filename))
}

#[allow(clippy::too_many_lines)]
pub async fn admin_backup(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Full backup download")?;
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
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
            let file_count = count_files_in_dir(uploads_base).saturating_add(favicon_file_count);
            let db_snapshot_size = std::fs::metadata(&temp_db)
                .map(|metadata| metadata.len())
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Stat DB snapshot: {e}")))?;
            let manifest = create::build_full_backup_manifest(
                &conn,
                db_snapshot_size,
                file_count.saturating_sub(favicon_file_count),
                favicon_file_count,
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
#[allow(clippy::arithmetic_side_effects)]
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

// ─── POST /admin/restore ──────────────────────────────────────────────────────

/// Replace the live database with the contents of a backup zip.
///
/// Design — why we use `SQLite`'s backup API instead of swapping files:
///
///   The r2d2 pool keeps up to 8 `SQLite` connections open permanently.  On
///   Linux, renaming a new file over chan.db does NOT update the connections
///   already open — they still hold file descriptors to the old inode.  File-
///   swapping therefore leaves the pool reading stale data until the process
///   restarts, and deleting the WAL while live connections are active can
///   corrupt the database.
///
///   `rusqlite::backup::Backup` wraps `SQLite`'s `sqlite3_backup_init()` API,
///   which copies data directly into the destination connection's live file —
///   through the WAL, through the same file descriptors, safely.  After
///   `run_to_completion()` returns, every connection in the pool immediately
///   sees the restored data.  No file swapping, no WAL deletion, no restart
///   required.
///
/// Security:
///   • Admin session + CSRF required before any data is touched.
///   • Zip path-traversal entries (containing ".." or absolute paths) are
///     rejected.
///   • Only "chan.db" and "uploads/…" entries are extracted; everything else
///     is silently ignored.
///   • The uploaded DB is written to a temp file then opened read-only as the
///     backup source; it is deleted on success or failure.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn admin_restore(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    request: Request,
) -> Response {
    let xhr_request = is_xml_http_request(&headers);
    let _maintenance_guard = match state
        .maintenance_gate
        .try_begin(RestoreKind::Full.maintenance_label())
    {
        Ok(guard) => guard,
        Err(error) => return restore_start_response(RestoreKind::Full, xhr_request, &error),
    };
    log_restore_upload_started(RestoreKind::Full, &headers, &jar);

    let mut multipart = match Multipart::from_request(request, &state).await {
        Ok(multipart) => multipart,
        Err(error) => {
            tracing::error!(
                target: "admin",
                route = RestoreKind::Full.route(),
                error = %error,
                "{} multipart parsing failed before handler body",
                RestoreKind::Full.title()
            );
            return restore_upload_parse_response(RestoreKind::Full, xhr_request, &error);
        }
    };

    let result: Result<String> = async {
        let session_id = restore_auth_preflight(&state, &headers, &jar).await?;
        let upload = stream_restore_upload_to_tempfile(RestoreKind::Full, &mut multipart).await?;
        validate_streamed_restore_upload(RestoreKind::Full, &jar, &upload)?;
        let zip_tmp = upload.temp_file;
        let uploaded_filename = upload.uploaded_filename;

        let upload_dir = CONFIG.upload_dir.clone();

        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<String> {
                let mut live_conn = pool.get()?;
                let admin_id = super::require_admin_session_sid(&live_conn, session_id.as_deref())?;

                let zip_file = zip_tmp
                    .reopen()
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Reopen zip: {error}")))?;
                let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                    .map_err(|error| AppError::BadRequest(format!("Invalid zip: {error}")))?;

                if let Err(error) = validate_full_restore_archive_layout(&archive) {
                    tracing::warn!(
                        target: "admin",
                        route = RestoreKind::Full.route(),
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        error = %error,
                        "{} archive layout validation failed",
                        RestoreKind::Full.title()
                    );
                    return Err(error);
                }

                execute_full_restore(
                    &mut live_conn,
                    admin_id,
                    &upload_dir,
                    &mut archive,
                    "Restore",
                    "Restore completed, new session issued",
                    "Restore",
                    "Restore",
                )
            }
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
    }
    .await;

    match result {
        Ok(fresh_sid) => {
            if fresh_sid.is_empty() {
                let jar = jar.remove(Cookie::from(super::SESSION_COOKIE));
                if xhr_request {
                    let response = crate::handlers::board::xhr_redirect_response("/admin")
                        .unwrap_or_else(|error| error.into_response());
                    return (jar, response).into_response();
                }
                return (jar, Redirect::to("/admin")).into_response();
            }

            let mut new_cookie = Cookie::new(super::SESSION_COOKIE, fresh_sid);
            new_cookie.set_http_only(true);
            new_cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
            new_cookie.set_path("/");
            new_cookie.set_secure(super::should_set_secure_cookie(&headers, Some(peer)));
            new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            if xhr_request {
                let response =
                    crate::handlers::board::xhr_redirect_response("/admin/panel?restored=1")
                        .unwrap_or_else(|error| error.into_response());
                return (jar.add(new_cookie), response).into_response();
            }

            (jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response()
        }
        Err(e) => {
            tracing::error!(
                target: "admin",
                route = RestoreKind::Full.route(),
                error = %e,
                "{} failed",
                RestoreKind::Full.title()
            );
            restore_failure_response(RestoreKind::Full, xhr_request, &e)
        }
    }
}

// ─── Zip decompression size limiter ────────────────────────────────────
//
// std::io::copy() has no bound on how much data it will write.  A malicious
// 1 KiB zip (a "zip bomb") can expand to gigabytes, exhausting disk or memory.
// copy_limited() caps the decompressed size of each entry.

// Maximum bytes to extract from any single zip entry.
// Set to 16 GiB — these are admin-only restore endpoints, so individual
// entries (large videos, the SQLite DB) can legitimately be several GiB.
// ─── Quotelink ID remapping ───────────────────────────────────────────────────
//
// When a board backup is restored into a site where the original post IDs are
// already occupied, the restore falls back to fresh auto-incremented IDs.
// `remap_body_quotelinks` then rewrites the raw text of each restored post so
// that in-board quotelinks point to the new IDs instead of the now-stale
// original ones; HTML is then re-rendered from the trusted raw body.
//
// Design constraints:
//
// 1. ONLY same-board links are remapped.  Cross-board references (`>>>/b/N`)
//    point to other boards whose IDs are unchanged by this restore operation
//    and must not be altered.
//
// 2. `pairs` must be sorted by old-ID string length *descending* before being
//    passed to these functions.  This prevents a shorter ID (e.g. "10") from
//    being substituted as a prefix of a longer one ("1000") before the longer
//    match has a chance to fire.  Example:
//      pairs = [("1000","2500"), ("100","800"), ("10","50"), ("1","3")]
//    Processing "1000" before "100" prevents "1000" → "8000" (wrong first).
//
// 3. `body` stores the original markdown-like text the user typed.  Same-board
//    references can appear as `>>{old_id}` or `>>>/{board}/{old_id}`. A
//    regex-free approach is used so replacements only fire when the matched ID
//    is followed by a non-digit (or end-of-string), avoiding `>>100` matching
//    inside `>>1000`.
//
// Rewrite in-board post references in the raw post body.
// `pairs` must be pre-sorted by old-ID string length descending.
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

fn temp_board_download_token_path(filename: &str) -> PathBuf {
    temp_board_download_dir().join(format!("{filename}.token"))
}

pub fn write_temp_board_download_token(filename: &str, token: &str) -> Result<()> {
    std::fs::create_dir_all(temp_board_download_dir()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create temp board backup dir: {error}"))
    })?;
    std::fs::write(temp_board_download_token_path(filename), token).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Write temp board download token: {error}"))
    })?;
    Ok(())
}

fn consume_temp_board_download_token(filename: &str, token: &str) -> Result<bool> {
    let token_path = temp_board_download_token_path(filename);
    let stored = match std::fs::read_to_string(&token_path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(AppError::Internal(anyhow::anyhow!(
                "Read temp board download token: {error}"
            )));
        }
    };
    if stored.trim() != token {
        return Ok(false);
    }
    std::fs::remove_file(token_path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Remove temp board download token: {error}"))
    })?;
    Ok(true)
}

fn prune_stale_temp_board_downloads() {
    let dir = temp_board_download_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let cutoff = std::time::Duration::from_secs(60 * 60);
    for entry in entries.flatten() {
        let path = entry.path();
        let is_zip = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if !is_zip {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = modified.elapsed() else {
            continue;
        };
        if age >= cutoff {
            let _ = std::fs::remove_file(path);
            if let Some(filename) = entry.file_name().to_str() {
                let _ = std::fs::remove_file(temp_board_download_token_path(filename));
            }
        }
    }
}

struct TempFileStream {
    inner: Option<ReaderStream<tokio::fs::File>>,
    cleanup_path: Option<PathBuf>,
}

impl TempFileStream {
    fn new(file: tokio::fs::File, cleanup_path: PathBuf) -> Self {
        Self {
            inner: Some(ReaderStream::new(file)),
            cleanup_path: Some(cleanup_path),
        }
    }
}

impl Stream for TempFileStream {
    type Item = std::result::Result<axum::body::Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner
            .as_mut()
            .map_or_else(|| Poll::Ready(None), |inner| Pin::new(inner).poll_next(cx))
    }
}

impl Drop for TempFileStream {
    fn drop(&mut self) {
        let _ = self.inner.take();
        if let Some(path) = self.cleanup_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn latest_board_backup_filename(board_short: &str) -> Option<String> {
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
    // Sort newest first (filename encodes timestamp for full/board backups).
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

fn prune_full_backup_dir_to_limit(dir: &Path, keep_limit: usize) -> Result<Vec<String>> {
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

fn latest_verified_full_backup_modified_time_in_dir(dir: &Path) -> Option<SystemTime> {
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

// ─── POST /admin/backup/create ────────────────────────────────────────────────

// Create/save handlers live in `backup/create.rs`.
// ─── GET /admin/backup/download/{kind}/{filename} ────────────────────────────

/// Download a saved backup file.  `kind` must be "full" or "board".
///
/// MEM-FIX (original bug): The old implementation used `tokio::fs::read()`
/// which loaded the entire file into a Vec<u8> before beginning the HTTP
/// response.  For a 5 GiB backup on a slow connection that means 5 GiB of
/// heap held for the entire download duration.
///
/// The fix: open a `tokio::fs::File` and wrap it in a `ReaderStream` so Axum
/// sends the data in 64 KiB chunks pulled directly from the OS page cache.
/// Peak heap = one 64 KiB chunk; the rest stays on disk.
#[allow(clippy::arithmetic_side_effects)]
pub async fn download_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DownloadBackupQuery>,
    axum::extract::Path((kind, filename)): axum::extract::Path<(String, String)>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

    // Validate filename — only allow safe characters to prevent path traversal.
    let safe_filename = sanitize_backup_zip_filename(&filename)?;

    match kind.as_str() {
        "temp-board" => {
            prune_stale_temp_board_downloads();
            if let Some(token) = query.token.as_deref() {
                if !consume_temp_board_download_token(&safe_filename, token)? {
                    return Err(AppError::Forbidden(
                        "Invalid or expired download token.".into(),
                    ));
                }
            } else {
                tokio::task::spawn_blocking({
                    let pool = state.db.clone();
                    move || -> Result<()> {
                        let conn = pool.get()?;
                        super::require_admin_session_sid(&conn, session_id.as_deref())?;
                        Ok(())
                    }
                })
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
            }
        }
        "full" | "board" => {
            tokio::task::spawn_blocking({
                let pool = state.db.clone();
                move || -> Result<()> {
                    let conn = pool.get()?;
                    super::require_admin_session_sid(&conn, session_id.as_deref())?;
                    Ok(())
                }
            })
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        }
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
    }

    let backup_dir = match kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        "temp-board" => temp_board_download_dir(),
        _ => unreachable!("validated above"),
    };

    let path = backup_dir.join(&safe_filename);

    // Get file size for Content-Length (so the browser shows a progress bar).
    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|_| AppError::NotFound("Backup file not found.".into()))?
        .len();

    // MEM-FIX: stream the file in 64 KiB chunks instead of loading it all.
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
    let cleanup_temp = kind == "temp-board" && query.cleanup.as_deref() == Some("1");
    let stream: Pin<
        Box<dyn Stream<Item = std::result::Result<axum::body::Bytes, std::io::Error>> + Send>,
    > = if cleanup_temp {
        Box::pin(TempFileStream::new(file, path.clone()))
    } else {
        Box::pin(ReaderStream::new(file))
    };
    let body = axum::body::Body::from_stream(stream);

    let disposition = format!("attachment; filename=\"{safe_filename}\"");
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

// ─── GET /admin/backup/progress ──────────────────────────────────────────────

/// Return current backup progress as JSON.  Polled by the admin panel JS.
///
/// Response: { phase: u64, `files_done`: u64, `files_total`: u64,
///              `bytes_done`: u64, `bytes_total`: u64 }
///
/// phase codes: `0=idle`, `1=snapshot_db`, `2=count_files`, `3=compress`, `4=save`, `5=done`
///
/// Auth is required to prevent any guest from watching backup progress.
pub async fn backup_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let p = &state.backup_progress;
    let json = format!(
        r#"{{"phase":{},"files_done":{},"files_total":{},"bytes_done":{},"bytes_total":{}}}"#,
        p.phase.load(Ordering::Relaxed),
        p.files_done.load(Ordering::Relaxed),
        p.files_total.load(Ordering::Relaxed),
        p.bytes_done.load(Ordering::Relaxed),
        p.bytes_total.load(Ordering::Relaxed),
    );

    Ok((
        [(header::CONTENT_TYPE, "application/json".to_string())],
        json,
    )
        .into_response())
}

#[derive(Default, Deserialize)]
pub struct DownloadBackupQuery {
    cleanup: Option<String>,
    token: Option<String>,
}

// ─── POST /admin/backup/delete ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteBackupForm {
    kind: String, // "full" or "board"
    filename: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Delete a saved backup file from disk.
pub async fn delete_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteBackupForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    // Validate filename.
    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;

    let backup_dir = match form.kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
    };
    let backup_kind = match form.kind.as_str() {
        "full" => BackupListKind::Full,
        "board" => BackupListKind::Board,
        _ => unreachable!(),
    };

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            let path = backup_dir.join(&safe_filename);
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Delete backup: {e}")))?;
                invalidate_backup_list_cache(&backup_dir, backup_kind);
                tracing::info!(target: "admin", filename = %safe_filename, "Backup file deleted");
            }
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_deleted=1").into_response())
}

// ─── POST /admin/backup/restore-saved ────────────────────────────────────────

#[derive(Deserialize)]
pub struct RestoreSavedForm {
    filename: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct ExtractBoardFromFullBackupForm {
    filename: String,
    board_short: String,
    action: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Restore a full backup from a saved file in backups/full/.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn restore_saved_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<RestoreSavedForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;

    let path = full_backup_dir().join(&safe_filename);
    // Do NOT read the file in the async context before auth is verified.
    // std::fs::read() blocks the Tokio runtime and an unauthenticated caller could
    // force the server to read gigabytes off disk before being rejected.  The read
    // is deferred into spawn_blocking where it runs only after the session check.
    let upload_dir = CONFIG.upload_dir.clone();

    let fresh_sid: String = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut live_conn = pool.get()?;
            // Auth check first — only read the (potentially huge) file if valid.
            let admin_id = super::require_admin_session_sid(&live_conn, session_id.as_deref())?;

            let zip_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;
            execute_full_restore(
                &mut live_conn,
                admin_id,
                &upload_dir,
                &mut archive,
                "Restore-saved",
                "Restore-saved completed",
                "Restore-saved",
                "Restore-saved",
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if fresh_sid.is_empty() {
        let jar = jar.remove(Cookie::from(super::SESSION_COOKIE));
        return Ok((jar, Redirect::to("/admin")).into_response());
    }

    let mut new_cookie = Cookie::new(super::SESSION_COOKIE, fresh_sid);
    new_cookie.set_http_only(true);
    new_cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
    new_cookie.set_path("/");
    new_cookie.set_secure(super::should_set_secure_cookie(&headers, Some(peer)));
    // Set Max-Age to match normal login behaviour.
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));
    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

enum ExtractBoardFromFullBackupOutcome {
    Download { filename: String },
    Restore { board_short: String },
}

#[allow(clippy::too_many_lines)]
pub async fn extract_board_from_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ExtractBoardFromFullBackupForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;
    let safe_board = sanitize_board_short_value(&form.board_short)?;
    let action = form.action.clone();
    let upload_dir = CONFIG.upload_dir.clone();

    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<ExtractBoardFromFullBackupOutcome> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            let full_backup_path = full_backup_dir().join(&safe_filename);
            let (temp_board_backup_path, temp_board_backup_filename) =
                create_temp_board_backup_from_full_backup_path(&full_backup_path, &safe_board)?;

            match action.as_str() {
                "download" => Ok(ExtractBoardFromFullBackupOutcome::Download {
                    filename: temp_board_backup_filename,
                }),
                "restore" => {
                    let restore_result = (|| -> Result<String> {
                        let zip_file =
                            std::fs::File::open(&temp_board_backup_path).map_err(|_| {
                                AppError::NotFound("Extracted board backup file not found.".into())
                            })?;
                        let mut manifest_archive = zip::ZipArchive::new(std::io::BufReader::new(
                            zip_file,
                        ))
                        .map_err(|error| AppError::BadRequest(format!("Invalid zip: {error}")))?;
                        let manifest = parse_board_backup_manifest_from_zip(&mut manifest_archive)?;

                        let extract_file =
                            std::fs::File::open(&temp_board_backup_path).map_err(|_| {
                                AppError::NotFound("Extracted board backup file not found.".into())
                            })?;
                        let mut extract_archive = zip::ZipArchive::new(std::io::BufReader::new(
                            extract_file,
                        ))
                        .map_err(|error| AppError::BadRequest(format!("Invalid zip: {error}")))?;

                        execute_board_restore(
                            &mut conn,
                            &upload_dir,
                            manifest,
                            |staged_root| extract_uploads_to_dir(&mut extract_archive, staged_root),
                            "Board restore-from-full",
                            "Board restore-from-full completed",
                        )
                    })();
                    let _ = std::fs::remove_file(&temp_board_backup_path);
                    restore_result.map(|board_short| ExtractBoardFromFullBackupOutcome::Restore {
                        board_short,
                    })
                }
                _ => {
                    let _ = std::fs::remove_file(&temp_board_backup_path);
                    Err(AppError::BadRequest(
                        "Unknown board extraction action.".into(),
                    ))
                }
            }
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    match outcome {
        ExtractBoardFromFullBackupOutcome::Download { filename } => {
            let download_token = new_session_id();
            write_temp_board_download_token(&filename, &download_token)?;
            Ok(Redirect::to(&format!(
                "/admin/backup/download/temp-board/{filename}?cleanup=1&token={download_token}"
            ))
            .into_response())
        }
        ExtractBoardFromFullBackupOutcome::Restore { board_short } => {
            Ok(Redirect::to(&format!("/admin/panel?board_restored={board_short}")).into_response())
        }
    }
}

// ─── POST /admin/board/backup/restore-saved ───────────────────────────────────

/// Restore a board backup from a saved file in backups/boards/.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn restore_saved_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RestoreSavedForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;

    let path = board_backup_dir().join(&safe_filename);
    // Defer the blocking file read until after auth is verified inside
    // spawn_blocking — mirrors the fix applied to restore_saved_full_backup (A3).
    let upload_dir = CONFIG.upload_dir.clone();

    let board_short_result: Result<Result<String>> = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
            // Auth check first — only read the file if the session is valid.
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            let zip_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut manifest_archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;
            let manifest = parse_board_backup_manifest_from_zip(&mut manifest_archive)?;
            let extract_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut extract_archive =
                zip::ZipArchive::new(std::io::BufReader::new(extract_file))
                    .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;

            execute_board_restore(
                &mut conn,
                &upload_dir,
                manifest,
                |staged_root| extract_uploads_to_dir(&mut extract_archive, staged_root),
                "Board restore-saved",
                "Board restore-saved completed",
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)));

    match board_short_result {
        Ok(Ok(board_short)) => {
            Ok(Redirect::to(&format!("/admin/panel?board_restored={board_short}")).into_response())
        }
        Ok(Err(app_err)) => {
            let msg = encode_q(&app_err.to_string());
            Ok(Redirect::to(&format!("/admin/panel?restore_error={msg}")).into_response())
        }
        Err(join_err) => {
            let msg = encode_q(&join_err.to_string());
            Ok(Redirect::to(&format!("/admin/panel?restore_error={msg}")).into_response())
        }
    }
}

// ─── Board-level backup / restore ─────────────────────────────────────────────

/// Stream a board-level backup zip: manifest JSON + that board's upload files.
///
/// MEM-FIX: Same approach as `admin_backup` — build zip into a `NamedTempFile` on
/// disk, then stream the result in 64 KiB chunks.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
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

/// Restore a single board from a board-level backup zip or raw board.json.
///
/// Returns `Response` (not `Result<Response>`) so ALL errors — including
/// CSRF failures and multipart parse errors — redirect to the admin panel
/// with a flash message instead of producing a blank crash page.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn board_restore(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let xhr_request = is_xml_http_request(&headers);
    let _maintenance_guard = match state
        .maintenance_gate
        .try_begin(RestoreKind::Board.maintenance_label())
    {
        Ok(guard) => guard,
        Err(error) => return restore_start_response(RestoreKind::Board, xhr_request, &error),
    };
    log_restore_upload_started(RestoreKind::Board, &headers, &jar);
    let mut multipart = match Multipart::from_request(request, &state).await {
        Ok(multipart) => multipart,
        Err(error) => {
            tracing::error!(
                target: "admin",
                route = RestoreKind::Board.route(),
                error = %error,
                "{} multipart parsing failed before handler body",
                RestoreKind::Board.title()
            );
            return restore_upload_parse_response(RestoreKind::Board, xhr_request, &error);
        }
    };
    // Run the whole operation as a fallible async block so any early return
    // with Err(...) is caught below and turned into a redirect.
    let result: Result<String> = async {
        let session_id = restore_auth_preflight(&state, &headers, &jar).await?;
        let upload_dir = CONFIG.upload_dir.clone();

        // MEM-FIX: stream the uploaded file to a NamedTempFile on disk instead
        // of buffering the entire zip into a Vec<u8>.  Board backups can be
        // hundreds of MB for active boards with many uploads.
        let upload = stream_restore_upload_to_tempfile(RestoreKind::Board, &mut multipart).await?;
        let file_size = validate_streamed_restore_upload(RestoreKind::Board, &jar, &upload)?;
        let zip_tmp = upload.temp_file;
        let uploaded_filename = upload.uploaded_filename;

        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<String> {
                use std::io::Read;

                let mut conn = pool.get()?;
                super::require_admin_session_sid(&conn, session_id.as_deref())?;

                // Detect format from the first four bytes (ZIP magic or JSON '{').
                let mut magic = [0u8; 4];
                let mut probe = zip_tmp
                    .reopen()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen: {e}")))?;
                let n = probe.read(&mut magic).unwrap_or(0);
                drop(probe);

                let is_zip = n >= 4
                    && magic[0] == b'P'
                    && magic[1] == b'K'
                    && magic[2] == 0x03
                    && magic[3] == 0x04;
                // Skip optional UTF-8 BOM (EF BB BF) before the JSON '{'.
                let is_json = if n >= 3 && magic[0] == 0xef && magic[1] == 0xbb && magic[2] == 0xbf
                {
                    n >= 4 && magic[3] == b'{'
                } else {
                    n >= 1 && magic[0] == b'{'
                };
                tracing::info!(
                    target: "admin",
                    route = RestoreKind::Board.route(),
                    filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                    temp_file_size = file_size,
                    probe_len = n,
                    magic = %format_magic_bytes(magic.get(..n.min(magic.len())).unwrap_or(&[])),
                    is_zip,
                    is_json,
                    "{} detected uploaded file format",
                    RestoreKind::Board.title()
                );

                if !is_zip && !is_json {
                    return Err(AppError::BadRequest(
                        "Unrecognized format. Upload a .zip board backup or a raw board.json file."
                            .into(),
                    ));
                }

                // We need two re-openings: one for the manifest (BufReader<File>)
                // and one for the archive used during file extraction.  Both derive
                // independent file descriptors from the same NamedTempFile so the
                // underlying bytes are always available.
                let (manifest, mut archive_opt) = if is_zip {
                    let f = zip_tmp
                        .reopen()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip: {e}")))?;
                    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f))
                        .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;
                    let entry_names = archive
                        .file_names()
                        .take(8)
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    let has_board_json = entry_names.iter().any(|name| name == "board.json")
                        || archive.file_names().any(|name| name == "board.json");
                    tracing::info!(
                        target: "admin",
                        route = RestoreKind::Board.route(),
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        sample_entries = ?entry_names,
                        has_board_json,
                        "{} inspected zip entries",
                        RestoreKind::Board.title()
                    );
                    if !has_board_json {
                        return Err(AppError::BadRequest(
                            "Invalid board backup: zip must contain 'board.json'. \
                             (Did you upload a full-site backup instead?)"
                                .into(),
                        ));
                    }
                    let manifest = parse_board_backup_manifest_from_zip(&mut archive)?;
                    tracing::info!(
                        target: "admin",
                        route = RestoreKind::Board.route(),
                        version = manifest.version,
                        board = %manifest.board.short_name,
                        threads = manifest.threads.len(),
                        posts = manifest.posts.len(),
                        polls = manifest.polls.len(),
                        file_hashes = manifest.file_hashes.len(),
                        "{} parsed board backup manifest from zip",
                        RestoreKind::Board.title()
                    );
                    // Re-open a fresh archive for file extraction in the second pass.
                    let f2 = zip_tmp
                        .reopen()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip (2): {e}")))?;
                    let archive2 = zip::ZipArchive::new(std::io::BufReader::new(f2))
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen archive: {e}")))?;
                    (manifest, Some(archive2))
                } else {
                    // Raw board.json — read fully (manifests are small).
                    let mut f = zip_tmp
                        .reopen()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen json: {e}")))?;
                    let buf = read_limited_bytes(
                        &mut f,
                        BOARD_MANIFEST_MAX_BYTES,
                        "board.json manifest",
                    )?;
                    let manifest: board_backup_types::BoardBackupManifest =
                        serde_json::from_slice(&buf).map_err(|e| {
                            AppError::BadRequest(format!("Invalid board.json: {e}"))
                        })?;
                    tracing::info!(
                        target: "admin",
                        route = RestoreKind::Board.route(),
                        version = manifest.version,
                        board = %manifest.board.short_name,
                        threads = manifest.threads.len(),
                        posts = manifest.posts.len(),
                        polls = manifest.polls.len(),
                        file_hashes = manifest.file_hashes.len(),
                        "{} parsed raw board.json manifest",
                        RestoreKind::Board.title()
                    );
                    (manifest, None)
                };
                execute_board_restore(
                    &mut conn,
                    &upload_dir,
                    manifest,
                    |staged_root| {
                        if let Some(ref mut archive) = archive_opt {
                            extract_uploads_to_dir(archive, staged_root)?;
                        }
                        Ok(())
                    },
                    "Board restore",
                    "Board restore completed",
                )
            }
        })
        .await
        .unwrap_or_else(|e| Err(AppError::Internal(anyhow::anyhow!("Task panicked: {e}"))))
    }
    .await;

    match result {
        Ok(board_short) => {
            if xhr_request {
                return crate::handlers::board::xhr_redirect_response(&format!(
                    "/admin/panel?board_restored={board_short}"
                ))
                .unwrap_or_else(|error| error.into_response());
            }
            redirect_page_response(
                &format!("/admin/panel?board_restored={board_short}"),
                &format!("Board /{board_short}/ restored successfully."),
            )
        }
        Err(e) => {
            tracing::error!(
                target: "admin",
                route = RestoreKind::Board.route(),
                error = %e,
                "{} failed",
                RestoreKind::Board.title()
            );
            restore_failure_response(RestoreKind::Board, xhr_request, &e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_board_backup_manifest, consume_temp_board_download_token,
        create_temp_board_backup_from_full_backup_path, execute_board_restore,
        latest_verified_full_backup_modified_time_in_dir, render_restored_body_html,
        should_store_without_recompress, temp_board_download_token_path,
        validate_full_restore_archive_layout, write_temp_board_download_token, RestoreKind,
    };
    use crate::error::AppError;
    use crate::models::BackupBoardSummary;
    use axum::{body::to_bytes, http::StatusCode};
    use rusqlite::params;
    use std::io::{Cursor, Write as _};
    use std::path::Path;

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
