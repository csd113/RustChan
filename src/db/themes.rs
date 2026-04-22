use crate::{
    config::CONFIG,
    models::Theme,
    theme::{builtin_theme, builtin_theme_rows},
};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeSet;

const LEGACY_DEFAULT_BUILTIN_THEMES: &[&str] = &[
    "terminal",
    "aero",
    "dorfic",
    "forest",
    "chanclassic",
    "neoncubicle",
    "fluorogrid",
];

fn configured_enabled_builtin_slugs() -> Vec<String> {
    let mut enabled_builtin_slugs = CONFIG
        .initial_enabled_builtin_themes
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

    let configured = enabled_builtin_slugs
        .iter()
        .map(|slug| slug.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let legacy_default = LEGACY_DEFAULT_BUILTIN_THEMES
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<BTreeSet<_>>();

    if configured == legacy_default {
        enabled_builtin_slugs.extend(["blue-sky".to_string(), "deep-orbit".to_string()]);
    }

    enabled_builtin_slugs
}

fn map_theme(row: &rusqlite::Row<'_>) -> rusqlite::Result<Theme> {
    Ok(Theme {
        slug: row.get(0)?,
        display_name: row.get(1)?,
        description: row.get(2)?,
        swatch_hex: row.get(3)?,
        enabled: row.get::<_, i32>(4)? != 0,
        sort_order: row.get(5)?,
        is_builtin: row.get::<_, i32>(6)? != 0,
        custom_css: row.get(7)?,
    })
}

/// Load all themes in display order.
///
/// # Errors
/// Returns an error if the query fails.
pub fn load_themes(conn: &rusqlite::Connection) -> Result<Vec<Theme>> {
    let mut stmt = conn.prepare_cached(
        "SELECT slug, display_name, description, swatch_hex, enabled, sort_order, is_builtin, custom_css
         FROM themes
         ORDER BY is_builtin DESC, sort_order ASC, slug ASC",
    )?;
    let themes = stmt
        .query_map([], map_theme)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(themes)
}

/// Sync the in-memory theme cache from the database.
///
/// # Errors
/// Returns an error if theme loading fails.
pub fn sync_live_theme_state(conn: &rusqlite::Connection) -> Result<()> {
    crate::templates::set_live_default_theme(&crate::db::get_default_user_theme(conn));
    crate::templates::set_live_themes(load_themes(conn)?);
    Ok(())
}

/// Load a theme by slug, case-insensitively.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_theme(conn: &rusqlite::Connection, slug: &str) -> Result<Option<Theme>> {
    let mut stmt = conn.prepare_cached(
        "SELECT slug, display_name, description, swatch_hex, enabled, sort_order, is_builtin, custom_css
         FROM themes
         WHERE lower(slug) = lower(?1)",
    )?;
    stmt.query_row(params![slug], map_theme)
        .optional()
        .map_err(Into::into)
}

/// Insert or update the built-in theme rows.
///
/// # Errors
/// Returns an error if any database write fails.
pub fn upsert_builtin_themes(conn: &rusqlite::Connection) -> Result<()> {
    let enabled_builtin_slugs = configured_enabled_builtin_slugs();

    for theme in builtin_theme_rows(&enabled_builtin_slugs) {
        conn.execute(
            "INSERT INTO themes (slug, display_name, description, swatch_hex, enabled, sort_order, is_builtin, custom_css)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, '')
             ON CONFLICT(slug) DO UPDATE SET
                display_name = excluded.display_name,
                description = excluded.description,
                swatch_hex = excluded.swatch_hex,
                sort_order = excluded.sort_order",
            params![
                theme.slug,
                theme.display_name,
                theme.description,
                theme.swatch_hex,
                i32::from(theme.enabled),
                theme.sort_order,
            ],
        )
        .context("Failed to upsert built-in theme")?;
    }
    Ok(())
}

/// Create a custom theme row.
///
/// # Errors
/// Returns an error if the insert fails.
pub fn create_custom_theme(
    conn: &rusqlite::Connection,
    slug: &str,
    display_name: &str,
    description: &str,
    swatch_hex: &str,
    custom_css: &str,
    enabled: bool,
) -> Result<()> {
    let next_sort_order: i64 = conn.query_row(
        "SELECT COALESCE(MAX(sort_order) + 10, 1000) FROM themes",
        [],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO themes (slug, display_name, description, swatch_hex, enabled, sort_order, is_builtin, custom_css)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
        params![
            slug,
            display_name,
            description,
            swatch_hex,
            i32::from(enabled),
            next_sort_order,
            custom_css,
        ],
    )
    .context("Failed to create custom theme")?;
    Ok(())
}

/// Update a theme and migrate any references if the slug changes.
///
/// # Errors
/// Returns an error if the theme is missing or the update fails.
#[allow(clippy::too_many_arguments)]
pub fn update_theme(
    conn: &rusqlite::Connection,
    existing_slug: &str,
    new_slug: &str,
    display_name: &str,
    description: &str,
    swatch_hex: &str,
    enabled: bool,
    custom_css: Option<&str>,
) -> Result<()> {
    let current = get_theme(conn, existing_slug)?
        .ok_or_else(|| anyhow::anyhow!("Theme {existing_slug} not found"))?;
    let css_to_save = if current.is_builtin {
        current.custom_css
    } else {
        custom_css.unwrap_or("").to_string()
    };
    conn.execute(
        "UPDATE themes
         SET slug = ?1,
             display_name = ?2,
             description = ?3,
             swatch_hex = ?4,
             enabled = ?5,
             custom_css = ?6
         WHERE slug = ?7",
        params![
            new_slug,
            display_name,
            description,
            swatch_hex,
            i32::from(enabled),
            css_to_save,
            existing_slug,
        ],
    )
    .context("Failed to update theme")?;
    if existing_slug != new_slug {
        conn.execute(
            "UPDATE boards SET default_theme = ?1 WHERE lower(default_theme) = lower(?2)",
            params![new_slug, existing_slug],
        )
        .context("Failed to update board theme references")?;
        conn.execute(
            "UPDATE site_settings SET value = ?1
             WHERE key = 'default_theme' AND lower(value) = lower(?2)",
            params![new_slug, existing_slug],
        )
        .context("Failed to update site default theme reference")?;
    }
    Ok(())
}

/// Delete a non-built-in theme.
///
/// # Errors
/// Returns an error if the theme is missing or cannot be deleted.
pub fn delete_custom_theme(conn: &rusqlite::Connection, slug: &str) -> Result<()> {
    let theme = get_theme(conn, slug)?.ok_or_else(|| anyhow::anyhow!("Theme not found"))?;
    if theme.is_builtin {
        anyhow::bail!("Built-in themes cannot be deleted");
    }
    conn.execute("DELETE FROM themes WHERE slug = ?1", params![slug])?;
    conn.execute(
        "UPDATE boards SET default_theme = '' WHERE lower(default_theme) = lower(?1)",
        params![slug],
    )?;
    conn.execute(
        "UPDATE site_settings SET value = ?2
         WHERE key = 'default_theme' AND lower(value) = lower(?1)",
        params![slug, crate::theme::HARD_DEFAULT_THEME],
    )?;
    Ok(())
}

#[must_use]
pub fn sanitize_theme_slug(slug: &str) -> String {
    slug.trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase()
}

#[must_use]
pub fn sanitize_theme_name(name: &str) -> String {
    let value = name.trim().chars().take(64).collect::<String>();
    if value.is_empty() {
        "Untitled Theme".to_string()
    } else {
        value
    }
}

#[must_use]
pub fn sanitize_theme_description(description: &str) -> String {
    description.trim().chars().take(256).collect()
}

#[must_use]
pub fn sanitize_theme_css(css: &str) -> String {
    css.trim().chars().take(32_000).collect()
}

#[must_use]
pub fn sanitize_theme_swatch(swatch: &str) -> String {
    let trimmed = swatch.trim();
    if trimmed.len() == 7
        && trimmed.starts_with('#')
        && trimmed.chars().skip(1).all(|ch| ch.is_ascii_hexdigit())
    {
        trimmed.to_ascii_lowercase()
    } else {
        "#888888".to_string()
    }
}

/// Render the stylesheet body for a theme, if it is enabled.
///
/// # Errors
/// Returns an error if the query fails.
pub fn theme_css_response(conn: &rusqlite::Connection, slug: &str) -> Result<Option<String>> {
    let Some(theme) = get_theme(conn, slug)? else {
        return Ok(None);
    };
    if !theme.enabled {
        return Ok(None);
    }
    let css = if theme.custom_css.contains('{') {
        theme.custom_css
    } else if theme.custom_css.trim().is_empty() {
        String::new()
    } else {
        format!(
            "html[data-theme=\"{slug}\"] {{\n{css}\n}}",
            slug = theme.slug,
            css = theme.custom_css
        )
    };
    Ok(Some(css))
}

#[must_use]
pub fn is_builtin_slug(slug: &str) -> bool {
    builtin_theme(slug).is_some()
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_default_builtin_list_is_upgraded_with_new_featured_themes() {
        let enabled_builtin_slugs = super::configured_enabled_builtin_slugs();

        assert!(enabled_builtin_slugs.iter().any(|slug| slug == "blue-sky"));
        assert!(enabled_builtin_slugs
            .iter()
            .any(|slug| slug == "deep-orbit"));
    }

    #[test]
    fn load_themes_keeps_featured_builtins_first() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let themes = super::load_themes(&conn).expect("load themes");
        let builtins = themes
            .iter()
            .filter(|theme| theme.is_builtin && theme.enabled)
            .map(|theme| theme.slug.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            builtins,
            vec![
                "forest",
                "blue-sky",
                "deep-orbit",
                "terminal",
                "dorfic",
                "chanclassic",
                "aero",
                "neoncubicle",
                "fluorogrid",
            ]
        );
    }

    #[test]
    fn load_themes_includes_new_builtin_theme_metadata() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        let themes = super::load_themes(&conn).expect("load themes");

        let blue_sky = themes
            .iter()
            .find(|theme| theme.slug == "blue-sky")
            .expect("blue sky theme");
        assert_eq!(blue_sky.display_name, "Blue Sky");
        assert!(blue_sky.enabled);

        let deep_orbit = themes
            .iter()
            .find(|theme| theme.slug == "deep-orbit")
            .expect("deep orbit theme");
        assert_eq!(deep_orbit.display_name, "Deep Orbit");
        assert!(deep_orbit.enabled);
    }
}
