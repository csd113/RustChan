#![allow(clippy::wildcard_imports)]

use super::*;

fn validate_board_backup_access_settings(
    manifest: &mut board_backup_types::BoardBackupManifest,
) -> Result<()> {
    let access_mode =
        BoardAccessMode::from_db_str(&manifest.board.access_mode).ok_or_else(|| {
            AppError::BadRequest("Board backup contains an invalid access mode.".into())
        })?;
    manifest.board.access_mode = access_mode.as_str().to_string();

    if access_mode.is_password_protected() && manifest.board.access_password_hash.is_empty() {
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

struct BoardRestoreWorkspace {
    staged_upload_root: PathBuf,
    pending_restore_id: String,
    pending_restore_payload: crate::pending_fs::BoardRestoreSwapPayload,
    pending_restore_op: crate::pending_fs::PendingFsOpInsert,
}

impl BoardRestoreWorkspace {
    fn prepare(upload_dir: &str, board_short: &str) -> Result<Self> {
        let upload_root = PathBuf::from(upload_dir);
        let staged_upload_root = create_staging_dir(&upload_root, "board-restore-stage")?;
        let staged_board_dir = staged_upload_root.join(board_short);
        std::fs::create_dir_all(&staged_board_dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Create staged board dir: {error}"))
        })?;
        let live_board_dir = upload_root.join(board_short);
        let previous_board_dir = upload_root.join(format!(
            ".{board_short}.restore-old.{}",
            uuid::Uuid::new_v4().simple()
        ));
        let pending_restore_id = uuid::Uuid::new_v4().to_string();
        let pending_restore_payload = crate::pending_fs::BoardRestoreSwapPayload {
            staged: staged_board_dir.display().to_string(),
            live: live_board_dir.display().to_string(),
            previous: previous_board_dir.display().to_string(),
        };
        let pending_restore_op = crate::pending_fs::PendingFsOpInsert {
            id: pending_restore_id.clone(),
            kind: crate::pending_fs::BOARD_RESTORE_SWAP_KIND,
            payload_json: serde_json::to_string(&pending_restore_payload).map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Serialize board restore pending_fs_op payload: {error}"
                ))
            })?,
        };

        Ok(Self {
            staged_upload_root,
            pending_restore_id,
            pending_restore_payload,
            pending_restore_op,
        })
    }
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
pub(super) fn execute_board_restore<F>(
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
    let workspace = BoardRestoreWorkspace::prepare(upload_dir, &board_short)?;
    extract_uploads(&workspace.staged_upload_root)?;

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
                 banner_mode=?20, access_mode=?21, access_password_hash=?22
                 WHERE id=?23",
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
                    manifest.board.banner_mode,
                    manifest.board.access_mode,
                    manifest.board.access_password_hash,
                    existing_id,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Update board: {error}")))?;
            conn.execute(
                "DELETE FROM banner_assets WHERE scope_type = 'board' AND board_id = ?1",
                params![existing_id],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Clear board banners: {error}")))?;
            existing_id
        } else {
            insert_returning_id(
                conn,
                "INSERT INTO boards (short_name, name, description, nsfw, max_threads,
                 max_archived_threads, bump_limit, allow_images, allow_video, allow_audio, allow_any_files,
                 allow_tripcodes, edit_window_secs, allow_editing, allow_archive,
                 allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext,
                 post_cooldown_secs, banner_mode, access_mode, access_password_hash, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)
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
                    manifest.board.banner_mode,
                    manifest.board.access_mode,
                    manifest.board.access_password_hash,
                    manifest.board.created_at,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Insert board: {error}")))?
        };
        for banner in &manifest.banners {
            banner::validate_banner_storage_key(&banner.storage_key).map_err(|error| {
                AppError::BadRequest(format!(
                    "Invalid banner storage key in backup manifest: {error}"
                ))
            })?;
            conn.execute(
                "INSERT INTO banner_assets
                 (scope_type, board_id, storage_key, width, height, file_size, enabled, sort_order,
                  target_type, target_value, show_on_index, show_on_catalog, created_at)
                 VALUES ('board', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    live_board_id,
                    banner.storage_key,
                    banner.width,
                    banner.height,
                    banner.file_size,
                    i64::from(banner.enabled),
                    banner.sort_order,
                    banner.target_type,
                    banner.target_value,
                    i64::from(banner.show_on_index),
                    i64::from(banner.show_on_catalog),
                    banner.created_at,
                ],
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Insert board banner: {error}")))?;
        }

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

        db::insert_pending_fs_op(conn, &workspace.pending_restore_op)?;
        Ok(())
    })();

    match restore_result {
        Ok(()) => {
            conn.execute("COMMIT", [])
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Commit tx: {error}")))?;
            if let Err(error) = crate::pending_fs::finalize_board_restore_payload(
                &workspace.pending_restore_payload,
            ) {
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
            db::delete_pending_fs_op(conn, &workspace.pending_restore_id)?;
        }
        Err(error) => {
            let _ = conn.execute("ROLLBACK", []);
            let _ = remove_path_if_exists(&workspace.staged_upload_root);
            let _ = std::fs::remove_file(&db_snapshot);
            return Err(error);
        }
    }

    let _ = std::fs::remove_file(&db_snapshot);
    let _ = remove_path_if_exists(&workspace.staged_upload_root);

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

fn format_magic_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
pub struct ExtractBoardFromFullBackupForm {
    filename: String,
    board_short: String,
    action: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
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

    let outcome_result = tokio::task::spawn_blocking({
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
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)));

    let outcome = match outcome_result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            return Ok(Redirect::to(&restore_error_redirect_target(
                RestoreKind::Full,
                &error.to_string(),
            ))
            .into_response());
        }
        Err(join_error) => {
            return Ok(Redirect::to(&restore_error_redirect_target(
                RestoreKind::Full,
                &join_error.to_string(),
            ))
            .into_response());
        }
    };

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
            Ok(super::admin_panel_redirect_anchor_open(
                &format!("Board /{board_short}/ restored."),
                &format!("board-backup-{board_short}"),
                BOARD_BACKUP_RESTORE_SECTION,
            )
            .into_response())
        }
    }
}

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
    let upload_dir = CONFIG.upload_dir.clone();

    let board_short_result: Result<Result<String>> = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
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
        Ok(Ok(board_short)) => Ok(super::admin_panel_redirect_anchor_open(
            &format!("Board /{board_short}/ restored."),
            &format!("board-backup-{board_short}"),
            BOARD_BACKUP_RESTORE_SECTION,
        )
        .into_response()),
        Ok(Err(app_err)) => Ok(Redirect::to(&restore_error_redirect_target(
            RestoreKind::Board,
            &app_err.to_string(),
        ))
        .into_response()),
        Err(join_err) => Ok(Redirect::to(&restore_error_redirect_target(
            RestoreKind::Board,
            &join_err.to_string(),
        ))
        .into_response()),
    }
}

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
    let result: Result<String> = async {
        let session_id = restore_auth_preflight(&state, &headers, &jar).await?;
        let upload_dir = CONFIG.upload_dir.clone();

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
                    let f2 = zip_tmp
                        .reopen()
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen zip (2): {e}")))?;
                    let archive2 = zip::ZipArchive::new(std::io::BufReader::new(f2))
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("Reopen archive: {e}")))?;
                    (manifest, Some(archive2))
                } else {
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
            let redirect_url =
                restore_success_redirect_target(RestoreKind::Board, Some(&board_short));
            if xhr_request {
                return crate::handlers::board::xhr_redirect_response(&redirect_url)
                    .unwrap_or_else(|error| error.into_response());
            }
            redirect_page_response(&redirect_url, &format!("Board /{board_short}/ restored."))
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
