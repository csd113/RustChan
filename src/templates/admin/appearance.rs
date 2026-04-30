use super::{
    escape_html, render_banner_asset_list, render_banner_upload_form, render_board_appearance_card,
    AdminPanelViewModel,
};
use crate::theme_builder::{
    builder_defaults_for_preset, parse_builder_config, ThemeBuilderConfig, ThemeDensity,
    ThemeFontFamily, BUILDER_PRESETS,
};
use std::fmt::Write;

pub(super) fn render_site_settings(view: &AdminPanelViewModel<'_>) -> String {
    let global_favicon_exists = crate::favicon::global_has_custom_favicon();
    let global_favicon_version =
        crate::favicon::favicon_version_for_board(None).unwrap_or_default();
    let global_favicon_preview = if global_favicon_exists {
        format!(
            r#"<img class="favicon-inline-preview" src="/favicon-32x32.png?v={version}" alt="global favicon">"#,
            version = escape_html(&global_favicon_version)
        )
    } else {
        String::new()
    };
    let global_favicon_label = if global_favicon_exists {
        "replace favicon"
    } else {
        "global favicon"
    };
    let global_favicon_button = if global_favicon_exists {
        "replace"
    } else {
        "upload"
    };
    let global_favicon_status = if global_favicon_exists {
        "Custom global favicon is active and stored under rustchan-data/runtime/favicon/."
    } else {
        "No custom global favicon uploaded yet."
    };

    render_admin_site_settings_section(
        view.csrf_token,
        view.appearance.site_name,
        view.appearance.site_subtitle,
        view.appearance.homepage_new_thread_badges_enabled,
        view.appearance.thread_new_reply_badges_enabled,
        &render_enabled_theme_options(view),
        &global_favicon_preview,
        global_favicon_label,
        global_favicon_button,
        global_favicon_status,
    )
}

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let theme_catalog_open_attr = if view.open_section == Some("theme-catalog") {
        " open"
    } else {
        ""
    };
    let banner_settings_open_attr = if matches!(
        view.open_section,
        Some("board-banners" | "global-banners" | "home-banners")
    ) || view
        .open_section
        .is_some_and(|section| section.starts_with("board-appearance-"))
    {
        " open"
    } else {
        ""
    };
    let banner_external_links_enabled_checked = if view.appearance.banner_external_links_enabled {
        " checked"
    } else {
        ""
    };
    let global_banner_upload_form = render_banner_upload_form(
        "/admin/site/banner",
        view.csrf_token,
        None,
        view.boards,
        true,
        "upload global banner",
    );
    let home_banner_upload_form = render_banner_upload_form(
        "/admin/home/banner",
        view.csrf_token,
        None,
        view.boards,
        false,
        "upload home banner",
    );
    let global_banner_rows = render_banner_asset_list(
        view.appearance.global_banners,
        view.csrf_token,
        view.boards,
        true,
        "No global board banners uploaded yet.",
    );
    let home_banner_rows = render_banner_asset_list(
        view.appearance.home_banners,
        view.csrf_token,
        view.boards,
        false,
        "No home page banners uploaded yet.",
    );
    let (builtin_theme_cards, custom_theme_cards) = render_theme_cards(view);
    let custom_theme_cards_or_empty = if custom_theme_cards.is_empty() {
        r#"<div class="theme-empty-state">No custom themes yet. Create one above and it will show up here.</div>"#.to_string()
    } else {
        custom_theme_cards
    };

    render_admin_appearance_section(
        view.csrf_token,
        view.appearance.banner_rotation_interval_minutes,
        banner_external_links_enabled_checked,
        banner_settings_open_attr,
        &global_banner_upload_form,
        &global_banner_rows,
        &home_banner_upload_form,
        &home_banner_rows,
        &render_board_appearance_cards(view),
        theme_catalog_open_attr,
        &builtin_theme_cards,
        &custom_theme_cards_or_empty,
    )
}

fn render_enabled_theme_options(view: &AdminPanelViewModel<'_>) -> String {
    let mut enabled_theme_options = String::new();
    for theme in view.appearance.themes.iter().filter(|theme| theme.enabled) {
        let _ = write!(
            enabled_theme_options,
            r#"<option value="{slug}"{selected}>{label}</option>"#,
            slug = escape_html(&theme.slug),
            selected = if theme.slug == view.appearance.default_theme {
                " selected"
            } else {
                ""
            },
            label = escape_html(&theme.display_name)
        );
    }
    enabled_theme_options
}

fn render_board_appearance_cards(view: &AdminPanelViewModel<'_>) -> String {
    let mut board_appearance_cards = String::new();
    for board in view.boards {
        let board_assets = view
            .appearance
            .board_banners
            .iter()
            .filter(|asset| {
                asset.scope == crate::models::BannerScope::Board && asset.board_id == Some(board.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        board_appearance_cards.push_str(&render_board_appearance_card(
            board,
            view.boards,
            view.csrf_token,
            view.appearance.themes,
            &board_assets,
            view.open_section,
        ));
    }
    board_appearance_cards
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
fn render_preset_options(selected_slug: &str) -> String {
    let mut out = String::new();
    for preset in BUILDER_PRESETS {
        let _ = write!(
            out,
            r#"<option value="{slug}"{selected}>{label}</option>"#,
            slug = escape_html(preset.slug),
            selected = if preset.slug == selected_slug {
                " selected"
            } else {
                ""
            },
            label = escape_html(preset.label),
        );
    }
    out
}

fn render_color_control(label: &str, name: &str, value: &str, help: &str) -> String {
    format!(
        r#"<label class="theme-builder-color-field">{label}
  <input type="color" name="{name}" value="{value}" data-theme-builder-field="{name}">
  <span class="theme-builder-color-value" data-theme-builder-value-for="{name}">{value}</span>
  <small>{help}</small>
</label>"#,
        label = escape_html(label),
        name = escape_html(name),
        value = escape_html(value),
        help = escape_html(help),
    )
}

#[allow(clippy::too_many_lines)]
fn render_builder_sections(config: &ThemeBuilderConfig) -> String {
    let basics = format!(
        r#"<details class="theme-builder-section" open>
  <summary>Basics</summary>
  <div class="theme-builder-section-body board-settings-grid">
    <label>Starting preset
      <select name="base_preset" data-theme-builder-field="base_preset">{preset_options}</select>
      <small>Pick the built-in theme that is closest to what you want, then tune from there.</small>
    </label>
    <label>Compactness
      <select name="density" data-theme-builder-field="density">
        <option value="cozy"{cozy_selected}>Cozy</option>
        <option value="compact"{compact_selected}>Compact</option>
      </select>
      <small>Compact reduces padding and spacing around posts and cards.</small>
    </label>
    <label>Font family
      <select name="font_family" data-theme-builder-field="font_family">
        <option value="system_sans"{sans_selected}>System Sans</option>
        <option value="system_serif"{serif_selected}>System Serif</option>
        <option value="system_mono"{mono_selected}>System Mono</option>
      </select>
      <small>System fonts only, so saved themes stay lightweight and safe.</small>
    </label>
    <label>Border radius
      <input type="range" name="border_radius_px" min="0" max="24" step="1" value="{radius}" data-theme-builder-field="border_radius_px">
      <span class="theme-builder-range-value" data-theme-builder-range-value="border_radius_px">{radius}px</span>
      <small>Lower values feel sharper. Higher values feel softer.</small>
    </label>
  </div>
</details>"#,
        preset_options = render_preset_options(&config.base_preset),
        cozy_selected = if config.density == ThemeDensity::Cozy {
            " selected"
        } else {
            ""
        },
        compact_selected = if config.density == ThemeDensity::Compact {
            " selected"
        } else {
            ""
        },
        sans_selected = if config.font_family == ThemeFontFamily::Sans {
            " selected"
        } else {
            ""
        },
        serif_selected = if config.font_family == ThemeFontFamily::Serif {
            " selected"
        } else {
            ""
        },
        mono_selected = if config.font_family == ThemeFontFamily::Mono {
            " selected"
        } else {
            ""
        },
        radius = config.border_radius_px,
    );
    let colors = format!(
        r#"<details class="theme-builder-section" open>
  <summary>Colors</summary>
  <div class="theme-builder-section-body theme-builder-colors-grid">
    {background}{panel}{text}{muted}{link}{link_hover}{border}{quote}{meta}{success}{danger}
  </div>
</details>"#,
        background = render_color_control(
            "Background",
            "background_color",
            &config.background_color,
            "Main page background.",
        ),
        panel = render_color_control(
            "Panel/Card",
            "panel_color",
            &config.panel_color,
            "Boxes like cards and panels.",
        ),
        text = render_color_control(
            "Text",
            "text_color",
            &config.text_color,
            "Main readable text."
        ),
        muted = render_color_control(
            "Muted Text",
            "muted_text_color",
            &config.muted_text_color,
            "Helper text and softer labels.",
        ),
        link = render_color_control(
            "Link",
            "link_color",
            &config.link_color,
            "Standard link color."
        ),
        link_hover = render_color_control(
            "Link Hover",
            "link_hover_color",
            &config.link_hover_color,
            "Link color when hovered.",
        ),
        border = render_color_control(
            "Border",
            "border_color",
            &config.border_color,
            "General outline and divider color.",
        ),
        quote = render_color_control(
            "Quote",
            "quote_color",
            &config.quote_color,
            "Greentext and quotes."
        ),
        meta = render_color_control(
            "Metadata",
            "meta_text_color",
            &config.meta_text_color,
            "Timestamps, post numbers, and secondary post info.",
        ),
        success = render_color_control(
            "Success/OK",
            "success_color",
            &config.success_color,
            "Positive notices and success accents.",
        ),
        danger = render_color_control(
            "Error/Alert",
            "danger_color",
            &config.danger_color,
            "Warnings and error accents.",
        ),
    );
    let posts = format!(
        r#"<details class="theme-builder-section">
  <summary>Posts &amp; Cards</summary>
  <div class="theme-builder-section-body theme-builder-colors-grid">
    {card}{op_card}{header_bg}{header_text}{header_border}
  </div>
</details>"#,
        card = render_color_control(
            "Post Background",
            "card_color",
            &config.card_color,
            "Reply cards and regular post boxes.",
        ),
        op_card = render_color_control(
            "Thread Starter",
            "op_card_color",
            &config.op_card_color,
            "Original post card background.",
        ),
        header_bg = render_color_control(
            "Header/Nav Background",
            "header_background_color",
            &config.header_background_color,
            "Top site bar background.",
        ),
        header_text = render_color_control(
            "Header/Nav Text",
            "header_text_color",
            &config.header_text_color,
            "Top site bar links and labels.",
        ),
        header_border = render_color_control(
            "Header/Nav Border",
            "header_border_color",
            &config.header_border_color,
            "Top site bar bottom border.",
        ),
    );
    let forms = format!(
        r#"<details class="theme-builder-section">
  <summary>Forms &amp; Buttons</summary>
  <div class="theme-builder-section-body theme-builder-colors-grid">
    {input_bg}{input_text}{input_border}{button_bg}{button_text}{button_border}{button_hover}
  </div>
</details>"#,
        input_bg = render_color_control(
            "Input Background",
            "input_background_color",
            &config.input_background_color,
            "Text fields and textarea background.",
        ),
        input_text = render_color_control(
            "Input Text",
            "input_text_color",
            &config.input_text_color,
            "Text inside inputs and textareas.",
        ),
        input_border = render_color_control(
            "Input Border",
            "input_border_color",
            &config.input_border_color,
            "Outline for form fields.",
        ),
        button_bg = render_color_control(
            "Button Background",
            "button_background_color",
            &config.button_background_color,
            "Default button background.",
        ),
        button_text = render_color_control(
            "Button Text",
            "button_text_color",
            &config.button_text_color,
            "Button label color.",
        ),
        button_border = render_color_control(
            "Button Border",
            "button_border_color",
            &config.button_border_color,
            "Button outline color.",
        ),
        button_hover = render_color_control(
            "Button Hover",
            "button_hover_color",
            &config.button_hover_color,
            "Button background on hover.",
        ),
    );
    let advanced = format!(
        r#"<details class="theme-builder-section">
  <summary>Advanced</summary>
  <div class="theme-builder-section-body">
    <label>Optional advanced CSS
      <textarea name="advanced_css" rows="8" spellcheck="false" data-theme-builder-field="advanced_css">{advanced_css}</textarea>
      <small>Optional. Use this only for small finishing touches after the guided controls. Imports and script-like URLs are rejected.</small>
    </label>
  </div>
</details>"#,
        advanced_css = escape_html(&config.advanced_css),
    );

    format!("{basics}{colors}{posts}{forms}{advanced}")
}

fn render_builder_preview(config: &ThemeBuilderConfig, slug: &str) -> String {
    format!(
        r##"<section class="theme-builder-preview-card">
  <div class="admin-card-header">
    <h4>Preview</h4>
    <p>Representative surfaces update as you change values when JavaScript is available. Saved themes still render server-side without JavaScript.</p>
  </div>
  <style data-theme-preview-style></style>
  <div class="theme-preview-shell" data-theme-preview data-theme-preview-slug="{slug}" data-theme-preview-preset="{preset}">
    <div class="theme-preview-header">
      <span class="theme-preview-title">RustChan</span>
      <nav class="theme-preview-nav"><a href="#">/tech/</a> <a href="#">/art/</a> <a href="#">/mu/</a></nav>
    </div>
    <div class="theme-preview-panels">
      <article class="theme-preview-panel">
        <h5>Homepage card</h5>
        <p class="theme-preview-muted">Board subtitle and secondary text.</p>
        <a href="#">open board</a>
      </article>
      <article class="theme-preview-post theme-preview-op">
        <div class="theme-preview-meta">OP 04/29/2026 No.101</div>
        <p><span class="theme-preview-quote">&gt; quoted line</span><br>Starter post content with a <a href="#">link</a>.</p>
      </article>
      <article class="theme-preview-post">
        <div class="theme-preview-meta">Reply No.102</div>
        <p>Reply card with metadata, links, and regular body text.</p>
      </article>
      <form class="theme-preview-form">
        <input type="text" value="Name">
        <textarea rows="3">Body text</textarea>
        <div class="theme-preview-actions">
          <button type="button">Post</button>
          <button type="button" class="theme-preview-secondary">Preview</button>
        </div>
      </form>
      <div class="theme-preview-flashes">
        <div class="admin-flash flash-ok">Saved theme preview</div>
        <div class="admin-flash flash-error">Validation message preview</div>
      </div>
    </div>
  </div>
</section>"##,
        slug = escape_html(slug),
        preset = escape_html(&config.base_preset),
    )
}

fn render_builder_editor(theme_slug: &str, config: &ThemeBuilderConfig) -> String {
    format!(
        r#"<input type="hidden" name="theme_mode" value="builder">
<div class="theme-builder-shell" data-theme-builder>
  <div class="theme-builder-controls">
    {sections}
  </div>
  {preview}
</div>"#,
        sections = render_builder_sections(config),
        preview = render_builder_preview(config, theme_slug),
    )
}

fn render_legacy_editor(theme_slug: &str, custom_css: &str) -> String {
    format!(
        r#"<input type="hidden" name="theme_mode" value="legacy">
<div class="theme-editor-built-in-note">
  <p>This is a legacy custom CSS theme. RustChan will keep loading it as-is for compatibility. You can still edit the raw CSS below, or create a new guided theme if you want the simpler builder.</p>
</div>
<div class="theme-editor-css-panel">
  <div class="theme-editor-panel-header">
    <h4>Legacy custom CSS</h4>
    <p>Scope everything to <code>html[data-theme="{slug}"]</code>. This is the advanced escape hatch.</p>
  </div>
  <textarea name="custom_css" rows="18" spellcheck="false">{custom_css}</textarea>
  <p class="theme-editor-code-note">Legacy themes continue to work without migration. New guided themes use the builder above instead of requiring raw CSS.</p>
</div>"#,
        slug = escape_html(theme_slug),
        custom_css = escape_html(custom_css),
    )
}

fn render_theme_metadata_fields(theme: &crate::models::Theme) -> String {
    if theme.is_builtin {
        format!(
            r#"<div class="board-settings-grid">
        <label>Display name<input type="text" value="{name}" maxlength="64" readonly aria-readonly="true"></label>
        <label>Slug<input type="text" value="{slug}" maxlength="32" readonly aria-readonly="true"></label>
        <label>Swatch<input type="color" value="{swatch}" disabled></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" value="{description}" maxlength="256" readonly aria-readonly="true"></label>
      </div>
      <p class="admin-meta-note">Built-in theme metadata is managed by RustChan and cannot be edited here. Only picker visibility can be changed.</p>"#,
            name = escape_html(&theme.display_name),
            slug = escape_html(&theme.slug),
            swatch = escape_html(&theme.swatch_hex),
            description = escape_html(&theme.description),
        )
    } else {
        format!(
            r#"<div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" value="{name}" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" value="{slug}" maxlength="32"></label>
        <label>Swatch<input type="color" name="swatch_hex" value="{swatch}"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" value="{description}" maxlength="256"></label>
      </div>"#,
            name = escape_html(&theme.display_name),
            slug = escape_html(&theme.slug),
            swatch = escape_html(&theme.swatch_hex),
            description = escape_html(&theme.description),
        )
    }
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
fn render_theme_cards(view: &AdminPanelViewModel<'_>) -> (String, String) {
    let mut builtin_theme_cards = String::new();
    let mut custom_theme_cards = String::new();
    for theme in view.appearance.themes {
        let theme_editor = if theme.is_builtin {
            r#"<div class="theme-editor-built-in-note">
<p>Built-in themes are maintained in <code>static/style.css</code>. You can toggle them here for the picker, but guided editing is reserved for custom themes so the shipped presets stay stable.</p>
</div>"#
                .to_string()
        } else if let Some(builder_config) = parse_builder_config(&theme.custom_css) {
            render_builder_editor(&theme.slug, &builder_config)
        } else {
            render_legacy_editor(&theme.slug, &theme.custom_css)
        };
        let card_markup = format!(
            r#"<details class="board-settings-card theme-editor-card" id="theme-{slug}">
<summary class="theme-card-summary">
  <span class="theme-card-swatch" style="--theme-swatch:{swatch}"></span>
  <span class="theme-card-heading">
    <strong>{name}</strong>
    <span class="theme-card-meta"><code>{slug}</code>{builtin_tag}{disabled_tag}</span>
  </span>
  <span class="theme-card-description">{description}</span>
</summary>
<form method="POST" action="/admin/theme/update" class="board-settings-form theme-editor-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="existing_slug" value="{slug}">
  <div class="theme-editor-layout">
    <div class="theme-editor-basics">
      {metadata_fields}
      <div class="board-settings-checks">
        <label><input type="checkbox" name="enabled" value="1"{enabled_ck}> Enabled in theme picker</label>
      </div>
      {theme_editor}
    </div>
  </div>
  <div class="board-settings-actions">
    <button type="submit">save theme settings</button>
  </div>
</form>
{delete_form}
</details>"#,
            csrf = escape_html(view.csrf_token),
            name = escape_html(&theme.display_name),
            slug = escape_html(&theme.slug),
            swatch = escape_html(&theme.swatch_hex),
            builtin_tag = if theme.is_builtin {
                r#" <span class="tag">built-in</span>"#
            } else {
                r#" <span class="tag">custom</span>"#
            },
            disabled_tag = if theme.enabled {
                ""
            } else {
                r#" <span class="tag locked">disabled</span>"#
            },
            description = if theme.description.trim().is_empty() {
                "No description yet.".to_string()
            } else {
                escape_html(&theme.description)
            },
            enabled_ck = if theme.enabled { " checked" } else { "" },
            metadata_fields = render_theme_metadata_fields(theme),
            theme_editor = theme_editor,
            delete_form = if theme.is_builtin {
                String::new()
            } else {
                format!(
                    r#"<form method="POST" action="/admin/theme/delete" class="theme-editor-delete">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="slug" value="{slug}">
  <button type="submit" class="btn-danger" data-confirm="Delete custom theme {slug}?">delete theme</button>
</form>"#,
                    csrf = escape_html(view.csrf_token),
                    slug = escape_html(&theme.slug)
                )
            }
        );
        if theme.is_builtin {
            builtin_theme_cards.push_str(&card_markup);
        } else {
            custom_theme_cards.push_str(&card_markup);
        }
    }
    (builtin_theme_cards, custom_theme_cards)
}

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
fn render_admin_site_settings_section(
    csrf_token: &str,
    site_name_val: &str,
    site_subtitle_val: &str,
    homepage_new_thread_badges_enabled: bool,
    thread_new_reply_badges_enabled: bool,
    enabled_theme_options: &str,
    global_favicon_preview: &str,
    global_favicon_label: &str,
    global_favicon_button: &str,
    global_favicon_status: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-site-settings" id="site-settings-panel">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // site settings
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="site-settings">
<h2>// site settings</h2>
<form method="POST" action="/admin/site/settings" class="admin-site-settings-form">
<input type="hidden" name="_csrf" value="{csrf}">
<div class="board-settings-grid admin-settings-grid">
  <label>Site name
    <input type="text" name="site_name" value="{site_name_val}" maxlength="64" placeholder="RustChan"
           style="font-family:inherit">
  </label>
  <label>Home page subtitle
    <input type="text" name="site_subtitle" value="{site_subtitle_val}" maxlength="128" placeholder="select board to proceed"
           style="font-family:inherit">
  </label>
  <label>Default theme
    <select name="default_theme" style="font-family:inherit;padding:0.25rem 0.4rem;background:var(--bg-input);color:var(--text);border:1px solid var(--border)">
      {enabled_theme_options}
    </select>
  </label>
</div>
<div class="board-settings-checks">
  <label class="admin-inline-checkbox">
    <input type="checkbox" name="homepage_new_thread_badges_enabled" value="1"{homepage_new_thread_badges_enabled_checked}>
    Homepage board-card new-thread badges
  </label>
  <label class="admin-inline-checkbox">
    <input type="checkbox" name="thread_new_reply_badges_enabled" value="1"{thread_new_reply_badges_enabled_checked}>
    Board/catalog thread-card new-reply badges
  </label>
</div>
<p class="admin-meta-note admin-meta-note-spaced">
  Track newly created threads on the home page and new replies inside board index/catalog cards independently.
</p>
<div class="board-settings-actions">
  <button type="submit">save settings</button>
</div>
</form>
<div class="favicon-inline-row favicon-inline-row-global">
{global_favicon_preview}
<form method="POST" action="/admin/site/favicon" enctype="multipart/form-data" class="favicon-inline-form">
<input type="hidden" name="_csrf" value="{csrf}">
<label class="favicon-inline-label">
  {global_favicon_label}
  <input type="file" name="favicon" accept="image/png,image/jpeg,image/webp" required class="favicon-inline-input">
</label>
<button type="submit">{global_favicon_button}</button>
</form>
</div>
<p class="admin-meta-note admin-meta-note-spaced">
  {global_favicon_status}
</p>
</section>
</div>"#,
        csrf = escape_html(csrf_token),
        site_name_val = escape_html(site_name_val),
        site_subtitle_val = escape_html(site_subtitle_val),
        homepage_new_thread_badges_enabled_checked = if homepage_new_thread_badges_enabled {
            " checked"
        } else {
            ""
        },
        thread_new_reply_badges_enabled_checked = if thread_new_reply_badges_enabled {
            " checked"
        } else {
            ""
        },
        enabled_theme_options = enabled_theme_options,
        global_favicon_preview = global_favicon_preview,
        global_favicon_label = global_favicon_label,
        global_favicon_button = global_favicon_button,
        global_favicon_status = global_favicon_status,
    )
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
fn render_admin_appearance_section(
    csrf_token: &str,
    banner_rotation_interval_minutes: i64,
    banner_external_links_enabled_checked: &str,
    banner_settings_open_attr: &str,
    global_banner_upload_form: &str,
    global_banner_rows: &str,
    home_banner_upload_form: &str,
    home_banner_rows: &str,
    board_appearance_cards: &str,
    theme_catalog_open_attr: &str,
    builtin_theme_cards: &str,
    custom_theme_cards_or_empty: &str,
) -> String {
    let starter_builder = builder_defaults_for_preset("forest");
    format!(
        r##"<div class="admin-panel-appearance" id="appearance">
<section class="admin-section admin-section-collapsible" id="board-banners">
<details class="admin-dropdown" data-admin-dropdown-key="board-banners"{banner_settings_open_attr}>
<summary>// board banners &amp; favicons</summary>
<div class="admin-dropdown-content">
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// global board banner settings</h3>
    <p>Control rotation timing and whether banner clicks are allowed to leave the site.</p>
  </div>
  <form method="POST" action="/admin/site/settings" class="admin-site-settings-form admin-banner-settings-form">
    <input type="hidden" name="_csrf" value="{csrf}">
    <div class="board-settings-grid admin-settings-grid">
      <label class="board-settings-field-compact" title="0 means pick a new banner on each refresh. Values above 0 enforce timed rotation.">Rotate banners every (minutes)
        <input type="number" name="banner_rotation_interval_minutes" value="{banner_rotation_interval_minutes}" min="0" max="43200"
               style="font-family:inherit">
      </label>
      <label class="admin-inline-checkbox admin-banner-settings-toggle">
        <input type="checkbox" name="banner_external_links_enabled" value="1"{banner_external_links_enabled_checked} data-banner-external-toggle>
        Allow banners to open external websites after showing the warning page
      </label>
    </div>
    <div class="board-settings-actions">
      <button type="submit">save banner settings</button>
    </div>
  </form>
</div>

<div class="admin-subsection admin-subsection-tight" id="global-banners">
  <div class="admin-card-header">
    <h3>// global board banners</h3>
    <p>These banners rotate on board index and catalog pages unless a board uses its own banner set.</p>
  </div>
  <p class="admin-meta-note">Exact 468x60 aspect ratio required. Minimum 468x60, recommended 936x120. Uploads are converted to WebP.</p>
  {global_banner_upload_form}
  <div class="admin-banner-list">{global_banner_rows}</div>
</div>

<div class="admin-subsection admin-subsection-tight" id="home-banners">
  <div class="admin-card-header">
    <h3>// home page banner settings</h3>
    <p>Use this separate banner area for MOTD, news, or maintenance notices on the home page only.</p>
  </div>
  <p class="admin-meta-note">Exact 468x60 aspect ratio required. Minimum 468x60, recommended 936x120. Uploads are converted to WebP.</p>
  {home_banner_upload_form}
  <div class="admin-banner-list">{home_banner_rows}</div>
</div>

<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// board appearance overrides</h3>
    <p>Board-specific themes, favicon overrides, and board banner sets are managed here instead of inside the routine board cards.</p>
  </div>
  <div class="admin-board-cards">{board_appearance_cards}</div>
</div>
</div>
</details>
</section>

<section class="admin-section admin-section-collapsible" id="theme-catalog">
<details class="admin-dropdown" data-admin-dropdown-key="theme-catalog"{theme_catalog_open_attr}>
<summary><span>// themes</span></summary>
<div class="admin-dropdown-content">
<details class="admin-dropdown theme-workbench-dropdown" data-admin-dropdown-key="theme-workbench">
<summary><span>// custom theme workshop</span></summary>
<div class="admin-dropdown-content">
<div class="theme-manager-shell">
  <section class="theme-guide-card">
    <div class="admin-card-header">
      <h3>// guided theme builder</h3>
      <p>Build a theme with presets, color pickers, spacing controls, and safe system-font choices. RustChan still saves the final theme as regular server-rendered CSS, so Tor and no-JS visitors see the saved result normally.</p>
    </div>
    <div class="theme-guide-grid">
      <div class="theme-guide-block">
        <h4>Main flow</h4>
        <p>1. Pick a built-in preset.</p>
        <p>2. Adjust basics, colors, posts, and forms.</p>
        <p>3. Preview the result.</p>
        <p>4. Save it as a custom theme.</p>
      </div>
      <div class="theme-guide-block">
        <h4>Compatibility</h4>
        <p>Built-in themes stay untouched.</p>
        <p>Saved custom themes from older versions still load.</p>
        <p>Older raw-CSS themes are shown as legacy advanced themes instead of being auto-migrated.</p>
      </div>
    </div>
    <p class="theme-guide-note">Need full control? Guided themes include a smaller advanced CSS box for finishing touches, while older raw CSS themes remain editable in legacy mode.</p>
  </section>

  <section class="theme-create-card">
    <div class="admin-card-header">
      <h3>// create custom theme</h3>
      <p>Start from a preset, tweak the friendly fields, and RustChan will generate the scoped theme CSS internally.</p>
    </div>
    <form method="POST" action="/admin/theme/create" class="theme-create-form">
      <input type="hidden" name="_csrf" value="{csrf}">
      <div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" maxlength="32" required placeholder="mytheme"></label>
        <label>Theme picker swatch<input type="color" name="swatch_hex" value="#7ab84e"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" maxlength="256" placeholder="What makes this theme distinct?"></label>
      </div>
      <div class="board-settings-checks">
        <label><input type="checkbox" name="enabled" value="1" checked> Shown in theme picker</label>
      </div>
      {starter_builder_form}
      <div class="board-settings-actions">
        <button type="submit">create theme</button>
      </div>
    </form>
  </section>
</div>
</div>
</details>

<section class="theme-manager-group">
  <div class="theme-manager-group-header">
    <h3>// built-in themes</h3>
    <p>Toggle which shipped themes appear in the picker.</p>
  </div>
  <div class="theme-card-grid">{builtin_theme_cards}</div>
</section>

<section class="theme-manager-group">
  <div class="theme-manager-group-header">
    <h3>// custom themes</h3>
    <p>Guided themes reopen in the builder. Older themes without builder metadata stay available as legacy advanced CSS themes.</p>
  </div>
  <div class="theme-card-grid">{custom_theme_cards_or_empty}</div>
</section>
</div>
</details>
</section>
</div>"##,
        csrf = escape_html(csrf_token),
        banner_rotation_interval_minutes = banner_rotation_interval_minutes,
        banner_external_links_enabled_checked = banner_external_links_enabled_checked,
        banner_settings_open_attr = banner_settings_open_attr,
        global_banner_upload_form = global_banner_upload_form,
        global_banner_rows = global_banner_rows,
        home_banner_upload_form = home_banner_upload_form,
        home_banner_rows = home_banner_rows,
        board_appearance_cards = board_appearance_cards,
        theme_catalog_open_attr = theme_catalog_open_attr,
        builtin_theme_cards = builtin_theme_cards,
        custom_theme_cards_or_empty = custom_theme_cards_or_empty,
        starter_builder_form = render_builder_editor("new-theme-preview", &starter_builder),
    )
}
