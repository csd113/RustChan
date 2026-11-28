//! Board-directory and board-creation sections of the admin panel.

use super::{board_settings_form_id, escape_html, AdminPanelViewModel};
use crate::models::Board;

/// Renders the complete boards tab of the admin panel.
pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let boards_open_attr = if view.open_section == Some("boards")
        || view
            .open_section
            .is_some_and(|section| section.starts_with("board-"))
    {
        " open"
    } else {
        ""
    };
    let mut board_cards = String::new();
    for (index, board) in view.boards.iter().enumerate() {
        board_cards.push_str(&render_board_settings_card(
            board,
            index,
            view.boards,
            view.csrf_token,
            view.open_section,
        ));
    }

    render_admin_boards_section(view.csrf_token, &board_cards, boards_open_attr)
}

/// Renders the board directory and quick-create form.
fn render_admin_boards_section(
    csrf_token: &str,
    board_cards: &str,
    boards_open_attr: &str,
) -> String {
    format!(
        r#"<div class="admin-panel-boards" id="boards">
<section class="admin-section admin-section-collapsible">
<details class="admin-dropdown" data-admin-dropdown-key="boards"{boards_open_attr}>
<summary><span>// boards</span></summary>
<div class="admin-dropdown-content">
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
</div>
</details>
</section>
</div>"#,
        board_cards = board_cards,
        csrf = escape_html(csrf_token),
        boards_open_attr = boards_open_attr,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the board settings card preserves one cohesive form and its stable field hooks"
)]
/// Renders the general settings card for one board.
fn render_board_settings_card(
    board: &Board,
    index: usize,
    boards: &[Board],
    csrf_token: &str,
    open_section: Option<&str>,
) -> String {
    let checked = |value: bool| if value { " checked" } else { "" };
    let bytes_to_mib = |bytes: i64, fallback: usize| -> usize {
        usize::try_from(bytes)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
            / 1024
            / 1024
    };
    let prev_same_group = index
        .checked_sub(1)
        .and_then(|prev| boards.get(prev))
        .is_some_and(|prev| prev.nsfw == board.nsfw);
    let next_same_group = index
        .checked_add(1)
        .and_then(|next| boards.get(next))
        .is_some_and(|next| next.nsfw == board.nsfw);
    let any_files_toggle = if crate::config::CONFIG.enable_any_file_uploads_feature {
        format!(
            r#"<label><input type="checkbox" name="allow_any_files" value="1"{}> Allow any file uploads</label>"#,
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
<form method="POST" action="/admin/board/settings" class="board-settings-form" id="{form_id}">
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
  <p class="admin-meta-note">Archive retention keeps the most recently bumped threads, including archived sticky threads. Lowering the archive limit permanently removes overflow threads. The global Archive before prune setting can also archive live overflow when this checkbox is off.</p>
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
      <input type="password" name="access_password" maxlength="256" autocomplete="off" placeholder="{access_password_placeholder}">
      <span style="font-size:0.72rem;color:var(--text-dim)">{access_password_status}</span>
    </label>
    <label title="Minimum seconds a user must wait between posts on this board. 0 = no cooldown.">
      Post cooldown (s)<input type="number" name="post_cooldown_secs" value="{cooldown}" min="0" max="3600">
    </label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="clear_access_password" value="1"> Remove saved password</label>
    <label><input type="checkbox" name="allow_captcha" value="1"{captcha_checked}> CAPTCHA on threads and replies
      <span class="admin-quick-help">Enabling this makes posting require JavaScript on this board.</span>
    </label>
  </div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// uploads &amp; post features</h3>
    <p>Control accepted media types, per-board upload caps, poster identity tools, embeds, and editing behavior.</p>
  </div>
  <div class="board-settings-grid">
    <label title="Per-board image upload size cap in MiB.">
      Image size limit (MiB)<input type="number" name="max_image_size_mb" value="{max_image_size_mb}" min="1">
    </label>
    <label title="Per-board video upload size cap in MiB.">
      Video size limit (MiB)<input type="number" name="max_video_size_mb" value="{max_video_size_mb}" min="1">
    </label>
    <label title="Per-board audio upload size cap in MiB.">
      Audio size limit (MiB)<input type="number" name="max_audio_size_mb" value="{max_audio_size_mb}" min="1">
    </label>
    <label title="Per-board PDF upload size cap in MiB.">
      PDF size limit (MiB)<input type="number" name="max_pdf_size_mb" value="{max_pdf_size_mb}" min="1">
    </label>
  </div>
  <p class="admin-meta-note">PDF uploads use the PDF cap. Any-file uploads use the largest configured cap for this board.</p>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="allow_images" value="1"{images_checked}> Allow images</label>
    <label><input type="checkbox" name="allow_video" value="1"{video_checked}> Allow video</label>
    <label><input type="checkbox" name="allow_audio" value="1"{audio_checked}> Allow audio</label>
    <label><input type="checkbox" name="allow_pdf" value="1"{pdf_checked}> Allow PDF uploads</label>
    {any_files_toggle}
    <label><input type="checkbox" name="strict_metadata_uploads" value="1"{strict_metadata_checked}> Strict metadata-safe uploads
      <span class="admin-quick-help">New uploads must be re-encoded to metadata-free WebP. Supports JPEG, static PNG, BMP, GIF and WebP animation; rejects other files and failed conversions. Overrides video, audio, PDF and any-file permissions. Existing files and backups are unchanged. Does not remove identifying image content or guarantee anonymity.</span>
    </label>
    <label><input type="checkbox" name="allow_tripcodes" value="1"{tripcodes_checked}> Allow tripcodes</label>
    <label><input type="checkbox" name="allow_video_embeds" value="1"{video_embeds_checked}> Embed video links (YouTube)</label>
    <label><input type="checkbox" name="show_poster_ids" value="1"{poster_ids_checked}> Show thread-local poster IDs</label>
    <label title="When enabled, 3 or more consecutive greentext lines are wrapped in a collapsible block for this board. Existing posts are not affected.">
      <input type="checkbox" name="collapse_greentext" value="1"{collapse_greentext_checked}> Collapse long greentext
    </label>
    <label><input type="checkbox" name="allow_editing" value="1"{allow_editing_checked}> Allow users to edit their own posts during the 60-second grace window</label>
    <label><input type="checkbox" name="allow_self_delete" value="1"{allow_self_delete_checked}> Allow users to delete their own posts during the 60-second grace window</label>
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
        form_id = board_settings_form_id(board.id),
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
        } else if matches!(board.access_mode, crate::models::BoardAccessMode::Public) {
            "A password is saved but unused while this board is public."
        } else {
            "A password is saved. Leave blank to keep it."
        },
        cooldown = board.post_cooldown_secs,
        captcha_checked = checked(board.allow_captcha),
        strict_metadata_checked = checked(board.strict_metadata_uploads),
        images_checked = checked(board.allow_images),
        video_checked = checked(board.allow_video),
        audio_checked = checked(board.allow_audio),
        max_image_size_mb =
            bytes_to_mib(board.max_image_size, crate::config::CONFIG.max_image_size),
        max_video_size_mb =
            bytes_to_mib(board.max_video_size, crate::config::CONFIG.max_video_size),
        max_audio_size_mb =
            bytes_to_mib(board.max_audio_size, crate::config::CONFIG.max_audio_size),
        max_pdf_size_mb = bytes_to_mib(board.max_pdf_size, crate::config::CONFIG.max_image_size),
        pdf_checked = checked(board.allow_pdf),
        tripcodes_checked = checked(board.allow_tripcodes),
        video_embeds_checked = checked(board.allow_video_embeds),
        poster_ids_checked = checked(board.show_poster_ids),
        collapse_greentext_checked = checked(board.collapse_greentext),
        allow_editing_checked = checked(board.allow_editing),
        allow_self_delete_checked = checked(board.allow_self_delete),
        any_files_toggle = any_files_toggle,
        open_attr = open_attr,
    )
}
