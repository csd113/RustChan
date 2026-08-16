use super::{
    admin_panel_redirect_anchor_open, db, hash_password, require_admin_post_origin_and_csrf,
    require_admin_session_sid, AppError, AppState, BoardAccessMode, BoardBannerMode, CookieJar,
    Form, HeaderMap, Response, Result, State, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;

#[derive(Deserialize)]
/// Form fields accepted by the board settings request.
pub(crate) struct BoardSettingsForm {
    /// The board identifier.
    board_id: i64,
    /// The name.
    name: String,
    /// The description.
    description: String,
    /// The optional default theme.
    default_theme: Option<String>,
    /// The optional bump limit.
    bump_limit: Option<String>,
    /// The optional max threads.
    max_threads: Option<String>,
    /// The optional max archived threads.
    max_archived_threads: Option<String>,
    /// The optional NSFW.
    nsfw: Option<String>,
    /// The optional allow images.
    allow_images: Option<String>,
    /// The optional allow video.
    allow_video: Option<String>,
    /// The optional allow audio.
    allow_audio: Option<String>,
    /// The optional max image size MB.
    max_image_size_mb: Option<String>,
    /// The optional max video size MB.
    max_video_size_mb: Option<String>,
    /// The optional max audio size MB.
    max_audio_size_mb: Option<String>,
    /// The optional max PDF size MB.
    max_pdf_size_mb: Option<String>,
    /// The optional allow PDF.
    allow_pdf: Option<String>,
    /// The optional allow any files.
    allow_any_files: Option<String>,
    /// The optional allow tripcodes.
    allow_tripcodes: Option<String>,
    /// The optional allow editing.
    allow_editing: Option<String>,
    /// The optional allow self delete.
    allow_self_delete: Option<String>,
    /// The optional allow archive.
    allow_archive: Option<String>,
    /// The optional allow video embeds.
    allow_video_embeds: Option<String>,
    /// The optional allow captcha.
    allow_captcha: Option<String>,
    /// The show poster identifiers.
    show_poster_ids: Option<String>,
    /// The optional collapse greentext.
    collapse_greentext: Option<String>,
    /// The post cooldown duration in seconds.
    post_cooldown_secs: Option<String>,
    /// The optional access mode.
    access_mode: Option<String>,
    /// The optional access password.
    access_password: Option<String>,
    /// The optional clear access password.
    clear_access_password: Option<String>,
    /// The optional banner mode.
    banner_mode: Option<String>,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Parses board upload limit bytes.
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

/// Resolves board access password hash.
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
/// Handles the update board settings request.
pub(crate) async fn update_board_settings(
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
            require_admin_session_sid(&conn, session_id.as_deref())?;
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
                max_threads,
                max_archived_threads,
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
    Ok(
        admin_panel_redirect_anchor_open("Board settings saved.", &board_anchor, &board_anchor)
            .into_response(),
    )
}
