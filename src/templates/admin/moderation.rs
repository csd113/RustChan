//! Report, appeal, ban, and word-filter sections of the admin panel.

use super::{
    base_layout, escape_html, fmt_ts, fmt_ts_short, render_pagination, urlencoding_simple,
    AdminPanelViewModel,
};
use crate::models::Board;
use std::collections::BTreeSet;

/// Renders the complete moderation tab of the admin panel.
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
        &render_appeal_rows(view.csrf_token, view.moderation.appeals),
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

/// Renders active-ban table rows.
fn render_ban_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut ban_rows = String::new();
    for ban in view.moderation.bans {
        let expires = ban
            .expires_at
            .map_or_else(|| "permanent".to_owned(), fmt_ts);
        crate::templates::append_html(
            &mut ban_rows,
            format_args!(
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
            ),
        );
    }
    ban_rows
}

/// Renders word-filter table rows.
fn render_filter_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut filter_rows = String::new();
    for f in view.moderation.filters {
        crate::templates::append_html(
            &mut filter_rows,
            format_args!(
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
            ),
        );
    }
    filter_rows
}

/// Renders open-report table rows.
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
        let reason = if rc.report.reason.chars().nth(160).is_some() {
            let summary: String = rc.report.reason.chars().take(80).collect();
            format!(
                r#"<div class="admin-report-summary">{summary}…</div><details class="admin-report-detail"><summary>Full report</summary><p>{reason}</p></details>"#,
                summary = escape_html(&summary),
            )
        } else {
            reason
        };
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
        crate::templates::append_html(
            &mut report_rows,
            format_args!(
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
            ),
        );
    }
    report_rows
}

/// Renders pending ban-appeal table rows.
fn render_appeal_rows(csrf_token: &str, appeals: &[crate::models::BanAppeal]) -> String {
    let mut appeal_rows = String::new();
    if appeals.is_empty() {
        appeal_rows.push_str(
            r#"<tr><td colspan="4" style="color:var(--text-dim);text-align:center">no open appeals</td></tr>"#,
        );
    }
    for a in appeals {
        let reason = escape_html(a.reason.trim());
        let age = fmt_ts(a.created_at);
        let ip_short = a.ip_hash.get(..16).unwrap_or(&a.ip_hash);
        let ip_short = escape_html(ip_short);
        crate::templates::append_html(
            &mut appeal_rows,
            format_args!(
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
                csrf = escape_html(csrf_token),
                aid = a.id,
                ip_hash = escape_html(&a.ip_hash)
            ),
        );
    }
    appeal_rows
}

#[expect(
    clippy::too_many_lines,
    reason = "the moderation controls form one stable server-rendered HTML fragment"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the section interpolates independent pre-rendered moderation tables"
)]
/// Renders all moderation subsections and their counters.
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
      <div class="admin-table-wrap" tabindex="0" role="region" aria-label="Report inbox">
      <table class="admin-table admin-report-table">
        <thead><tr><th>post</th><th>user</th><th>content preview</th><th>reason</th><th>filed</th><th>action</th></tr></thead>
        <tbody>{report_rows}</tbody>
      </table>
      </div>
    </div>

    <div class="admin-subsection admin-subsection-tight">
      <h4 id="appeals">// ban appeals{appeal_badge}</h4>
      <div class="admin-table-wrap" tabindex="0" role="region" aria-label="Ban appeals">
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
      <div class="admin-table-wrap" tabindex="0" role="region" aria-label="Active bans">
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
      <div class="admin-table-wrap" tabindex="0" role="region" aria-label="Word filters">
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
