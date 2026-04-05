use crate::{
    config::CONFIG,
    models::Theme,
    theme::{builtin_theme, builtin_theme_rows},
};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

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

pub fn sync_live_theme_state(conn: &rusqlite::Connection) -> Result<()> {
    crate::templates::set_live_default_theme(&crate::db::get_default_user_theme(conn));
    crate::templates::set_live_themes(load_themes(conn)?);
    Ok(())
}

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

pub fn upsert_builtin_themes(conn: &rusqlite::Connection) -> Result<()> {
    let enabled_builtin_slugs = CONFIG
        .initial_enabled_builtin_themes
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();

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

pub fn sanitize_theme_slug(slug: &str) -> String {
    slug.trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase()
}

pub fn sanitize_theme_name(name: &str) -> String {
    let value = name.trim().chars().take(64).collect::<String>();
    if value.is_empty() {
        "Untitled Theme".to_string()
    } else {
        value
    }
}

pub fn sanitize_theme_description(description: &str) -> String {
    description.trim().chars().take(256).collect()
}

pub fn sanitize_theme_css(css: &str) -> String {
    css.trim().chars().take(32_000).collect()
}

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

pub fn is_builtin_slug(slug: &str) -> bool {
    builtin_theme(slug).is_some()
}
