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
    models::BackupInfo,
    utils::crypto::new_session_id,
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
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
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

fn format_magic_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_board_backup_manifest_from_zip<R: std::io::Read + std::io::Seek>(
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
    archive: &mut zip::ZipArchive<R>,
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
    manifest: board_backup_types::BoardBackupManifest,
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
                 show_poster_ids=?17, collapse_greentext=?18, post_cooldown_secs=?19
                 WHERE id=?20",
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
                 allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext, post_cooldown_secs, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
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
                    manifest.board.created_at,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Insert board: {error}")))?
        };

        let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
        for thread in &manifest.threads {
            let new_thread_id = insert_returning_id(
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
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert thread {}: {error}", thread.id))
            })?;
            thread_id_map.insert(thread.id, new_thread_id);
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
            let new_post_id = insert_returning_id(
                conn,
                "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                 body, body_html, ip_hash, file_path, file_name, file_size,
                 thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
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
                ],
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert post {}: {error}", post.id))
            })?;
            post_id_map.insert(post.id, new_post_id);
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

                let new_body = remap_body_quotelinks(&post.body, &pairs);
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
            let new_poll_id = insert_returning_id(
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
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert poll {}: {error}", poll.id))
            })?;
            poll_id_map.insert(poll.id, new_poll_id);
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
            let new_option_id = insert_returning_id(
                conn,
                "INSERT INTO poll_options (poll_id, text, position)
                 VALUES (?1,?2,?3)
                 RETURNING id",
                params![new_poll_id, option.text, option.position],
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Insert option {}: {error}", option.id))
            })?;
            option_id_map.insert(option.id, new_option_id);
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

#[allow(clippy::too_many_lines)]
pub async fn admin_backup(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
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
            drop(conn);

            // Count files for progress bar before compressing.
            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            log_backup_phase(crate::middleware::backup_phase::COUNT_FILES);
            let uploads_base = std::path::Path::new(&upload_dir);
            let favicon_file_count = count_files_in_dir(&global_favicon_dir);
            let file_count = count_files_in_dir(uploads_base).saturating_add(favicon_file_count);
            // +1 for chan.db
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            // MEM-FIX: write zip directly to a NamedTempFile instead of Vec<u8>.
            let zip_tmp = tempfile::NamedTempFile::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp zip: {e}")))?;
            {
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
                    .store(file_count.saturating_add(1), Ordering::Relaxed);

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
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        route = "/admin/restore",
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        has_session_cookie = jar.get(super::SESSION_COOKIE).is_some(),
        has_csrf_cookie = jar.get("csrf_token").is_some(),
        "Full restore upload started"
    );

    let mut multipart = match Multipart::from_request(request, &state).await {
        Ok(multipart) => multipart,
        Err(error) => {
            tracing::error!(
                target: "admin",
                route = "/admin/restore",
                error = %error,
                "Full restore multipart parsing failed before handler body"
            );
            let target = format!(
                "/admin/panel?restore_error={}",
                encode_q(&format!("Upload parsing failed: {error}"))
            );
            return redirect_page_response(&target, "Restore upload failed.");
        }
    };

    let result: Result<String> = async {
        let session_id = jar
            .get(super::SESSION_COOKIE)
            .map(|c| c.value().to_string());
        super::require_same_origin_request(&headers)?;
        {
            let pool = state.db.clone();
            let session_id = session_id.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = pool.get()?;
                super::require_admin_session_sid(&conn, session_id.as_deref())?;
                Ok(())
            })
            .await
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Admin auth preflight task failed: {error}"))
            })??;
        }

        let mut zip_tmp: Option<tempfile::NamedTempFile> = None;
        let mut form_csrf: Option<String> = None;
        let mut uploaded_filename: Option<String> = None;
        let mut uploaded_content_type: Option<String> = None;
        let mut uploaded_bytes = 0u64;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("Multipart error: {e}")))?
        {
            let field_name = field.name().unwrap_or("<unnamed>").to_string();
            match field.name() {
                Some("_csrf") => {
                    tracing::debug!(
                        target: "admin",
                        route = "/admin/restore",
                        field = "_csrf",
                        "Full restore received CSRF field"
                    );
                    form_csrf = Some(
                        field
                            .text()
                            .await
                            .map_err(|e| AppError::BadRequest(e.to_string()))?,
                    );
                }
                Some("backup_file") => {
                    uploaded_filename = field.file_name().map(str::to_string);
                    uploaded_content_type = field.content_type().map(str::to_string);
                    tracing::info!(
                        target: "admin",
                        route = "/admin/restore",
                        field = field_name,
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
                        "Full restore received backup file field"
                    );
                    let tmp = tempfile::NamedTempFile::new()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Tempfile: {e}")))?;
                    let std_clone = tmp
                        .as_file()
                        .try_clone()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Clone fd: {e}")))?;
                    let async_file = tokio::fs::File::from_std(std_clone);
                    let mut writer = tokio::io::BufWriter::new(async_file);
                    let mut field = field;
                    while let Some(chunk) = field
                        .chunk()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?
                    {
                        uploaded_bytes = uploaded_bytes
                            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                        writer
                            .write_all(&chunk)
                            .await
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write chunk: {e}")))?;
                    }
                    writer
                        .flush()
                        .await
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush: {e}")))?;
                    zip_tmp = Some(tmp);
                }
                _ => {
                    tracing::debug!(
                        target: "admin",
                        route = "/admin/restore",
                        field = field_name,
                        "Full restore ignored unexpected multipart field"
                    );
                    let _ = field.bytes().await;
                }
            }
        }

        let has_csrf_cookie = jar.get("csrf_token").is_some();
        if super::check_csrf_jar(&jar, form_csrf.as_deref()).is_err() {
            tracing::warn!(
                target: "admin",
                route = "/admin/restore",
                has_csrf_cookie = has_csrf_cookie,
                has_form_csrf = form_csrf.is_some(),
                "Full restore failed CSRF validation"
            );
            return Err(AppError::Forbidden("CSRF token mismatch.".into()));
        }

        let zip_tmp =
            zip_tmp.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
        let zip_size = zip_tmp
            .as_file()
            .seek(std::io::SeekFrom::End(0))
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Seek check: {e}")))?;
        if zip_size == 0 {
            return Err(AppError::BadRequest(
                "Uploaded backup file is empty.".into(),
            ));
        }

        tracing::info!(
            target: "admin",
            route = "/admin/restore",
            filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
            mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
            streamed_bytes = uploaded_bytes,
            temp_file_size = zip_size,
            "Full restore upload streamed to disk"
        );

        let upload_dir = CONFIG.upload_dir.clone();

        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<String> {
                let mut live_conn = pool.get()?;
                let admin_id = super::require_admin_session_sid(&live_conn, session_id.as_deref())?;

                let zip_file = zip_tmp
                    .reopen()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip: {e}")))?;
                let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                    .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;

                if let Err(error) = validate_full_restore_archive_layout(&mut archive) {
                    tracing::warn!(
                        target: "admin",
                        route = "/admin/restore",
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        error = %error,
                        "Full restore archive layout validation failed"
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
                return (jar, Redirect::to("/admin")).into_response();
            }

            let mut new_cookie = Cookie::new(super::SESSION_COOKIE, fresh_sid);
            new_cookie.set_http_only(true);
            new_cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
            new_cookie.set_path("/");
            new_cookie.set_secure(super::should_set_secure_cookie(&headers, Some(peer)));
            new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

            (jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response()
        }
        Err(e) => {
            tracing::error!(
                target: "admin",
                route = "/admin/restore",
                error = %e,
                "Full restore failed"
            );
            redirect_page_response(
                &format!("/admin/panel?restore_error={}", encode_q(&e.to_string())),
                "Restore failed.",
            )
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
// When a board backup is restored, posts receive new auto-incremented IDs
// because other boards' posts already occupy the original IDs in the global
// `posts` table. `remap_body_quotelinks` rewrites the raw text of each restored
// post so that in-board quotelinks point to the new IDs instead of the now-stale
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
// 3. `body` stores the original markdown-like text the user typed.  In-board
//    quotelinks appear as `>>{old_id}` (e.g. `>>500`).  A regex-free approach
//    is used: for each (old, new) pair, replace `>>{old}` followed by a
//    non-digit (or end-of-string) to avoid `>>100` matching inside `>>1000`.
//
// Rewrite in-board `>>{old_id}` references in the raw post body.
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
    let mut matches = list_backup_files(&board_backup_dir())
        .into_iter()
        .filter(|info| info.filename.starts_with(&prefix));
    matches.next().map(|info| info.filename)
}

/// List `.zip` files in `dir`, newest-filename-first.
pub fn list_backup_files(dir: &std::path::Path) -> Vec<BackupInfo> {
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
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        let secs = d.as_secs().cast_signed();
                        #[allow(deprecated)]
                        chrono::DateTime::<Utc>::from_timestamp(secs, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                files.push(BackupInfo {
                    filename: name,
                    size_bytes: meta.len(),
                    modified,
                });
            }
        }
    }
    // Sort newest first (filename encodes timestamp for full/board backups).
    files.sort_by(|a, b| b.filename.cmp(&a.filename));
    files
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
    let safe_filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != filename || safe_filename.contains("..") {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    if !std::path::Path::new(&safe_filename)
        .extension()
        .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest(
            "Only .zip files can be downloaded.".into(),
        ));
    }

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
    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename || safe_filename.contains("..") {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    if !std::path::Path::new(&safe_filename)
        .extension()
        .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest(
            "Only .zip files can be deleted.".into(),
        ));
    }

    let backup_dir = match form.kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
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

    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename
        || safe_filename.contains("..")
        || !std::path::Path::new(&safe_filename)
            .extension()
            .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }

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

    let safe_filename: String = form
        .filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != form.filename
        || safe_filename.contains("..")
        || !std::path::Path::new(&safe_filename)
            .extension()
            .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }

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
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        route = "/admin/board/restore",
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        has_session_cookie = jar.get(super::SESSION_COOKIE).is_some(),
        has_csrf_cookie = jar.get("csrf_token").is_some(),
        "Board restore upload started"
    );
    let mut multipart = match Multipart::from_request(request, &state).await {
        Ok(multipart) => multipart,
        Err(error) => {
            tracing::error!(
                target: "admin",
                route = "/admin/board/restore",
                error = %error,
                "Board restore multipart parsing failed before handler body"
            );
            let target = format!(
                "/admin/panel?restore_error={}",
                encode_q(&format!("Upload parsing failed: {error}"))
            );
            return redirect_page_response(&target, "Board restore upload failed.");
        }
    };
    // Run the whole operation as a fallible async block so any early return
    // with Err(...) is caught below and turned into a redirect.
    let result: Result<String> = async {
        let session_id = jar
            .get(super::SESSION_COOKIE)
            .map(|c| c.value().to_string());
        super::require_same_origin_request(&headers)?;
        {
            let pool = state.db.clone();
            let session_id = session_id.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = pool.get()?;
                super::require_admin_session_sid(&conn, session_id.as_deref())?;
                Ok(())
            })
            .await
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Admin auth preflight task failed: {error}"))
            })??;
        }
        let upload_dir = CONFIG.upload_dir.clone();

        // MEM-FIX: stream the uploaded file to a NamedTempFile on disk instead
        // of buffering the entire zip into a Vec<u8>.  Board backups can be
        // hundreds of MB for active boards with many uploads.
        let mut zip_tmp: Option<tempfile::NamedTempFile> = None;
        let mut form_csrf: Option<String> = None;
        let mut uploaded_filename: Option<String> = None;
        let mut uploaded_content_type: Option<String> = None;
        let mut uploaded_bytes = 0u64;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("Multipart error: {e}")))?
        {
            let field_name = field.name().unwrap_or("<unnamed>").to_string();
            match field.name() {
                Some("_csrf") => {
                    tracing::debug!(
                        target: "admin",
                        route = "/admin/board/restore",
                        field = "_csrf",
                        "Board restore received CSRF field"
                    );
                    form_csrf = Some(
                        field
                            .text()
                            .await
                            .map_err(|e| AppError::BadRequest(e.to_string()))?,
                    );
                }
                Some("backup_file") => {
                    uploaded_filename = field.file_name().map(str::to_string);
                    uploaded_content_type = field.content_type().map(str::to_string);
                    tracing::info!(
                        target: "admin",
                        route = "/admin/board/restore",
                        field = field_name,
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
                        "Board restore received backup file field"
                    );
                    let tmp = tempfile::NamedTempFile::new()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Tempfile: {e}")))?;
                    // Clone the underlying fd for async writing; the original
                    // NamedTempFile retains ownership and the delete-on-drop guard.
                    let std_clone = tmp
                        .as_file()
                        .try_clone()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Clone fd: {e}")))?;
                    let async_file = tokio::fs::File::from_std(std_clone);
                    let mut writer = tokio::io::BufWriter::new(async_file);
                    let mut field = field;
                    while let Some(chunk) = field
                        .chunk()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?
                    {
                        uploaded_bytes = uploaded_bytes
                            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                        writer
                            .write_all(&chunk)
                            .await
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write chunk: {e}")))?;
                    }
                    writer
                        .flush()
                        .await
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush: {e}")))?;
                    zip_tmp = Some(tmp);
                }
                _ => {
                    tracing::debug!(
                        target: "admin",
                        route = "/admin/board/restore",
                        field = field_name,
                        "Board restore ignored unexpected multipart field"
                    );
                }
            }
        }

        let has_csrf_cookie = jar.get("csrf_token").is_some();
        if super::check_csrf_jar(&jar, form_csrf.as_deref()).is_err() {
            tracing::warn!(
                target: "admin",
                route = "/admin/board/restore",
                has_csrf_cookie = has_csrf_cookie,
                has_form_csrf = form_csrf.is_some(),
                "Board restore failed CSRF validation"
            );
            return Err(AppError::Forbidden("CSRF token mismatch.".into()));
        }

        let zip_tmp =
            zip_tmp.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
        // Determine size without reading into RAM.
        let file_size = zip_tmp
            .as_file()
            .seek(std::io::SeekFrom::End(0))
            .map_err(|e| AppError::Internal(anyhow::anyhow!("Seek check: {e}")))?;
        if file_size == 0 {
            return Err(AppError::BadRequest(
                "Uploaded backup file is empty.".into(),
            ));
        }
        tracing::info!(
            target: "admin",
            route = "/admin/board/restore",
            filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
            mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
            streamed_bytes = uploaded_bytes,
            temp_file_size = file_size,
            "Board restore upload streamed to disk"
        );

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
                    route = "/admin/board/restore",
                    filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                    temp_file_size = file_size,
                    probe_len = n,
                    magic = %format_magic_bytes(magic.get(..n.min(magic.len())).unwrap_or(&[])),
                    is_zip,
                    is_json,
                    "Board restore detected uploaded file format"
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
                        route = "/admin/board/restore",
                        filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                        sample_entries = ?entry_names,
                        has_board_json,
                        "Board restore inspected zip entries"
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
                        route = "/admin/board/restore",
                        version = manifest.version,
                        board = %manifest.board.short_name,
                        threads = manifest.threads.len(),
                        posts = manifest.posts.len(),
                        polls = manifest.polls.len(),
                        file_hashes = manifest.file_hashes.len(),
                        "Board restore parsed board backup manifest from zip"
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
                        route = "/admin/board/restore",
                        version = manifest.version,
                        board = %manifest.board.short_name,
                        threads = manifest.threads.len(),
                        posts = manifest.posts.len(),
                        polls = manifest.polls.len(),
                        file_hashes = manifest.file_hashes.len(),
                        "Board restore parsed raw board.json manifest"
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
        Ok(board_short) => redirect_page_response(
            &format!("/admin/panel?board_restored={board_short}"),
            &format!("Board /{board_short}/ restored successfully."),
        ),
        Err(e) => {
            tracing::error!(
                target: "admin",
                route = "/admin/board/restore",
                error = %e,
                "Board restore failed"
            );
            redirect_page_response(
                &format!("/admin/panel?restore_error={}", encode_q(&e.to_string())),
                "Board restore failed.",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        consume_temp_board_download_token, should_store_without_recompress,
        temp_board_download_token_path, validate_full_restore_archive_layout,
        write_temp_board_download_token,
    };
    use crate::error::AppError;
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

    #[test]
    fn full_restore_layout_accepts_full_backup_archive() {
        let mut archive = zip_with_entries(&[("chan.db", b"SQLite format 3\0stub")]);
        assert!(validate_full_restore_archive_layout(&mut archive).is_ok());
    }

    #[test]
    fn full_restore_layout_rejects_board_backup_archive_with_helpful_hint() {
        let mut archive = zip_with_entries(&[("board.json", br#"{"version":1}"#)]);
        let error = validate_full_restore_archive_layout(&mut archive).expect_err("should fail");
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
}
