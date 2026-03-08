// templates/admin.rs
//
// Page templates for the admin interface:
//   admin_login_page        — login form
//   admin_panel_page        — main control panel (boards, bans, reports, …)
//   mod_log_page            — moderation history
//   admin_vacuum_result_page — post-VACUUM feedback
//   admin_ip_history_page   — posts by IP hash

use crate::models::*;
use crate::utils::{files::format_file_size, sanitize::escape_html};

use super::{base_layout, fmt_ts, fmt_ts_short, render_pagination};

// ─── Admin login ──────────────────────────────────────────────────────────────

pub fn admin_login_page(error: Option<&str>, csrf_token: &str, boards: &[Board]) -> String {
    let err_html = error
        .map(|e| format!(r#"<div class="error">{}</div>"#, escape_html(e)))
        .unwrap_or_default();

    let body = format!(
        r#"<div class="page-box admin-login">
<h2>[ admin login ]</h2>
{err}
<form method="POST" action="/admin/login">
<input type="hidden" name="_csrf" value="{csrf}">
<table>
<tr><td>username</td><td><input type="text" name="username" autofocus required autocomplete="username"></td></tr>
<tr><td>password</td><td><input type="password" name="password" required autocomplete="current-password"></td></tr>
<tr><td></td><td><button type="submit">authenticate</button></td></tr>
</table>
</form>
</div>"#,
        err = err_html,
        csrf = escape_html(csrf_token),
    );
    base_layout("admin login", None, &body, csrf_token, boards, false, false)
}

// ─── Admin panel ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn admin_panel_page(
    boards: &[Board],
    bans: &[Ban],
    filters: &[WordFilter],
    collapse_greentext: bool,
    csrf_token: &str,
    full_backups: &[BackupInfo],
    board_backups: &[BackupInfo],
    db_size_bytes: i64,
    reports: &[crate::models::ReportWithContext],
    appeals: &[crate::models::BanAppeal],
    site_name: &str,
    site_subtitle: &str,
    tor_address: Option<&str>,
) -> String {
    let mut board_cards = String::new();
    for b in boards {
        let checked = |v: bool| if v { " checked" } else { "" };
        board_cards.push_str(&format!(
            r#"<details class="board-settings-card">
<summary>/{short}/ — {name} {nsfw_tag}</summary>
<form method="POST" action="/admin/board/settings" class="board-settings-form">
<input type="hidden" name="_csrf"     value="{csrf}">
<input type="hidden" name="board_id"  value="{id}">
<div class="board-settings-grid">
  <label>Name<input type="text" name="name" value="{name_raw}" maxlength="64" required></label>
  <label>Description<input type="text" name="description" value="{desc_raw}" maxlength="256"></label>
  <label>Bump limit<input type="number" name="bump_limit" value="{bump}" min="1" max="10000"></label>
  <label>Max threads<input type="number" name="max_threads" value="{maxt}" min="1" max="1000"></label>
</div>
<div class="board-settings-checks">
  <label><input type="checkbox" name="nsfw"            value="1"{nsfw_ck}> NSFW</label>
  <label><input type="checkbox" name="allow_images"    value="1"{img_ck}>  Allow images</label>
  <label><input type="checkbox" name="allow_video"     value="1"{vid_ck}>  Allow video</label>
  <label><input type="checkbox" name="allow_audio"     value="1"{aud_ck}>  Allow audio</label>
  <label><input type="checkbox" name="allow_tripcodes" value="1"{trip_ck}> Allow tripcodes</label>
  <label><input type="checkbox" name="allow_archive"   value="1"{archive_ck}> Enable archive</label>
  <label><input type="checkbox" name="allow_video_embeds" value="1"{embeds_ck}> Embed video links (YouTube / Invidious / Streamable)</label>
  <label><input type="checkbox" name="allow_captcha"      value="1"{captcha_ck}> PoW CAPTCHA on new threads (hashcash, JS-solved)</label>
  <label><input type="checkbox" name="allow_editing"   value="1"{edit_ck}>
    Allow post editing</label>
</div>
<div class="board-settings-grid edit-window-row" style="margin-top:0.4rem;{edit_win_display}">
  <label title="How long (seconds) after posting a user may edit. 0 = use default (300 s).">
    Edit window (s)<input type="number" name="edit_window_secs" value="{edit_win}" min="0" max="86400">
  </label>
</div>
<div class="board-settings-grid" style="margin-top:0.4rem;">
  <label title="Minimum seconds a user must wait between posts on this board. 0 = no cooldown.">
    Post cooldown (s)<input type="number" name="post_cooldown_secs" value="{cooldown}" min="0" max="3600">
  </label>
</div>
<div class="board-settings-actions">
  <button type="submit">save settings</button>
</div>
</form>
<!-- FIX[LOW-10]: Delete form is now OUTSIDE the settings form. -->
<form method="POST" action="/admin/board/delete" style="display:inline;margin-top:4px">
  <input type="hidden" name="_csrf"     value="{csrf}">
  <input type="hidden" name="board_id"  value="{id}">
  <button type="submit" class="btn-danger"
          data-confirm="Delete /{short}/ and ALL its content?">delete board</button>
</form>
<a href="/admin/board/backup/{short}" style="display:inline-block;margin-left:0.5rem;margin-top:4px">
  <button type="button">&#8659; download to computer /{short}/</button>
</a>
<form method="POST" action="/admin/board/backup/create" style="display:inline-block;margin-left:0.25rem;margin-top:4px">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_short" value="{short}">
  <button type="submit">&#128190; save to server /{short}/</button>
</form>
</details>"#,
            short = escape_html(&b.short_name),
            name = escape_html(&b.name),
            nsfw_tag = if b.nsfw { r#"<span class="tag nsfw-tag">NSFW</span>"# } else { "" },
            csrf = escape_html(csrf_token),
            id = b.id,
            name_raw = escape_html(&b.name),
            desc_raw = escape_html(&b.description),
            bump = b.bump_limit,
            maxt = b.max_threads,
            edit_win = b.edit_window_secs,
            edit_win_display = if b.allow_editing { "" } else { "display:none" },
            cooldown = b.post_cooldown_secs,
            nsfw_ck = checked(b.nsfw),
            img_ck = checked(b.allow_images),
            vid_ck = checked(b.allow_video),
            aud_ck = checked(b.allow_audio),
            trip_ck = checked(b.allow_tripcodes),
            archive_ck = checked(b.allow_archive),
            edit_ck = checked(b.allow_editing),
            embeds_ck = checked(b.allow_video_embeds),
            captcha_ck = checked(b.allow_captcha),
        ));
    }

    let mut ban_rows = String::new();
    for ban in bans {
        let expires = ban
            .expires_at
            .map(fmt_ts)
            .unwrap_or_else(|| "permanent".to_string());
        ban_rows.push_str(&format!(
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
            // FIX[MEDIUM-13]: Use .get(..16) to avoid a panic if ip_hash < 16 chars.
            escape_html(ban.ip_hash.get(..16).unwrap_or(&ban.ip_hash)),
            escape_html(ban.reason.as_deref().unwrap_or("")),
            escape_html(&expires),
            csrf = escape_html(csrf_token),
            id = ban.id,
        ));
    }

    let mut filter_rows = String::new();
    for f in filters {
        filter_rows.push_str(&format!(
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
            csrf = escape_html(csrf_token),
            id = f.id,
        ));
    }

    // ── Full backup file list ─────────────────────────────────────────────────
    let mut full_backup_rows = String::new();
    if full_backups.is_empty() {
        full_backup_rows.push_str(
            "<tr><td colspan=\"4\" style=\"color:var(--text-dim);text-align:center\">no backups yet</td></tr>",
        );
    }
    for bf in full_backups {
        let size_fmt = format_file_size(bf.size_bytes as i64);
        full_backup_rows.push_str(&format!(
            r#"<tr>
<td style="word-break:break-all">{fname}</td>
<td style="white-space:nowrap">{size}</td>
<td style="white-space:nowrap">{modified}</td>
<td style="white-space:nowrap">
  <a href="/admin/backup/download/full/{fname}" style="margin-right:0.4rem">&#8659; download to computer</a>
  <form method="POST" action="/admin/backup/restore-saved" style="display:inline;margin-right:0.4rem">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="filename" value="{fname}">
    <button type="submit" data-confirm="WARNING: Restore from {fname}? This will overwrite the live database and all uploads. Cannot be undone.">&#8635; restore</button>
  </form>
  <form method="POST" action="/admin/backup/delete" style="display:inline">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="kind" value="full">
    <input type="hidden" name="filename" value="{fname}">
    <button type="submit" class="btn-danger" data-confirm="Delete {fname}? This cannot be undone.">&#10005; delete</button>
  </form>
</td>
</tr>"#,
            fname = escape_html(&bf.filename),
            size = size_fmt,
            modified = escape_html(&bf.modified),
            csrf = escape_html(csrf_token),
        ));
    }

    // ── Board backup file list ────────────────────────────────────────────────
    let mut board_backup_rows = String::new();
    if board_backups.is_empty() {
        board_backup_rows.push_str(
            "<tr><td colspan=\"4\" style=\"color:var(--text-dim);text-align:center\">no board backups yet</td></tr>",
        );
    }
    for bf in board_backups {
        let size_fmt = format_file_size(bf.size_bytes as i64);
        board_backup_rows.push_str(&format!(
            r#"<tr>
<td style="word-break:break-all">{fname}</td>
<td style="white-space:nowrap">{size}</td>
<td style="white-space:nowrap">{modified}</td>
<td style="white-space:nowrap">
  <a href="/admin/backup/download/board/{fname}" style="margin-right:0.4rem">&#8659; download to computer</a>
  <form method="POST" action="/admin/board/backup/restore-saved" style="display:inline;margin-right:0.4rem">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="filename" value="{fname}">
    <button type="submit" data-confirm="WARNING: Restore board from {fname}? This will wipe and replace that board. Cannot be undone.">&#8635; restore</button>
  </form>
  <form method="POST" action="/admin/backup/delete" style="display:inline">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="kind" value="board">
    <input type="hidden" name="filename" value="{fname}">
    <button type="submit" class="btn-danger" data-confirm="Delete {fname}? This cannot be undone.">&#10005; delete</button>
  </form>
</td>
</tr>"#,
            fname = escape_html(&bf.filename),
            size = size_fmt,
            modified = escape_html(&bf.modified),
            csrf = escape_html(csrf_token),
        ));
    }

    // ── Report inbox ──────────────────────────────────────────────────────────
    let report_count = reports.len();
    let report_badge = if report_count > 0 {
        format!(r#" <span class="report-badge">{}</span>"#, report_count)
    } else {
        String::new()
    };

    let mut report_rows = String::new();
    if reports.is_empty() {
        report_rows.push_str(
            r#"<tr><td colspan="5" style="color:var(--text-dim);text-align:center">no open reports</td></tr>"#,
        );
    }
    for rc in reports {
        let preview = escape_html(rc.post_preview.trim());
        let reason = escape_html(&rc.report.reason);
        let age = fmt_ts(rc.report.created_at);
        let ip_short = rc.post_ip_hash.get(..16).unwrap_or(&rc.post_ip_hash);
        report_rows.push_str(&format!(
            r#"<tr>
<td><a href="/{board}/thread/{tid}#p{pid}" title="view post">/{board}/ No.{pid}</a></td>
<td style="max-width:240px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="{preview}">{preview}</td>
<td>{reason}</td>
<td style="white-space:nowrap;font-size:0.78rem">{age}</td>
<td style="white-space:nowrap">
  <form method="POST" action="/admin/report/resolve" style="display:inline">
    <input type="hidden" name="_csrf"      value="{csrf}">
    <input type="hidden" name="report_id"  value="{rid}">
    <button type="submit">&#10003; resolve</button>
  </form>
  <form method="POST" action="/admin/report/resolve" style="display:inline;margin-left:0.35rem"
        data-confirm-submit="Resolve report AND permanently ban this IP?">
    <input type="hidden" name="_csrf"      value="{csrf}">
    <input type="hidden" name="report_id"  value="{rid}">
    <input type="hidden" name="ban_ip_hash" value="{ip_hash}">
    <input type="hidden" name="ban_reason"  value="Reported content">
    <button type="submit" class="btn-danger">&#10003; resolve + ban</button>
  </form>
</td>
</tr>"#,
            board   = escape_html(&rc.board_short),
            tid     = rc.report.thread_id,
            pid     = rc.report.post_id,
            preview = preview,
            reason  = reason,
            age     = escape_html(&age),
            csrf    = escape_html(csrf_token),
            rid     = rc.report.id,
            ip_hash = escape_html(&rc.post_ip_hash),
        ));
        let _ = ip_short; // suppress unused warning
    }

    // ── Ban appeals ───────────────────────────────────────────────────────────
    let appeal_badge = if !appeals.is_empty() {
        format!(r#" <span class="report-badge">{}</span>"#, appeals.len())
    } else {
        String::new()
    };

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
        appeal_rows.push_str(&format!(
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
            reason   = reason,
            age      = escape_html(&age),
            csrf     = escape_html(csrf_token),
            aid      = a.id,
            ip_hash  = escape_html(&a.ip_hash),
        ));
    }

    let body = format!(
        r#"<div class="admin-panel">
<h1>[ admin panel ]</h1>
<form method="POST" action="/admin/logout">
<input type="hidden" name="_csrf" value="{csrf}">
<button type="submit">logout</button>
</form>

<section class="admin-section" id="reports">
<h2>// report inbox{report_badge}</h2>
<table class="admin-table">
<thead><tr><th>post</th><th>content preview</th><th>reason</th><th>filed</th><th>action</th></tr></thead>
<tbody>{report_rows}</tbody>
</table>
</section>

<section class="admin-section" id="appeals">
<h2>// ban appeals{appeal_badge}</h2>
<table class="admin-table">
<thead><tr><th>ip (partial)</th><th>appeal message</th><th>filed</th><th>action</th></tr></thead>
<tbody>{appeal_rows}</tbody>
</table>
</section>

<section class="admin-section">
<h2>// boards</h2>
<div class="admin-board-cards">{board_cards}</div>
<h3>create board</h3>
<form method="POST" action="/admin/board/create">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="text" name="short_name" placeholder="short (e.g. tech)" maxlength="8" required>
<input type="text" name="name" placeholder="full name" maxlength="64" required>
<input type="text" name="description" placeholder="description" maxlength="256">
<label style="color:var(--text-dim);font-size:0.8rem"><input type="checkbox" name="nsfw" value="1"> NSFW</label>
<button type="submit">create</button>
</form>
</section>

<section class="admin-section">
<h2>// active bans</h2>
<table class="admin-table">
<thead><tr><th>ip hash (partial)</th><th>reason</th><th>expires</th><th>action</th></tr></thead>
<tbody>{ban_rows}</tbody>
</table>
<h3>add ban</h3>
<form method="POST" action="/admin/ban/add">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="text" name="ip_hash" placeholder="ip hash" required>
<input type="text" name="reason" placeholder="reason">
<input type="text" name="duration_hours" placeholder="hours (blank=perm)" style="width:120px">
<button type="submit">ban</button>
</form>
</section>

<section class="admin-section">
<h2>// word filters</h2>
<table class="admin-table">
<thead><tr><th>pattern</th><th>replacement</th><th>action</th></tr></thead>
<tbody>{filter_rows}</tbody>
</table>
<h3>add filter</h3>
<form method="POST" action="/admin/filter/add">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="text" name="pattern" placeholder="pattern to match" required>
<input type="text" name="replacement" placeholder="replace with">
<button type="submit">add</button>
</form>
</section>

<section class="admin-section">
<h2>// site settings</h2>
<form method="POST" action="/admin/site/settings">
<input type="hidden" name="_csrf" value="{csrf}">
<div class="board-settings-grid" style="margin-bottom:0.75rem">
  <label>Site name
    <input type="text" name="site_name" value="{site_name_val}" maxlength="64" placeholder="RustChan"
           style="font-family:inherit">
  </label>
  <label>Home page subtitle
    <input type="text" name="site_subtitle" value="{site_subtitle_val}" maxlength="128" placeholder="select board to proceed"
           style="font-family:inherit">
  </label>
</div>
<div class="board-settings-checks" style="margin-bottom:0.75rem">
  <label title="When enabled, 3 or more consecutive greentext lines are wrapped in a collapsible block. Existing posts are not affected — only new posts use the current setting.">
    <input type="checkbox" name="collapse_greentext" value="1"{collapse_ck}>
    Collapse long greentext walls (3+ lines) into expandable blocks
  </label>
</div>
<button type="submit">save settings</button>
</form>
</section>

<section class="admin-section">
<h2>// moderation log <a href="/admin/mod-log" style="font-size:0.78rem;margin-left:0.6rem;color:var(--text-dim)">[ view full log ]</a></h2>
<p style="color:var(--text-dim);font-size:0.82rem">All admin actions are recorded in the moderation log. Click <em>view full log</em> to browse the history.</p>
</section>

<section class="admin-section">
<h2>// backup &amp; restore</h2>
<p style="color:var(--text-dim);font-size:0.85rem">Full backups include the complete database and all uploaded files. <strong>Save to server</strong> stores the backup in <code>rustchan-data/full-backups/</code> on the server filesystem (listed below). <strong>Restore from local file</strong> uploads a zip from your computer.</p>
<div style="display:flex;gap:1rem;flex-wrap:wrap;align-items:center;margin-top:0.75rem;margin-bottom:0.75rem">
<form method="POST" action="/admin/backup/create">
<input type="hidden" name="_csrf" value="{csrf}">
<button type="submit">&#128190; save to server</button>
</form>
<form method="POST" action="/admin/restore" enctype="multipart/form-data" style="display:flex;gap:0.5rem;align-items:center;flex-wrap:wrap">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="file" name="backup_file" accept=".zip" required style="color:var(--text)">
<button type="submit" class="btn-danger"
        data-confirm="WARNING: This will overwrite the database and all uploaded files. Cannot be undone. Continue?">&#8635; restore from local file</button>
</form>
</div>
<table style="width:100%;border-collapse:collapse;font-size:0.85rem">
<thead><tr style="color:var(--text-dim)"><th style="text-align:left">filename</th><th style="text-align:left">size</th><th style="text-align:left">created</th><th></th></tr></thead>
<tbody>{full_backup_rows}</tbody>
</table>
</section>

<section class="admin-section">
<h2>// board backup &amp; restore</h2>
<p style="color:var(--text-dim);font-size:0.85rem">Board backups cover a single board. Use <em>save to server</em> on a board card above to store the backup in <code>rustchan-data/board-backups/</code>, or use the table below to download, restore, or delete saved backups. <strong>Restore from local file</strong> uploads a zip from your computer.</p>
<div style="margin-top:0.5rem;margin-bottom:0.75rem">
<form method="POST" action="/admin/board/restore" enctype="multipart/form-data" style="display:flex;gap:0.5rem;align-items:center;flex-wrap:wrap">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="file" name="backup_file" accept=".zip" required style="color:var(--text)">
<button type="submit" class="btn-danger"
        data-confirm="WARNING: This will wipe and replace the board from the backup zip. Other boards are unaffected. Continue?">&#8635; restore board from local file</button>
</form>
</div>
<table style="width:100%;border-collapse:collapse;font-size:0.85rem">
<thead><tr style="color:var(--text-dim)"><th style="text-align:left">filename</th><th style="text-align:left">size</th><th style="text-align:left">created</th><th></th></tr></thead>
<tbody>{board_backup_rows}</tbody>
</table>
</section>

<section class="admin-section">
<h2>// database maintenance</h2>
<p style="color:var(--text-dim);font-size:0.85rem">
  Current database size: <strong>{db_size_str}</strong>.
  Running <strong>VACUUM</strong> rewrites the database file compactly, reclaiming space left after
  bulk deletions (deleted threads, pruned posts, etc.).  This may take a few seconds on large
  databases and briefly blocks writes.
</p>
<form method="POST" action="/admin/vacuum">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="Run VACUUM? This will briefly block the database while it rebuilds. Continue?">&#x1F9F9; run VACUUM</button>
</form>
</section>
{tor_section}
</div>"#,
        csrf = escape_html(csrf_token),
        board_cards = board_cards,
        ban_rows = ban_rows,
        filter_rows = filter_rows,
        collapse_ck = if collapse_greentext { " checked" } else { "" },
        full_backup_rows = full_backup_rows,
        board_backup_rows = board_backup_rows,
        db_size_str = format_file_size(db_size_bytes),
        report_rows = report_rows,
        report_badge = report_badge,
        appeal_rows = appeal_rows,
        appeal_badge = appeal_badge,
        site_name_val = escape_html(site_name),
        site_subtitle_val = escape_html(site_subtitle),
        tor_section = match tor_address {
            Some(addr) => format!(
                r#"<section class="admin-section" style="border-top:1px solid var(--border);padding-top:1rem;margin-top:0;text-align:center">
<p style="color:var(--text-dim);font-size:0.82rem;margin:0">
  <code style="user-select:all;color:var(--text)">{}</code>
</p>
</section>"#,
                escape_html(addr)
            ),
            None => String::new(),
        },
    );

    base_layout("admin panel", None, &body, csrf_token, boards, false, false)
}

// ─── Moderation log ───────────────────────────────────────────────────────────

pub fn mod_log_page(
    entries: &[crate::models::ModLogEntry],
    pagination: &crate::models::Pagination,
    csrf_token: &str,
    boards: &[Board],
) -> String {
    let mut rows = String::new();
    if entries.is_empty() {
        rows.push_str(r#"<tr><td colspan="6" style="color:var(--text-dim);text-align:center">no entries yet</td></tr>"#);
    }
    for e in entries {
        let target = if let Some(id) = e.target_id {
            format!("{} #{}", e.target_type, id)
        } else {
            e.target_type.clone()
        };
        let board_link = if !e.board_short.is_empty() {
            format!(r#"<a href="/{s}">{s}</a>"#, s = escape_html(&e.board_short))
        } else {
            String::new()
        };
        rows.push_str(&format!(
            r#"<tr>
<td style="white-space:nowrap;font-size:0.78rem">{time}</td>
<td><strong>{admin}</strong></td>
<td><code>{action}</code></td>
<td style="font-size:0.82rem">{target}</td>
<td>{board}</td>
<td style="max-width:260px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:0.8rem"
    title="{detail}">{detail}</td>
</tr>"#,
            time   = escape_html(&fmt_ts(e.created_at)),
            admin  = escape_html(&e.admin_name),
            action = escape_html(&e.action),
            target = escape_html(&target),
            board  = board_link,
            detail = escape_html(&e.detail),
        ));
    }

    let pagination_html = render_pagination(pagination, "/admin/mod-log");

    let body = format!(
        r#"<div class="page-box">
<div class="board-header">
  <a href="/admin/panel">[ back to panel ]</a>
  <h2 style="margin:0.5rem 0 0.25rem">// moderation log</h2>
  <p style="color:var(--text-dim);font-size:0.82rem">{total} total entries</p>
</div>
<table class="admin-table" style="width:100%;font-size:0.85rem">
<thead><tr>
  <th>time</th><th>admin</th><th>action</th><th>target</th><th>board</th><th>detail</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>
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
        false,
        false,
    )
}

// ─── VACUUM result ────────────────────────────────────────────────────────────

pub fn admin_vacuum_result_page(size_before: i64, size_after: i64, csrf_token: &str) -> String {
    let saved = size_before.saturating_sub(size_after);
    let pct = if size_before > 0 {
        (saved as f64 / size_before as f64 * 100.0) as u64
    } else {
        0
    };

    let body = format!(
        r#"<div class="admin-panel">
<h1>[ VACUUM complete ]</h1>
<section class="admin-section">
<h2>// result</h2>
<table class="admin-table" style="max-width:420px">
<tbody>
  <tr><td>Before</td><td><strong>{before}</strong></td></tr>
  <tr><td>After</td><td><strong>{after}</strong></td></tr>
  <tr><td>Reclaimed</td><td><strong style="color:var(--green-bright)">{saved}</strong> ({pct}%)</td></tr>
</tbody>
</table>
<p style="margin-top:1rem">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        before = format_file_size(size_before),
        after = format_file_size(size_after),
        saved = format_file_size(saved),
        pct = pct,
    );

    base_layout("VACUUM result", None, &body, csrf_token, &[], false, false)
}

// ─── IP history ───────────────────────────────────────────────────────────────

pub fn admin_ip_history_page(
    ip_hash: &str,
    posts_with_boards: &[(crate::models::Post, String)],
    pagination: &crate::models::Pagination,
    all_boards: &[Board],
    csrf_token: &str,
) -> String {
    use crate::models::MediaType;

    let mut rows = String::new();

    if posts_with_boards.is_empty() {
        rows.push_str(r#"<tr><td colspan="6" style="color:var(--text-dim);text-align:center">no posts found for this IP hash</td></tr>"#);
    }

    for (post, board_short) in posts_with_boards {
        let media_badge = match &post.media_type {
            Some(MediaType::Image) => r#"<span style="color:var(--green-bright)">[img]</span>"#,
            Some(MediaType::Video) => r#"<span style="color:var(--text-dim)">[vid]</span>"#,
            Some(MediaType::Audio) => r#"<span style="color:var(--text-dim)">[aud]</span>"#,
            None => "",
        };
        let thread_link = format!(
            r#"<a href="/{board}/thread/{tid}#p{pid}">/{board}/ No.{pid}</a>"#,
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
        let body_preview = if post.body.len() > 120 {
            format!("{}…", escape_html(&body_preview))
        } else {
            escape_html(&body_preview)
        };

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

        rows.push_str(&format!(
            r#"<tr>
<td style="white-space:nowrap;font-size:0.8rem">{time}</td>
<td>{link}{op}</td>
<td style="font-size:0.8rem">{media}</td>
<td style="max-width:480px;word-break:break-word;font-size:0.85rem">{body}</td>
<td>{del}</td>
</tr>"#,
            time = fmt_ts_short(post.created_at),
            link = thread_link,
            op = op_badge,
            media = media_badge,
            body = body_preview,
            del = del_form,
        ));
    }

    let pag_html = render_pagination(pagination, &format!("/admin/ip/{}", escape_html(ip_hash)));

    let body = format!(
        r#"<div class="admin-panel">
<h1>[ IP history ]</h1>
<section class="admin-section">
<h2>// posts by <code style="font-size:0.9rem">{hash_display}</code></h2>
<p style="color:var(--text-dim);font-size:0.85rem">
  {total} post{plural} found across all boards.
  <a href="/admin/panel" style="margin-left:1rem">&#8592; back to panel</a>
</p>
<table class="admin-table" style="width:100%">
<thead><tr>
  <th style="text-align:left">time</th>
  <th style="text-align:left">post</th>
  <th>media</th>
  <th style="text-align:left">body</th>
  <th>del</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>
{pagination}
</section>
</div>"#,
        hash_display = escape_html(ip_hash),
        total = pagination.total,
        plural = if pagination.total == 1 { "" } else { "s" },
        rows = rows,
        pagination = pag_html,
    );

    base_layout(
        &format!("IP history — {}", &ip_hash[..ip_hash.len().min(12)]),
        None,
        &body,
        csrf_token,
        all_boards,
        false,
        false,
    )
}
