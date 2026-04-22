#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(Deserialize)]
pub struct CreateThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub swatch_hex: Option<String>,
    pub custom_css: String,
    pub enabled: Option<String>,
}

pub async fn create_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let slug = db::sanitize_theme_slug(&form.slug);
            if slug.is_empty() {
                return Err(AppError::BadRequest("Theme slug is required.".into()));
            }
            if db::is_builtin_slug(&slug) {
                return Err(AppError::BadRequest(
                    "That slug is reserved by a built-in theme.".into(),
                ));
            }
            db::create_custom_theme(
                &conn,
                &slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or("")),
                &db::sanitize_theme_css(&form.custom_css),
                form.enabled.as_deref() == Some("1"),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme created.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
}

#[derive(Deserialize)]
pub struct UpdateThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub existing_slug: String,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub swatch_hex: Option<String>,
    pub custom_css: Option<String>,
    pub enabled: Option<String>,
}

pub async fn update_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UpdateThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let existing_slug = db::sanitize_theme_slug(&form.existing_slug);
            let theme = db::get_theme(&conn, &existing_slug)?
                .ok_or_else(|| AppError::BadRequest("Theme not found.".into()))?;
            let mut new_slug = db::sanitize_theme_slug(&form.slug);
            if theme.is_builtin {
                new_slug = existing_slug.clone();
            }
            if new_slug.is_empty() {
                return Err(AppError::BadRequest("Theme slug is required.".into()));
            }
            let custom_css = form.custom_css.as_deref().map(db::sanitize_theme_css);
            db::update_theme(
                &conn,
                &existing_slug,
                &new_slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or("")),
                form.enabled.as_deref() == Some("1"),
                custom_css.as_deref(),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme updated.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
}

#[derive(Deserialize)]
pub struct DeleteThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub slug: String,
}

pub async fn delete_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DeleteThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let slug = db::sanitize_theme_slug(&form.slug);
            db::delete_custom_theme(&conn, &slug)?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(
        super::admin_panel_redirect_anchor_open("Theme deleted.", "theme-catalog", "theme-catalog")
            .into_response(),
    )
}

// ─── POST /admin/vacuum ───────────────────────────────────────────────────────
//
// Runs SQLite VACUUM to reclaim space after bulk deletions.
// Returns an inline result page showing DB size before and after.
