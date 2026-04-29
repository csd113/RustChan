use super::{escape_html, format_file_size, AdminPanelViewModel};
use std::fmt::Write;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let db_warn_banner = if view.maintenance.db_size_warning {
        format!(
            r#"<div class="admin-flash flash-error" style="margin-bottom:0.75rem">
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
            let _ = write!(
                addresses,
                r#"<p class="admin-copy">Onion address: <a href="http://{addr}" target="_blank" rel="noreferrer">{addr}</a></p>"#,
                addr = escape_html(addr)
            );
        }
        addresses
    };
    let db_size_str = format_file_size(view.maintenance.db_size_bytes);
    let ffmpeg_timeout_help =
        crate::config::describe_timeout_secs(view.maintenance.ffmpeg_timeout_secs);
    render_admin_maintenance_section(
        view.csrf_token,
        &db_warn_banner,
        &db_size_str,
        &tor_section,
        view.maintenance.ffmpeg_timeout_secs,
        &ffmpeg_timeout_help,
    )
}

fn render_admin_maintenance_section(
    csrf_token: &str,
    db_warn_banner: &str,
    db_size_str: &str,
    tor_section: &str,
    ffmpeg_timeout_secs: u64,
    ffmpeg_timeout_help: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-maintenance" id="maintenance">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // database maintenance
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// database maintenance</h2>
{db_warn_banner}<p style="color:var(--text-dim);font-size:0.85rem">
  Current database size: <strong>{db_size_str}</strong>.
  Running <strong>VACUUM</strong> rewrites the database file compactly, reclaiming space left after
  bulk deletions (deleted threads, pruned posts, etc.).  This may take a few seconds on large
  databases and briefly blocks writes.
</p>
<p style="color:var(--text-dim);font-size:0.85rem">
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
</section>

<section class="admin-section">
<h2>// media processing</h2>
<p style="color:var(--text-dim);font-size:0.85rem">
  RustChan currently allows ffmpeg to run for <strong>{ffmpeg_timeout_help}</strong> before a long-running media job is killed.
  This primarily affects uploaded video re-encoding, especially slow MP4 to WebM/VP9 conversion.
</p>
<form method="POST" action="/admin/media/settings" class="admin-site-settings-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <div class="board-settings-grid admin-settings-grid">
    <label title="Slow systems may need a higher value for ffmpeg video conversion jobs.">
      Video re-encoding timeout (seconds)
      <input type="number" name="ffmpeg_timeout_secs" value="{ffmpeg_timeout_secs}" min="{ffmpeg_timeout_min}" max="{ffmpeg_timeout_max}" step="1" inputmode="numeric" style="font-family:inherit" required>
    </label>
  </div>
  <p class="admin-meta-note admin-meta-note-spaced">
    This controls how long RustChan lets ffmpeg run while converting uploaded videos.
    Slow systems such as Raspberry Pi devices may need a higher value.
    MP4 to WebM/VP9 encoding can be especially slow without hardware acceleration.
    If videos fail to convert because of timeouts, increase this value.
  </p>
  <div class="board-settings-actions">
    <button type="submit">save media processing settings</button>
  </div>
</form>
</section>

<!-- ═══════════════════════════════════════════════════════════════════════════
     // active onion address
     ═══════════════════════════════════════════════════════════════════════════ -->
{tor_section}
</div>"#,
        csrf = escape_html(csrf_token),
        db_warn_banner = db_warn_banner,
        db_size_str = db_size_str,
        ffmpeg_timeout_secs = ffmpeg_timeout_secs,
        ffmpeg_timeout_help = escape_html(ffmpeg_timeout_help),
        ffmpeg_timeout_min = crate::config::MIN_FFMPEG_TIMEOUT_SECS,
        ffmpeg_timeout_max = crate::config::MAX_FFMPEG_TIMEOUT_SECS,
        tor_section = tor_section,
    )
}
