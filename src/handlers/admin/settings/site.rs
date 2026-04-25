// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Deserialize)]
pub struct SiteSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    /// Custom site name (replaces [ `RustChan` ] on home page and footer).
    pub site_name: Option<String>,
    /// Custom home page subtitle line below the site name.
    pub site_subtitle: Option<String>,
    /// Toggle browser-local new-activity badges.
    pub new_activity_notifications_enabled: Option<String>,
    /// Default theme served to first-time visitors.
    pub default_theme: Option<String>,
    pub banner_rotation_interval_minutes: Option<String>,
    pub banner_external_links_enabled: Option<String>,
}

pub async fn update_site_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<SiteSettingsForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    let is_banner_settings_only = form.site_name.is_none()
        && form.site_subtitle.is_none()
        && form.new_activity_notifications_enabled.is_none()
        && form.default_theme.is_none()
        && (form.banner_rotation_interval_minutes.is_some()
            || form.banner_external_links_enabled.is_some());
    let banner_rotation_interval_minutes = form
        .banner_rotation_interval_minutes
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 43_200);
    let banner_external_links_enabled =
        checkbox_is_on(form.banner_external_links_enabled.as_deref());
    let new_activity_notifications_enabled =
        checkbox_is_on(form.new_activity_notifications_enabled.as_deref());

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            // Save the custom site name (trimmed, max 64 chars).
            let new_name = form.site_name.as_deref().map_or_else(
                || db::get_site_name(&conn),
                |value| value.trim().chars().take(64).collect::<String>(),
            );
            db::set_site_setting(&conn, "site_name", &new_name)?;
            // Update the in-memory live name so all pages reflect it immediately.
            crate::templates::set_live_site_name(&new_name);
            tracing::info!(target: "admin", "Site name updated");

            // Save the custom subtitle.
            let new_subtitle = form.site_subtitle.as_deref().map_or_else(
                || db::get_site_subtitle(&conn),
                |value| value.trim().chars().take(128).collect::<String>(),
            );
            db::set_site_setting(&conn, "site_subtitle", &new_subtitle)?;
            crate::templates::set_live_site_subtitle(&new_subtitle);
            tracing::info!(target: "admin", "Site subtitle updated");

            // Save the default theme slug (validated against allowed values).
            let new_theme = if let Some(value) = form.default_theme.as_deref() {
                let candidate = db::sanitize_theme_slug(value);
                if candidate.is_empty() {
                    crate::theme::HARD_DEFAULT_THEME.to_string()
                } else if db::get_theme(&conn, &candidate)?.is_some_and(|theme| theme.enabled) {
                    candidate
                } else {
                    crate::theme::HARD_DEFAULT_THEME.to_string()
                }
            } else {
                db::get_default_user_theme(&conn)
            };
            db::set_site_setting(&conn, "default_theme", &new_theme)?;
            db::sync_live_theme_state(&conn)?;
            tracing::info!(target: "admin", "Default theme updated");

            db::set_site_setting(
                &conn,
                "new_activity_notifications_enabled",
                if new_activity_notifications_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            tracing::info!(target: "admin", "New-activity badge setting updated");

            // Persist overlapping global settings back to settings.toml so
            // they survive a restart without requiring a manual file edit.
            crate::config::update_settings_file_site_settings(
                &new_name,
                &new_subtitle,
                new_activity_notifications_enabled,
                &new_theme,
            );
            tracing::info!(target: "admin", "settings.toml updated");

            db::set_site_setting(
                &conn,
                "banner_rotation_interval_minutes",
                &banner_rotation_interval_minutes.to_string(),
            )?;
            db::set_site_setting(
                &conn,
                "banner_external_links_enabled",
                if banner_external_links_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            tracing::info!(target: "admin", "Banner settings updated");

            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    if is_banner_settings_only {
        Ok(super::admin_panel_redirect_anchor_open(
            "Banner settings saved.",
            "board-banners",
            "board-banners",
        )
        .into_response())
    } else {
        Ok(Redirect::to("/admin/panel?settings_saved=1").into_response())
    }
}
