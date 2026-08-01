//! Task-oriented administrator Control Center rendering.

use super::{escape_html, AdminDashboardState, AdminPanelDashboardView, AdminPanelViewModel};
use std::cmp::Reverse;
use std::fmt::Write as _;

/// One operational signal displayed by the Control Center.
#[derive(Clone, Copy)]
struct DashboardSignal<'a> {
    /// Stable key used by focused browser tests and status styling.
    key: &'static str,
    /// Human-readable signal label.
    label: &'static str,
    /// Compact current value.
    value: &'a str,
    /// Secondary explanation shown in progressive disclosure.
    detail: &'a str,
    /// Semantic presentation state.
    state: AdminDashboardState,
    /// Server-rendered section to open when JavaScript is unavailable.
    open_section: &'static str,
    /// Final fragment target within the opened section.
    anchor: &'static str,
    /// Action label for the signal.
    action: &'static str,
}

/// Named operational signals used across task groups and attention triage.
#[derive(Clone, Copy)]
struct DashboardSignals<'a> {
    /// First-run setup route state.
    setup: DashboardSignal<'a>,
    /// Database readiness and integrity state.
    database: DashboardSignal<'a>,
    /// Saved full-backup state.
    backup: DashboardSignal<'a>,
    /// Data and upload storage state.
    storage: DashboardSignal<'a>,
    /// Tor configuration and onion-service state.
    tor: DashboardSignal<'a>,
    /// Optional media dependency state.
    dependencies: DashboardSignal<'a>,
    /// Background-job state.
    jobs: DashboardSignal<'a>,
    /// Moderation report-queue state.
    reports: DashboardSignal<'a>,
}

/// Renders the complete operational Control Center.
pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let dashboard = &view.dashboard;
    let signals = dashboard_signals(dashboard);
    let overall_status = render_dashboard_overall_status(&signals);
    let attention = render_attention(&signals);
    let common_actions = render_common_actions(&signals);
    let task_groups = render_task_groups(view, &signals);
    let system_details = render_system_details(view, &signals);
    let (open_attr, default_open_attr) = match view.open_section {
        Some("control-center") => (" open", ""),
        None => (" open", " data-admin-dropdown-default-open"),
        Some(_) => ("", ""),
    };

    format!(
        r#"<!-- ═══════════════════════════════════════════════════════════════════════════
     // control center
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-section-collapsible" id="control-center" aria-labelledby="control-center-title">
<details class="admin-dropdown" data-admin-dropdown-key="control-center"{default_open_attr}{open_attr}>
<summary><h2 id="control-center-title"><span>// control center</span><span class="admin-dropdown-badges">{overall_status}</span></h2></summary>
<div class="admin-dropdown-content admin-control-center">
  <header class="admin-control-center-header">
    <p class="admin-panel-lead">Prioritized operations for <strong>{site_title}</strong> · RustChan {version} on {build}.</p>
    <p class="admin-meta-note">Warnings, action-needed states, and failures appear first. Routine, pending, disabled, and informational states stay in their task groups.</p>
  </header>
  {attention}
  {common_actions}
  <div class="admin-control-grid">{task_groups}</div>
  {system_details}
</div>
</details>
</section>"#,
        site_title = escape_html(dashboard.site_title),
        version = escape_html(dashboard.version),
        build = escape_html(dashboard.build),
    )
}

/// Builds the named operational signal set from the existing dashboard snapshot.
const fn dashboard_signals<'a>(dashboard: &'a AdminPanelDashboardView<'a>) -> DashboardSignals<'a> {
    DashboardSignals {
        setup: DashboardSignal {
            key: "setup",
            label: "Setup",
            value: dashboard.setup_status,
            detail: dashboard.setup_detail,
            state: dashboard.setup_state,
            open_section: "database-maintenance",
            anchor: "database-maintenance",
            action: "open setup controls",
        },
        database: DashboardSignal {
            key: "database",
            label: "Database",
            value: dashboard.db_status,
            detail: dashboard.db_detail,
            state: dashboard.db_state,
            open_section: "database-maintenance",
            anchor: "database-maintenance",
            action: "open maintenance",
        },
        backup: DashboardSignal {
            key: "backups",
            label: "Backups",
            value: dashboard.backup_status,
            detail: dashboard.backup_detail,
            state: dashboard.backup_state,
            open_section: "full-backup-restore",
            anchor: "full-backup-restore",
            action: "manage backups",
        },
        storage: DashboardSignal {
            key: "storage",
            label: "Storage",
            value: dashboard.storage_status,
            detail: dashboard.storage_detail,
            state: dashboard.storage_state,
            open_section: "media-settings",
            anchor: "media-settings",
            action: "open media settings",
        },
        tor: DashboardSignal {
            key: "tor",
            label: "Tor",
            value: dashboard.tor_status,
            detail: dashboard.tor_detail,
            state: dashboard.tor_state,
            open_section: "site-health",
            anchor: "tor-status",
            action: "open Tor diagnostics",
        },
        dependencies: DashboardSignal {
            key: "media-tools",
            label: "Media tools",
            value: dashboard.dependency_status,
            detail: dashboard.dependency_detail,
            state: dashboard.dependency_state,
            open_section: "media-settings",
            anchor: "media-settings",
            action: "review media tools",
        },
        jobs: DashboardSignal {
            key: "jobs",
            label: "Background jobs",
            value: dashboard.job_status,
            detail: dashboard.job_detail,
            state: dashboard.job_state,
            open_section: "site-health",
            anchor: "site-health",
            action: "inspect jobs",
        },
        reports: DashboardSignal {
            key: "reports",
            label: "Reports and appeals",
            value: dashboard.report_status,
            detail: dashboard.report_detail,
            state: dashboard.report_state,
            open_section: "reports",
            anchor: "reports",
            action: "review moderation",
        },
    }
}

/// Renders the aggregate status while ignoring intentionally neutral states.
fn render_dashboard_overall_status(signals: &DashboardSignals<'_>) -> String {
    let state = overall_state(&[
        signals.setup.state,
        signals.database.state,
        signals.backup.state,
        signals.storage.state,
        signals.tor.state,
        signals.dependencies.state,
        signals.jobs.state,
        signals.reports.state,
    ]);
    render_state_pill(state, overall_state_label(state))
}

/// Selects the highest relevant state for the aggregate status.
fn overall_state(states: &[AdminDashboardState]) -> AdminDashboardState {
    states
        .iter()
        .copied()
        .fold(AdminDashboardState::Ok, |current, state| {
            if state_severity(state) > state_severity(current) {
                state
            } else {
                current
            }
        })
}

/// Renders current warning, action-needed, and failure states before routine data.
fn render_attention(signals: &DashboardSignals<'_>) -> String {
    let mut alerts = [
        signals.setup,
        signals.database,
        signals.backup,
        signals.storage,
        signals.tor,
        signals.dependencies,
        signals.jobs,
        signals.reports,
    ]
    .into_iter()
    .filter(|signal| is_attention_state(signal.state))
    .collect::<Vec<_>>();
    alerts.sort_by_key(|signal| Reverse(state_severity(signal.state)));

    if alerts.is_empty() {
        return r#"<div class="admin-control-calm" data-dashboard-attention-count="0">
  <strong>No warning, action-needed, or failure states detected.</strong>
  <span>Review pending, disabled, informational, and unavailable states by task below.</span>
</div>"#
            .to_owned();
    }

    let mut rows = String::new();
    for signal in &alerts {
        let _ = write!(
            rows,
            r#"<li class="admin-control-attention-item admin-control-attention-item-{state}" data-dashboard-alert="{key}">
  <div class="admin-control-attention-copy">
    <span>{label}</span>
    <strong>{value}</strong>
    <p>{detail}</p>
  </div>
  {pill}
  {action}
</li>"#,
            state = state_class(signal.state),
            key = escape_html(signal.key),
            label = escape_html(signal.label),
            value = escape_html(signal.value),
            detail = escape_html(signal.detail),
            pill = render_state_pill(signal.state, state_label(signal.state)),
            action = section_action_link(signal.open_section, signal.anchor, signal.action),
        );
    }
    format!(
        r#"<section class="admin-control-attention" aria-labelledby="control-center-attention-title" data-dashboard-attention-count="{count}">
  <header>
    <h3 id="control-center-attention-title">// needs attention</h3>
    <span>{count} current</span>
  </header>
  <ul>{rows}</ul>
</section>"#,
        count = alerts.len(),
    )
}

/// Renders the compact primary task shortcuts near the top of the Control Center.
fn render_common_actions(signals: &DashboardSignals<'_>) -> String {
    let mut actions = String::new();
    if !is_attention_state(signals.reports.state) {
        actions.push_str(&section_action_link(
            "reports",
            "reports",
            "review moderation",
        ));
    }
    actions.push_str(&section_action_link("boards", "boards", "manage boards"));
    if !is_attention_state(signals.backup.state) {
        actions.push_str(&section_action_link(
            "full-backup-restore",
            "full-backup-restore",
            "manage backups",
        ));
    }
    actions.push_str(&section_action_link(
        "site-health",
        "site-health",
        "site health",
    ));
    actions.push_str(&section_action_link(
        "site-settings",
        "site-settings",
        "site settings",
    ));
    format!(
        r#"<nav class="admin-control-common-actions" aria-label="Common Control Center tasks">{actions}</nav>"#,
    )
}

/// Renders all compact task-oriented groups.
fn render_task_groups(view: &AdminPanelViewModel<'_>, signals: &DashboardSignals<'_>) -> String {
    [
        render_site_group(view, signals),
        render_moderation_group(view, signals),
        render_backups_group(view, signals),
        render_maintenance_group(view, signals),
        render_network_group(view, signals),
        render_configuration_group(signals),
    ]
    .concat()
}

/// Renders site identity, readiness, and storage.
fn render_site_group(view: &AdminPanelViewModel<'_>, signals: &DashboardSignals<'_>) -> String {
    let dashboard = &view.dashboard;
    render_task_group(
        "control-site-overview",
        "site overview and health",
        "Core readiness and storage state.",
        &[signals.database, signals.storage],
        &[
            ("Site", dashboard.site_title),
            ("Version", dashboard.version),
        ],
        &[
            section_action_link("site-health", "site-health", "health and diagnostics"),
            direct_action_link("/", "view site"),
        ]
        .concat(),
    )
}

/// Renders moderation queue state and neutral activity facts.
fn render_moderation_group(
    view: &AdminPanelViewModel<'_>,
    signals: &DashboardSignals<'_>,
) -> String {
    let dashboard = &view.dashboard;
    let mut actions = String::new();
    if !is_attention_state(signals.reports.state) {
        actions.push_str(&section_action_link(
            "reports",
            "reports",
            "review moderation",
        ));
    }
    actions.push_str(&direct_action_link("/admin/mod-log", "view mod log"));
    render_task_group(
        "control-moderation",
        "moderation and recent activity",
        "Current queue first, then neutral activity totals.",
        &[signals.reports],
        &[
            ("Boards", dashboard.board_count),
            ("Threads", dashboard.thread_count),
            ("Posts", dashboard.post_count),
            ("Recent", dashboard.recent_activity),
        ],
        &actions,
    )
}

/// Renders backup readiness and scheduling facts.
fn render_backups_group(view: &AdminPanelViewModel<'_>, signals: &DashboardSignals<'_>) -> String {
    let action = if is_attention_state(signals.backup.state) {
        String::new()
    } else {
        section_action_link(
            "full-backup-restore",
            "full-backup-restore",
            "manage backups",
        )
    };
    render_task_group(
        "control-backups",
        "backups and recovery",
        "Saved full-backup readiness without exposing restore actions here.",
        &[signals.backup],
        &[
            ("Last successful", view.site_health.last_successful_backup),
            ("Next scheduled", view.site_health.next_scheduled_backup),
        ],
        &action,
    )
}

/// Renders background work and optional media capability.
fn render_maintenance_group(
    view: &AdminPanelViewModel<'_>,
    signals: &DashboardSignals<'_>,
) -> String {
    render_task_group(
        "control-maintenance",
        "maintenance and background jobs",
        "Active work, failures, and optional media capability.",
        &[signals.jobs, signals.dependencies],
        &[("Active media", view.dashboard.media_summary)],
        &[
            section_action_link(
                "database-maintenance",
                "database-maintenance",
                "database maintenance",
            ),
            section_action_link("media-settings", "media-settings", "media settings"),
            section_action_link("live-log", "live-log", "live log"),
        ]
        .concat(),
    )
}

/// Renders public and onion entry-point state.
fn render_network_group(view: &AdminPanelViewModel<'_>, signals: &DashboardSignals<'_>) -> String {
    render_task_group(
        "control-network",
        "network and Tor",
        "Configured public entry point and onion-service state.",
        &[signals.tor],
        &[("Public URL", view.dashboard.public_url)],
        &[
            section_action_link("site-health", "tor-status", "Tor diagnostics"),
            section_action_link(
                "site-settings",
                "public-url-settings",
                "public URL settings",
            ),
        ]
        .concat(),
    )
}

/// Renders safe configuration shortcuts and the setup-state link.
fn render_configuration_group(signals: &DashboardSignals<'_>) -> String {
    render_task_group(
        "control-configuration",
        "configuration shortcuts",
        "Safe navigation to existing settings and rare setup controls.",
        &[signals.setup],
        &[],
        &[
            section_action_link("site-settings", "site-settings", "site settings"),
            section_action_link("boards", "boards", "manage or create boards"),
            section_action_link("theme-catalog", "theme-catalog", "appearance and themes"),
            section_action_link(
                "database-maintenance",
                "database-maintenance",
                "setup controls",
            ),
        ]
        .concat(),
    )
}

/// Renders one labelled task group.
fn render_task_group(
    id: &str,
    title: &str,
    description: &str,
    signals: &[DashboardSignal<'_>],
    facts: &[(&str, &str)],
    actions: &str,
) -> String {
    let status_rows = render_status_rows(signals);
    let fact_rows = render_fact_rows(facts);
    let action_nav = if actions.is_empty() {
        String::new()
    } else {
        format!(
            r#"<nav class="admin-control-group-actions" aria-label="{title} actions">{actions}</nav>"#,
            title = escape_html(title),
        )
    };
    format!(
        r#"<section class="admin-control-group" id="{id}" aria-labelledby="{id}-title">
  <header>
    <h3 id="{id}-title">// {title}</h3>
    <p>{description}</p>
  </header>
  {status_rows}
  {fact_rows}
  {action_nav}
</section>"#,
        id = escape_html(id),
        title = escape_html(title),
        description = escape_html(description),
    )
}

/// Renders semantic status rows for one task group.
fn render_status_rows(signals: &[DashboardSignal<'_>]) -> String {
    let mut rows = String::new();
    for signal in signals {
        let _ = write!(
            rows,
            r#"<li class="admin-control-status-item admin-control-status-item-{state}" data-dashboard-status="{key}" data-dashboard-state="{state}">
  <span class="admin-control-status-copy"><span>{label}</span><strong>{value}</strong></span>
  {pill}
</li>"#,
            state = state_class(signal.state),
            key = escape_html(signal.key),
            label = escape_html(signal.label),
            value = escape_html(signal.value),
            pill = render_state_pill(signal.state, state_label(signal.state)),
        );
    }
    format!(r#"<ul class="admin-control-status-list">{rows}</ul>"#)
}

/// Renders neutral label-value facts without attaching health semantics.
fn render_fact_rows(facts: &[(&str, &str)]) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut rows = String::new();
    for (label, value) in facts {
        let _ = write!(
            rows,
            r"<div><dt>{label}</dt><dd>{value}</dd></div>",
            label = escape_html(label),
            value = escape_html(value),
        );
    }
    format!(r#"<dl class="admin-control-facts">{rows}</dl>"#)
}

/// Renders secondary technical data in a native no-JavaScript disclosure.
fn render_system_details(view: &AdminPanelViewModel<'_>, signals: &DashboardSignals<'_>) -> String {
    let dashboard = &view.dashboard;
    let details = [
        signals.database,
        signals.storage,
        signals.backup,
        signals.jobs,
        signals.dependencies,
        signals.tor,
        signals.setup,
        signals.reports,
    ];
    let mut detail_rows = String::new();
    for signal in details {
        let _ = write!(
            detail_rows,
            r"<div><dt>{label}</dt><dd>{detail}</dd></div>",
            label = escape_html(signal.label),
            detail = escape_html(signal.detail),
        );
    }
    let technical_facts = render_fact_rows(&[
        ("Build", dashboard.build),
        ("Thread totals", dashboard.thread_count),
        ("Recent activity", dashboard.recent_activity),
        ("Media totals", dashboard.media_summary),
    ]);
    let supporting_links = [
        section_action_link("site-health", "site-health", "site health and diagnostics"),
        section_action_link("live-log", "live-log", "live log"),
        direct_action_link("/admin/mod-log", "moderation log"),
    ]
    .concat();

    format!(
        r#"<details class="admin-control-system-details">
  <summary>system details, logs, and diagnostics</summary>
  <div class="admin-control-system-details-content">
    <dl class="admin-control-detail-list">{detail_rows}</dl>
    {technical_facts}
    <nav class="admin-control-group-actions" aria-label="System detail actions">{supporting_links}</nav>
  </div>
</details>"#,
    )
}

/// Renders a section link with a server fallback and same-page enhancement hooks.
fn section_action_link(open_section: &str, anchor: &str, label: &str) -> String {
    format!(
        r#"<a class="admin-link-button admin-control-action" href="/admin/panel?open={open_section}#{anchor}" data-open-admin-section="{open_section}" data-open-admin-anchor="{anchor}">{label}</a>"#,
        open_section = escape_html(open_section),
        anchor = escape_html(anchor),
        label = escape_html(label),
    )
}

/// Renders an ordinary Control Center action link.
fn direct_action_link(href: &str, label: &str) -> String {
    format!(
        r#"<a class="admin-link-button admin-control-action" href="{href}">{label}</a>"#,
        href = escape_html(href),
        label = escape_html(label),
    )
}

/// Renders a compact textual status pill.
fn render_state_pill(state: AdminDashboardState, label: &str) -> String {
    format!(
        r#"<span class="admin-state-pill admin-state-pill-{class}" aria-label="Status: {label}">{label}</span>"#,
        class = state_class(state),
        label = escape_html(label),
    )
}

/// Returns whether a state belongs in the urgent attention list.
const fn is_attention_state(state: AdminDashboardState) -> bool {
    matches!(
        state,
        AdminDashboardState::Warning
            | AdminDashboardState::ActionNeeded
            | AdminDashboardState::Failure
    )
}

/// Returns the ordering weight used by attention and overall status aggregation.
const fn state_severity(state: AdminDashboardState) -> u8 {
    match state {
        AdminDashboardState::Failure => 5,
        AdminDashboardState::ActionNeeded => 4,
        AdminDashboardState::Warning => 3,
        AdminDashboardState::Unknown => 2,
        AdminDashboardState::Pending => 1,
        AdminDashboardState::Ok
        | AdminDashboardState::Informational
        | AdminDashboardState::Disabled => 0,
    }
}

/// Returns the CSS suffix for a dashboard state.
const fn state_class(state: AdminDashboardState) -> &'static str {
    match state {
        AdminDashboardState::Ok => "ok",
        AdminDashboardState::Informational => "informational",
        AdminDashboardState::Pending => "pending",
        AdminDashboardState::Warning => "warning",
        AdminDashboardState::ActionNeeded => "action-needed",
        AdminDashboardState::Failure => "failure",
        AdminDashboardState::Disabled => "disabled",
        AdminDashboardState::Unknown => "unknown",
    }
}

/// Returns the visible label for a dashboard state.
const fn state_label(state: AdminDashboardState) -> &'static str {
    match state {
        AdminDashboardState::Ok => "healthy",
        AdminDashboardState::Informational => "information",
        AdminDashboardState::Pending => "in progress",
        AdminDashboardState::Warning => "warning",
        AdminDashboardState::ActionNeeded => "action needed",
        AdminDashboardState::Failure => "failure",
        AdminDashboardState::Disabled => "disabled",
        AdminDashboardState::Unknown => "not checked",
    }
}

/// Returns the concise aggregate label for a dashboard state.
const fn overall_state_label(state: AdminDashboardState) -> &'static str {
    match state {
        AdminDashboardState::Ok
        | AdminDashboardState::Informational
        | AdminDashboardState::Disabled => "Operational",
        AdminDashboardState::Pending => "Work in progress",
        AdminDashboardState::Warning => "Warning",
        AdminDashboardState::ActionNeeded => "Action needed",
        AdminDashboardState::Failure => "Failure",
        AdminDashboardState::Unknown => "Review status",
    }
}

#[cfg(test)]
mod tests {
    use super::overall_state;
    use crate::templates::AdminDashboardState;

    #[test]
    fn neutral_states_do_not_override_operational_overall_status() {
        assert_eq!(
            overall_state(&[
                AdminDashboardState::Ok,
                AdminDashboardState::Informational,
                AdminDashboardState::Disabled,
            ]),
            AdminDashboardState::Ok,
        );
    }

    #[test]
    fn pending_and_unknown_states_remain_distinct() {
        assert_eq!(
            overall_state(&[AdminDashboardState::Ok, AdminDashboardState::Pending]),
            AdminDashboardState::Pending,
        );
        assert_eq!(
            overall_state(&[AdminDashboardState::Pending, AdminDashboardState::Unknown,]),
            AdminDashboardState::Unknown,
        );
    }

    #[test]
    fn failures_take_priority_over_actionable_and_warning_states() {
        assert_eq!(
            overall_state(&[
                AdminDashboardState::Warning,
                AdminDashboardState::ActionNeeded,
                AdminDashboardState::Failure,
            ]),
            AdminDashboardState::Failure,
        );
    }
}
