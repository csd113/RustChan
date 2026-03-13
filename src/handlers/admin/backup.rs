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
    extract::{Form, Multipart, State},
    http::header,
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::Utc;
use rusqlite::{backup::Backup, params};
use serde::Deserialize;
use serde_json;
use std::io::{Seek, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use time;
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;
use tracing::{info, warn};

#[allow(clippy::too_many_lines)]
pub async fn admin_backup(State(state): State<AppState>, jar: CookieJar) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);

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
            let uploads_base = std::path::Path::new(&upload_dir);
            let file_count = count_files_in_dir(uploads_base);
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

                // ── Upload files (streamed file-by-file via io::copy) ──────
                if uploads_base.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
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
            info!("Admin downloaded full backup ({} bytes on disk)", file_size);
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
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
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("read_dir {}: {}", dir.display(), e)))?;

    for entry in entries {
        let entry = entry.map_err(|e| AppError::Internal(anyhow::anyhow!("dir entry: {e}")))?;
        let path = entry.path();

        let relative = path
            .strip_prefix(base)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("strip_prefix: {e}")))?;
        let rel_str = relative.to_string_lossy().replace('\\', "/");
        let zip_path = format!("uploads/{rel_str}");

        if path.is_dir() {
            zip.add_directory(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip dir: {e}")))?;
            add_dir_to_zip(zip, base, &path, opts, progress)?;
        } else if path.is_file() {
            // MEM-FIX: open file, stream through io::copy — no Vec<u8> allocation.
            let mut src = std::fs::File::open(&path).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("open {}: {}", path.display(), e))
            })?;
            zip.start_file(&zip_path, opts)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip file entry: {e}")))?;
            let copied = std::io::copy(&mut src, zip).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("copy {} to zip: {}", path.display(), e))
            })?;
            progress.files_done.fetch_add(1, Ordering::Relaxed);
            progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
        }
    }
    Ok(())
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
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

    // FIX[A7]: Stream the uploaded zip to a NamedTempFile on disk instead of
    // buffering the entire upload into a Vec<u8>.  Full-site backups can be
    // several GiB; loading them entirely into the heap exhausts available memory.
    let mut zip_tmp: Option<tempfile::NamedTempFile> = None;
    let mut form_csrf: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {e}")))?
    {
        match field.name() {
            Some("_csrf") => {
                form_csrf = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| AppError::BadRequest(e.to_string()))?,
                );
            }
            Some("backup_file") => {
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
                // Drain unknown fields so the multipart stream advances.
                let _ = field.bytes().await;
            }
        }
    }

    // CSRF check.
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form_csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let zip_tmp = zip_tmp.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;
    // Determine size without reading into RAM: seeking to end gives the byte count.
    let zip_size = zip_tmp
        .as_file()
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Seek check: {e}")))?;
    if zip_size == 0 {
        return Err(AppError::BadRequest(
            "Uploaded backup file is empty.".into(),
        ));
    }

    let upload_dir = CONFIG.upload_dir.clone();

    let fresh_sid: String = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            // ── Auth ──────────────────────────────────────────────────────
            // Hold this connection open for the entire restore so the pool
            // can't recycle it and open a fresh one mid-copy.
            let mut live_conn = pool.get()?;
            // Save admin_id now — we'll need it to create a new session
            // in the restored DB once the backup completes.
            let admin_id = super::require_admin_session_sid(&live_conn, session_id.as_deref())?;

            // ── Open the on-disk zip (FIX[A7]) ───────────────────────────
            // reopen() gives a fresh File descriptor seeked to position 0,
            // so ZipArchive can navigate entries without loading into RAM.
            let zip_file = zip_tmp
                .reopen()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip: {e}")))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;

            // Quick pre-flight: make sure there is a chan.db entry.
            // file_names() is a stable iterator available in zip 2+ and zip 8+.
            let has_db = archive.file_names().any(|n| n == "chan.db");
            if !has_db {
                return Err(AppError::BadRequest(
                    "Invalid backup: zip must contain 'chan.db' at the root.".into(),
                ));
            }

            // ── Single-pass extraction ────────────────────────────────────
            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().simple().to_string();
            let temp_db = temp_dir.join(format!("chan_restore_{tmp_id}.db"));
            let mut db_extracted = false;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip read [{i}]: {e}")))?;
                let name = entry.name().to_string();

                // Security: skip any path-traversal attempts.
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Restore: skipping suspicious zip entry '{name}'");
                    continue;
                }

                if name == "chan.db" {
                    let mut out = std::fs::File::create(&temp_db)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp DB: {e}")))?;
                    copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp DB: {e}")))?;
                    db_extracted = true;

                } else if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() { continue; }
                    let target = PathBuf::from(&upload_dir).join(rel);

                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir {}: {}", target.display(), e)))?;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)
                                .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir parent: {e}")))?;
                        }
                        let mut out = std::fs::File::create(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Create {}: {}", target.display(), e)))?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write {}: {}", target.display(), e)))?;
                    }
                }
            }
            drop(archive);

            if !db_extracted {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "chan.db was found in pre-flight but not extracted — corrupted zip?"
                )));
            }

            // ── SQLite backup API: copy temp DB → live DB ─────────────────
            let backup_result = (|| -> Result<()> {
                let src = rusqlite::Connection::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open backup source: {e}")))?;
                                let backup = Backup::new(&src, &mut live_conn)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup init: {e}")))?;
                backup.run_to_completion(100, std::time::Duration::from_millis(0), None)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup copy: {e}")))?;
                Ok(())
            })();

            let _ = std::fs::remove_file(&temp_db);
            backup_result?;

            // ── Re-issue session cookie ───────────────────────────────────
            //
            // The backup API just replaced the admin_sessions table with the
            // one from the backup file, so the browser's current session ID is
            // now invalid against the restored DB.  Create a fresh session for
            // the same admin_id so the redirect to /admin/panel succeeds.
            //
            // If admin_id doesn't exist in the restored DB (e.g. restoring
            // from a much older backup) the INSERT will fail with a FK error.
            // We catch that, log it, and return an empty string to signal that
            // the handler should redirect to the login page instead.
            let fresh_sid = new_session_id();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            match db::create_session(&live_conn, &fresh_sid, admin_id, expires_at) {
                Ok(()) => {
                    info!("Admin restore completed; new session issued for admin_id={admin_id}");
                    // Refresh live board list — the restored DB may have
                    // different boards than what was running before.
                    if let Ok(boards) = db::get_all_boards(&live_conn) {
                        crate::templates::set_live_boards(boards);
                    }
                    Ok(fresh_sid)
                }
                Err(e) => {
                    warn!("Restore: could not create new session (admin_id={} may not exist in backup): {}", admin_id, e);
                    Ok(String::new())
                }
            }
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // If we got a valid session ID back, replace the cookie and go to the
    // panel.  If not (admin didn't exist in the backup), go to login instead.
    if fresh_sid.is_empty() {
        let jar = jar.remove(Cookie::from(super::SESSION_COOKIE));
        return Ok((jar, Redirect::to("/admin")).into_response());
    }

    let mut new_cookie = Cookie::new(super::SESSION_COOKIE, fresh_sid);
    new_cookie.set_http_only(true);
    new_cookie.set_same_site(SameSite::Strict);
    new_cookie.set_path("/");
    new_cookie.set_secure(CONFIG.https_cookies);
    // FIX[A5]: Set Max-Age so the browser expires the cookie after the configured
    // session lifetime — matching the behaviour of the normal login handler.
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

// ─── CRIT-4: Zip decompression size limiter ────────────────────────────────────
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
// `posts` table.  `remap_body_quotelinks` and `remap_body_html_quotelinks`
// rewrite the raw text and rendered HTML of each restored post so that in-board
// quotelinks point to the new IDs instead of the now-stale original ones.
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
// 4. `body_html` stores pre-rendered HTML.  In-board quotelinks look like:
//      <a href="#p{N}" class="quotelink" data-pid="{N}">&gt;&gt;{N}</a>
//    Cross-board quotelinks look like:
//      <a href="/board/post/{N}" class="quotelink crosslink" ...>&gt;&gt;&gt;/…</a>
//    We replace `href="#p{old}"` (exclusively in-board) and
//    `data-pid="{old}">&gt;&gt;{old}</a>` (display text also exclusive to
//    in-board links — cross-board display text has `&gt;&gt;&gt;/` prefix).

// Rewrite in-board `>>{old_id}` references in the raw post body.
// `pairs` must be pre-sorted by old-ID string length descending.
#[allow(clippy::arithmetic_side_effects)]
fn remap_body_quotelinks(body: &str, pairs: &[(String, String)]) -> String {
    // Avoid cloning when there is nothing to change.
    if pairs.is_empty() {
        return body.to_string();
    }
    let mut result = body.to_string();
    for (old, new) in pairs {
        // Match `>>{old}` only when NOT immediately followed by another digit,
        // so we don't turn `>>1000` into `>>new_id_for_1000` when processing
        // the `>>100` entry first.
        //
        // Implementation: scan for every occurrence of `>>{old}` in the string
        // and check the next character.  Replace left-to-right using byte indices
        // to avoid re-scanning already-replaced sections.
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
                    // Only replace when the char after the match is not a digit.
                    let next_is_digit = bytes.get(after).is_some_and(u8::is_ascii_digit);
                    out.push_str(&result[pos..abs]);
                    if next_is_digit {
                        // Not the right match — keep the original text.
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

/// Rewrite in-board quotelink IDs in pre-rendered `body_html`.
///
/// Targets two patterns that are exclusive to same-board quotelinks:
///   • `href="#p{old}"` — the anchor href
///   • `data-pid="{old}">&gt;&gt;{old}</a>` — the data attribute + display text
///
/// Cross-board links use `href="/board/post/{N}"` and display `&gt;&gt;&gt;/…`
/// so neither pattern matches them.
///
/// `pairs` must be pre-sorted by old-ID string length descending.
fn remap_body_html_quotelinks(body_html: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return body_html.to_string();
    }
    let mut result = body_html.to_string();
    for (old, new) in pairs {
        // Pattern 1: href="#p{old}" → href="#p{new}"
        let old_href = format!("href=\"#p{old}\"");
        let new_href = format!("href=\"#p{new}\"");
        result = result.replace(&old_href, &new_href);

        // Pattern 2: data-pid="{old}">&gt;&gt;{old}</a>
        // This uniquely identifies in-board quotelinks: cross-board links have
        // "&gt;&gt;&gt;/" as their display text, never bare "&gt;&gt;{N}".
        let old_tail = format!("data-pid=\"{old}\">&gt;&gt;{old}</a>");
        let new_tail = format!("data-pid=\"{new}\">&gt;&gt;{new}</a>");
        result = result.replace(&old_tail, &new_tail);
    }
    result
}

const ZIP_ENTRY_MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Like `std::io::copy` but returns `InvalidData` if more than `max_bytes`
/// would be written.  Reads in 64 KiB chunks; aborts as soon as the limit
/// is exceeded so disk space is not wasted.
#[allow(clippy::arithmetic_side_effects)]
fn copy_limited<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 65536];
    let mut total: u64 = 0;
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

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Check CSRF using the cookie jar. Returns error on mismatch.
/// Verify admin session and also return the admin's username.
/// For use inside `spawn_blocking` closures.
fn db_dir() -> PathBuf {
    PathBuf::from(&CONFIG.database_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf)
}

/// rustchan-data/full-backups/
pub fn full_backup_dir() -> PathBuf {
    db_dir().join("full-backups")
}

/// rustchan-data/board-backups/
pub fn board_backup_dir() -> PathBuf {
    db_dir().join("board-backups")
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

/// Create a full backup and save it to rustchan-data/full-backups/.
///
/// MEM-FIX: The zip is written directly to the final destination file via a
/// `BufWriter`, so peak RAM usage is O(compression-buffer) not O(zip-size).
/// A `.tmp` suffix is used during writing; the file is renamed on success so
/// the backup list never shows a partial/corrupt zip.
#[allow(clippy::arithmetic_side_effects)]
pub async fn create_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<super::CsrfOnly>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);

            // VACUUM INTO for a consistent snapshot.
            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().simple().to_string();
            let temp_db = temp_dir.join(format!("chan_backup_{tmp_id}.db"));
            let temp_db_str = temp_db
                .to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path non-UTF-8")))?
                .replace('\'', "''");

            conn.execute_batch(&format!("VACUUM INTO '{temp_db_str}'"))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {e}")))?;
            drop(conn);

            // Count files for progress bar before compressing.
            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            let uploads_base = std::path::Path::new(&upload_dir);
            let file_count = count_files_in_dir(uploads_base);
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            // MEM-FIX: write zip directly to a .tmp file on disk, not a Vec<u8>.
            let backup_dir = full_backup_dir();
            std::fs::create_dir_all(&backup_dir)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create full-backups dir: {e}")))?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-backup-{ts}.zip");
            let final_path = backup_dir.join(&fname);
            let tmp_path = backup_dir.join(format!("{fname}.tmp"));

            {
                let out_file = std::io::BufWriter::new(
                    std::fs::File::create(&tmp_path)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create zip tmp: {e}")))?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                progress.reset(crate::middleware::backup_phase::COMPRESS);
                progress
                    .files_total
                    .store(file_count.saturating_add(1), Ordering::Relaxed);

                // ── Database snapshot (streamed, not read into RAM) ────────
                zip.start_file("chan.db", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip DB: {e}")))?;
                let mut db_src = std::fs::File::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {e}")))?;
                let copied = std::io::copy(&mut db_src, &mut zip)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {e}")))?;
                drop(db_src);
                let _ = std::fs::remove_file(&temp_db);
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);

                // ── Upload files (streamed via io::copy) ───────────────────
                if uploads_base.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
                }

                let writer = zip
                    .finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {e}")))?;
                // Flush the BufWriter so the OS buffer is committed to disk.
                writer
                    .into_inner()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush zip writer: {e}")))?
                    .sync_all()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Sync zip file: {e}")))?;
            }

            // Atomic rename: only becomes visible in the list when complete.
            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename backup: {e}"))
            })?;

            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            info!("Admin created full backup: {} ({} bytes)", fname, size);
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_created=1").into_response())
}

// ─── POST /admin/board/backup/create ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardBackupCreateForm {
    board_short: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Create a board backup and save it to rustchan-data/board-backups/.
#[allow(clippy::too_many_lines)]
pub async fn create_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardBackupCreateForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let board_short = form
        .board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if board_short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
    use board_backup_types::{BoardRow, ThreadRow, PostRow, PollRow, PollOptionRow, PollVoteRow, FileHashRow, BoardBackupManifest};

            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            let board: BoardRow = conn
                .query_row(
                    "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                             allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                             allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                             post_cooldown_secs, created_at
                      FROM boards WHERE short_name = ?1",
                    params![board_short],
                    |r| {
                        Ok(BoardRow {
                            id: r.get(0)?,
                            short_name: r.get(1)?,
                            name: r.get(2)?,
                            description: r.get(3)?,
                            nsfw: r.get::<_, i64>(4)? != 0,
                            max_threads: r.get(5)?,
                            bump_limit: r.get(6)?,
                            allow_images: r.get::<_, i64>(7)? != 0,
                            allow_video: r.get::<_, i64>(8)? != 0,
                            allow_audio: r.get::<_, i64>(9)? != 0,
                            allow_tripcodes: r.get::<_, i64>(10)? != 0,
                            edit_window_secs: r.get(11)?,
                            allow_editing: r.get::<_, i64>(12)? != 0,
                            allow_archive: r.get::<_, i64>(13)? != 0,
                            allow_video_embeds: r.get::<_, i64>(14)? != 0,
                            allow_captcha: r.get::<_, i64>(15)? != 0,
                            post_cooldown_secs: r.get(16)?,
                            created_at: r.get(17)?,
                        })
                    },
                )
                .map_err(|_| AppError::NotFound(format!("Board '{board_short}' not found")))?;

            let board_id = board.id;

            let threads: Vec<ThreadRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, reply_count
                         FROM threads WHERE board_id = ?1 ORDER BY id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(ThreadRow {
                            id: r.get(0)?,
                            board_id: r.get(1)?,
                            subject: r.get(2)?,
                            created_at: r.get(3)?,
                            bumped_at: r.get(4)?,
                            locked: r.get::<_, i64>(5)? != 0,
                            sticky: r.get::<_, i64>(6)? != 0,
                            reply_count: r.get(7)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let posts: Vec<PostRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                                media_type, created_at, deletion_token, is_op
                         FROM posts WHERE board_id = ?1 ORDER BY id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PostRow {
                            id: r.get(0)?,
                            thread_id: r.get(1)?,
                            board_id: r.get(2)?,
                            name: r.get(3)?,
                            tripcode: r.get(4)?,
                            subject: r.get(5)?,
                            body: r.get(6)?,
                            body_html: r.get(7)?,
                            ip_hash: r.get(8)?,
                            file_path: r.get(9)?,
                            file_name: r.get(10)?,
                            file_size: r.get(11)?,
                            thumb_path: r.get(12)?,
                            mime_type: r.get(13)?,
                            media_type: r.get(14)?,
                            created_at: r.get(15)?,
                            deletion_token: r.get(16)?,
                            is_op: r.get::<_, i64>(17)? != 0,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let polls: Vec<PollRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT p.id, p.thread_id, p.question, p.expires_at, p.created_at
                         FROM polls p JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY p.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollRow {
                            id: r.get(0)?,
                            thread_id: r.get(1)?,
                            question: r.get(2)?,
                            expires_at: r.get(3)?,
                            created_at: r.get(4)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let poll_options: Vec<PollOptionRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT po.id, po.poll_id, po.text, po.position
                         FROM poll_options po
                         JOIN polls p ON p.id = po.poll_id
                         JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY po.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollOptionRow {
                            id: r.get(0)?,
                            poll_id: r.get(1)?,
                            text: r.get(2)?,
                            position: r.get(3)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let poll_votes: Vec<PollVoteRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash
                         FROM poll_votes pv
                         JOIN polls p ON p.id = pv.poll_id
                         JOIN threads t ON t.id = p.thread_id
                         WHERE t.board_id = ?1 ORDER BY pv.id ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(PollVoteRow {
                            id: r.get(0)?,
                            poll_id: r.get(1)?,
                            option_id: r.get(2)?,
                            ip_hash: r.get(3)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let file_hashes: Vec<FileHashRow> = {
                let mut s = conn
                    .prepare(
                        "SELECT DISTINCT fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at
                         FROM file_hashes fh
                         JOIN posts po ON po.file_path = fh.file_path
                         WHERE po.board_id = ?1 ORDER BY fh.created_at ASC",
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s
                    .query_map(params![board_id], |r| {
                        Ok(FileHashRow {
                            sha256: r.get(0)?,
                            file_path: r.get(1)?,
                            thumb_path: r.get(2)?,
                            mime_type: r.get(3)?,
                            created_at: r.get(4)?,
                        })
                    })
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                rows
            };

            let manifest = BoardBackupManifest {
                version: 1,
                board,
                threads,
                posts,
                polls,
                poll_options,
                poll_votes,
                file_hashes,
            };
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON: {e}")))?;

            // MEM-FIX: write zip directly to a .tmp file on disk, not a Vec<u8>.
            let backup_dir = board_backup_dir();
            std::fs::create_dir_all(&backup_dir).map_err(|e| {
                AppError::Internal(anyhow::anyhow!("Create board-backups dir: {e}"))
            })?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-board-{board_short}-{ts}.zip");
            let final_path = backup_dir.join(&fname);
            let tmp_path = backup_dir.join(format!("{fname}.tmp"));

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = count_files_in_dir(&board_upload_path);
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            // +1 for board.json manifest
            progress.files_total.store(file_count.saturating_add(1), Ordering::Relaxed);

            {
                let out_file = std::io::BufWriter::new(
                    std::fs::File::create(&tmp_path).map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Create zip tmp: {e}"))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                zip.start_file("board.json", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip manifest: {e}")))?;
                zip.write_all(&manifest_json)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write manifest: {e}")))?;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(manifest_json.len() as u64, Ordering::Relaxed);

                if board_upload_path.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, &board_upload_path, opts, &progress)?;
                }

                let writer = zip
                    .finish()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Finalise zip: {e}")))?;
                writer
                    .into_inner()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Flush zip writer: {e}")))?
                    .sync_all()
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Sync zip file: {e}")))?;
            }

            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename board backup: {e}"))
            })?;

            let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
            info!("Admin created board backup: {} ({} bytes)", fname, size);
            progress.phase.store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel?backup_created=1").into_response())
}

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
pub async fn download_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path((kind, filename)): axum::extract::Path<(String, String)>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

    // Auth check.
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

    let backup_dir = match kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        _ => return Err(AppError::BadRequest("Unknown backup kind.".into())),
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
    let stream = ReaderStream::new(file);
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
                info!("Admin deleted backup file: {safe_filename}");
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

/// Restore a full backup from a saved file in full-backups/.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn restore_saved_full_backup(
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

    let path = full_backup_dir().join(&safe_filename);
    // FIX[A3]: Do NOT read the file in the async context before auth is verified.
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

            // MEM-FIX: open the zip as a seekable BufReader<File> instead of
            // reading the whole file into a Vec<u8>.  The FIX[A3] comment above
            // correctly deferred the read to after auth, but std::fs::read still
            // loaded the entire zip into heap.  A 5 GiB backup would exhaust RAM.
            let zip_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;

            let has_db = archive.file_names().any(|n| n == "chan.db");
            if !has_db {
                return Err(AppError::BadRequest(
                    "Invalid backup: zip must contain 'chan.db' at the root.".into(),
                ));
            }

            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().simple().to_string();
            let temp_db = temp_dir.join(format!("chan_restore_{tmp_id}.db"));
            let mut db_extracted = false;

            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{i}]: {e}")))?;
                let name = entry.name().to_string();
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Restore-saved: skipping suspicious entry '{name}'");
                    continue;
                }
                if name == "chan.db" {
                    let mut out = std::fs::File::create(&temp_db)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp DB: {e}")))?;
                    copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Write temp DB: {e}")))?;
                    db_extracted = true;
                } else if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() {
                        continue;
                    }
                    let target = PathBuf::from(&upload_dir).join(rel);
                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {e}")))?;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| {
                                AppError::Internal(anyhow::anyhow!("mkdir parent: {e}"))
                            })?;
                        }
                        let mut out = std::fs::File::create(&target).map_err(|e| {
                            AppError::Internal(anyhow::anyhow!(
                                "Create {}: {}",
                                target.display(),
                                e
                            ))
                        })?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES).map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Write {}: {}", target.display(), e))
                        })?;
                    }
                }
            }
            drop(archive);

            if !db_extracted {
                return Err(AppError::Internal(anyhow::anyhow!("chan.db not extracted")));
            }

            let backup_result = (|| -> Result<()> {
                let src = rusqlite::Connection::open(&temp_db)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Open source: {e}")))?;
                let backup = Backup::new(&src, &mut live_conn)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup init: {e}")))?;
                backup
                    .run_to_completion(100, std::time::Duration::from_millis(0), None)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Backup copy: {e}")))?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&temp_db);
            backup_result?;

            let fresh_sid = new_session_id();
            let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
            match db::create_session(&live_conn, &fresh_sid, admin_id, expires_at) {
                Ok(()) => {
                    info!("Admin restore-saved completed; new session for admin_id={admin_id}");
                    Ok(fresh_sid)
                }
                Err(e) => {
                    warn!("Restore-saved: could not create session: {e}");
                    Ok(String::new())
                }
            }
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
    new_cookie.set_same_site(SameSite::Strict);
    new_cookie.set_path("/");
    new_cookie.set_secure(CONFIG.https_cookies);
    // FIX[A5]: Set Max-Age to match normal login behaviour.
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));
    Ok((jar.add(new_cookie), Redirect::to("/admin/panel?restored=1")).into_response())
}

// ─── POST /admin/board/backup/restore-saved ───────────────────────────────────

/// Restore a board backup from a saved file in board-backups/.
#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub async fn restore_saved_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<RestoreSavedForm>,
) -> Result<Response> {
    fn encode_q(s: &str) -> String {
        #[allow(clippy::arithmetic_side_effects)]
        const fn nibble(n: u8) -> char {
            match n {
                0..=9 => (b'0' + n) as char,
                _ => (b'A' + n - 10) as char,
            }
        }
        s.bytes()
            .flat_map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    vec![b as char]
                }
                b' ' => vec!['+'],
                b => vec!['%', nibble(b >> 4), nibble(b & 0xf)],
            })
            .collect()
    }
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
    // FIX[A4]: Defer the blocking file read until after auth is verified inside
    // spawn_blocking — mirrors the fix applied to restore_saved_full_backup (A3).
    let upload_dir = CONFIG.upload_dir.clone();

    let board_short_result: Result<Result<String>> = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
    use board_backup_types::BoardBackupManifest;
                        use std::collections::HashMap;

            let conn = pool.get()?;
            // Auth check first — only read the file if the session is valid.
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            // MEM-FIX: open the zip as BufReader<File> instead of loading the
            // entire file into a Vec<u8>.  Board backups can be hundreds of MB.
            let zip_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;

            if !archive.file_names().any(|n| n == "board.json") {
                return Err(AppError::BadRequest(
                    "Invalid board backup: zip must contain 'board.json'.".into(),
                ));
            }

            let manifest: BoardBackupManifest = {
                let mut entry = archive
                    .by_name("board.json")
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read board.json: {e}")))?;
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut buf)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Read bytes: {e}")))?;
                serde_json::from_slice(&buf)
                    .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {e}")))?
            };

            let board_short = manifest.board.short_name.clone();

            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM boards WHERE short_name = ?1",
                    params![board_short],
                    |r| r.get(0),
                )
                .ok();

            // FIX[A6]: BEGIN IMMEDIATE must cover the DELETE + UPDATE/INSERT of the
            // board row as well as the thread/post/poll inserts.  Previously those
            // DDL statements ran outside any transaction, so a crash between the
            // DELETE and the first INSERT left the board with zero threads and no way
            // to recover without manual intervention.
            conn.execute("BEGIN IMMEDIATE", [])
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Begin tx: {e}")))?;

            let restore_result = (|| -> Result<()> {
                let live_board_id: i64 = if let Some(eid) = existing_id {
                    conn.execute("DELETE FROM threads WHERE board_id = ?1", params![eid])
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Clear threads: {e}")))?;
                    conn.execute(
                        "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                         max_threads=?4, bump_limit=?5,
                         allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
                         edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
                         allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
                         WHERE id=?16",
                        params![
                            manifest.board.name,
                            manifest.board.description,
                            i64::from(manifest.board.nsfw),
                            manifest.board.max_threads,
                            manifest.board.bump_limit,
                            i64::from(manifest.board.allow_images),
                            i64::from(manifest.board.allow_video),
                            i64::from(manifest.board.allow_audio),
                            i64::from(manifest.board.allow_tripcodes),
                            manifest.board.edit_window_secs,
                            i64::from(manifest.board.allow_editing),
                            i64::from(manifest.board.allow_archive),
                            i64::from(manifest.board.allow_video_embeds),
                            i64::from(manifest.board.allow_captcha),
                            manifest.board.post_cooldown_secs,
                            eid,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Update board: {e}")))?;
                    eid
                } else {
                    conn.execute(
                        "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                         bump_limit, allow_images, allow_video, allow_audio, allow_tripcodes,
                         edit_window_secs, allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                         post_cooldown_secs, created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        params![
                            manifest.board.short_name,
                            manifest.board.name,
                            manifest.board.description,
                            i64::from(manifest.board.nsfw),
                            manifest.board.max_threads,
                            manifest.board.bump_limit,
                            i64::from(manifest.board.allow_images),
                            i64::from(manifest.board.allow_video),
                            i64::from(manifest.board.allow_audio),
                            i64::from(manifest.board.allow_tripcodes),
                            manifest.board.edit_window_secs,
                            i64::from(manifest.board.allow_editing),
                            i64::from(manifest.board.allow_archive),
                            i64::from(manifest.board.allow_video_embeds),
                            i64::from(manifest.board.allow_captcha),
                            manifest.board.post_cooldown_secs,
                            manifest.board.created_at,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert board: {e}")))?;
                    conn.last_insert_rowid()
                };

                let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
                for t in &manifest.threads {
                    conn.execute(
                        "INSERT INTO threads (board_id, subject, created_at, bumped_at,
                         locked, sticky, reply_count) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            live_board_id,
                            t.subject,
                            t.created_at,
                            t.bumped_at,
                            i64::from(t.locked),
                            i64::from(t.sticky),
                            t.reply_count,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert thread: {e}")))?;
                    thread_id_map.insert(t.id, conn.last_insert_rowid());
                }

                for p in &manifest.posts {
                    let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown thread {}", p.thread_id))
                    })?;
                    conn.execute(
                        "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                         body, body_html, ip_hash, file_path, file_name, file_size,
                         thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                        params![
                            new_tid,
                            live_board_id,
                            p.name,
                            p.tripcode,
                            p.subject,
                            p.body,
                            p.body_html,
                            p.ip_hash,
                            p.file_path,
                            p.file_name,
                            p.file_size,
                            p.thumb_path,
                            p.mime_type,
                            p.media_type,
                            p.created_at,
                            p.deletion_token,
                            i64::from(p.is_op),
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert post: {e}")))?;
                }

                let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
                for p in &manifest.polls {
                    let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown thread {}", p.thread_id))
                    })?;
                    conn.execute(
                        "INSERT INTO polls (thread_id, question, expires_at, created_at)
                         VALUES (?1,?2,?3,?4)",
                        params![new_tid, p.question, p.expires_at, p.created_at],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert poll: {e}")))?;
                    poll_id_map.insert(p.id, conn.last_insert_rowid());
                }

                let mut option_id_map: HashMap<i64, i64> = HashMap::new();
                for o in &manifest.poll_options {
                    let new_poll_id = *poll_id_map.get(&o.poll_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown poll {}", o.poll_id))
                    })?;
                    conn.execute(
                        "INSERT INTO poll_options (poll_id, text, position) VALUES (?1,?2,?3)",
                        params![new_poll_id, o.text, o.position],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert option: {e}")))?;
                    option_id_map.insert(o.id, conn.last_insert_rowid());
                }

                for v in &manifest.poll_votes {
                    let new_poll_id = *poll_id_map.get(&v.poll_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown poll {}", v.poll_id))
                    })?;
                    let new_option_id = *option_id_map.get(&v.option_id).ok_or_else(|| {
                        AppError::Internal(anyhow::anyhow!("Unknown option {}", v.option_id))
                    })?;
                    conn.execute(
                        "INSERT OR IGNORE INTO poll_votes
                         (poll_id, option_id, ip_hash) VALUES (?1,?2,?3)",
                        params![new_poll_id, new_option_id, v.ip_hash],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert vote: {e}")))?;
                }

                for fh in &manifest.file_hashes {
                    conn.execute(
                        "INSERT OR IGNORE INTO file_hashes
                         (sha256, file_path, thumb_path, mime_type, created_at)
                         VALUES (?1,?2,?3,?4,?5)",
                        params![
                            fh.sha256,
                            fh.file_path,
                            fh.thumb_path,
                            fh.mime_type,
                            fh.created_at,
                        ],
                    )
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert file_hash: {e}")))?;
                }
                Ok(())
            })();

            match restore_result {
                Ok(()) => {
                    conn.execute("COMMIT", [])
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Commit: {e}")))?;
                }
                Err(e) => {
                    let _ = conn.execute("ROLLBACK", []);
                    return Err(e);
                }
            }

            // Extract upload files.
            for i in 0..archive.len() {
                let mut entry = archive
                    .by_index(i)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{i}]: {e}")))?;
                let name = entry.name().to_string();
                if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                    warn!("Board restore-saved: skipping suspicious entry '{name}'");
                    continue;
                }
                if let Some(rel) = name.strip_prefix("uploads/") {
                    if rel.is_empty() {
                        continue;
                    }
                    let target = PathBuf::from(&upload_dir).join(rel);
                    if entry.is_dir() {
                        std::fs::create_dir_all(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("mkdir: {e}")))?;
                    } else {
                        if let Some(p) = target.parent() {
                            std::fs::create_dir_all(p).map_err(|e| {
                                AppError::Internal(anyhow::anyhow!("mkdir parent: {e}"))
                            })?;
                        }
                        let mut out = std::fs::File::create(&target)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Create: {e}")))?;
                        copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Write: {e}")))?;
                    }
                }
            }

            info!("Admin board restore-saved completed for /{board_short}/");
            let safe_short: String = board_short
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(8)
                .collect();
            Ok(safe_short)
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

/// Flat structs used exclusively for board-level backup serialisation.
mod board_backup_types {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    #[allow(clippy::struct_excessive_bools)]
    pub struct BoardRow {
        pub id: i64,
        pub short_name: String,
        pub name: String,
        pub description: String,
        pub nsfw: bool,
        pub max_threads: i64,
        pub bump_limit: i64,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_images: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_video: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default false.
        #[serde(default)]
        pub allow_audio: bool,
        /// Added via ALTER TABLE — absent in oldest backups; default true.
        #[serde(default = "default_true")]
        pub allow_tripcodes: bool,
        /// Added in a later version — absent in older backups; default to 300 s.
        #[serde(default = "default_edit_window_secs")]
        pub edit_window_secs: i64,
        /// Added in a later version — absent in older backups; default to false.
        #[serde(default)]
        pub allow_editing: bool,
        /// Added in a later version — absent in older backups; default to true.
        #[serde(default = "default_true")]
        pub allow_archive: bool,
        /// Added in v1.0.10 — absent in older backups; default to false.
        #[serde(default)]
        pub allow_video_embeds: bool,
        /// Added in v1.0.10 — absent in older backups; default to false.
        #[serde(default)]
        pub allow_captcha: bool,
        /// Added for per-board post cooldowns — absent in older backups; default 0 (disabled).
        #[serde(default)]
        pub post_cooldown_secs: i64,
        pub created_at: i64,
    }

    const fn default_true() -> bool {
        true
    }

    const fn default_edit_window_secs() -> i64 {
        300
    }
    #[derive(Serialize, Deserialize)]
    pub struct ThreadRow {
        pub id: i64,
        pub board_id: i64,
        pub subject: Option<String>,
        pub created_at: i64,
        pub bumped_at: i64,
        pub locked: bool,
        pub sticky: bool,
        pub reply_count: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PostRow {
        pub id: i64,
        pub thread_id: i64,
        pub board_id: i64,
        pub name: String,
        pub tripcode: Option<String>,
        pub subject: Option<String>,
        pub body: String,
        pub body_html: String,
        pub ip_hash: String,
        pub file_path: Option<String>,
        pub file_name: Option<String>,
        pub file_size: Option<i64>,
        pub thumb_path: Option<String>,
        pub mime_type: Option<String>,
        pub media_type: Option<String>,
        pub created_at: i64,
        pub deletion_token: String,
        pub is_op: bool,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollRow {
        pub id: i64,
        pub thread_id: i64,
        pub question: String,
        pub expires_at: i64,
        pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollOptionRow {
        pub id: i64,
        pub poll_id: i64,
        pub text: String,
        pub position: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct PollVoteRow {
        pub id: i64,
        pub poll_id: i64,
        pub option_id: i64,
        pub ip_hash: String,
    }
    #[derive(Serialize, Deserialize)]
    pub struct FileHashRow {
        pub sha256: String,
        pub file_path: String,
        pub thumb_path: String,
        pub mime_type: String,
        pub created_at: i64,
    }
    #[derive(Serialize, Deserialize)]
    pub struct BoardBackupManifest {
        pub version: u32,
        pub board: BoardRow,
        pub threads: Vec<ThreadRow>,
        pub posts: Vec<PostRow>,
        pub polls: Vec<PollRow>,
        pub poll_options: Vec<PollOptionRow>,
        pub poll_votes: Vec<PollVoteRow>,
        pub file_hashes: Vec<FileHashRow>,
    }
}

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
    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    let (tmp_path, filename, file_size) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(PathBuf, String, u64)> {
    use board_backup_types::{BoardRow, ThreadRow, PostRow, PollRow, PollOptionRow, PollVoteRow, FileHashRow, BoardBackupManifest};

            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            let board: BoardRow = conn.query_row(
                "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                        allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                        allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                        post_cooldown_secs, created_at
                 FROM boards WHERE short_name = ?1",
                params![board_short],
                |r| Ok(BoardRow {
                    id: r.get(0)?,
                    short_name: r.get(1)?,
                    name: r.get(2)?,
                    description: r.get(3)?,
                    nsfw: r.get::<_, i64>(4)? != 0,
                    max_threads: r.get(5)?,
                    bump_limit: r.get(6)?,
                    allow_images: r.get::<_, i64>(7)? != 0,
                    allow_video: r.get::<_, i64>(8)? != 0,
                    allow_audio: r.get::<_, i64>(9)? != 0,
                    allow_tripcodes: r.get::<_, i64>(10)? != 0,
                    edit_window_secs: r.get(11)?,
                    allow_editing: r.get::<_, i64>(12)? != 0,
                    allow_archive: r.get::<_, i64>(13)? != 0,
                    allow_video_embeds: r.get::<_, i64>(14)? != 0,
                    allow_captcha: r.get::<_, i64>(15)? != 0,
                    post_cooldown_secs: r.get(16)?,
                    created_at: r.get(17)?,
                }),
            ).map_err(|_| AppError::NotFound(format!("Board '{board_short}' not found")))?;

            let board_id = board.id;

            // ── Threads ───────────────────────────────────────────────────
            let threads: Vec<ThreadRow> = {
                let mut s = conn.prepare(
                    "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, reply_count
                     FROM threads WHERE board_id = ?1 ORDER BY id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(ThreadRow {
                    id: r.get(0)?, board_id: r.get(1)?, subject: r.get(2)?,
                    created_at: r.get(3)?, bumped_at: r.get(4)?,
                    locked: r.get::<_,i64>(5)? != 0, sticky: r.get::<_,i64>(6)? != 0,
                    reply_count: r.get(7)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Posts ─────────────────────────────────────────────────────
            let posts: Vec<PostRow> = {
                let mut s = conn.prepare(
                    "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                            ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                            media_type, created_at, deletion_token, is_op
                     FROM posts WHERE board_id = ?1 ORDER BY id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PostRow {
                    id: r.get(0)?, thread_id: r.get(1)?, board_id: r.get(2)?,
                    name: r.get(3)?, tripcode: r.get(4)?, subject: r.get(5)?,
                    body: r.get(6)?, body_html: r.get(7)?, ip_hash: r.get(8)?,
                    file_path: r.get(9)?, file_name: r.get(10)?, file_size: r.get(11)?,
                    thumb_path: r.get(12)?, mime_type: r.get(13)?, media_type: r.get(14)?,
                    created_at: r.get(15)?, deletion_token: r.get(16)?,
                    is_op: r.get::<_,i64>(17)? != 0,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Polls ─────────────────────────────────────────────────────
            let polls: Vec<PollRow> = {
                let mut s = conn.prepare(
                    "SELECT p.id, p.thread_id, p.question, p.expires_at, p.created_at
                     FROM polls p JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY p.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollRow {
                    id: r.get(0)?, thread_id: r.get(1)?, question: r.get(2)?,
                    expires_at: r.get(3)?, created_at: r.get(4)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Poll options ──────────────────────────────────────────────
            let poll_options: Vec<PollOptionRow> = {
                let mut s = conn.prepare(
                    "SELECT po.id, po.poll_id, po.text, po.position
                     FROM poll_options po
                     JOIN polls p ON p.id = po.poll_id
                     JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY po.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollOptionRow {
                    id: r.get(0)?, poll_id: r.get(1)?, text: r.get(2)?, position: r.get(3)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Poll votes ────────────────────────────────────────────────
            let poll_votes: Vec<PollVoteRow> = {
                let mut s = conn.prepare(
                    "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash
                     FROM poll_votes pv
                     JOIN polls p ON p.id = pv.poll_id
                     JOIN threads t ON t.id = p.thread_id
                     WHERE t.board_id = ?1 ORDER BY pv.id ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(PollVoteRow {
                    id: r.get(0)?, poll_id: r.get(1)?, option_id: r.get(2)?,
                    ip_hash: r.get(3)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── File hashes referenced by this board ──────────────────────
            let file_hashes: Vec<FileHashRow> = {
                let mut s = conn.prepare(
                    "SELECT DISTINCT fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at
                     FROM file_hashes fh
                     JOIN posts po ON po.file_path = fh.file_path
                     WHERE po.board_id = ?1 ORDER BY fh.created_at ASC"
                ).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
                let rows = s.query_map(params![board_id], |r| Ok(FileHashRow {
                    sha256: r.get(0)?, file_path: r.get(1)?, thumb_path: r.get(2)?,
                    mime_type: r.get(3)?, created_at: r.get(4)?,
                })).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
                .collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?; rows
            };

            // ── Serialise manifest ────────────────────────────────────────
            let manifest = BoardBackupManifest {
                version: 1, board, threads, posts, polls,
                poll_options, poll_votes, file_hashes,
            };

            // ── Build zip to NamedTempFile (MEM-FIX) ─────────────────────
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("JSON serialise: {e}")))?;

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = count_files_in_dir(&board_upload_path);
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            progress.files_total.store(file_count.saturating_add(1), Ordering::Relaxed);

            let zip_tmp = tempfile::NamedTempFile::new()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("Create temp zip: {e}")))?;
            {
                let out_file = std::io::BufWriter::new(
                    zip_tmp.as_file().try_clone().map_err(|e| {
                        AppError::Internal(anyhow::anyhow!("Clone temp file handle: {e}"))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                zip.start_file("board.json", opts)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip manifest: {e}")))?;
                zip.write_all(&manifest_json)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Write manifest: {e}")))?;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(manifest_json.len() as u64, Ordering::Relaxed);

                if board_upload_path.exists() {
                    add_dir_to_zip(&mut zip, uploads_base, &board_upload_path, opts, &progress)?;
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

            let (_, tmp_path_obj) = zip_tmp.into_parts();
            let final_path = tmp_path_obj.keep().map_err(|e| {
                AppError::Internal(anyhow::anyhow!("Persist temp zip: {e}"))
            })?;

            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            let fname = format!("rustchan-board-{board_short}-{ts}.zip");
            info!("Admin downloaded board backup for /{}/  ({} bytes on disk)", board_short, file_size);
            progress.phase.store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            Ok((final_path, fname, file_size))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let file = tokio::fs::File::open(&tmp_path)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Open board backup for streaming: {e}")))?;
    let stream = ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

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
    mut multipart: Multipart,
) -> Response {
    #[allow(clippy::arithmetic_side_effects)]
    const fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            _ => (b'A' + n - 10) as char,
        }
    }
    fn encode_q(s: &str) -> String {
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

    // Run the whole operation as a fallible async block so any early return
    // with Err(...) is caught below and turned into a redirect.
    let result: Result<String> = async {
        let session_id = jar
            .get(super::SESSION_COOKIE)
            .map(|c| c.value().to_string());
        let upload_dir = CONFIG.upload_dir.clone();

        // MEM-FIX: stream the uploaded file to a NamedTempFile on disk instead
        // of buffering the entire zip into a Vec<u8>.  Board backups can be
        // hundreds of MB for active boards with many uploads.
        let mut zip_tmp: Option<tempfile::NamedTempFile> = None;
        let mut form_csrf: Option<String> = None;

        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(format!("Multipart error: {e}")))?
        {
            match field.name() {
                Some("_csrf") => {
                    form_csrf = Some(
                        field
                            .text()
                            .await
                            .map_err(|e| AppError::BadRequest(e.to_string()))?,
                    );
                }
                Some("backup_file") => {
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
                _ => {}
            }
        }

        let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
        if !crate::middleware::validate_csrf(
            csrf_cookie.as_deref(),
            form_csrf.as_deref().unwrap_or(""),
        ) {
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

        tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || -> Result<String> {
                use board_backup_types::BoardBackupManifest;
                use std::collections::HashMap;
                use std::io::Read;

                let conn = pool.get()?;
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
                let (manifest, mut archive_opt): (
                    BoardBackupManifest,
                    Option<zip::ZipArchive<std::io::BufReader<std::fs::File>>>,
                ) = if is_zip {
                    let f = zip_tmp
                        .reopen()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip: {e}")))?;
                    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(f))
                        .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;
                    if !archive.file_names().any(|n| n == "board.json") {
                        return Err(AppError::BadRequest(
                            "Invalid board backup: zip must contain 'board.json'. \
                             (Did you upload a full-site backup instead?)"
                                .into(),
                        ));
                    }
                    let manifest: BoardBackupManifest = {
                        let mut entry = archive.by_name("board.json").map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Read board.json: {e}"))
                        })?;
                        let mut buf = Vec::new();
                        entry
                            .read_to_end(&mut buf)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Read bytes: {e}")))?;
                        serde_json::from_slice(&buf)
                            .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {e}")))?
                    };
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
                    let mut buf = Vec::new();
                    f.read_to_end(&mut buf)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Read json: {e}")))?;
                    let manifest: BoardBackupManifest = serde_json::from_slice(&buf)
                        .map_err(|e| AppError::BadRequest(format!("Invalid board.json: {e}")))?;
                    (manifest, None)
                };

                let board_short = manifest.board.short_name.clone();

                let existing_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM boards WHERE short_name = ?1",
                        params![board_short],
                        |r| r.get(0),
                    )
                    .ok();

                // FIX[A6]: BEGIN IMMEDIATE must cover the DELETE + UPDATE/INSERT of the
                // board row.  Previously those statements ran outside any transaction.
                conn.execute("BEGIN IMMEDIATE", [])
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Begin tx: {e}")))?;

                let restore_result = (|| -> Result<()> {
                    let live_board_id: i64 = if let Some(eid) = existing_id {
                        conn.execute("DELETE FROM threads WHERE board_id = ?1", params![eid])
                            .map_err(|e| {
                                AppError::Internal(anyhow::anyhow!("Clear threads: {e}"))
                            })?;
                        conn.execute(
                            "UPDATE boards SET name=?1, description=?2, nsfw=?3,
                             max_threads=?4, bump_limit=?5,
                             allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
                             edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
                             allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
                             WHERE id=?16",
                            params![
                                manifest.board.name,
                                manifest.board.description,
                                i64::from(manifest.board.nsfw),
                                manifest.board.max_threads,
                                manifest.board.bump_limit,
                                i64::from(manifest.board.allow_images),
                                i64::from(manifest.board.allow_video),
                                i64::from(manifest.board.allow_audio),
                                i64::from(manifest.board.allow_tripcodes),
                                manifest.board.edit_window_secs,
                                i64::from(manifest.board.allow_editing),
                                i64::from(manifest.board.allow_archive),
                                i64::from(manifest.board.allow_video_embeds),
                                i64::from(manifest.board.allow_captcha),
                                manifest.board.post_cooldown_secs,
                                eid,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Update board: {e}")))?;
                        eid
                    } else {
                        conn.execute(
                            "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                             bump_limit, allow_images, allow_video, allow_audio, allow_tripcodes,
                             edit_window_secs, allow_editing, allow_archive, allow_video_embeds,
                             allow_captcha, post_cooldown_secs, created_at)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                            params![
                                manifest.board.short_name,
                                manifest.board.name,
                                manifest.board.description,
                                i64::from(manifest.board.nsfw),
                                manifest.board.max_threads,
                                manifest.board.bump_limit,
                                i64::from(manifest.board.allow_images),
                                i64::from(manifest.board.allow_video),
                                i64::from(manifest.board.allow_audio),
                                i64::from(manifest.board.allow_tripcodes),
                                manifest.board.edit_window_secs,
                                i64::from(manifest.board.allow_editing),
                                i64::from(manifest.board.allow_archive),
                                i64::from(manifest.board.allow_video_embeds),
                                i64::from(manifest.board.allow_captcha),
                                manifest.board.post_cooldown_secs,
                                manifest.board.created_at,
                            ],
                        )
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Insert board: {e}")))?;
                        conn.last_insert_rowid()
                    };

                    let mut thread_id_map: HashMap<i64, i64> = HashMap::new();
                    for t in &manifest.threads {
                        conn.execute(
                            "INSERT INTO threads (board_id, subject, created_at, bumped_at,
                             locked, sticky, reply_count)
                             VALUES (?1,?2,?3,?4,?5,?6,?7)",
                            params![
                                live_board_id,
                                t.subject,
                                t.created_at,
                                t.bumped_at,
                                i64::from(t.locked),
                                i64::from(t.sticky),
                                t.reply_count,
                            ],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert thread {}: {e}", t.id))
                        })?;
                        thread_id_map.insert(t.id, conn.last_insert_rowid());
                    }

                    // ── Insert posts, recording old → new ID mapping ──────
                    //
                    // Board restore cannot reuse original post IDs because
                    // other boards' posts may already occupy those rows in the
                    // global `posts` table (SQLite AUTOINCREMENT is site-wide,
                    // not per-board).  The posts therefore land at new IDs.
                    //
                    // `post_id_map` captures every (old_id → new_id) pair so
                    // that we can fix up in-board quotelink references in
                    // `body` and `body_html` in the second pass below.
                    // Without this, `>>500` in a restored post still points to
                    // old ID 500 which no longer exists — clicks produce 404s
                    // and hover previews show "Post not found".
                    let mut post_id_map: HashMap<i64, i64> = HashMap::new();
                    for p in &manifest.posts {
                        let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "Post {} refs unknown thread {}",
                                p.id,
                                p.thread_id
                            ))
                        })?;
                        conn.execute(
                            "INSERT INTO posts (thread_id, board_id, name, tripcode, subject,
                             body, body_html, ip_hash, file_path, file_name, file_size,
                             thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                            params![
                                new_tid,
                                live_board_id,
                                p.name,
                                p.tripcode,
                                p.subject,
                                p.body,
                                p.body_html,
                                p.ip_hash,
                                p.file_path,
                                p.file_name,
                                p.file_size,
                                p.thumb_path,
                                p.mime_type,
                                p.media_type,
                                p.created_at,
                                p.deletion_token,
                                i64::from(p.is_op),
                            ],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert post {}: {e}", p.id))
                        })?;
                        post_id_map.insert(p.id, conn.last_insert_rowid());
                    }

                    // ── Quotelink fixup pass ──────────────────────────────
                    //
                    // If any post IDs changed (which they almost always do
                    // when restoring into a live DB), rewrite `body` and
                    // `body_html` for every restored post so that in-board
                    // quotelinks point at the new IDs.
                    //
                    // Only same-board references are remapped.  Cross-board
                    // links (`>>>/board/N`) point to other boards whose IDs
                    // are unchanged; they are deliberately left untouched.
                    let any_changed = post_id_map.iter().any(|(old, new)| old != new);
                    if any_changed {
                        // Sort by old-ID string length descending so that
                        // longer IDs are replaced before any prefix of theirs.
                        // e.g. replace >>1000 before >>100 before >>10 before >>1.
                        let mut pairs: Vec<(String, String)> = post_id_map
                            .iter()
                            .filter(|(old, new)| old != new)
                            .map(|(old, new)| (old.to_string(), new.to_string()))
                            .collect();
                        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.0.cmp(&a.0)));

                        for p in &manifest.posts {
                            let Some(&new_post_id) = post_id_map.get(&p.id) else {
                                continue;
                            };

                            let new_body = remap_body_quotelinks(&p.body, &pairs);
                            let new_body_html = remap_body_html_quotelinks(&p.body_html, &pairs);

                            // Only issue the UPDATE when the text actually
                            // changed — avoids unnecessary I/O when none of
                            // the post IDs appear in this post's body.
                            if new_body != p.body || new_body_html != p.body_html {
                                conn.execute(
                                    "UPDATE posts SET body = ?1, body_html = ?2 WHERE id = ?3",
                                    params![new_body, new_body_html, new_post_id],
                                )
                                .map_err(|e| {
                                    AppError::Internal(anyhow::anyhow!(
                                        "Fixup quotelinks for post {new_post_id}: {e}"
                                    ))
                                })?;
                            }
                        }
                    }

                    let mut poll_id_map: HashMap<i64, i64> = HashMap::new();
                    for p in &manifest.polls {
                        let new_tid = *thread_id_map.get(&p.thread_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "Poll {} refs unknown thread {}",
                                p.id,
                                p.thread_id
                            ))
                        })?;
                        conn.execute(
                            "INSERT INTO polls (thread_id, question, expires_at, created_at)
                             VALUES (?1,?2,?3,?4)",
                            params![new_tid, p.question, p.expires_at, p.created_at],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert poll {}: {e}", p.id))
                        })?;
                        poll_id_map.insert(p.id, conn.last_insert_rowid());
                    }

                    let mut option_id_map: HashMap<i64, i64> = HashMap::new();
                    for o in &manifest.poll_options {
                        let new_poll_id = *poll_id_map.get(&o.poll_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "Option {} refs unknown poll {}",
                                o.id,
                                o.poll_id
                            ))
                        })?;
                        conn.execute(
                            "INSERT INTO poll_options (poll_id, text, position) VALUES (?1,?2,?3)",
                            params![new_poll_id, o.text, o.position],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert option {}: {e}", o.id))
                        })?;
                        option_id_map.insert(o.id, conn.last_insert_rowid());
                    }

                    for v in &manifest.poll_votes {
                        let new_poll_id = *poll_id_map.get(&v.poll_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "Vote {} refs unknown poll {}",
                                v.id,
                                v.poll_id
                            ))
                        })?;
                        let new_option_id = *option_id_map.get(&v.option_id).ok_or_else(|| {
                            AppError::Internal(anyhow::anyhow!(
                                "Vote {} refs unknown option {}",
                                v.id,
                                v.option_id
                            ))
                        })?;
                        conn.execute(
                            "INSERT OR IGNORE INTO poll_votes
                             (poll_id, option_id, ip_hash) VALUES (?1,?2,?3)",
                            params![new_poll_id, new_option_id, v.ip_hash],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert vote {}: {e}", v.id))
                        })?;
                    }

                    for fh in &manifest.file_hashes {
                        conn.execute(
                            "INSERT OR IGNORE INTO file_hashes
                             (sha256, file_path, thumb_path, mime_type, created_at)
                             VALUES (?1,?2,?3,?4,?5)",
                            params![
                                fh.sha256,
                                fh.file_path,
                                fh.thumb_path,
                                fh.mime_type,
                                fh.created_at
                            ],
                        )
                        .map_err(|e| {
                            AppError::Internal(anyhow::anyhow!("Insert file_hash: {e}"))
                        })?;
                    }
                    Ok(())
                })();

                match restore_result {
                    Ok(()) => {
                        conn.execute("COMMIT", [])
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Commit tx: {e}")))?;
                    }
                    Err(e) => {
                        let _ = conn.execute("ROLLBACK", []);
                        return Err(e);
                    }
                }

                if let Some(ref mut archive) = archive_opt {
                    for i in 0..archive.len() {
                        let mut entry = archive
                            .by_index(i)
                            .map_err(|e| AppError::Internal(anyhow::anyhow!("Zip[{i}]: {e}")))?;
                        let name = entry.name().to_string();
                        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
                            warn!("Board restore: skipping suspicious entry '{name}'");
                            continue;
                        }
                        if let Some(rel) = name.strip_prefix("uploads/") {
                            if rel.is_empty() {
                                continue;
                            }
                            let target = PathBuf::from(&upload_dir).join(rel);
                            if entry.is_dir() {
                                std::fs::create_dir_all(&target).map_err(|e| {
                                    AppError::Internal(anyhow::anyhow!("mkdir: {e}"))
                                })?;
                            } else {
                                if let Some(p) = target.parent() {
                                    std::fs::create_dir_all(p).map_err(|e| {
                                        AppError::Internal(anyhow::anyhow!("mkdir parent: {e}"))
                                    })?;
                                }
                                let mut out = std::fs::File::create(&target).map_err(|e| {
                                    AppError::Internal(anyhow::anyhow!("Create file: {e}"))
                                })?;
                                copy_limited(&mut entry, &mut out, ZIP_ENTRY_MAX_BYTES).map_err(
                                    |e| AppError::Internal(anyhow::anyhow!("Write file: {e}")),
                                )?;
                            }
                        }
                    }
                }

                info!("Admin board restore completed for /{board_short}/");
                // Refresh live board list — board_restore may have created a
                // board that didn't exist before, so the top bar must update.
                if let Ok(boards) = db::get_all_boards(&conn) {
                    crate::templates::set_live_boards(boards);
                }
                let safe_short: String = board_short
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .take(8)
                    .collect();
                Ok(safe_short)
            }
        })
        .await
        .unwrap_or_else(|e| Err(AppError::Internal(anyhow::anyhow!("Task panicked: {e}"))))
    }
    .await;

    match result {
        Ok(board_short) => {
            Redirect::to(&format!("/admin/panel?board_restored={board_short}")).into_response()
        }
        Err(e) => Redirect::to(&format!(
            "/admin/panel?restore_error={}",
            encode_q(&e.to_string())
        ))
        .into_response(),
    }
}
