//! Registry of themes bundled with `RustChan`.

use crate::models::Theme;

/// Metadata for a theme shipped with `RustChan`.
#[derive(Debug)]
pub struct BuiltinTheme {
    /// Stable identifier used in configuration and URLs.
    pub slug: &'static str,
    /// Human-readable theme name.
    pub display_name: &'static str,
    /// Short description shown in theme selectors.
    pub description: &'static str,
    /// Representative CSS color for theme previews.
    pub swatch_hex: &'static str,
    /// Display order relative to other built-in themes.
    pub sort_order: i64,
}

/// Theme used when no configured default can be resolved.
pub const HARD_DEFAULT_THEME: &str = "forest";

/// Metadata for every theme bundled with `RustChan`.
pub const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        slug: "forest",
        display_name: "Forest",
        description: "Earthy dark woodland palette with parchment accents.",
        swatch_hex: "#6fa84a",
        sort_order: 10,
    },
    BuiltinTheme {
        slug: "blue-sky",
        display_name: "Blue Sky",
        description:
            "Soft hazy daylight palette with cloud, sky blue, stone, and gentle accent tones.",
        swatch_hex: "#6f9fbd",
        sort_order: 20,
    },
    BuiltinTheme {
        slug: "deep-orbit",
        display_name: "Deep Orbit",
        description:
            "Cozy charcoal-indigo night palette with moon-gray text and soft teal-lavender accents.",
        swatch_hex: "#88a8a2",
        sort_order: 30,
    },
    BuiltinTheme {
        slug: "terminal",
        display_name: "Terminal",
        description: "CRT-style dark green terminal theme.",
        swatch_hex: "#00ff41",
        sort_order: 40,
    },
    BuiltinTheme {
        slug: "dorfic",
        display_name: "DORFic",
        description: "Warm amber sci-fi terminal with darker chrome.",
        swatch_hex: "#ffcc66",
        sort_order: 50,
    },
    BuiltinTheme {
        slug: "chanclassic",
        display_name: "ChanClassic",
        description: "Light beige classic imageboard styling.",
        swatch_hex: "#800000",
        sort_order: 60,
    },
    BuiltinTheme {
        slug: "aero",
        display_name: "Frutiger Aero",
        description: "Bright glossy blues with soft rounded chrome.",
        swatch_hex: "#6aaed6",
        sort_order: 70,
    },
    BuiltinTheme {
        slug: "neoncubicle",
        display_name: "NeonCubicle",
        description: "Soft office-futurist magenta and gray palette.",
        swatch_hex: "#b03888",
        sort_order: 80,
    },
    BuiltinTheme {
        slug: "fluorogrid",
        display_name: "FluoroGrid",
        description: "Light retro-futurist grid with bright accent colors.",
        swatch_hex: "#8833aa",
        sort_order: 90,
    },
];

#[must_use]
/// Find a built-in theme by slug, ignoring ASCII case and surrounding space.
pub fn builtin_theme(slug: &str) -> Option<&'static BuiltinTheme> {
    BUILTIN_THEMES
        .iter()
        .find(|theme| theme.slug.eq_ignore_ascii_case(slug.trim()))
}

#[must_use]
/// Return the stable slugs of all built-in themes in display order.
pub fn builtin_theme_slugs() -> Vec<&'static str> {
    BUILTIN_THEMES.iter().map(|theme| theme.slug).collect()
}

#[must_use]
/// Build database-compatible theme rows with configured enablement state.
pub fn builtin_theme_rows(enabled_slugs: &[String]) -> Vec<Theme> {
    BUILTIN_THEMES
        .iter()
        .map(|theme| Theme {
            slug: theme.slug.to_owned(),
            display_name: theme.display_name.to_owned(),
            description: theme.description.to_owned(),
            swatch_hex: theme.swatch_hex.to_owned(),
            enabled: enabled_slugs
                .iter()
                .any(|slug| slug.eq_ignore_ascii_case(theme.slug)),
            sort_order: theme.sort_order,
            is_builtin: true,
            custom_css: String::new(),
        })
        .collect()
}
