//! Media-pipeline and database-maintenance sections of the admin panel.

use super::{base_layout, escape_html, format_file_size, AdminPanelViewModel};
use crate::db::DbHealthReport;

/// Pre-rendered values interpolated into the maintenance section.
struct MaintenanceSectionView<'a> {
    /// CSRF token for state-changing forms.
    csrf_token: &'a str,
    /// Optional database-size warning banner.
    db_warn_banner: &'a str,
    /// Human-readable database size.
    db_size_str: &'a str,
    /// Optional Tor address details.
    tor_section: &'a str,
    /// Configured external-media process timeout.
    ffmpeg_timeout_secs: u64,
    /// Human-readable timeout help text.
    ffmpeg_timeout_help: &'a str,
    /// Whether automatic media pruning is enabled.
    media_auto_prune_enabled: bool,
    /// Numeric component of the active-media size threshold.
    media_max_active_content_size_value: u64,
    /// Unit component of the active-media size threshold.
    media_max_active_content_size_unit: &'a str,
    /// Pre-rendered media dependency cards.
    media_detection_cards: &'a str,
    /// `open` attribute for the media settings disclosure.
    media_settings_open_attr: &'a str,
    /// `open` attribute for the database maintenance disclosure.
    database_maintenance_open_attr: &'a str,
    /// Short setup-state label.
    setup_status: &'a str,
    /// Detailed setup-state explanation.
    setup_status_detail: &'a str,
    /// Warning displayed before reopening setup.
    setup_reopen_warning: &'a str,
    /// Optional form for closing a reopened setup wizard.
    setup_close_control: &'a str,
}

/// Renders the complete maintenance tab of the admin panel.
pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let media_settings_open_attr = if view.open_section == Some("media-settings") {
        " open"
    } else {
        ""
    };
    let database_maintenance_open_attr = if view.open_section == Some("database-maintenance") {
        " open"
    } else {
        ""
    };
    let db_warn_banner = if view.maintenance.db_size_warning {
        format!(
            r#"<div class="admin-flash flash-error admin-flash-spaced">
&#9888; <strong>Database size warning:</strong> The database file has exceeded the configured
warning threshold ({size}). Consider running <strong>VACUUM</strong> below or archiving
old boards to prevent query performance degradation.
</div>"#,
            size = format_file_size(view.maintenance.db_size_bytes),
        )
    } else {
        String::new()
    };

    let tor_section = if view.tor_address.is_none() {
        String::new()
    } else {
        let mut addresses = String::new();
        if let Some(addr) = view.tor_address {
            crate::templates::append_html(
                &mut addresses,
                format_args!(
                    r#"<p class="admin-copy">Onion address: <a href="http://{addr}" target="_blank" rel="noreferrer">{addr}</a></p>"#,
                    addr = escape_html(addr)
                ),
            );
        }
        addresses
    };
    let db_size_str = format_file_size(view.maintenance.db_size_bytes);
    let ffmpeg_timeout_help =
        crate::config::describe_timeout_secs(view.maintenance.ffmpeg_timeout_secs);
    let (media_max_value, media_max_unit) =
        media_size_input_parts(view.maintenance.media_max_active_content_size_bytes);
    let media_detection_cards = render_media_detection_cards(view);
    let (setup_status, setup_status_detail) = match view.maintenance.setup_status {
        super::AdminPanelSetupStatus::Reopened => (
            "reopened",
            "The setup wizard has been reopened by an admin and is available only to authenticated admins.",
        ),
        super::AdminPanelSetupStatus::Complete => (
            "complete",
            "The setup wizard has completed and public setup routes are blocked.",
        ),
        super::AdminPanelSetupStatus::Available => (
            "available",
            "This instance still appears to be in first-run setup.",
        ),
        super::AdminPanelSetupStatus::Initialized => (
            "initialized",
            "This instance has existing durable runtime state, so first-run setup routes are blocked.",
        ),
    };
    let setup_reopen_warning = "Reopening setup exposes live settings for editing. It does not replace existing admin credentials and still requires an authenticated admin session.";
    let setup_close_control = if matches!(
        view.maintenance.setup_status,
        super::AdminPanelSetupStatus::Reopened
    ) {
        r#"<form method="POST" action="/admin/setup/close" class="admin-inline-actions">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit"
            data-confirm="Close the setup wizard without changing live settings?">close setup wizard</button>
  </form>"#
    } else {
        ""
    };
    let section_view = MaintenanceSectionView {
        csrf_token: view.csrf_token,
        db_warn_banner: &db_warn_banner,
        db_size_str: &db_size_str,
        tor_section: &tor_section,
        ffmpeg_timeout_secs: view.maintenance.ffmpeg_timeout_secs,
        ffmpeg_timeout_help: &ffmpeg_timeout_help,
        media_auto_prune_enabled: view.maintenance.media_auto_prune_enabled,
        media_max_active_content_size_value: media_max_value,
        media_max_active_content_size_unit: media_max_unit,
        media_detection_cards: &media_detection_cards,
        media_settings_open_attr,
        database_maintenance_open_attr,
        setup_status,
        setup_status_detail,
        setup_reopen_warning,
        setup_close_control,
    };
    render_admin_maintenance_section(&section_view)
}

/// Splits a byte threshold into an exact mebibyte or gibibyte input value.
const fn media_size_input_parts(bytes: u64) -> (u64, &'static str) {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes > 0 && bytes.is_multiple_of(GIB) {
        (bytes / GIB, "gib")
    } else {
        (bytes / MIB, "mib")
    }
}

/// Renders startup detection cards for media-processing capabilities.
fn render_media_detection_cards(view: &AdminPanelViewModel<'_>) -> String {
    let pdf_renderer = view
        .maintenance
        .media_detection
        .pdf_thumbnail_renderer
        .as_deref()
        .unwrap_or("none");
    let pdf_detail = if view
        .maintenance
        .media_detection
        .pdf_thumbnail_renderer
        .is_some()
    {
        format!("selected renderer: {pdf_renderer}")
    } else {
        "using built-in generic PDF placeholder thumbnail".to_owned()
    };

    let mut cards = String::new();
    for (label, ok, detail) in [
        (
            "ffmpeg",
            view.maintenance.media_detection.ffmpeg.is_detected(),
            "video thumbnails and transcoding entrypoint",
        ),
        (
            "WebP/images",
            true,
            "built in (Rust), including animated WebP",
        ),
        (
            "VP9/WebM pipeline",
            view.maintenance.media_detection.vp9_pipeline.is_detected(),
            "video conversion to WebM with VP9 + Opus",
        ),
        (
            "AV1 decoding",
            view.maintenance.media_detection.av1_decoder.is_detected(),
            "AV1 video inputs; independent of AV1 encoding",
        ),
        (
            "AV1 encoding",
            view.maintenance.media_detection.av1_encoder.is_detected(),
            "installed build capability; RustChan output uses VP9 + Opus",
        ),
        (
            "PDF thumbnails",
            view.maintenance
                .media_detection
                .pdf_thumbnail_renderer
                .is_some(),
            &pdf_detail,
        ),
    ] {
        crate::templates::append_html(
            &mut cards,
            format_args!(
                r#"<article class="admin-detection-card">
  <div class="admin-detection-card-header">
    <h3>{label}</h3>
    <span class="admin-detection-pill {pill_class}">{status}</span>
  </div>
  <p>{detail}</p>
</article>"#,
                label = escape_html(label),
                pill_class = if ok {
                    "admin-detection-pill-ok"
                } else {
                    "admin-detection-pill-missing"
                },
                status = if ok { "detected" } else { "missing" },
                detail = escape_html(detail),
            ),
        );
    }
    cards
}

#[expect(
    clippy::too_many_lines,
    reason = "the maintenance controls form one stable server-rendered HTML fragment"
)]
/// Renders media settings, database tools, and setup controls.
fn render_admin_maintenance_section(view: &MaintenanceSectionView<'_>) -> String {
    format!(
        r#"<div class="admin-panel-maintenance" id="maintenance">
<section class="admin-section admin-section-collapsible" id="media-settings">
<details class="admin-dropdown" data-admin-dropdown-key="media-settings"{media_settings_open_attr}>
<summary><span>// media settings</span></summary>
<div class="admin-dropdown-content">
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// media pipeline detection</h3>
    <p>Boot-time checks for the main external media tooling.</p>
  </div>
  <div class="admin-detection-grid">{media_detection_cards}</div>
</div>
<form method="POST" action="/admin/media/settings" class="admin-site-settings-form">
  <input type="hidden" name="_csrf" value="{csrf}">
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// ffmpeg timeout</h3>
    <p>Adjust how long RustChan waits before killing a slow video conversion job.</p>
  </div>
<p class="admin-copy">
  RustChan currently allows ffmpeg to run for <strong>{ffmpeg_timeout_help}</strong> before a long-running media job is killed.
  This primarily affects uploaded video re-encoding, especially slow MP4 to WebM/VP9 conversion.
</p>
  <div class="board-settings-grid admin-settings-grid">
    <label title="Slow systems may need a higher value for ffmpeg video conversion jobs.">
      Video re-encoding timeout (seconds)
      <input type="number" name="ffmpeg_timeout_secs" value="{ffmpeg_timeout_secs}" min="{ffmpeg_timeout_min}" max="{ffmpeg_timeout_max}" step="1" inputmode="numeric" class="admin-input-compact" required>
    </label>
  </div>
  <p class="admin-meta-note admin-meta-note-spaced">
    This controls how long RustChan lets ffmpeg run while converting uploaded videos.
    Slow systems such as Raspberry Pi devices may need a higher value.
    MP4 to WebM/VP9 encoding can be especially slow without hardware acceleration.
    If videos fail to convert because of timeouts, increase this value.
  </p>
</div>
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// media pruning</h3>
    <p>Delete oldest full-size post media when active stored media exceeds the configured cap. Thumbnails are kept where practical.</p>
  </div>
  <div class="board-settings-grid admin-settings-grid">
    <label title="Set to 0 to leave the active media cap unset. When pruning is enabled, use at least 1 MiB.">
      Maximum active content database/media size
      <span class="admin-inline-control">
        <input type="number" name="media_max_active_content_size" value="{media_max_active_content_size}" min="0" step="1" inputmode="numeric" class="admin-input-compact">
        <select name="media_max_active_content_size_unit">
          <option value="mib"{media_max_unit_mib_selected}>MiB</option>
          <option value="gib"{media_max_unit_gib_selected}>GiB</option>
          <option value="bytes"{media_max_unit_bytes_selected}>bytes</option>
        </select>
      </span>
    </label>
  </div>
  <div class="board-settings-checks">
    <label class="admin-inline-checkbox" title="Delete oldest full-size post media when active stored media exceeds the configured cap. Thumbnails are kept where practical.">
      <input type="checkbox" name="media_auto_prune_enabled" value="1"{media_auto_prune_checked}>
      Enable automatic active content pruning
    </label>
  </div>
</div>
<div class="board-settings-actions">
  <button type="submit">save media settings</button>
</div>
</form>
</div>
</details>
</section>

<section class="admin-section admin-section-collapsible" id="database-maintenance">
<details class="admin-dropdown" data-admin-dropdown-key="database-maintenance"{database_maintenance_open_attr}>
<summary><span>// database maintenance</span></summary>
<div class="admin-dropdown-content">
{db_warn_banner}<p class="admin-copy">
  Current database size: <strong>{db_size_str}</strong>.
  Running <strong>VACUUM</strong> rewrites the database file compactly, reclaiming space left after
  bulk deletions (deleted threads, pruned posts, etc.).  This may take a few seconds on large
  databases and briefly blocks writes.
</p>
<p class="admin-copy admin-copy-spaced">
  Run database checks after restores or large deletes. Before repair, take a backup; repair can
  rebuild indexes and search data, but may not fix true file corruption.
</p>
<div class="admin-inline-actions">
<form method="POST" action="/admin/db/check">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit">&#x1F50E; check database health</button>
</form>
<form method="POST" action="/admin/vacuum">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="Run VACUUM? This will briefly block the database while it rebuilds. Continue?">&#x1F9F9; run VACUUM</button>
</form>
</div>
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// setup status</h3>
    <p>First-run setup route state and controlled maintenance reopen.</p>
  </div>
  <p class="admin-copy">
    Setup status: <strong>{setup_status}</strong>. {setup_status_detail}
  </p>
  <p class="admin-meta-note admin-meta-note-spaced">{setup_reopen_warning}</p>
  <form method="POST" action="/admin/setup/reopen" class="admin-inline-actions">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit"
            data-confirm="Reopen the setup wizard? This edits live settings and remains admin-only. Continue?">reopen setup wizard</button>
  </form>
  {setup_close_control}
</div>
</div>
</details>
</section>

{tor_section}
</div>"#,
        csrf = escape_html(view.csrf_token),
        db_warn_banner = view.db_warn_banner,
        db_size_str = view.db_size_str,
        ffmpeg_timeout_secs = view.ffmpeg_timeout_secs,
        media_auto_prune_checked = if view.media_auto_prune_enabled {
            " checked"
        } else {
            ""
        },
        media_max_active_content_size = view.media_max_active_content_size_value,
        media_max_unit_mib_selected = if view.media_max_active_content_size_unit == "mib" {
            " selected"
        } else {
            ""
        },
        media_max_unit_gib_selected = if view.media_max_active_content_size_unit == "gib" {
            " selected"
        } else {
            ""
        },
        media_max_unit_bytes_selected = if view.media_max_active_content_size_unit == "bytes" {
            " selected"
        } else {
            ""
        },
        ffmpeg_timeout_help = escape_html(view.ffmpeg_timeout_help),
        media_detection_cards = view.media_detection_cards,
        ffmpeg_timeout_min = crate::config::MIN_FFMPEG_TIMEOUT_SECS,
        ffmpeg_timeout_max = crate::config::MAX_FFMPEG_TIMEOUT_SECS,
        media_settings_open_attr = view.media_settings_open_attr,
        database_maintenance_open_attr = view.database_maintenance_open_attr,
        setup_status = escape_html(view.setup_status),
        setup_status_detail = escape_html(view.setup_status_detail),
        setup_reopen_warning = escape_html(view.setup_reopen_warning),
        setup_close_control = view
            .setup_close_control
            .replace("{csrf}", &escape_html(view.csrf_token)),
        tor_section = view.tor_section,
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
