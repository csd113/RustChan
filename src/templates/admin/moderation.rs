use super::{escape_html, fmt_ts, AdminPanelViewModel};
use std::fmt::Write;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let report_count = view.moderation.reports.len();
    let appeal_count = view.moderation.appeals.len();
    let ban_count = view.moderation.bans.len();
    let filter_count = view.moderation.filters.len();
    let report_badge = if report_count > 0 {
        format!(r#" <span class="report-badge">{report_count}</span>"#)
    } else {
        String::new()
    };
    let appeal_badge = if appeal_count > 0 {
        format!(r#" <span class="report-badge">{appeal_count}</span>"#)
    } else {
        String::new()
    };
    let ban_badge = format!(r#" <span class="admin-count-badge">{ban_count}</span>"#);
    let filter_badge = format!(r#" <span class="admin-count-badge">{filter_count}</span>"#);
    let moderation_summary_counter = format!("Report inbox: [{report_count}]");

    render_admin_moderation_section(
        view.csrf_token,
        &render_report_rows(view),
        &render_appeal_rows(view),
        &render_ban_rows(view),
        &render_filter_rows(view),
        &report_badge,
        &appeal_badge,
        &ban_badge,
        &filter_badge,
        &moderation_summary_counter,
        view.open_section,
    )
}

fn render_ban_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut ban_rows = String::new();
    for ban in view.moderation.bans {
        let expires = ban
            .expires_at
            .map_or_else(|| "permanent".to_string(), fmt_ts);
        let _ = write!(
            ban_rows,
            r#"<tr>
<td class="ip-hash">{}</td><td>{}</td><td>{}</td>
<td>
<form method="POST" action="/admin/ban/remove" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="ban_id" value="{id}">
<button type="submit">lift</button>
</form>
</td>
</tr>"#,
            escape_html(ban.ip_hash.get(..16).unwrap_or(&ban.ip_hash)),
            escape_html(ban.reason.as_deref().unwrap_or("")),
            escape_html(&expires),
            csrf = escape_html(view.csrf_token),
            id = ban.id
        );
    }
    ban_rows
}

fn render_filter_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut filter_rows = String::new();
    for f in view.moderation.filters {
        let _ = write!(
            filter_rows,
            r#"<tr>
<td>{}</td><td>{}</td>
<td>
<form method="POST" action="/admin/filter/remove" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="filter_id" value="{id}">
<button type="submit">remove</button>
</form>
</td>
</tr>"#,
            escape_html(&f.pattern),
            escape_html(&f.replacement),
            csrf = escape_html(view.csrf_token),
            id = f.id
        );
    }
    filter_rows
}

fn render_report_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut report_rows = String::new();
    if view.moderation.reports.is_empty() {
        report_rows.push_str(
            r#"<tr><td colspan="6" style="color:var(--text-dim);text-align:center">no open reports</td></tr>"#,
        );
    }
    for rc in view.moderation.reports {
        let preview = escape_html(rc.post_preview.trim());
        let reason = escape_html(&rc.report.reason);
        let age = fmt_ts(rc.report.created_at);
        let user_info = rc.post_ip_hash.as_deref().map_or_else(
            || String::from(r#"<span style="color:var(--text-dim)">n/a</span>"#),
            |ip_hash| {
                let short = ip_hash.get(..16).unwrap_or(ip_hash);
                format!(
                    r#"<a href="/admin/ip/{ip_hash}" title="View hashed IP history">{short}…</a>"#,
                    ip_hash = escape_html(ip_hash),
                    short = escape_html(short),
                )
            },
        );
        let _ = write!(
            report_rows,
            r#"<tr>
<td><a href="/{board}/thread/{tid}#p{pid}" title="view post">/{board}/ No.{pid}</a></td>
<td>{user_info}</td>
<td style="max-width:240px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="{preview}">{preview}</td>
<td>{reason}</td>
<td style="white-space:nowrap;font-size:0.78rem">{age}</td>
<td style="white-space:nowrap">
  <form method="POST" action="/admin/report/resolve" style="display:inline">
    <input type="hidden" name="_csrf"      value="{csrf}">
    <input type="hidden" name="report_id"  value="{rid}">
    <button type="submit">&#10003; resolve</button>
  </form>
</td>
</tr>"#,
            board = escape_html(&rc.board_short),
            tid = rc.report.thread_id,
            pid = rc.report.post_id,
            user_info = user_info,
            preview = preview,
            reason = reason,
            age = escape_html(&age),
            csrf = escape_html(view.csrf_token),
            rid = rc.report.id
        );
    }
    report_rows
}

fn render_appeal_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut appeal_rows = String::new();
    if view.moderation.appeals.is_empty() {
        appeal_rows.push_str(
            r#"<tr><td colspan="4" style="color:var(--text-dim);text-align:center">no open appeals</td></tr>"#,
        );
    }
    for a in view.moderation.appeals {
        let reason = escape_html(a.reason.trim());
        let age = fmt_ts(a.created_at);
        let ip_short = a.ip_hash.get(..16).unwrap_or(&a.ip_hash);
        let _ = write!(
            appeal_rows,
            r#"<tr>
<td style="font-size:0.78rem;font-family:monospace">{ip_short}…</td>
<td style="max-width:300px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="{reason}">{reason}</td>
<td style="white-space:nowrap;font-size:0.78rem">{age}</td>
<td style="white-space:nowrap">
  <form method="POST" action="/admin/appeal/dismiss" style="display:inline">
    <input type="hidden" name="_csrf"      value="{csrf}">
    <input type="hidden" name="appeal_id"  value="{aid}">
    <button type="submit">✕ dismiss</button>
  </form>
  <form method="POST" action="/admin/appeal/accept" style="display:inline;margin-left:0.35rem"
        data-confirm-submit="Accept appeal and lift ban for this IP?">
    <input type="hidden" name="_csrf"      value="{csrf}">
    <input type="hidden" name="appeal_id"  value="{aid}">
    <input type="hidden" name="ip_hash"    value="{ip_hash}">
    <button type="submit" class="btn-success">✓ accept + unban</button>
  </form>
</td>
</tr>"#,
            ip_short = ip_short,
            reason = reason,
            age = escape_html(&age),
            csrf = escape_html(view.csrf_token),
            aid = a.id,
            ip_hash = escape_html(&a.ip_hash)
        );
    }
    appeal_rows
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
fn render_admin_moderation_section(
    csrf_token: &str,
    report_rows: &str,
    appeal_rows: &str,
    ban_rows: &str,
    filter_rows: &str,
    report_badge: &str,
    appeal_badge: &str,
    ban_badge: &str,
    filter_badge: &str,
    moderation_summary_counter: &str,
    open_section: Option<&str>,
) -> String {
    let reports_open_attr = if open_section == Some("reports") {
        " open"
    } else {
        ""
    };
    format!(
        r#"<div class="admin-panel-moderation" id="moderation">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // moderation dropdown (log + reports + moderation tools)
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-section-collapsible" id="reports">
<details class="admin-dropdown" data-admin-dropdown-key="reports"{reports_open_attr}>
<summary><span>// moderation</span><span class="admin-dropdown-badges admin-dropdown-counter-label">{moderation_summary_counter}</span></summary>
<div class="admin-dropdown-content">
<p class="admin-moderation-intro">
  Review queues first. Policy tools and the log are below.
</p>
<div class="admin-moderation-grid">
  <section class="admin-moderation-card admin-moderation-card-review">
    <div class="admin-card-header">
      <h3>// review queue</h3>
      <p>Handle open reports and ban appeals first.</p>
    </div>
    <div class="admin-subsection admin-subsection-tight">
      <h4>// report inbox{report_badge}</h4>
      <div class="admin-table-wrap">
      <table class="admin-table">
        <thead><tr><th>post</th><th>user</th><th>content preview</th><th>reason</th><th>filed</th><th>action</th></tr></thead>
        <tbody>{report_rows}</tbody>
      </table>
      </div>
    </div>

    <div class="admin-subsection admin-subsection-tight">
      <h4 id="appeals">// ban appeals{appeal_badge}</h4>
      <div class="admin-table-wrap">
      <table class="admin-table">
        <thead><tr><th>ip (partial)</th><th>appeal message</th><th>filed</th><th>action</th></tr></thead>
        <tbody>{appeal_rows}</tbody>
      </table>
      </div>
    </div>
  </section>

  <section class="admin-moderation-card admin-moderation-card-controls">
    <div class="admin-card-header">
      <h3>// policy controls</h3>
      <p>Manage bans and automated word replacements.</p>
    </div>

    <div class="admin-subsection admin-subsection-tight" id="active-bans">
      <h4>// active bans{ban_badge}</h4>
      <div class="admin-table-wrap">
      <table class="admin-table">
        <thead><tr><th>ip hash (partial)</th><th>reason</th><th>expires</th><th>action</th></tr></thead>
        <tbody>{ban_rows}</tbody>
      </table>
      </div>
      <h4>add ban</h4>
      <form method="POST" action="/admin/ban/add" class="admin-moderation-form admin-quick-form admin-moderation-compact-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <label class="admin-quick-field admin-moderation-field">IP hash
          <input class="admin-moderation-input" type="text" name="ip_hash" required placeholder="ab12cd34ef56...">
        </label>
        <label class="admin-quick-field admin-moderation-field">Reason
          <input class="admin-moderation-input" type="text" name="reason" placeholder="Rule violation">
        </label>
        <label class="admin-quick-field admin-quick-field-compact admin-moderation-field">Duration (hours)
          <input class="admin-moderation-input" type="text" name="duration_hours" placeholder="blank = permanent" inputmode="numeric">
        </label>
        <button type="submit">ban</button>
      </form>
    </div>

    <div class="admin-subsection admin-subsection-tight" id="word-filters">
      <h4>// word filters{filter_badge}</h4>
      <div class="admin-table-wrap">
      <table class="admin-table">
        <thead><tr><th>pattern</th><th>replacement</th><th>action</th></tr></thead>
        <tbody>{filter_rows}</tbody>
      </table>
      </div>
      <h4>add filter</h4>
      <form method="POST" action="/admin/filter/add" class="admin-moderation-form admin-quick-form admin-moderation-compact-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <label class="admin-quick-field admin-moderation-field">Pattern
          <input class="admin-moderation-input" type="text" name="pattern" required placeholder="old phrase">
        </label>
        <label class="admin-quick-field admin-moderation-field">Replacement
          <input class="admin-moderation-input" type="text" name="replacement" placeholder="new phrase">
        </label>
        <button type="submit">add</button>
      </form>
    </div>
  </section>

  <section class="admin-moderation-card admin-moderation-card-log">
    <div class="admin-card-header">
      <h3>// audit trail</h3>
      <p>Every moderation action is recorded here.</p>
    </div>
    <div class="admin-card-actions">
      <a href="/admin/mod-log" class="admin-link-button">view full log</a>
    </div>
    <p class="admin-card-note">Use the full log for history and follow-up. The live queues stay visible in this panel.</p>
  </section>
</div>
</div>
</details>
</section>
</div>"#,
        csrf = escape_html(csrf_token),
        report_rows = report_rows,
        appeal_rows = appeal_rows,
        ban_rows = ban_rows,
        filter_rows = filter_rows,
        report_badge = report_badge,
        appeal_badge = appeal_badge,
        ban_badge = ban_badge,
        filter_badge = filter_badge,
        moderation_summary_counter = escape_html(moderation_summary_counter),
    )
}
