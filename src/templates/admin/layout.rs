use super::{
    appearance, backups, base_layout, boards, escape_html, maintenance, moderation, site_health,
};
use super::{AdminDashboardState, AdminPanelFlash, AdminPanelViewModel};
use std::fmt::Write as _;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let flash_html = render_flash(view.flash);
    let section_index = render_admin_section_index();
    let overview_section = render_admin_overview_section(view);
    let site_settings_section = appearance::render_site_settings(view);
    let site_health_section = site_health::render(view);
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
{site_health_section}
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
        view.current_theme,
        Some(view.appearance.default_theme),
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
  <a href="#control-center">control center</a>
  <a href="#site-settings">site settings</a>
  <a href="#site-health">site health</a>
  <a href="#boards">boards</a>
  <a href="#moderation">moderation</a>
  <a href="#appearance">appearance</a>
  <a href="#backups">backups</a>
  <a href="#maintenance">maintenance</a>
</nav>"##
}

fn render_admin_overview_section(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = render_admin_dashboard_section(view);
    format!(
        r#"<div class="admin-panel-overview" id="overview">
{dashboard}
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
<p id="admin-live-log-status" class="admin-meta-note">Connecting to live log…</p>
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
</div>"#,
    )
}

fn render_admin_dashboard_section(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = &view.dashboard;
    let overview_cards = render_dashboard_overview_cards(view);
    let health_cards = render_dashboard_health_cards(view);
    let activity_cards = render_dashboard_activity_cards(view);
    let quick_actions = render_dashboard_quick_actions(view);

    format!(
        r#"<!-- ═══════════════════════════════════════════════════════════════════════════
     // control center
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-control-center" id="control-center" aria-labelledby="control-center-title">
  <div class="admin-control-center-header">
    <div>
      <h2 id="control-center-title">// control center</h2>
      <p class="admin-panel-lead">Operational summary for {site_title}.</p>
    </div>
    <div class="admin-control-center-status">
      {overall_status}
    </div>
  </div>
  <div class="admin-dashboard-block">
    <div class="admin-card-header">
      <h3>// instance overview</h3>
      <p>Version, setup state, configured public entry point, and safe navigation.</p>
    </div>
    <div class="admin-dashboard-grid admin-dashboard-grid-overview">{overview_cards}</div>
  </div>
  <div class="admin-dashboard-block">
    <div class="admin-card-header">
      <h3>// health and needs attention</h3>
      <p>Cheap startup and stored status checks only.</p>
    </div>
    <div class="admin-dashboard-grid">{health_cards}</div>
  </div>
  <div class="admin-dashboard-block">
    <div class="admin-card-header">
      <h3>// activity and moderation</h3>
      <p>Current boards, posting activity, media totals, and report queues.</p>
    </div>
    <div class="admin-dashboard-grid">{activity_cards}</div>
  </div>
  <div class="admin-dashboard-block">
    <div class="admin-card-header">
      <h3>// quick actions</h3>
      <p>Links into existing admin tools; mutating actions keep their normal protections.</p>
    </div>
    {quick_actions}
  </div>
</section>"#,
        site_title = escape_html(dashboard.site_title),
        overall_status = render_dashboard_overall_status(dashboard),
    )
}

fn render_dashboard_overall_status(dashboard: &super::AdminPanelDashboardView<'_>) -> String {
    let state = [
        dashboard.setup_state,
        dashboard.db_state,
        dashboard.backup_state,
        dashboard.storage_state,
        dashboard.tor_state,
        dashboard.dependency_state,
        dashboard.job_state,
        dashboard.report_state,
    ]
    .into_iter()
    .max_by_key(|state| state_severity(*state))
    .unwrap_or(AdminDashboardState::Unknown);
    let label = match state {
        AdminDashboardState::Ok => "OK",
        AdminDashboardState::Warning => "Warning",
        AdminDashboardState::ActionNeeded => "Action needed",
        AdminDashboardState::Disabled => "Disabled",
        AdminDashboardState::Unknown => "Unknown",
    };
    render_state_pill(state, label)
}

const fn state_severity(state: AdminDashboardState) -> u8 {
    match state {
        AdminDashboardState::ActionNeeded => 4,
        AdminDashboardState::Warning => 3,
        AdminDashboardState::Unknown => 2,
        AdminDashboardState::Disabled => 1,
        AdminDashboardState::Ok => 0,
    }
}

fn render_dashboard_overview_cards(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = &view.dashboard;
    let mut out = String::new();
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Version and build",
            value: dashboard.version,
            detail: dashboard.build,
            state: AdminDashboardState::Ok,
            href: Some("#site-health"),
            action: Some("site health"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Setup status",
            value: dashboard.setup_status,
            detail: dashboard.setup_detail,
            state: dashboard.setup_state,
            href: Some("#database-maintenance"),
            action: Some("setup controls"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Site title",
            value: dashboard.site_title,
            detail: "Rendered from saved site settings.",
            state: AdminDashboardState::Ok,
            href: Some("#site-settings"),
            action: Some("site settings"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Public URL",
            value: dashboard.public_url,
            detail: "Configured public host, when available.",
            state: if dashboard.public_url == "not configured" {
                AdminDashboardState::Unknown
            } else {
                AdminDashboardState::Ok
            },
            href: Some("/"),
            action: Some("open home"),
        },
    );
    out
}

fn render_dashboard_health_cards(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = &view.dashboard;
    let mut out = String::new();
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Database",
            value: dashboard.db_status,
            detail: dashboard.db_detail,
            state: dashboard.db_state,
            href: Some("#database-maintenance"),
            action: Some("maintenance"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Backups",
            value: dashboard.backup_status,
            detail: dashboard.backup_detail,
            state: dashboard.backup_state,
            href: Some("#full-backup-restore"),
            action: Some("backups"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Storage",
            value: dashboard.storage_status,
            detail: dashboard.storage_detail,
            state: dashboard.storage_state,
            href: Some("#media-settings"),
            action: Some("media settings"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Tor",
            value: dashboard.tor_status,
            detail: dashboard.tor_detail,
            state: dashboard.tor_state,
            href: Some("#database-maintenance"),
            action: Some("maintenance"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Media tools",
            value: dashboard.dependency_status,
            detail: dashboard.dependency_detail,
            state: dashboard.dependency_state,
            href: Some("#media-settings"),
            action: Some("dependencies"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Jobs",
            value: dashboard.job_status,
            detail: dashboard.job_detail,
            state: dashboard.job_state,
            href: Some("#site-health"),
            action: Some("job details"),
        },
    );
    out
}

fn render_dashboard_activity_cards(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = &view.dashboard;
    let mut out = String::new();
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Boards",
            value: dashboard.board_count,
            detail: "Configured board directory.",
            state: AdminDashboardState::Ok,
            href: Some("#boards"),
            action: Some("manage boards"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Threads",
            value: dashboard.thread_count,
            detail: "Live and total thread counts.",
            state: AdminDashboardState::Ok,
            href: Some("#boards"),
            action: Some("board tools"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Posts",
            value: dashboard.post_count,
            detail: dashboard.recent_activity,
            state: AdminDashboardState::Ok,
            href: Some("/"),
            action: Some("home"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Uploads",
            value: dashboard.media_summary,
            detail: "Active media bytes are counted from stored post metadata.",
            state: AdminDashboardState::Ok,
            href: Some("#media-settings"),
            action: Some("media"),
        },
    );
    append_dashboard_metric(
        &mut out,
        DashboardMetric {
            label: "Reports",
            value: dashboard.report_status,
            detail: dashboard.report_detail,
            state: dashboard.report_state,
            href: Some("#reports"),
            action: Some("moderation"),
        },
    );
    out
}

fn render_dashboard_quick_actions(view: &AdminPanelViewModel<'_>) -> String {
    let close_setup = if view.dashboard.setup_state == AdminDashboardState::Warning
        && view.dashboard.setup_status == "reopened"
    {
        format!(
            r#"<form method="POST" action="/admin/setup/close">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit" data-confirm="Close the setup wizard without changing live settings?">close setup</button>
</form>"#,
            csrf = escape_html(view.csrf_token),
        )
    } else {
        String::new()
    };

    format!(
        r##"<div class="admin-dashboard-actions">
  <a class="admin-link-button" href="#boards" data-open-admin-section="boards">manage boards</a>
  <a class="admin-link-button" href="#boards" data-open-admin-section="boards">create board</a>
  <a class="admin-link-button" href="#full-backup-restore" data-open-admin-section="full-backup-restore">backups</a>
  <a class="admin-link-button" href="#site-health" data-open-admin-section="site-health">health</a>
  <a class="admin-link-button" href="#media-settings" data-open-admin-section="media-settings">dependencies</a>
  <a class="admin-link-button" href="#live-log">logs</a>
  <a class="admin-link-button" href="/admin/mod-log">mod log</a>
  <details class="admin-dashboard-action-details">
    <summary>setup controls</summary>
    <form method="POST" action="/admin/setup/reopen">
      <input type="hidden" name="_csrf" value="{csrf}">
      <button type="submit" data-confirm="Reopen the setup wizard? This edits live settings and remains admin-only. Continue?">reopen setup</button>
    </form>
    {close_setup}
  </details>
  <details class="admin-dashboard-action-details">
    <summary>diagnostics</summary>
    <p class="admin-meta-note">Use the Site Health diagnostics panel for copy support. Without JavaScript, the diagnostics text remains selectable there.</p>
    <a class="admin-link-button" href="#site-health" data-open-admin-section="site-health">open diagnostics</a>
  </details>
</div>"##,
        csrf = escape_html(view.csrf_token),
        close_setup = close_setup,
    )
}

#[derive(Clone, Copy)]
struct DashboardMetric<'a> {
    label: &'a str,
    value: &'a str,
    detail: &'a str,
    state: AdminDashboardState,
    href: Option<&'a str>,
    action: Option<&'a str>,
}

fn append_dashboard_metric(out: &mut String, metric: DashboardMetric<'_>) {
    let action = match (metric.href, metric.action) {
        (Some(href), Some(action)) => format!(
            r#"<a href="{href}" class="admin-dashboard-card-link">{action}</a>"#,
            href = escape_html(href),
            action = escape_html(action),
        ),
        _ => String::new(),
    };
    let _ = write!(
        out,
        r#"<article class="admin-dashboard-card admin-dashboard-card-{state_class}">
  <div class="admin-dashboard-card-top">
    <span>{label}</span>
    {pill}
  </div>
  <strong>{value}</strong>
  <p>{detail}</p>
  {action}
</article>"#,
        state_class = state_class(metric.state),
        label = escape_html(metric.label),
        pill = render_state_pill(metric.state, state_label(metric.state)),
        value = escape_html(metric.value),
        detail = escape_html(metric.detail),
        action = action,
    );
}

fn render_state_pill(state: AdminDashboardState, label: &str) -> String {
    format!(
        r#"<span class="admin-state-pill admin-state-pill-{class}">{label}</span>"#,
        class = state_class(state),
        label = escape_html(label),
    )
}

const fn state_class(state: AdminDashboardState) -> &'static str {
    match state {
        AdminDashboardState::Ok => "ok",
        AdminDashboardState::Warning => "warning",
        AdminDashboardState::ActionNeeded => "action-needed",
        AdminDashboardState::Disabled => "disabled",
        AdminDashboardState::Unknown => "unknown",
    }
}

const fn state_label(state: AdminDashboardState) -> &'static str {
    match state {
        AdminDashboardState::Ok => "OK",
        AdminDashboardState::Warning => "warning",
        AdminDashboardState::ActionNeeded => "action",
        AdminDashboardState::Disabled => "disabled",
        AdminDashboardState::Unknown => "unknown",
    }
}
