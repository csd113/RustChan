// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) fn temp_board_download_token_path(filename: &str) -> PathBuf {
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

pub(super) fn consume_temp_board_download_token(filename: &str, token: &str) -> Result<bool> {
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

pub(super) fn prune_stale_temp_board_downloads() {
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

#[derive(Default, Deserialize)]
pub struct DownloadBackupQuery {
    cleanup: Option<String>,
    token: Option<String>,
    part: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteBackupForm {
    kind: String,
    filename: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn download_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DownloadBackupQuery>,
    axum::extract::Path((kind, filename)): axum::extract::Path<(String, String)>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());

    let safe_filename = if query.part.is_some() && matches!(kind.as_str(), "full" | "board") {
        sanitize_saved_backup_ref(&filename)?
    } else {
        sanitize_backup_zip_filename(&filename)?
    };

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

    if let Some(part_name) = query.part.as_deref() {
        if !matches!(kind.as_str(), "full" | "board") {
            return Err(AppError::BadRequest(
                "Backup parts are not available for this download kind.".into(),
            ));
        }
        let safe_part = sanitize_backup_zip_filename(part_name)?;
        let backup_root = crate::config::backups_dir().join(&safe_filename);
        let expected_scopes: &[v4::BackupScope] = match kind.as_str() {
            "full" => &[v4::BackupScope::FullSite],
            "board" => &[v4::BackupScope::Board],
            _ => unreachable!("validated above"),
        };
        let verified = v4::verify_saved_v4_root(&backup_root, expected_scopes)?;
        let part_filename = format!("parts/{safe_part}");
        let part = verified
            .manifest
            .parts
            .iter()
            .find(|part| part.filename == part_filename)
            .ok_or_else(|| AppError::NotFound("Backup part not found.".into()))?;
        let path = backup_root.join(&part.filename);
        let resolved = crate::utils::fs_security::canonical_child_of(&backup_root, &path).map_err(
            |error| AppError::BadRequest(format!("Backup part path is unsafe: {error}")),
        )?;
        crate::utils::fs_security::assert_regular_file_no_symlink(&resolved).map_err(|error| {
            AppError::BadRequest(format!("Backup part path is unsafe: {error}"))
        })?;
        let file_size = tokio::fs::metadata(&resolved)
            .await
            .map_err(|_error| AppError::NotFound("Backup part not found.".into()))?
            .len();
        if file_size != part.size {
            return Err(AppError::BadRequest(
                "Backup part size changed since verification.".into(),
            ));
        }
        let file_sha256 = v4::sha256_hex_for_file(&resolved)?;
        if file_sha256 != part.sha256 {
            return Err(AppError::BadRequest(
                "Backup part checksum changed since verification.".into(),
            ));
        }
        let file = tokio::fs::File::open(&resolved)
            .await
            .map_err(|_error| AppError::NotFound("Backup part not found.".into()))?;
        let body = axum::body::Body::from_stream(ReaderStream::new(file));
        let disposition = format!("attachment; filename=\"{safe_part}\"");
        return Ok((
            [
                (header::CONTENT_TYPE, "application/zip".to_owned()),
                (header::CONTENT_DISPOSITION, disposition),
                (header::CONTENT_LENGTH, file_size.to_string()),
            ],
            body,
        )
            .into_response());
    }

    let backup_dir = match kind.as_str() {
        "full" => full_backup_dir(),
        "board" => board_backup_dir(),
        "temp-board" => temp_board_download_dir(),
        _ => unreachable!("validated above"),
    };

    let path = backup_dir.join(&safe_filename);

    let file_size = tokio::fs::metadata(&path)
        .await
        .map_err(|_error| AppError::NotFound("Backup file not found.".into()))?
        .len();

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_error| AppError::NotFound("Backup file not found.".into()))?;
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
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CONTENT_LENGTH, file_size.to_string()),
        ],
        body,
    )
        .into_response())
}

pub async fn backup_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
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
        [(header::CONTENT_TYPE, "application/json".to_owned())],
        json,
    )
        .into_response())
}

pub async fn delete_backup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<DeleteBackupForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let safe_filename = sanitize_saved_backup_ref(&form.filename)?;

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

            let v4_root = crate::config::backups_dir().join(&safe_filename);
            let legacy_path = backup_dir.join(&safe_filename);
            if v4_root.is_dir() {
                super::listing::safe_saved_backup_dir_for_delete(&v4_root)?;
                std::fs::remove_dir_all(&v4_root)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("Delete backup: {e}")))?;
                invalidate_backup_list_cache(&backup_dir, backup_kind);
                tracing::info!(target: "admin", backup_ref = %safe_filename, "Backup directory deleted");
            } else if legacy_path.exists() {
                std::fs::remove_file(&legacy_path)
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
