//! Page templates for the administrative interface.

use crate::db::DbHealthReport;
use crate::models::{BackupInfo, Ban, BannerAsset, Board, WordFilter};
use crate::utils::{files::format_file_size, sanitize::escape_html};
use std::collections::BTreeSet;

use super::{base_layout, fmt_ts, fmt_ts_short, render_pagination, urlencoding_simple};

// Admin login
#[must_use]
/// Renders the administrator login form and an optional authentication error.
pub fn admin_login_page(
    error: Option<&str>,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
) -> String {
    let err_html = error
        .map(|e| {
            format!(
                r#"<div class="error admin-login-error" role="alert">{}</div>"#,
                escape_html(e)
            )
        })
        .unwrap_or_default();

    let body = format!(
        r#"<div class="page-box admin-login">
<div class="admin-login-header">
  <h1>Admin Login</h1>
  <p>Sign in to manage boards, moderation, backups, and site settings.</p>
</div>
{err}
<form method="POST" action="/admin/login" class="admin-login-form">
<input type="hidden" name="_csrf" value="{csrf}">
<label class="admin-login-field">Username
  <input type="text" name="username" autofocus required autocomplete="username">
</label>
<label class="admin-login-field">Password
  <input type="password" name="password" required autocomplete="current-password">
</label>
<div class="admin-login-actions">
  <button type="submit">authenticate</button>
</div>
</form>
</div>"#,
        err = err_html,
        csrf = escape_html(csrf_token),
    );
    base_layout(
        "admin login",
        None,
        &body,
        csrf_token,
        boards,
        current_theme,
        None,
        false,
        "/admin",
    )
}

// Admin panel
mod accounts;
/// Appearance-section rendering.
mod appearance;
mod application;
/// Backup-section rendering.
mod backups;
/// Board-section rendering.
mod boards;
/// Task-oriented Control Center rendering.
mod control_center;
/// Admin page layout and dashboard rendering.
mod layout;
/// Maintenance-section rendering.
mod maintenance;
mod management;
/// Moderation-section rendering.
mod moderation;
/// Network settings rendering.
mod network;
/// Settings restart banner and explicit form.
mod restart;
/// Server-rendered settings search.
mod search;
/// Site-health rendering.
mod site_health;
/// Software update and pre-upgrade backup views.
mod updates;

/// Render authenticated network configuration status and controls.
#[must_use]
pub fn render_network_settings(
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
    csrf: &str,
) -> String {
    network::render(fields, csrf)
}

/// Render a related restart-required process configuration section.
#[must_use]
pub fn render_runtime_settings(
    section: crate::config::admin::runtime::RuntimeSection,
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
    csrf: &str,
) -> String {
    network::render_section(fields, csrf, section.key(), section.label(), &format!("/admin/config/{}", section.route_key()), "Process configuration. Existing board preferences and live maintenance tools remain available in their sections.")
}

/// Complete input model for the administrator control panel.
#[derive(Debug)]
pub struct AdminPanelViewModel<'a> {
    /// CSRF token embedded in state-changing forms.
    pub csrf_token: &'a str,
    /// Boards visible to administrative controls and global navigation.
    pub boards: &'a [Board],
    /// Visitor-selected theme, when present.
    pub current_theme: Option<&'a str>,
    /// Authenticated network configuration controls.
    pub network_html: String,
    /// Authenticated grouped runtime settings.
    pub runtime_html: String,
    /// Optional runtime settings search query.
    pub settings_query: Option<&'a str>,
    /// Control-center dashboard data.
    pub dashboard: AdminPanelDashboardView<'a>,
    /// Moderation data.
    pub moderation: AdminPanelModerationView<'a>,
    /// Site appearance data.
    pub appearance: AdminPanelAppearanceView<'a>,
    /// Runtime health data.
    pub site_health: AdminPanelSiteHealthView<'a>,
    /// Backup configuration and saved backup data.
    pub backups: AdminPanelBackupsView<'a>,
    /// Database and media maintenance data.
    pub maintenance: AdminPanelMaintenanceView,
    /// Active onion address, when Tor is running.
    pub tor_address: Option<&'a str>,
    /// Optional result message displayed above the panel.
    pub flash: Option<AdminPanelFlash<'a>>,
    /// Section requested by the current URL fragment or redirect.
    pub open_section: Option<&'a str>,
}

/// Values displayed by the operational control-center dashboard.
#[derive(Debug)]
pub struct AdminPanelDashboardView<'a> {
    /// Application version.
    pub version: &'a str,
    /// Build identifier.
    pub build: &'a str,
    /// Compact setup-state label.
    pub setup_status: &'a str,
    /// Detailed setup-state explanation.
    pub setup_detail: &'a str,
    /// Severity of the setup state.
    pub setup_state: AdminDashboardState,
    /// Configured site title.
    pub site_title: &'a str,
    /// Configured public entry point.
    pub public_url: &'a str,
    /// Compact database-health label.
    pub db_status: &'a str,
    /// Detailed database-health explanation.
    pub db_detail: &'a str,
    /// Severity of the database state.
    pub db_state: AdminDashboardState,
    /// Compact backup-health label.
    pub backup_status: &'a str,
    /// Detailed backup-health explanation.
    pub backup_detail: &'a str,
    /// Severity of the backup state.
    pub backup_state: AdminDashboardState,
    /// Compact storage-health label.
    pub storage_status: &'a str,
    /// Detailed storage-health explanation.
    pub storage_detail: &'a str,
    /// Severity of the storage state.
    pub storage_state: AdminDashboardState,
    /// Compact Tor-health label.
    pub tor_status: &'a str,
    /// Detailed Tor-health explanation.
    pub tor_detail: &'a str,
    /// Severity of the Tor state.
    pub tor_state: AdminDashboardState,
    /// Compact media-dependency label.
    pub dependency_status: &'a str,
    /// Detailed media-dependency explanation.
    pub dependency_detail: &'a str,
    /// Severity of the dependency state.
    pub dependency_state: AdminDashboardState,
    /// Compact background-job label.
    pub job_status: &'a str,
    /// Detailed background-job explanation.
    pub job_detail: &'a str,
    /// Severity of the background-job state.
    pub job_state: AdminDashboardState,
    /// Human-readable board count.
    pub board_count: &'a str,
    /// Human-readable thread count.
    pub thread_count: &'a str,
    /// Human-readable post count.
    pub post_count: &'a str,
    /// Recent posting activity summary.
    pub recent_activity: &'a str,
    /// Active media summary.
    pub media_summary: &'a str,
    /// Compact report-queue label.
    pub report_status: &'a str,
    /// Detailed report-queue explanation.
    pub report_detail: &'a str,
    /// Severity of the report-queue state.
    pub report_state: AdminDashboardState,
}

/// Severity used to style and order dashboard signals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminDashboardState {
    /// Healthy or complete.
    Ok,
    /// Neutral context that does not require administrator action.
    Informational,
    /// Normal work is actively progressing.
    Pending,
    /// Degraded or worth reviewing.
    Warning,
    /// Requires administrator action.
    ActionNeeded,
    /// An operation or required capability failed.
    Failure,
    /// Intentionally unavailable.
    Disabled,
    /// State could not be determined.
    Unknown,
}

/// Moderation queues and rules displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelModerationView<'a> {
    /// Active bans.
    pub bans: &'a [Ban],
    /// Configured word filters.
    pub filters: &'a [WordFilter],
    /// Open reports enriched with post context.
    pub reports: &'a [crate::models::ReportWithContext],
    /// Pending ban appeals.
    pub appeals: &'a [crate::models::BanAppeal],
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "the view mirrors independent administrator appearance toggles"
)]
/// Appearance settings and assets displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelAppearanceView<'a> {
    /// Configured site name.
    pub site_name: &'a str,
    /// Configured site subtitle.
    pub site_subtitle: &'a str,
    /// Whether homepage new-thread badges are enabled.
    pub homepage_new_thread_badges_enabled: bool,
    /// Whether homepage new-reply badges are enabled.
    pub homepage_new_reply_badges_enabled: bool,
    /// Whether thread-page new-reply badges are enabled.
    pub thread_new_reply_badges_enabled: bool,
    /// Configured default theme slug.
    pub default_theme: &'a str,
    /// Banner rotation interval in minutes.
    pub banner_rotation_interval_minutes: i64,
    /// Whether banners may link to external sites.
    pub banner_external_links_enabled: bool,
    /// Available themes.
    pub themes: &'a [crate::models::Theme],
    /// Banners shared by board pages.
    pub global_banners: &'a [BannerAsset],
    /// Banners displayed on the homepage.
    pub home_banners: &'a [BannerAsset],
    /// Board-specific banners.
    pub board_banners: &'a [BannerAsset],
}

/// Backup settings and saved backup inventory displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelBackupsView<'a> {
    /// Saved full-site backups.
    pub full_backups: &'a [BackupInfo],
    /// Saved board-only backups.
    pub board_backups: &'a [BackupInfo],
    /// Current automatic-backup status.
    pub backup_status_line: &'a str,
    /// Optional backup warning.
    pub backup_warning: Option<&'a str>,
    /// Scheduled full-backup interval in hours.
    pub auto_full_backup_interval_hours: u64,
    /// Number of scheduled full backups retained.
    pub auto_full_backup_copies_to_keep: u64,
    /// Whether scheduled backups include onion-service keys.
    pub auto_full_backup_include_tor_hidden_service_keys: bool,
    /// Storage mode for scheduled full backups.
    pub auto_full_backup_storage_mode: &'a str,
    /// Part size used for split ZIP archives.
    pub auto_full_backup_split_zip_part_size_gib: u64,
    /// Whether onion-service keys are available to back up.
    pub tor_hidden_service_key_backup_available: bool,
}

/// Runtime and dependency health values displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelSiteHealthView<'a> {
    /// Overall server status.
    pub server_status: &'a str,
    /// Running `RustChan` version.
    pub rustchan_version: &'a str,
    /// Database schema status.
    pub database_schema_status: &'a str,
    /// Database integrity status.
    pub database_integrity_status: &'a str,
    /// Time of the last successful backup.
    pub last_successful_backup: &'a str,
    /// Time of the next scheduled backup.
    pub next_scheduled_backup: &'a str,
    /// Data-directory disk usage.
    pub data_dir_usage: &'a str,
    /// Upload-directory size.
    pub upload_dir_size: &'a str,
    /// Overall Tor status.
    pub tor_status: &'a str,
    /// Current onion address.
    pub tor_onion_address: Option<&'a str>,
    /// Onion-service runtime status.
    pub tor_service_status: &'a str,
    /// Configured Tor mode.
    pub tor_mode: &'a str,
    /// Safe Tor configuration summary.
    pub tor_config_summary: &'a str,
    /// Detailed Tor status.
    pub tor_detail: &'a str,
    /// Optional media dependency detection summary.
    pub dependency_summary: AdminSiteHealthDependencySummary,
    /// Number of currently running jobs.
    pub running_jobs: i64,
    /// Number of queued jobs.
    pub queued_jobs: i64,
    /// Number of recently completed jobs.
    pub recent_completed_jobs: i64,
    /// Number of failed jobs awaiting dismissal.
    pub failed_jobs: i64,
    /// Backup-job status summary.
    pub backup_jobs: &'a str,
    /// Restore-job status summary.
    pub restore_jobs: &'a str,
    /// Copyable diagnostic report.
    pub diagnostics_text: &'a str,
}

/// Detection summary for optional media-processing capabilities.
#[derive(Clone, Copy, Debug)]
pub struct AdminSiteHealthDependencySummary {
    /// `ffmpeg` detection state.
    pub ffmpeg: AdminDetectionStatus,
    /// AV1 decoder availability; independent of encoding.
    pub av1_decoder: AdminDetectionStatus,
    /// AV1 encoder availability; output policy still uses VP9.
    pub av1_encoder: AdminDetectionStatus,
    /// VP9 pipeline detection state.
    pub vp9: AdminDetectionStatus,
    /// Opus encoder detection state.
    pub opus: AdminDetectionStatus,
}

/// Database and media maintenance settings displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelMaintenanceView {
    /// Current database file size in bytes.
    pub db_size_bytes: i64,
    /// Whether the database size exceeds its warning threshold.
    pub db_size_warning: bool,
    /// Setup wizard availability state.
    pub setup_status: AdminPanelSetupStatus,
    /// External media process timeout in seconds.
    pub ffmpeg_timeout_secs: u64,
    /// Whether automatic active-media pruning is enabled.
    pub media_auto_prune_enabled: bool,
    /// Maximum active database and media size before pruning.
    pub media_max_active_content_size_bytes: u64,
    /// Detected media-processing capabilities.
    pub media_detection: AdminMediaDetectionView,
}

/// Durable setup state relevant to administrator controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminPanelSetupStatus {
    /// Initial setup remains publicly available.
    Available,
    /// Initial setup completed normally.
    Complete,
    /// An administrator temporarily reopened setup.
    Reopened,
    /// Durable state exists even though no completion marker was found.
    Initialized,
}

/// Whether an optional executable or pipeline was detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminDetectionStatus {
    /// The capability is available.
    Detected,
    /// The capability is unavailable.
    Missing,
}

impl AdminDetectionStatus {
    /// Returns whether the capability was detected.
    #[must_use]
    pub const fn is_detected(self) -> bool {
        matches!(self, Self::Detected)
    }
}

/// Detailed media capability detection results.
#[derive(Debug)]
pub struct AdminMediaDetectionView {
    /// `ffmpeg` detection state.
    pub ffmpeg: AdminDetectionStatus,
    /// AV1 decoder availability; independent of encoding.
    pub av1_decoder: AdminDetectionStatus,
    /// AV1 encoder availability; output policy still uses VP9.
    pub av1_encoder: AdminDetectionStatus,
    /// VP9 and Opus pipeline detection state.
    pub vp9_pipeline: AdminDetectionStatus,
    /// Selected PDF thumbnail executable, when available.
    pub pdf_thumbnail_renderer: Option<String>,
}

/// Message displayed after an administrative action.
#[derive(Clone, Copy, Debug)]
pub struct AdminPanelFlash<'a> {
    /// Whether the message represents an error.
    pub is_error: bool,
    /// User-facing result text.
    pub message: &'a str,
}

/// Builds the settings form id shared by the board settings and appearance cards.
fn board_settings_form_id(board_id: i64) -> String {
    format!("board-settings-form-{board_id}")
}

#[must_use]
/// Renders the main administrator control panel.
pub fn admin_panel_page(view: &AdminPanelViewModel<'_>) -> String {
    layout::render(view)
}

// Moderation log
#[must_use]
/// Renders the paginated moderation log.
pub fn mod_log_page(
    entries: &[crate::models::ModLogEntry],
    pagination: &crate::models::Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
) -> String {
    let mut rows = String::new();
    if entries.is_empty() {
        rows.push_str(r#"<tr><td colspan="6" class="admin-table-empty">no entries yet</td></tr>"#);
    }
    for e in entries {
        let target = e.target_id.map_or_else(
            || e.target_type.clone(),
            |id| format!("{} #{id}", e.target_type),
        );
        let board_link = if e.board_short.is_empty() {
            String::new()
        } else {
            format!(r#"<a href="/{s}">{s}</a>"#, s = escape_html(&e.board_short))
        };
        crate::templates::append_html(
            &mut rows,
            format_args!(
                r#"<tr>
<td style="white-space:nowrap;font-size:0.78rem">{time}</td>
<td class="admin-log-user">{admin}</td>
<td><code>{action}</code></td>
<td style="font-size:0.82rem">{target}</td>
<td>{board}</td>
<td><details class="admin-log-detail"><summary>View detail</summary><p>{detail}</p></details></td>
</tr>"#,
                time = escape_html(&fmt_ts(e.created_at)),
                admin = escape_html(&e.admin_name),
                action = escape_html(&e.action),
                target = escape_html(&target),
                board = board_link,
                detail = escape_html(&e.detail)
            ),
        );
    }

    let pagination_html = render_pagination(pagination, "/admin/mod-log");

    let body = format!(
        r#"<div class="page-box admin-detail-page">
<div class="board-header">
  <a href="/admin/panel">[ back to panel ]</a>
  <h2 style="margin:0.5rem 0 0.25rem">// moderation log</h2>
  <p style="color:var(--text-dim);font-size:0.82rem">{total} total entries</p>
</div>
<div class="admin-table-wrap" tabindex="0" role="region" aria-label="Moderation log">
<table class="admin-table admin-log-table">
<thead><tr>
  <th>time</th><th>admin</th><th>action</th><th>target</th><th>board</th><th>detail</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>
</div>
{pagination}
</div>"#,
        total = pagination.total,
        rows = rows,
        pagination = pagination_html,
    );

    base_layout(
        "mod log — admin",
        None,
        &body,
        csrf_token,
        boards,
        current_theme,
        None,
        false,
        "/admin/log",
    )
}

// VACUUM result
#[must_use]
/// Renders the space reclaimed by a completed `SQLite` `VACUUM`.
pub fn admin_vacuum_result_page(
    size_before: i64,
    size_after: i64,
    csrf_token: &str,
    current_theme: Option<&str>,
) -> String {
    let saved = size_before.saturating_sub(size_after);
    let pct = if size_before > 0 {
        saved
            .max(0)
            .saturating_mul(100)
            .checked_div(size_before)
            .unwrap_or(0)
    } else {
        0
    };

    let body = format!(
        r#"<div class="admin-panel">
<h1>[ VACUUM complete ]</h1>
<section class="admin-section">
<h2>// result</h2>
<div class="admin-table-wrap admin-table-wrap-compact">
<table class="admin-table admin-result-table">
<tbody>
  <tr><td>Before</td><td><strong>{before}</strong></td></tr>
  <tr><td>After</td><td><strong>{after}</strong></td></tr>
  <tr><td>Reclaimed</td><td><strong class="admin-status-ok">{saved}</strong> ({pct}%)</td></tr>
</tbody>
</table>
</div>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        before = format_file_size(size_before),
        after = format_file_size(size_after),
        saved = format_file_size(saved),
        pct = pct,
    );

    base_layout(
        "VACUUM result",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the health report keeps one cohesive diagnostic result document"
)]
#[must_use]
/// Renders database health checks and an optional repair result.
pub fn admin_db_health_result_page(
    report: &DbHealthReport,
    attempted_repair: bool,
    csrf_token: &str,
    repair_job_id: Option<u64>,
    current_theme: Option<&str>,
) -> String {
    let title = if attempted_repair {
        "[ database repair ]"
    } else {
        "[ database check ]"
    };
    let status_line = if attempted_repair {
        if report.repair_backup_error.is_some() {
            r#"<p class="error">Repair was not run because the pre-repair backup failed.</p>"#
        } else {
            match report.after.as_ref().map(crate::db::DbHealthSnapshot::ok) {
                Some(true) => {
                    r#"<p class="admin-result-status admin-status-ok">Maintenance completed. Database health checks passed afterward.</p>"#
                }
                Some(false) => {
                    r#"<p class="error">Repair finished, but the database still reports a problem. Restoring a known-good full backup is recommended.</p>"#
                }
                None => {
                    r#"<p class="error">Repair finished, but no final health result was produced.</p>"#
                }
            }
        }
    } else if report.before.ok() {
        r#"<p class="admin-result-status admin-status-ok">Database health checks passed.</p>"#
    } else {
        r#"<p class="error">Database health checks found a problem.</p>"#
    };
    let repair_action = if attempted_repair {
        String::new()
    } else {
        let (label, confirm) = if report.before.ok() {
            (
                "&#x1F6E0; run maintenance rebuild",
                "Run maintenance rebuild? This will create a Backup v4 DB + config pre-maintenance backup, then run REINDEX, rebuild the search index, recreate its triggers, and optimize SQLite statistics. Continue?",
            )
        } else {
            (
                "&#x1F6E0; attempt repair",
                "Attempt database repair? This will create a Backup v4 DB + config pre-maintenance backup, then run integrity checks, REINDEX, and rebuild the search index. It may not fix true file corruption. Continue?",
            )
        };
        format!(
            r#"<form method="POST" action="/admin/db/repair" class="admin-result-action-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="{confirm}">{label}</button>
</form>"#,
            csrf = escape_html(csrf_token),
            confirm = escape_html(confirm),
            label = label,
        )
    };

    let mut repair_summary_html = String::new();
    if report.repair_summary.is_empty() {
        repair_summary_html
            .push_str(r#"<li class="admin-muted-list-item">No repairs were run.</li>"#);
    } else {
        for line in &report.repair_summary {
            crate::templates::append_html(
                &mut repair_summary_html,
                format_args!(r"<li>{line}</li>", line = escape_html(line)),
            );
        }
    }

    let mut repair_steps_html = String::new();
    if report.repair_steps.is_empty() {
        repair_steps_html
            .push_str(r#"<li class="admin-muted-list-item">No maintenance steps were run.</li>"#);
    } else {
        for step in &report.repair_steps {
            crate::templates::append_html(
                &mut repair_steps_html,
                format_args!(r"<li>{step}</li>", step = escape_html(step)),
            );
        }
    }

    let backup_html = report.repair_backup.as_ref().map_or_else(
        || {
            report.repair_backup_error.as_ref().map_or_else(
                || r"<p><strong>Pre-repair backup:</strong> Not run</p>".to_owned(),
                |error| {
                    format!(
                        r#"<p><strong>Pre-repair backup:</strong> <span class="admin-status-error">Failed</span> <code>{}</code></p>"#,
                        escape_html(error)
                    )
                },
            )
        },
        |backup| {
            format!(
                r"<p><strong>Pre-repair backup:</strong> <code>{}</code></p>
<p><strong>Pre-repair backup type:</strong> {}</p>
<p><strong>Verification status:</strong> {}</p>
<p><strong>Backup path:</strong> <code>{}</code></p>",
                escape_html(&backup.backup_id),
                escape_html(&backup.backup_type),
                if backup.verified { "Verified" } else { "Unverified" },
                escape_html(&backup.backup_path)
            )
        },
    );
    let before_checks_html = render_db_health_snapshot(&report.before);
    let after_checks_html = report.after.as_ref().map_or_else(
        || r"<p><strong>After:</strong> Not run</p>".to_owned(),
        render_db_health_snapshot,
    );

    let body = format!(
        r#"<div class="admin-panel">
<h1>{title}</h1>
<section class="admin-section">
<h2>// summary</h2>
{status_line}
<div class="admin-result-card">
<p><strong>Before:</strong> {before_status}</p>
{before_checks}
<p><strong>Repair run:</strong> {repair_attempted}</p>
{repair_job_id_html}
{backup}
<p><strong>After:</strong> {after_status}</p>
{after_checks}
</div>
<h2 class="admin-result-heading">// repair outcome</h2>
<ul class="admin-result-list">
{repair_summary}
</ul>
<h2 class="admin-result-heading">// maintenance actions run</h2>
<ul class="admin-result-list">
{repair_steps}
</ul>
{repair_action}
<p class="admin-result-note">
  Run checks after restores or large deletes. Take a backup before repair; this repair flow now creates a Backup v4 DB + config pre-maintenance snapshot before making changes.
  This tool can repair index and search-index issues, but true SQLite file corruption may still require restoring a known-good backup.
</p>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        title = title,
        status_line = status_line,
        before_status = if report.before.ok() {
            r#"<span class="admin-status-ok">Passed</span>"#
        } else {
            r#"<span class="admin-status-error">Problem found</span>"#
        },
        before_checks = before_checks_html,
        repair_attempted = if report.repair_attempted { "Yes" } else { "No" },
        repair_job_id_html = repair_job_id.map_or_else(String::new, |job_id| {
            format!(r"<p><strong>Run id:</strong> <code>{job_id}</code></p>")
        }),
        backup = backup_html,
        after_status = match report.after.as_ref().map(crate::db::DbHealthSnapshot::ok) {
            Some(true) => r#"<span class="admin-status-ok">Passed</span>"#,
            Some(false) => r#"<span class="admin-status-error">Problem found</span>"#,
            None => "Not run",
        },
        after_checks = after_checks_html,
        repair_summary = repair_summary_html,
        repair_steps = repair_steps_html,
        repair_action = repair_action,
    );

    base_layout(
        "Database health",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
/// Renders the database-repair status page when no job is active.
pub fn admin_db_repair_idle_page(csrf_token: &str, current_theme: Option<&str>) -> String {
    let body = r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild</h2>
<div class="admin-result-card">
<p>No maintenance rebuild is running.</p>
<p class="admin-meta-note">Start a new maintenance rebuild from the admin panel when you need to create a backup and rebuild indexes.</p>
</div>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#.to_owned();

    base_layout(
        "Database repair",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
/// Renders live progress for an active database-repair job.
pub fn admin_db_repair_running_page(
    csrf_token: &str,
    job_id: u64,
    started_at: i64,
    current_theme: Option<&str>,
) -> String {
    let progress_url = format!("/admin/db/repair/progress?job_id={job_id}");
    let status_url = format!("/admin/db/repair/status?job_id={job_id}");
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild running</h2>
<div class="admin-result-card">
<p>Maintenance rebuild started at <code>{started_at}</code>.</p>
<div class="compress-progress admin-progress-spaced" data-db-repair-progress data-db-repair-job-id="{job_id}" data-db-repair-progress-url="{progress_url}">
  <div class="compress-progress-track"><div class="compress-progress-bar admin-progress-bar-start" data-db-repair-progress-bar></div></div>
  <div class="compress-progress-text" data-db-repair-progress-text>Starting maintenance rebuild...</div>
</div>
<p class="admin-meta-note">This page updates live while the backup and database rebuild finish.</p>
</div>
<p class="admin-result-actions">
  <a href="{status_url}">refresh status</a> · <a href="/admin/panel">back to admin panel</a>
</p>
</section>
</div>"#
    );

    base_layout(
        "Database repair running",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
/// Renders a stale database-repair job status request.
pub fn admin_db_repair_stale_page(
    csrf_token: &str,
    requested_job_id: u64,
    current_job_id: Option<u64>,
    current_theme: Option<&str>,
) -> String {
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild status</h2>
<div class="admin-result-card">
<p class="error">This page is for maintenance rebuild <code>{requested_job_id}</code>, but that run is no longer the current status.</p>
{current_job_html}
</div>
<p class="admin-result-actions">
  <a href="/admin/db/repair/status">current status</a> · <a href="/admin/panel">back to admin panel</a>
</p>
</section>
</div>"#,
        current_job_html = current_job_id.map_or_else(
            || "<p>No maintenance rebuild is currently active.</p>".to_owned(),
            |job_id| {
                format!(
                    r#"<p>The current maintenance rebuild is <code>{job_id}</code>. <a href="/admin/db/repair/status?job_id={job_id}">Open that status page.</a></p>"#
                )
            }
        ),
    );

    base_layout(
        "Database repair status",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
/// Renders the terminal failure state of a database-repair job.
pub fn admin_db_repair_failed_page(
    csrf_token: &str,
    message: &str,
    finished_at: i64,
    job_id: u64,
    current_theme: Option<&str>,
) -> String {
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild failed</h2>
<div class="admin-result-card">
<p class="error">The background maintenance rebuild failed.</p>
<p><strong>Run id:</strong> <code>{job_id}</code></p>
<p><strong>Finished:</strong> <code>{finished_at}</code></p>
<p><strong>Error:</strong> <code>{message}</code></p>
</div>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        job_id = job_id,
        message = escape_html(message),
    );

    base_layout(
        "Database repair failed",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

/// Renders schema, integrity, and foreign-key results for one health snapshot.
fn render_db_health_snapshot(snapshot: &crate::db::DbHealthSnapshot) -> String {
    format!(
        "{schema}{integrity}{foreign_keys}",
        schema = render_db_check_result("schema baseline", &snapshot.schema),
        integrity = render_db_check_result("integrity check", &snapshot.integrity),
        foreign_keys = render_db_check_result("foreign key check", &snapshot.foreign_keys),
    )
}

/// Renders one database check and its detailed output when needed.
fn render_db_check_result(label: &str, result: &crate::db::DbCheckResult) -> String {
    let status = if result.ok {
        r#"<span class="admin-status-ok">Passed</span>"#
    } else {
        r#"<span class="admin-status-error">Problem found</span>"#
    };
    let output = result.output();
    if result.ok || result.messages.len() <= 1 {
        return format!(
            r"<p><strong>{label}:</strong> {status} <code>{output}</code></p>",
            label = label,
            status = status,
            output = escape_html(&output),
        );
    }

    format!(
        r#"<p><strong>{label}:</strong> {status}. Found {count} issues.</p>
<details class="admin-result-details">
  <summary>show full {label} output</summary>
  <pre>{output}</pre>
</details>"#,
        label = label,
        status = status,
        count = result.messages.len(),
        output = escape_html(&result.messages.join("\n")),
    )
}

// IP history
#[expect(
    clippy::too_many_lines,
    reason = "the investigation page keeps its summary, results, and controls together"
)]
#[must_use]
/// Renders posts and identity hints associated with a hashed IP.
pub fn admin_ip_history_page(
    ip_hash: &str,
    posts_with_boards: &[(crate::models::Post, String)],
    pagination: &crate::models::Pagination,
    all_boards: &[Board],
    csrf_token: &str,
    return_to: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    use crate::models::MediaType;

    let mut rows = String::new();
    let mut seen_names = BTreeSet::new();
    let mut seen_tripcodes = BTreeSet::new();

    for (post, _) in posts_with_boards {
        let name = post.name.trim();
        if !name.is_empty() && name != "Anonymous" {
            let _was_new_name = seen_names.insert(name.to_owned());
        }

        if let Some(tripcode) = post
            .tripcode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let _was_new_tripcode = seen_tripcodes.insert(tripcode.to_owned());
        }
    }

    if posts_with_boards.is_empty() {
        rows.push_str(r#"<tr><td colspan="8" style="color:var(--text-dim);text-align:center">no posts found for this hashed IP</td></tr>"#);
    }

    for (post, board_short) in posts_with_boards {
        let media_badge = match &post.media_type {
            Some(MediaType::Image) => r#"<span style="color:var(--green-bright)">[img]</span>"#,
            Some(MediaType::Video) => r#"<span style="color:var(--text-dim)">[vid]</span>"#,
            Some(MediaType::Audio) => r#"<span style="color:var(--text-dim)">[aud]</span>"#,
            Some(MediaType::Pdf) => r#"<span style="color:var(--text-dim)">[pdf]</span>"#,
            Some(MediaType::Other) => r#"<span style="color:var(--text-dim)">[file]</span>"#,
            None => "",
        };
        let thread_link = format!(
            r#"<a class="quotelink crosslink" href="/{board}/thread/{tid}#p{pid}" data-crossboard="{board}" data-pid="{pid}" title="hover to preview">/{board}/ No.{pid}</a>"#,
            board = escape_html(board_short),
            tid = post.thread_id,
            pid = post.id,
        );
        let op_badge = if post.is_op {
            r#" <span style="color:var(--green-bright);font-size:0.75rem">OP</span>"#
        } else {
            ""
        };
        let body_preview: String = post.body.chars().take(120).collect();
        // test character count, not byte count.  post.body.len()
        // measures UTF-8 bytes so multi-byte characters (emoji, CJK, …) can
        // cause the ellipsis to be added even when nothing was truncated, or
        // omitted even when content was.
        let body_preview = if post.body.chars().count() > 120 {
            format!("{}…", escape_html(&body_preview))
        } else {
            escape_html(&body_preview)
        };
        let name_html = {
            let name = post.name.trim();
            if name.is_empty() || name == "Anonymous" {
                String::from("&mdash;")
            } else {
                escape_html(name)
            }
        };
        let tripcode_html = post
            .tripcode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| String::from("&mdash;"), escape_html);

        let del_form = format!(
            r#"<form method="POST" action="/admin/post/delete" style="display:inline">
<input type="hidden" name="_csrf"   value="{csrf}">
<input type="hidden" name="post_id" value="{pid}">
<input type="hidden" name="board"   value="{board}">
<button type="submit" class="admin-del-btn"
        data-confirm="Admin delete post No.{pid}?">&#x2715;</button>
</form>"#,
            csrf = escape_html(csrf_token),
            pid = post.id,
            board = escape_html(board_short),
        );
        let report_form = post
            .ip_hash
            .as_deref()
            .map_or_else(String::new, |post_ip_hash| {
                format!(
                    r#"<button type="button" class="admin-toolbar-btn" data-action="open-report"
        data-pid="{pid}" data-tid="{tid}" data-board="{board}" data-csrf="{csrf}"
        data-report-action="/admin/ip/report" data-report-ip-hash="{ip_hash}"
        data-report-title="Report Hashed IP Post"
        data-report-submit-label="Submit Admin Report"
        data-report-reason-required="1"
        data-report-label="Report post No.{pid} for hashed IP {ip_hash}">report</button>
<noscript><form method="POST" action="/admin/ip/report" class="admin-ip-report-fallback">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="post_id" value="{pid}">
  <input type="hidden" name="thread_id" value="{tid}">
  <input type="hidden" name="board" value="{board}">
  <input type="hidden" name="ip_hash" value="{ip_hash}">
  <label>Report reason<input type="text" name="reason" required maxlength="512"></label>
  <button type="submit" class="admin-toolbar-btn">report</button>
</form></noscript>"#,
                    csrf = escape_html(csrf_token),
                    pid = post.id,
                    tid = post.thread_id,
                    board = escape_html(board_short),
                    ip_hash = escape_html(post_ip_hash),
                )
            });

        crate::templates::append_html(
            &mut rows,
            format_args!(
                r#"<tr>
<td style="white-space:nowrap;font-size:0.8rem">{time}</td>
<td>{link}{op}</td>
<td style="font-size:0.8rem">{name}</td>
<td style="font-size:0.8rem">{tripcode}</td>
<td style="font-size:0.8rem">{media}</td>
<td style="max-width:480px;word-break:break-word;font-size:0.85rem">{body}</td>
<td>{report}</td>
<td>{del}</td>
</tr>"#,
                time = fmt_ts_short(post.created_at),
                link = thread_link,
                op = op_badge,
                name = name_html,
                tripcode = tripcode_html,
                media = media_badge,
                body = body_preview,
                report = report_form,
                del = del_form
            ),
        );
    }

    let identity_summary = {
        let mut parts = Vec::new();
        if !seen_names.is_empty() {
            parts.push(format!(
                "names: {}",
                seen_names
                    .iter()
                    .map(|name| format!(r"<code>{}</code>", escape_html(name)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !seen_tripcodes.is_empty() {
            parts.push(format!(
                "tripcodes: {}",
                seen_tripcodes
                    .iter()
                    .map(|tripcode| format!(r"<code>{}</code>", escape_html(tripcode)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if parts.is_empty() {
            String::from(
                r#"<span style="color:var(--text-dim)">no alternate names or tripcodes found on these posts.</span>"#,
            )
        } else {
            format!(
                r#"<span style="color:var(--text-dim)">{}</span>"#,
                parts.join(" | ")
            )
        }
    };

    let mut pag_base = format!("/admin/ip/{}", escape_html(ip_hash));
    if let Some(return_to) = return_to.filter(|value| !value.is_empty()) {
        let sep = if pag_base.contains('?') { "&" } else { "?" };
        crate::templates::append_html(
            &mut pag_base,
            format_args!("{sep}return_to={}", urlencoding_simple(return_to)),
        );
    }
    let pag_html = render_pagination(pagination, &pag_base);

    let return_buttons = return_to.filter(|value| !value.is_empty()).map_or_else(
        || String::from(r#"<a class="admin-toolbar-btn" href="/admin/panel">Go to admin pannel</a>"#),
        |return_to| {
            format!(
                r#"<a class="admin-toolbar-btn" href="{thread}">Back to thread</a> <a class="admin-toolbar-btn" href="/admin/panel">Go to admin pannel</a>"#,
                thread = escape_html(return_to)
            )
        },
    );

    let body = format!(
        r#"<div class="admin-panel">
<h1>[ IP history ]</h1>
<section class="admin-section">
<h2>// posts by Hashed IP <code style="font-size:0.9rem;overflow-wrap:anywhere">{hash_display}</code></h2>
<p style="color:var(--text-dim);font-size:0.85rem">
  {total} post{plural} found across all boards.
</p>
<p style="color:var(--text-dim);font-size:0.82rem">{identity_summary}</p>
<p style="margin:0.35rem 0 1rem 0">{return_buttons}</p>
<div class="admin-table-wrap" tabindex="0" role="region" aria-label="IP history">
<table class="admin-table" style="width:100%">
<thead><tr>
  <th style="text-align:left">time</th>
  <th style="text-align:left">post</th>
  <th style="text-align:left">name</th>
  <th style="text-align:left">tripcode</th>
  <th>media</th>
  <th style="text-align:left">body</th>
  <th>report</th>
  <th>del</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>
</div>
{pagination}
</section>
</div>
{report_modal}"#,
        hash_display = escape_html(ip_hash),
        total = pagination.total,
        plural = if pagination.total == 1 { "" } else { "s" },
        rows = rows,
        pagination = pag_html,
        identity_summary = identity_summary,
        return_buttons = return_buttons,
        report_modal = super::report_modal_script(),
    );

    base_layout(
        &format!(
            "Hashed IP — {}",
            ip_hash.get(..ip_hash.len().min(12)).unwrap_or(ip_hash)
        ),
        None,
        &body,
        csrf_token,
        all_boards,
        current_theme,
        None,
        false,
        &pag_base,
    )
}

/// Render administrator credential workflows without existing secret values.
#[must_use]
pub fn render_accounts(users: &[(i64, String, i64)], csrf: &str) -> String {
    accounts::render(users, csrf)
}

/// Render dedicated path/secret management without current secret material.
#[must_use]
pub fn render_management(
    state: &Result<crate::config::admin::management::ManagementState, String>,
    csrf: &str,
) -> String {
    management::render(state, csrf)
}

/// Render active/saved/source information for the retained live controls.
#[must_use]
pub fn render_application_state(
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
) -> String {
    application::render(fields)
}

/// Render administrator-only software status inside the existing admin workspace.
#[must_use]
pub fn render_software_updates(
    status: &crate::updates::Status,
    csrf: &str,
    managed: bool,
    container: bool,
) -> String {
    updates::render(status, csrf, managed, container)
}

/// Render pre-upgrade snapshot history inside the existing Backups workspace.
#[must_use]
pub fn render_update_backups(status: &crate::updates::Status) -> String {
    updates::backups(status)
}

/// Render durable configuration restart state and its POST-only action.
#[must_use]
pub fn render_restart(view: &crate::restart::View, csrf: &str) -> String {
    restart::render(view, csrf)
}
