// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

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

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn execute_full_restore<R: std::io::Read + std::io::Seek>(
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
    let live_global_banner_dir = crate::banner::backup_source_dir();
    let staged_global_banner_dir = create_staging_dir(&live_global_banner_dir, "restore-stage")?;
    let mut favicon_extracted = false;
    let mut banner_extracted = false;
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
            copy_limited(&mut entry, &mut out, BANNER_RESTORE_ENTRY_MAX_BYTES).map_err(
                |error| AppError::Internal(anyhow::anyhow!("Write {}: {error}", target.display())),
            )?;
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
        let _ = remove_path_if_exists(&staged_global_banner_dir);
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
        let _ = remove_path_if_exists(&staged_global_banner_dir);
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
    if banner_extracted {
        remove_path_if_exists(&live_global_banner_dir)?;
        std::fs::rename(&staged_global_banner_dir, &live_global_banner_dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "{restore_label} global banner swap failed: {error}"
            ))
        })?;
    } else {
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
        let session_id = restore_auth_preflight(&state, &headers, &jar).await?;
        let upload = stream_restore_upload_to_tempfile(RestoreKind::Full, &mut multipart).await?;
        validate_streamed_restore_upload(RestoreKind::Full, &jar, &upload)?;
        let zip_tmp = upload.temp_file;
        let uploaded_filename = upload.uploaded_filename;

        let upload_dir = CONFIG.upload_dir.clone();

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
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let safe_filename = sanitize_backup_zip_filename(&form.filename)?;

    let path = full_backup_dir().join(&safe_filename);
    let upload_dir = CONFIG.upload_dir.clone();

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
    use super::full_restore_success_response;
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
    use axum_extra::extract::cookie::CookieJar;

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
}
