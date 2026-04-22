use super::{
    escape_html, render_banner_asset_list, render_banner_upload_form, render_board_appearance_card,
    theme_css_starter, AdminPanelViewModel,
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
    let new_theme_starter_css = escape_html(&theme_css_starter("your-theme", "#7ab84e"));
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
        &new_theme_starter_css,
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
fn render_theme_cards(view: &AdminPanelViewModel<'_>) -> (String, String) {
    let mut builtin_theme_cards = String::new();
    let mut custom_theme_cards = String::new();
    for theme in view.appearance.themes {
        let theme_css_value = if theme.custom_css.trim().is_empty() {
            theme_css_starter(&theme.slug, &theme.swatch_hex)
        } else {
            theme.custom_css.clone()
        };
        let theme_editor = if theme.is_builtin {
            r#"<div class="theme-editor-built-in-note">
<p>Built-in themes are maintained in <code>static/style.css</code>. You can toggle them here for the picker, but custom CSS is reserved for custom themes.</p>
</div>"#
                .to_string()
        } else {
            format!(
                r#"<div class="theme-editor-css-panel">
  <div class="theme-editor-panel-header">
    <h4>Custom CSS</h4>
    <p>Scope everything to <code>html[data-theme="{slug}"]</code>. This textarea accepts full CSS, not just variables.</p>
  </div>
  <textarea name="custom_css" rows="18" spellcheck="false">{custom_css}</textarea>
  <p class="theme-editor-code-note">Tip: start by changing the variables block, then add selector overrides for <code>body</code>, <code>.site-header</code>, <code>.page-box</code>, <code>.op</code>, <code>.reply</code>, and buttons if you need more personality.</p>
</div>"#,
                slug = escape_html(&theme.slug),
                custom_css = escape_html(&theme_css_value),
            )
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
      <div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" value="{name}" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" value="{slug}" maxlength="32"{slug_readonly}></label>
        <label>Swatch<input type="text" name="swatch_hex" value="{swatch}" maxlength="7"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" value="{description_raw}" maxlength="256"></label>
      </div>
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
            description_raw = escape_html(&theme.description),
            slug_readonly = if theme.is_builtin { " readonly" } else { "" },
            enabled_ck = if theme.enabled { " checked" } else { "" },
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
    new_theme_starter_css: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-appearance" id="appearance">
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
      <h3>// how RustChan themes work</h3>
      <p>Every theme is just CSS scoped to <code>html[data-theme="slug"]</code>. Most of the site styling comes from shared variables first, then optional selector overrides for the pieces you want to customize.</p>
    </div>
    <div class="theme-guide-grid">
      <div class="theme-guide-block">
        <h4>Core variables</h4>
        <pre class="theme-guide-code">--bg
--bg-panel
--bg-post
--bg-op
--bg-input
--border
--border-glow
--green
--green-dim
--green-bright
--green-pale
--amber
--red
--gray
--gray-light
--text
--text-dim
--font
--font-display</pre>
      </div>
      <div class="theme-guide-block">
        <h4>Common selectors</h4>
        <pre class="theme-guide-code">body
.site-header
.admin-section
.page-box
.post-form-container
.op
.reply
a / a:hover
button / button:hover</pre>
      </div>
    </div>
    <p class="theme-guide-note">Use the starter below for new themes. Built-in theme source lives in <code>static/style.css</code> if you want examples of complete themes.</p>
  </section>

  <section class="theme-create-card">
    <div class="admin-card-header">
      <h3>// create custom theme</h3>
      <p>Start from a working scaffold instead of a blank textarea, then tune variables and add overrides where needed.</p>
    </div>
    <form method="POST" action="/admin/theme/create" class="theme-create-form">
      <input type="hidden" name="_csrf" value="{csrf}">
      <div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" maxlength="32" required placeholder="mytheme"></label>
        <label>Swatch<input type="text" name="swatch_hex" maxlength="7" placeholder="7ab84e"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" maxlength="256" placeholder="What makes this theme distinct?"></label>
      </div>
      <div class="board-settings-checks">
        <label><input type="checkbox" name="enabled" value="1" checked> Shown in theme picker</label>
      </div>
      <div class="theme-editor-css-panel">
        <div class="theme-editor-panel-header">
          <h4>Starter CSS</h4>
          <p>Replace <code>your-theme</code> in the selector with the slug above before saving.</p>
        </div>
        <textarea name="custom_css" rows="22" spellcheck="false" required>{new_theme_starter_css}</textarea>
        <p class="theme-editor-code-note">You can keep this file variable-driven and only add selector overrides where the default site structure needs extra styling.</p>
      </div>
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
    <p>Edit your own themes with a full CSS editor and swatch metadata.</p>
  </div>
  <div class="theme-card-grid">{custom_theme_cards_or_empty}</div>
</section>
</div>
</details>
</section>
</div>"#,
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
        new_theme_starter_css = new_theme_starter_css,
    )
}
