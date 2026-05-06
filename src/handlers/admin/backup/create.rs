// Public re-exports here match the module layout and keep paths stable for callers.
#![allow(clippy::redundant_pub_crate, clippy::too_many_lines)]
use super::*;

pub(crate) fn create_full_backup_to_server(
    pool: &crate::db::DbPool,
    session_id: Option<&str>,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    copies_to_keep: u64,
    include_tor_hidden_service_keys: bool,
) -> Result<String> {
    let conn = pool.get()?;
    if let Some(session_id) = session_id {
        super::super::require_admin_session_sid(&conn, Some(session_id))?;
    }
    let uploads_base = std::path::Path::new(&CONFIG.upload_dir);
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
    let tor_hidden_service_keys_dir = if include_tor_hidden_service_keys {
        match super::common::resolve_tor_hidden_service_keys_availability(
            true,
            crate::config::configured_tor_hidden_service_keys_dir(),
            "Tor hidden service key backups are not available with the current configuration.",
        )? {
            super::common::TorHiddenServiceKeysAvailability::Skipped => None,
            super::common::TorHiddenServiceKeysAvailability::Available(dir) => Some(dir),
        }
    } else {
        None
    };

    progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
    log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);

    let temp_dir = std::env::temp_dir();
    let tmp_id = uuid::Uuid::new_v4().simple().to_string();
    let temp_db = temp_dir.join(format!("chan_backup_{tmp_id}.db"));
    let temp_db_str = temp_db
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path non-UTF-8")))?
        .replace('\'', "''");

    conn.execute_batch(&format!("VACUUM INTO '{temp_db_str}'"))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {error}")))?;

    progress.reset(crate::middleware::backup_phase::COUNT_FILES);
    log_backup_phase(crate::middleware::backup_phase::COUNT_FILES);
    let global_banner_dir = crate::banner::backup_source_dir();
    let favicon_file_count = super::count_files_in_dir(&global_favicon_dir);
    let banner_file_count = super::count_files_in_dir(&global_banner_dir);
    let tor_hidden_service_key_file_count = if let Some(dir) = tor_hidden_service_keys_dir.as_ref()
    {
        count_required_private_files(
            dir,
            "Tor hidden service keys were requested, but the configured identity directory could not be read.",
        )?
    } else {
        0
    };
    let file_count = super::count_files_in_dir(uploads_base)
        .saturating_add(favicon_file_count)
        .saturating_add(banner_file_count)
        .saturating_add(tor_hidden_service_key_file_count);
    let db_snapshot_size = std::fs::metadata(&temp_db)
        .map(|metadata| metadata.len())
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Stat DB snapshot: {error}")))?;
    let manifest = build_full_backup_manifest(
        &conn,
        db_snapshot_size,
        full_backup_upload_file_count(
            file_count,
            favicon_file_count,
            banner_file_count,
            tor_hidden_service_key_file_count,
        ),
        favicon_file_count,
        banner_file_count,
        include_tor_hidden_service_keys,
        tor_hidden_service_key_file_count,
    )?;
    drop(conn);
    progress
        .files_total
        .store(file_count.saturating_add(2), Ordering::Relaxed);

    let backup_dir = super::full_backup_dir();
    std::fs::create_dir_all(&backup_dir)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Create full backup dir: {error}")))?;
    let ts = super::local_backup_timestamp_label();
    let filename = super::unique_backup_filename(&backup_dir, &format!("rustchan-backup-{ts}.zip"));
    let final_path = backup_dir.join(&filename);
    let tmp_path = backup_dir.join(format!("{filename}.tmp"));

    let build_result = (|| -> Result<()> {
        let out_file = std::io::BufWriter::new(
            std::fs::File::create(&tmp_path)
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Create zip tmp: {error}")))?,
        );
        let mut zip = zip::ZipWriter::new(out_file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        progress.reset(crate::middleware::backup_phase::COMPRESS);
        log_backup_phase(crate::middleware::backup_phase::COMPRESS);
        progress
            .files_total
            .store(file_count.saturating_add(2), Ordering::Relaxed);

        let manifest_json = serde_json::to_vec_pretty(&manifest).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Serialize full backup manifest: {error}"))
        })?;
        zip.start_file(super::common::FULL_BACKUP_MANIFEST_NAME, opts)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip backup manifest: {error}")))?;
        zip.write_all(&manifest_json).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Write backup manifest: {error}"))
        })?;

        zip.start_file("chan.db", opts)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip DB: {error}")))?;
        let mut db_src = std::fs::File::open(&temp_db)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Open DB snapshot: {error}")))?;
        let copied = std::io::copy(&mut db_src, &mut zip)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Stream DB to zip: {error}")))?;
        drop(db_src);
        let _ = std::fs::remove_file(&temp_db);
        progress.files_done.fetch_add(1, Ordering::Relaxed);
        progress.bytes_done.fetch_add(copied, Ordering::Relaxed);
        log_backup_progress(progress);

        if uploads_base.exists() {
            super::add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, progress)?;
        }
        if global_favicon_dir.exists() {
            super::add_dir_to_zip_with_prefix(
                &mut zip,
                &global_favicon_dir,
                &global_favicon_dir,
                "favicon",
                opts,
                progress,
            )?;
        }
        if global_banner_dir.exists() {
            super::add_dir_to_zip_with_prefix(
                &mut zip,
                &global_banner_dir,
                &global_banner_dir,
                "banner",
                opts,
                progress,
            )?;
        }
        if let Some(tor_hidden_service_keys_dir) = tor_hidden_service_keys_dir.as_ref() {
            super::add_dir_to_zip_with_prefix(
                &mut zip,
                tor_hidden_service_keys_dir,
                tor_hidden_service_keys_dir,
                super::common::FULL_BACKUP_TOR_KEYS_PREFIX,
                opts,
                progress,
            )?;
        }

        let writer = zip
            .finish()
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Finalise zip: {error}")))?;
        writer
            .into_inner()
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Flush zip writer: {error}")))?
            .sync_all()
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Sync zip file: {error}")))?;
        Ok(())
    })();

    if let Err(error) = build_result {
        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_file(&temp_db);
        return Err(error);
    }

    if let Err(error) = super::common::verify_full_backup_zip(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_file(&temp_db);
        return Err(error);
    }

    std::fs::rename(&tmp_path, &final_path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_file(&temp_db);
        AppError::Internal(anyhow::anyhow!("Rename backup: {error}"))
    })?;
    super::invalidate_backup_list_cache(&backup_dir, super::BackupListKind::Full);

    match super::enforce_full_backup_retention(copies_to_keep) {
        Ok(removed) if !removed.is_empty() => {
            tracing::info!(
                target: "admin",
                removed = removed.len(),
                copies_to_keep = copies_to_keep.max(1),
                "Trimmed older saved full backups after creating a new saved full backup"
            );
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(
                target: "admin",
                error = %error,
                copies_to_keep = copies_to_keep.max(1),
                "Full backup retention trim failed after creating a saved full backup"
            );
        }
    }

    let size = std::fs::metadata(&final_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    tracing::info!(
        target: "admin",
        filename = %filename,
        bytes = size,
        automated = session_id.is_none(),
        includes_tor_hidden_service_keys = include_tor_hidden_service_keys,
        "Full backup created"
    );
    progress
        .phase
        .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
    log_backup_phase(crate::middleware::backup_phase::DONE);
    Ok(filename)
}

const fn full_backup_upload_file_count(
    total_file_count: u64,
    favicon_file_count: u64,
    banner_file_count: u64,
    tor_hidden_service_key_file_count: u64,
) -> u64 {
    total_file_count
        .saturating_sub(favicon_file_count)
        .saturating_sub(banner_file_count)
        .saturating_sub(tor_hidden_service_key_file_count)
}

fn count_required_private_files(dir: &Path, missing_message: &str) -> Result<u64> {
    fn count_recursive(dir: &Path) -> Result<u64> {
        crate::utils::fs_security::assert_dir_no_symlink(dir).map_err(|error| {
            AppError::BadRequest(format!("Private backup directory is unsafe: {error}"))
        })?;
        let entries = std::fs::read_dir(dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read {}: {error}", dir.display()))
        })?;
        let mut count = 0u64;
        for entry in entries {
            let entry = entry
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Read dir entry: {error}")))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Inspect {}: {error}", path.display()))
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_dir() {
                count = count.saturating_add(count_recursive(&path)?);
            } else if metadata.file_type().is_file()
                && crate::utils::fs_security::assert_regular_file_no_symlink(&path).is_ok()
            {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    if !dir.exists() {
        return Err(AppError::BadRequest(missing_message.into()));
    }
    if !dir.is_dir() {
        return Err(AppError::BadRequest(missing_message.into()));
    }

    let count = count_recursive(dir)?;
    if count == 0 {
        return Err(AppError::BadRequest(missing_message.into()));
    }
    Ok(count)
}

#[derive(Deserialize)]
pub struct FullBackupCreateForm {
    #[serde(default, deserialize_with = "super::form_checkbox_bool")]
    include_tor_hidden_service_keys: bool,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
pub async fn create_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<FullBackupCreateForm>,
) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Full backup creation")?;
    let session_id = jar
        .get(super::super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::super::require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.csrf.as_deref(),
    )?;
    let progress = state.backup_progress.clone();
    let copies_to_keep = state.auto_full_backup_settings.snapshot().copies_to_keep;
    let include_tor_hidden_service_keys = form.include_tor_hidden_service_keys;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            create_full_backup_to_server(
                &pool,
                session_id.as_deref(),
                &progress,
                copies_to_keep,
                include_tor_hidden_service_keys,
            )?;
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(super::super::admin_panel_redirect("Full backup saved on the server.").into_response())
}

#[derive(Deserialize)]
pub struct BoardBackupCreateForm {
    board_short: String,
    download_after_create: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
pub async fn create_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BoardBackupCreateForm>,
) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Board backup creation")?;

    let session_id = jar
        .get(super::super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::super::require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.csrf.as_deref(),
    )?;

    let board_short = form
        .board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if board_short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }
    let board_short_for_flash = board_short.clone();
    let download_after_create = form.download_after_create.as_deref() == Some("1");

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    let filename = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::super::require_admin_session_sid(&conn, session_id.as_deref())?;
            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);
            let manifest = build_board_backup_manifest(&conn, &board_short)?;
            let manifest_json = serde_json::to_vec_pretty(&manifest)
                .map_err(|error| AppError::Internal(anyhow::anyhow!("JSON: {error}")))?;
            tracing::info!(
                target: "admin",
                board = %manifest.board.short_name,
                version = manifest.version,
                threads = manifest.threads.len(),
                posts = manifest.posts.len(),
                polls = manifest.polls.len(),
                poll_options = manifest.poll_options.len(),
                poll_votes = manifest.poll_votes.len(),
                file_hashes = manifest.file_hashes.len(),
                manifest_bytes = manifest_json.len(),
                "Board backup manifest assembled"
            );

            let backup_dir = if download_after_create {
                super::prune_stale_temp_board_downloads();
                super::temp_board_download_dir()
            } else {
                super::board_backup_dir()
            };
            std::fs::create_dir_all(&backup_dir).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Create board backup dir: {error}"))
            })?;
            let ts = super::local_backup_timestamp_label();
            let filename = super::unique_backup_filename(
                &backup_dir,
                &format!("rustchan-board-{board_short}-{ts}.zip"),
            );
            let final_path = backup_dir.join(&filename);
            let tmp_path = backup_dir.join(format!("{filename}.tmp"));

            let uploads_base = std::path::Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = super::count_files_in_dir(&board_upload_path);
            tracing::info!(
                target: "admin",
                board = %board_short,
                uploads_dir = %board_upload_path.display(),
                upload_file_count = file_count,
                "Board backup starting zip build"
            );
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            log_backup_phase(crate::middleware::backup_phase::COMPRESS);
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            let build_result = write_board_backup_archive_from_dir(
                &tmp_path,
                &manifest_json,
                uploads_base,
                &board_upload_path,
                Some(&progress),
            );

            if let Err(error) = build_result {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error);
            }

            if let Err(error) = super::common::verify_board_backup_zip(&tmp_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error);
            }

            std::fs::rename(&tmp_path, &final_path).map_err(|error| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename board backup: {error}"))
            })?;
            if !download_after_create {
                super::invalidate_backup_list_cache(&backup_dir, super::BackupListKind::Board);
            }

            let size = std::fs::metadata(&final_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            tracing::info!(
                target: "admin",
                board = %board_short,
                filename = %filename,
                path = %final_path.display(),
                bytes = size,
                "Board backup created"
            );
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            log_backup_phase(crate::middleware::backup_phase::DONE);
            Ok(filename)
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    let wants_json = headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
        && headers
            .get("x-rustchan-download-after-create")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == "1");

    if wants_json {
        let download_token = new_session_id();
        super::write_temp_board_download_token(&filename, &download_token)?;
        let body = serde_json::json!({
            "filename": filename,
            "download_url": format!(
                "/admin/backup/download/temp-board/{filename}?cleanup=1&token={download_token}"
            ),
            "board": board_short_for_flash,
        });
        return Ok((
            [(header::CONTENT_TYPE, "application/json".to_string())],
            body.to_string(),
        )
            .into_response());
    }

    if form.download_after_create.as_deref() == Some("1") {
        let download_token = new_session_id();
        super::write_temp_board_download_token(&filename, &download_token)?;
        return Ok(Redirect::to(&format!(
            "/admin/backup/download/temp-board/{filename}?cleanup=1&token={download_token}"
        ))
        .into_response());
    }

    Ok(super::super::admin_panel_redirect_anchor_open(
        &format!("Board /{board_short_for_flash}/ backup saved on the server."),
        &format!("board-backup-{board_short_for_flash}"),
        "board-backup-restore",
    )
    .into_response())
}

pub(super) fn build_full_backup_manifest(
    conn: &rusqlite::Connection,
    db_bytes: u64,
    upload_file_count: u64,
    favicon_file_count: u64,
    banner_file_count: u64,
    tor_hidden_service_keys_included: bool,
    tor_hidden_service_key_file_count: u64,
) -> Result<super::common::FullBackupManifest> {
    let boards = collect_all_rows(
        conn,
        "SELECT short_name, name FROM boards ORDER BY short_name ASC",
        |row| {
            let short_name: String = row.get(0)?;
            let name: String = row.get(1)?;
            Ok(crate::models::BackupBoardSummary { short_name, name })
        },
    )?;
    Ok(super::common::FullBackupManifest {
        version: 3,
        generated_at: Utc::now().timestamp(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        db_bytes,
        upload_file_count,
        favicon_file_count,
        banner_file_count,
        tor_hidden_service_keys_included,
        tor_hidden_service_key_file_count,
        boards,
    })
}

pub(super) fn build_board_backup_manifest(
    conn: &rusqlite::Connection,
    board_short: &str,
) -> Result<board_backup_types::BoardBackupManifest> {
    use board_backup_types::{
        BannerRow, BoardBackupManifest, BoardRow, FileHashRow, PollOptionRow, PollRow, PollVoteRow,
        PostRow, ThreadRow,
    };

    let board: BoardRow = conn
        .query_row(
            "SELECT id, short_name, name, description, nsfw, max_threads, max_archived_threads, bump_limit,
                     allow_images, allow_video, allow_audio, allow_any_files, allow_tripcodes,
                     edit_window_secs, allow_editing, allow_self_delete, allow_archive, allow_video_embeds,
                     allow_captcha, show_poster_ids, collapse_greentext, post_cooldown_secs,
                     banner_mode, access_mode, access_password_hash, created_at
              FROM boards WHERE short_name = ?1",
            params![board_short],
            |row| {
                Ok(BoardRow {
                    id: row.get(0)?,
                    short_name: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    nsfw: row.get::<_, i64>(4)? != 0,
                    max_threads: row.get(5)?,
                    max_archived_threads: row.get(6)?,
                    bump_limit: row.get(7)?,
                    allow_images: row.get::<_, i64>(8)? != 0,
                    allow_video: row.get::<_, i64>(9)? != 0,
                    allow_audio: row.get::<_, i64>(10)? != 0,
                    allow_any_files: row.get::<_, i64>(11)? != 0,
                    allow_tripcodes: row.get::<_, i64>(12)? != 0,
                    edit_window_secs: row.get(13)?,
                    allow_editing: row.get::<_, i64>(14)? != 0,
                    allow_self_delete: row.get::<_, i64>(15)? != 0,
                    allow_archive: row.get::<_, i64>(16)? != 0,
                    allow_video_embeds: row.get::<_, i64>(17)? != 0,
                    allow_captcha: row.get::<_, i64>(18)? != 0,
                    show_poster_ids: row.get::<_, i64>(19)? != 0,
                    collapse_greentext: row.get::<_, i64>(20)? != 0,
                    post_cooldown_secs: row.get(21)?,
                    banner_mode: row.get(22)?,
                    access_mode: row.get(23)?,
                    access_password_hash: row.get(24)?,
                    created_at: row.get(25)?,
                })
            },
        )
        .map_err(|_| AppError::NotFound(format!("Board '{board_short}' not found")))?;

    let board_id = board.id;
    let threads = collect_rows(
        conn,
        board_id,
        "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, archived, reply_count
         FROM threads WHERE board_id = ?1 ORDER BY id ASC",
        |row| {
            Ok(ThreadRow {
                id: row.get(0)?,
                board_id: row.get(1)?,
                subject: row.get(2)?,
                created_at: row.get(3)?,
                bumped_at: row.get(4)?,
                locked: row.get::<_, i64>(5)? != 0,
                sticky: row.get::<_, i64>(6)? != 0,
                archived: row.get::<_, i64>(7)? != 0,
                reply_count: row.get(8)?,
            })
        },
    )?;
    let posts = collect_rows(
        conn,
        board_id,
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                media_type, created_at, deletion_token, is_op,
                media_processing_state, media_processing_error
         FROM posts WHERE board_id = ?1 ORDER BY id ASC",
        |row| {
            Ok(PostRow {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                board_id: row.get(2)?,
                name: row.get(3)?,
                tripcode: row.get(4)?,
                subject: row.get(5)?,
                body: row.get(6)?,
                body_html: row.get(7)?,
                ip_hash: row.get(8)?,
                file_path: row.get(9)?,
                file_name: row.get(10)?,
                file_size: row.get(11)?,
                thumb_path: row.get(12)?,
                mime_type: row.get(13)?,
                media_type: row.get(14)?,
                created_at: row.get(15)?,
                deletion_token: row.get(16)?,
                is_op: row.get::<_, i64>(17)? != 0,
                media_processing_state: row.get(18)?,
                media_processing_error: row.get(19)?,
            })
        },
    )?;
    let polls = collect_rows(
        conn,
        board_id,
        "SELECT p.id, p.thread_id, p.question, p.expires_at, p.created_at
         FROM polls p JOIN threads t ON t.id = p.thread_id
         WHERE t.board_id = ?1 ORDER BY p.id ASC",
        |row| {
            Ok(PollRow {
                id: row.get(0)?,
                thread_id: row.get(1)?,
                question: row.get(2)?,
                expires_at: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )?;
    let poll_options = collect_rows(
        conn,
        board_id,
        "SELECT po.id, po.poll_id, po.text, po.position
         FROM poll_options po
         JOIN polls p ON p.id = po.poll_id
         JOIN threads t ON t.id = p.thread_id
         WHERE t.board_id = ?1 ORDER BY po.id ASC",
        |row| {
            Ok(PollOptionRow {
                id: row.get(0)?,
                poll_id: row.get(1)?,
                text: row.get(2)?,
                position: row.get(3)?,
            })
        },
    )?;
    let poll_votes = collect_rows(
        conn,
        board_id,
        "SELECT pv.id, pv.poll_id, pv.option_id, pv.ip_hash
         FROM poll_votes pv
         JOIN polls p ON p.id = pv.poll_id
         JOIN threads t ON t.id = p.thread_id
         WHERE t.board_id = ?1 ORDER BY pv.id ASC",
        |row| {
            Ok(PollVoteRow {
                id: row.get(0)?,
                poll_id: row.get(1)?,
                option_id: row.get(2)?,
                ip_hash: row.get(3)?,
            })
        },
    )?;
    let file_hashes = collect_rows(
        conn,
        board_id,
        "SELECT DISTINCT fh.sha256, fh.file_path, fh.thumb_path, fh.mime_type, fh.created_at
         FROM file_hashes fh
         JOIN posts po ON po.file_path = fh.file_path
         WHERE po.board_id = ?1 ORDER BY fh.created_at ASC",
        |row| {
            Ok(FileHashRow {
                sha256: row.get(0)?,
                file_path: row.get(1)?,
                thumb_path: row.get(2)?,
                mime_type: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )?;
    let banners = collect_rows(
        conn,
        board_id,
        "SELECT storage_key, width, height, file_size, enabled, sort_order,
                target_type, target_value, show_on_index, show_on_catalog, created_at
         FROM banner_assets
         WHERE scope_type = 'board' AND board_id = ?1
         ORDER BY sort_order ASC, id ASC",
        |row| {
            Ok(BannerRow {
                storage_key: row.get(0)?,
                width: row.get(1)?,
                height: row.get(2)?,
                file_size: row.get(3)?,
                enabled: row.get::<_, i64>(4)? != 0,
                sort_order: row.get(5)?,
                target_type: row.get(6)?,
                target_value: row.get(7)?,
                show_on_index: row.get::<_, i64>(8)? != 0,
                show_on_catalog: row.get::<_, i64>(9)? != 0,
                created_at: row.get(10)?,
            })
        },
    )?;

    Ok(BoardBackupManifest {
        version: 2,
        board,
        threads,
        posts,
        polls,
        poll_options,
        poll_votes,
        file_hashes,
        banners,
    })
}

pub(super) fn write_board_backup_archive_from_dir(
    output_path: &Path,
    manifest_json: &[u8],
    uploads_base: &Path,
    board_upload_path: &Path,
    progress: Option<&crate::middleware::BackupProgress>,
) -> Result<()> {
    write_board_backup_archive(output_path, manifest_json, progress, |zip| {
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        if board_upload_path.exists() {
            if let Some(progress) = progress {
                super::add_dir_to_zip(zip, uploads_base, board_upload_path, opts, progress)?;
            } else {
                let noop_progress = crate::middleware::BackupProgress::new();
                super::add_dir_to_zip(zip, uploads_base, board_upload_path, opts, &noop_progress)?;
            }
        }
        Ok(())
    })
}

pub(super) fn write_board_backup_archive<F>(
    output_path: &Path,
    manifest_json: &[u8],
    progress: Option<&crate::middleware::BackupProgress>,
    mut write_uploads: F,
) -> Result<()>
where
    F: FnMut(&mut zip::ZipWriter<std::io::BufWriter<std::fs::File>>) -> Result<()>,
{
    let out_file = std::io::BufWriter::new(
        std::fs::File::create(output_path)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Create zip tmp: {error}")))?,
    );
    let mut zip = zip::ZipWriter::new(out_file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("board.json", opts)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip manifest: {error}")))?;
    zip.write_all(manifest_json)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Write manifest: {error}")))?;
    if let Some(progress) = progress {
        progress.files_done.fetch_add(1, Ordering::Relaxed);
        progress.bytes_done.fetch_add(
            u64::try_from(manifest_json.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        log_backup_progress(progress);
    }

    write_uploads(&mut zip)?;

    let writer = zip
        .finish()
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Finalise zip: {error}")))?;
    writer
        .into_inner()
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Flush zip writer: {error}")))?
        .sync_all()
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Sync zip file: {error}")))?;
    Ok(())
}

fn collect_rows<T, F>(
    conn: &rusqlite::Connection,
    board_id: i64,
    sql: &str,
    mapper: F,
) -> Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    let rows = statement
        .query_map(params![board_id], mapper)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    Ok(rows)
}

fn collect_all_rows<T, F>(conn: &rusqlite::Connection, sql: &str, mapper: F) -> Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let mut statement = conn
        .prepare(sql)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    let rows = statement
        .query_map([], mapper)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{
        build_full_backup_manifest, count_required_private_files, full_backup_upload_file_count,
        FullBackupCreateForm,
    };
    use crate::handlers::admin::backup::common::{
        resolve_tor_hidden_service_keys_availability, verify_full_backup_zip,
        TorHiddenServiceKeysAvailability, FULL_BACKUP_MANIFEST_NAME,
        FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX,
    };
    use axum::{
        body::{to_bytes, Body},
        extract::Form,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use std::io::Write as _;
    use tower::ServiceExt as _;

    async fn echo_full_backup_create_form(Form(form): Form<FullBackupCreateForm>) -> String {
        form.include_tor_hidden_service_keys.to_string()
    }

    #[tokio::test]
    async fn full_backup_create_form_accepts_checked_browser_checkbox_value() {
        let app = Router::new().route("/parse", post(echo_full_backup_create_form));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(
                        header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded;charset=UTF-8",
                    )
                    .body(Body::from("_csrf=test&include_tor_hidden_service_keys=1"))
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

    #[tokio::test]
    async fn full_backup_create_form_defaults_missing_checkbox_to_false() {
        let app = Router::new().route("/parse", post(echo_full_backup_create_form));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("_csrf=test"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"false");
    }

    fn write_test_full_backup_zip(zip_path: &std::path::Path, include_tor_keys: bool) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let uploads_dir = temp_dir.path().join("uploads");
        let tor_keys_dir = temp_dir.path().join("tor-keys");
        std::fs::create_dir_all(uploads_dir.join("tech")).expect("create uploads");
        std::fs::write(uploads_dir.join("tech/post.txt"), "post").expect("write upload");
        std::fs::create_dir_all(&tor_keys_dir).expect("create tor key dir");
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "secret")
            .expect("write secret key");
        std::fs::write(tor_keys_dir.join("hs_ed25519_public_key"), "public")
            .expect("write public key");

        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db conn");
        crate::db::create_board(&conn, "tech", "Technology", "", false).expect("create board");

        let db_path = temp_dir.path().join("snapshot.db");
        let db_path_str = db_path.to_str().expect("db path").replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{db_path_str}'"))
            .expect("vacuum snapshot");

        let tor_key_file_count = if include_tor_keys { 2 } else { 0 };
        let total_archive_file_count = 1_u64.saturating_add(tor_key_file_count);
        let upload_file_count =
            full_backup_upload_file_count(total_archive_file_count, 0, 0, tor_key_file_count);
        let manifest = build_full_backup_manifest(
            &conn,
            std::fs::metadata(&db_path).expect("db metadata").len(),
            upload_file_count,
            0,
            0,
            include_tor_keys,
            tor_key_file_count,
        )
        .expect("build manifest");
        let manifest_json = serde_json::to_vec(&manifest).expect("manifest json");
        let db_bytes = std::fs::read(&db_path).expect("read db");

        let file = std::fs::File::create(zip_path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(FULL_BACKUP_MANIFEST_NAME, options)
            .expect("start manifest");
        zip.write_all(&manifest_json).expect("write manifest");
        zip.start_file("chan.db", options).expect("start db");
        zip.write_all(&db_bytes).expect("write db");
        super::super::add_dir_to_zip_with_prefix(
            &mut zip,
            &uploads_dir,
            &uploads_dir,
            "uploads",
            options,
            &crate::middleware::BackupProgress::new(),
        )
        .expect("zip uploads");
        if include_tor_keys {
            super::super::add_dir_to_zip_with_prefix(
                &mut zip,
                &tor_keys_dir,
                &tor_keys_dir,
                super::super::common::FULL_BACKUP_TOR_KEYS_PREFIX,
                options,
                &crate::middleware::BackupProgress::new(),
            )
            .expect("zip tor keys");
        }
        zip.finish().expect("finish zip");
    }

    #[test]
    fn full_backup_manifest_defaults_to_no_tor_keys_when_not_requested() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db conn");
        crate::db::create_board(&conn, "tech", "Technology", "", false).expect("create board");

        let manifest =
            build_full_backup_manifest(&conn, 1024, 5, 1, 2, false, 0).expect("build manifest");

        assert!(!manifest.tor_hidden_service_keys_included);
        assert_eq!(manifest.tor_hidden_service_key_file_count, 0);
    }

    #[test]
    fn full_backup_archive_can_record_and_package_tor_key_material() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("full-with-tor.zip");
        write_test_full_backup_zip(&zip_path, true);

        let manifest = verify_full_backup_zip(&zip_path).expect("verify zip");
        assert!(manifest.tor_hidden_service_keys_included);
        assert_eq!(manifest.upload_file_count, 1);
        assert_eq!(manifest.tor_hidden_service_key_file_count, 2);

        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        assert!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_secret_key"
            ))
            .is_ok());
        assert!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_public_key"
            ))
            .is_ok());
    }

    #[test]
    fn full_backup_archive_omits_tor_key_material_when_not_requested() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("full-without-tor.zip");
        write_test_full_backup_zip(&zip_path, false);

        let manifest = verify_full_backup_zip(&zip_path).expect("verify zip");
        assert!(!manifest.tor_hidden_service_keys_included);
        assert_eq!(manifest.upload_file_count, 1);
        assert_eq!(manifest.tor_hidden_service_key_file_count, 0);

        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        assert!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_secret_key"
            ))
            .is_err());
    }

    #[test]
    fn requested_tor_key_backup_fails_clearly_when_identity_dir_is_missing() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let missing = temp_dir.path().join("missing-keys");
        let error = count_required_private_files(
            &missing,
            "Tor hidden service keys were requested, but the configured identity directory could not be read.",
        )
        .expect_err("missing Tor key dir should fail");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("Tor hidden service keys were requested"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn requested_tor_key_backup_skips_cleanly_when_not_requested() {
        let result = resolve_tor_hidden_service_keys_availability(
            false,
            None,
            "Tor hidden service key backups are not available with the current configuration.",
        )
        .expect("resolve skipped tor keys");

        assert_eq!(result, TorHiddenServiceKeysAvailability::Skipped);
    }

    #[test]
    fn requested_tor_key_backup_is_rejected_when_tor_is_disabled_or_unconfigured() {
        let error = resolve_tor_hidden_service_keys_availability(
            true,
            None,
            "Tor hidden service key backups are not available with the current configuration.",
        )
        .expect_err("requested tor keys should be rejected without configuration");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("Tor hidden service key backups are not available"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn requested_tor_key_backup_is_rejected_when_configured_path_is_missing() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let missing = temp_dir.path().join("missing-keys");
        let error = resolve_tor_hidden_service_keys_availability(
            true,
            Some(missing.clone()),
            "Tor hidden service key backups are not available with the current configuration.",
        )
        .expect_err("missing tor keys dir should fail");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("could not be read"));
                assert!(message.contains(&missing.display().to_string()));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }
}
