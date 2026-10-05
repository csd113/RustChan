use super::{
    admin_panel_redirect_anchor_open, db, hash_password, require_admin_post_origin_and_csrf,
    require_admin_session_sid, AppError, AppState, BoardAccessMode, BoardBannerMode, CookieJar,
    Form, HeaderMap, Response, Result, State, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;

#[derive(Deserialize)]
pub(in crate::server) struct BoardSettingsForm {
    board_id: i64,
    name: String,
    description: String,
    default_theme: Option<String>,
    bump_limit: Option<String>,
    max_threads: Option<String>,
    max_archived_threads: Option<String>,
    nsfw: Option<String>,
    allow_images: Option<String>,
    allow_video: Option<String>,
    allow_audio: Option<String>,
    max_image_size_mb: Option<String>,
    max_video_size_mb: Option<String>,
    max_audio_size_mb: Option<String>,
    max_pdf_size_mb: Option<String>,
    allow_pdf: Option<String>,
    allow_any_files: Option<String>,
    allow_tripcodes: Option<String>,
    allow_editing: Option<String>,
    allow_self_delete: Option<String>,
    allow_archive: Option<String>,
    allow_video_embeds: Option<String>,
    allow_captcha: Option<String>,
    show_poster_ids: Option<String>,
    collapse_greentext: Option<String>,
    post_cooldown_secs: Option<String>,
    access_mode: Option<String>,
    access_password: Option<String>,
    clear_access_password: Option<String>,
    banner_mode: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

/// Parse destructive retention settings without silently reducing saved limits.
fn parse_retention_setting(raw: Option<&str>, archived: bool) -> Result<Option<i64>> {
    let upper = if archived { 10_000 } else { 1_000 };
    let label = if archived {
        "Archive thread limit"
    } else {
        "Active thread limit"
    };
    raw.map(|value| {
        let invalid = || {
            AppError::BadRequest(format!(
                "{label} must be a whole number between 1 and {upper}."
            ))
        };
        let parsed = value.trim().parse::<i64>().map_err(|_error| invalid())?;
        if !(1..=upper).contains(&parsed) {
            return Err(invalid());
        }
        Ok(parsed)
    })
    .transpose()
}

fn parse_board_upload_limit_bytes(raw_value: Option<&str>, fallback_bytes: i64) -> Result<i64> {
    const MIB: i64 = 1024 * 1024;
    // Deliberate site maximum for each per-board media cap. Aggregate public
    // multipart limits still bound a whole request separately.
    const SITE_MAX_BOARD_UPLOAD_CAP_MB: i64 = 512;

    let fallback_mb = (fallback_bytes / MIB).max(1);
    let parsed_mb = match raw_value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value.parse::<i64>().map_err(|_error| {
            AppError::BadRequest(
                "Board upload size limits must be positive whole MiB values.".into(),
            )
        })?,
        None => fallback_mb,
    };
    if parsed_mb <= 0 {
        return Err(AppError::BadRequest(
            "Board upload size limits must be at least 1 MiB.".into(),
        ));
    }

    if parsed_mb > SITE_MAX_BOARD_UPLOAD_CAP_MB {
        return Err(AppError::BadRequest(format!(
            "Board upload size limits must be {SITE_MAX_BOARD_UPLOAD_CAP_MB} MiB or less."
        )));
    }

    parsed_mb
        .checked_mul(MIB)
        .ok_or_else(|| AppError::BadRequest("Board upload size limit is too large.".into()))
}

fn resolve_board_access_password_hash(
    access_mode: BoardAccessMode,
    existing_password_hash: String,
    submitted_password: &str,
    clear_password: bool,
) -> Result<String> {
    if submitted_password.chars().count() > 256 {
        return Err(AppError::BadRequest(
            "Board password must be 256 characters or fewer.".into(),
        ));
    }

    let access_password_hash = if submitted_password.is_empty() {
        if clear_password {
            String::new()
        } else {
            existing_password_hash
        }
    } else {
        hash_password(submitted_password)?
    };

    if access_mode.requires_post_password() && access_password_hash.is_empty() {
        return Err(AppError::BadRequest(
            "Password-protected boards require a saved password. Enter a new board password or switch access mode to Public before removing it.".into(),
        ));
    }

    Ok(access_password_hash)
}

#[expect(
    clippy::too_many_lines,
    reason = "cross-field board validation and the atomic settings update share one request boundary"
)]
pub(in crate::server) async fn update_board_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BoardSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let bump_limit = form
        .bump_limit
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(500)
        .clamp(1, 10_000);
    let max_threads = parse_retention_setting(form.max_threads.as_deref(), false)?;
    let max_archived_threads = parse_retention_setting(form.max_archived_threads.as_deref(), true)?;
    let post_cooldown_secs = form
        .post_cooldown_secs
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 3_600); // 0 = disabled, max 1 hour

    // Enforce server-side length limits on free-text fields
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form
        .description
        .trim()
        .chars()
        .take(256)
        .collect::<String>();
    let access_mode = BoardAccessMode::from_db_str(form.access_mode.as_deref().unwrap_or("public"))
        .ok_or_else(|| AppError::BadRequest("Invalid board access mode.".into()))?;
    let access_password = form.access_password.clone().unwrap_or_default();
    let board_id = form.board_id;
    let banner_mode =
        BoardBannerMode::from_db_str(form.banner_mode.as_deref().unwrap_or("inherit"))
            .ok_or_else(|| AppError::BadRequest("Invalid board banner mode.".into()))?;

    let board_short = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            let current_board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            let resolved_default_theme = form
                .default_theme
                .as_deref()
                .map(db::sanitize_theme_slug)
                .filter(|slug| {
                    slug.is_empty()
                        || db::get_theme(&conn, slug)
                            .ok()
                            .flatten()
                            .is_some_and(|theme| theme.enabled)
                })
                .unwrap_or_default();
            let access_password_hash = resolve_board_access_password_hash(
                access_mode,
                current_board.access_password_hash.clone(),
                &access_password,
                form.clear_access_password.as_deref() == Some("1"),
            )?;
            let max_image_size = parse_board_upload_limit_bytes(
                form.max_image_size_mb.as_deref(),
                current_board.max_image_size,
            )?;
            let max_video_size = parse_board_upload_limit_bytes(
                form.max_video_size_mb.as_deref(),
                current_board.max_video_size,
            )?;
            let max_audio_size = parse_board_upload_limit_bytes(
                form.max_audio_size_mb.as_deref(),
                current_board.max_audio_size,
            )?;
            let max_pdf_size = parse_board_upload_limit_bytes(
                form.max_pdf_size_mb.as_deref(),
                current_board.max_pdf_size,
            )?;
            db::update_board_settings(
                &mut conn,
                board_id,
                &name,
                &description,
                form.nsfw.as_deref() == Some("1"),
                bump_limit,
                max_threads.unwrap_or(current_board.max_threads),
                max_archived_threads.unwrap_or(current_board.max_archived_threads),
                form.allow_images.as_deref() == Some("1"),
                form.allow_video.as_deref() == Some("1"),
                form.allow_audio.as_deref() == Some("1"),
                max_image_size,
                max_video_size,
                max_audio_size,
                max_pdf_size,
                form.allow_pdf.as_deref() == Some("1"),
                CONFIG.enable_any_file_uploads_feature
                    && form.allow_any_files.as_deref() == Some("1"),
                form.allow_tripcodes.as_deref() == Some("1"),
                // The old board edit-window field is kept for schema/backup
                // compatibility; self-service edits now share the fixed
                // short ownership window used by deletes.
                crate::handlers::board::self_action_window_secs(),
                form.allow_editing.as_deref() == Some("1"),
                form.allow_self_delete.as_deref() == Some("1"),
                form.allow_archive.as_deref() == Some("1"),
                form.allow_video_embeds.as_deref() == Some("1"),
                form.allow_captcha.as_deref() == Some("1"),
                form.show_poster_ids.as_deref() == Some("1"),
                form.collapse_greentext.as_deref() == Some("1"),
                post_cooldown_secs,
                &resolved_default_theme,
                banner_mode,
                access_mode,
                &access_password_hash,
            )?;
            tracing::info!(
                target: "admin",
                board = %board_short,
                board_id = board_id,
                "Saved board settings"
            );
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let board_anchor = format!("board-{board_short}");
    Ok(
        admin_panel_redirect_anchor_open("Board settings saved.", &board_anchor, &board_anchor)
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::parse_retention_setting;
    use anyhow::{ensure, Result};

    #[test]
    fn retention_settings_reject_invalid_values_and_preserve_omitted_limits() -> Result<()> {
        for archived in [false, true] {
            let upper = if archived { 10_000_i64 } else { 1_000_i64 };
            ensure!(parse_retention_setting(None, archived)?.is_none());
            ensure!(parse_retention_setting(Some(" 1 "), archived)? == Some(1));
            ensure!(parse_retention_setting(Some(&upper.to_string()), archived)? == Some(upper));
            for value in ["", "0", "-1", "garbage", "1.5", "9223372036854775808"] {
                ensure!(parse_retention_setting(Some(value), archived).is_err());
            }
            ensure!(parse_retention_setting(Some(&(upper + 1_i64).to_string()), archived).is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn invalid_retention_http_settings_cannot_reduce_saved_caps() -> Result<()> {
        use super::{
            db, update_board_settings, BoardSettingsForm, CookieJar, Form, HeaderMap, State,
        };
        use axum::http::{header, HeaderValue};
        use axum_extra::extract::cookie::Cookie;
        let state = crate::test_support::app_state();
        let board_id = {
            let conn = state.db.get()?;
            let admin = db::create_admin(
                &conn,
                "archive-admin",
                &crate::utils::crypto::hash_password("password")?,
            )?;
            db::create_session(
                &conn,
                "session123",
                admin,
                chrono::Utc::now().timestamp() + 3600,
            )?;
            let id = db::create_board(&conn, "settings", "Settings", "", false)?;
            conn.execute(
                "UPDATE boards SET max_threads=200,max_archived_threads=2000 WHERE id=?1",
                [id],
            )
            .map(|_rows| ())?;
            id
        };
        for value in [Some("0"), Some("-1"), Some("garbage"), None] {
            let input = serde_json::json!({
                "board_id": board_id, "name": "Settings", "description": "",
                "max_archived_threads": value,
                "_csrf": crate::test_support::admin_signed_csrf()
            });
            let form: BoardSettingsForm = serde_json::from_value(input)?;
            let jar = CookieJar::new()
                .add(Cookie::new(super::SESSION_COOKIE, "session123"))
                .add(Cookie::new("csrf_token", "csrf123"));
            let mut headers = HeaderMap::new();
            let _host = headers.insert(header::HOST, HeaderValue::from_static("localhost"));
            let _origin =
                headers.insert(header::ORIGIN, HeaderValue::from_static("http://localhost"));
            let outcome = update_board_settings(
                State(state.clone()),
                jar,
                headers,
                crate::test_support::connect_info(),
                Form(form),
            )
            .await;
            if value.is_some() {
                ensure!(matches!(outcome, Err(super::AppError::BadRequest(_))));
            } else {
                let response = outcome?;
                ensure!(response.status() == axum::http::StatusCode::SEE_OTHER);
            }
            let conn = state.db.get()?;
            let board = db::get_board_by_short(&conn, "settings")?
                .ok_or_else(|| anyhow::anyhow!("saved board"))?;
            ensure!(board.max_threads == 200 && board.max_archived_threads == 2000);
        }
        Ok(())
    }
}
