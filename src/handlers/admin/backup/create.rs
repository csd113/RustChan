use super::*;

pub async fn create_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<super::super::CsrfOnly>,
) -> Result<Response> {
    let session_id = jar
        .get(super::super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::super::require_admin_session_sid(&conn, session_id.as_deref())?;

            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);

            let temp_dir = std::env::temp_dir();
            let tmp_id = uuid::Uuid::new_v4().simple().to_string();
            let temp_db = temp_dir.join(format!("chan_backup_{tmp_id}.db"));
            let temp_db_str = temp_db
                .to_str()
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Temp path non-UTF-8")))?
                .replace('\'', "''");

            conn.execute_batch(&format!("VACUUM INTO '{temp_db_str}'"))
                .map_err(|error| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {error}")))?;
            drop(conn);

            progress.reset(crate::middleware::backup_phase::COUNT_FILES);
            let uploads_base = std::path::Path::new(&upload_dir);
            let favicon_file_count = super::count_files_in_dir(&global_favicon_dir);
            let file_count =
                super::count_files_in_dir(uploads_base).saturating_add(favicon_file_count);
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            let backup_dir = super::full_backup_dir();
            std::fs::create_dir_all(&backup_dir).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Create full-backups dir: {error}"))
            })?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let filename = format!("rustchan-backup-{ts}.zip");
            let final_path = backup_dir.join(&filename);
            let tmp_path = backup_dir.join(format!("{filename}.tmp"));

            {
                let out_file = std::io::BufWriter::new(
                    std::fs::File::create(&tmp_path).map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Create zip tmp: {error}"))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                progress.reset(crate::middleware::backup_phase::COMPRESS);
                progress
                    .files_total
                    .store(file_count.saturating_add(1), Ordering::Relaxed);

                zip.start_file("chan.db", opts)
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip DB: {error}")))?;
                let mut db_src = std::fs::File::open(&temp_db).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Open DB snapshot: {error}"))
                })?;
                let copied = std::io::copy(&mut db_src, &mut zip).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Stream DB to zip: {error}"))
                })?;
                drop(db_src);
                let _ = std::fs::remove_file(&temp_db);
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(copied, Ordering::Relaxed);

                if uploads_base.exists() {
                    super::add_dir_to_zip(&mut zip, uploads_base, uploads_base, opts, &progress)?;
                }
                if global_favicon_dir.exists() {
                    super::add_dir_to_zip_with_prefix(
                        &mut zip,
                        &global_favicon_dir,
                        &global_favicon_dir,
                        "favicon",
                        opts,
                        &progress,
                    )?;
                }

                let writer = zip.finish().map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Finalise zip: {error}"))
                })?;
                writer
                    .into_inner()
                    .map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Flush zip writer: {error}"))
                    })?
                    .sync_all()
                    .map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Sync zip file: {error}"))
                    })?;
            }

            std::fs::rename(&tmp_path, &final_path).map_err(|error| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename backup: {error}"))
            })?;

            let size = std::fs::metadata(&final_path).map(|metadata| metadata.len()).unwrap_or(0);
            tracing::info!(target: "admin", filename = %filename, bytes = size, "Full backup created");
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
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
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub async fn create_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardBackupCreateForm>,
) -> Result<Response> {
    use super::board_backup_types::{
        BoardBackupManifest, BoardRow, FileHashRow, PollOptionRow, PollRow, PollVoteRow, PostRow,
        ThreadRow,
    };

    let session_id = jar
        .get(super::super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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

    let upload_dir = CONFIG.upload_dir.clone();
    let progress = state.backup_progress.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::super::require_admin_session_sid(&conn, session_id.as_deref())?;
            progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
            let board: BoardRow = conn
                .query_row(
                    "SELECT id, short_name, name, description, nsfw, max_threads, max_archived_threads, bump_limit,
                             allow_images, allow_video, allow_audio, allow_any_files, allow_tripcodes,
                             edit_window_secs, allow_editing, allow_archive, allow_video_embeds,
                             allow_captcha, show_poster_ids, post_cooldown_secs, created_at
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
                            allow_archive: row.get::<_, i64>(15)? != 0,
                            allow_video_embeds: row.get::<_, i64>(16)? != 0,
                            allow_captcha: row.get::<_, i64>(17)? != 0,
                            show_poster_ids: row.get::<_, i64>(18)? != 0,
                            post_cooldown_secs: row.get(19)?,
                            created_at: row.get(20)?,
                        })
                    },
                )
                .map_err(|_| AppError::NotFound(format!("Board '{board_short}' not found")))?;

            let board_id = board.id;
            let threads = collect_rows(
                &conn,
                board_id,
                "SELECT id, board_id, subject, created_at, bumped_at, locked, sticky, reply_count
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
                        reply_count: row.get(7)?,
                    })
                },
            )?;
            let posts = collect_rows(
                &conn,
                board_id,
                "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                        ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                        media_type, created_at, deletion_token, is_op
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
                    })
                },
            )?;
            let polls = collect_rows(
                &conn,
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
                &conn,
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
                &conn,
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
                &conn,
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

            let backup_dir = super::board_backup_dir();
            std::fs::create_dir_all(&backup_dir).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Create board-backups dir: {error}"))
            })?;
            let ts = Utc::now().format("%Y%m%d_%H%M%S");
            let filename = format!("rustchan-board-{board_short}-{ts}.zip");
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
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            {
                let out_file = std::io::BufWriter::new(
                    std::fs::File::create(&tmp_path).map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Create zip tmp: {error}"))
                    })?,
                );
                let mut zip = zip::ZipWriter::new(out_file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);

                zip.start_file("board.json", opts).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Zip manifest: {error}"))
                })?;
                zip.write_all(&manifest_json).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write manifest: {error}"))
                })?;
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(
                    u64::try_from(manifest_json.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );

                if board_upload_path.exists() {
                    super::add_dir_to_zip(
                        &mut zip,
                        uploads_base,
                        &board_upload_path,
                        opts,
                        &progress,
                    )?;
                }

                let writer = zip.finish().map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Finalise zip: {error}"))
                })?;
                writer
                    .into_inner()
                    .map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Flush zip writer: {error}"))
                    })?
                    .sync_all()
                    .map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Sync zip file: {error}"))
                    })?;
            }

            std::fs::rename(&tmp_path, &final_path).map_err(|error| {
                let _ = std::fs::remove_file(&tmp_path);
                AppError::Internal(anyhow::anyhow!("Rename board backup: {error}"))
            })?;

            let size = std::fs::metadata(&final_path).map(|metadata| metadata.len()).unwrap_or(0);
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
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(super::super::admin_panel_redirect(&format!(
        "Board /{board_short_for_flash}/ backup saved on the server."
    ))
    .into_response())
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
