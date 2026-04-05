// templates/admin.rs
//
// Page templates for the admin interface:
//   admin_login_page        — login form
//   admin_panel_page        — main control panel (boards, bans, reports, …)
//   mod_log_page            — moderation history
//   admin_vacuum_result_page — post-VACUUM feedback
//   admin_ip_history_page   — posts by IP hash

use crate::db::DbHealthReport;
use crate::models::{BackupInfo, Ban, Board, WordFilter};
use crate::utils::{files::format_file_size, sanitize::escape_html};
use std::fmt::Write;

use super::{base_layout, fmt_ts, fmt_ts_short, render_pagination};

// ─── Admin login ──────────────────────────────────────────────────────────────

#[must_use]
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
    base_layout(
        "admin login",
        None,
        &body,
        csrf_token,
        boards,
        None,
        None,
        false,
        "/admin",
    )
}

// ─── Admin panel ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn admin_panel_page(
    boards: &[Board],
    bans: &[Ban],
    filters: &[WordFilter],
    csrf_token: &str,
    full_backups: &[BackupInfo],
    board_backups: &[BackupInfo],
    db_size_bytes: i64,
    db_size_warning: bool,
    reports: &[crate::models::ReportWithContext],
    appeals: &[crate::models::BanAppeal],
    site_name: &str,
    site_subtitle: &str,
    default_theme: &str,
    themes: &[crate::models::Theme],
    tor_address: Option<&str>,
    // Optional one-time flash message shown at the top of the panel.
    // (is_error, message) — is_error=true → red, false → green.
    flash: Option<(bool, &str)>,
) -> String {
    let global_favicon_exists = crate::favicon::global_has_custom_favicon();
    let global_favicon_version =
        crate::favicon::favicon_version_for_board(None).unwrap_or_default();
    let enabled_theme_options = themes
        .iter()
        .filter(|theme| theme.enabled)
        .map(|theme| {
            format!(
                r#"<option value="{slug}"{selected}>{label}</option>"#,
                slug = escape_html(&theme.slug),
                selected = if theme.slug == default_theme {
                    " selected"
                } else {
                    ""
                },
                label = escape_html(&theme.display_name)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let mut theme_cards = String::new();
    let mut board_cards = String::new();
    for (index, b) in boards.iter().enumerate() {
        let checked = |v: bool| if v { " checked" } else { "" };
        let prev_same_group = index
            .checked_sub(1)
            .and_then(|prev| boards.get(prev))
            .is_some_and(|prev| prev.nsfw == b.nsfw);
        let next_same_group = boards
            .get(index + 1)
            .is_some_and(|next| next.nsfw == b.nsfw);
        let board_favicon_exists = crate::favicon::board_has_custom_favicon(&b.short_name);
        let board_favicon_version =
            crate::favicon::favicon_version_for_board(Some(&b.short_name)).unwrap_or_default();
        let _ = write!(
            board_cards,
            r#"{group_gap}<details class="board-settings-card" id="board-{short}">
<summary>/{short}/ — {name} {nsfw_tag}</summary>
<div class="board-order-toolbar">
<span>{group_label} order: {display_order}</span>
<form method="POST" action="/admin/board/reorder">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_id" value="{id}">
  <input type="hidden" name="direction" value="up">
  <input type="hidden" name="return_to" value="/admin/panel#board-{short}">
  <button type="submit"{move_up_disabled}>move up</button>
</form>
<form method="POST" action="/admin/board/reorder">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_id" value="{id}">
  <input type="hidden" name="direction" value="down">
  <input type="hidden" name="return_to" value="/admin/panel#board-{short}">
  <button type="submit"{move_down_disabled}>move down</button>
</form>
</div>
<form method="POST" action="/admin/board/settings" class="board-settings-form">
<input type="hidden" name="_csrf"     value="{csrf}">
<input type="hidden" name="board_id"  value="{id}">
<div class="board-settings-grid">
  <label>Name<input type="text" name="name" value="{name_raw}" maxlength="64" required></label>
  <label>Description<input type="text" name="description" value="{desc_raw}" maxlength="256"></label>
  <label>Bump limit<input type="number" name="bump_limit" value="{bump}" min="1" max="10000"></label>
  <label>Max threads<input type="number" name="max_threads" value="{maxt}" min="1" max="1000"></label>
  <label>Max archived threads<input type="number" name="max_archived_threads" value="{max_archived}" min="1" max="10000"></label>
  <label>Board default theme
    <select name="default_theme">
      <option value=""{inherit_theme_selected}>Inherit site default</option>
      {board_theme_options}
    </select>
  </label>
</div>
<div class="board-settings-checks">
  <label><input type="checkbox" name="nsfw"            value="1"{nsfw_ck}> NSFW</label>
  <label><input type="checkbox" name="allow_images"    value="1"{img_ck}>  Allow images</label>
  <label><input type="checkbox" name="allow_video"     value="1"{vid_ck}>  Allow video</label>
  <label><input type="checkbox" name="allow_audio"     value="1"{aud_ck}>  Allow audio</label>
  {any_files_toggle}
  <label><input type="checkbox" name="allow_tripcodes" value="1"{trip_ck}> Allow tripcodes</label>
  <label><input type="checkbox" name="allow_archive"   value="1"{archive_ck}> Enable archive</label>
  <label><input type="checkbox" name="allow_video_embeds" value="1"{embeds_ck}> Embed video links (YouTube / Invidious / Streamable)</label>
  <label><input type="checkbox" name="allow_captcha"      value="1"{captcha_ck}> PoW CAPTCHA on threads and replies (hashcash, JS-solved)</label>
  <label><input type="checkbox" name="show_poster_ids"    value="1"{poster_ids_ck}> Show thread-local poster IDs</label>
  <label title="When enabled, 3 or more consecutive greentext lines are wrapped in a collapsible block for this board. Existing posts are not affected.">
    <input type="checkbox" name="collapse_greentext" value="1"{collapse_ck}> Collapse long greentext walls (3+ lines) into expandable blocks
  </label>
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
<div class="favicon-inline-row">
{board_favicon_preview}
<form method="POST" action="/admin/board/favicon" enctype="multipart/form-data" class="favicon-inline-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_id" value="{id}">
  <label class="favicon-inline-label">
    {board_favicon_label}
    <input type="file" name="favicon" accept="image/png,image/jpeg,image/webp" required class="favicon-inline-input">
  </label>
  <button type="submit">{board_favicon_button}</button>
</form>
{board_favicon_clear}
</div>
<p style="color:var(--text-dim);font-size:0.78rem;margin:0.35rem 0 0">
  {board_favicon_status}
</p>
<!-- Delete form is now OUTSIDE the settings form. -->
<form method="POST" action="/admin/board/delete" style="display:inline;margin-top:4px">
  <input type="hidden" name="_csrf"     value="{csrf}">
  <input type="hidden" name="board_id"  value="{id}">
  <button type="submit" class="btn-danger"
          data-confirm="Delete /{short}/ and ALL its content?">delete board</button>
</form>
<form method="POST" action="/admin/board/backup/create" class="board-backup-download-form" data-board="{short}" style="display:inline-block;margin-left:0.5rem;margin-top:4px">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_short" value="{short}">
  <input type="hidden" name="download_after_create" value="1">
  <button type="submit">&#8659; download to computer /{short}/</button>
</form>
<form method="POST" action="/admin/board/backup/create" class="board-backup-create-form" data-board="{short}" style="display:inline-block;margin-left:0.25rem;margin-top:4px">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_short" value="{short}">
  <button type="submit">&#128190; save to server /{short}/</button>
</form>
</details>"#,
            short = escape_html(&b.short_name),
            name = escape_html(&b.name),
            nsfw_tag = if b.nsfw {
                r#"<span class="tag nsfw-tag">NSFW</span>"#
            } else {
                ""
            },
            csrf = escape_html(csrf_token),
            id = b.id,
            group_gap = if index > 0 && b.nsfw && !prev_same_group {
                "<div class=\"admin-board-group-gap\" aria-hidden=\"true\"></div>"
            } else {
                ""
            },
            group_label = if b.nsfw { "NSFW" } else { "SFW" },
            display_order = b.display_order,
            name_raw = escape_html(&b.name),
            desc_raw = escape_html(&b.description),
            bump = b.bump_limit,
            maxt = b.max_threads,
            max_archived = b.max_archived_threads,
            inherit_theme_selected = if b.default_theme.is_empty() {
                " selected"
            } else {
                ""
            },
            board_theme_options = themes
                .iter()
                .filter(|theme| theme.enabled)
                .map(|theme| {
                    format!(
                        r#"<option value="{slug}"{selected}>{label}</option>"#,
                        slug = escape_html(&theme.slug),
                        selected = if theme.slug == b.default_theme {
                            " selected"
                        } else {
                            ""
                        },
                        label = escape_html(&theme.display_name)
                    )
                })
                .collect::<Vec<_>>()
                .join(""),
            edit_win = b.edit_window_secs,
            edit_win_display = if b.allow_editing { "" } else { "display:none" },
            cooldown = b.post_cooldown_secs,
            nsfw_ck = checked(b.nsfw),
            img_ck = checked(b.allow_images),
            vid_ck = checked(b.allow_video),
            aud_ck = checked(b.allow_audio),
            any_files_toggle = if crate::config::CONFIG.enable_any_file_uploads_feature {
                format!(
                    r#"<label><input type="checkbox" name="allow_any_files" value="1"{}> Allow any file downloads</label>"#,
                    checked(b.allow_any_files)
                )
            } else {
                String::new()
            },
            trip_ck = checked(b.allow_tripcodes),
            archive_ck = checked(b.allow_archive),
            edit_ck = checked(b.allow_editing),
            embeds_ck = checked(b.allow_video_embeds),
            captcha_ck = checked(b.allow_captcha),
            poster_ids_ck = checked(b.show_poster_ids),
            collapse_ck = checked(b.collapse_greentext),
            move_up_disabled = if prev_same_group { "" } else { " disabled" },
            move_down_disabled = if next_same_group { "" } else { " disabled" },
            board_favicon_preview = if board_favicon_exists {
                format!(
                    r#"<img class="favicon-inline-preview" src="/boards/{short}/_favicon/favicon-32x32.png?v={version}" alt="/{short}/ favicon">"#,
                    short = escape_html(&b.short_name),
                    version = escape_html(&board_favicon_version)
                )
            } else {
                String::new()
            },
            board_favicon_label = if board_favicon_exists {
                "replace favicon"
            } else {
                "board favicon"
            },
            board_favicon_button = if board_favicon_exists {
                "replace"
            } else {
                "upload"
            },
            board_favicon_status = if board_favicon_exists {
                "Custom board favicon is active here and overrides the global favicon."
            } else {
                "No board-specific favicon set. This board uses the global favicon."
            },
            board_favicon_clear = if board_favicon_exists {
                format!(
                    r#"<form method="POST" action="/admin/board/favicon/clear" class="favicon-inline-clear">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="board_id" value="{id}">
  <button type="submit">clear</button>
</form>"#,
                    csrf = escape_html(csrf_token),
                    id = b.id
                )
            } else {
                String::new()
            }
        );
    }

    let mut ban_rows = String::new();
    for ban in bans {
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
            // Use .get(..16) to avoid a panic if ip_hash < 16 chars.
            escape_html(ban.ip_hash.get(..16).unwrap_or(&ban.ip_hash)),
            escape_html(ban.reason.as_deref().unwrap_or("")),
            escape_html(&expires),
            csrf = escape_html(csrf_token),
            id = ban.id
        );
    }

    let mut filter_rows = String::new();
    for f in filters {
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
            csrf = escape_html(csrf_token),
            id = f.id
        );
    }

    // ── Full backup file list ─────────────────────────────────────────────────
    let mut full_backup_rows = String::new();
    if full_backups.is_empty() {
        full_backup_rows.push_str(
            "<tr><td colspan=\"4\" style=\"color:var(--text-dim);text-align:center\">no backups yet</td></tr>",
        );
    }
    for bf in full_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let _ = write!(
            full_backup_rows,
            r#"<tr>
<td style="word-break:break-all">{fname}</td>
<td style="white-space:nowrap">{size}</td>
<td style="white-space:nowrap">{modified}</td>
<td style="white-space:nowrap">
  <a href="/admin/backup/download/full/{fname}" class="backup-download-link" data-backup-label="full backup" style="margin-right:0.4rem">&#8659; download to computer</a>
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
            csrf = escape_html(csrf_token)
        );
    }

    // ── Board backup file list ────────────────────────────────────────────────
    let mut board_backup_rows = String::new();
    if board_backups.is_empty() {
        board_backup_rows.push_str(
            "<tr><td colspan=\"4\" style=\"color:var(--text-dim);text-align:center\">no board backups yet</td></tr>",
        );
    }
    for bf in board_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let _ = write!(
            board_backup_rows,
            r#"<tr>
<td style="word-break:break-all">{fname}</td>
<td style="white-space:nowrap">{size}</td>
<td style="white-space:nowrap">{modified}</td>
<td style="white-space:nowrap">
  <a href="/admin/backup/download/board/{fname}" class="backup-download-link" data-backup-label="board backup" style="margin-right:0.4rem">&#8659; download to computer</a>
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
            csrf = escape_html(csrf_token)
        );
    }

    // ── Report inbox ──────────────────────────────────────────────────────────
    let report_count = reports.len();
    let appeal_count = appeals.len();
    let ban_count = bans.len();
    let filter_count = filters.len();
    let report_badge = if report_count > 0 {
        format!(r#" <span class="report-badge">{report_count}</span>"#)
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
        // ip_short was computed here but immediately discarded with
        // `let _ = ip_short` — dead code from an unfinished refactor.  Removed.
        let _ = write!(
            report_rows,
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
            board = escape_html(&rc.board_short),
            tid = rc.report.thread_id,
            pid = rc.report.post_id,
            preview = preview,
            reason = reason,
            age = escape_html(&age),
            csrf = escape_html(csrf_token),
            rid = rc.report.id,
            ip_hash = escape_html(rc.post_ip_hash.as_deref().unwrap_or(""))
        );
    }

    // ── Ban appeals ───────────────────────────────────────────────────────────
    let appeal_badge = if appeal_count > 0 {
        format!(r#" <span class="report-badge">{appeal_count}</span>"#)
    } else {
        String::new()
    };
    let ban_badge = format!(r#" <span class="admin-count-badge">{ban_count}</span>"#);
    let filter_badge = format!(r#" <span class="admin-count-badge">{filter_count}</span>"#);
    let moderation_open_attr = "";
    let moderation_summary_badges =
        format!("{report_badge}{appeal_badge}{ban_badge}{filter_badge}");

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
            csrf = escape_html(csrf_token),
            aid = a.id,
            ip_hash = escape_html(&a.ip_hash)
        );
    }

    if themes.is_empty() {
        theme_cards.push_str(r#"<p style="color:var(--text-dim);margin:0">No themes loaded.</p>"#);
    } else {
        for theme in themes {
            let _ = write!(
                theme_cards,
                r#"<details class="board-settings-card">
<summary>{name} <code>/{slug}/</code>{builtin_tag}{disabled_tag}</summary>
<form method="POST" action="/admin/theme/update" class="board-settings-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="existing_slug" value="{slug}">
  <div class="board-settings-grid">
    <label>Display name<input type="text" name="display_name" value="{name}" maxlength="64" required></label>
    <label>Slug<input type="text" name="slug" value="{slug}" maxlength="32"{slug_readonly}></label>
    <label>Swatch<input type="text" name="swatch_hex" value="{swatch}" maxlength="7"></label>
  </div>
  <div class="board-settings-grid" style="margin-top:0.5rem">
    <label>Description<input type="text" name="description" value="{description}" maxlength="256"></label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="enabled" value="1"{enabled_ck}> Enabled</label>
  </div>
  <div class="board-settings-grid" style="margin-top:0.5rem;{custom_css_display}">
    <label>Custom CSS
      <textarea name="custom_css" rows="10" placeholder="--bg: #111; --text: #eee; or full scoped CSS">{custom_css}</textarea>
    </label>
  </div>
  <div class="board-settings-actions">
    <button type="submit">save theme</button>
  </div>
</form>
{delete_form}
</details>"#,
                csrf = escape_html(csrf_token),
                name = escape_html(&theme.display_name),
                slug = escape_html(&theme.slug),
                builtin_tag = if theme.is_builtin {
                    r#" <span class="tag">built-in</span>"#
                } else {
                    ""
                },
                disabled_tag = if theme.enabled {
                    ""
                } else {
                    r#" <span class="tag locked">disabled</span>"#
                },
                slug_readonly = if theme.is_builtin { " readonly" } else { "" },
                swatch = escape_html(&theme.swatch_hex),
                description = escape_html(&theme.description),
                enabled_ck = if theme.enabled { " checked" } else { "" },
                custom_css_display = if theme.is_builtin { "display:none" } else { "" },
                custom_css = escape_html(&theme.custom_css),
                delete_form = if theme.is_builtin {
                    String::new()
                } else {
                    format!(
                        r#"<form method="POST" action="/admin/theme/delete" style="display:inline-block;margin-top:0.5rem">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="slug" value="{slug}">
  <button type="submit" class="btn-danger" data-confirm="Delete custom theme {slug}?">delete theme</button>
</form>"#,
                        csrf = escape_html(csrf_token),
                        slug = escape_html(&theme.slug)
                    )
                }
            );
        }
    }

    let flash_html = match flash {
        Some((is_error, msg)) => {
            let cls = if is_error { "flash-error" } else { "flash-ok" };
            format!(
                r#"<div class="admin-flash {cls}">{msg}</div>"#,
                cls = cls,
                msg = escape_html(msg),
            )
        }
        None => String::new(),
    };

    // 1.8: DB size warning banner — shown when the file exceeds the configured threshold.
    let db_warn_banner = if db_size_warning {
        format!(
            r#"<div class="admin-flash flash-error" style="margin-bottom:0.75rem">
&#9888; <strong>Database size warning:</strong> The database file has exceeded the configured
warning threshold ({size}). Consider running <strong>VACUUM</strong> below or archiving
old boards to prevent query performance degradation.
</div>"#,
            size = format_file_size(db_size_bytes),
        )
    } else {
        String::new()
    };

    let body = format!(
        r#"<div class="admin-panel">
{flash}
<h1>[ admin panel ]</h1>
<form method="POST" action="/admin/logout">
<input type="hidden" name="_csrf" value="{csrf}">
<button type="submit">logout</button>
</form>

<!-- ═══════════════════════════════════════════════════════════════════════════
     // live log
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="live-log">
<details class="admin-dropdown">
<summary>// live log</summary>
<div class="admin-dropdown-content">
<p style="color:var(--text-dim);font-size:0.85rem">
  Watching <span id="admin-live-log-file">current log</span>. Updates every 2 seconds.
</p>
<div style="display:flex;gap:0.75rem;align-items:center;flex-wrap:wrap;margin-bottom:0.6rem">
  <button type="button" id="admin-live-log-refresh">refresh now</button>
  <button type="button" id="admin-live-log-clear">clear</button>
  <label style="color:var(--text-dim);font-size:0.82rem">
    <input type="checkbox" id="admin-live-log-autoscroll" checked> auto-scroll
  </label>
</div>
<pre id="admin-live-log-output" style="margin:0;max-height:24rem;overflow:auto;padding:0.85rem;border:1px solid var(--border);background:var(--bg-input);color:var(--text);font-size:0.78rem;line-height:1.45;white-space:pre-wrap;word-break:break-word">Loading live log…</pre>
</div>
</details>
</section>

<!-- ═══════════════════════════════════════════════════════════════════════════
     // site settings
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="site-settings">
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
  <label>Default theme
    <select name="default_theme" style="font-family:inherit;padding:0.25rem 0.4rem;background:var(--bg-input);color:var(--text);border:1px solid var(--border)">
      {enabled_theme_options}
    </select>
  </label>
</div>
<div class="board-settings-actions" style="margin-top:0.2rem">
  <button type="submit">save settings</button>
</div>
</form>
<div class="favicon-inline-row favicon-inline-row-global">
{global_favicon_preview}
<form method="POST" action="/admin/site/favicon" enctype="multipart/form-data" class="favicon-inline-form">
<input type="hidden" name="_csrf" value="{csrf}">
<label class="favicon-inline-label">
  {global_favicon_label}
  <input type="file" name="favicon" accept="image/png,image/jpeg,image/webp" required class="favicon-inline-input">
</label>
<button type="submit">{global_favicon_button}</button>
</form>
</div>
<p style="color:var(--text-dim);font-size:0.78rem;margin:0.45rem 0 0">
  {global_favicon_status}
</p>
</section>

<section class="admin-section" id="theme-catalog">
<h2>// themes</h2>
<p style="color:var(--text-dim);font-size:0.85rem">Built-in themes can be enabled or disabled at runtime. Custom themes can be added here and selected globally or per-board once enabled.</p>
<div class="admin-board-cards">{theme_cards}</div>
<h3>create custom theme</h3>
<form method="POST" action="/admin/theme/create">
  <input type="hidden" name="_csrf" value="{csrf}">
  <div class="board-settings-grid">
    <label>Display name<input type="text" name="display_name" maxlength="64" required></label>
    <label>Slug<input type="text" name="slug" maxlength="32" required></label>
    <label>Swatch<input type="text" name="swatch_hex" maxlength="7" placeholder="7ab84e"></label>
  </div>
  <div class="board-settings-grid" style="margin-top:0.5rem">
    <label>Description<input type="text" name="description" maxlength="256"></label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="enabled" value="1" checked> Enabled</label>
  </div>
  <div class="board-settings-grid" style="margin-top:0.5rem">
    <label>Custom CSS
      <textarea name="custom_css" rows="10" placeholder="Add CSS variables or full scoped CSS for this theme." required></textarea>
    </label>
  </div>
  <div class="board-settings-actions">
    <button type="submit">create theme</button>
  </div>
</form>
</section>

<!-- ═══════════════════════════════════════════════════════════════════════════
     // boards
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// boards</h2>
<p class="admin-order-note">Board order is shared across the homepage, top bar, and this panel, with SFW and NSFW boards each keeping their own separate order.</p>
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

<!-- ═══════════════════════════════════════════════════════════════════════════
     // moderation dropdown (log + reports + moderation tools)
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-section-collapsible" id="reports">
<details class="admin-dropdown"{moderation_open_attr}>
<summary><span>// moderation</span><span class="admin-dropdown-badges">{moderation_summary_badges}</span></summary>
<div class="admin-dropdown-content">
<p class="admin-moderation-intro">
  Live queues come first, policy controls come second, and the log stays available for historical review.
</p>
<div class="admin-moderation-grid">
  <section class="admin-moderation-card admin-moderation-card-review">
    <div class="admin-card-header">
      <h3>// review queue</h3>
      <p>Handle open reports and ban appeals before changing policy.</p>
    </div>
    <div class="admin-subsection admin-subsection-tight">
      <h4>// report inbox{report_badge}</h4>
      <table class="admin-table">
        <thead><tr><th>post</th><th>content preview</th><th>reason</th><th>filed</th><th>action</th></tr></thead>
        <tbody>{report_rows}</tbody>
      </table>
    </div>

    <div class="admin-subsection admin-subsection-tight">
      <h4 id="appeals">// ban appeals{appeal_badge}</h4>
      <table class="admin-table">
        <thead><tr><th>ip (partial)</th><th>appeal message</th><th>filed</th><th>action</th></tr></thead>
        <tbody>{appeal_rows}</tbody>
      </table>
    </div>
  </section>

  <section class="admin-moderation-card admin-moderation-card-controls">
    <div class="admin-card-header">
      <h3>// policy controls</h3>
      <p>Manage bans and automated word replacements.</p>
    </div>

    <div class="admin-subsection admin-subsection-tight" id="active-bans">
      <h4>// active bans{ban_badge}</h4>
      <table class="admin-table">
        <thead><tr><th>ip hash (partial)</th><th>reason</th><th>expires</th><th>action</th></tr></thead>
        <tbody>{ban_rows}</tbody>
      </table>
      <h4>add ban</h4>
      <form method="POST" action="/admin/ban/add" class="admin-moderation-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="text" name="ip_hash" placeholder="ip hash" required>
        <input type="text" name="reason" placeholder="reason">
        <input type="text" name="duration_hours" placeholder="hours (blank=perm)" style="width:120px">
        <button type="submit">ban</button>
      </form>
    </div>

    <div class="admin-subsection admin-subsection-tight" id="word-filters">
      <h4>// word filters{filter_badge}</h4>
      <table class="admin-table">
        <thead><tr><th>pattern</th><th>replacement</th><th>action</th></tr></thead>
        <tbody>{filter_rows}</tbody>
      </table>
      <h4>add filter</h4>
      <form method="POST" action="/admin/filter/add" class="admin-moderation-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="text" name="pattern" placeholder="pattern to match" required>
        <input type="text" name="replacement" placeholder="replace with">
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

<!-- ═══════════════════════════════════════════════════════════════════════════
     // full site backup & restore
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// full site backup &amp; restore</h2>
<p style="color:var(--text-dim);font-size:0.85rem">Full backups include the complete database and all uploaded files. <strong>Save to server</strong> stores the backup in <code>rustchan-data/backups/full/</code> on the server filesystem (listed below). <strong>Restore from local file</strong> uploads a zip from your computer.</p>
<div style="display:flex;gap:1rem;flex-wrap:wrap;align-items:center;margin-top:0.75rem;margin-bottom:0.75rem">
<form method="POST" action="/admin/backup/create" id="full-backup-create-form">
<input type="hidden" name="_csrf" value="{csrf}">
<button type="submit" id="full-backup-btn">&#128190; save to server</button>
</form>
<form method="POST" action="/admin/restore" enctype="multipart/form-data" class="backup-restore-upload-form" data-restore-label="full backup" style="display:flex;gap:0.5rem;align-items:center;flex-wrap:wrap">
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

<!-- ═══════════════════════════════════════════════════════════════════════════
     // board backup & restore
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// board backup &amp; restore</h2>
<p style="color:var(--text-dim);font-size:0.85rem">Board backups cover a single board. Use <em>save to server</em> on a board card above to store the backup in <code>rustchan-data/backups/boards/</code>, or use the table below to download, restore, or delete saved backups. <strong>Restore from local file</strong> uploads a zip from your computer.</p>
<div style="margin-top:0.5rem;margin-bottom:0.75rem">
<form method="POST" action="/admin/board/restore" enctype="multipart/form-data" class="backup-restore-upload-form" data-restore-label="board backup" style="display:flex;gap:0.5rem;align-items:center;flex-wrap:wrap">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="file" name="backup_file" accept=".zip,.json" required style="color:var(--text)">
<button type="submit" class="btn-danger"
        data-confirm="WARNING: This will wipe and replace the board from the backup. Other boards are unaffected. Continue?">&#8635; restore board from local file</button>
</form>
</div>
<table style="width:100%;border-collapse:collapse;font-size:0.85rem">
<thead><tr style="color:var(--text-dim)"><th style="text-align:left">filename</th><th style="text-align:left">size</th><th style="text-align:left">created</th><th></th></tr></thead>
<tbody>{board_backup_rows}</tbody>
</table>
</section>

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
<div style="display:flex;gap:0.5rem;flex-wrap:wrap;align-items:center">
<form method="POST" action="/admin/db/check">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit">&#x1F50E; check integrity</button>
</form>
<form method="POST" action="/admin/vacuum">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="Run VACUUM? This will briefly block the database while it rebuilds. Continue?">&#x1F9F9; run VACUUM</button>
</form>
</div>
</section>

<!-- ═══════════════════════════════════════════════════════════════════════════
     // active onion address
     ═══════════════════════════════════════════════════════════════════════════ -->
{tor_section}
</div>

<!-- ── Backup progress modal ─────────────────────────────────────────────── -->
<div id="backup-modal" class="compress-modal" style="display:none" role="dialog" aria-modal="true" aria-labelledby="backup-modal-title">
  <div class="compress-modal-box">
    <div class="compress-modal-title" id="backup-modal-title">&#128190; Creating Backup…</div>
    <div class="compress-progress" id="backup-progress-wrap" style="display:block;margin:0.75rem 0">
      <div class="compress-progress-track"><div class="compress-progress-bar" id="backup-progress-bar" style="width:0%"></div></div>
      <div class="compress-progress-text" id="backup-progress-text">Starting…</div>
    </div>
    <div class="compress-done-actions" id="backup-done-actions" style="display:none">
      <button class="compress-cancel-btn" data-action="close-backup-modal">&#10003; Done — reload</button>
    </div>
  </div>
</div>"#,
        csrf = escape_html(csrf_token),
        flash = flash_html,
        board_cards = board_cards,
        ban_rows = ban_rows,
        filter_rows = filter_rows,
        full_backup_rows = full_backup_rows,
        board_backup_rows = board_backup_rows,
        db_size_str = format_file_size(db_size_bytes),
        db_warn_banner = db_warn_banner,
        report_rows = report_rows,
        report_badge = report_badge,
        appeal_rows = appeal_rows,
        appeal_badge = appeal_badge,
        moderation_open_attr = moderation_open_attr,
        moderation_summary_badges = moderation_summary_badges,
        global_favicon_preview = if global_favicon_exists {
            format!(
                r#"<img class="favicon-inline-preview" src="/favicon-32x32.png?v={version}" alt="global favicon">"#,
                version = escape_html(&global_favicon_version)
            )
        } else {
            String::new()
        },
        global_favicon_label = if global_favicon_exists {
            "replace favicon"
        } else {
            "global favicon"
        },
        global_favicon_button = if global_favicon_exists {
            "replace"
        } else {
            "upload"
        },
        site_name_val = escape_html(site_name),
        site_subtitle_val = escape_html(site_subtitle),
        enabled_theme_options = enabled_theme_options,
        theme_cards = theme_cards,
        global_favicon_status = if global_favicon_exists {
            "Custom global favicon is active and stored under rustchan-data/runtime/favicon/."
        } else {
            "No custom global favicon uploaded yet."
        },
        tor_section = if tor_address.is_none() {
            String::new()
        } else {
            let mut addresses = String::new();
            if let Some(addr) = tor_address {
                let _ = write!(
                    addresses,
                    r#"<p class="admin-access-address">
  <code class="onion-addr">{}</code>
</p>"#,
                    escape_html(addr)
                );
            }
            format!(
                r#"<section class="admin-section admin-access-addresses" style="border-top:1px solid var(--border);padding-top:1rem;margin-top:0;text-align:center">
<h2>// active access addresses</h2>
{addresses}
</section>"#
            )
        },
    );

    base_layout(
        "admin panel",
        None,
        &body,
        csrf_token,
        boards,
        None,
        None,
        false,
        "/admin",
    )
}

// ─── Moderation log ───────────────────────────────────────────────────────────

#[must_use]
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
        let target = e.target_id.map_or_else(
            || e.target_type.clone(),
            |id| format!("{} #{id}", e.target_type),
        );
        let board_link = if e.board_short.is_empty() {
            String::new()
        } else {
            format!(r#"<a href="/{s}">{s}</a>"#, s = escape_html(&e.board_short))
        };
        let _ = write!(
            rows,
            r#"<tr>
<td style="white-space:nowrap;font-size:0.78rem">{time}</td>
<td><strong>{admin}</strong></td>
<td><code>{action}</code></td>
<td style="font-size:0.82rem">{target}</td>
<td>{board}</td>
<td style="max-width:260px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:0.8rem"
    title="{detail}">{detail}</td>
</tr>"#,
            time = escape_html(&fmt_ts(e.created_at)),
            admin = escape_html(&e.admin_name),
            action = escape_html(&e.action),
            target = escape_html(&target),
            board = board_link,
            detail = escape_html(&e.detail)
        );
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
        None,
        None,
        false,
        "/admin/log",
    )
}

// ─── VACUUM result ────────────────────────────────────────────────────────────

#[must_use]
pub fn admin_vacuum_result_page(size_before: i64, size_after: i64, csrf_token: &str) -> String {
    let saved = size_before.saturating_sub(size_after);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
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

    base_layout(
        "VACUUM result",
        None,
        &body,
        csrf_token,
        &[],
        None,
        None,
        false,
        "/admin",
    )
}

#[must_use]
pub fn admin_db_health_result_page(
    report: &DbHealthReport,
    attempted_repair: bool,
    csrf_token: &str,
) -> String {
    let title = if attempted_repair {
        "[ database repair ]"
    } else {
        "[ database check ]"
    };
    let status_line = if attempted_repair {
        match report.after_ok {
            Some(true) => {
                r#"<p style="color:var(--green-bright)">Repair completed. The database integrity check passed afterward.</p>"#
            }
            Some(false) => {
                r#"<p class="error">Repair finished, but the database still reports a problem. Restoring a known-good full backup is recommended.</p>"#
            }
            None => {
                r#"<p class="error">Repair finished, but no final integrity result was produced.</p>"#
            }
        }
    } else if report.before_ok {
        r#"<p style="color:var(--green-bright)">The database integrity check passed.</p>"#
    } else {
        r#"<p class="error">The database integrity check found a problem.</p>"#
    };
    let repair_action = if attempted_repair {
        String::new()
    } else {
        format!(
            r#"<form method="POST" action="/admin/db/repair" style="margin-top:1rem">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="Attempt database repair? This will run integrity checks, REINDEX, and rebuild the search index. It is safe to try, but it may not fix true file corruption. Continue?">&#x1F6E0; attempt repair</button>
</form>"#,
            csrf = escape_html(csrf_token),
        )
    };

    let mut repair_summary_html = String::new();
    if report.repair_summary.is_empty() {
        repair_summary_html
            .push_str(r#"<li style="color:var(--text-dim)">No repairs were run.</li>"#);
    } else {
        for line in &report.repair_summary {
            let _ = write!(
                repair_summary_html,
                r#"<li>{line}</li>"#,
                line = escape_html(line)
            );
        }
    }

    let mut repair_steps_html = String::new();
    if report.repair_steps.is_empty() {
        repair_steps_html
            .push_str(r#"<li style="color:var(--text-dim)">No maintenance steps were run.</li>"#);
    } else {
        for step in &report.repair_steps {
            let _ = write!(
                repair_steps_html,
                r#"<li>{step}</li>"#,
                step = escape_html(step)
            );
        }
    }

    let body = format!(
        r#"<div class="admin-panel">
<h1>{title}</h1>
<section class="admin-section">
<h2>// summary</h2>
{status_line}
<div class="page-box" style="margin-top:0.75rem;max-width:760px">
<p><strong>Before:</strong> {before_status}</p>
<p><strong>Check output:</strong> <code>{before}</code></p>
<p><strong>Repair run:</strong> {repair_attempted}</p>
<p><strong>After:</strong> {after_status}</p>
<p><strong>Final output:</strong> <code>{after}</code></p>
</div>
<h2 style="margin-top:1rem">// repair outcome</h2>
<ul style="margin:0.75rem 0 0 1.25rem;max-width:760px">
{repair_summary}
</ul>
<h2 style="margin-top:1rem">// maintenance actions run</h2>
<ul style="margin:0.75rem 0 0 1.25rem;max-width:760px">
{repair_steps}
</ul>
{repair_action}
<p style="margin-top:1rem;color:var(--text-dim)">
  This tool can repair index and search-index issues, but true SQLite file corruption may still require restoring a known-good full backup.
</p>
<p style="margin-top:1rem">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        title = title,
        status_line = status_line,
        before = escape_html(&report.before_check),
        before_status = if report.before_ok {
            r#"<span style="color:var(--green-bright)">Passed</span>"#
        } else {
            r#"<span style="color:var(--red-bright)">Problem found</span>"#
        },
        repair_attempted = if report.repair_attempted { "Yes" } else { "No" },
        after = escape_html(report.after_check.as_deref().unwrap_or("not run")),
        after_status = match report.after_ok {
            Some(true) => r#"<span style="color:var(--green-bright)">Passed</span>"#,
            Some(false) => r#"<span style="color:var(--red-bright)">Problem found</span>"#,
            None => "Not run",
        },
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
        None,
        None,
        false,
        "/admin",
    )
}

// ─── IP history ───────────────────────────────────────────────────────────────

#[must_use]
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
            Some(MediaType::Other) => r#"<span style="color:var(--text-dim)">[file]</span>"#,
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
        // test character count, not byte count.  post.body.len()
        // measures UTF-8 bytes so multi-byte characters (emoji, CJK, …) can
        // cause the ellipsis to be added even when nothing was truncated, or
        // omitted even when content was.
        let body_preview = if post.body.chars().count() > 120 {
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

        let _ = write!(
            rows,
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
            del = del_form
        );
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
        None,
        None,
        false,
        &format!("/admin/ip/{ip_hash}"),
    )
}
