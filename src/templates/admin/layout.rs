use super::{appearance, backups, base_layout, boards, escape_html, maintenance, moderation};
use super::{AdminPanelFlash, AdminPanelViewModel};

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let flash_html = render_flash(view.flash);
    let section_index = render_admin_section_index();
    let overview_section = render_admin_overview_section();
    let site_settings_section = appearance::render_site_settings(view);
    let boards_section = boards::render(view);
    let moderation_section = moderation::render(view);
    let appearance_section = appearance::render(view);
    let backups_section = backups::render(view);
    let maintenance_section = maintenance::render(view);

    let body = format!(
        r#"<div class="admin-panel">
{flash}
<div class="admin-panel-header">
  <div class="admin-panel-heading">
    <h1>[ admin panel ]</h1>
    <p class="admin-panel-lead">Manage boards, moderation, themes, backups, and site settings from one place.</p>
  </div>
  <form method="POST" action="/admin/logout" class="admin-panel-logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">logout</button>
  </form>
</div>

{section_index}
{overview_section}
{site_settings_section}
{boards_section}
{moderation_section}
{appearance_section}
{backups_section}
{maintenance_section}

<!-- ── Backup progress modal ─────────────────────────────────────────────── -->
<div id="backup-modal" class="compress-modal admin-modal-hidden" role="dialog" aria-modal="true" aria-labelledby="backup-modal-title">
  <div class="compress-modal-box">
    <div class="compress-modal-title" id="backup-modal-title">&#128190; Creating Backup…</div>
    <div class="compress-progress admin-progress-spaced" id="backup-progress-wrap">
      <div class="compress-progress-track"><div class="compress-progress-bar" id="backup-progress-bar"></div></div>
      <div class="compress-progress-text" id="backup-progress-text">Starting…</div>
    </div>
    <div class="compress-done-actions admin-modal-hidden" id="backup-done-actions">
      <button class="compress-cancel-btn" data-action="close-backup-modal">&#10003; Done — reload</button>
    </div>
  </div>
</div>"#,
        flash = flash_html,
        section_index = section_index,
        csrf = escape_html(view.csrf_token),
    );

    base_layout(
        "admin panel",
        None,
        &body,
        view.csrf_token,
        view.boards,
        None,
        None,
        false,
        "/admin/panel",
    )
}

fn render_flash(flash: Option<AdminPanelFlash<'_>>) -> String {
    flash.map_or_else(String::new, |flash| {
        let cls = if flash.is_error {
            "flash-error"
        } else {
            "flash-ok"
        };
        format!(
            r#"<div class="admin-flash {cls}">{msg}</div>"#,
            cls = cls,
            msg = escape_html(flash.message),
        )
    })
}

const fn render_admin_section_index() -> &'static str {
    r##"<nav class="admin-section-index" aria-label="Admin panel sections">
  <span>jump to</span>
  <a href="#site-settings">site settings</a>
  <a href="#boards">boards</a>
  <a href="#moderation">moderation</a>
  <a href="#appearance">appearance</a>
  <a href="#backups">backups</a>
  <a href="#maintenance">maintenance</a>
</nav>"##
}

fn render_admin_overview_section() -> String {
    r#"<div class="admin-panel-overview" id="overview">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // live log
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="live-log">
<details class="admin-dropdown" data-admin-dropdown-key="live-log">
<summary>// live log</summary>
<div class="admin-dropdown-content">
<p class="admin-copy">
  Watching <span id="admin-live-log-file">current log</span>. Updates every 2 seconds.
</p>
<div class="admin-inline-actions admin-inline-actions-spaced">
  <button type="button" id="admin-live-log-refresh">refresh now</button>
  <button type="button" id="admin-live-log-clear">clear</button>
  <label class="admin-inline-toggle">
    <input type="checkbox" id="admin-live-log-autoscroll" checked> auto-scroll
  </label>
</div>
<pre id="admin-live-log-output" class="admin-log-output">Loading live log…</pre>
</div>
</details>
</section>
</div>"#
        .to_string()
}
