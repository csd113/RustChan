use serde::{Deserialize, Serialize};

const BUILDER_DATA_PROPERTY: &str = "--rustchan-builder-data";
const BUILDER_DATA_PREFIX: &str = "\"";
const BUILDER_DATA_SUFFIX: &str = "\"";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeDensity {
    Cozy,
    Compact,
}

impl ThemeDensity {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cozy" => Some(Self::Cozy),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeFontFamily {
    Sans,
    Serif,
    Mono,
}

impl ThemeFontFamily {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system_sans" => Some(Self::Sans),
            "system_serif" => Some(Self::Serif),
            "system_mono" => Some(Self::Mono),
            _ => None,
        }
    }

    #[must_use]
    pub const fn css_stack(self) -> &'static str {
        match self {
            Self::Sans => {
                "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
            }
            Self::Serif => "Georgia, 'Times New Roman', Times, 'Noto Serif', serif",
            Self::Mono => "'SFMono-Regular', Consolas, 'Liberation Mono', 'Courier New', monospace",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeBuilderConfig {
    pub base_preset: String,
    pub background_color: String,
    pub panel_color: String,
    pub card_color: String,
    pub op_card_color: String,
    pub text_color: String,
    pub muted_text_color: String,
    pub link_color: String,
    pub link_hover_color: String,
    pub border_color: String,
    pub input_background_color: String,
    pub input_text_color: String,
    pub input_border_color: String,
    pub button_background_color: String,
    pub button_text_color: String,
    pub button_border_color: String,
    pub button_hover_color: String,
    pub header_background_color: String,
    pub header_text_color: String,
    pub header_border_color: String,
    pub quote_color: String,
    pub meta_text_color: String,
    pub success_color: String,
    pub danger_color: String,
    pub border_radius_px: u8,
    pub density: ThemeDensity,
    pub font_family: ThemeFontFamily,
    pub advanced_css: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ThemeBuilderPreset {
    pub slug: &'static str,
    pub label: &'static str,
}

pub const BUILDER_PRESETS: &[ThemeBuilderPreset] = &[
    ThemeBuilderPreset {
        slug: "forest",
        label: "Forest",
    },
    ThemeBuilderPreset {
        slug: "blue-sky",
        label: "Blue Sky",
    },
    ThemeBuilderPreset {
        slug: "deep-orbit",
        label: "Deep Orbit",
    },
    ThemeBuilderPreset {
        slug: "terminal",
        label: "Terminal",
    },
    ThemeBuilderPreset {
        slug: "dorfic",
        label: "DORFic",
    },
    ThemeBuilderPreset {
        slug: "chanclassic",
        label: "ChanClassic",
    },
    ThemeBuilderPreset {
        slug: "aero",
        label: "Frutiger Aero",
    },
    ThemeBuilderPreset {
        slug: "neoncubicle",
        label: "NeonCubicle",
    },
    ThemeBuilderPreset {
        slug: "fluorogrid",
        label: "FluoroGrid",
    },
];

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn builder_defaults_for_preset(preset_slug: &str) -> ThemeBuilderConfig {
    match preset_slug {
        "blue-sky" => ThemeBuilderConfig {
            base_preset: "blue-sky".to_string(),
            background_color: "#dfeaf2".to_string(),
            panel_color: "#f8fbfe".to_string(),
            card_color: "#f3f7fb".to_string(),
            op_card_color: "#edf4fa".to_string(),
            text_color: "#223446".to_string(),
            muted_text_color: "#61758b".to_string(),
            link_color: "#356d9b".to_string(),
            link_hover_color: "#204f7a".to_string(),
            border_color: "#bdd1e3".to_string(),
            input_background_color: "#ffffff".to_string(),
            input_text_color: "#223446".to_string(),
            input_border_color: "#9fb8cc".to_string(),
            button_background_color: "#5d8fb5".to_string(),
            button_text_color: "#f8fcff".to_string(),
            button_border_color: "#4d7696".to_string(),
            button_hover_color: "#476f92".to_string(),
            header_background_color: "#edf5fb".to_string(),
            header_text_color: "#1f3344".to_string(),
            header_border_color: "#9eb8ce".to_string(),
            quote_color: "#4f7f4e".to_string(),
            meta_text_color: "#61758b".to_string(),
            success_color: "#4c8a67".to_string(),
            danger_color: "#b85d69".to_string(),
            border_radius_px: 10,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
        "deep-orbit" => ThemeBuilderConfig {
            base_preset: "deep-orbit".to_string(),
            background_color: "#161b26".to_string(),
            panel_color: "#202636".to_string(),
            card_color: "#252d40".to_string(),
            op_card_color: "#2a3347".to_string(),
            text_color: "#dde3ef".to_string(),
            muted_text_color: "#99a5ba".to_string(),
            link_color: "#8dc6cd".to_string(),
            link_hover_color: "#badbe5".to_string(),
            border_color: "#3d485f".to_string(),
            input_background_color: "#171d2a".to_string(),
            input_text_color: "#dde3ef".to_string(),
            input_border_color: "#53617d".to_string(),
            button_background_color: "#64739d".to_string(),
            button_text_color: "#f4f7fb".to_string(),
            button_border_color: "#54607f".to_string(),
            button_hover_color: "#7381ab".to_string(),
            header_background_color: "#1b2130".to_string(),
            header_text_color: "#eef3fb".to_string(),
            header_border_color: "#56637e".to_string(),
            quote_color: "#9fcb97".to_string(),
            meta_text_color: "#aab6cb".to_string(),
            success_color: "#6eb090".to_string(),
            danger_color: "#c87d8f".to_string(),
            border_radius_px: 12,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
        "terminal" => ThemeBuilderConfig {
            base_preset: "terminal".to_string(),
            background_color: "#050505".to_string(),
            panel_color: "#0f1210".to_string(),
            card_color: "#101612".to_string(),
            op_card_color: "#121a14".to_string(),
            text_color: "#c7e7c7".to_string(),
            muted_text_color: "#89ae89".to_string(),
            link_color: "#26d85c".to_string(),
            link_hover_color: "#cffff0".to_string(),
            border_color: "#224228".to_string(),
            input_background_color: "#060c06".to_string(),
            input_text_color: "#c7e7c7".to_string(),
            input_border_color: "#1f4a27".to_string(),
            button_background_color: "#103c1d".to_string(),
            button_text_color: "#d9f7dd".to_string(),
            button_border_color: "#2d7a44".to_string(),
            button_hover_color: "#17552a".to_string(),
            header_background_color: "#0f1210".to_string(),
            header_text_color: "#d4f0d4".to_string(),
            header_border_color: "#17b84a".to_string(),
            quote_color: "#8fd66d".to_string(),
            meta_text_color: "#8fbd93".to_string(),
            success_color: "#26d85c".to_string(),
            danger_color: "#ff4c68".to_string(),
            border_radius_px: 0,
            density: ThemeDensity::Compact,
            font_family: ThemeFontFamily::Mono,
            advanced_css: String::new(),
        },
        "dorfic" => ThemeBuilderConfig {
            base_preset: "dorfic".to_string(),
            background_color: "#17110b".to_string(),
            panel_color: "#2a1d11".to_string(),
            card_color: "#332215".to_string(),
            op_card_color: "#3a2718".to_string(),
            text_color: "#ecd5a8".to_string(),
            muted_text_color: "#b6965f".to_string(),
            link_color: "#d9a755".to_string(),
            link_hover_color: "#ffcc66".to_string(),
            border_color: "#694726".to_string(),
            input_background_color: "#20150d".to_string(),
            input_text_color: "#f0ddb5".to_string(),
            input_border_color: "#7d5530".to_string(),
            button_background_color: "#5b3818".to_string(),
            button_text_color: "#ffe1aa".to_string(),
            button_border_color: "#8c602f".to_string(),
            button_hover_color: "#714821".to_string(),
            header_background_color: "#26190f".to_string(),
            header_text_color: "#f6e3bd".to_string(),
            header_border_color: "#a1682d".to_string(),
            quote_color: "#d3b46b".to_string(),
            meta_text_color: "#c3a06f".to_string(),
            success_color: "#d3a04a".to_string(),
            danger_color: "#d97d5d".to_string(),
            border_radius_px: 4,
            density: ThemeDensity::Compact,
            font_family: ThemeFontFamily::Mono,
            advanced_css: String::new(),
        },
        "chanclassic" => ThemeBuilderConfig {
            base_preset: "chanclassic".to_string(),
            background_color: "#eef2ff".to_string(),
            panel_color: "#ffffff".to_string(),
            card_color: "#f7f8ff".to_string(),
            op_card_color: "#f4f4fb".to_string(),
            text_color: "#1c1c2b".to_string(),
            muted_text_color: "#62627a".to_string(),
            link_color: "#8b0000".to_string(),
            link_hover_color: "#b20000".to_string(),
            border_color: "#c4c9df".to_string(),
            input_background_color: "#ffffff".to_string(),
            input_text_color: "#1f1f30".to_string(),
            input_border_color: "#acb4d0".to_string(),
            button_background_color: "#e8e9f7".to_string(),
            button_text_color: "#2c2b44".to_string(),
            button_border_color: "#b1b6cb".to_string(),
            button_hover_color: "#d9dbeb".to_string(),
            header_background_color: "#d8daf0".to_string(),
            header_text_color: "#24243a".to_string(),
            header_border_color: "#aab2d3".to_string(),
            quote_color: "#789922".to_string(),
            meta_text_color: "#62627a".to_string(),
            success_color: "#6d8e24".to_string(),
            danger_color: "#b54747".to_string(),
            border_radius_px: 3,
            density: ThemeDensity::Compact,
            font_family: ThemeFontFamily::Serif,
            advanced_css: String::new(),
        },
        "aero" => ThemeBuilderConfig {
            base_preset: "aero".to_string(),
            background_color: "#d9eef8".to_string(),
            panel_color: "#ffffff".to_string(),
            card_color: "#f8fdff".to_string(),
            op_card_color: "#eef8fd".to_string(),
            text_color: "#234156".to_string(),
            muted_text_color: "#5f7e93".to_string(),
            link_color: "#1a6fa8".to_string(),
            link_hover_color: "#0d5a8a".to_string(),
            border_color: "#a3c8de".to_string(),
            input_background_color: "#ffffff".to_string(),
            input_text_color: "#234156".to_string(),
            input_border_color: "#94b7cc".to_string(),
            button_background_color: "#dceefb".to_string(),
            button_text_color: "#20435b".to_string(),
            button_border_color: "#8eb5d0".to_string(),
            button_hover_color: "#cfe6f7".to_string(),
            header_background_color: "#f4fbff".to_string(),
            header_text_color: "#21465f".to_string(),
            header_border_color: "#8eb7d5".to_string(),
            quote_color: "#4a8f59".to_string(),
            meta_text_color: "#64849b".to_string(),
            success_color: "#4a9f7a".to_string(),
            danger_color: "#c76272".to_string(),
            border_radius_px: 12,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
        "neoncubicle" => ThemeBuilderConfig {
            base_preset: "neoncubicle".to_string(),
            background_color: "#17141b".to_string(),
            panel_color: "#241f2b".to_string(),
            card_color: "#2c2431".to_string(),
            op_card_color: "#32283a".to_string(),
            text_color: "#efe6ef".to_string(),
            muted_text_color: "#ac96a9".to_string(),
            link_color: "#db63b4".to_string(),
            link_hover_color: "#ff9fdc".to_string(),
            border_color: "#5f4a63".to_string(),
            input_background_color: "#1a151f".to_string(),
            input_text_color: "#f6eef7".to_string(),
            input_border_color: "#6e5470".to_string(),
            button_background_color: "#55314b".to_string(),
            button_text_color: "#ffeefe".to_string(),
            button_border_color: "#8a4e78".to_string(),
            button_hover_color: "#683d5b".to_string(),
            header_background_color: "#211b27".to_string(),
            header_text_color: "#f7eef7".to_string(),
            header_border_color: "#985787".to_string(),
            quote_color: "#a4d283".to_string(),
            meta_text_color: "#bb9fb4".to_string(),
            success_color: "#72bb8c".to_string(),
            danger_color: "#d97b9a".to_string(),
            border_radius_px: 8,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
        "fluorogrid" => ThemeBuilderConfig {
            base_preset: "fluorogrid".to_string(),
            background_color: "#f4f6fb".to_string(),
            panel_color: "#ffffff".to_string(),
            card_color: "#fefefe".to_string(),
            op_card_color: "#f9f7ff".to_string(),
            text_color: "#1f2430".to_string(),
            muted_text_color: "#5f6473".to_string(),
            link_color: "#7a38aa".to_string(),
            link_hover_color: "#4b9bc1".to_string(),
            border_color: "#cfd4ea".to_string(),
            input_background_color: "#ffffff".to_string(),
            input_text_color: "#1f2430".to_string(),
            input_border_color: "#b9bfd9".to_string(),
            button_background_color: "#f0ebff".to_string(),
            button_text_color: "#31205a".to_string(),
            button_border_color: "#b898df".to_string(),
            button_hover_color: "#e6dcff".to_string(),
            header_background_color: "#ffffff".to_string(),
            header_text_color: "#2d2a46".to_string(),
            header_border_color: "#a5afda".to_string(),
            quote_color: "#2b9e66".to_string(),
            meta_text_color: "#6d7280".to_string(),
            success_color: "#27a26b".to_string(),
            danger_color: "#d05f79".to_string(),
            border_radius_px: 10,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
        _ => ThemeBuilderConfig {
            base_preset: "forest".to_string(),
            background_color: "#141914".to_string(),
            panel_color: "#1e281d".to_string(),
            card_color: "#243022".to_string(),
            op_card_color: "#2a3827".to_string(),
            text_color: "#e5e6d8".to_string(),
            muted_text_color: "#b0b796".to_string(),
            link_color: "#7ab84e".to_string(),
            link_hover_color: "#a8d77b".to_string(),
            border_color: "#4c6441".to_string(),
            input_background_color: "#161d15".to_string(),
            input_text_color: "#eceedd".to_string(),
            input_border_color: "#657e57".to_string(),
            button_background_color: "#466735".to_string(),
            button_text_color: "#f4f5e8".to_string(),
            button_border_color: "#6d9652".to_string(),
            button_hover_color: "#577f42".to_string(),
            header_background_color: "#1b2419".to_string(),
            header_text_color: "#f0efdd".to_string(),
            header_border_color: "#6a8c4f".to_string(),
            quote_color: "#98c86e".to_string(),
            meta_text_color: "#c2c6ab".to_string(),
            success_color: "#7eb25b".to_string(),
            danger_color: "#c46f6f".to_string(),
            border_radius_px: 8,
            density: ThemeDensity::Cozy,
            font_family: ThemeFontFamily::Sans,
            advanced_css: String::new(),
        },
    }
}

#[must_use]
pub fn builder_marker_hex(config: &ThemeBuilderConfig) -> String {
    let json = serde_json::to_vec(config).unwrap_or_default();
    hex::encode(json)
}

#[must_use]
pub fn parse_builder_config(css: &str) -> Option<ThemeBuilderConfig> {
    let marker_index = css.find(BUILDER_DATA_PROPERTY)?;
    let after_marker = &css[marker_index + BUILDER_DATA_PROPERTY.len()..];
    let start_quote = after_marker.find(BUILDER_DATA_PREFIX)?;
    let rest = &after_marker[start_quote + BUILDER_DATA_PREFIX.len()..];
    let end_quote = rest.find(BUILDER_DATA_SUFFIX)?;
    let hex_payload = &rest[..end_quote];
    let bytes = hex::decode(hex_payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn build_theme_css(slug: &str, config: &ThemeBuilderConfig) -> String {
    let density_gap = match config.density {
        ThemeDensity::Cozy => "0.55rem",
        ThemeDensity::Compact => "0.35rem",
    };
    let density_padding = match config.density {
        ThemeDensity::Cozy => "0.75rem",
        ThemeDensity::Compact => "0.45rem",
    };
    let density_line_height = match config.density {
        ThemeDensity::Cozy => "1.62",
        ThemeDensity::Compact => "1.48",
    };
    let radius = config.border_radius_px;
    let advanced_css = config.advanced_css.trim();
    let advanced_block = if advanced_css.is_empty() {
        String::new()
    } else {
        format!("\n\n/* Optional advanced overrides */\n{advanced_css}\n")
    };

    format!(
        r#"html[data-theme="{slug}"] {{
  {marker_property}: "{marker_hex}";
  color-scheme: dark;
  --bg: {background};
  --bg-panel: {panel};
  --bg-post: {card};
  --bg-op: {op_card};
  --bg-input: {input_background};
  --border: {border};
  --border-glow: {link_hover};
  --green: {link};
  --green-dim: {muted_text};
  --green-bright: {link_hover};
  --green-pale: {text};
  --amber: {success};
  --red: {danger};
  --gray: {border};
  --gray-light: {meta_text};
  --text: {text};
  --text-dim: {muted_text};
  --post-highlight-outline: {link};
  --post-highlight-bg: rgba(255, 255, 255, 0.06);
  --font: {font_stack};
  --font-display: {font_stack};
}}

html[data-theme="{slug}"] body {{
  background: var(--bg);
  color: var(--text);
  line-height: {density_line_height};
  background-image:
    radial-gradient(circle at top, rgba(255,255,255,0.04), transparent 34%),
    linear-gradient(180deg, rgba(255,255,255,0.03), transparent 48%);
}}

html[data-theme="{slug}"] .site-header {{
  background: {header_background};
  border-bottom: 1px solid {header_border};
  color: {header_text};
  box-shadow: none;
}}

html[data-theme="{slug}"] .site-header::before {{
  color: {link_hover};
}}

html[data-theme="{slug}"] .site-header a,
html[data-theme="{slug}"] .board-list,
html[data-theme="{slug}"] .board-list a,
html[data-theme="{slug}"] .home-btn,
html[data-theme="{slug}"] .admin-header-link {{
  color: {header_text};
}}

html[data-theme="{slug}"] .board-list a:hover,
html[data-theme="{slug}"] .home-btn:hover,
html[data-theme="{slug}"] .admin-header-link:hover {{
  color: {link_hover};
}}

html[data-theme="{slug}"] a {{
  color: {link};
}}

html[data-theme="{slug}"] a:hover {{
  color: {link_hover};
  text-shadow: none;
}}

html[data-theme="{slug}"] .page-box,
html[data-theme="{slug}"] .board-card,
html[data-theme="{slug}"] .catalog-item,
html[data-theme="{slug}"] .admin-section {{
  background: var(--bg-panel);
  border-color: var(--border);
  border-radius: {radius}px;
}}

html[data-theme="{slug}"] .post-form-container,
html[data-theme="{slug}"] .reply {{
  background: var(--bg-post);
  border-color: var(--border);
  border-radius: {radius}px;
}}

html[data-theme="{slug}"] .op {{
  background: var(--bg-op);
  border-color: var(--border);
  border-radius: {radius}px;
}}

html[data-theme="{slug}"] .post-meta,
html[data-theme="{slug}"] .post-time,
html[data-theme="{slug}"] .post-num,
html[data-theme="{slug}"] .post-ref,
html[data-theme="{slug}"] .backrefs .backref {{
  color: {meta_text};
}}

html[data-theme="{slug}"] .post-body .quote {{
  color: {quote};
}}

html[data-theme="{slug}"] input[type="text"],
html[data-theme="{slug}"] input[type="password"],
html[data-theme="{slug}"] input[type="search"],
html[data-theme="{slug}"] input[type="number"],
html[data-theme="{slug}"] input[type="email"],
html[data-theme="{slug}"] input[type="url"],
html[data-theme="{slug}"] select,
html[data-theme="{slug}"] textarea {{
  background: {input_background};
  color: {input_text};
  border: 1px solid {input_border};
  border-radius: {radius}px;
}}

html[data-theme="{slug}"] input::placeholder,
html[data-theme="{slug}"] textarea::placeholder {{
  color: {muted_text};
}}

html[data-theme="{slug}"] button,
html[data-theme="{slug}"] .btn,
html[data-theme="{slug}"] input[type="submit"] {{
  background: {button_background};
  color: {button_text};
  border: 1px solid {button_border};
  border-radius: {radius}px;
}}

html[data-theme="{slug}"] button:hover,
html[data-theme="{slug}"] .btn:hover,
html[data-theme="{slug}"] input[type="submit"]:hover {{
  background: {button_hover};
  color: {button_text};
}}

html[data-theme="{slug}"] .reply,
html[data-theme="{slug}"] .op,
html[data-theme="{slug}"] .page-box,
html[data-theme="{slug}"] .board-card,
html[data-theme="{slug}"] .catalog-item,
html[data-theme="{slug}"] .post-form-container {{
  padding: {density_padding};
}}

html[data-theme="{slug}"] .post-meta {{
  gap: {density_gap};
}}

html[data-theme="{slug}"] .admin-flash.flash-ok {{
  border-color: {success};
  color: {success};
}}

html[data-theme="{slug}"] .admin-flash.flash-error,
html[data-theme="{slug}"] .error {{
  border-color: {danger};
  color: {danger};
}}{advanced_block}"#,
        slug = slug,
        marker_property = BUILDER_DATA_PROPERTY,
        marker_hex = builder_marker_hex(config),
        background = config.background_color,
        panel = config.panel_color,
        card = config.card_color,
        op_card = config.op_card_color,
        input_background = config.input_background_color,
        border = config.border_color,
        link = config.link_color,
        link_hover = config.link_hover_color,
        muted_text = config.muted_text_color,
        text = config.text_color,
        success = config.success_color,
        danger = config.danger_color,
        font_stack = config.font_family.css_stack(),
        density_line_height = density_line_height,
        header_background = config.header_background_color,
        header_border = config.header_border_color,
        header_text = config.header_text_color,
        radius = radius,
        meta_text = config.meta_text_color,
        quote = config.quote_color,
        input_text = config.input_text_color,
        input_border = config.input_border_color,
        button_background = config.button_background_color,
        button_text = config.button_text_color,
        button_border = config.button_border_color,
        button_hover = config.button_hover_color,
        density_padding = density_padding,
        density_gap = density_gap,
        advanced_block = advanced_block,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        build_theme_css, builder_defaults_for_preset, builder_marker_hex, parse_builder_config,
        ThemeDensity, ThemeFontFamily,
    };

    #[test]
    fn builder_marker_round_trips_config() {
        let mut config = builder_defaults_for_preset("forest");
        config.advanced_css = "html[data-theme=\"forest\"] .subject { font-style: italic; }".into();
        config.density = ThemeDensity::Compact;
        config.font_family = ThemeFontFamily::Mono;

        let css = build_theme_css("forest-copy", &config);
        let parsed = parse_builder_config(&css).expect("builder config");

        assert_eq!(parsed, config);
        assert!(css.contains(&builder_marker_hex(&config)));
    }
}
