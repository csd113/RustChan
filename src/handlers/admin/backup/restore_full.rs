// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use crate::handlers::admin::backup::common::{
    copy_limited_with_total_budget, RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
};

pub(super) fn restore_db_from_snapshot(
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

pub(super) fn refresh_live_site_state_from_db(conn: &rusqlite::Connection) -> Result<()> {
    crate::templates::set_live_site_name(&db::get_site_name(conn));
    crate::templates::set_live_site_subtitle(&db::get_site_subtitle(conn));
    crate::templates::set_live_boards(db::get_all_boards(conn)?);
    db::sync_live_theme_state(conn)?;
    Ok(())
}

fn validate_full_restore_db_trust_boundary(conn: &rusqlite::Connection) -> Result<()> {
    let mut valid_board_shorts = std::collections::HashSet::new();
    let mut board_ids_to_shorts = std::collections::HashMap::new();
    let mut stmt = conn
        .prepare("SELECT id, short_name FROM boards")
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in rows {
        let (board_id, short_name) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid board row: {error}"
            ))
        })?;
        validate_board_short_name(&short_name)?;
        valid_board_shorts.insert(short_name.clone());
        board_ids_to_shorts.insert(board_id, short_name);
    }

    let mut post_stmt = conn
        .prepare(
            "SELECT id, board_id, file_path, thumb_path, audio_file_path
             FROM posts
             WHERE file_path IS NOT NULL OR thumb_path IS NOT NULL OR audio_file_path IS NOT NULL",
        )
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let post_rows = post_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in post_rows {
        let (post_id, board_id, file_path, thumb_path, audio_file_path) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid post media row: {error}"
            ))
        })?;
        let expected_board_short = board_ids_to_shorts.get(&board_id).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Restored post {post_id} points to unknown board_id {board_id}."
            ))
        })?;
        for (label, path) in [
            ("file_path", file_path.as_deref()),
            ("thumb_path", thumb_path.as_deref()),
            ("audio_file_path", audio_file_path.as_deref()),
        ] {
            let Some(path) = path else {
                continue;
            };
            let board_short = super::common::validate_restored_media_path(
                path,
                &format!("Restored post {post_id} {label}"),
            )?;
            if !valid_board_shorts.contains(&board_short) {
                return Err(AppError::BadRequest(format!(
                    "Restored post {post_id} {label} points to unknown board /{board_short}/."
                )));
            }
            if &board_short != expected_board_short {
                return Err(AppError::BadRequest(format!(
                    "Restored post {post_id} {label} escapes its board /{expected_board_short}/."
                )));
            }
        }
    }

    let mut file_hash_stmt = conn
        .prepare("SELECT sha256, file_path, thumb_path FROM file_hashes")
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let file_hash_rows = file_hash_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in file_hash_rows {
        let (sha256, file_path, thumb_path) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid file_hash row: {error}"
            ))
        })?;
        let file_board_short = super::common::validate_restored_media_path(
            &file_path,
            &format!("Restored file_hash {sha256} file_path"),
        )?;
        if !valid_board_shorts.contains(&file_board_short) {
            return Err(AppError::BadRequest(format!(
                "Restored file_hash {sha256} file_path points to unknown board /{file_board_short}/."
            )));
        }
        if !thumb_path.is_empty() {
            let thumb_board_short = super::common::validate_restored_media_path(
                &thumb_path,
                &format!("Restored file_hash {sha256} thumb_path"),
            )?;
            if !valid_board_shorts.contains(&thumb_board_short) {
                return Err(AppError::BadRequest(format!(
                    "Restored file_hash {sha256} thumb_path points to unknown board /{thumb_board_short}/."
                )));
            }
            if thumb_board_short != file_board_short {
                return Err(AppError::BadRequest(format!(
                    "Restored file_hash {sha256} mixes boards between file_path and thumb_path."
                )));
            }
        }
    }

    Ok(())
}

fn scrub_full_restore_runtime_state(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute("DELETE FROM admin_sessions", [])
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Clear restored sessions: {error}")))?;
    Ok(())
}

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn execute_full_restore<R: std::io::Read + std::io::Seek>(
    live_conn: &mut rusqlite::Connection,
    admin_id: i64,
    upload_dir: &str,
    live_tor_hidden_service_keys_dir: Option<&Path>,
    restore_tor_hidden_service_keys: bool,
    archive: &mut zip::ZipArchive<R>,
    restore_label: &str,
    completion_log: &str,
    suspicious_entry_log: &str,
    session_warning_log: &str,
) -> Result<String> {
    validate_full_restore_archive_layout(archive)?;
    let manifest = verify_full_backup_archive(archive)?;
    if restore_tor_hidden_service_keys && !manifest.tor_hidden_service_keys_included {
        return Err(AppError::BadRequest(
            "This backup does not include Tor hidden service keys.".into(),
        ));
    }
    let live_tor_hidden_service_keys_dir =
        match super::common::resolve_tor_hidden_service_keys_restore_target(
            restore_tor_hidden_service_keys,
            live_tor_hidden_service_keys_dir.map(Path::to_path_buf),
            "Tor hidden service key restore is not available with the current configuration.",
        )? {
            super::common::TorHiddenServiceKeysAvailability::Skipped => None,
            super::common::TorHiddenServiceKeysAvailability::Available(dir) => Some(dir),
        };

    let temp_dir = std::env::temp_dir();
    let tmp_id = uuid::Uuid::new_v4().simple().to_string();
    let temp_db = temp_dir.join(format!("chan_restore_{tmp_id}.db"));
    let upload_root = PathBuf::from(upload_dir);
    let staged_upload_root = create_staging_dir(&upload_root, "restore-stage")?;
    let live_global_favicon_dir = crate::favicon::global_backup_source_dir();
    let staged_global_favicon_dir = create_staging_dir(&live_global_favicon_dir, "restore-stage")?;
    let live_global_banner_dir = crate::banner::backup_source_dir();
    let staged_global_banner_dir = create_staging_dir(&live_global_banner_dir, "restore-stage")?;
    let staged_tor_hidden_service_keys_dir = live_tor_hidden_service_keys_dir
        .as_deref()
        .map(|live_path| create_staging_dir(live_path, "restore-stage"))
        .transpose()?;
    let mut favicon_extracted = false;
    let mut banner_extracted = false;
    let mut tor_hidden_service_key_files_extracted = 0u64;
    let mut extracted_bytes = 0u64;
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
    let previous_global_favicon_dir = live_global_favicon_dir.parent().map_or_else(
        || PathBuf::from(format!("{}.restore-old", live_global_favicon_dir.display())),
        |parent| {
            parent.join(format!(
                ".{}.restore-old.{}",
                live_global_favicon_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("favicon"),
                uuid::Uuid::new_v4().simple()
            ))
        },
    );
    let previous_global_banner_dir = live_global_banner_dir.parent().map_or_else(
        || PathBuf::from(format!("{}.restore-old", live_global_banner_dir.display())),
        |parent| {
            parent.join(format!(
                ".{}.restore-old.{}",
                live_global_banner_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("banner"),
                uuid::Uuid::new_v4().simple()
            ))
        },
    );
    let previous_tor_hidden_service_keys_dir =
        live_tor_hidden_service_keys_dir.as_ref().map(|live_path| {
            live_path.parent().map_or_else(
                || PathBuf::from(format!("{}.restore-old", live_path.display())),
                |parent| {
                    parent.join(format!(
                        ".{}.restore-old.{}",
                        live_path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("tor-keys"),
                        uuid::Uuid::new_v4().simple()
                    ))
                },
            )
        });
    let db_snapshot = temp_dir.join(format!("chan_restore_live_before_{tmp_id}.db"));
    let mut db_extracted = false;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip[{index}]: {error}")))?;
        let name = entry.name().to_string();
        validate_restore_safe_entry_name(&name)?;

        if name == "chan.db" {
            let mut out = std::fs::File::create(&temp_db)
                .map_err(|error| AppError::Internal(anyhow::anyhow!("Create temp DB: {error}")))?;
            copy_limited_with_total_budget(
                &mut entry,
                &mut out,
                ZIP_ENTRY_MAX_BYTES,
                &mut extracted_bytes,
                RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
                "Full restore archive",
            )
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
        } else if let Some(rel_path) = restore_safe_relative_path_under_prefix(&name, "uploads/")? {
            let target = staged_upload_root.join(&rel_path);
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
                copy_limited_with_total_budget(
                    &mut entry,
                    &mut out,
                    ZIP_ENTRY_MAX_BYTES,
                    &mut extracted_bytes,
                    RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
                    "Full restore archive",
                )
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
                })?;
            }
        } else if let Some(rel_path) = restore_safe_relative_path_under_prefix(&name, "favicon/")? {
            favicon_extracted = true;
            let target = staged_global_favicon_dir.join(&rel_path);
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
                copy_limited_with_total_budget(
                    &mut entry,
                    &mut out,
                    ZIP_ENTRY_MAX_BYTES,
                    &mut extracted_bytes,
                    RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
                    "Full restore archive",
                )
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
                })?;
            }
        } else if let Some(rel) = name.strip_prefix("banner/") {
            if rel.is_empty() {
                continue;
            }
            let rel_name = match banner::validate_banner_restore_entry_name(rel) {
                Ok(value) => value,
                Err(error) => {
                    warn!("{suspicious_entry_log}: skipping suspicious entry '{name}': {error}");
                    continue;
                }
            };
            if entry.is_dir() {
                warn!("{suspicious_entry_log}: skipping banner directory entry '{name}'");
                continue;
            }
            banner_extracted = true;
            let target = staged_global_banner_dir.join(&rel_name);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("mkdir parent: {error}"))
                })?;
            }
            let mut out = std::fs::File::create(&target).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Create {}: {error}", target.display()))
            })?;
            copy_limited_with_total_budget(
                &mut entry,
                &mut out,
                BANNER_RESTORE_ENTRY_MAX_BYTES,
                &mut extracted_bytes,
                RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
                "Full restore archive",
            )
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
            })?;
        } else if let (Some(rel_path), Some(staged_tor_hidden_service_keys_dir)) = (
            restore_safe_relative_path_under_prefix(
                &name,
                super::common::FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX,
            )?,
            staged_tor_hidden_service_keys_dir.as_ref(),
        ) {
            let target = staged_tor_hidden_service_keys_dir.join(&rel_path);
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
                copy_limited_with_total_budget(
                    &mut entry,
                    &mut out,
                    ZIP_ENTRY_MAX_BYTES,
                    &mut extracted_bytes,
                    RESTORE_TOTAL_EXTRACTED_MAX_BYTES,
                    "Full restore archive",
                )
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display()))
                })?;
                tor_hidden_service_key_files_extracted =
                    tor_hidden_service_key_files_extracted.saturating_add(1);
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

    if banner_extracted {
        canonicalize_restored_banner_dir(&staged_global_banner_dir)?;
    }
    if restore_tor_hidden_service_keys {
        if tor_hidden_service_key_files_extracted != manifest.tor_hidden_service_key_file_count
            || tor_hidden_service_key_files_extracted == 0
        {
            let _ = remove_path_if_exists(&staged_upload_root);
            let _ = remove_path_if_exists(&staged_global_favicon_dir);
            let _ = remove_path_if_exists(&staged_global_banner_dir);
            if let Some(staged_tor_hidden_service_keys_dir) =
                staged_tor_hidden_service_keys_dir.as_ref()
            {
                let _ = remove_path_if_exists(staged_tor_hidden_service_keys_dir);
            }
            let _ = std::fs::remove_file(&temp_db);
            return Err(AppError::BadRequest(
                "This backup does not contain a complete Tor hidden service identity.".into(),
            ));
        }
        if let Some(staged_tor_hidden_service_keys_dir) =
            staged_tor_hidden_service_keys_dir.as_ref()
        {
            restrict_private_key_material_permissions(staged_tor_hidden_service_keys_dir)?;
        }
    }

    let pending_restore_id = uuid::Uuid::new_v4().to_string();
    let mut additional_swaps = Vec::new();
    if favicon_extracted {
        additional_swaps.push(crate::pending_fs::RestorePathSwapPayload {
            staged: staged_global_favicon_dir.display().to_string(),
            live: live_global_favicon_dir.display().to_string(),
            previous: previous_global_favicon_dir.display().to_string(),
            restrict_private_permissions: false,
        });
    }
    if banner_extracted {
        additional_swaps.push(crate::pending_fs::RestorePathSwapPayload {
            staged: staged_global_banner_dir.display().to_string(),
            live: live_global_banner_dir.display().to_string(),
            previous: previous_global_banner_dir.display().to_string(),
            restrict_private_permissions: false,
        });
    }
    additional_swaps.extend(
        live_tor_hidden_service_keys_dir
            .as_ref()
            .zip(staged_tor_hidden_service_keys_dir.as_ref())
            .zip(previous_tor_hidden_service_keys_dir.as_ref())
            .map(
                |(
                    (live_tor_hidden_service_keys_dir, staged_tor_hidden_service_keys_dir),
                    previous_tor_hidden_service_keys_dir,
                )| {
                    crate::pending_fs::RestorePathSwapPayload {
                        staged: staged_tor_hidden_service_keys_dir.display().to_string(),
                        live: live_tor_hidden_service_keys_dir.display().to_string(),
                        previous: previous_tor_hidden_service_keys_dir.display().to_string(),
                        restrict_private_permissions: true,
                    }
                },
            ),
    );

    let pending_restore_payload = crate::pending_fs::FullRestoreSwapPayload {
        staged: staged_upload_root.display().to_string(),
        live: upload_root.display().to_string(),
        previous: previous_upload_root.display().to_string(),
        additional_swaps,
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
        validate_full_restore_db_trust_boundary(&src)?;
        scrub_full_restore_runtime_state(&src)?;
        db::rebuild_pending_fs_ops_for_restore(&src)?;
        db::insert_pending_fs_op(&src, &pending_restore_op)?;
        db::verify_pending_fs_op_present(&src, &pending_restore_id)?;
        let backup = Backup::new(&src, live_conn)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Backup init: {error}")))?;
        backup
            .run_to_completion(100, std::time::Duration::from_millis(0), None)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Backup copy: {error}")))?;
        drop(backup);
        db::verify_pending_fs_op_present(live_conn, &pending_restore_id)?;
        Ok(())
    })();

    if let Err(error) = backup_result {
        let restore_db_result = restore_db_from_snapshot(live_conn, &db_snapshot, restore_label);
        let _ = std::fs::remove_file(&temp_db);
        let _ = std::fs::remove_file(&db_snapshot);
        let _ = remove_path_if_exists(&staged_upload_root);
        let _ = remove_path_if_exists(&staged_global_favicon_dir);
        let _ = remove_path_if_exists(&staged_global_banner_dir);
        if let Some(staged_tor_hidden_service_keys_dir) =
            staged_tor_hidden_service_keys_dir.as_ref()
        {
            let _ = remove_path_if_exists(staged_tor_hidden_service_keys_dir);
        }
        if let Err(restore_err) = restore_db_result {
            return Err(AppError::Internal(anyhow::anyhow!(
                "{restore_label} failed and rollback failed: {error}; rollback error: {restore_err}"
            )));
        }
        return Err(error);
    }

    if let Err(error) = crate::pending_fs::finalize_full_restore_payload(
        &pending_restore_payload,
        &upload_root,
        live_tor_hidden_service_keys_dir.as_deref(),
    ) {
        let _ = std::fs::remove_file(&temp_db);
        let _ = std::fs::remove_file(&db_snapshot);
        return Err(AppError::Internal(anyhow::anyhow!(
            "{restore_label} filesystem swap failed and remains pending for startup reconciliation: {error}"
        )));
    }
    db::delete_pending_fs_op(live_conn, &pending_restore_id)?;

    if !favicon_extracted {
        let _ = remove_path_if_exists(&staged_global_favicon_dir);
    }
    if !banner_extracted {
        let _ = remove_path_if_exists(&staged_global_banner_dir);
    }
    let _ = std::fs::remove_file(&temp_db);
    let _ = std::fs::remove_file(&db_snapshot);

    let fresh_sid = new_session_id();
    let expires_at = Utc::now().timestamp() + CONFIG.session_duration;
    match db::create_session(live_conn, &fresh_sid, admin_id, expires_at) {
        Ok(()) => {
            tracing::info!(target: "admin", admin_id = admin_id, "{completion_log}");
            if let Err(error) = refresh_live_site_state_from_db(live_conn) {
                tracing::warn!(
                    target: "admin",
                    %error,
                    "Full restore completed but in-memory site state refresh failed"
                );
            }
            Ok(fresh_sid)
        }
        Err(error) => {
            warn!("{session_warning_log}: could not create session: {error}");
            Ok(String::new())
        }
    }
}

fn restrict_private_key_material_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::metadata(path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Inspect {}: {error}", path.display()))
        })?;
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Set permissions on {}: {error}",
                path.display()
            ))
        })?;

        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Read {}: {error}", path.display()))
            })? {
                let entry = entry.map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Read dir entry: {error}"))
                })?;
                restrict_private_key_material_permissions(&entry.path())?;
            }
        }
    }

    #[cfg(not(unix))]
    let _ = path;

    Ok(())
}

fn full_restore_success_response(
    jar: CookieJar,
    headers: &HeaderMap,
    peer: std::net::SocketAddr,
    fresh_sid: String,
    xhr_request: bool,
) -> Response {
    let mut new_cookie = Cookie::new(super::SESSION_COOKIE, fresh_sid);
    new_cookie.set_http_only(true);
    new_cookie.set_same_site(super::ADMIN_COOKIE_SAME_SITE);
    new_cookie.set_path("/");
    new_cookie.set_secure(super::should_set_secure_cookie(headers, Some(peer)));
    new_cookie.set_max_age(time::Duration::seconds(CONFIG.session_duration));

    if xhr_request {
        let response = crate::handlers::board::xhr_redirect_response(
            &restore_success_redirect_target(RestoreKind::Full, None),
        )
        .unwrap_or_else(|error| error.into_response());
        return (jar.add(new_cookie), response).into_response();
    }

    (
        jar.add(new_cookie),
        Redirect::to(&restore_success_redirect_target(RestoreKind::Full, None)),
    )
        .into_response()
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
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
        let session_id = restore_auth_preflight(&state, &headers, &jar, Some(peer)).await?;
        let upload = stream_restore_upload_to_tempfile(RestoreKind::Full, &mut multipart).await?;
        validate_streamed_restore_upload(RestoreKind::Full, &jar, &upload)?;
        let zip_tmp = upload.temp_file;
        let restore_tor_hidden_service_keys = upload.restore_tor_hidden_service_keys;
        let uploaded_filename = upload.uploaded_filename;

        let upload_dir = CONFIG.upload_dir.clone();
        let live_tor_hidden_service_keys_dir =
            crate::config::configured_tor_hidden_service_keys_dir();

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
                    live_tor_hidden_service_keys_dir.as_deref(),
                    restore_tor_hidden_service_keys,
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

            full_restore_success_response(jar, &headers, peer, fresh_sid, xhr_request)
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

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
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
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;

    let path = full_backup_dir().join(&safe_filename);
    let upload_dir = CONFIG.upload_dir.clone();
    let restore_tor_hidden_service_keys = form.restore_tor_hidden_service_keys;
    let live_tor_hidden_service_keys_dir = crate::config::configured_tor_hidden_service_keys_dir();

    let restore_result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut live_conn = pool.get()?;
            let admin_id = super::require_admin_session_sid(&live_conn, session_id.as_deref())?;

            let zip_file = std::fs::File::open(&path)
                .map_err(|_| AppError::NotFound("Backup file not found.".into()))?;
            let mut archive = zip::ZipArchive::new(std::io::BufReader::new(zip_file))
                .map_err(|e| AppError::BadRequest(format!("Invalid zip: {e}")))?;
            execute_full_restore(
                &mut live_conn,
                admin_id,
                &upload_dir,
                live_tor_hidden_service_keys_dir.as_deref(),
                restore_tor_hidden_service_keys,
                &mut archive,
                "Restore-saved",
                "Restore-saved completed",
                "Restore-saved",
                "Restore-saved",
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)));

    let fresh_sid = match restore_result {
        Ok(Ok(fresh_sid)) => fresh_sid,
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

    if fresh_sid.is_empty() {
        let jar = jar.remove(Cookie::from(super::SESSION_COOKIE));
        return Ok((jar, Redirect::to("/admin")).into_response());
    }

    Ok(full_restore_success_response(
        jar, &headers, peer, fresh_sid, false,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        execute_full_restore, full_restore_success_response,
        validate_full_restore_db_trust_boundary,
    };
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use axum_extra::extract::cookie::CookieJar;
    use std::collections::BTreeMap;
    use std::io::Write as _;

    static TOR_PERMISSION_FAILURE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PrivatePermissionFailureReset;

    impl Drop for PrivatePermissionFailureReset {
        fn drop(&mut self) {
            crate::pending_fs::set_private_permission_failure_for_test(None);
        }
    }

    fn create_snapshot_db() -> std::path::PathBuf {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("snapshot.db");
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db conn");
        crate::db::create_board(&conn, "tech", "Technology", "", false).expect("create board");
        let db_path_str = db_path.to_str().expect("db path").replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{db_path_str}'"))
            .expect("vacuum snapshot");
        temp_dir.keep().join("snapshot.db")
    }

    fn write_full_backup_zip(
        zip_path: &std::path::Path,
        backup_tor_keys: Option<&[(&str, &str)]>,
        legacy_manifest: bool,
    ) {
        let db_path = create_snapshot_db();
        write_full_backup_zip_from_db(zip_path, &db_path, backup_tor_keys, legacy_manifest);
    }

    fn write_full_backup_zip_from_db(
        zip_path: &std::path::Path,
        db_path: &std::path::Path,
        backup_tor_keys: Option<&[(&str, &str)]>,
        legacy_manifest: bool,
    ) {
        let db_bytes = std::fs::read(db_path).expect("read db");
        let (tor_hidden_service_keys_included, tor_hidden_service_key_file_count) =
            backup_tor_keys.map_or((false, 0_u64), |files| (true, files.len() as u64));
        let manifest_json = if legacy_manifest {
            serde_json::json!({
                "version": 2,
                "generated_at": 1_700_000_000_i64,
                "rustchan_version": "1.1.3",
                "db_bytes": db_bytes.len(),
                "upload_file_count": 0_u64,
                "favicon_file_count": 0_u64,
                "banner_file_count": 0_u64,
                "boards": []
            })
        } else {
            serde_json::json!({
                "version": 3,
                "generated_at": 1_700_000_000_i64,
                "rustchan_version": "1.1.3",
                "db_bytes": db_bytes.len(),
                "upload_file_count": 0_u64,
                "favicon_file_count": 0_u64,
                "banner_file_count": 0_u64,
                "tor_hidden_service_keys_included": tor_hidden_service_keys_included,
                "tor_hidden_service_key_file_count": tor_hidden_service_key_file_count,
                "boards": []
            })
        };

        let file = std::fs::File::create(zip_path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(super::super::common::FULL_BACKUP_MANIFEST_NAME, options)
            .expect("start manifest");
        zip.write_all(&serde_json::to_vec(&manifest_json).expect("serialize full backup manifest"))
            .expect("write manifest");
        zip.start_file("chan.db", options).expect("start db");
        zip.write_all(&db_bytes).expect("write db");
        if let Some(files) = backup_tor_keys {
            for (name, contents) in files {
                zip.start_file(
                    format!(
                        "{}{}",
                        super::super::common::FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX,
                        name
                    ),
                    options,
                )
                .expect("start tor key file");
                zip.write_all(contents.as_bytes())
                    .expect("write tor key file");
            }
        }
        zip.finish().expect("finish zip");
    }

    fn restore_zip_into_temp_site(zip_path: &std::path::Path) -> (String, Vec<String>) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let fresh_sid = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("restore should succeed");

        let session_ids = live_conn
            .prepare("SELECT id FROM admin_sessions ORDER BY id")
            .expect("prepare session query")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query sessions")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect sessions");

        (fresh_sid, session_ids)
    }

    fn read_tree(root: &std::path::Path) -> BTreeMap<String, String> {
        fn visit(
            root: &std::path::Path,
            dir: &std::path::Path,
            out: &mut BTreeMap<String, String>,
        ) {
            let entries = std::fs::read_dir(dir).expect("read dir");
            for entry in entries {
                let entry = entry.expect("dir entry");
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else if path.is_file() {
                    let rel = path
                        .strip_prefix(root)
                        .expect("relative path")
                        .to_string_lossy()
                        .replace('\\', "/");
                    let contents = std::fs::read_to_string(&path).expect("read file");
                    out.insert(rel, contents);
                }
            }
        }

        let mut out = BTreeMap::new();
        if root.exists() {
            visit(root, root, &mut out);
        }
        out
    }

    #[test]
    fn saved_full_restore_success_response_sets_session_cookie_and_reopens_section() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost"));

        let response = full_restore_success_response(
            CookieJar::new(),
            &headers,
            std::net::SocketAddr::from(([127, 0, 0, 1], 41000)),
            "fresh-session".to_string(),
            false,
        );

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/admin/panel?restored=1&open=full-backup-restore#full-backup-restore")
        );

        let set_cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.contains(super::super::SESSION_COOKIE))
            .expect("session cookie");
        assert!(set_cookie.contains("chan_admin_session=fresh-session"));
    }

    #[test]
    fn full_restore_without_tor_key_opt_in_leaves_live_tor_identity_untouched() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let tor_keys_dir = temp_dir.path().join("live-tor-keys");
        std::fs::create_dir_all(&tor_keys_dir).expect("create live tor dir");
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "live-secret")
            .expect("write live secret");
        std::fs::write(tor_keys_dir.join("hs_ed25519_public_key"), "live-public")
            .expect("write live public");

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("restore should succeed");

        let live_tree = read_tree(&tor_keys_dir);
        assert_eq!(
            live_tree,
            BTreeMap::from([
                (
                    "hs_ed25519_public_key".to_string(),
                    "live-public".to_string()
                ),
                (
                    "hs_ed25519_secret_key".to_string(),
                    "live-secret".to_string()
                ),
            ])
        );
    }

    #[test]
    fn full_restore_without_tor_key_opt_in_succeeds_without_any_tor_configuration() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("restore should succeed without tor configuration");
    }

    #[test]
    fn full_restore_purges_restored_admin_sessions_before_issuing_new_session() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = create_snapshot_db();
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
            conn.execute(
                "INSERT INTO admin_users (id, username, password_hash)
                 VALUES (1, 'restored-admin', 'restored-hash')",
                [],
            )
            .expect("seed restored admin");
            conn.execute(
                "INSERT INTO admin_sessions (id, admin_id, expires_at)
                 VALUES ('stale-session-from-backup', 1, unixepoch() + 86400)",
                [],
            )
            .expect("seed stale session");
        }
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip_from_db(&zip_path, &db_path, None, false);

        let (fresh_sid, session_ids) = restore_zip_into_temp_site(&zip_path);

        assert_eq!(session_ids, vec![fresh_sid]);
    }

    #[test]
    fn full_restore_rejects_restored_board_short_name_that_would_escape_routes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = create_snapshot_db();
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
            conn.execute("UPDATE boards SET short_name = '../admin'", [])
                .expect("seed invalid board short name");
        }
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip_from_db(&zip_path, &db_path, None, false);

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should reject invalid restored board short name");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("Invalid board short name"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_rejects_restored_post_media_path_pointing_to_unknown_board() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = create_snapshot_db();
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
            let board_id: i64 = conn
                .query_row(
                    "SELECT id FROM boards WHERE short_name = 'tech'",
                    [],
                    |row| row.get(0),
                )
                .expect("tech board id");
            conn.execute(
                "INSERT INTO threads (id, board_id, subject, created_at, bumped_at, locked, sticky, archived, reply_count)
                 VALUES (1, ?1, 'ghost', unixepoch(), unixepoch(), 0, 0, 0, 0)",
                [board_id],
            )
            .expect("seed thread");
            conn.execute(
                "INSERT INTO posts
                 (id, thread_id, board_id, name, body, body_html, file_path, file_name, file_size,
                  thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                 VALUES
                 (1, 1, ?1, 'anon', 'body', '<p>body</p>', 'ghost/doc.pdf', 'doc.pdf', 1,
                  'ghost/thumbs/doc.svg', 'application/pdf', 'pdf', unixepoch(), 'token', 1)",
                [board_id],
            )
            .expect("seed invalid restored media path");
            conn.execute(
                "INSERT INTO file_hashes (sha256, file_path, thumb_path, mime_type, created_at)
                 VALUES ('ghost-hash', 'ghost/doc.pdf', 'ghost/thumbs/doc.svg', 'application/pdf', unixepoch())",
                [],
            )
            .expect("seed invalid restored file hash path");
        }
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip_from_db(&zip_path, &db_path, None, false);

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should reject invalid restored media paths");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("points to unknown board /ghost/"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_rejects_cross_board_thumb_path_on_restored_post() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = create_snapshot_db();
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create second board");
            let tech_board_id: i64 = conn
                .query_row(
                    "SELECT id FROM boards WHERE short_name = 'tech'",
                    [],
                    |row| row.get(0),
                )
                .expect("tech board id");
            conn.execute(
                "INSERT INTO threads (id, board_id, subject, created_at, bumped_at, locked, sticky, archived, reply_count)
                 VALUES (1, ?1, 'doc', unixepoch(), unixepoch(), 0, 0, 0, 0)",
                [tech_board_id],
            )
            .expect("seed thread");
            conn.execute(
                "INSERT INTO posts
                 (id, thread_id, board_id, name, body, body_html, file_path, file_name, file_size,
                  thumb_path, mime_type, media_type, created_at, deletion_token, is_op)
                 VALUES
                 (1, 1, ?1, 'anon', 'body', '<p>body</p>', 'tech/doc.pdf', 'doc.pdf', 1,
                  'b/thumbs/doc.svg', 'application/pdf', 'pdf', unixepoch(), 'token', 1)",
                [tech_board_id],
            )
            .expect("seed cross-board thumb path");
            conn.execute(
                "INSERT INTO file_hashes (sha256, file_path, thumb_path, mime_type, created_at)
                 VALUES ('cross-board-hash', 'tech/doc.pdf', 'b/thumbs/doc.svg', 'application/pdf', unixepoch())",
                [],
            )
            .expect("seed cross-board file hash");
        }
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip_from_db(&zip_path, &db_path, None, false);

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should reject cross-board thumb paths");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("escapes its board /tech/"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_trust_boundary_allows_empty_file_hash_thumb_path() {
        let db_path = create_snapshot_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
        conn.execute(
            "INSERT INTO file_hashes (sha256, file_path, thumb_path, mime_type, created_at)
             VALUES ('generic-hash', 'tech/file.bin', '', 'application/octet-stream', unixepoch())",
            [],
        )
        .expect("seed generic file hash");

        validate_full_restore_db_trust_boundary(&conn)
            .expect("empty file-hash thumb path should remain allowed");
    }

    #[test]
    fn full_restore_trust_boundary_rejects_cross_board_file_hash_pairing() {
        let db_path = create_snapshot_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open snapshot");
        crate::db::create_board(&conn, "b", "Random", "", false).expect("create second board");
        conn.execute(
            "INSERT INTO file_hashes (sha256, file_path, thumb_path, mime_type, created_at)
             VALUES ('cross-board-hash', 'tech/doc.pdf', 'b/thumbs/doc.svg', 'application/pdf', unixepoch())",
            [],
        )
        .expect("seed cross-board file hash");

        let error = validate_full_restore_db_trust_boundary(&conn)
            .expect_err("cross-board file_hash should be rejected");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("mixes boards between file_path and thumb_path"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_rejects_tor_key_opt_in_when_tor_is_unconfigured() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            None,
            true,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should reject tor opt-in without configuration");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("Tor hidden service key restore is not available"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_with_tor_key_opt_in_creates_missing_live_identity_dir() {
        let _tor_permission_failure_guard = TOR_PERMISSION_FAILURE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let tor_keys_dir = temp_dir.path().join("runtime/tor/state/keystore");
        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            true,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("restore should create missing Tor identity dir");

        assert_eq!(
            read_tree(&tor_keys_dir),
            BTreeMap::from([
                (
                    "hs_ed25519_public_key".to_string(),
                    "backup-public".to_string()
                ),
                (
                    "hs_ed25519_secret_key".to_string(),
                    "backup-secret".to_string()
                ),
            ])
        );
    }

    #[test]
    fn full_restore_with_tor_key_opt_in_replaces_live_identity_without_merging() {
        let _tor_permission_failure_guard = TOR_PERMISSION_FAILURE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let tor_keys_dir = temp_dir.path().join("live-tor-keys");
        std::fs::create_dir_all(&tor_keys_dir).expect("create live tor dir");
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "live-secret")
            .expect("write live secret");
        std::fs::write(tor_keys_dir.join("hs_ed25519_public_key"), "live-public")
            .expect("write live public");
        std::fs::write(tor_keys_dir.join("stale-file.txt"), "stale").expect("write stale");

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            true,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("restore should succeed");

        let live_tree = read_tree(&tor_keys_dir);
        assert_eq!(
            live_tree,
            BTreeMap::from([
                (
                    "hs_ed25519_public_key".to_string(),
                    "backup-public".to_string()
                ),
                (
                    "hs_ed25519_secret_key".to_string(),
                    "backup-secret".to_string()
                ),
            ])
        );
        assert!(!tor_keys_dir.join("stale-file.txt").exists());
    }

    #[test]
    fn full_restore_tor_key_finalize_failure_keeps_pending_restore_for_reconciliation() {
        let _tor_permission_failure_guard = TOR_PERMISSION_FAILURE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(
            &zip_path,
            Some(&[
                ("hs_ed25519_secret_key", "backup-secret"),
                ("hs_ed25519_public_key", "backup-public"),
            ]),
            false,
        );

        let tor_keys_dir = temp_dir.path().join("live-tor-keys");
        std::fs::create_dir_all(&tor_keys_dir).expect("create live tor dir");
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "live-secret")
            .expect("write live secret");

        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        crate::pending_fs::set_private_permission_failure_for_test(Some(
            "simulated Tor key permission failure".to_string(),
        ));
        let _private_permission_failure_reset = PrivatePermissionFailureReset;
        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            true,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should report failed Tor key finalization");

        assert!(error
            .to_string()
            .contains("remains pending for startup reconciliation"));
        let pending_ops =
            crate::db::list_pending_fs_ops(&live_conn).expect("list pending fs ops after failure");
        let [pending_op] = pending_ops.as_slice() else {
            panic!("expected exactly one pending restore op");
        };
        assert_eq!(pending_op.kind, crate::pending_fs::FULL_RESTORE_SWAP_KIND);
        let payload: crate::pending_fs::FullRestoreSwapPayload =
            serde_json::from_str(&pending_op.payload_json).expect("restore payload json");
        let [tor_swap] = payload.additional_swaps.as_slice() else {
            panic!("expected one additional Tor key swap");
        };
        assert_eq!(tor_swap.live, tor_keys_dir.display().to_string());
        assert!(tor_swap.restrict_private_permissions);

        crate::pending_fs::set_private_permission_failure_for_test(None);
        crate::pending_fs::finalize_full_restore_payload(
            &payload,
            &upload_dir,
            Some(&tor_keys_dir),
        )
        .expect("finalize pending restore after failure clears");
        crate::db::delete_pending_fs_op(&live_conn, &pending_op.id).expect("delete pending op");
        assert!(
            crate::db::list_pending_fs_ops(&live_conn)
                .expect("list pending ops after finalization")
                .is_empty(),
            "pending restore op should clear only after finalization completes"
        );
        assert_eq!(
            read_tree(&tor_keys_dir),
            BTreeMap::from([
                (
                    "hs_ed25519_public_key".to_string(),
                    "backup-public".to_string()
                ),
                (
                    "hs_ed25519_secret_key".to_string(),
                    "backup-secret".to_string()
                ),
            ])
        );
    }

    #[test]
    fn full_restore_rejects_requested_tor_key_restore_when_backup_has_none() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("backup.zip");
        write_full_backup_zip(&zip_path, None, false);

        let tor_keys_dir = temp_dir.path().join("live-tor-keys");
        std::fs::create_dir_all(&tor_keys_dir).expect("create live tor dir");
        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        let error = execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            true,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect_err("restore should reject missing Tor identity");

        match error {
            crate::error::AppError::BadRequest(message) => {
                assert!(message.contains("does not include Tor hidden service keys"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn full_restore_accepts_legacy_full_backup_without_tor_metadata() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let zip_path = temp_dir.path().join("legacy.zip");
        write_full_backup_zip(&zip_path, None, true);

        let tor_keys_dir = temp_dir.path().join("live-tor-keys");
        std::fs::create_dir_all(&tor_keys_dir).expect("create live tor dir");
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "live-secret")
            .expect("write live secret");
        let pool = crate::db::init_test_pool().expect("test pool");
        let mut live_conn = pool.get().expect("db conn");
        let file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(file).expect("zip archive");
        let upload_dir = temp_dir.path().join("uploads");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");

        execute_full_restore(
            &mut live_conn,
            1,
            upload_dir.to_str().expect("upload dir"),
            Some(&tor_keys_dir),
            false,
            &mut archive,
            "Test restore",
            "Test restore completed",
            "Test restore",
            "Test restore",
        )
        .expect("legacy restore should succeed");

        assert_eq!(
            read_tree(&tor_keys_dir),
            BTreeMap::from([(
                "hs_ed25519_secret_key".to_string(),
                "live-secret".to_string()
            )])
        );
    }
}
