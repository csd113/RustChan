use super::{
    add_dir_to_zip, admin_panel_redirect_anchor_open, banner, board_backup_dir, board_backup_types,
    common, count_files_in_dir, db, enforce_full_backup_retention, full_backup_dir, header,
    invalidate_backup_list_cache, local_backup_timestamp_label, log_backup_phase,
    log_backup_progress, new_session_id, prune_stale_temp_board_downloads,
    require_admin_post_origin_and_csrf, require_admin_session_sid, saved_backup,
    temp_board_download_dir, unique_backup_filename, validate_board_short_name,
    write_temp_board_download_token, zip_file_options_for_path, AppError, AppState, BackupListKind,
    BackupStorageMode, CookieJar, Form, HeaderMap, Ordering, Path, PathBuf, Redirect, Response,
    Result, State, Utc, Write, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use rusqlite::params;
use serde::Deserialize;

#[expect(
    clippy::cognitive_complexity,
    reason = "full backup stages and cleanup remain together to preserve a fail-closed operation"
)]
#[expect(
    clippy::too_many_lines,
    reason = "snapshot, manifest, payload publication, and cleanup share one fail-closed backup operation"
)]
pub(in crate::server) fn create_full_backup_to_server(
    pool: &db::DbPool,
    session_id: Option<&str>,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    copies_to_keep: u64,
    include_tor_hidden_service_keys: bool,
    storage_mode: BackupStorageMode,
    split_zip_part_size: u64,
) -> Result<String> {
    let conn = pool.get()?;
    let automated = session_id.is_none();
    if let Some(session_id) = session_id {
        require_admin_session_sid(&conn, Some(session_id))?;
    }
    let uploads_base = Path::new(&CONFIG.upload_dir);
    let global_favicon_dir = crate::favicon::global_backup_source_dir();
    let mut tor_hidden_service_keys_dir = if include_tor_hidden_service_keys {
        match common::resolve_tor_hidden_service_keys_availability(
            true,
            crate::config::configured_tor_hidden_service_keys_dir(),
            "Tor hidden service key backups are not available with the current configuration.",
        ) {
            Ok(common::TorHiddenServiceKeysAvailability::Skipped) => None,
            Ok(common::TorHiddenServiceKeysAvailability::Available(dir)) => Some(dir),
            Err(error) if automated => {
                tracing::warn!(
                    target: "admin",
                    error = %error,
                    "Skipping Tor hidden service keys for scheduled full backup"
                );
                None
            }
            Err(error) => return Err(error),
        }
    } else {
        None
    };

    progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
    log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);

    progress.reset(crate::middleware::backup_phase::COUNT_FILES);
    log_backup_phase(crate::middleware::backup_phase::COUNT_FILES);
    let global_banner_dir = banner::backup_source_dir();
    let favicon_file_count = count_files_in_dir(&global_favicon_dir);
    let banner_file_count = count_files_in_dir(&global_banner_dir);
    let tor_hidden_service_key_file_count = if let Some(dir) = tor_hidden_service_keys_dir.as_ref()
    {
        match count_required_private_files(
            dir,
            "Tor hidden service keys were requested, but the configured identity directory could not be read.",
        ) {
            Ok(count) => count,
            Err(error) if automated => {
                tracing::warn!(
                    target: "admin",
                    error = %error,
                    "Skipping Tor hidden service keys for scheduled full backup"
                );
                tor_hidden_service_keys_dir = None;
                0
            }
            Err(error) => return Err(error),
        }
    } else {
        0
    };
    let include_tor_hidden_service_keys = tor_hidden_service_keys_dir.is_some();
    let file_count = count_files_in_dir(uploads_base)
        .saturating_add(favicon_file_count)
        .saturating_add(banner_file_count)
        .saturating_add(tor_hidden_service_key_file_count);
    let backup_id = saved_backup::build_backup_id(saved_backup::BackupScope::FullSite, "full-site");
    let root_dir = saved_backup::create_backup_root(&backup_id)?;
    let db_dir = root_dir.join("db");
    let config_dir = root_dir.join("config");
    ensure_backup_dir(&db_dir)?;
    ensure_backup_dir(&config_dir)?;

    progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
    log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);
    let db_snapshot_path = db_dir.join("rustchan.sqlite3");
    let db_snapshot_str = db_snapshot_path
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Backup DB path non-UTF-8")))?
        .replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{db_snapshot_str}'"))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {error}")))?;
    restrict_backup_file(&db_snapshot_path)?;
    let db_snapshot_size = std::fs::metadata(&db_snapshot_path)
        .map(|metadata| metadata.len())
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Stat DB snapshot: {error}")))?;
    let db_snapshot_sha = saved_backup::sha256_hex_for_file(&db_snapshot_path)?;

    let mut files = Vec::new();
    push_v4_file_entry(
        &mut files,
        "db/rustchan.sqlite3".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Db,
        db_snapshot_size,
        db_snapshot_sha.clone(),
    );

    let boards = collect_backup_board_summaries(&conn)?;
    let settings_path = crate::config::data_dir().join("settings.toml");
    if settings_path.is_file() {
        let destination = config_dir.join("settings.toml");
        let (size, sha256) = copy_regular_file_to_backup(&settings_path, &destination)?;
        push_v4_file_entry(
            &mut files,
            "config/settings.toml".to_owned(),
            None,
            None,
            saved_backup::BackupFileKind::Settings,
            size,
            sha256,
        );
    }

    progress.reset(crate::middleware::backup_phase::COMPRESS);
    log_backup_phase(crate::middleware::backup_phase::COMPRESS);
    progress
        .files_total
        .store(file_count.saturating_add(1), Ordering::Relaxed);

    for board in &boards {
        validate_board_short_name(&board.short_name)?;
        let board_manifest = build_board_backup_manifest(&conn, &board.short_name)?;
        write_board_exports_to_v4_dir(&root_dir, &board_manifest, &mut files)?;
    }

    copy_runtime_tree_into_v4_dir(
        uploads_base,
        &root_dir,
        &mut files,
        |_path, runtime_rel| {
            let board_short = runtime_rel.split('/').next().ok_or_else(|| {
                AppError::BadRequest("Upload path missing board directory.".into())
            })?;
            validate_board_short_name(board_short)?;
            let (logical_path, kind) =
                saved_backup::runtime_upload_path_to_logical(board_short, runtime_rel)?;
            Ok((
                logical_path,
                Some(runtime_rel.to_owned()),
                Some(board_short.to_owned()),
                kind,
            ))
        },
        Some(progress),
    )?;

    copy_runtime_tree_into_v4_dir(
        &global_favicon_dir,
        &root_dir,
        &mut files,
        |_path, runtime_rel| {
            let logical_path = format!("site-assets/favicon/{runtime_rel}");
            Ok((
                logical_path,
                Some(format!("favicon/{runtime_rel}")),
                None,
                saved_backup::BackupFileKind::Favicon,
            ))
        },
        Some(progress),
    )?;

    copy_runtime_tree_into_v4_dir(
        &global_banner_dir,
        &root_dir,
        &mut files,
        |_path, runtime_rel| {
            let logical_path = format!("site-assets/banner/{runtime_rel}");
            Ok((
                logical_path,
                Some(format!("banner/{runtime_rel}")),
                None,
                saved_backup::BackupFileKind::Banner,
            ))
        },
        Some(progress),
    )?;

    if let Some(tor_hidden_service_keys_dir) = tor_hidden_service_keys_dir.as_ref() {
        copy_runtime_tree_into_v4_dir(
            tor_hidden_service_keys_dir,
            &root_dir,
            &mut files,
            |_path, runtime_rel| {
                let logical_path = format!("tor-keys/{runtime_rel}");
                Ok((
                    logical_path,
                    Some(runtime_rel.to_owned()),
                    None,
                    saved_backup::BackupFileKind::TorKey,
                ))
            },
            Some(progress),
        )?;
    }

    let mut manifest = saved_backup::BackupManifest {
        format: saved_backup::BACKUP_V4_FORMAT.to_owned(),
        archive_container: saved_backup::BACKUP_V4_ARCHIVE_CONTAINER.to_owned(),
        backup_id,
        created_at: Utc::now().timestamp(),
        completed_at: None,
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        scope: saved_backup::BackupScope::FullSite,
        storage_mode,
        included_boards: boards,
        includes: saved_backup::BackupIncludeFlags {
            database: true,
            settings: settings_path.is_file(),
            uploads: true,
            thumbnails: true,
            tor_keys: include_tor_hidden_service_keys,
            board_exports: true,
            file_inventory: true,
        },
        db_snapshot: Some(saved_backup::DbSnapshotInfo {
            path: "db/rustchan.sqlite3".to_owned(),
            size: db_snapshot_size,
            sha256: db_snapshot_sha,
            integrity_check: snapshot_db_health_output(&conn, "integrity_check"),
            foreign_key_check: snapshot_db_health_output(&conn, "foreign_key_check"),
        }),
        files,
        parts: Vec::new(),
        maintenance: None,
    };
    if storage_mode == BackupStorageMode::SplitZip {
        materialize_split_zip_parts(&root_dir, &mut manifest, split_zip_part_size)?;
    }
    drop(conn);

    let backup_ref = finalize_v4_backup_root(&root_dir, manifest)?;
    invalidate_backup_list_cache(&full_backup_dir(), BackupListKind::Full);

    match enforce_full_backup_retention(copies_to_keep) {
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

    let size = saved_backup::scan_dir_stats(&root_dir).bytes;
    tracing::info!(
        target: "admin",
        backup_id = %backup_ref,
        path = %root_dir.display(),
        bytes = size,
        automated = session_id.is_none(),
        includes_tor_hidden_service_keys = include_tor_hidden_service_keys,
        "Backup v4 full backup created"
    );
    progress
        .phase
        .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
    log_backup_phase(crate::middleware::backup_phase::DONE);
    Ok(backup_ref)
}

#[expect(
    clippy::too_many_lines,
    reason = "maintenance snapshot, manifest publication, retention, and status updates form one operation"
)]
pub(in crate::server::handlers::admin) fn create_pre_maintenance_backup_to_server(
    pool: &db::DbPool,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    operation: &str,
    job_id: u64,
    reason: &str,
) -> Result<String> {
    let conn = pool.get()?;
    let backup_id =
        saved_backup::build_backup_id(saved_backup::BackupScope::PreMaintenance, "pre-repair-db");
    let root_dir = saved_backup::create_backup_root(&backup_id)?;
    let db_dir = root_dir.join("db");
    let config_dir = root_dir.join("config");
    let maintenance_dir = root_dir.join("maintenance");
    ensure_backup_dir(&db_dir)?;
    ensure_backup_dir(&config_dir)?;
    ensure_backup_dir(&maintenance_dir)?;

    progress.reset(crate::middleware::backup_phase::SNAPSHOT_DB);
    log_backup_phase(crate::middleware::backup_phase::SNAPSHOT_DB);

    let db_snapshot_path = db_dir.join("rustchan.sqlite3");
    let db_snapshot_str = db_snapshot_path
        .to_str()
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Backup DB path non-UTF-8")))?
        .replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{db_snapshot_str}'"))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("VACUUM INTO: {error}")))?;
    restrict_backup_file(&db_snapshot_path)?;
    let db_snapshot_size = std::fs::metadata(&db_snapshot_path)
        .map(|metadata| metadata.len())
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Stat DB snapshot: {error}")))?;
    let db_snapshot_sha = saved_backup::sha256_hex_for_file(&db_snapshot_path)?;

    let mut files = Vec::new();
    push_v4_file_entry(
        &mut files,
        "db/rustchan.sqlite3".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Db,
        db_snapshot_size,
        db_snapshot_sha.clone(),
    );

    let settings_path = crate::config::data_dir().join("settings.toml");
    if settings_path.is_file() {
        let destination = config_dir.join("settings.toml");
        let (size, sha256) = copy_regular_file_to_backup(&settings_path, &destination)?;
        push_v4_file_entry(
            &mut files,
            "config/settings.toml".to_owned(),
            None,
            None,
            saved_backup::BackupFileKind::Settings,
            size,
            sha256,
        );
    }

    let pre_integrity = snapshot_db_health_output(&conn, "integrity_check").unwrap_or_default();
    let pre_foreign_key = snapshot_db_health_output(&conn, "foreign_key_check").unwrap_or_default();

    let repair_request_path = maintenance_dir.join("repair-request.json");
    let repair_request = serde_json::json!({
        "operation": operation,
        "job_id": job_id,
        "requested_at": Utc::now().timestamp(),
        "reason": reason,
        "backup_id": backup_id,
    });
    let (request_size, request_sha) =
        write_pretty_json_file(&repair_request_path, &repair_request)?;
    push_v4_file_entry(
        &mut files,
        "maintenance/repair-request.json".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Maintenance,
        request_size,
        request_sha,
    );

    let integrity_path = maintenance_dir.join("pre-integrity-check.txt");
    write_backup_bytes(&integrity_path, pre_integrity.as_bytes())?;
    push_v4_file_entry(
        &mut files,
        "maintenance/pre-integrity-check.txt".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Maintenance,
        u64::try_from(pre_integrity.len()).unwrap_or(u64::MAX),
        saved_backup::sha256_hex_for_bytes(pre_integrity.as_bytes()),
    );

    let foreign_key_path = maintenance_dir.join("pre-foreign-key-check.txt");
    write_backup_bytes(&foreign_key_path, pre_foreign_key.as_bytes())?;
    push_v4_file_entry(
        &mut files,
        "maintenance/pre-foreign-key-check.txt".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Maintenance,
        u64::try_from(pre_foreign_key.len()).unwrap_or(u64::MAX),
        saved_backup::sha256_hex_for_bytes(pre_foreign_key.as_bytes()),
    );

    let schema_dump = {
        let mut statement = conn
            .prepare(
                "SELECT sql FROM sqlite_schema
                 WHERE sql IS NOT NULL
                 ORDER BY type ASC, name ASC",
            )
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Prepare schema dump: {error}")))?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Query schema dump: {error}")))?;
        let mut sql = String::new();
        for row in rows {
            let statement_sql = row.map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Read schema dump: {error}"))
            })?;
            sql.push_str(&statement_sql);
            sql.push_str(";\n\n");
        }
        sql
    };
    let schema_path = maintenance_dir.join("pre-schema.sql");
    write_backup_bytes(&schema_path, schema_dump.as_bytes())?;
    push_v4_file_entry(
        &mut files,
        "maintenance/pre-schema.sql".to_owned(),
        None,
        None,
        saved_backup::BackupFileKind::Maintenance,
        u64::try_from(schema_dump.len()).unwrap_or(u64::MAX),
        saved_backup::sha256_hex_for_bytes(schema_dump.as_bytes()),
    );

    let pending_fs_ops = db::list_pending_fs_ops(&conn)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("List pending_fs_ops: {error}")))?;
    if !pending_fs_ops.is_empty() {
        let pending_fs_path = maintenance_dir.join("pending-fs-ops.json");
        let snapshot = pending_fs_ops
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "kind": row.kind,
                    "payload_json": row.payload_json,
                })
            })
            .collect::<Vec<_>>();
        let (size, sha256) = write_pretty_json_file(&pending_fs_path, &snapshot)?;
        push_v4_file_entry(
            &mut files,
            "maintenance/pending-fs-ops.json".to_owned(),
            None,
            None,
            saved_backup::BackupFileKind::PendingFsOps,
            size,
            sha256,
        );
    }

    progress.reset(crate::middleware::backup_phase::DONE);
    log_backup_phase(crate::middleware::backup_phase::DONE);

    let manifest = saved_backup::BackupManifest {
        format: saved_backup::BACKUP_V4_FORMAT.to_owned(),
        archive_container: saved_backup::BACKUP_V4_ARCHIVE_CONTAINER.to_owned(),
        backup_id,
        created_at: Utc::now().timestamp(),
        completed_at: None,
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        scope: saved_backup::BackupScope::PreMaintenance,
        storage_mode: BackupStorageMode::Directory,
        included_boards: Vec::new(),
        includes: saved_backup::BackupIncludeFlags {
            database: true,
            settings: settings_path.is_file(),
            uploads: false,
            thumbnails: false,
            tor_keys: false,
            board_exports: false,
            file_inventory: false,
        },
        db_snapshot: Some(saved_backup::DbSnapshotInfo {
            path: "db/rustchan.sqlite3".to_owned(),
            size: db_snapshot_size,
            sha256: db_snapshot_sha,
            integrity_check: Some(pre_integrity.clone()),
            foreign_key_check: Some(pre_foreign_key.clone()),
        }),
        files,
        parts: Vec::new(),
        maintenance: Some(saved_backup::MaintenanceMetadata {
            operation: Some(operation.to_owned()),
            job_id: Some(job_id),
            requested_at: Some(Utc::now().timestamp()),
            risk_class: Some("db_mutating".to_owned()),
            includes_uploads: false,
            includes_file_inventory: false,
            includes_tor_keys: false,
            reason: Some(reason.to_owned()),
            pre_integrity_check: Some(pre_integrity),
            pre_foreign_key_check: Some(pre_foreign_key),
        }),
    };

    finalize_v4_backup_root(&root_dir, manifest)
}

/// Counts required private files.
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

/// Ensures backup dir.
fn ensure_backup_dir(path: &Path) -> Result<()> {
    crate::config::ensure_private_dir(path)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Create {}: {error}", path.display())))
}

fn restrict_backup_file(path: &Path) -> Result<()> {
    crate::config::restrict_private_file_permissions(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Set private permissions on {}: {error}",
            path.display()
        ))
    })
}

/// Writes backup bytes.
fn write_backup_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::config::write_private_file(path, bytes)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Write {}: {error}", path.display())))
}

/// Writes jsonl file.
fn write_jsonl_file<T: serde::Serialize>(path: &Path, rows: &[T]) -> Result<(u64, String)> {
    use sha2::Digest as _;

    if let Some(parent) = path.parent() {
        ensure_backup_dir(parent)?;
    }
    let mut file = std::fs::File::create(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create {}: {error}", path.display()))
    })?;
    restrict_backup_file(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut written = 0u64;
    for row in rows {
        let mut line = serde_json::to_vec(row)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Serialize JSONL row: {error}")))?;
        line.push(b'\n');
        file.write_all(&line).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Write {}: {error}", path.display()))
        })?;
        hasher.update(&line);
        written = written.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
    }
    Ok((written, hex::encode(hasher.finalize())))
}

/// Writes pretty JSON file.
fn write_pretty_json_file<T: serde::Serialize>(path: &Path, value: &T) -> Result<(u64, String)> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Serialize {}: {error}", path.display()))
    })?;
    write_backup_bytes(path, &bytes)?;
    Ok((
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        saved_backup::sha256_hex_for_bytes(&bytes),
    ))
}

fn relative_path_string(path: &Path, root: &Path) -> Result<String> {
    let rel = path.strip_prefix(root).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Resolve {} relative to {}: {error}",
            path.display(),
            root.display()
        ))
    })?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// Copies regular file to backup.
fn copy_regular_file_to_backup(source: &Path, destination: &Path) -> Result<(u64, String)> {
    if let Some(parent) = destination.parent() {
        ensure_backup_dir(parent)?;
    }
    let mut output = std::fs::File::create(destination).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create {}: {error}", destination.display()))
    })?;
    restrict_backup_file(destination)?;
    saved_backup::copy_file_and_hash(source, &mut output)
}

fn snapshot_db_health_output(conn: &rusqlite::Connection, pragma: &str) -> Option<String> {
    let sql = format!("PRAGMA {pragma}");
    let mut statement = conn.prepare(&sql).ok()?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .ok()?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row.ok()?);
    }
    (!values.is_empty()).then(|| values.join(" | "))
}

#[derive(Deserialize)]
pub(in crate::server) struct FullBackupCreateForm {
    #[serde(default, deserialize_with = "super::form_checkbox_bool")]
    include_tor_hidden_service_keys: bool,
    #[serde(default)]
    storage_mode: Option<String>,
    #[serde(default)]
    split_zip_part_size_gib: Option<u64>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Default split ZIP part size used by this handler.
const DEFAULT_SPLIT_ZIP_PART_SIZE: u64 = 4 * 1024 * 1024 * 1024;
/// Minimum permitted split ZIP part size.
const MIN_SPLIT_ZIP_PART_SIZE: u64 = 64 * 1024 * 1024;
/// Maximum permitted split ZIP part size.
const MAX_SPLIT_ZIP_PART_SIZE: u64 = 64 * 1024 * 1024 * 1024;

pub(in crate::server) fn parse_backup_storage_mode_value(
    value: Option<&str>,
) -> Result<BackupStorageMode> {
    match value.unwrap_or("directory") {
        "directory" => Ok(BackupStorageMode::Directory),
        "split_zip" => Ok(BackupStorageMode::SplitZip),
        _ => Err(AppError::BadRequest("Unknown backup storage mode.".into())),
    }
}

pub(in crate::server::handlers::admin) fn parse_split_zip_part_size_gib(
    value: Option<u64>,
) -> Result<u64> {
    let gib = value.unwrap_or(4);
    let bytes = gib
        .checked_mul(1024 * 1024 * 1024)
        .ok_or_else(|| AppError::BadRequest("Split ZIP part size is too large.".into()))?;
    if !(MIN_SPLIT_ZIP_PART_SIZE..=MAX_SPLIT_ZIP_PART_SIZE).contains(&bytes) {
        return Err(AppError::BadRequest(
            "Split ZIP part size must be between 64 MiB and 64 GiB.".into(),
        ));
    }
    Ok(bytes)
}

pub(in crate::server::handlers) const fn split_zip_part_size_gib(bytes: u64) -> u64 {
    bytes / (1024 * 1024 * 1024)
}

fn parse_full_backup_storage_mode(form: &FullBackupCreateForm) -> Result<BackupStorageMode> {
    parse_backup_storage_mode_value(form.storage_mode.as_deref())
}

fn parse_split_zip_part_size(form: &FullBackupCreateForm) -> Result<u64> {
    parse_split_zip_part_size_gib(form.split_zip_part_size_gib)
}

pub(in crate::server) async fn create_full_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<FullBackupCreateForm>,
) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Full backup creation")?;
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let progress = std::sync::Arc::clone(&state.backup_progress);
    let copies_to_keep = state.auto_full_backup_settings.snapshot().copies_to_keep;
    let include_tor_hidden_service_keys = form.include_tor_hidden_service_keys;
    let storage_mode = parse_full_backup_storage_mode(&form)?;
    let split_zip_part_size = if storage_mode == BackupStorageMode::SplitZip {
        parse_split_zip_part_size(&form)?
    } else {
        DEFAULT_SPLIT_ZIP_PART_SIZE
    };

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            create_full_backup_to_server(
                &pool,
                session_id.as_deref(),
                &progress,
                copies_to_keep,
                include_tor_hidden_service_keys,
                storage_mode,
                split_zip_part_size,
            )?;
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(super::super::admin_panel_redirect("Full backup saved on the server.").into_response())
}

#[derive(Deserialize)]
pub(in crate::server) struct BoardBackupCreateForm {
    board_short: String,
    download_after_create: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[expect(
    clippy::cognitive_complexity,
    reason = "board backup validation, snapshotting, and packaging form one guarded operation"
)]
#[expect(
    clippy::too_many_lines,
    reason = "board validation, snapshotting, archive publication, and cleanup share one operation"
)]
pub(in crate::server) async fn create_board_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BoardBackupCreateForm>,
) -> Result<Response> {
    let _maintenance_guard = state.maintenance_gate.try_begin("Board backup creation")?;

    let session_id = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

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
    let progress = std::sync::Arc::clone(&state.backup_progress);

    let filename = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
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

            if download_after_create {
                let backup_dir = {
                    prune_stale_temp_board_downloads();
                    temp_board_download_dir()
                };
                ensure_backup_dir(&backup_dir)?;
                let ts = local_backup_timestamp_label();
                let filename = unique_backup_filename(
                    &backup_dir,
                    &format!("rustchan-board-{board_short}-{ts}.zip"),
                );
                let final_path = backup_dir.join(&filename);
                let tmp_path = backup_dir.join(format!("{filename}.tmp"));

                let uploads_base = Path::new(&upload_dir);
                let board_upload_path = uploads_base.join(&board_short);
                let file_count = count_files_in_dir(&board_upload_path);
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
                    drop(std::fs::remove_file(&tmp_path));
                    return Err(error);
                }

                if let Err(error) = common::verify_board_backup_zip(&tmp_path) {
                    drop(std::fs::remove_file(&tmp_path));
                    return Err(error);
                }

                std::fs::rename(&tmp_path, &final_path).map_err(|error| {
                    drop(std::fs::remove_file(&tmp_path));
                    AppError::Internal(anyhow::anyhow!("Rename board backup: {error}"))
                })?;
                restrict_backup_file(&final_path)?;

                let size = std::fs::metadata(&final_path).map_or(0, |metadata| metadata.len());
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
                return Ok(filename);
            }

            let uploads_base = Path::new(&upload_dir);
            let board_upload_path = uploads_base.join(&board_short);
            let file_count = count_files_in_dir(&board_upload_path);
            progress.reset(crate::middleware::backup_phase::COMPRESS);
            log_backup_phase(crate::middleware::backup_phase::COMPRESS);
            progress
                .files_total
                .store(file_count.saturating_add(1), Ordering::Relaxed);

            let backup_id = saved_backup::build_backup_id(
                saved_backup::BackupScope::Board,
                &format!("board-{board_short}"),
            );
            let root_dir = saved_backup::create_backup_root(&backup_id)?;
            let boards = vec![crate::models::BackupBoardSummary {
                short_name: manifest.board.short_name.clone(),
                name: manifest.board.name.clone(),
            }];
            let mut files = Vec::new();

            write_board_exports_to_v4_dir(&root_dir, &manifest, &mut files)?;

            copy_runtime_tree_into_v4_dir(
                &board_upload_path,
                &root_dir,
                &mut files,
                |_path, runtime_rel| {
                    let runtime_rel = format!("{board_short}/{runtime_rel}");
                    let (logical_path, kind) =
                        saved_backup::runtime_upload_path_to_logical(&board_short, &runtime_rel)?;
                    Ok((
                        logical_path,
                        Some(runtime_rel),
                        Some(board_short.clone()),
                        kind,
                    ))
                },
                Some(&progress),
            )?;

            let manifest_v4 = saved_backup::BackupManifest {
                format: saved_backup::BACKUP_V4_FORMAT.to_owned(),
                archive_container: saved_backup::BACKUP_V4_ARCHIVE_CONTAINER.to_owned(),
                backup_id,
                created_at: Utc::now().timestamp(),
                completed_at: None,
                rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
                scope: saved_backup::BackupScope::Board,
                storage_mode: BackupStorageMode::Directory,
                included_boards: boards,
                includes: saved_backup::BackupIncludeFlags {
                    database: false,
                    settings: false,
                    uploads: true,
                    thumbnails: true,
                    tor_keys: false,
                    board_exports: true,
                    file_inventory: true,
                },
                db_snapshot: None,
                files,
                parts: Vec::new(),
                maintenance: None,
            };

            let backup_ref = finalize_v4_backup_root(&root_dir, manifest_v4)?;
            invalidate_backup_list_cache(&board_backup_dir(), BackupListKind::Board);

            let size = saved_backup::scan_dir_stats(&root_dir).bytes;
            tracing::info!(
                target: "admin",
                board = %board_short,
                backup_id = %backup_ref,
                path = %root_dir.display(),
                bytes = size,
                "Backup v4 board backup created"
            );
            progress
                .phase
                .store(crate::middleware::backup_phase::DONE, Ordering::Relaxed);
            log_backup_phase(crate::middleware::backup_phase::DONE);
            Ok(backup_ref)
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
        write_temp_board_download_token(&filename, &download_token)?;
        let body = serde_json::json!({
            "filename": filename,
            "download_url": format!(
                "/admin/backup/download/temp-board/{filename}?cleanup=1&token={download_token}"
            ),
            "board": board_short_for_flash,
        });
        return Ok((
            [(header::CONTENT_TYPE, "application/json".to_owned())],
            body.to_string(),
        )
            .into_response());
    }

    if form.download_after_create.as_deref() == Some("1") {
        let download_token = new_session_id();
        write_temp_board_download_token(&filename, &download_token)?;
        return Ok(Redirect::to(&format!(
            "/admin/backup/download/temp-board/{filename}?cleanup=1&token={download_token}"
        ))
        .into_response());
    }

    Ok(admin_panel_redirect_anchor_open(
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
) -> Result<common::FullBackupManifest> {
    let boards = collect_all_rows(
        conn,
        "SELECT short_name, name FROM boards ORDER BY short_name ASC",
        |row| {
            let short_name: String = row.get(0)?;
            let name: String = row.get(1)?;
            Ok(crate::models::BackupBoardSummary { short_name, name })
        },
    )?;
    Ok(common::FullBackupManifest {
        version: 3,
        generated_at: Utc::now().timestamp(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        db_bytes,
        upload_file_count,
        favicon_file_count,
        banner_file_count,
        tor_hidden_service_keys_included,
        tor_hidden_service_key_file_count,
        boards,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "the function maps the complete relational board snapshot into one versioned manifest"
)]
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
                     allow_images, allow_video, allow_audio, allow_pdf, allow_any_files, allow_tripcodes,
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
                    allow_pdf: row.get::<_, i64>(11)? != 0,
                    allow_any_files: row.get::<_, i64>(12)? != 0,
                    allow_tripcodes: row.get::<_, i64>(13)? != 0,
                    edit_window_secs: row.get(14)?,
                    allow_editing: row.get::<_, i64>(15)? != 0,
                    allow_self_delete: row.get::<_, i64>(16)? != 0,
                    allow_archive: row.get::<_, i64>(17)? != 0,
                    allow_video_embeds: row.get::<_, i64>(18)? != 0,
                    allow_captcha: row.get::<_, i64>(19)? != 0,
                    show_poster_ids: row.get::<_, i64>(20)? != 0,
                    collapse_greentext: row.get::<_, i64>(21)? != 0,
                    post_cooldown_secs: row.get(22)?,
                    banner_mode: row.get(23)?,
                    access_mode: row.get(24)?,
                    access_password_hash: row.get(25)?,
                    created_at: row.get(26)?,
                })
            },
        )
        .map_err(|_error| AppError::NotFound(format!("Board '{board_short}' not found")))?;
    validate_board_short_name(&board.short_name)?;

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

/// Writes board backup archive from dir.
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
                add_dir_to_zip(zip, uploads_base, board_upload_path, opts, progress)?;
            } else {
                let noop_progress = crate::middleware::BackupProgress::new();
                add_dir_to_zip(zip, uploads_base, board_upload_path, opts, &noop_progress)?;
            }
        }
        Ok(())
    })
}

/// Writes board backup archive.
pub(super) fn write_board_backup_archive<F>(
    output_path: &Path,
    manifest_json: &[u8],
    progress: Option<&crate::middleware::BackupProgress>,
    mut write_uploads: F,
) -> Result<()>
where
    F: FnMut(&mut zip::ZipWriter<std::io::BufWriter<std::fs::File>>) -> Result<()>,
{
    if let Some(parent) = output_path.parent() {
        ensure_backup_dir(parent)?;
    }
    let file = std::fs::File::create(output_path)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Create zip tmp: {error}")))?;
    restrict_backup_file(output_path)?;
    let out_file = std::io::BufWriter::new(file);
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

/// Collects rows.
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

/// Collects all rows.
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

/// Collects backup board summaries.
fn collect_backup_board_summaries(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::BackupBoardSummary>> {
    let boards = collect_all_rows(
        conn,
        "SELECT short_name, name FROM boards ORDER BY short_name ASC",
        |row| {
            let short_name: String = row.get(0)?;
            let name: String = row.get(1)?;
            Ok(crate::models::BackupBoardSummary { short_name, name })
        },
    )?;
    for board in &boards {
        validate_board_short_name(&board.short_name)?;
    }
    Ok(boards)
}

fn push_v4_file_entry(
    entries: &mut Vec<saved_backup::BackupFileEntry>,
    logical_path: String,
    runtime_logical_path: Option<String>,
    board: Option<String>,
    kind: saved_backup::BackupFileKind,
    size: u64,
    sha256: String,
) {
    entries.push(saved_backup::BackupFileEntry {
        logical_path,
        runtime_logical_path,
        board,
        kind,
        size,
        sha256,
        zip_part: None,
        zip_entry_path: None,
        compression_method: None,
    });
}

#[derive(Debug)]
struct SplitZipPlannedPart {
    files: Vec<usize>,
    bytes: u64,
    oversized: bool,
}

fn plan_split_zip_parts(
    files: &[saved_backup::BackupFileEntry],
    target_part_size: u64,
) -> Vec<SplitZipPlannedPart> {
    let mut ordered = files
        .iter()
        .enumerate()
        .collect::<Vec<(usize, &saved_backup::BackupFileEntry)>>();
    ordered.sort_by(|left, right| left.1.logical_path.cmp(&right.1.logical_path));

    let mut parts = Vec::new();
    let mut current = SplitZipPlannedPart {
        files: Vec::new(),
        bytes: 0,
        oversized: false,
    };
    for (index, entry) in ordered {
        if current.files.is_empty() && entry.size > target_part_size {
            parts.push(SplitZipPlannedPart {
                files: vec![index],
                bytes: entry.size,
                oversized: true,
            });
            continue;
        }
        if !current.files.is_empty() && current.bytes.saturating_add(entry.size) > target_part_size
        {
            parts.push(current);
            current = SplitZipPlannedPart {
                files: Vec::new(),
                bytes: 0,
                oversized: false,
            };
        }
        current.bytes = current.bytes.saturating_add(entry.size);
        current.files.push(index);
    }
    if !current.files.is_empty() {
        parts.push(current);
    }
    parts
}

#[expect(
    clippy::too_many_lines,
    reason = "part sizing, hashing, manifest updates, and partial-output cleanup form one publication step"
)]
fn materialize_split_zip_parts(
    root_dir: &Path,
    manifest: &mut saved_backup::BackupManifest,
    target_part_size: u64,
) -> Result<()> {
    let parts_dir = root_dir.join(saved_backup::PARTS_DIR_NAME);
    ensure_backup_dir(&parts_dir)?;
    let planned_parts = plan_split_zip_parts(&manifest.files, target_part_size);
    let total_parts = u32::try_from(planned_parts.len()).unwrap_or(u32::MAX);
    let mut part_infos = Vec::with_capacity(planned_parts.len());

    for (part_offset, planned) in planned_parts.iter().enumerate() {
        let part_index = u32::try_from(part_offset + 1).unwrap_or(u32::MAX);
        let part_filename = format!("parts/part-{part_index:04}.zip");
        let part_path = root_dir.join(&part_filename);
        let tmp_path = root_dir.join(format!("{part_filename}.tmp"));
        if let Some(parent) = tmp_path.parent() {
            ensure_backup_dir(parent)?;
        }

        let write_result = (|| -> Result<()> {
            let output = std::fs::File::create(&tmp_path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Create {}: {error}", tmp_path.display()))
            })?;
            restrict_backup_file(&tmp_path)?;
            let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(output));
            for file_index in &planned.files {
                let entry = manifest.files.get(*file_index).ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("Invalid split ZIP planner index"))
                })?;
                let source_path = root_dir.join(&entry.logical_path);
                zip.start_file(
                    &entry.logical_path,
                    zip_file_options_for_path(Path::new(&entry.logical_path)),
                )
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!(
                        "Start split ZIP entry '{}': {error}",
                        entry.logical_path
                    ))
                })?;
                let mut source = std::fs::File::open(&source_path).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Open {}: {error}", source_path.display()))
                })?;
                std::io::copy(&mut source, &mut zip).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!(
                        "Copy {} into split ZIP: {error}",
                        source_path.display()
                    ))
                })?;
            }
            let writer = zip.finish().map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Finalize {}: {error}", tmp_path.display()))
            })?;
            writer
                .into_inner()
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Flush {}: {error}", tmp_path.display()))
                })?
                .sync_all()
                .map_err(|error| {
                    AppError::Internal(anyhow::anyhow!("Sync {}: {error}", tmp_path.display()))
                })?;
            Ok(())
        })();
        if let Err(error) = write_result {
            drop(std::fs::remove_file(&tmp_path));
            return Err(error);
        }
        std::fs::rename(&tmp_path, &part_path).map_err(|error| {
            drop(std::fs::remove_file(&tmp_path));
            AppError::Internal(anyhow::anyhow!(
                "Rename split ZIP part {}: {error}",
                part_path.display()
            ))
        })?;
        restrict_backup_file(&part_path)?;

        let part_size = std::fs::metadata(&part_path)
            .map(|metadata| metadata.len())
            .map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Inspect {}: {error}", part_path.display()))
            })?;
        let part_sha = saved_backup::sha256_hex_for_file(&part_path)?;
        for file_index in &planned.files {
            let entry = manifest.files.get_mut(*file_index).ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("Invalid split ZIP planner index"))
            })?;
            entry.zip_part = Some(part_filename.clone());
            entry.zip_entry_path = Some(entry.logical_path.clone());
            entry.compression_method = Some("zip".to_owned());
        }
        part_infos.push(saved_backup::BackupPartInfo {
            filename: part_filename,
            part_index,
            total_parts,
            backup_id: manifest.backup_id.clone(),
            size: part_size,
            sha256: part_sha,
            target_part_size,
            oversized: planned.oversized,
        });
    }

    for entry in &manifest.files {
        if entry.zip_part.is_some() {
            let path = root_dir.join(&entry.logical_path);
            if path.is_file() {
                std::fs::remove_file(&path).map_err(|error| {
                    AppError::Internal(anyhow::anyhow!(
                        "Remove root payload {} after split ZIP packaging: {error}",
                        path.display()
                    ))
                })?;
            }
        }
    }

    manifest.parts = part_infos;
    Ok(())
}

/// Copies runtime tree into v4 dir.
fn copy_runtime_tree_into_v4_dir<F>(
    source_root: &Path,
    destination_root: &Path,
    entries: &mut Vec<saved_backup::BackupFileEntry>,
    mut map_entry: F,
    progress: Option<&crate::middleware::BackupProgress>,
) -> Result<()>
where
    F: FnMut(
        &Path,
        &str,
    ) -> Result<(
        String,
        Option<String>,
        Option<String>,
        saved_backup::BackupFileKind,
    )>,
{
    fn visit<F>(
        current: &Path,
        source_root: &Path,
        destination_root: &Path,
        entries: &mut Vec<saved_backup::BackupFileEntry>,
        map_entry: &mut F,
        progress: Option<&crate::middleware::BackupProgress>,
    ) -> Result<()>
    where
        F: FnMut(
            &Path,
            &str,
        ) -> Result<(
            String,
            Option<String>,
            Option<String>,
            saved_backup::BackupFileKind,
        )>,
    {
        let dir_entries = std::fs::read_dir(current).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read {}: {error}", current.display()))
        })?;
        for entry in dir_entries {
            let entry = entry.map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Read dir entry {}: {error}",
                    current.display()
                ))
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Inspect {}: {error}", path.display()))
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_dir() {
                visit(
                    &path,
                    source_root,
                    destination_root,
                    entries,
                    map_entry,
                    progress,
                )?;
                continue;
            }
            if !metadata.file_type().is_file()
                || crate::utils::fs_security::assert_regular_file_no_symlink(&path).is_err()
            {
                continue;
            }
            let runtime_rel = relative_path_string(&path, source_root)?;
            let (logical_path, runtime_logical_path, board, kind) = map_entry(&path, &runtime_rel)?;
            saved_backup::sanitize_logical_path(&logical_path)?;
            let destination = destination_root.join(&logical_path);
            let (size, sha256) = copy_regular_file_to_backup(&path, &destination)?;
            push_v4_file_entry(
                entries,
                logical_path,
                runtime_logical_path,
                board,
                kind,
                size,
                sha256,
            );
            if let Some(progress) = progress {
                progress.files_done.fetch_add(1, Ordering::Relaxed);
                progress.bytes_done.fetch_add(size, Ordering::Relaxed);
                log_backup_progress(progress);
            }
        }
        Ok(())
    }

    if !source_root.exists() {
        return Ok(());
    }
    visit(
        source_root,
        source_root,
        destination_root,
        entries,
        &mut map_entry,
        progress,
    )
}

/// Writes board exports to v4 dir.
fn write_board_exports_to_v4_dir(
    destination_root: &Path,
    manifest: &board_backup_types::BoardBackupManifest,
    entries: &mut Vec<saved_backup::BackupFileEntry>,
) -> Result<()> {
    validate_board_short_name(&manifest.board.short_name)?;
    let board_root = destination_root
        .join("boards")
        .join(&manifest.board.short_name);
    ensure_backup_dir(&board_root)?;

    let board_json_path = board_root.join("board.json");
    let (board_json_size, board_json_sha) = write_pretty_json_file(&board_json_path, manifest)?;
    push_v4_file_entry(
        entries,
        format!("boards/{}/board.json", manifest.board.short_name),
        None,
        Some(manifest.board.short_name.clone()),
        saved_backup::BackupFileKind::BoardJson,
        board_json_size,
        board_json_sha,
    );

    let threads_path = board_root.join("threads.jsonl");
    let (threads_size, threads_sha) = write_jsonl_file(&threads_path, &manifest.threads)?;
    push_v4_file_entry(
        entries,
        format!("boards/{}/threads.jsonl", manifest.board.short_name),
        None,
        Some(manifest.board.short_name.clone()),
        saved_backup::BackupFileKind::ThreadExport,
        threads_size,
        threads_sha,
    );

    let posts_path = board_root.join("posts.jsonl");
    let (posts_size, posts_sha) = write_jsonl_file(&posts_path, &manifest.posts)?;
    push_v4_file_entry(
        entries,
        format!("boards/{}/posts.jsonl", manifest.board.short_name),
        None,
        Some(manifest.board.short_name.clone()),
        saved_backup::BackupFileKind::PostExport,
        posts_size,
        posts_sha,
    );

    let files_path = board_root.join("files.jsonl");
    let (files_size, files_sha) = write_jsonl_file(&files_path, &manifest.file_hashes)?;
    push_v4_file_entry(
        entries,
        format!("boards/{}/files.jsonl", manifest.board.short_name),
        None,
        Some(manifest.board.short_name.clone()),
        saved_backup::BackupFileKind::FileInventoryExport,
        files_size,
        files_sha,
    );
    Ok(())
}

fn finalize_v4_backup_root(
    root_dir: &Path,
    mut manifest: saved_backup::BackupManifest,
) -> Result<String> {
    manifest.completed_at = Some(Utc::now().timestamp());

    let mut metadata = saved_backup::BackupMetadata {
        format: saved_backup::BACKUP_V4_FORMAT.to_owned(),
        backup_id: manifest.backup_id.clone(),
        scope: manifest.scope,
        storage_mode: manifest.storage_mode,
        created_at: manifest.created_at,
        completed_at: manifest.completed_at,
        total_size_bytes: 0,
        verified: true,
        part_count: u32::try_from(manifest.parts.len()).unwrap_or(u32::MAX),
        includes_tor_keys: manifest.includes.tor_keys,
        included_boards: manifest.included_boards.clone(),
        manifest_path: Some(
            root_dir
                .join(saved_backup::MANIFEST_FILE_NAME)
                .display()
                .to_string(),
        ),
    };

    let manifest_path = root_dir.join(saved_backup::MANIFEST_FILE_NAME);
    let metadata_path = root_dir.join(saved_backup::BACKUP_METADATA_FILE_NAME);
    let readme_path = root_dir.join(saved_backup::README_FILE_NAME);

    saved_backup::write_json_pretty(&manifest_path, &manifest)?;
    saved_backup::write_json_pretty(&metadata_path, &metadata)?;
    let readme = saved_backup::build_readme(&manifest, &metadata, manifest.includes.tor_keys);
    saved_backup::write_text(&readme_path, &readme)?;

    let part_paths = manifest
        .parts
        .iter()
        .map(|part| root_dir.join(&part.filename))
        .collect::<Vec<_>>();
    let part_path_refs = part_paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    saved_backup::write_root_checksums(root_dir, &part_path_refs)?;
    metadata.total_size_bytes = saved_backup::scan_dir_stats(root_dir).bytes;
    saved_backup::write_json_pretty(&metadata_path, &metadata)?;
    saved_backup::write_root_checksums(root_dir, &part_path_refs)?;
    Ok(manifest.backup_id)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::copy_regular_file_to_backup;
    use super::{build_full_backup_manifest, count_required_private_files, FullBackupCreateForm};
    use crate::handlers::admin::backup::common::{
        resolve_tor_hidden_service_keys_availability, verify_full_backup_zip,
        TorHiddenServiceKeysAvailability, FULL_BACKUP_MANIFEST_NAME,
        FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX,
    };
    use crate::handlers::admin::backup::saved_backup;
    use anyhow::{bail, ensure, Context as _, Result as TestResult};
    use axum::{
        body::{to_bytes, Body},
        extract::Form,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use std::io::Write as _;
    use tower::ServiceExt as _;

    const fn test_full_backup_upload_file_count(
        total_file_count: u64,
        tor_hidden_service_key_file_count: u64,
    ) -> u64 {
        total_file_count.saturating_sub(tor_hidden_service_key_file_count)
    }

    async fn echo_full_backup_create_form(Form(form): Form<FullBackupCreateForm>) -> String {
        form.include_tor_hidden_service_keys.to_string()
    }

    #[tokio::test]
    async fn full_backup_create_form_accepts_checked_browser_checkbox_value() -> TestResult<()> {
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
                    .context("build checked-checkbox request")?,
            )
            .await
            .context("send checked-checkbox request")?;

        ensure!(response.status() == StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read checked-checkbox response body")?;
        ensure!(&body[..] == b"true");
        Ok(())
    }

    #[tokio::test]
    async fn full_backup_create_form_defaults_missing_checkbox_to_false() -> TestResult<()> {
        let app = Router::new().route("/parse", post(echo_full_backup_create_form));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/parse")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("_csrf=test"))
                    .context("build unchecked-checkbox request")?,
            )
            .await
            .context("send unchecked-checkbox request")?;

        ensure!(response.status() == StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read unchecked-checkbox response body")?;
        ensure!(&body[..] == b"false");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn backup_secret_file_copy_uses_private_mode() -> TestResult<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let source = temp_dir.path().join("settings.toml");
        let destination = temp_dir.path().join("backup/config/settings.toml");
        std::fs::write(&source, "cookie_secret = \"secret\"")
            .context("write source settings file")?;

        copy_regular_file_to_backup(&source, &destination)?;

        let destination_mode = std::fs::metadata(&destination)
            .context("read destination metadata")?
            .permissions()
            .mode()
            & 0o777;
        ensure!(destination_mode == 0o600);
        let destination_parent = destination
            .parent()
            .context("backup destination has no parent")?;
        let parent_mode = std::fs::metadata(destination_parent)
            .context("read destination parent metadata")?
            .permissions()
            .mode()
            & 0o777;
        ensure!(parent_mode == 0o700);
        Ok(())
    }

    fn write_test_full_backup_zip(
        zip_path: &std::path::Path,
        include_tor_keys: bool,
    ) -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create backup fixture directory")?;
        let uploads_dir = temp_dir.path().join("uploads");
        let tor_keys_dir = temp_dir.path().join("tor-keys");
        std::fs::create_dir_all(uploads_dir.join("tech")).context("create uploads directory")?;
        std::fs::write(uploads_dir.join("tech/post.txt"), "post")
            .context("write upload fixture")?;
        std::fs::create_dir_all(&tor_keys_dir).context("create Tor key directory")?;
        std::fs::write(tor_keys_dir.join("hs_ed25519_secret_key"), "secret")
            .context("write secret key")?;
        std::fs::write(tor_keys_dir.join("hs_ed25519_public_key"), "public")
            .context("write public key")?;

        let pool = crate::db::init_test_pool().context("create test pool")?;
        let conn = pool.get().context("get test database connection")?;
        crate::db::create_board(&conn, "tech", "Technology", "", false)
            .context("create test board")?;

        let db_path = temp_dir.path().join("snapshot.db");
        let db_path_str = db_path
            .to_str()
            .context("database fixture path is not UTF-8")?
            .replace('\'', "''");
        conn.execute_batch(&format!("VACUUM INTO '{db_path_str}'"))
            .context("create database snapshot")?;

        let tor_key_file_count = if include_tor_keys { 2 } else { 0 };
        let total_archive_file_count = 1_u64.saturating_add(tor_key_file_count);
        let upload_file_count =
            test_full_backup_upload_file_count(total_archive_file_count, tor_key_file_count);
        let manifest = build_full_backup_manifest(
            &conn,
            std::fs::metadata(&db_path)
                .context("read database snapshot metadata")?
                .len(),
            upload_file_count,
            0,
            0,
            include_tor_keys,
            tor_key_file_count,
        )
        .context("build full-backup manifest")?;
        let manifest_json = serde_json::to_vec(&manifest).context("serialize backup manifest")?;
        let db_bytes = std::fs::read(&db_path).context("read database snapshot")?;

        let file = std::fs::File::create(zip_path).context("create backup ZIP")?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(FULL_BACKUP_MANIFEST_NAME, options)
            .context("start manifest ZIP entry")?;
        zip.write_all(&manifest_json)
            .context("write manifest ZIP entry")?;
        zip.start_file("chan.db", options)
            .context("start database ZIP entry")?;
        zip.write_all(&db_bytes)
            .context("write database ZIP entry")?;
        super::super::add_dir_to_zip_with_prefix(
            &mut zip,
            &uploads_dir,
            &uploads_dir,
            "uploads",
            options,
            &crate::middleware::BackupProgress::new(),
        )
        .context("add uploads to backup ZIP")?;
        if include_tor_keys {
            super::super::add_dir_to_zip_with_prefix(
                &mut zip,
                &tor_keys_dir,
                &tor_keys_dir,
                super::super::common::FULL_BACKUP_TOR_KEYS_PREFIX,
                options,
                &crate::middleware::BackupProgress::new(),
            )
            .context("add Tor keys to backup ZIP")?;
        }
        zip.finish().context("finish backup ZIP")?;
        Ok(())
    }

    #[test]
    fn full_backup_manifest_defaults_to_no_tor_keys_when_not_requested() -> TestResult<()> {
        let pool = crate::db::init_test_pool().context("create test pool")?;
        let conn = pool.get().context("get test database connection")?;
        crate::db::create_board(&conn, "tech", "Technology", "", false)
            .context("create test board")?;

        let manifest = build_full_backup_manifest(&conn, 1024, 5, 1, 2, false, 0)?;

        ensure!(!manifest.tor_hidden_service_keys_included);
        ensure!(manifest.tor_hidden_service_key_file_count == 0);
        Ok(())
    }

    #[test]
    fn full_backup_archive_can_record_and_package_tor_key_material() -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("full-with-tor.zip");
        write_test_full_backup_zip(&zip_path, true)?;

        let manifest = verify_full_backup_zip(&zip_path)?;
        ensure!(manifest.tor_hidden_service_keys_included);
        ensure!(manifest.upload_file_count == 1);
        ensure!(manifest.tor_hidden_service_key_file_count == 2);

        let file = std::fs::File::open(&zip_path).context("open backup ZIP")?;
        let mut archive = zip::ZipArchive::new(file).context("parse backup ZIP")?;
        ensure!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_secret_key"
            ))
            .is_ok());
        ensure!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_public_key"
            ))
            .is_ok());
        Ok(())
    }

    #[test]
    fn full_backup_archive_omits_tor_key_material_when_not_requested() -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let zip_path = temp_dir.path().join("full-without-tor.zip");
        write_test_full_backup_zip(&zip_path, false)?;

        let manifest = verify_full_backup_zip(&zip_path)?;
        ensure!(!manifest.tor_hidden_service_keys_included);
        ensure!(manifest.upload_file_count == 1);
        ensure!(manifest.tor_hidden_service_key_file_count == 0);

        let file = std::fs::File::open(&zip_path).context("open backup ZIP")?;
        let mut archive = zip::ZipArchive::new(file).context("parse backup ZIP")?;
        ensure!(archive
            .by_name(&format!(
                "{FULL_BACKUP_TOR_KEYS_ENTRY_PREFIX}hs_ed25519_secret_key"
            ))
            .is_err());
        Ok(())
    }

    #[test]
    fn split_zip_part_planner_does_not_create_empty_parts() {
        let files = vec![
            saved_backup::BackupFileEntry {
                logical_path: "b.txt".to_owned(),
                runtime_logical_path: None,
                board: None,
                kind: saved_backup::BackupFileKind::Settings,
                size: 6,
                sha256: "b".to_owned(),
                zip_part: None,
                zip_entry_path: None,
                compression_method: None,
            },
            saved_backup::BackupFileEntry {
                logical_path: "a.txt".to_owned(),
                runtime_logical_path: None,
                board: None,
                kind: saved_backup::BackupFileKind::Settings,
                size: 6,
                sha256: "a".to_owned(),
                zip_part: None,
                zip_entry_path: None,
                compression_method: None,
            },
        ];

        let parts = super::plan_split_zip_parts(&files, 6);

        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|part| !part.files.is_empty()));
    }

    #[test]
    fn split_zip_part_planner_marks_oversized_single_file_part() -> TestResult<()> {
        let files = vec![saved_backup::BackupFileEntry {
            logical_path: "huge.bin".to_owned(),
            runtime_logical_path: None,
            board: None,
            kind: saved_backup::BackupFileKind::OriginalMedia,
            size: 128,
            sha256: "huge".to_owned(),
            zip_part: None,
            zip_entry_path: None,
            compression_method: None,
        }];

        let parts = super::plan_split_zip_parts(&files, 64);

        ensure!(parts.len() == 1);
        let part = parts.first().context("planner returned no part")?;
        ensure!(part.oversized);
        ensure!(part.files.len() == 1);
        Ok(())
    }

    #[test]
    fn full_backup_board_summary_collection_rejects_unsafe_db_short_name() -> TestResult<()> {
        let pool = crate::db::init_test_pool().context("create test pool")?;
        let conn = pool.get().context("get test database connection")?;
        crate::db::create_board(&conn, "tech", "Technology", "", false)
            .context("create test board")?;
        conn.execute(
            "UPDATE boards SET short_name = '../escape' WHERE short_name = 'tech'",
            [],
        )
        .context("corrupt stored board short name")?;

        let error = super::collect_backup_board_summaries(&conn)
            .err()
            .context("unsafe stored board short name was unexpectedly accepted")?;

        ensure!(error.to_string().contains("Invalid board short name"));
        Ok(())
    }

    #[test]
    fn board_export_writer_rejects_unsafe_manifest_short_name_before_path_join() -> TestResult<()> {
        let manifest =
            crate::handlers::admin::backup::types::board_backup_types::BoardBackupManifest {
                version: 1,
                board: crate::handlers::admin::backup::types::board_backup_types::BoardRow {
                    id: 1,
                    short_name: "a/b".to_owned(),
                    name: "Bad".to_owned(),
                    description: String::new(),
                    nsfw: false,
                    max_threads: 100,
                    max_archived_threads: 150,
                    bump_limit: 300,
                    allow_images: true,
                    allow_video: true,
                    allow_audio: false,
                    allow_pdf: false,
                    allow_any_files: false,
                    allow_tripcodes: true,
                    edit_window_secs: 300,
                    allow_editing: false,
                    allow_self_delete: false,
                    allow_archive: true,
                    allow_video_embeds: false,
                    allow_captcha: false,
                    show_poster_ids: false,
                    collapse_greentext: false,
                    post_cooldown_secs: 0,
                    banner_mode: "inherit".to_owned(),
                    access_mode: "public".to_owned(),
                    access_password_hash: String::new(),
                    created_at: 1,
                },
                threads: Vec::new(),
                posts: Vec::new(),
                polls: Vec::new(),
                poll_options: Vec::new(),
                poll_votes: Vec::new(),
                file_hashes: Vec::new(),
                banners: Vec::new(),
            };
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let mut entries = Vec::new();

        let error = super::write_board_exports_to_v4_dir(temp_dir.path(), &manifest, &mut entries)
            .err()
            .context("unsafe manifest board short name was unexpectedly accepted")?;

        ensure!(error.to_string().contains("Invalid board short name"));
        ensure!(!temp_dir.path().join("boards").exists());
        Ok(())
    }

    #[test]
    fn requested_tor_key_backup_fails_clearly_when_identity_dir_is_missing() -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let missing = temp_dir.path().join("missing-keys");
        let error = count_required_private_files(
            &missing,
            "Tor hidden service keys were requested, but the configured identity directory could not be read.",
        )
        .err()
        .context("missing Tor key directory was unexpectedly accepted")?;

        match error {
            crate::error::AppError::BadRequest(message) => {
                ensure!(message.contains("Tor hidden service keys were requested"));
            }
            other => bail!("expected BadRequest, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn requested_tor_key_backup_skips_cleanly_when_not_requested() -> TestResult<()> {
        let result = resolve_tor_hidden_service_keys_availability(
            false,
            None,
            "Tor hidden service key backups are not available with the current configuration.",
        )?;

        ensure!(result == TorHiddenServiceKeysAvailability::Skipped);
        Ok(())
    }

    #[test]
    fn requested_tor_key_backup_is_rejected_when_tor_is_disabled_or_unconfigured() -> TestResult<()>
    {
        let error = resolve_tor_hidden_service_keys_availability(
            true,
            None,
            "Tor hidden service key backups are not available with the current configuration.",
        )
        .err()
        .context("requested Tor keys were unexpectedly accepted without configuration")?;

        match error {
            crate::error::AppError::BadRequest(message) => {
                ensure!(message.contains("Tor hidden service key backups are not available"));
            }
            other => bail!("expected BadRequest, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn requested_tor_key_backup_is_rejected_when_configured_path_is_missing() -> TestResult<()> {
        let temp_dir = tempfile::tempdir().context("create temporary directory")?;
        let missing = temp_dir.path().join("missing-keys");
        let error = resolve_tor_hidden_service_keys_availability(
            true,
            Some(missing.clone()),
            "Tor hidden service key backups are not available with the current configuration.",
        )
        .err()
        .context("missing configured Tor key path was unexpectedly accepted")?;

        match error {
            crate::error::AppError::BadRequest(message) => {
                ensure!(message.contains("could not be read"));
                ensure!(message.contains(&missing.display().to_string()));
            }
            other => bail!("expected BadRequest, got {other:?}"),
        }
        Ok(())
    }
}
