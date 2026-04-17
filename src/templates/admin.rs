// templates/admin.rs
//
// Page templates for the admin interface:
//   admin_login_page        — login form
//   admin_panel_page        — main control panel (boards, bans, reports, …)
//   mod_log_page            — moderation history
//   admin_vacuum_result_page — post-VACUUM feedback
//   admin_ip_history_page   — posts by IP hash

use crate::db::DbHealthReport;
use crate::models::{
    BackupInfo, Ban, BannerAsset, BannerScope, BannerTargetType, Board, BoardBannerMode, WordFilter,
};
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
<table class="admin-login-table">
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

fn theme_css_starter(slug: &str, swatch_hex: &str) -> String {
    format!(
        r#"/* RustChan theme starter.
   Keep everything scoped to html[data-theme="{slug}"].
   Start with variables, then add selector overrides underneath as needed. */

html[data-theme="{slug}"] {{
  color-scheme: dark;
  --bg:           #11161a;
  --bg-panel:     rgba(20, 26, 32, 0.92);
  --bg-post:      rgba(24, 32, 40, 0.92);
  --bg-op:        rgba(28, 37, 47, 0.95);
  --bg-input:     #0d1216;
  --border:       #2d3d49;
  --border-glow:  {swatch_hex};
  --green:        {swatch_hex};
  --green-dim:    #6f8291;
  --green-bright: #d7f0ff;
  --green-pale:   #9fc5da;
  --amber:        #d5a35b;
  --red:          #d06b6b;
  --gray:         #5c6670;
  --gray-light:   #88949f;
  --text:         #dce6ee;
  --text-dim:     #90a0ad;
  --font:         'IBM Plex Sans', 'Segoe UI', sans-serif;
  --font-display: 'IBM Plex Sans', 'Segoe UI', sans-serif;
}}

html[data-theme="{slug}"] body {{
  background: var(--bg);
  background-image:
    radial-gradient(circle at top, rgba(255,255,255,0.04), transparent 32%),
    linear-gradient(180deg, rgba(255,255,255,0.02), transparent 45%);
}}

html[data-theme="{slug}"] .site-header,
html[data-theme="{slug}"] .admin-section,
html[data-theme="{slug}"] .page-box,
html[data-theme="{slug}"] .post-form-container,
html[data-theme="{slug}"] .op,
html[data-theme="{slug}"] .reply {{
  border-color: var(--border);
  background: var(--bg-panel);
}}

html[data-theme="{slug}"] a {{
  color: var(--green);
}}

html[data-theme="{slug}"] a:hover {{
  color: var(--green-bright);
}}
"#
    )
}

fn banner_target_type_options(selected: BannerTargetType) -> String {
    let options = [
        (BannerTargetType::None, "No link"),
        (BannerTargetType::InternalBoard, "Open another board"),
        (BannerTargetType::InternalPath, "Open a specific thread"),
        (BannerTargetType::ExternalUrl, "Open another website"),
    ];
    let mut out = String::new();
    for (value, label) in options {
        let _ = write!(
            out,
            r#"<option value="{value}"{selected}>{label}</option>"#,
            value = value.as_str(),
            selected = if value == selected { " selected" } else { "" },
            label = label,
        );
    }
    out
}

fn banner_preview_html(asset: &BannerAsset, alt: &str) -> String {
    format!(
        r#"<img class="board-banner-preview-image" src="{src}" alt="{alt}" width="{width}" height="{height}">"#,
        src = escape_html(&crate::banner::banner_asset_url(asset)),
        alt = escape_html(alt),
        width = crate::banner::DISPLAY_WIDTH,
        height = crate::banner::DISPLAY_HEIGHT,
    )
}

fn banner_board_options(boards: &[Board], selected_value: &str) -> String {
    let trimmed_selected = selected_value.trim().trim_matches('/');
    let mut out = String::new();
    let _ = write!(
        out,
        r#"<option value=""{}>Choose a board</option>"#,
        if trimmed_selected.is_empty() {
            " selected"
        } else {
            ""
        }
    );
    let board_exists = boards
        .iter()
        .any(|board| board.short_name == trimmed_selected);
    if !trimmed_selected.is_empty() && !board_exists {
        let _ = write!(
            out,
            r#"<option value="{value}" selected>Missing board (/{value}/)</option>"#,
            value = escape_html(trimmed_selected),
        );
    }
    for board in boards {
        let _ = write!(
            out,
            r#"<option value="{short}"{selected}>/{short}/ — {name}</option>"#,
            short = escape_html(&board.short_name),
            selected = if board.short_name == trimmed_selected {
                " selected"
            } else {
                ""
            },
            name = escape_html(&board.name),
        );
    }
    out
}

fn render_banner_target_picker(
    boards: &[Board],
    selected: BannerTargetType,
    target_value: &str,
) -> String {
    let draft = crate::banner::banner_target_draft(selected, target_value);
    format!(
        r#"<div class="admin-banner-target-picker" data-banner-target-picker>
  <label class="admin-banner-field admin-banner-field-wide admin-banner-field-select">When someone clicks the banner
    <select name="target_type" data-banner-target-select>{target_options}</select>
  </label>
  <label class="admin-banner-field admin-banner-target-field" data-banner-target-field="internal_board">Open board
    <select name="target_board_value" data-banner-target-input="internal_board">{board_options}</select>
  </label>
  <label class="admin-banner-field admin-banner-target-field" data-banner-target-field="internal_path">Open specific thread
    <input type="text" name="target_thread_value" value="{thread_value}" maxlength="512" placeholder="/tech/thread/123">
  </label>
  <label class="admin-banner-field admin-banner-target-field" data-banner-target-field="external_url">Open website
    <input type="url" name="target_external_url" value="{external_url}" maxlength="512" placeholder="https://example.com">
  </label>
</div>"#,
        target_options = banner_target_type_options(selected),
        board_options = banner_board_options(boards, &draft.board_value),
        thread_value = escape_html(&draft.thread_value),
        external_url = escape_html(&draft.external_url),
    )
}

fn render_banner_upload_form(
    action: &str,
    csrf_token: &str,
    board_id: Option<i64>,
    boards: &[Board],
    show_placements: bool,
    button_label: &str,
) -> String {
    let placement_controls = if show_placements {
        r#"<div class="admin-banner-toggle-group">
  <label class="admin-inline-checkbox"><input type="checkbox" name="enabled" value="1" checked> Enabled</label>
  <label class="admin-inline-checkbox"><input type="checkbox" name="show_on_index" value="1" checked> Show on board index</label>
  <label class="admin-inline-checkbox"><input type="checkbox" name="show_on_catalog" value="1" checked> Show on catalog</label>
</div>"#
            .to_string()
    } else {
        r#"<div class="admin-banner-toggle-group">
  <label class="admin-inline-checkbox"><input type="checkbox" name="enabled" value="1" checked> Enabled</label>
</div>"#
        .to_string()
    };
    format!(
        r#"<form method="POST" action="{action}" enctype="multipart/form-data" class="admin-banner-upload-form admin-banner-editor" data-banner-editor="1">
  <input type="hidden" name="_csrf" value="{csrf}">
  {board_id_input}
  {target_picker}
  {placement_controls}
  <label class="admin-file-field admin-banner-field-wide admin-banner-file-field">Banner image
    <input type="file" name="banner" accept="image/png,image/jpeg,image/gif,image/webp" required class="admin-file-input">
  </label>
  <div class="admin-banner-form-actions">
    <button type="submit">{button_label}</button>
  </div>
  <div class="admin-flash flash-error admin-banner-inline-warning" data-banner-warning hidden></div>
</form>"#,
        action = action,
        csrf = escape_html(csrf_token),
        board_id_input = board_id.map_or_else(String::new, |id| {
            format!(r#"<input type="hidden" name="board_id" value="{id}">"#)
        }),
        target_picker = render_banner_target_picker(boards, BannerTargetType::None, ""),
        placement_controls = placement_controls,
        button_label = escape_html(button_label),
    )
}

fn render_banner_asset_row(
    asset: &BannerAsset,
    csrf_token: &str,
    boards: &[Board],
    show_placements: bool,
) -> String {
    let placement_controls = if show_placements {
        format!(
            r#"<label class="admin-inline-checkbox"><input type="checkbox" name="show_on_index" value="1"{}> Show on board index</label>
<label class="admin-inline-checkbox"><input type="checkbox" name="show_on_catalog" value="1"{}> Show on catalog</label>"#,
            if asset.show_on_index { " checked" } else { "" },
            if asset.show_on_catalog {
                " checked"
            } else {
                ""
            },
        )
    } else {
        String::new()
    };
    let move_controls = format!(
        r#"<form method="POST" action="/admin/banner/move" class="admin-inline-actions">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="banner_id" value="{id}">
  <input type="hidden" name="direction" value="up">
  <button type="submit">up</button>
</form>
<form method="POST" action="/admin/banner/move" class="admin-inline-actions">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="banner_id" value="{id}">
  <input type="hidden" name="direction" value="down">
  <button type="submit">down</button>
</form>"#,
        csrf = escape_html(csrf_token),
        id = asset.id,
    );
    format!(
        r#"<div class="admin-banner-row">
  <div class="admin-banner-thumb">{preview}</div>
  <form method="POST" action="/admin/banner/update" class="admin-banner-meta-form admin-banner-editor" data-banner-editor="1">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="banner_id" value="{id}">
    {target_picker}
    <div class="admin-banner-toggle-group">
      <label class="admin-inline-checkbox"><input type="checkbox" name="enabled" value="1"{enabled}> Enabled</label>
      {placement_controls}
    </div>
    <div class="admin-banner-form-actions">
      <button type="submit">save banner</button>
    </div>
    <div class="admin-flash flash-error admin-banner-inline-warning" data-banner-warning hidden></div>
  </form>
  <div class="admin-inline-actions admin-inline-actions-spaced">
    {move_controls}
    <form method="POST" action="/admin/banner/delete" class="admin-inline-actions">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="banner_id" value="{id}">
      <button type="submit" class="btn-danger">delete</button>
    </form>
  </div>
</div>"#,
        preview = banner_preview_html(asset, "banner preview"),
        csrf = escape_html(csrf_token),
        id = asset.id,
        target_picker = render_banner_target_picker(boards, asset.target_type, &asset.target_value),
        enabled = if asset.enabled { " checked" } else { "" },
        placement_controls = placement_controls,
        move_controls = move_controls,
    )
}

fn render_banner_asset_list(
    assets: &[BannerAsset],
    csrf_token: &str,
    boards: &[Board],
    show_placements: bool,
    empty_message: &str,
) -> String {
    if assets.is_empty() {
        return format!(r#"<p class="admin-meta-note">{empty_message}</p>"#);
    }
    assets
        .iter()
        .map(|asset| render_banner_asset_row(asset, csrf_token, boards, show_placements))
        .collect::<String>()
}

fn render_board_favicon_controls(board: &Board, csrf_token: &str) -> String {
    let board_favicon_exists = crate::favicon::board_has_custom_favicon(&board.short_name);
    let board_favicon_version =
        crate::favicon::favicon_version_for_board(Some(&board.short_name)).unwrap_or_default();
    format!(
        r#"<div class="admin-subsection">
  <div class="admin-card-header board-card-edge-header">
    <h3>// favicon override</h3>
    <p>Give /{short}/ its own icon without changing the global site favicon.</p>
  </div>
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
  <p class="favicon-inline-status">{board_favicon_status}</p>
</div>"#,
        short = escape_html(&board.short_name),
        csrf = escape_html(csrf_token),
        id = board.id,
        board_favicon_preview = if board_favicon_exists {
            format!(
                r#"<img class="favicon-inline-preview" src="/boards/{short}/_favicon/favicon-32x32.png?v={version}" alt="/{short}/ favicon">"#,
                short = escape_html(&board.short_name),
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
                id = board.id
            )
        } else {
            String::new()
        },
    )
}

fn render_board_banner_controls(
    board: &Board,
    boards: &[Board],
    csrf_token: &str,
    board_banners: &[BannerAsset],
) -> String {
    let existing = render_banner_asset_list(
        board_banners,
        csrf_token,
        boards,
        true,
        "No board-specific banners uploaded yet.",
    );
    format!(
        r#"<div class="admin-subsection board-banner-settings-subsection">
  <div class="admin-card-header">
    <h3>// board banner settings</h3>
    <p>Add one or more banners for /{short}/. Uploading here switches this board to use its own banner set automatically.</p>
  </div>
  {upload_form}
  <p class="admin-meta-note">Exact 468x60 aspect ratio required. Minimum 468x60, recommended 936x120. Uploads are converted to WebP.</p>
  {existing}
</div>"#,
        short = escape_html(&board.short_name),
        upload_form = render_banner_upload_form(
            "/admin/board/banner",
            csrf_token,
            Some(board.id),
            boards,
            true,
            "add board banner",
        ),
        existing = existing,
    )
}

fn render_board_backup_actions(board: &Board, csrf_token: &str) -> String {
    format!(
        r#"<div class="board-card-footer-actions">
  <form method="POST" action="/admin/board/backup/create" class="board-backup-download-form" data-board="{short}">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="board_short" value="{short}">
    <input type="hidden" name="download_after_create" value="1">
    <button type="submit">&#8659; download /{short}/ backup</button>
  </form>
  <form method="POST" action="/admin/board/backup/create" class="board-backup-create-form" data-board="{short}">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="board_short" value="{short}">
    <button type="submit">&#128190; save /{short}/ backup to server</button>
  </form>
</div>"#,
        short = escape_html(&board.short_name),
        csrf = escape_html(csrf_token),
    )
}

#[allow(clippy::too_many_lines)]
fn render_board_settings_card(
    board: &Board,
    index: usize,
    boards: &[Board],
    csrf_token: &str,
    _themes: &[crate::models::Theme],
    _board_banners: &[BannerAsset],
    open_section: Option<&str>,
) -> String {
    let checked = |value: bool| if value { " checked" } else { "" };
    let prev_same_group = index
        .checked_sub(1)
        .and_then(|prev| boards.get(prev))
        .is_some_and(|prev| prev.nsfw == board.nsfw);
    let next_same_group = boards
        .get(index + 1)
        .is_some_and(|next| next.nsfw == board.nsfw);
    let any_files_toggle = if crate::config::CONFIG.enable_any_file_uploads_feature {
        format!(
            r#"<label><input type="checkbox" name="allow_any_files" value="1"{}> Allow any file downloads</label>"#,
            checked(board.allow_any_files)
        )
    } else {
        String::new()
    };
    let board_section = format!("board-{}", board.short_name);
    let open_attr = if open_section.is_some_and(|section| section == board_section) {
        " open"
    } else {
        ""
    };

    format!(
        r#"{group_gap}<details class="board-settings-card" id="board-{short}"{open_attr}>
<summary>/{short}/ — {name} {nsfw_tag}{access_tag}</summary>
<div class="board-order-toolbar">
<span>{group_label} order: {display_order}. Homepage and header follow this group ordering.</span>
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
<form method="POST" action="/admin/board/settings" class="board-settings-form" id="board-settings-form-{id}">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="board_id" value="{id}">
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// basic setup</h3>
    <p>Name, board grouping, thread limits, and archival behavior.</p>
  </div>
  <div class="board-settings-grid">
    <label>Name<input type="text" name="name" value="{name_raw}" maxlength="64" required></label>
    <label>Description<input type="text" name="description" value="{desc_raw}" maxlength="256"></label>
    <label>Bump limit<input type="number" name="bump_limit" value="{bump}" min="1" max="10000"></label>
    <label>Max threads<input type="number" name="max_threads" value="{max_threads}" min="1" max="1000"></label>
    <label>Max archived threads<input type="number" name="max_archived_threads" value="{max_archived_threads}" min="1" max="10000"></label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="nsfw" value="1"{nsfw_checked}> NSFW</label>
    <label><input type="checkbox" name="allow_archive" value="1"{archive_checked}> Archive overflow threads</label>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// access &amp; anti-spam</h3>
    <p>Choose who can read or post here, then set the board-specific friction controls.</p>
  </div>
  <div class="board-settings-grid">
    <label>Access mode
      <select name="access_mode">
        <option value="public"{access_public_selected}>Public</option>
        <option value="view_password"{access_view_selected}>Password required to view board</option>
        <option value="post_password"{access_post_selected}>Board is viewable, but posting requires a password</option>
      </select>
    </label>
    <label>Board password
      <input type="text" name="access_password" maxlength="256" autocomplete="off" placeholder="{access_password_placeholder}">
      <span style="font-size:0.72rem;color:var(--text-dim)">{access_password_status}</span>
    </label>
    <label title="Minimum seconds a user must wait between posts on this board. 0 = no cooldown.">
      Post cooldown (s)<input type="number" name="post_cooldown_secs" value="{cooldown}" min="0" max="3600">
    </label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="clear_access_password" value="1"> Remove saved password</label>
    <label><input type="checkbox" name="allow_captcha" value="1"{captcha_checked}> PoW CAPTCHA on threads and replies (hashcash, JS-solved)</label>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// uploads &amp; post features</h3>
    <p>Control accepted media types, poster identity tools, embeds, and editing behavior.</p>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="allow_images" value="1"{images_checked}> Allow images</label>
    <label><input type="checkbox" name="allow_video" value="1"{video_checked}> Allow video</label>
    <label><input type="checkbox" name="allow_audio" value="1"{audio_checked}> Allow audio</label>
    {any_files_toggle}
    <label><input type="checkbox" name="allow_tripcodes" value="1"{tripcodes_checked}> Allow tripcodes</label>
    <label><input type="checkbox" name="allow_video_embeds" value="1"{video_embeds_checked}> Embed video links (YouTube)</label>
    <label><input type="checkbox" name="show_poster_ids" value="1"{poster_ids_checked}> Show thread-local poster IDs</label>
    <label title="When enabled, 3 or more consecutive greentext lines are wrapped in a collapsible block for this board. Existing posts are not affected.">
      <input type="checkbox" name="collapse_greentext" value="1"{collapse_greentext_checked}> Collapse long greentext
    </label>
    <label><input type="checkbox" name="allow_editing" value="1"{allow_editing_checked}> Allow post editing</label>
  </div>
  <div class="board-settings-grid edit-window-row" style="margin-top:0.4rem;{edit_window_display}">
    <label title="How long (seconds) after posting a user may edit. 0 = use default (300 s).">
      Edit window (s)<input type="number" name="edit_window_secs" value="{edit_window_secs}" min="0" max="86400">
    </label>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// save board management</h3>
    <p>Appearance controls, board banners, and board backup actions now live in their own sections.</p>
  </div>
  <div class="board-settings-actions">
    <button type="submit">save settings</button>
  </div>
</div>
</form>
<div class="admin-subsection">
  <div class="admin-card-header board-card-edge-header">
    <h3>// danger zone</h3>
    <p>Permanent board deletion stays separate from routine maintenance tools.</p>
  </div>
  <div class="board-card-footer-actions">
  <form method="POST" action="/admin/board/delete">
    <input type="hidden" name="_csrf" value="{csrf}">
    <input type="hidden" name="board_id" value="{id}">
    <button type="submit" class="btn-danger"
            data-confirm="Delete /{short}/ and ALL its content?">delete board</button>
  </form>
</div>
</div>
</details>"#,
        short = escape_html(&board.short_name),
        name = escape_html(&board.name),
        nsfw_tag = if board.nsfw {
            r#"<span class="tag nsfw-tag">NSFW</span>"#
        } else {
            ""
        },
        access_tag = match board.access_mode {
            crate::models::BoardAccessMode::Public => "",
            crate::models::BoardAccessMode::ViewPassword => {
                r#" <span class="tag locked">PASSWORD</span>"#
            }
            crate::models::BoardAccessMode::PostPassword => {
                r#" <span class="tag sticky">POST PASSWORD</span>"#
            }
        },
        group_gap = if index > 0 && board.nsfw && !prev_same_group {
            "<div class=\"admin-board-group-gap\" aria-hidden=\"true\"></div>"
        } else {
            ""
        },
        group_label = if board.nsfw { "NSFW" } else { "SFW" },
        display_order = board.display_order,
        csrf = escape_html(csrf_token),
        id = board.id,
        move_up_disabled = if prev_same_group { "" } else { " disabled" },
        move_down_disabled = if next_same_group { "" } else { " disabled" },
        name_raw = escape_html(&board.name),
        desc_raw = escape_html(&board.description),
        bump = board.bump_limit,
        max_threads = board.max_threads,
        max_archived_threads = board.max_archived_threads,
        nsfw_checked = checked(board.nsfw),
        archive_checked = checked(board.allow_archive),
        access_public_selected =
            if matches!(board.access_mode, crate::models::BoardAccessMode::Public) {
                " selected"
            } else {
                ""
            },
        access_view_selected = if matches!(
            board.access_mode,
            crate::models::BoardAccessMode::ViewPassword
        ) {
            " selected"
        } else {
            ""
        },
        access_post_selected = if matches!(
            board.access_mode,
            crate::models::BoardAccessMode::PostPassword
        ) {
            " selected"
        } else {
            ""
        },
        access_password_placeholder = if board.access_password_hash.is_empty() {
            "set a board password"
        } else {
            "leave blank to keep current password"
        },
        access_password_status = if board.access_password_hash.is_empty() {
            "No board password is currently saved."
        } else {
            "A password is saved. Leave blank to keep it."
        },
        cooldown = board.post_cooldown_secs,
        captcha_checked = checked(board.allow_captcha),
        images_checked = checked(board.allow_images),
        video_checked = checked(board.allow_video),
        audio_checked = checked(board.allow_audio),
        tripcodes_checked = checked(board.allow_tripcodes),
        video_embeds_checked = checked(board.allow_video_embeds),
        poster_ids_checked = checked(board.show_poster_ids),
        collapse_greentext_checked = checked(board.collapse_greentext),
        allow_editing_checked = checked(board.allow_editing),
        any_files_toggle = any_files_toggle,
        open_attr = open_attr,
        edit_window_display = if board.allow_editing {
            ""
        } else {
            "display:none"
        },
        edit_window_secs = board.edit_window_secs,
    )
}

fn render_board_appearance_card(
    board: &Board,
    boards: &[Board],
    csrf_token: &str,
    themes: &[crate::models::Theme],
    board_banners: &[BannerAsset],
    open_section: Option<&str>,
) -> String {
    let mut board_theme_options = String::new();
    for theme in themes.iter().filter(|theme| theme.enabled) {
        let _ = write!(
            board_theme_options,
            r#"<option value="{slug}"{selected}>{label}</option>"#,
            slug = escape_html(&theme.slug),
            selected = if theme.slug == board.default_theme {
                " selected"
            } else {
                ""
            },
            label = escape_html(&theme.display_name)
        );
    }
    let appearance_section = format!("board-appearance-{}", board.short_name);
    let open_attr = if open_section.is_some_and(|section| section == appearance_section) {
        " open"
    } else {
        ""
    };
    let form_id = format!("board-settings-form-{}", board.id);
    format!(
        r#"<details class="board-settings-card" id="board-appearance-{short}"{open_attr}>
<summary>/{short}/ — {name}</summary>
<div class="admin-subsection board-appearance-settings-subsection">
  <div class="admin-card-header board-card-edge-header">
    <h3>// board appearance</h3>
    <p>Theme selection, banner mode, favicon overrides, and board-specific banners live here.</p>
  </div>
  <div class="board-settings-grid">
    <label class="board-settings-field-compact">Board default theme
      <select name="default_theme" form="{form_id}">
        <option value=""{inherit_theme_selected}>Inherit site default</option>
        {board_theme_options}
      </select>
    </label>
    <label class="board-settings-field-compact">Board banner mode
      <select name="banner_mode" form="{form_id}">
        <option value="inherit"{banner_inherit_selected}>Rotate site-wide board banners</option>
        <option value="none"{banner_none_selected}>Hide banners on this board</option>
        <option value="override"{banner_override_selected}>Use this board's own banners</option>
      </select>
    </label>
  </div>
  <div class="board-settings-actions">
    <button type="submit" form="{form_id}">save board appearance</button>
  </div>
</div>
{board_favicon_controls}
{board_banner_controls}
</details>"#,
        short = escape_html(&board.short_name),
        name = escape_html(&board.name),
        open_attr = open_attr,
        form_id = escape_html(&form_id),
        inherit_theme_selected = if board.default_theme.is_empty() {
            " selected"
        } else {
            ""
        },
        banner_inherit_selected = if matches!(board.banner_mode, BoardBannerMode::Inherit) {
            " selected"
        } else {
            ""
        },
        banner_none_selected = if matches!(board.banner_mode, BoardBannerMode::None) {
            " selected"
        } else {
            ""
        },
        banner_override_selected = if matches!(board.banner_mode, BoardBannerMode::Override) {
            " selected"
        } else {
            ""
        },
        board_theme_options = board_theme_options,
        board_favicon_controls = render_board_favicon_controls(board, csrf_token),
        board_banner_controls =
            render_board_banner_controls(board, boards, csrf_token, board_banners),
    )
}

fn render_board_backup_card(board: &Board, csrf_token: &str, open_section: Option<&str>) -> String {
    let backup_section = format!("board-backup-{}", board.short_name);
    let open_attr = if open_section.is_some_and(|section| section == backup_section) {
        " open"
    } else {
        ""
    };
    format!(
        r#"<details class="board-settings-card" id="board-backup-{short}"{open_attr}>
<summary>/{short}/ — {name}</summary>
<div class="admin-subsection">
  <div class="admin-card-header board-card-edge-header">
    <h3>// board backup tools</h3>
    <p>Create a fresh board-only package for immediate download or save one to the server for later restores.</p>
  </div>
  {board_backup_actions}
</div>
</details>"#,
        short = escape_html(&board.short_name),
        name = escape_html(&board.name),
        open_attr = open_attr,
        board_backup_actions = render_board_backup_actions(board, csrf_token),
    )
}

fn render_admin_overview_section() -> String {
    r#"<div class="admin-panel-overview" id="overview">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // live log
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="live-log">
<details class="admin-dropdown" data-admin-dropdown-key="live-log">
<summary>// live log</summary>
<div class="admin-dropdown-content">
<p class="admin-copy">
  Watching <span id="admin-live-log-file">current log</span>. Updates every 2 seconds.
</p>
<div class="admin-inline-actions admin-inline-actions-spaced">
  <button type="button" id="admin-live-log-refresh">refresh now</button>
  <button type="button" id="admin-live-log-clear">clear</button>
  <label class="admin-inline-toggle">
    <input type="checkbox" id="admin-live-log-autoscroll" checked> auto-scroll
  </label>
</div>
<pre id="admin-live-log-output" class="admin-log-output">Loading live log…</pre>
</div>
</details>
</section>
</div>"#
        .to_string()
}

fn render_admin_boards_section(csrf_token: &str, board_cards: &str) -> String {
    format!(
        r#"<div class="admin-panel-boards" id="boards">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // boards
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// boards</h2>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// board directory</h3>
  <p>Open a board to edit its settings.</p>
  </div>
  <p class="admin-order-note">Board order is shared across the homepage, top bar, and this panel. SFW and NSFW boards each keep their own order.</p>
  <div class="admin-board-cards">{board_cards}</div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// create board</h3>
    <p>Start with the short name and label, then edit the rest in its board card above.</p>
  </div>
  <form method="POST" action="/admin/board/create" class="admin-board-create-form admin-quick-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <label class="admin-quick-field">Short name
    <input type="text" name="short_name" maxlength="8" required placeholder="tech">
  </label>
  <label class="admin-quick-field">Display name
    <input type="text" name="name" maxlength="64" required placeholder="Technology">
  </label>
  <label class="admin-quick-field">Description
    <input type="text" name="description" maxlength="256" placeholder="Programming, hardware, and internet culture">
  </label>
  <label class="admin-inline-checkbox admin-quick-checkbox"><input type="checkbox" name="nsfw" value="1"> NSFW board</label>
  <label class="admin-inline-checkbox admin-quick-checkbox"><input type="checkbox" name="allow_audio" value="1"> Enable audio uploads</label>
  <button type="submit">create</button>
  </form>
</div>
</section>
</div>"#,
        board_cards = board_cards,
        csrf = escape_html(csrf_token),
    )
}

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
        <thead><tr><th>post</th><th>content preview</th><th>reason</th><th>filed</th><th>action</th></tr></thead>
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

#[allow(clippy::too_many_arguments)]
fn render_admin_site_settings_section(
    csrf_token: &str,
    site_name_val: &str,
    site_subtitle_val: &str,
    enabled_theme_options: &str,
    global_favicon_preview: &str,
    global_favicon_label: &str,
    global_favicon_button: &str,
    global_favicon_status: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-site-settings" id="site-settings-panel">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // site settings
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section" id="site-settings">
<h2>// site settings</h2>
<form method="POST" action="/admin/site/settings" class="admin-site-settings-form">
<input type="hidden" name="_csrf" value="{csrf}">
<div class="board-settings-grid admin-settings-grid">
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
<div class="board-settings-actions">
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
<p class="admin-meta-note admin-meta-note-spaced">
  {global_favicon_status}
</p>
</section>
</div>"#,
        csrf = escape_html(csrf_token),
        site_name_val = escape_html(site_name_val),
        site_subtitle_val = escape_html(site_subtitle_val),
        enabled_theme_options = enabled_theme_options,
        global_favicon_preview = global_favicon_preview,
        global_favicon_label = global_favicon_label,
        global_favicon_button = global_favicon_button,
        global_favicon_status = global_favicon_status,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_admin_appearance_section(
    csrf_token: &str,
    banner_rotation_interval_minutes: i64,
    banner_external_links_enabled_checked: &str,
    banner_settings_open_attr: &str,
    global_banner_upload_form: &str,
    global_banner_rows: &str,
    home_banner_upload_form: &str,
    home_banner_rows: &str,
    board_appearance_cards: &str,
    theme_catalog_open_attr: &str,
    builtin_theme_cards: &str,
    custom_theme_cards_or_empty: &str,
    new_theme_starter_css: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-appearance" id="appearance">
<section class="admin-section admin-section-collapsible" id="board-banners">
<details class="admin-dropdown" data-admin-dropdown-key="board-banners"{banner_settings_open_attr}>
<summary>// board banners &amp; favicons</summary>
<div class="admin-dropdown-content">
<div class="admin-subsection admin-subsection-tight">
  <div class="admin-card-header">
    <h3>// global board banner settings</h3>
    <p>Control rotation timing and whether banner clicks are allowed to leave the site.</p>
  </div>
  <form method="POST" action="/admin/site/settings" class="admin-site-settings-form admin-banner-settings-form">
    <input type="hidden" name="_csrf" value="{csrf}">
    <div class="board-settings-grid admin-settings-grid">
      <label class="board-settings-field-compact" title="0 means pick a new banner on each refresh. Values above 0 enforce timed rotation.">Rotate banners every (minutes)
        <input type="number" name="banner_rotation_interval_minutes" value="{banner_rotation_interval_minutes}" min="0" max="43200"
               style="font-family:inherit">
      </label>
      <label class="admin-inline-checkbox admin-banner-settings-toggle">
        <input type="checkbox" name="banner_external_links_enabled" value="1"{banner_external_links_enabled_checked} data-banner-external-toggle>
        Allow banners to open external websites after showing the warning page
      </label>
    </div>
    <div class="board-settings-actions">
      <button type="submit">save banner settings</button>
    </div>
  </form>
</div>

<div class="admin-subsection admin-subsection-tight" id="global-banners">
  <div class="admin-card-header">
    <h3>// global board banners</h3>
    <p>These banners rotate on board index and catalog pages unless a board uses its own banner set.</p>
  </div>
  <p class="admin-meta-note">Exact 468x60 aspect ratio required. Minimum 468x60, recommended 936x120. Uploads are converted to WebP.</p>
  {global_banner_upload_form}
  <div class="admin-banner-list">{global_banner_rows}</div>
</div>

<div class="admin-subsection admin-subsection-tight" id="home-banners">
  <div class="admin-card-header">
    <h3>// home page banner settings</h3>
    <p>Use this separate banner area for MOTD, news, or maintenance notices on the home page only.</p>
  </div>
  <p class="admin-meta-note">Exact 468x60 aspect ratio required. Minimum 468x60, recommended 936x120. Uploads are converted to WebP.</p>
  {home_banner_upload_form}
  <div class="admin-banner-list">{home_banner_rows}</div>
</div>

<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// board appearance overrides</h3>
    <p>Board-specific themes, favicon overrides, and board banner sets are managed here instead of inside the routine board cards.</p>
  </div>
  <div class="admin-board-cards">{board_appearance_cards}</div>
</div>
</div>
</details>
</section>

<section class="admin-section admin-section-collapsible" id="theme-catalog">
<details class="admin-dropdown" data-admin-dropdown-key="theme-catalog"{theme_catalog_open_attr}>
<summary><span>// themes</span></summary>
<div class="admin-dropdown-content">
<details class="admin-dropdown theme-workbench-dropdown" data-admin-dropdown-key="theme-workbench">
<summary><span>// custom theme workshop</span></summary>
<div class="admin-dropdown-content">
<div class="theme-manager-shell">
  <section class="theme-guide-card">
    <div class="admin-card-header">
      <h3>// how RustChan themes work</h3>
      <p>Every theme is just CSS scoped to <code>html[data-theme="slug"]</code>. Most of the site styling comes from shared variables first, then optional selector overrides for the pieces you want to customize.</p>
    </div>
    <div class="theme-guide-grid">
      <div class="theme-guide-block">
        <h4>Core variables</h4>
        <pre class="theme-guide-code">--bg
--bg-panel
--bg-post
--bg-op
--bg-input
--border
--border-glow
--green
--green-dim
--green-bright
--green-pale
--amber
--red
--gray
--gray-light
--text
--text-dim
--font
--font-display</pre>
      </div>
      <div class="theme-guide-block">
        <h4>Common selectors</h4>
        <pre class="theme-guide-code">body
.site-header
.admin-section
.page-box
.post-form-container
.op
.reply
a / a:hover
button / button:hover</pre>
      </div>
    </div>
    <p class="theme-guide-note">Use the starter below for new themes. Built-in theme source lives in <code>static/style.css</code> if you want examples of complete themes.</p>
  </section>

  <section class="theme-create-card">
    <div class="admin-card-header">
      <h3>// create custom theme</h3>
      <p>Start from a working scaffold instead of a blank textarea, then tune variables and add overrides where needed.</p>
    </div>
    <form method="POST" action="/admin/theme/create" class="theme-create-form">
      <input type="hidden" name="_csrf" value="{csrf}">
      <div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" maxlength="32" required placeholder="mytheme"></label>
        <label>Swatch<input type="text" name="swatch_hex" maxlength="7" placeholder="7ab84e"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" maxlength="256" placeholder="What makes this theme distinct?"></label>
      </div>
      <div class="board-settings-checks">
        <label><input type="checkbox" name="enabled" value="1" checked> Shown in theme picker</label>
      </div>
      <div class="theme-editor-css-panel">
        <div class="theme-editor-panel-header">
          <h4>Starter CSS</h4>
          <p>Replace <code>your-theme</code> in the selector with the slug above before saving.</p>
        </div>
        <textarea name="custom_css" rows="22" spellcheck="false" required>{new_theme_starter_css}</textarea>
        <p class="theme-editor-code-note">You can keep this file variable-driven and only add selector overrides where the default site structure needs extra styling.</p>
      </div>
      <div class="board-settings-actions">
        <button type="submit">create theme</button>
      </div>
    </form>
  </section>
</div>
</div>
</details>

<section class="theme-manager-group">
  <div class="theme-manager-group-header">
    <h3>// built-in themes</h3>
    <p>Toggle which shipped themes appear in the picker.</p>
  </div>
  <div class="theme-card-grid">{builtin_theme_cards}</div>
</section>

<section class="theme-manager-group">
  <div class="theme-manager-group-header">
    <h3>// custom themes</h3>
    <p>Edit your own themes with a full CSS editor and swatch metadata.</p>
  </div>
  <div class="theme-card-grid">{custom_theme_cards_or_empty}</div>
</section>
</div>
</details>
</section>
</div>"#,
        csrf = escape_html(csrf_token),
        banner_rotation_interval_minutes = banner_rotation_interval_minutes,
        banner_external_links_enabled_checked = banner_external_links_enabled_checked,
        banner_settings_open_attr = banner_settings_open_attr,
        global_banner_upload_form = global_banner_upload_form,
        global_banner_rows = global_banner_rows,
        home_banner_upload_form = home_banner_upload_form,
        home_banner_rows = home_banner_rows,
        board_appearance_cards = board_appearance_cards,
        theme_catalog_open_attr = theme_catalog_open_attr,
        builtin_theme_cards = builtin_theme_cards,
        custom_theme_cards_or_empty = custom_theme_cards_or_empty,
        new_theme_starter_css = new_theme_starter_css,
    )
}

#[allow(clippy::too_many_arguments)]
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

fn render_admin_maintenance_section(
    csrf_token: &str,
    db_warn_banner: &str,
    db_size_str: &str,
    tor_section: &str,
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
<div class="admin-inline-actions">
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
</div>"#,
        csrf = escape_html(csrf_token),
        db_warn_banner = db_warn_banner,
        db_size_str = db_size_str,
        tor_section = tor_section,
    )
}

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
    backup_status_line: &str,
    backup_warning: Option<&str>,
    reports: &[crate::models::ReportWithContext],
    appeals: &[crate::models::BanAppeal],
    site_name: &str,
    site_subtitle: &str,
    default_theme: &str,
    banner_rotation_interval_minutes: i64,
    banner_external_links_enabled: bool,
    auto_full_backup_interval_hours: u64,
    auto_full_backup_copies_to_keep: u64,
    themes: &[crate::models::Theme],
    global_banners: &[BannerAsset],
    home_banners: &[BannerAsset],
    board_banners: &[BannerAsset],
    tor_address: Option<&str>,
    // Optional one-time flash message shown at the top of the panel.
    // (is_error, message) — is_error=true → red, false → green.
    flash: Option<(bool, &str)>,
    open_section: Option<&str>,
) -> String {
    let backup_warning_html = backup_warning.map_or_else(String::new, |message| {
        format!(
            r#"<div class="error" style="margin-bottom:0.75rem">{}</div>"#,
            escape_html(message)
        )
    });
    let theme_catalog_open_attr = if open_section == Some("theme-catalog") {
        " open"
    } else {
        ""
    };
    let global_favicon_exists = crate::favicon::global_has_custom_favicon();
    let global_favicon_version =
        crate::favicon::favicon_version_for_board(None).unwrap_or_default();
    let banner_settings_open_attr = if matches!(
        open_section,
        Some("board-banners" | "global-banners" | "home-banners")
    ) || open_section
        .is_some_and(|section| section.starts_with("board-appearance-"))
    {
        " open"
    } else {
        ""
    };
    let full_backup_open_attr = if open_section == Some("full-backup-restore") {
        " open"
    } else {
        ""
    };
    let board_backup_open_attr = if open_section == Some("board-backup-restore")
        || open_section.is_some_and(|section| section.starts_with("board-backup-"))
    {
        " open"
    } else {
        ""
    };
    let mut enabled_theme_options = String::new();
    for theme in themes.iter().filter(|theme| theme.enabled) {
        let _ = write!(
            enabled_theme_options,
            r#"<option value="{slug}"{selected}>{label}</option>"#,
            slug = escape_html(&theme.slug),
            selected = if theme.slug == default_theme {
                " selected"
            } else {
                ""
            },
            label = escape_html(&theme.display_name)
        );
    }
    let mut builtin_theme_cards = String::new();
    let mut custom_theme_cards = String::new();
    let mut board_cards = String::new();
    let mut board_appearance_cards = String::new();
    let mut board_backup_cards = String::new();
    for (index, board) in boards.iter().enumerate() {
        let board_assets = board_banners
            .iter()
            .filter(|asset| asset.scope == BannerScope::Board && asset.board_id == Some(board.id))
            .cloned()
            .collect::<Vec<_>>();
        board_cards.push_str(&render_board_settings_card(
            board,
            index,
            boards,
            csrf_token,
            themes,
            &board_assets,
            open_section,
        ));
        board_appearance_cards.push_str(&render_board_appearance_card(
            board,
            boards,
            csrf_token,
            themes,
            &board_assets,
            open_section,
        ));
        board_backup_cards.push_str(&render_board_backup_card(board, csrf_token, open_section));
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
            "<tr><td colspan=\"5\" style=\"color:var(--text-dim);text-align:center\">no backups yet</td></tr>",
        );
    }
    for bf in full_backups {
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
            csrf = escape_html(csrf_token),
            board_picker = board_picker,
            board_help = escape_html(board_help),
        );
    }

    // ── Board backup file list ────────────────────────────────────────────────
    let mut board_backup_rows = String::new();
    if board_backups.is_empty() {
        board_backup_rows.push_str(
            "<tr><td colspan=\"5\" style=\"color:var(--text-dim);text-align:center\">no board backups yet</td></tr>",
        );
    }
    for bf in board_backups {
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
</td>
</tr>"#,
            board = escape_html(&rc.board_short),
            tid = rc.report.thread_id,
            pid = rc.report.post_id,
            preview = preview,
            reason = reason,
            age = escape_html(&age),
            csrf = escape_html(csrf_token),
            rid = rc.report.id
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
    let moderation_summary_counter = format!("Report inbox: [{report_count}]");

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

    if !themes.is_empty() {
        for theme in themes {
            let theme_css_value = if theme.custom_css.trim().is_empty() {
                theme_css_starter(&theme.slug, &theme.swatch_hex)
            } else {
                theme.custom_css.clone()
            };
            let theme_editor = if theme.is_builtin {
                r#"<div class="theme-editor-built-in-note">
<p>Built-in themes are maintained in <code>static/style.css</code>. You can toggle them here for the picker, but custom CSS is reserved for custom themes.</p>
</div>"#
                    .to_string()
            } else {
                format!(
                    r#"<div class="theme-editor-css-panel">
  <div class="theme-editor-panel-header">
    <h4>Custom CSS</h4>
    <p>Scope everything to <code>html[data-theme="{slug}"]</code>. This textarea accepts full CSS, not just variables.</p>
  </div>
  <textarea name="custom_css" rows="18" spellcheck="false">{custom_css}</textarea>
  <p class="theme-editor-code-note">Tip: start by changing the variables block, then add selector overrides for <code>body</code>, <code>.site-header</code>, <code>.page-box</code>, <code>.op</code>, <code>.reply</code>, and buttons if you need more personality.</p>
</div>"#,
                    slug = escape_html(&theme.slug),
                    custom_css = escape_html(&theme_css_value),
                )
            };
            let card_markup = format!(
                r#"<details class="board-settings-card theme-editor-card" id="theme-{slug}">
<summary class="theme-card-summary">
  <span class="theme-card-swatch" style="--theme-swatch:{swatch}"></span>
  <span class="theme-card-heading">
    <strong>{name}</strong>
    <span class="theme-card-meta"><code>{slug}</code>{builtin_tag}{disabled_tag}</span>
  </span>
  <span class="theme-card-description">{description}</span>
</summary>
<form method="POST" action="/admin/theme/update" class="board-settings-form theme-editor-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="existing_slug" value="{slug}">
  <div class="theme-editor-layout">
    <div class="theme-editor-basics">
      <div class="board-settings-grid">
        <label>Display name<input type="text" name="display_name" value="{name}" maxlength="64" required></label>
        <label>Slug<input type="text" name="slug" value="{slug}" maxlength="32"{slug_readonly}></label>
        <label>Swatch<input type="text" name="swatch_hex" value="{swatch}" maxlength="7"></label>
      </div>
      <div class="board-settings-grid" style="margin-top:0.65rem">
        <label>Description<input type="text" name="description" value="{description_raw}" maxlength="256"></label>
      </div>
      <div class="board-settings-checks">
        <label><input type="checkbox" name="enabled" value="1"{enabled_ck}> Enabled in theme picker</label>
      </div>
      {theme_editor}
    </div>
  </div>
  <div class="board-settings-actions">
    <button type="submit">save theme settings</button>
  </div>
</form>
{delete_form}
</details>"#,
                csrf = escape_html(csrf_token),
                name = escape_html(&theme.display_name),
                slug = escape_html(&theme.slug),
                swatch = escape_html(&theme.swatch_hex),
                builtin_tag = if theme.is_builtin {
                    r#" <span class="tag">built-in</span>"#
                } else {
                    r#" <span class="tag">custom</span>"#
                },
                disabled_tag = if theme.enabled {
                    ""
                } else {
                    r#" <span class="tag locked">disabled</span>"#
                },
                description = if theme.description.trim().is_empty() {
                    "No description yet.".to_string()
                } else {
                    escape_html(&theme.description)
                },
                description_raw = escape_html(&theme.description),
                slug_readonly = if theme.is_builtin { " readonly" } else { "" },
                enabled_ck = if theme.enabled { " checked" } else { "" },
                theme_editor = theme_editor,
                delete_form = if theme.is_builtin {
                    String::new()
                } else {
                    format!(
                        r#"<form method="POST" action="/admin/theme/delete" class="theme-editor-delete">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="slug" value="{slug}">
  <button type="submit" class="btn-danger" data-confirm="Delete custom theme {slug}?">delete theme</button>
</form>"#,
                        csrf = escape_html(csrf_token),
                        slug = escape_html(&theme.slug)
                    )
                }
            );
            if theme.is_builtin {
                builtin_theme_cards.push_str(&card_markup);
            } else {
                custom_theme_cards.push_str(&card_markup);
            }
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

    let tor_section = if tor_address.is_none() {
        String::new()
    } else {
        let mut addresses = String::new();
        if let Some(addr) = tor_address {
            let _ = write!(
                addresses,
                r#"<p class="admin-copy">Onion address: <a href="http://{addr}" target="_blank" rel="noreferrer">{addr}</a></p>"#,
                addr = escape_html(addr)
            );
        }
        addresses
    };
    let db_size_str = format_file_size(db_size_bytes);
    let global_favicon_preview = if global_favicon_exists {
        format!(
            r#"<img class="favicon-inline-preview" src="/favicon-32x32.png?v={version}" alt="global favicon">"#,
            version = escape_html(&global_favicon_version)
        )
    } else {
        String::new()
    };
    let global_favicon_label = if global_favicon_exists {
        "replace favicon"
    } else {
        "global favicon"
    };
    let global_favicon_button = if global_favicon_exists {
        "replace"
    } else {
        "upload"
    };
    let global_favicon_status = if global_favicon_exists {
        "Custom global favicon is active and stored under rustchan-data/runtime/favicon/."
    } else {
        "No custom global favicon uploaded yet."
    };
    let banner_external_links_enabled_checked = if banner_external_links_enabled {
        " checked"
    } else {
        ""
    };
    let custom_theme_cards_or_empty = if custom_theme_cards.is_empty() {
        r#"<div class="theme-empty-state">No custom themes yet. Create one above and it will show up here.</div>"#.to_string()
    } else {
        custom_theme_cards
    };
    let new_theme_starter_css = escape_html(&theme_css_starter("your-theme", "#7ab84e"));
    let global_banner_upload_form = render_banner_upload_form(
        "/admin/site/banner",
        csrf_token,
        None,
        boards,
        true,
        "upload global banner",
    );
    let home_banner_upload_form = render_banner_upload_form(
        "/admin/home/banner",
        csrf_token,
        None,
        boards,
        false,
        "upload home banner",
    );
    let global_banner_rows = render_banner_asset_list(
        global_banners,
        csrf_token,
        boards,
        true,
        "No global board banners uploaded yet.",
    );
    let home_banner_rows = render_banner_asset_list(
        home_banners,
        csrf_token,
        boards,
        false,
        "No home page banners uploaded yet.",
    );
    let overview_section = render_admin_overview_section();
    let site_settings_section = render_admin_site_settings_section(
        csrf_token,
        site_name,
        site_subtitle,
        &enabled_theme_options,
        &global_favicon_preview,
        global_favicon_label,
        global_favicon_button,
        global_favicon_status,
    );
    let boards_section = render_admin_boards_section(csrf_token, &board_cards);
    let moderation_section = render_admin_moderation_section(
        csrf_token,
        &report_rows,
        &appeal_rows,
        &ban_rows,
        &filter_rows,
        &report_badge,
        &appeal_badge,
        &ban_badge,
        &filter_badge,
        &moderation_summary_counter,
        open_section,
    );
    let appearance_section = render_admin_appearance_section(
        csrf_token,
        banner_rotation_interval_minutes,
        banner_external_links_enabled_checked,
        banner_settings_open_attr,
        &global_banner_upload_form,
        &global_banner_rows,
        &home_banner_upload_form,
        &home_banner_rows,
        &board_appearance_cards,
        theme_catalog_open_attr,
        &builtin_theme_cards,
        &custom_theme_cards_or_empty,
        &new_theme_starter_css,
    );
    let backups_section = render_admin_backups_section(
        csrf_token,
        &backup_warning_html,
        backup_status_line,
        auto_full_backup_interval_hours,
        auto_full_backup_copies_to_keep,
        full_backup_open_attr,
        board_backup_open_attr,
        &board_backup_cards,
        &full_backup_rows,
        &board_backup_rows,
    );
    let maintenance_section =
        render_admin_maintenance_section(csrf_token, &db_warn_banner, &db_size_str, &tor_section);

    let body = format!(
        r#"<div class="admin-panel">
{flash}
<div class="admin-panel-header">
  <div class="admin-panel-heading">
    <h1>[ admin panel ]</h1>
    <p class="admin-panel-lead">Manage boards, moderation, themes, backups, and site settings from one place.</p>
  </div>
  <form method="POST" action="/admin/logout" class="admin-panel-logout">
    <input type="hidden" name="_csrf" value="{csrf}">
    <button type="submit">logout</button>
  </form>
</div>

{overview_section}
{site_settings_section}
{boards_section}
{moderation_section}
{appearance_section}
{backups_section}
{maintenance_section}

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
        flash = flash_html,
        csrf = escape_html(csrf_token),
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
        "/admin/panel",
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
<div class="admin-table-wrap">
<table class="admin-table" style="width:100%;font-size:0.85rem">
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
<div class="admin-table-wrap admin-table-wrap-compact">
<table class="admin-table" style="max-width:420px">
<tbody>
  <tr><td>Before</td><td><strong>{before}</strong></td></tr>
  <tr><td>After</td><td><strong>{after}</strong></td></tr>
  <tr><td>Reclaimed</td><td><strong style="color:var(--green-bright)">{saved}</strong> ({pct}%)</td></tr>
</tbody>
</table>
</div>
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

#[allow(clippy::too_many_lines)]
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
                r"<li>{line}</li>",
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
                r"<li>{step}</li>",
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

#[allow(clippy::too_many_lines)]
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
<div class="admin-table-wrap">
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
</div>
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

#[cfg(test)]
mod tests {
    use super::{admin_panel_page, render_board_settings_card};
    use crate::models::{
        BackupBoardSummary, BackupInfo, Board, BoardAccessMode, BoardBannerMode, Report,
        ReportWithContext, Theme,
    };

    fn sample_board() -> Board {
        Board {
            id: 7,
            display_order: 2,
            short_name: "tech".into(),
            name: "Technology".into(),
            description: "Computers and code".into(),
            nsfw: false,
            max_threads: 120,
            max_archived_threads: 240,
            bump_limit: 300,
            allow_images: true,
            allow_video: true,
            allow_audio: true,
            allow_any_files: false,
            allow_tripcodes: true,
            allow_editing: true,
            edit_window_secs: 900,
            allow_archive: true,
            allow_video_embeds: true,
            allow_captcha: true,
            show_poster_ids: true,
            collapse_greentext: true,
            post_cooldown_secs: 15,
            default_theme: "terminal".into(),
            banner_mode: BoardBannerMode::Inherit,
            access_mode: BoardAccessMode::PostPassword,
            access_password_hash: "hashed".into(),
            created_at: 0,
        }
    }

    fn sample_theme() -> Theme {
        Theme {
            slug: "terminal".into(),
            display_name: "Terminal".into(),
            description: "Classic green glow".into(),
            swatch_hex: "#7ab84e".into(),
            enabled: true,
            sort_order: 1,
            is_builtin: true,
            custom_css: String::new(),
        }
    }

    fn sample_full_backup() -> BackupInfo {
        BackupInfo {
            filename: "full-2026-04-07.zip".into(),
            size_bytes: 2048,
            modified: "2026-04-07 10:15 UTC".into(),
            modified_epoch: Some(1_775_555_700),
            verified: true,
            verification_note: "verified".into(),
            boards: vec![BackupBoardSummary {
                short_name: "tech".into(),
                name: "Technology".into(),
            }],
        }
    }

    fn sample_board_backup() -> BackupInfo {
        BackupInfo {
            filename: "tech-2026-04-07.zip".into(),
            size_bytes: 1024,
            modified: "2026-04-07 11:00 UTC".into(),
            modified_epoch: Some(1_775_558_400),
            verified: true,
            verification_note: "verified".into(),
            boards: Vec::new(),
        }
    }

    fn sample_report() -> ReportWithContext {
        ReportWithContext {
            report: Report {
                id: 11,
                post_id: 42,
                thread_id: 9,
                board_id: 7,
                reason: "spam".into(),
                reporter_hash: "reporter".into(),
                status: "open".into(),
                created_at: 1_775_560_000,
                resolved_at: None,
                resolved_by: None,
            },
            board_short: "tech".into(),
            post_preview: "Buy my thing".into(),
            post_ip_hash: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            ),
        }
    }

    #[test]
    fn board_settings_card_separates_board_management_tasks() {
        let board = sample_board();
        let html = render_board_settings_card(
            &board,
            0,
            std::slice::from_ref(&board),
            "csrf",
            &[sample_theme()],
            &[],
            None,
        );

        assert!(html.contains("// basic setup"));
        assert!(html.contains("// access &amp; anti-spam"));
        assert!(html.contains("// uploads &amp; post features"));
        assert!(html.contains("// save board management"));
        assert!(!html.contains("// appearance"));
        assert!(!html.contains("// board backup tools"));
        assert!(html.contains("// danger zone"));
        assert!(!html.contains("class=\"board-backup-download-form\""));
        assert!(html.contains("action=\"/admin/board/delete\""));
    }

    #[test]
    fn admin_panel_groups_board_and_backup_areas_by_task() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = admin_panel_page(
            std::slice::from_ref(&board),
            &[],
            &[],
            "csrf",
            &[sample_full_backup()],
            &[sample_board_backup()],
            4096,
            false,
            "All saved backups verified.",
            None,
            &[],
            &[],
            "RustChan",
            "select board to proceed",
            "terminal",
            0,
            false,
            24,
            7,
            &themes,
            &[],
            &[],
            &[],
            None,
            None,
            None,
        );

        let overview = html
            .find(r#"class="admin-panel-overview" id="overview""#)
            .expect("overview section");
        let site_settings = html
            .find(r#"class="admin-panel-site-settings" id="site-settings-panel""#)
            .expect("site settings section");
        let boards = html
            .find(r#"class="admin-panel-boards" id="boards""#)
            .expect("boards section");
        let moderation = html
            .find(r#"class="admin-panel-moderation" id="moderation""#)
            .expect("moderation section");
        let appearance = html
            .find(r#"class="admin-panel-appearance" id="appearance""#)
            .expect("appearance section");
        let backups = html
            .find(r#"class="admin-panel-backups" id="backups""#)
            .expect("backups section");
        let maintenance = html
            .find(r#"class="admin-panel-maintenance" id="maintenance""#)
            .expect("maintenance section");

        assert!(overview < site_settings);
        assert!(site_settings < boards);
        assert!(boards < moderation);
        assert!(moderation < appearance);
        assert!(appearance < backups);
        assert!(backups < maintenance);
        assert!(html.contains("<h2>// site settings</h2>"));
        assert!(html.contains("// board directory"));
        assert!(html.contains("// create board"));
        assert!(html.contains(r#"name="allow_audio" value="1"> Enable audio uploads"#));
        assert!(html.contains("// board appearance overrides"));
        assert!(html.contains("id=\"board-appearance-tech\""));
        assert!(html.contains("save board appearance"));
        assert!(html.contains("id=\"board-backup-tech\""));
        assert!(html.contains("// create board backups"));
        assert!(html.contains("// automated full backups"));
        assert!(html.contains("// run or restore now"));
        assert!(html.contains("// saved full backups"));
        assert!(html.contains("data-admin-dropdown-key=\"full-backup-restore\""));
        assert!(html.contains("single-board tools"));
        assert!(html.contains("// restore from local file"));
        assert!(html.contains("// saved board backups"));
        assert!(html.contains("data-admin-dropdown-key=\"board-backup-restore\""));
        assert!(html.contains("single-board snapshot"));
    }

    #[test]
    fn admin_panel_reports_only_render_resolve_action() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let report = sample_report();
        let html = admin_panel_page(
            std::slice::from_ref(&board),
            &[],
            &[],
            "csrf",
            &[sample_full_backup()],
            &[sample_board_backup()],
            4096,
            false,
            "All saved backups verified.",
            None,
            std::slice::from_ref(&report),
            &[],
            "RustChan",
            "select board to proceed",
            "terminal",
            0,
            false,
            24,
            7,
            &themes,
            &[],
            &[],
            &[],
            None,
            None,
            None,
        );

        assert!(html.contains("action=\"/admin/report/resolve\""));
        assert!(html.contains("&#10003; resolve</button>"));
        assert!(!html.contains("resolve + ban"));
        assert!(!html.contains("ban_ip_hash"));
    }

    #[test]
    fn admin_panel_reports_section_honors_open_target() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let report = sample_report();
        let html = admin_panel_page(
            std::slice::from_ref(&board),
            &[],
            &[],
            "csrf",
            &[sample_full_backup()],
            &[sample_board_backup()],
            4096,
            false,
            "All saved backups verified.",
            None,
            std::slice::from_ref(&report),
            &[],
            "RustChan",
            "select board to proceed",
            "terminal",
            0,
            false,
            24,
            7,
            &themes,
            &[],
            &[],
            &[],
            None,
            None,
            Some("reports"),
        );

        assert!(html.contains(
            r#"<details class="admin-dropdown" data-admin-dropdown-key="reports" open>"#
        ));
    }
}
