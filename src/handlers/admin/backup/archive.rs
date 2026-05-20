// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) struct TempZipCleanupGuard {
    path: Option<PathBuf>,
}

impl TempZipCleanupGuard {
    pub(super) const fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    pub(super) fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempZipCleanupGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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

pub(super) fn validate_full_restore_archive_layout<R: std::io::Read + std::io::Seek>(
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

pub(super) fn extract_sqlite_db_from_full_backup_archive<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    temp_db: &Path,
) -> Result<()> {
    let mut db_entry = archive.by_name("chan.db").map_err(|_error| {
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

pub(super) fn canonicalize_restored_banner_dir(root: &Path) -> Result<()> {
    let mut total_bytes = 0u64;
    canonicalize_restored_banner_dir_inner(root, root, &mut total_bytes)
}

fn canonicalize_restored_banner_dir_inner(
    root: &Path,
    current: &Path,
    total_bytes: &mut u64,
) -> Result<()> {
    for entry in std::fs::read_dir(current).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Read restored banner directory {}: {error}",
            current.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read restored banner directory entry {}: {error}",
                current.display()
            ))
        })?;
        let file_type = entry.file_type().map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Inspect restored banner entry {}: {error}",
                entry.path().display()
            ))
        })?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Resolve restored banner path {}: {error}",
                path.display()
            ))
        })?;
        if file_type.is_dir() {
            canonicalize_restored_banner_dir_inner(root, &path, total_bytes)?;
            continue;
        }
        let rel_name = rel.to_string_lossy();
        banner::validate_banner_restore_entry_name(&rel_name)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read restored banner file {}: {error}",
                path.display()
            ))
        })?;
        let (_, _, file_size) = banner::canonicalize_banner_bytes(&bytes, &path)?;
        *total_bytes = total_bytes.saturating_add(file_size);
        if *total_bytes > BANNER_RESTORE_TOTAL_MAX_BYTES {
            return Err(AppError::BadRequest(
                "Restored banner files exceed the safe total size limit.".into(),
            ));
        }
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
        let name = entry.name().to_owned();
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

fn write_v4_file_to_legacy_zip<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    zip_path: &str,
    source: &v4::VerifiedSavedV4File,
) -> Result<()> {
    zip.start_file(zip_path, zip_file_options_for_path(Path::new(zip_path)))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip {zip_path}: {error}")))?;
    v4::copy_verified_file_to_writer(source, zip)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Copy {zip_path}: {error}")))
}

fn temp_legacy_zip_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}_{}.zip", uuid::Uuid::new_v4().simple()))
}

pub(super) fn create_temp_legacy_full_backup_from_v4_path(root_dir: &Path) -> Result<PathBuf> {
    let verified = v4::verify_saved_v4_root(root_dir, &[v4::BackupScope::FullSite])?;
    create_temp_legacy_full_backup_from_verified_v4(&verified)
}

pub(super) fn create_temp_legacy_full_backup_from_v4_transfer_zip<R: std::io::Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<PathBuf> {
    let mut db_index = None;
    let mut db_bytes = 0_u64;
    let mut mapped_files = Vec::new();
    let mut upload_file_count = 0_u64;
    let mut favicon_file_count = 0_u64;
    let mut banner_file_count = 0_u64;
    let mut tor_hidden_service_key_file_count = 0_u64;

    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read Backup v4 transfer entry #{index}: {error}"
            ))
        })?;
        let name = entry.name().to_owned();
        super::common::validate_restore_safe_entry_name(&name)?;
        if entry.is_dir() {
            continue;
        }

        if name == "db/rustchan.sqlite3" {
            db_index = Some(index);
            db_bytes = entry.size();
        } else if name.starts_with("boards/") {
            if let Ok((runtime_path, kind)) = v4::logical_upload_path_to_runtime(&name) {
                mapped_files.push((index, format!("uploads/{runtime_path}")));
                match kind {
                    v4::BackupFileKind::OriginalMedia
                    | v4::BackupFileKind::Thumbnail
                    | v4::BackupFileKind::Banner
                    | v4::BackupFileKind::Favicon => {
                        upload_file_count = upload_file_count.saturating_add(1);
                    }
                    _ => {}
                }
            }
        } else if let Some(rel) = name.strip_prefix("site-assets/favicon/") {
            super::common::validate_restore_safe_entry_name(rel)?;
            mapped_files.push((index, format!("favicon/{rel}")));
            favicon_file_count = favicon_file_count.saturating_add(1);
        } else if let Some(rel) = name.strip_prefix("site-assets/banner/") {
            super::common::validate_restore_safe_entry_name(rel)?;
            mapped_files.push((index, format!("banner/{rel}")));
            banner_file_count = banner_file_count.saturating_add(1);
        } else if let Some(rel) = name.strip_prefix("tor-keys/") {
            super::common::validate_restore_safe_entry_name(rel)?;
            mapped_files.push((
                index,
                format!("{}/{}", super::common::FULL_BACKUP_TOR_KEYS_PREFIX, rel),
            ));
            tor_hidden_service_key_file_count = tor_hidden_service_key_file_count.saturating_add(1);
        }
    }

    let db_index = db_index.ok_or_else(|| {
        AppError::BadRequest(
            "Invalid Backup v4 transfer archive: missing db/rustchan.sqlite3.".into(),
        )
    })?;
    let temp_zip = temp_legacy_zip_path("rustchan_v4_transfer_full_restore");
    let output = std::fs::File::create(&temp_zip).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create {}: {error}", temp_zip.display()))
    })?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(output));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let manifest = serde_json::json!({
        "version": 3,
        "generated_at": Utc::now().timestamp(),
        "rustchan_version": env!("CARGO_PKG_VERSION"),
        "db_bytes": db_bytes,
        "upload_file_count": upload_file_count,
        "favicon_file_count": favicon_file_count,
        "banner_file_count": banner_file_count,
        "tor_hidden_service_keys_included": tor_hidden_service_key_file_count > 0,
        "tor_hidden_service_key_file_count": tor_hidden_service_key_file_count,
        "boards": [],
    });
    zip.start_file(super::common::FULL_BACKUP_MANIFEST_NAME, options)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip backup.json: {error}")))?;
    zip.write_all(
        &serde_json::to_vec_pretty(&manifest).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Serialize backup.json: {error}"))
        })?,
    )
    .map_err(|error| AppError::Internal(anyhow::anyhow!("Write backup.json: {error}")))?;

    zip.start_file("chan.db", options)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip chan.db: {error}")))?;
    {
        let mut entry = archive.by_index(db_index).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read Backup v4 DB entry: {error}"))
        })?;
        super::common::copy_limited(&mut entry, &mut zip, super::common::ZIP_ENTRY_MAX_BYTES)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Copy chan.db: {error}")))?;
    }

    for (index, zip_path) in mapped_files {
        zip.start_file(&zip_path, zip_file_options_for_path(Path::new(&zip_path)))
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip {zip_path}: {error}")))?;
        let mut entry = archive.by_index(index).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read Backup v4 transfer entry #{index}: {error}"
            ))
        })?;
        super::common::copy_limited(&mut entry, &mut zip, super::common::ZIP_ENTRY_MAX_BYTES)
            .map_err(|error| AppError::Internal(anyhow::anyhow!("Copy {zip_path}: {error}")))?;
    }

    zip.finish().map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Finalize {}: {error}", temp_zip.display()))
    })?;
    Ok(temp_zip)
}

fn create_temp_legacy_full_backup_from_verified_v4(
    verified: &v4::VerifiedSavedV4Root,
) -> Result<PathBuf> {
    debug_assert_eq!(
        verified.metadata.backup_id, verified.manifest.backup_id,
        "verified metadata backup_id must match manifest backup_id"
    );
    debug_assert_eq!(
        Some(verified.completed_at),
        verified.manifest.completed_at,
        "verified completion time must match manifest completion time"
    );

    let manifest = &verified.manifest;
    let temp_zip = temp_legacy_zip_path("rustchan_v4_full_restore");
    let output = std::fs::File::create(&temp_zip).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create {}: {error}", temp_zip.display()))
    })?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(output));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let upload_file_count = manifest
        .files
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                v4::BackupFileKind::OriginalMedia
                    | v4::BackupFileKind::Thumbnail
                    | v4::BackupFileKind::Banner
                    | v4::BackupFileKind::Favicon
            ) && entry.board.is_some()
        })
        .count() as u64;
    let favicon_file_count = manifest
        .files
        .iter()
        .filter(|entry| entry.kind == v4::BackupFileKind::Favicon && entry.board.is_none())
        .count() as u64;
    let banner_file_count = manifest
        .files
        .iter()
        .filter(|entry| entry.kind == v4::BackupFileKind::Banner && entry.board.is_none())
        .count() as u64;
    let tor_hidden_service_key_file_count = manifest
        .files
        .iter()
        .filter(|entry| entry.kind == v4::BackupFileKind::TorKey)
        .count() as u64;
    let db_bytes = manifest
        .db_snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.size);

    let legacy_manifest = serde_json::json!({
        "version": 3,
        "generated_at": manifest.created_at,
        "rustchan_version": manifest.rustchan_version,
        "db_bytes": db_bytes,
        "upload_file_count": upload_file_count,
        "favicon_file_count": favicon_file_count,
        "banner_file_count": banner_file_count,
        "tor_hidden_service_keys_included": manifest.includes.tor_keys,
        "tor_hidden_service_key_file_count": tor_hidden_service_key_file_count,
        "boards": manifest.included_boards,
    });
    zip.start_file(super::common::FULL_BACKUP_MANIFEST_NAME, options)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip backup.json: {error}")))?;
    zip.write_all(
        &serde_json::to_vec_pretty(&legacy_manifest).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Serialize backup.json: {error}"))
        })?,
    )
    .map_err(|error| AppError::Internal(anyhow::anyhow!("Write backup.json: {error}")))?;

    let db_entry = manifest.db_snapshot.as_ref().ok_or_else(|| {
        AppError::BadRequest("Backup v4 full restore is missing a DB snapshot.".into())
    })?;
    let verified_db = verified.db_snapshot.as_ref().ok_or_else(|| {
        AppError::BadRequest("Backup v4 full restore is missing a verified DB snapshot.".into())
    })?;
    if verified_db.file.logical_path != db_entry.path {
        return Err(AppError::BadRequest(
            "Backup v4 full restore DB snapshot metadata is inconsistent.".into(),
        ));
    }
    write_v4_file_to_legacy_zip(&mut zip, "chan.db", &verified_db.file)?;

    let mut boards: Vec<_> = verified.boards.iter().collect();
    boards.sort_by_key(|(board_short, _)| *board_short);
    for (_, board) in boards {
        for entry in &board.upload_files {
            match entry.kind {
                v4::BackupFileKind::OriginalMedia
                | v4::BackupFileKind::Thumbnail
                | v4::BackupFileKind::Banner
                | v4::BackupFileKind::Favicon => {
                    let (runtime_path, _) =
                        v4::logical_upload_path_to_runtime(&entry.logical_path)?;
                    write_v4_file_to_legacy_zip(
                        &mut zip,
                        &format!("uploads/{runtime_path}"),
                        entry,
                    )?;
                }
                _ => {}
            }
        }
    }

    for entry in &verified.site_favicon_files {
        let rel = entry
            .logical_path
            .strip_prefix("site-assets/favicon/")
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 favicon path {} is invalid.",
                    entry.logical_path
                ))
            })?;
        write_v4_file_to_legacy_zip(&mut zip, &format!("favicon/{rel}"), entry)?;
    }

    for entry in &verified.site_banner_files {
        let rel = entry
            .logical_path
            .strip_prefix("site-assets/banner/")
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 banner path {} is invalid.",
                    entry.logical_path
                ))
            })?;
        write_v4_file_to_legacy_zip(&mut zip, &format!("banner/{rel}"), entry)?;
    }

    for entry in &verified.tor_key_files {
        let rel = entry
            .logical_path
            .strip_prefix("tor-keys/")
            .ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 Tor key path {} is invalid.",
                    entry.logical_path
                ))
            })?;
        write_v4_file_to_legacy_zip(
            &mut zip,
            &format!("{}/{}", super::common::FULL_BACKUP_TOR_KEYS_PREFIX, rel),
            entry,
        )?;
    }

    zip.finish().map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Finalize {}: {error}", temp_zip.display()))
    })?;
    Ok(temp_zip)
}

pub(super) fn create_temp_legacy_board_backup_from_v4_path(
    root_dir: &Path,
    board_short: Option<&str>,
) -> Result<(PathBuf, String)> {
    let verified = v4::verify_saved_v4_root(
        root_dir,
        &[v4::BackupScope::Board, v4::BackupScope::SelectedBoards],
    )?;
    create_temp_legacy_board_backup_from_verified_v4(&verified, board_short)
}

fn create_temp_legacy_board_backup_from_verified_v4(
    verified: &v4::VerifiedSavedV4Root,
    board_short: Option<&str>,
) -> Result<(PathBuf, String)> {
    let board_short = match board_short {
        Some(board_short) => board_short.to_owned(),
        None => verified
            .manifest
            .included_boards
            .first()
            .map(|board| board.short_name.clone())
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Saved Backup v4 board backup does not describe an included board.".into(),
                )
            })?,
    };
    let board_layout = verified.boards.get(&board_short).ok_or_else(|| {
        AppError::NotFound(format!("Board /{board_short}/ not found in this backup."))
    })?;
    let board_json = v4::read_verified_file(&board_layout.board_json)?;

    let temp_zip = temp_legacy_zip_path("rustchan_v4_board_restore");
    let filename = format!(
        "rustchan-board-{board_short}-{}.zip",
        uuid::Uuid::new_v4().simple()
    );
    let output = std::fs::File::create(&temp_zip).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create {}: {error}", temp_zip.display()))
    })?;
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(output));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("board.json", options)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Zip board.json: {error}")))?;
    zip.write_all(&board_json)
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Write board.json: {error}")))?;

    for entry in &board_layout.upload_files {
        let (runtime_path, _) = v4::logical_upload_path_to_runtime(&entry.logical_path)?;
        write_v4_file_to_legacy_zip(&mut zip, &format!("uploads/{runtime_path}"), entry)?;
    }

    zip.finish().map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Finalize {}: {error}", temp_zip.display()))
    })?;
    Ok((temp_zip, filename))
}

pub(super) fn create_temp_legacy_board_backup_from_saved_full_v4_path(
    root_dir: &Path,
    board_short: &str,
) -> Result<(PathBuf, String)> {
    let verified = v4::verify_saved_v4_root(root_dir, &[v4::BackupScope::FullSite])?;
    create_temp_legacy_board_backup_from_verified_v4(&verified, Some(board_short))
}

pub(super) fn create_temp_board_backup_from_full_backup_path(
    full_backup_path: &Path,
    board_short: &str,
) -> Result<(PathBuf, String)> {
    prune_stale_temp_board_downloads();
    std::fs::create_dir_all(temp_board_download_dir()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Create temp board backup dir: {error}"))
    })?;

    let zip_file = std::fs::File::open(full_backup_path)
        .map_err(|_error| AppError::NotFound("Backup file not found.".into()))?;
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
    let ts = local_backup_timestamp_label();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn full_fixture_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(label);
        v4::write_saved_v4_fixture_for_test(
            &root,
            v4::BackupScope::FullSite,
            v4::board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_010_000_i64,
        );
        (dir, root)
    }

    fn board_fixture_root(label: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(label);
        v4::write_saved_v4_fixture_for_test(
            &root,
            v4::BackupScope::Board,
            v4::board_fixture_files_for_test(),
            None,
            1_715_020_000_i64,
        );
        (dir, root)
    }

    #[test]
    fn saved_full_v4_restore_rejects_db_snapshot_escape() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site_db-escape");
        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        manifest.db_snapshot.as_mut().expect("db snapshot").path = "../escape.db".to_owned();
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = create_temp_legacy_full_backup_from_v4_path(&root)
            .expect_err("db snapshot escape should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn saved_full_v4_restore_rejects_manifest_controlled_site_asset_escape() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site-favicon-escape");
        let favicon_bytes = b"icon".to_vec();
        std::fs::write(
            root.parent().expect("parent").join("escape.ico"),
            &favicon_bytes,
        )
        .expect("outside favicon");
        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        manifest.files.push(v4::test_file_entry_for_test(
            "../escape.ico",
            None,
            v4::BackupFileKind::Favicon,
            &favicon_bytes,
        ));
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = create_temp_legacy_full_backup_from_v4_path(&root)
            .expect_err("favicon escape should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn saved_full_v4_restore_rejects_tor_key_escape() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site-tor-escape");
        let tor_bytes = b"secret".to_vec();
        std::fs::write(
            root.parent().expect("parent").join("outside.key"),
            &tor_bytes,
        )
        .expect("outside key");
        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        manifest.includes.tor_keys = true;
        manifest.files.push(v4::test_file_entry_for_test(
            "../outside.key",
            None,
            v4::BackupFileKind::TorKey,
            &tor_bytes,
        ));
        let mut metadata =
            v4::load_metadata(&root.join(v4::BACKUP_METADATA_FILE_NAME)).expect("metadata");
        metadata.includes_tor_keys = true;
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");
        v4::write_json_pretty(&root.join(v4::BACKUP_METADATA_FILE_NAME), &metadata)
            .expect("metadata");

        let error = create_temp_legacy_full_backup_from_v4_path(&root)
            .expect_err("tor key escape should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn saved_board_v4_restore_rejects_escaping_manifest_path() {
        let (_dir, root) = board_fixture_root("2026-05-06_board-escape");
        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        if let Some(entry) = manifest
            .files
            .iter_mut()
            .find(|entry| entry.kind == v4::BackupFileKind::BoardJson)
        {
            entry.logical_path = "../board.json".to_owned();
        }
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = create_temp_legacy_board_backup_from_v4_path(&root, None)
            .expect_err("board restore strict verify should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn selected_board_extraction_from_saved_full_rejects_escaping_manifest_path() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site-board-escape");
        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        if let Some(entry) = manifest
            .files
            .iter_mut()
            .find(|entry| entry.kind == v4::BackupFileKind::BoardJson)
        {
            entry.logical_path = "../board.json".to_owned();
        }
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = create_temp_legacy_board_backup_from_saved_full_v4_path(&root, "tech")
            .expect_err("selected-board extraction strict verify should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn selected_board_extraction_from_saved_full_rejects_cross_board_file() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site-cross-board");
        let cross_board_path = root.join("boards/other/media/src/example.txt");
        std::fs::create_dir_all(cross_board_path.parent().expect("cross-board parent"))
            .expect("create cross-board parent");
        std::fs::write(&cross_board_path, b"media").expect("write cross-board file");

        let mut manifest = v4::load_manifest(&root.join(v4::MANIFEST_FILE_NAME)).expect("manifest");
        if let Some(entry) = manifest
            .files
            .iter_mut()
            .find(|entry| entry.logical_path == "boards/tech/media/src/example.txt")
        {
            entry.logical_path = "boards/other/media/src/example.txt".to_owned();
        }
        v4::write_json_pretty(&root.join(v4::MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = create_temp_legacy_board_backup_from_saved_full_v4_path(&root, "tech")
            .expect_err("selected-board extraction cross-board file should fail");
        assert!(error
            .to_string()
            .contains("must stay within /tech/ uploads"));
    }

    #[test]
    fn temp_zip_cleanup_guard_removes_conversion_file_on_early_failure() {
        let (_dir, root) = full_fixture_root("2026-05-06_full-site-cleanup");
        let temp_zip = create_temp_legacy_full_backup_from_v4_path(&root).expect("temp zip");
        assert!(temp_zip.exists());
        {
            let _guard = TempZipCleanupGuard::new(temp_zip.clone());
            let _early_failure: Result<()> =
                Err(AppError::BadRequest("synthetic early failure".into()));
        }
        assert!(!temp_zip.exists());
    }
}
