use super::{escape_html, AdminDetectionStatus, AdminPanelViewModel};
use std::fmt::Write;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let open_attr = if view.open_section == Some("site-health") {
        " open"
    } else {
        ""
    };
    let health = &view.site_health;
    let rows = render_health_rows(view);
    let dependency_rows = render_dependency_summary(view);
    let diagnostics = escape_html(health.diagnostics_text);
    format!(
        r##"<!-- ═══════════════════════════════════════════════════════════════════════════
     // site health
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-section-collapsible" id="site-health">
<details class="admin-dropdown" data-admin-dropdown-key="site-health"{open_attr}>
<summary><span>// site health</span></summary>
<div class="admin-dropdown-content admin-site-health" data-admin-health-jobs-url="/admin/site-health/jobs">
  <div class="admin-health-grid">{rows}</div>
  <div class="admin-subsection admin-subsection-tight admin-health-dependencies">
    <div class="admin-card-header">
      <h3>// optional dependency summary</h3>
      <p>Concise startup detection results. The full media tooling panel has details.</p>
    </div>
    <div class="admin-health-grid">{dependency_rows}</div>
    <p class="admin-copy admin-copy-spaced">
      <a class="admin-button-link" href="#media-settings" data-open-admin-section="media-settings">open media panel</a>
    </p>
  </div>
  <details class="admin-diagnostics-details" data-admin-diagnostics>
    <summary>copy diagnostics</summary>
    <div class="admin-diagnostics-panel" role="dialog" aria-modal="false" aria-labelledby="admin-diagnostics-title">
      <div class="admin-diagnostics-header">
        <h3 id="admin-diagnostics-title">// diagnostics</h3>
        <div class="admin-diagnostics-actions">
          <button type="button" data-admin-diagnostics-copy>copy</button>
          <button type="button" data-admin-diagnostics-close>close</button>
        </div>
      </div>
      <pre class="admin-diagnostics-text" data-admin-diagnostics-text>{diagnostics}</pre>
    </div>
  </details>
</div>
</details>
</section>"##,
    )
}

fn render_health_rows(view: &AdminPanelViewModel<'_>) -> String {
    let health = &view.site_health;
    let mut rows = String::new();
    for (label, value) in [
        ("Server status", health.server_status),
        ("RustChan version", health.rustchan_version),
        (
            "Database integrity status",
            health.database_integrity_status,
        ),
        ("Last successful backup", health.last_successful_backup),
        ("Next scheduled backup", health.next_scheduled_backup),
        ("Disk usage for rustchan-data/", health.data_dir_usage),
        ("Upload directory size", health.upload_dir_size),
        ("Tor status", health.tor_status),
        (
            "Tor onion address",
            health.tor_onion_address.unwrap_or("not available"),
        ),
        ("Tor bootstrap state", health.tor_bootstrap_state),
    ] {
        append_health_row(&mut rows, label, value);
    }
    append_job_rows(&mut rows, view);
    rows
}

fn append_job_rows(rows: &mut String, view: &AdminPanelViewModel<'_>) {
    let health = &view.site_health;
    append_health_job_row(
        rows,
        "Running jobs",
        &health.running_jobs.to_string(),
        "running_jobs",
    );
    append_health_job_row(
        rows,
        "Queued jobs",
        &health.queued_jobs.to_string(),
        "queued_jobs",
    );
    append_health_job_row(
        rows,
        "Recent completed jobs",
        &health.recent_completed_jobs.to_string(),
        "recent_completed_jobs",
    );
    append_health_job_row(
        rows,
        "Failed jobs",
        &health.failed_jobs.to_string(),
        "failed_jobs",
    );
    append_health_job_row(rows, "Backup jobs", health.backup_jobs, "backup_jobs");
    append_health_job_row(rows, "Restore jobs", health.restore_jobs, "restore_jobs");
    append_health_job_row(
        rows,
        "Thumbnail/transcode jobs",
        &health.thumbnail_transcode_jobs.to_string(),
        "thumbnail_transcode_jobs",
    );
}

fn append_health_row(out: &mut String, label: &str, value: &str) {
    let _ = write!(
        out,
        r#"<div class="admin-health-row"><span>{label}</span><strong>{value}</strong></div>"#,
        label = escape_html(label),
        value = escape_html(value),
    );
}

fn append_health_job_row(out: &mut String, label: &str, value: &str, key: &str) {
    let _ = write!(
        out,
        r#"<div class="admin-health-row"><span>{label}</span><strong data-admin-health-job="{key}">{value}</strong></div>"#,
        label = escape_html(label),
        key = escape_html(key),
        value = escape_html(value),
    );
}

fn render_dependency_summary(view: &AdminPanelViewModel<'_>) -> String {
    let dependencies = view.site_health.dependency_summary;
    let mut rows = String::new();
    for (label, status) in [
        ("ffmpeg", dependencies.ffmpeg),
        ("ffprobe", dependencies.ffprobe),
        ("WebP support", dependencies.webp),
        ("VP9 support", dependencies.vp9),
        ("Opus support", dependencies.opus),
    ] {
        append_health_row(&mut rows, label, detection_label(status));
    }
    rows
}

const fn detection_label(status: AdminDetectionStatus) -> &'static str {
    match status {
        AdminDetectionStatus::Detected => "found",
        AdminDetectionStatus::Missing => "missing",
    }
}
