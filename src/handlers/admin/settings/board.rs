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
    allow_any_files: Option<String>,
    allow_tripcodes: Option<String>,
    allow_editing: Option<String>,
    edit_window_secs: Option<String>,
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
    let edit_window_secs = form
        .edit_window_secs
        .as_deref()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(300)
        .clamp(0, 86_400); // 0 = disabled, max 24 h
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
    if access_password.chars().count() > 256 {
        return Err(AppError::BadRequest(
            "Board password must be 256 characters or fewer.".into(),
        ));
    }
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
            let existing_password_hash: String = conn.query_row(
                "SELECT access_password_hash FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
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
            let access_password_hash = if access_password.is_empty() {
                if form.clear_access_password.as_deref() == Some("1") {
                    String::new()
                } else {
                    existing_password_hash
                }
            } else {
                hash_password(&access_password)?
            };
            if access_mode.requires_post_password() && access_password_hash.is_empty() {
                return Err(AppError::BadRequest(
                    "Protected boards require a password before they can be saved.".into(),
                ));
            }
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
                CONFIG.enable_any_file_uploads_feature
                    && form.allow_any_files.as_deref() == Some("1"),
                form.allow_tripcodes.as_deref() == Some("1"),
                edit_window_secs,
                form.allow_editing.as_deref() == Some("1"),
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
