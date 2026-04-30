// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

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

pub(super) fn create_temp_board_backup_from_full_backup_path(
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
