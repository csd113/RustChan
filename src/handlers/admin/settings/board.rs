// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Deserialize)]
pub struct BoardSettingsForm {
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

fn parse_board_upload_limit_bytes(
    raw_value: Option<&str>,
    fallback_bytes: i64,
    global_max_bytes: usize,
) -> Result<i64> {
    const MIB: i64 = 1024 * 1024;

    let fallback_mb = (fallback_bytes / MIB).max(1);
    let parsed_mb = raw_value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(fallback_mb);
    let global_max_mb = i64::try_from(global_max_bytes / (1024 * 1024)).map_err(|_| {
        AppError::Internal(anyhow::anyhow!("Global upload limit does not fit in i64"))
    })?;
    let clamped_mb = parsed_mb.clamp(1, global_max_mb.max(1));

    clamped_mb
        .checked_mul(MIB)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("Board upload limit overflowed i64")))
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

    if access_mode.is_password_protected() && access_password_hash.is_empty() {
        return Err(AppError::BadRequest(
            "Password-protected boards require a saved password. Enter a new board password or switch access mode to Public before removing it.".into(),
        ));
    }

    Ok(access_password_hash)
}

pub async fn update_board_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardSettingsForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let bump_limit = form
        .bump_limit
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(500)
        .clamp(1, 10_000);
    let max_threads = form
        .max_threads
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150)
        .clamp(1, 1_000);
    let max_archived_threads = form
        .max_archived_threads
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(150)
        .clamp(1, 10_000);
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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
                CONFIG.max_image_size,
            )?;
            let max_video_size = parse_board_upload_limit_bytes(
                form.max_video_size_mb.as_deref(),
                current_board.max_video_size,
                CONFIG.max_video_size,
            )?;
            let max_audio_size = parse_board_upload_limit_bytes(
                form.max_audio_size_mb.as_deref(),
                current_board.max_audio_size,
                CONFIG.max_audio_size,
            )?;
            db::update_board_settings(
                &mut conn,
                board_id,
                &name,
                &description,
                form.nsfw.as_deref() == Some("1"),
                bump_limit,
                max_threads,
                max_archived_threads,
                form.allow_images.as_deref() == Some("1"),
                form.allow_video.as_deref() == Some("1"),
                form.allow_audio.as_deref() == Some("1"),
                max_image_size,
                max_video_size,
                max_audio_size,
                form.allow_pdf.as_deref() == Some("1"),
                CONFIG.enable_any_file_uploads_feature
                    && form.allow_any_files.as_deref() == Some("1"),
                form.allow_tripcodes.as_deref() == Some("1"),
                // The old board edit-window field is kept for schema/backup
                // compatibility; self-service edits now share the fixed
                // short ownership window used by deletes.
                crate::handlers::board::SELF_DELETE_WINDOW_SECS,
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
    Ok(super::admin_panel_redirect_anchor_open(
        "Board settings saved.",
        &board_anchor,
        &board_anchor,
    )
    .into_response())
}
