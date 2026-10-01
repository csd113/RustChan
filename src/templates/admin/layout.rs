//! Admin-panel shell, dashboard, and shared status components.

use super::{
    appearance, backups, base_layout, boards, control_center, escape_html, maintenance, moderation,
    site_health,
};
use super::{AdminPanelFlash, AdminPanelViewModel};

/// Renders the complete admin panel page.
pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let selected = default_task(view.open_section);
    let flash_html = render_flash(view.flash);
    let section_index = render_admin_section_index(view.open_section);
    let settings_search = super::search::render(view.settings_query);
    let overview_section = render_admin_overview_section(view);
    let site_settings_section = appearance::render_site_settings(view);
    let site_health_section = site_health::render(view);
    let boards_section = boards::render(view);
    let moderation_section = moderation::render(view);
    let appearance_section = appearance::render(view);
    let backups_section = backups::render(view);
    let maintenance_section = maintenance::render(view);
    let overview_section = task(
        "overview",
        &format!("{overview_section}{site_health_section}"),
        selected == "overview",
    );
    let appearance_section = task(
        "appearance",
        &format!("{site_settings_section}{appearance_section}"),
        selected == "appearance",
    );
    let boards_section = task("boards", &boards_section, selected == "boards");
    let moderation_section = task("moderation", &moderation_section, selected == "moderation");
    let backups_section = task("backups", &backups_section, selected == "backups");
    let maintenance_section = task(
        "maintenance",
        &maintenance_section,
        selected == "maintenance",
    );

    let update_section = task(
        "software-updates",
        "<!-- software-updates -->",
        selected == "software-updates",
    );
    let body = format!(
        r#"<div class="admin-panel">
{flash}
<div class="admin-panel-header">
  <div class="admin-panel-heading">
    <h1>[ admin panel ]</h1>
    <p class="admin-panel-lead">Choose a task below. Search finds configuration, operational controls, and management workflows.</p>
  </div>
  <form method="POST" action="/admin/logout" class="admin-panel-logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">logout</button>
  </form>
</div>

{settings_search}
<div class="admin-panel-workspace">
{section_index}
<div class="admin-task-content" role="region" aria-label="Selected administration task">
{overview_section}
{network_section}
{runtime_sections}
{update_section}
{boards_section}
{moderation_section}
{appearance_section}
{backups_section}
{maintenance_section}
</div></div>

<div id="backup-modal" class="compress-modal admin-modal-hidden" role="dialog" aria-modal="true" aria-labelledby="backup-modal-title" tabindex="-1" aria-hidden="true" hidden inert>
  <div class="compress-modal-box">
    <div class="compress-modal-title" id="backup-modal-title">&#128190; Creating Backup…</div>
    <div class="compress-progress admin-progress-spaced" id="backup-progress-wrap">
      <div class="compress-progress-track" role="progressbar" aria-label="Backup progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0"><div class="compress-progress-bar" id="backup-progress-bar"></div></div>
      <div class="compress-progress-text" id="backup-progress-text" role="status">Starting…</div>
    </div>
    <div class="compress-done-actions admin-modal-hidden" id="backup-done-actions" hidden>
      <button class="compress-cancel-btn" data-action="close-backup-modal">&#10003; Done — reload</button>
    </div>
  </div>
</div>"#,
        flash = flash_html,
        section_index = section_index,
        network_section = view.network_html,
        runtime_sections = view.runtime_html,
        csrf = escape_html(view.csrf_token),
    );

    base_layout(
        "admin panel",
        None,
        &body,
        view.csrf_token,
        view.boards,
        view.current_theme,
        Some(view.appearance.default_theme),
        false,
        "/admin/panel",
    )
}

/// Renders a success or error flash message when one is present.
fn render_flash(flash: Option<AdminPanelFlash<'_>>) -> String {
    flash.map_or_else(String::new, |flash| {
        let cls = if flash.is_error {
            "flash-error"
        } else {
            "flash-ok"
        };
        format!(
            r#"<div class="admin-flash {cls}" role="{role}">{msg}</div>"#,
            cls = cls,
            role = if flash.is_error { "alert" } else { "status" },
            msg = escape_html(flash.message),
        )
    })
}

/// Renders links to each major admin-panel section.
fn render_admin_section_index(open_section: Option<&str>) -> String {
    let index = r##"<nav class="admin-section-index" aria-label="Admin panel sections">
  <span>Administration</span>
  <a href="#control-center">Overview</a>
  <a href="#site-settings">Site settings</a>
  <a href="#site-health">Site health</a>
  <a href="#network-security">network &amp; security</a>
  <a href="#https">HTTPS &amp; Certificates</a>
  <a href="#tor">Tor</a>
  <a href="#access">Access policies</a>
  <a href="#timeouts">Request deadlines</a>
  <a href="#display">Index display</a>
  <a href="#media">media</a>
  <a href="#schedules">schedules</a>
  <a href="#accounts">Accounts</a>
  <a href="#storage">Storage &amp; secrets</a>
  <a href="#logging">Logging</a>
  <a href="#system">Advanced system</a>
  <a href="#configuration-state">Application state</a>
  <a href="#boards">boards</a>
  <a href="#moderation">moderation</a>
  <a href="#appearance">appearance</a>
  <a href="#backups">backups</a>
  <a href="#software-updates">Software Updates<!-- update-notification --></a>
  <a href="#maintenance">maintenance</a>
</nav>"##;
    let anchor = open_section
        .filter(|section| index.contains(&format!("href=\"#{section}\"")))
        .unwrap_or_else(|| match default_task(open_section) {
            "appearance" => "appearance",
            "backups" => "backups",
            "maintenance" => "maintenance",
            "boards" => "boards",
            "moderation" => "moderation",
            _ => "control-center",
        });
    index.replace(
        &format!("href=\"#{anchor}\""),
        &format!("href=\"#{anchor}\" aria-current=\"location\""),
    )
}

/// Renders the dashboard and live-log overview.
fn render_admin_overview_section(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = control_center::render(view);
    let live_log_open_attr = if view.open_section == Some("live-log") {
        " open"
    } else {
        ""
    };
    format!(
        r#"<div class="admin-panel-overview" id="overview">
{dashboard}
<section class="admin-section" id="live-log">
<details class="admin-dropdown" data-admin-dropdown-key="live-log"{live_log_open_attr}>
<summary>// live log</summary>
<div class="admin-dropdown-content">
<p class="admin-copy">
  Watching <span id="admin-live-log-file">current log</span>. Updates every 2 seconds.
</p>
<p id="admin-live-log-status" class="admin-meta-note">JavaScript enables live updates. The current log tail remains available below.</p>
<p class="admin-copy admin-copy-spaced"><a href="/admin/log/live?bytes=65536">open current log tail (JSON)</a></p>
<div class="admin-inline-actions admin-inline-actions-spaced" data-admin-live-log-controls hidden>
  <button type="button" id="admin-live-log-refresh">refresh now</button>
  <button type="button" id="admin-live-log-clear">clear</button>
  <label class="admin-inline-toggle">
    <input type="checkbox" id="admin-live-log-autoscroll" checked> auto-scroll
  </label>
</div>
<pre id="admin-live-log-output" class="admin-log-output">Live updates start when JavaScript is available.</pre>
</div>
</details>
</section>
</div>"#,
    )
}

/// Keep compatibility anchors while showing only the selected task in modern browsers.
fn task(key: &str, content: &str, default: bool) -> String {
    format!(
        "<div class=\"admin-task{}\" data-admin-task=\"{key}\">{content}</div>",
        if default { " admin-task-default" } else { "" }
    )
}

/// Preserve server-rendered ?open navigation when no fragment or JavaScript exists.
fn default_task(open: Option<&str>) -> &'static str {
    match open {
        Some(section) if section.starts_with("board-appearance-") => "appearance",
        Some(section) if section.starts_with("board-backup-") => "backups",
        Some(section) if section.starts_with("board-") => "boards",
        Some("software-updates") => "software-updates",
        Some("boards") => "boards",
        Some("reports" | "moderation") => "moderation",
        Some(
            "site-settings" | "appearance" | "board-banners" | "global-banners" | "home-banners"
            | "theme-catalog" | "theme-workbench",
        ) => "appearance",
        Some("backups" | "full-backup-restore") => "backups",
        Some("maintenance" | "media-settings" | "database-maintenance") => "maintenance",
        _ => "overview",
    }
}
