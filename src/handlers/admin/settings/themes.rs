// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use crate::theme_builder::{
    build_theme_css, builder_defaults_for_preset, parse_builder_config, ThemeBuilderConfig,
    ThemeDensity, ThemeFontFamily,
};

const DEFAULT_THEME_WORKSHOP_PRESET: &str = "forest";
const MAX_ADVANCED_CSS_LEN: usize = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemeEditorMode {
    Builder,
    Legacy,
}

impl ThemeEditorMode {
    fn from_field(value: Option<&str>) -> Self {
        if value.is_some_and(|item| item.eq_ignore_ascii_case("legacy")) {
            Self::Legacy
        } else {
            Self::Builder
        }
    }
}

#[derive(Deserialize)]
pub struct ThemeBuilderFields {
    pub base_preset: Option<String>,
    pub background_color: Option<String>,
    pub panel_color: Option<String>,
    pub card_color: Option<String>,
    pub op_card_color: Option<String>,
    pub text_color: Option<String>,
    pub muted_text_color: Option<String>,
    pub link_color: Option<String>,
    pub link_hover_color: Option<String>,
    pub border_color: Option<String>,
    pub input_background_color: Option<String>,
    pub input_text_color: Option<String>,
    pub input_border_color: Option<String>,
    pub button_background_color: Option<String>,
    pub button_text_color: Option<String>,
    pub button_border_color: Option<String>,
    pub button_hover_color: Option<String>,
    pub header_background_color: Option<String>,
    pub header_text_color: Option<String>,
    pub header_border_color: Option<String>,
    pub quote_color: Option<String>,
    pub meta_text_color: Option<String>,
    pub success_color: Option<String>,
    pub danger_color: Option<String>,
    pub border_radius_px: Option<String>,
    pub density: Option<String>,
    pub font_family: Option<String>,
    pub advanced_css: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateThemeForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub swatch_hex: Option<String>,
    pub theme_mode: Option<String>,
    pub custom_css: Option<String>,
    #[serde(flatten)]
    pub builder: ThemeBuilderFields,
    pub enabled: Option<String>,
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
    pub theme_mode: Option<String>,
    pub custom_css: Option<String>,
    #[serde(flatten)]
    pub builder: ThemeBuilderFields,
    pub enabled: Option<String>,
}

fn sanitize_builder_color(label: &str, raw_value: Option<&str>, fallback: &str) -> Result<String> {
    let value = raw_value.unwrap_or(fallback).trim();
    if value.len() == 7
        && value.starts_with('#')
        && value.chars().skip(1).all(|ch| ch.is_ascii_hexdigit())
    {
        Ok(value.to_ascii_lowercase())
    } else {
        Err(AppError::BadRequest(format!(
            "{label} must be a 6-digit hex color like #7ab84e."
        )))
    }
}

fn sanitize_builder_advanced_css(raw_value: Option<&str>) -> Result<String> {
    let trimmed = raw_value.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > MAX_ADVANCED_CSS_LEN {
        return Err(AppError::BadRequest(format!(
            "Advanced CSS must be {MAX_ADVANCED_CSS_LEN} characters or fewer."
        )));
    }

    let lowered = trimmed.to_ascii_lowercase();
    for blocked in [
        "@import",
        "<style",
        "</style",
        "expression(",
        "javascript:",
        "-moz-binding",
    ] {
        if lowered.contains(blocked) {
            return Err(AppError::BadRequest(
                "Advanced CSS may not use imports, script-like URLs, or style tags.".into(),
            ));
        }
    }

    Ok(trimmed.to_string())
}

fn parse_radius(raw_value: Option<&str>, fallback: u8) -> Result<u8> {
    raw_value
        .unwrap_or("")
        .trim()
        .parse::<u8>()
        .ok()
        .map(|value| value.clamp(0, 24))
        .or_else(|| {
            if raw_value.unwrap_or("").trim().is_empty() {
                Some(fallback)
            } else {
                None
            }
        })
        .ok_or_else(|| {
            AppError::BadRequest("Border radius must be a whole number from 0 to 24.".into())
        })
}

fn parse_density(raw_value: Option<&str>, fallback: ThemeDensity) -> Result<ThemeDensity> {
    raw_value.map_or(Ok(fallback), |value| {
        ThemeDensity::parse(value.trim())
            .ok_or_else(|| AppError::BadRequest("Compactness must be Cozy or Compact.".into()))
    })
}

fn parse_font_family(
    raw_value: Option<&str>,
    fallback: ThemeFontFamily,
) -> Result<ThemeFontFamily> {
    raw_value.map_or(Ok(fallback), |value| {
        ThemeFontFamily::parse(value.trim()).ok_or_else(|| {
            AppError::BadRequest(
                "Font family must be one of the built-in system font choices.".into(),
            )
        })
    })
}

fn resolve_builder_config(
    fields: &ThemeBuilderFields,
    existing_theme: Option<&crate::models::Theme>,
) -> Result<ThemeBuilderConfig> {
    let existing_config = existing_theme.and_then(|theme| parse_builder_config(&theme.custom_css));
    let requested_preset = fields
        .base_preset
        .as_deref()
        .map(db::sanitize_theme_slug)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            existing_config.as_ref().map_or_else(
                || DEFAULT_THEME_WORKSHOP_PRESET.to_string(),
                |config| config.base_preset.clone(),
            )
        });

    let preset_defaults = builder_defaults_for_preset(&requested_preset);
    let fallback = existing_config.as_ref().unwrap_or(&preset_defaults);

    Ok(ThemeBuilderConfig {
        base_preset: preset_defaults.base_preset.clone(),
        background_color: sanitize_builder_color(
            "Background color",
            fields.background_color.as_deref(),
            &fallback.background_color,
        )?,
        panel_color: sanitize_builder_color(
            "Panel color",
            fields.panel_color.as_deref(),
            &fallback.panel_color,
        )?,
        card_color: sanitize_builder_color(
            "Post/card color",
            fields.card_color.as_deref(),
            &fallback.card_color,
        )?,
        op_card_color: sanitize_builder_color(
            "Thread starter color",
            fields.op_card_color.as_deref(),
            &fallback.op_card_color,
        )?,
        text_color: sanitize_builder_color(
            "Text color",
            fields.text_color.as_deref(),
            &fallback.text_color,
        )?,
        muted_text_color: sanitize_builder_color(
            "Muted text color",
            fields.muted_text_color.as_deref(),
            &fallback.muted_text_color,
        )?,
        link_color: sanitize_builder_color(
            "Link color",
            fields.link_color.as_deref(),
            &fallback.link_color,
        )?,
        link_hover_color: sanitize_builder_color(
            "Link hover color",
            fields.link_hover_color.as_deref(),
            &fallback.link_hover_color,
        )?,
        border_color: sanitize_builder_color(
            "Border color",
            fields.border_color.as_deref(),
            &fallback.border_color,
        )?,
        input_background_color: sanitize_builder_color(
            "Input background color",
            fields.input_background_color.as_deref(),
            &fallback.input_background_color,
        )?,
        input_text_color: sanitize_builder_color(
            "Input text color",
            fields.input_text_color.as_deref(),
            &fallback.input_text_color,
        )?,
        input_border_color: sanitize_builder_color(
            "Input border color",
            fields.input_border_color.as_deref(),
            &fallback.input_border_color,
        )?,
        button_background_color: sanitize_builder_color(
            "Button background color",
            fields.button_background_color.as_deref(),
            &fallback.button_background_color,
        )?,
        button_text_color: sanitize_builder_color(
            "Button text color",
            fields.button_text_color.as_deref(),
            &fallback.button_text_color,
        )?,
        button_border_color: sanitize_builder_color(
            "Button border color",
            fields.button_border_color.as_deref(),
            &fallback.button_border_color,
        )?,
        button_hover_color: sanitize_builder_color(
            "Button hover color",
            fields.button_hover_color.as_deref(),
            &fallback.button_hover_color,
        )?,
        header_background_color: sanitize_builder_color(
            "Header background color",
            fields.header_background_color.as_deref(),
            &fallback.header_background_color,
        )?,
        header_text_color: sanitize_builder_color(
            "Header text color",
            fields.header_text_color.as_deref(),
            &fallback.header_text_color,
        )?,
        header_border_color: sanitize_builder_color(
            "Header border color",
            fields.header_border_color.as_deref(),
            &fallback.header_border_color,
        )?,
        quote_color: sanitize_builder_color(
            "Quote color",
            fields.quote_color.as_deref(),
            &fallback.quote_color,
        )?,
        meta_text_color: sanitize_builder_color(
            "Metadata color",
            fields.meta_text_color.as_deref(),
            &fallback.meta_text_color,
        )?,
        success_color: sanitize_builder_color(
            "Success color",
            fields.success_color.as_deref(),
            &fallback.success_color,
        )?,
        danger_color: sanitize_builder_color(
            "Error color",
            fields.danger_color.as_deref(),
            &fallback.danger_color,
        )?,
        border_radius_px: parse_radius(
            fields.border_radius_px.as_deref(),
            fallback.border_radius_px,
        )?,
        density: parse_density(fields.density.as_deref(), fallback.density)?,
        font_family: parse_font_family(fields.font_family.as_deref(), fallback.font_family)?,
        advanced_css: sanitize_builder_advanced_css(fields.advanced_css.as_deref())?,
    })
}

fn resolved_theme_css_for_create(form: &CreateThemeForm, slug: &str) -> Result<(String, String)> {
    match ThemeEditorMode::from_field(form.theme_mode.as_deref()) {
        ThemeEditorMode::Builder => {
            let config = resolve_builder_config(&form.builder, None)?;
            let css = build_theme_css(slug, &config);
            Ok((db::sanitize_theme_swatch(&config.link_color), css))
        }
        ThemeEditorMode::Legacy => {
            let css = db::sanitize_theme_css(form.custom_css.as_deref().unwrap_or(""));
            let swatch = db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or(""));
            Ok((swatch, css))
        }
    }
}

fn resolved_theme_css_for_update(
    form: &UpdateThemeForm,
    theme: &crate::models::Theme,
    new_slug: &str,
) -> Result<(String, Option<String>)> {
    match ThemeEditorMode::from_field(form.theme_mode.as_deref()) {
        ThemeEditorMode::Builder => {
            let config = resolve_builder_config(&form.builder, Some(theme))?;
            let css = build_theme_css(new_slug, &config);
            Ok((db::sanitize_theme_swatch(&config.link_color), Some(css)))
        }
        ThemeEditorMode::Legacy => {
            let swatch = db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or(""));
            let css = form.custom_css.as_deref().map(db::sanitize_theme_css);
            Ok((swatch, css))
        }
    }
}

pub async fn create_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<CreateThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
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
            let (swatch_hex, theme_css) = resolved_theme_css_for_create(&form, &slug)?;
            db::create_custom_theme(
                &conn,
                &slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &swatch_hex,
                &theme_css,
                form.enabled.as_deref() == Some("1"),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
    .map(|()| {
        super::admin_panel_redirect_anchor_open("Theme created.", "theme-catalog", "theme-catalog")
            .into_response()
    })
    .or_else(|error| match error {
        AppError::BadRequest(message) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "theme-catalog",
            "theme-catalog",
        )
        .into_response()),
        other => Err(other),
    })
}

pub async fn update_theme(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<UpdateThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
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
            let (swatch_hex, custom_css) = if theme.is_builtin {
                (
                    db::sanitize_theme_swatch(form.swatch_hex.as_deref().unwrap_or("")),
                    None,
                )
            } else {
                resolved_theme_css_for_update(&form, &theme, &new_slug)?
            };
            db::update_theme(
                &conn,
                &existing_slug,
                &new_slug,
                &db::sanitize_theme_name(&form.display_name),
                &db::sanitize_theme_description(form.description.as_deref().unwrap_or("")),
                &swatch_hex,
                form.enabled.as_deref() == Some("1"),
                custom_css.as_deref(),
            )?;
            db::sync_live_theme_state(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
    .map(|()| {
        super::admin_panel_redirect_anchor_open("Theme updated.", "theme-catalog", "theme-catalog")
            .into_response()
    })
    .or_else(|error| match error {
        AppError::BadRequest(message) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "theme-catalog",
            "theme-catalog",
        )
        .into_response()),
        other => Err(other),
    })
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
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<DeleteThemeForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
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

#[cfg(test)]
mod tests {
    use crate::error::AppError;

    use super::{
        resolved_theme_css_for_create, sanitize_builder_advanced_css, CreateThemeForm,
        ThemeBuilderFields, ThemeEditorMode,
    };

    fn builder_fields() -> ThemeBuilderFields {
        ThemeBuilderFields {
            base_preset: Some("forest".into()),
            background_color: Some("#101010".into()),
            panel_color: Some("#202020".into()),
            card_color: Some("#303030".into()),
            op_card_color: Some("#404040".into()),
            text_color: Some("#f0f0f0".into()),
            muted_text_color: Some("#bbbbbb".into()),
            link_color: Some("#77aa55".into()),
            link_hover_color: Some("#99cc77".into()),
            border_color: Some("#555555".into()),
            input_background_color: Some("#111111".into()),
            input_text_color: Some("#eeeeee".into()),
            input_border_color: Some("#666666".into()),
            button_background_color: Some("#335522".into()),
            button_text_color: Some("#ffffff".into()),
            button_border_color: Some("#557744".into()),
            button_hover_color: Some("#446633".into()),
            header_background_color: Some("#181818".into()),
            header_text_color: Some("#f3f3f3".into()),
            header_border_color: Some("#668855".into()),
            quote_color: Some("#88bb66".into()),
            meta_text_color: Some("#aaaaaa".into()),
            success_color: Some("#66aa77".into()),
            danger_color: Some("#cc6677".into()),
            border_radius_px: Some("9".into()),
            density: Some("compact".into()),
            font_family: Some("system_mono".into()),
            advanced_css: Some(
                "html[data-theme=\"builder-test\"] .subject { font-style: italic; }".into(),
            ),
        }
    }

    #[test]
    fn builder_theme_form_generates_scoped_css_and_swatch() {
        let form = CreateThemeForm {
            csrf: None,
            slug: "builder-test".into(),
            display_name: "Builder Test".into(),
            description: Some("Generated in tests".into()),
            swatch_hex: None,
            theme_mode: Some("builder".into()),
            custom_css: None,
            builder: builder_fields(),
            enabled: Some("1".into()),
        };

        let (swatch, css) = resolved_theme_css_for_create(&form, "builder-test").expect("css");

        assert_eq!(swatch, "#77aa55");
        assert!(css.contains("html[data-theme=\"builder-test\"]"));
        assert!(css.contains("--rustchan-builder-data:"));
        assert!(css.contains(".subject { font-style: italic; }"));
    }

    #[test]
    fn builder_theme_form_rejects_invalid_color_values() {
        let mut form = CreateThemeForm {
            csrf: None,
            slug: "builder-test".into(),
            display_name: "Builder Test".into(),
            description: None,
            swatch_hex: None,
            theme_mode: Some("builder".into()),
            custom_css: None,
            builder: builder_fields(),
            enabled: Some("1".into()),
        };
        form.builder.link_color = Some("javascript:alert(1)".into());

        let error =
            resolved_theme_css_for_create(&form, "builder-test").expect_err("invalid color");
        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("Link color"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn advanced_css_rejects_imports_and_scripty_constructs() {
        let error =
            sanitize_builder_advanced_css(Some("@import url(https://example.com/theme.css);"))
                .expect_err("blocked css");
        match error {
            AppError::BadRequest(message) => {
                assert!(message.contains("Advanced CSS"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn editor_mode_defaults_to_builder() {
        assert_eq!(ThemeEditorMode::from_field(None), ThemeEditorMode::Builder);
        assert_eq!(
            ThemeEditorMode::from_field(Some("legacy")),
            ThemeEditorMode::Legacy
        );
    }
}
