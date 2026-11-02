//! Authenticated, bounded access to the current main application log.

use super::{require_admin_session_sid, SESSION_COOKIE};
use crate::{
    error::{AppError, Result},
    middleware::AppState,
};
use axum::{
    extract::{Query, State},
    http::header,
    response::{IntoResponse as _, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Deserialize, Default)]
/// Query parameters controlling the live-log response size.
pub(in crate::server) struct LiveLogQuery {
    pub bytes: Option<usize>,
}

pub(in crate::server) async fn admin_live_log(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(params): Query<LiveLogQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    let max_bytes = params.bytes.unwrap_or(65_536).clamp(4_096, 262_144);

    let payload = crate::middleware::data_access::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;

            let logs_dir = crate::config::logs_dir();

            let Some(path) = latest_log_file(&logs_dir) else {
                return Ok(
                    serde_json::json!({
                        "filename": "no log file",
                        "content": "No live log file found yet.",
                        "truncated": false
                    })
                    .to_string(),
                );
            };

            let (content, truncated) = read_log_tail(&path, max_bytes)?;
            Ok(
                serde_json::json!({
                    "filename": path.file_name().and_then(|name| name.to_str()).unwrap_or("current log"),
                    "content": content,
                    "truncated": truncated
                })
                .to_string(),
            )
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_owned(),
            ),
            (
                header::CACHE_CONTROL,
                "private, no-cache, no-store, must-revalidate, no-transform".to_owned(),
            ),
            (header::PRAGMA, "no-cache".to_owned()),
            (header::EXPIRES, "0".to_owned()),
            (
                header::HeaderName::from_static("x-accel-buffering"),
                "no".to_owned(),
            ),
            (header::VARY, "Cookie".to_owned()),
        ],
        payload,
    )
        .into_response())
}

pub(super) fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(logs_dir).ok()?.flatten() {
        let path = entry.path();
        if !crate::logging::is_main_log_file(&path) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().ok()?;
        if latest
            .as_ref()
            .is_none_or(|(current_modified, current_path)| {
                modified > *current_modified
                    || (modified == *current_modified && path > *current_path)
            })
        {
            latest = Some((modified, path));
        }
    }
    latest.map(|(_, path)| path)
}

/// Reads log tail.
pub(super) fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool)> {
    crate::logging::read_log_tail(path, max_bytes)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))
}
