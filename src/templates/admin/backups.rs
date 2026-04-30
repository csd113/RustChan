use super::{escape_html, format_file_size, render_board_backup_card, AdminPanelViewModel};
use std::fmt::Write;

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let backup_warning_html = view
        .backups
        .backup_warning
        .map_or_else(String::new, |message| {
            format!(
                r#"<div class="admin-flash flash-error admin-flash-spaced">{}</div>"#,
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
        &render_auto_full_backup_tor_option(view),
        &render_full_backup_create_tor_option(view),
        &render_full_backup_restore_upload_tor_option(view),
        full_backup_open_attr,
        board_backup_open_attr,
        &board_backup_cards,
        &render_full_backup_rows(view),
        &render_board_backup_rows(view),
    )
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
fn render_full_backup_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut full_backup_rows = String::new();
    if view.backups.full_backups.is_empty() {
        full_backup_rows
            .push_str(r#"<tr><td colspan="5" class="admin-table-empty">no backups yet</td></tr>"#);
    }
    for bf in view.backups.full_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let status_html = if bf.verified {
            format!(
                r#"<span class="backup-verification-ok">{}</span>"#,
                escape_html(&bf.verification_note)
            )
        } else {
            format!(
                r#"<span class="backup-verification-error" title="{title}">verification failed</span>"#,
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
        let tor_backup_summary = if bf.contains_tor_hidden_service_keys {
            "includes Tor hidden service keys"
        } else {
            "no Tor hidden service keys"
        };
        let restore_tor_keys_option = if bf.contains_tor_hidden_service_keys
            && view.backups.tor_hidden_service_key_backup_available
        {
            r#"<label class="admin-inline-checkbox backup-tor-option backup-tor-option-compact">
        <input type="checkbox" name="restore_tor_hidden_service_keys" value="1">
        <span>
          <strong>Restore Tor keys</strong>
          <span class="admin-quick-help">Replaces the current onion identity with the one from this backup.</span>
        </span>
      </label>
      <p class="backup-extract-help backup-tor-warning">Anyone with these keys can impersonate this onion service.</p>"#
                .to_string()
        } else {
            String::new()
        };
        let restore_confirm = if bf.contains_tor_hidden_service_keys
            && view.backups.tor_hidden_service_key_backup_available
        {
            format!(
                "WARNING: Restore from {fname}? This will overwrite the live database and all uploads. If you also restore Tor keys, the current onion identity on disk will be replaced. Cannot be undone.",
                fname = bf.filename
            )
        } else {
            format!(
                "WARNING: Restore from {fname}? This will overwrite the live database and all uploads. Cannot be undone.",
                fname = bf.filename
            )
        };
        let _ = write!(
            full_backup_rows,
            r#"<tr>
<td class="backup-filename-cell">
  <div class="backup-filename">{fname}</div>
  <div class="backup-submeta">{indexed_boards_summary} · {tor_backup_summary}</div>
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
        <button type="submit" data-confirm="{restore_confirm}">&#8635; restore site</button>
        {restore_tor_keys_option}
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
            tor_backup_summary = escape_html(tor_backup_summary),
            size = size_fmt,
            modified = escape_html(&bf.modified),
            status = status_html,
            csrf = escape_html(view.csrf_token),
            restore_tor_keys_option = restore_tor_keys_option,
            restore_confirm = escape_html(&restore_confirm),
            board_picker = board_picker,
            board_help = escape_html(board_help),
        );
    }
    full_backup_rows
}

fn render_auto_full_backup_tor_option(view: &AdminPanelViewModel<'_>) -> String {
    if !view.backups.tor_hidden_service_key_backup_available {
        return String::new();
    }

    let checked = if view
        .backups
        .auto_full_backup_include_tor_hidden_service_keys
    {
        " checked"
    } else {
        ""
    };

    format!(
        r#"<label class="admin-inline-checkbox backup-tor-option">
      <input type="checkbox" name="auto_full_backup_include_tor_hidden_service_keys" value="1"{checked}>
      <span>
        <strong>Include Tor hidden service keys in automatic full backups</strong>
        <span class="admin-quick-help">Preserves the same .onion address after restore. Anyone with these keys can impersonate this onion service.</span>
      </span>
    </label>"#
    )
}

fn render_full_backup_create_tor_option(view: &AdminPanelViewModel<'_>) -> String {
    if !view.backups.tor_hidden_service_key_backup_available {
        return String::new();
    }

    r#"<label class="admin-inline-checkbox backup-tor-option">
      <input type="checkbox" name="include_tor_hidden_service_keys" value="1">
      <span>
        <strong>Include Tor hidden service keys</strong>
        <span class="admin-quick-help">Preserves the same .onion address after restore. Anyone with these keys can impersonate this onion service.</span>
      </span>
    </label>"#
        .to_string()
}

fn render_full_backup_restore_upload_tor_option(view: &AdminPanelViewModel<'_>) -> String {
    if !view.backups.tor_hidden_service_key_backup_available {
        return String::new();
    }

    r#"<label class="admin-inline-checkbox backup-tor-option">
      <input type="checkbox" name="restore_tor_hidden_service_keys" value="1">
      <span>
        <strong>Restore Tor hidden service keys</strong>
        <span class="admin-quick-help">Only applies when the uploaded backup includes Tor hidden service keys. Replaces the current onion identity with the one from the backup and restores the old .onion address.</span>
      </span>
    </label>
    <p class="backup-extract-help backup-tor-warning">Anyone with these keys can impersonate this onion service.</p>"#
        .to_string()
}

fn render_board_backup_rows(view: &AdminPanelViewModel<'_>) -> String {
    let mut board_backup_rows = String::new();
    if view.backups.board_backups.is_empty() {
        board_backup_rows.push_str(
            r#"<tr><td colspan="5" class="admin-table-empty">no board backups yet</td></tr>"#,
        );
    }
    for bf in view.backups.board_backups {
        let size_fmt = format_file_size(bf.size_bytes.cast_signed());
        let status_html = if bf.verified {
            format!(
                r#"<span class="backup-verification-ok">{}</span>"#,
                escape_html(&bf.verification_note)
            )
        } else {
            format!(
                r#"<span class="backup-verification-error" title="{title}">verification failed</span>"#,
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

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
fn render_admin_backups_section(
    csrf_token: &str,
    backup_warning_html: &str,
    backup_status_line: &str,
    auto_full_backup_interval_hours: u64,
    auto_full_backup_copies_to_keep: u64,
    auto_full_backup_tor_option: &str,
    full_backup_create_tor_option: &str,
    full_backup_restore_upload_tor_option: &str,
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
  <form method="POST" action="/admin/backup/settings" class="admin-site-settings-form full-backup-settings-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <div class="board-settings-grid admin-settings-grid">
    <label title="0 disables scheduled full backups.">
      Hours between automated backups
      <input type="number" name="auto_full_backup_interval_hours" value="{auto_full_backup_interval_hours}" min="0" max="8760">
    </label>
    <label title="When a saved full backup completes, the oldest saved full backups beyond this limit are deleted.">
      Full backups to keep
      <input type="number" name="auto_full_backup_copies_to_keep" value="{auto_full_backup_copies_to_keep}" min="1" max="1000">
    </label>
  </div>
  <div class="backup-form-options full-backup-options">
  {auto_full_backup_tor_option}
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
  <div class="full-backup-run-actions">
  <form method="POST" action="/admin/backup/create" id="full-backup-create-form" class="backup-action-form full-backup-action-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit" id="full-backup-btn">&#128190; save to server</button>
  <div class="backup-form-options full-backup-options">
  {full_backup_create_tor_option}
  </div>
  </form>
  <form method="POST" action="/admin/restore" enctype="multipart/form-data" class="backup-restore-upload-form admin-file-inline-form full-backup-action-form" data-restore-label="full backup">
  <input type="hidden" name="_csrf" value="{csrf}">
  <label class="admin-quick-field admin-file-field">Backup archive
    <input type="file" name="backup_file" accept=".zip" required class="admin-file-input">
    <span class="admin-quick-help">Upload a full-site zip backup.</span>
  </label>
  <button type="submit" class="btn-danger"
          data-confirm="WARNING: This will overwrite the database and all uploaded files. Cannot be undone. Continue?">&#8635; restore from local file</button>
  <div class="backup-form-options full-backup-options">
  {full_backup_restore_upload_tor_option}
  </div>
  </form>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// saved full backups</h3>
    <p>Download, restore, delete, or extract a single board from any saved full-site archive.</p>
  </div>
  <div class="admin-table-wrap">
  <table class="admin-table backup-table">
  <thead><tr><th>filename</th><th>size</th><th>created</th><th>status</th><th></th></tr></thead>
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
  <table class="admin-table backup-table">
  <thead><tr><th>filename</th><th>size</th><th>created</th><th>status</th><th></th></tr></thead>
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
        auto_full_backup_tor_option = auto_full_backup_tor_option,
        full_backup_create_tor_option = full_backup_create_tor_option,
        full_backup_restore_upload_tor_option = full_backup_restore_upload_tor_option,
        full_backup_open_attr = full_backup_open_attr,
        board_backup_open_attr = board_backup_open_attr,
        board_backup_cards = board_backup_cards,
        full_backup_rows = full_backup_rows,
        board_backup_rows = board_backup_rows,
    )
}
