use super::{escape_html, format_file_size, render_board_backup_card, AdminPanelViewModel};
use std::fmt::Write;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let backup_warning_html = view
        .backups
        .backup_warning
        .map_or_else(String::new, |message| {
            format!(
                r#"<div class="error" style="margin-bottom:0.75rem">{}</div>"#,
                escape_html(message)
            )
        });
    let full_backup_open_attr = if view.open_section == Some("full-backup-restore") {
        " open"
    } else {
        ""
    };
    let board_backup_open_attr = if view.open_section == Some("board-backup-restore")
        || view
            .open_section
            .is_some_and(|section| section.starts_with("board-backup-"))
    {
        " open"
    } else {
        ""
    };
    let mut board_backup_cards = String::new();
    for board in view.boards {
        board_backup_cards.push_str(&render_board_backup_card(
            board,
            view.csrf_token,
            view.open_section,
        ));
    }

    render_admin_backups_section(
        view.csrf_token,
        &backup_warning_html,
        view.backups.backup_status_line,
        view.backups.auto_full_backup_interval_hours,
        view.backups.auto_full_backup_copies_to_keep,
        full_backup_open_attr,
        board_backup_open_attr,
        &board_backup_cards,
        &render_full_backup_rows(view),
        &render_board_backup_rows(view),
    )
}

#[allow(clippy::too_many_lines)]
fn render_full_backup_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut full_backup_rows = String::new();
    if view.backups.full_backups.is_empty() {
        full_backup_rows.push_str(
            "<tr><td colspan=\"5\" style=\"color:var(--text-dim);text-align:center\">no backups yet</td></tr>",
        );
    }
    for bf in view.backups.full_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let status_html = if bf.verified {
            format!(
                r#"<span style="color:var(--green)">{}</span>"#,
                escape_html(&bf.verification_note)
            )
        } else {
            format!(
                r#"<span style="color:var(--red)" title="{title}">verification failed</span>"#,
                title = escape_html(&bf.verification_note)
            )
        };
        let mut board_options = String::new();
        for board in &bf.boards {
            let _ = write!(
                board_options,
                r#"<option value="{short}">/{short}/ — {name}</option>"#,
                short = escape_html(&board.short_name),
                name = escape_html(&board.name)
            );
        }
        let board_picker = if bf.boards.is_empty() {
            r#"<label>
        Board short name
        <input type="text" name="board_short" maxlength="8" pattern="[A-Za-z0-9]{1,8}" required placeholder="tech">
      </label>"#
                .to_string()
        } else {
            format!(
                r#"<label>
        Board
        <select name="board_short" required>
          <option value="">Select a board</option>
          {board_options}
        </select>
      </label>"#
            )
        };
        let board_help = if bf.boards.is_empty() {
            "This backup predates board indexing. Enter the board short name manually, like tech or b."
        } else {
            "Pick a board from this backup to restore it directly or download a board-only package."
        };
        let indexed_boards_summary = if bf.boards.is_empty() {
            "boards not indexed".to_string()
        } else {
            format!("{} boards indexed", bf.boards.len())
        };
        let _ = write!(
            full_backup_rows,
            r#"<tr>
<td class="backup-filename-cell">
  <div class="backup-filename">{fname}</div>
  <div class="backup-submeta">{indexed_boards_summary}</div>
</td>
<td class="backup-meta-cell">{size}</td>
<td class="backup-meta-cell">{modified}</td>
<td class="backup-status-cell">{status}</td>
<td class="backup-actions-cell">
  <div class="backup-actions-stack">
    <div class="backup-primary-actions">
      <a href="/admin/backup/download/full/{fname}" class="backup-download-link" data-backup-label="full backup">&#8659; download zip</a>
      <form method="POST" action="/admin/backup/restore-saved" class="backup-inline-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="filename" value="{fname}">
        <button type="submit" data-confirm="WARNING: Restore from {fname}? This will overwrite the live database and all uploads. Cannot be undone.">&#8635; restore site</button>
      </form>
      <form method="POST" action="/admin/backup/delete" class="backup-inline-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="kind" value="full">
        <input type="hidden" name="filename" value="{fname}">
        <button type="submit" class="btn-danger" data-confirm="Delete {fname}? This cannot be undone.">&#10005; delete</button>
      </form>
    </div>
    <details class="backup-extract-details">
      <summary>single-board tools</summary>
      <form method="POST" action="/admin/backup/extract-board" class="backup-extract-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="filename" value="{fname}">
        {board_picker}
        <p class="backup-extract-help">{board_help}</p>
        <div class="backup-extract-actions">
          <button type="submit" name="action" value="download">download board zip</button>
          <button type="submit" name="action" value="restore" class="btn-danger"
                  data-confirm="WARNING: Restore one board from {fname}? This will wipe and replace that board only. Continue?">&#8635; restore board</button>
        </div>
      </form>
    </details>
  </div>
</td>
</tr>"#,
            fname = escape_html(&bf.filename),
            indexed_boards_summary = escape_html(&indexed_boards_summary),
            size = size_fmt,
            modified = escape_html(&bf.modified),
            status = status_html,
            csrf = escape_html(view.csrf_token),
            board_picker = board_picker,
            board_help = escape_html(board_help),
        );
    }
    full_backup_rows
}

fn render_board_backup_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut board_backup_rows = String::new();
    if view.backups.board_backups.is_empty() {
        board_backup_rows.push_str(
            "<tr><td colspan=\"5\" style=\"color:var(--text-dim);text-align:center\">no board backups yet</td></tr>",
        );
    }
    for bf in view.backups.board_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let status_html = if bf.verified {
            format!(
                r#"<span style="color:var(--green)">{}</span>"#,
                escape_html(&bf.verification_note)
            )
        } else {
            format!(
                r#"<span style="color:var(--red)" title="{title}">verification failed</span>"#,
                title = escape_html(&bf.verification_note)
            )
        };
        let _ = write!(
            board_backup_rows,
            r#"<tr>
<td class="backup-filename-cell">
  <div class="backup-filename">{fname}</div>
  <div class="backup-submeta">single-board snapshot</div>
</td>
<td class="backup-meta-cell">{size}</td>
<td class="backup-meta-cell">{modified}</td>
<td class="backup-status-cell">{status}</td>
<td class="backup-actions-cell">
  <div class="backup-actions-stack">
    <div class="backup-primary-actions">
      <a href="/admin/backup/download/board/{fname}" class="backup-download-link" data-backup-label="board backup">&#8659; download zip</a>
      <form method="POST" action="/admin/board/backup/restore-saved" class="backup-inline-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="filename" value="{fname}">
        <button type="submit" data-confirm="WARNING: Restore board from {fname}? This will wipe and replace that board. Cannot be undone.">&#8635; restore board</button>
      </form>
      <form method="POST" action="/admin/backup/delete" class="backup-inline-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="kind" value="board">
        <input type="hidden" name="filename" value="{fname}">
        <button type="submit" class="btn-danger" data-confirm="Delete {fname}? This cannot be undone.">&#10005; delete</button>
      </form>
    </div>
  </div>
</td>
</tr>"#,
            fname = escape_html(&bf.filename),
            size = size_fmt,
            modified = escape_html(&bf.modified),
            status = status_html,
            csrf = escape_html(view.csrf_token)
        );
    }
    board_backup_rows
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn render_admin_backups_section(
    csrf_token: &str,
    backup_warning_html: &str,
    backup_status_line: &str,
    auto_full_backup_interval_hours: u64,
    auto_full_backup_copies_to_keep: u64,
    full_backup_open_attr: &str,
    board_backup_open_attr: &str,
    board_backup_cards: &str,
    full_backup_rows: &str,
    board_backup_rows: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-backups" id="backups">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // full site backup & restore
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section admin-section-collapsible" id="full-backup-restore">
<details class="admin-dropdown" data-admin-dropdown-key="full-backup-restore"{full_backup_open_attr}>
<summary><span>// full site backup &amp; restore</span></summary>
<div class="admin-dropdown-content">
<p class="admin-copy">Full backups include the complete database and all uploaded files. <strong>Save to server</strong> stores the backup in <code>rustchan-data/backups/full/</code> on the server filesystem (listed below). <strong>Restore from local file</strong> uploads a zip from your computer. Saved full backups can also be used to extract or directly restore a single board without scheduling separate per-board backups.</p>
{backup_warning_html}
<p class="admin-copy"><strong>Backup health:</strong> {backup_status_line}</p>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// automated full backups</h3>
    <p>Schedule background full-site snapshots and decide how many recent saved copies the server keeps.</p>
  </div>
  <form method="POST" action="/admin/backup/settings" class="admin-site-settings-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <div class="board-settings-grid admin-settings-grid">
    <label title="0 disables scheduled full backups.">
      Hours between automated backups
      <input type="number" name="auto_full_backup_interval_hours" value="{auto_full_backup_interval_hours}" min="0" max="8760" style="font-family:inherit">
    </label>
    <label title="When a saved full backup completes, the oldest saved full backups beyond this limit are deleted.">
      Full backups to keep
      <input type="number" name="auto_full_backup_copies_to_keep" value="{auto_full_backup_copies_to_keep}" min="1" max="1000" style="font-family:inherit">
    </label>
  </div>
  <div class="board-settings-actions">
    <button type="submit">save automated backup settings</button>
  </div>
  </form>
  <p class="admin-meta-note admin-meta-note-spaced">
    Set hours to <code>0</code> to disable automated full backups. Saving a full backup to server, including automated runs, trims the oldest saved full backups beyond the keep limit.
  </p>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// run or restore now</h3>
    <p>Create a full backup on the server or upload one to replace the live site.</p>
  </div>
  <div class="admin-inline-actions admin-inline-actions-spaced">
  <form method="POST" action="/admin/backup/create" id="full-backup-create-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit" id="full-backup-btn">&#128190; save to server</button>
  </form>
  <form method="POST" action="/admin/restore" enctype="multipart/form-data" class="backup-restore-upload-form admin-file-inline-form" data-restore-label="full backup">
  <input type="hidden" name="_csrf" value="{csrf}">
  <label class="admin-quick-field admin-file-field">Backup archive
    <input type="file" name="backup_file" accept=".zip" required class="admin-file-input">
    <span class="admin-quick-help">Upload a full-site zip backup.</span>
  </label>
  <button type="submit" class="btn-danger"
          data-confirm="WARNING: This will overwrite the database and all uploaded files. Cannot be undone. Continue?">&#8635; restore from local file</button>
  </form>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// saved full backups</h3>
    <p>Download, restore, delete, or extract a single board from any saved full-site archive.</p>
  </div>
  <div class="admin-table-wrap">
  <table class="admin-table backup-table" style="width:100%;border-collapse:collapse;font-size:0.85rem">
  <thead><tr style="color:var(--text-dim)"><th style="text-align:left">filename</th><th style="text-align:left">size</th><th style="text-align:left">created</th><th style="text-align:left">status</th><th></th></tr></thead>
  <tbody>{full_backup_rows}</tbody>
  </table>
  </div>
</div>
</div>
</details>
</section>

<section class="admin-section admin-section-collapsible" id="board-backup-restore">
<details class="admin-dropdown" data-admin-dropdown-key="board-backup-restore"{board_backup_open_attr}>
<summary><span>// board backup &amp; restore</span></summary>
<div class="admin-dropdown-content">
<p class="admin-copy">Board backups cover a single board. Use the per-board tools here to store a backup in <code>rustchan-data/backups/boards/</code>, or use the table below to download, restore, or delete saved backups. <strong>Restore from local file</strong> uploads a zip from your computer.</p>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// create board backups</h3>
    <p>Keep board-specific backup actions separate from routine board management.</p>
  </div>
  <div class="admin-board-cards">{board_backup_cards}</div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// restore from local file</h3>
    <p>Upload a board backup from your computer to wipe and replace exactly one board.</p>
  </div>
  <div class="admin-inline-actions admin-inline-actions-spaced">
  <form method="POST" action="/admin/board/restore" enctype="multipart/form-data" class="backup-restore-upload-form admin-file-inline-form" data-restore-label="board backup">
  <input type="hidden" name="_csrf" value="{csrf}">
  <label class="admin-quick-field admin-file-field">Board backup
    <input type="file" name="backup_file" accept=".zip,.json" required class="admin-file-input">
    <span class="admin-quick-help">Upload a board zip or raw <code>board.json</code> manifest.</span>
  </label>
  <button type="submit" class="btn-danger"
          data-confirm="WARNING: This will wipe and replace the board from the backup. Other boards are unaffected. Continue?">&#8635; restore board from local file</button>
  </form>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// saved board backups</h3>
    <p>Board-level backups are usually created from the board cards above, then downloaded, restored, or deleted here.</p>
  </div>
  <div class="admin-table-wrap">
  <table class="admin-table backup-table" style="width:100%;border-collapse:collapse;font-size:0.85rem">
  <thead><tr style="color:var(--text-dim)"><th style="text-align:left">filename</th><th style="text-align:left">size</th><th style="text-align:left">created</th><th style="text-align:left">status</th><th></th></tr></thead>
  <tbody>{board_backup_rows}</tbody>
  </table>
  </div>
</div>
</div>
</details>
</section>
</div>"#,
        csrf = escape_html(csrf_token),
        backup_warning_html = backup_warning_html,
        backup_status_line = backup_status_line,
        auto_full_backup_interval_hours = auto_full_backup_interval_hours,
        auto_full_backup_copies_to_keep = auto_full_backup_copies_to_keep,
        full_backup_open_attr = full_backup_open_attr,
        board_backup_open_attr = board_backup_open_attr,
        board_backup_cards = board_backup_cards,
        full_backup_rows = full_backup_rows,
        board_backup_rows = board_backup_rows,
    )
}
