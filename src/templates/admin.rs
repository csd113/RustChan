// Page templates for the admin interface:
//   admin_login_page        — login form
//   admin_panel_page        — main control panel (boards, bans, reports, …)
//   mod_log_page            — moderation history
//   admin_vacuum_result_page — post-VACUUM feedback
//   admin_ip_history_page   — posts by IP hash

use crate::db::DbHealthReport;
use crate::models::{
    BackupInfo, Ban, BannerAsset, BannerTargetType, Board, BoardBannerMode, WordFilter,
};
use crate::utils::{files::format_file_size, sanitize::escape_html};
use std::collections::BTreeSet;
use std::fmt::Write;

use super::{base_layout, fmt_ts, fmt_ts_short, render_pagination, urlencoding_simple};

// ─── Admin login ──────────────────────────────────────────────────────────────

#[must_use]
pub fn admin_login_page(
    error: Option<&str>,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
) -> String {
    let err_html = error
        .map(|e| {
            format!(
                r#"<div class="error admin-login-error" role="alert">{}</div>"#,
                escape_html(e)
            )
        })
        .unwrap_or_default();

    let body = format!(
        r#"<div class="page-box admin-login">
<div class="admin-login-header">
  <h1>Admin Login</h1>
  <p>Sign in to manage boards, moderation, backups, and site settings.</p>
</div>
{err}
<form method="POST" action="/admin/login" class="admin-login-form">
<input type="hidden" name="_csrf" value="{csrf}">
<label class="admin-login-field">Username
  <input type="text" name="username" autofocus required autocomplete="username">
</label>
<label class="admin-login-field">Password
  <input type="password" name="password" required autocomplete="current-password">
</label>
<div class="admin-login-actions">
  <button type="submit">authenticate</button>
</div>
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
        current_theme,
        None,
        false,
        "/admin",
    )
}

// ─── Admin panel ──────────────────────────────────────────────────────────────

mod appearance;
mod backups;
mod boards;
mod layout;
mod maintenance;
mod moderation;

pub struct AdminPanelViewModel<'a> {
    pub csrf_token: &'a str,
    pub boards: &'a [Board],
    pub current_theme: Option<&'a str>,
    pub moderation: AdminPanelModerationView<'a>,
    pub appearance: AdminPanelAppearanceView<'a>,
    pub backups: AdminPanelBackupsView<'a>,
    pub maintenance: AdminPanelMaintenanceView,
    pub tor_address: Option<&'a str>,
    pub flash: Option<AdminPanelFlash<'a>>,
    pub open_section: Option<&'a str>,
}

pub struct AdminPanelModerationView<'a> {
    pub bans: &'a [Ban],
    pub filters: &'a [WordFilter],
    pub reports: &'a [crate::models::ReportWithContext],
    pub appeals: &'a [crate::models::BanAppeal],
}

#[allow(clippy::struct_excessive_bools)]
pub struct AdminPanelAppearanceView<'a> {
    pub site_name: &'a str,
    pub site_subtitle: &'a str,
    pub homepage_new_thread_badges_enabled: bool,
    pub homepage_new_reply_badges_enabled: bool,
    pub thread_new_reply_badges_enabled: bool,
    pub default_theme: &'a str,
    pub banner_rotation_interval_minutes: i64,
    pub banner_external_links_enabled: bool,
    pub themes: &'a [crate::models::Theme],
    pub global_banners: &'a [BannerAsset],
    pub home_banners: &'a [BannerAsset],
    pub board_banners: &'a [BannerAsset],
}

pub struct AdminPanelBackupsView<'a> {
    pub full_backups: &'a [BackupInfo],
    pub board_backups: &'a [BackupInfo],
    pub backup_status_line: &'a str,
    pub backup_warning: Option<&'a str>,
    pub auto_full_backup_interval_hours: u64,
    pub auto_full_backup_copies_to_keep: u64,
    pub auto_full_backup_include_tor_hidden_service_keys: bool,
    pub auto_full_backup_storage_mode: &'a str,
    pub auto_full_backup_split_zip_part_size_gib: u64,
    pub tor_hidden_service_key_backup_available: bool,
}

pub struct AdminPanelMaintenanceView {
    pub db_size_bytes: i64,
    pub db_size_warning: bool,
    pub ffmpeg_timeout_secs: u64,
    pub media_auto_prune_enabled: bool,
    pub media_max_active_content_size_bytes: u64,
    pub media_detection: AdminMediaDetectionView,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum AdminDetectionStatus {
    Detected,
    Missing,
}

impl AdminDetectionStatus {
    #[must_use]
    pub const fn is_detected(self) -> bool {
        matches!(self, Self::Detected)
    }
}

pub struct AdminMediaDetectionView {
    pub ffmpeg: AdminDetectionStatus,
    pub ffprobe: AdminDetectionStatus,
    pub webp_encoder: AdminDetectionStatus,
    pub vp9_pipeline: AdminDetectionStatus,
    pub pdf_thumbnail_renderer: Option<String>,
}

#[derive(Clone, Copy)]
pub struct AdminPanelFlash<'a> {
    pub is_error: bool,
    pub message: &'a str,
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

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
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
    let next_same_group = boards
        .get(index + 1)
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
      <input type="password" name="access_password" maxlength="256" autocomplete="off" placeholder="{access_password_placeholder}">
      <span style="font-size:0.72rem;color:var(--text-dim)">{access_password_status}</span>
    </label>
    <label title="Minimum seconds a user must wait between posts on this board. 0 = no cooldown.">
      Post cooldown (s)<input type="number" name="post_cooldown_secs" value="{cooldown}" min="0" max="3600">
    </label>
  </div>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="clear_access_password" value="1"> Remove saved password</label>
    <label><input type="checkbox" name="allow_captcha" value="1"{captcha_checked}> PoW CAPTCHA on threads and replies (hashcash, JS-solved)
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
  </div>
  <p class="admin-meta-note">PDF and any-file uploads still use the largest enabled cap for this board.</p>
  <div class="board-settings-checks">
    <label><input type="checkbox" name="allow_images" value="1"{images_checked}> Allow images</label>
    <label><input type="checkbox" name="allow_video" value="1"{video_checked}> Allow video</label>
    <label><input type="checkbox" name="allow_audio" value="1"{audio_checked}> Allow audio</label>
    <label><input type="checkbox" name="allow_pdf" value="1"{pdf_checked}> Allow PDF uploads</label>
    {any_files_toggle}
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
        images_checked = checked(board.allow_images),
        video_checked = checked(board.allow_video),
        audio_checked = checked(board.allow_audio),
        max_image_size_mb =
            bytes_to_mib(board.max_image_size, crate::config::CONFIG.max_image_size),
        max_video_size_mb =
            bytes_to_mib(board.max_video_size, crate::config::CONFIG.max_video_size),
        max_audio_size_mb =
            bytes_to_mib(board.max_audio_size, crate::config::CONFIG.max_audio_size),
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
<summary>/{short}/ — {name} {nsfw_tag}</summary>
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
        nsfw_tag = if board.nsfw {
            r#"<span class="tag nsfw-tag">NSFW</span>"#
        } else {
            ""
        },
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

#[must_use]
pub fn admin_panel_page(view: &AdminPanelViewModel<'_>) -> String {
    layout::render(view)
}

// ─── Moderation log ───────────────────────────────────────────────────────────

#[must_use]
pub fn mod_log_page(
    entries: &[crate::models::ModLogEntry],
    pagination: &crate::models::Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
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
        current_theme,
        None,
        false,
        "/admin/log",
    )
}

// ─── VACUUM result ────────────────────────────────────────────────────────────

#[must_use]
pub fn admin_vacuum_result_page(
    size_before: i64,
    size_after: i64,
    csrf_token: &str,
    current_theme: Option<&str>,
) -> String {
    let saved = size_before.saturating_sub(size_after);
    // This cast is a local display or math conversion, and the values are already bounded by surrounding invariants.
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
<table class="admin-table admin-result-table">
<tbody>
  <tr><td>Before</td><td><strong>{before}</strong></td></tr>
  <tr><td>After</td><td><strong>{after}</strong></td></tr>
  <tr><td>Reclaimed</td><td><strong class="admin-status-ok">{saved}</strong> ({pct}%)</td></tr>
</tbody>
</table>
</div>
<p class="admin-result-actions">
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
        current_theme,
        None,
        false,
        "/admin",
    )
}

// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn admin_db_health_result_page(
    report: &DbHealthReport,
    attempted_repair: bool,
    csrf_token: &str,
    repair_job_id: Option<u64>,
    current_theme: Option<&str>,
) -> String {
    let title = if attempted_repair {
        "[ database repair ]"
    } else {
        "[ database check ]"
    };
    let status_line = if attempted_repair {
        if report.repair_backup_error.is_some() {
            r#"<p class="error">Repair was not run because the pre-repair backup failed.</p>"#
        } else {
            match report.after.as_ref().map(crate::db::DbHealthSnapshot::ok) {
                Some(true) => {
                    r#"<p class="admin-result-status admin-status-ok">Maintenance completed. Database health checks passed afterward.</p>"#
                }
                Some(false) => {
                    r#"<p class="error">Repair finished, but the database still reports a problem. Restoring a known-good full backup is recommended.</p>"#
                }
                None => {
                    r#"<p class="error">Repair finished, but no final health result was produced.</p>"#
                }
            }
        }
    } else if report.before.ok() {
        r#"<p class="admin-result-status admin-status-ok">Database health checks passed.</p>"#
    } else {
        r#"<p class="error">Database health checks found a problem.</p>"#
    };
    let repair_action = if attempted_repair {
        String::new()
    } else {
        let (label, confirm) = if report.before.ok() {
            (
                "&#x1F6E0; run maintenance rebuild",
                "Run maintenance rebuild? This will create a Backup v4 DB + config pre-maintenance backup, then run REINDEX, rebuild the search index, recreate its triggers, and optimize SQLite statistics. Continue?",
            )
        } else {
            (
                "&#x1F6E0; attempt repair",
                "Attempt database repair? This will create a Backup v4 DB + config pre-maintenance backup, then run integrity checks, REINDEX, and rebuild the search index. It may not fix true file corruption. Continue?",
            )
        };
        format!(
            r#"<form method="POST" action="/admin/db/repair" class="admin-result-action-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <button type="submit"
          data-confirm="{confirm}">{label}</button>
</form>"#,
            csrf = escape_html(csrf_token),
            confirm = escape_html(confirm),
            label = label,
        )
    };

    let mut repair_summary_html = String::new();
    if report.repair_summary.is_empty() {
        repair_summary_html
            .push_str(r#"<li class="admin-muted-list-item">No repairs were run.</li>"#);
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
            .push_str(r#"<li class="admin-muted-list-item">No maintenance steps were run.</li>"#);
    } else {
        for step in &report.repair_steps {
            let _ = write!(
                repair_steps_html,
                r"<li>{step}</li>",
                step = escape_html(step)
            );
        }
    }

    let backup_html = report.repair_backup.as_ref().map_or_else(
        || {
            report.repair_backup_error.as_ref().map_or_else(
                || r"<p><strong>Pre-repair backup:</strong> Not run</p>".to_string(),
                |error| {
                    format!(
                        r#"<p><strong>Pre-repair backup:</strong> <span class="admin-status-error">Failed</span> <code>{}</code></p>"#,
                        escape_html(error)
                    )
                },
            )
        },
        |backup| {
            format!(
                r"<p><strong>Pre-repair backup:</strong> <code>{}</code></p>
<p><strong>Pre-repair backup type:</strong> {}</p>
<p><strong>Verification status:</strong> {}</p>
<p><strong>Backup path:</strong> <code>{}</code></p>",
                escape_html(&backup.backup_id),
                escape_html(&backup.backup_type),
                if backup.verified { "Verified" } else { "Unverified" },
                escape_html(&backup.backup_path)
            )
        },
    );
    let before_checks_html = render_db_health_snapshot(&report.before);
    let after_checks_html = report.after.as_ref().map_or_else(
        || r"<p><strong>After:</strong> Not run</p>".to_string(),
        render_db_health_snapshot,
    );

    let body = format!(
        r#"<div class="admin-panel">
<h1>{title}</h1>
<section class="admin-section">
<h2>// summary</h2>
{status_line}
<div class="admin-result-card">
<p><strong>Before:</strong> {before_status}</p>
{before_checks}
<p><strong>Repair run:</strong> {repair_attempted}</p>
{repair_job_id_html}
{backup}
<p><strong>After:</strong> {after_status}</p>
{after_checks}
</div>
<h2 class="admin-result-heading">// repair outcome</h2>
<ul class="admin-result-list">
{repair_summary}
</ul>
<h2 class="admin-result-heading">// maintenance actions run</h2>
<ul class="admin-result-list">
{repair_steps}
</ul>
{repair_action}
<p class="admin-result-note">
  Run checks after restores or large deletes. Take a backup before repair; this repair flow now creates a Backup v4 DB + config pre-maintenance snapshot before making changes.
  This tool can repair index and search-index issues, but true SQLite file corruption may still require restoring a known-good backup.
</p>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        title = title,
        status_line = status_line,
        before_status = if report.before.ok() {
            r#"<span class="admin-status-ok">Passed</span>"#
        } else {
            r#"<span class="admin-status-error">Problem found</span>"#
        },
        before_checks = before_checks_html,
        repair_attempted = if report.repair_attempted { "Yes" } else { "No" },
        repair_job_id_html = repair_job_id.map_or_else(String::new, |job_id| {
            format!(r"<p><strong>Run id:</strong> <code>{job_id}</code></p>")
        }),
        backup = backup_html,
        after_status = match report.after.as_ref().map(crate::db::DbHealthSnapshot::ok) {
            Some(true) => r#"<span class="admin-status-ok">Passed</span>"#,
            Some(false) => r#"<span class="admin-status-error">Problem found</span>"#,
            None => "Not run",
        },
        after_checks = after_checks_html,
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
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
pub fn admin_db_repair_idle_page(csrf_token: &str, current_theme: Option<&str>) -> String {
    let body = r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild</h2>
<div class="admin-result-card">
<p>No maintenance rebuild is running.</p>
<p class="admin-meta-note">Start a new maintenance rebuild from the admin panel when you need to create a backup and rebuild indexes.</p>
</div>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#
        .to_string();

    base_layout(
        "Database repair",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
pub fn admin_db_repair_running_page(
    csrf_token: &str,
    job_id: u64,
    started_at: i64,
    current_theme: Option<&str>,
) -> String {
    let progress_url = format!("/admin/db/repair/progress?job_id={job_id}");
    let status_url = format!("/admin/db/repair/status?job_id={job_id}");
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild running</h2>
<div class="admin-result-card">
<p>Maintenance rebuild started at <code>{started_at}</code>.</p>
<div class="compress-progress admin-progress-spaced" data-db-repair-progress data-db-repair-job-id="{job_id}" data-db-repair-progress-url="{progress_url}">
  <div class="compress-progress-track"><div class="compress-progress-bar admin-progress-bar-start" data-db-repair-progress-bar></div></div>
  <div class="compress-progress-text" data-db-repair-progress-text>Starting maintenance rebuild...</div>
</div>
<p class="admin-meta-note">This page updates live while the backup and database rebuild finish.</p>
</div>
<p class="admin-result-actions">
  <a href="{status_url}">refresh status</a> · <a href="/admin/panel">back to admin panel</a>
</p>
</section>
</div>"#
    );

    base_layout(
        "Database repair running",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
pub fn admin_db_repair_stale_page(
    csrf_token: &str,
    requested_job_id: u64,
    current_job_id: Option<u64>,
    current_theme: Option<&str>,
) -> String {
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild status</h2>
<div class="admin-result-card">
<p class="error">This page is for maintenance rebuild <code>{requested_job_id}</code>, but that run is no longer the current status.</p>
{current_job_html}
</div>
<p class="admin-result-actions">
  <a href="/admin/db/repair/status">current status</a> · <a href="/admin/panel">back to admin panel</a>
</p>
</section>
</div>"#,
        current_job_html = current_job_id.map_or_else(
            || "<p>No maintenance rebuild is currently active.</p>".to_string(),
            |job_id| {
                format!(
                    r#"<p>The current maintenance rebuild is <code>{job_id}</code>. <a href="/admin/db/repair/status?job_id={job_id}">Open that status page.</a></p>"#
                )
            }
        ),
    );

    base_layout(
        "Database repair status",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

#[must_use]
pub fn admin_db_repair_failed_page(
    csrf_token: &str,
    message: &str,
    finished_at: i64,
    job_id: u64,
    current_theme: Option<&str>,
) -> String {
    let body = format!(
        r#"<div class="admin-panel">
<h1>[ database repair ]</h1>
<section class="admin-section">
<h2>// maintenance rebuild failed</h2>
<div class="admin-result-card">
<p class="error">The background maintenance rebuild failed.</p>
<p><strong>Run id:</strong> <code>{job_id}</code></p>
<p><strong>Finished:</strong> <code>{finished_at}</code></p>
<p><strong>Error:</strong> <code>{message}</code></p>
</div>
<p class="admin-result-actions">
  <a href="/admin/panel">&#8592; back to admin panel</a>
</p>
</section>
</div>"#,
        job_id = job_id,
        message = escape_html(message),
    );

    base_layout(
        "Database repair failed",
        None,
        &body,
        csrf_token,
        &[],
        current_theme,
        None,
        false,
        "/admin",
    )
}

fn render_db_health_snapshot(snapshot: &crate::db::DbHealthSnapshot) -> String {
    format!(
        "{integrity}{foreign_keys}",
        integrity = render_db_check_result("integrity check", &snapshot.integrity),
        foreign_keys = render_db_check_result("foreign key check", &snapshot.foreign_keys),
    )
}

fn render_db_check_result(label: &str, result: &crate::db::DbCheckResult) -> String {
    let status = if result.ok {
        r#"<span class="admin-status-ok">Passed</span>"#
    } else {
        r#"<span class="admin-status-error">Problem found</span>"#
    };
    let output = result.output();
    if result.ok || result.messages.len() <= 1 {
        return format!(
            r"<p><strong>{label}:</strong> {status} <code>{output}</code></p>",
            label = label,
            status = status,
            output = escape_html(&output),
        );
    }

    format!(
        r#"<p><strong>{label}:</strong> {status}. Found {count} issues.</p>
<details class="admin-result-details">
  <summary>show full {label} output</summary>
  <pre>{output}</pre>
</details>"#,
        label = label,
        status = status,
        count = result.messages.len(),
        output = escape_html(&result.messages.join("\n")),
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
            seen_names.insert(name.to_string());
        }

        if let Some(tripcode) = post
            .tripcode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            seen_tripcodes.insert(tripcode.to_string());
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
        let report_form = post.ip_hash.as_deref().map_or_else(String::new, |ip_hash| {
            format!(
                r#"<button type="button" class="admin-toolbar-btn" data-action="open-report"
        data-pid="{pid}" data-tid="{tid}" data-board="{board}" data-csrf="{csrf}"
        data-report-action="/admin/ip/report" data-report-ip-hash="{ip_hash}"
        data-report-title="Report Hashed IP Post"
        data-report-submit-label="Submit Admin Report"
        data-report-reason-required="1"
        data-report-label="Report post No.{pid} for hashed IP {ip_hash}">report</button>"#,
                csrf = escape_html(csrf_token),
                pid = post.id,
                tid = post.thread_id,
                board = escape_html(board_short),
                ip_hash = escape_html(ip_hash),
            )
        });

        let _ = write!(
            rows,
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
        let _ = write!(pag_base, "{sep}return_to={}", urlencoding_simple(return_to));
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
<h2>// posts by Hashed IP <code style="font-size:0.9rem">{hash_display}</code></h2>
<p style="color:var(--text-dim);font-size:0.85rem">
  {total} post{plural} found across all boards.
</p>
<p style="color:var(--text-dim);font-size:0.82rem">{identity_summary}</p>
<p style="margin:0.35rem 0 1rem 0">{return_buttons}</p>
<div class="admin-table-wrap">
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
        &format!("Hashed IP — {}", &ip_hash[..ip_hash.len().min(12)]),
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

#[cfg(test)]
mod tests {
    use super::{
        admin_db_health_result_page, admin_db_repair_idle_page, admin_login_page, admin_panel_page,
        render_board_appearance_card, render_board_settings_card, AdminDetectionStatus,
        AdminMediaDetectionView, AdminPanelAppearanceView, AdminPanelBackupsView,
        AdminPanelMaintenanceView, AdminPanelModerationView, AdminPanelViewModel,
    };
    use crate::db::{DbCheckResult, DbHealthReport, DbHealthSnapshot};
    use crate::models::{
        BackupBoardSummary, BackupInfo, Board, BoardAccessMode, BoardBannerMode, Report,
        ReportWithContext, Theme,
    };
    use crate::theme_builder::{build_theme_css, builder_defaults_for_preset};

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
            max_image_size: 8 * 1024 * 1024,
            max_video_size: 50 * 1024 * 1024,
            max_audio_size: 150 * 1024 * 1024,
            allow_pdf: false,
            allow_any_files: false,
            allow_tripcodes: true,
            allow_editing: true,
            allow_self_delete: true,
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

    fn sample_builder_theme() -> Theme {
        let config = builder_defaults_for_preset("forest");
        Theme {
            slug: "guided-forest".into(),
            display_name: "Guided Forest".into(),
            description: "Builder-backed theme".into(),
            swatch_hex: "#7ab84e".into(),
            enabled: true,
            sort_order: 1000,
            is_builtin: false,
            custom_css: build_theme_css("guided-forest", &config),
        }
    }

    fn sample_legacy_theme() -> Theme {
        Theme {
            slug: "legacy-sunset".into(),
            display_name: "Legacy Sunset".into(),
            description: "Older raw CSS theme".into(),
            swatch_hex: "#cc7744".into(),
            enabled: true,
            sort_order: 1010,
            is_builtin: false,
            custom_css: "html[data-theme=\"legacy-sunset\"] { --bg: #211; }".into(),
        }
    }

    fn sample_full_backup() -> BackupInfo {
        BackupInfo {
            backup_ref: "2026-04-07_1015_full-site_ab12cd".into(),
            backup_id: "2026-04-07_1015_full-site_ab12cd".into(),
            filename: "full-2026-04-07.zip".into(),
            size_bytes: 2048,
            modified: "2026-04-07 10:15 UTC".into(),
            modified_epoch: Some(1_775_555_700),
            verified: true,
            verification_note: "verified".into(),
            scope: "Full site".into(),
            mode: "Single ZIP".into(),
            part_count: 1,
            part_filenames: Vec::new(),
            contains_tor_hidden_service_keys: true,
            boards: vec![BackupBoardSummary {
                short_name: "tech".into(),
                name: "Technology".into(),
            }],
            server_path: "/tmp/rustchan-data/backups/2026-04-07_1015_full-site_ab12cd".into(),
            manifest_path:
                "/tmp/rustchan-data/backups/2026-04-07_1015_full-site_ab12cd/manifest.json".into(),
            downloadable_archive: true,
        }
    }

    fn sample_board_backup() -> BackupInfo {
        BackupInfo {
            backup_ref: "2026-04-07_1100_board-tech_ef34gh".into(),
            backup_id: "2026-04-07_1100_board-tech_ef34gh".into(),
            filename: "tech-2026-04-07.zip".into(),
            size_bytes: 1024,
            modified: "2026-04-07 11:00 UTC".into(),
            modified_epoch: Some(1_775_558_400),
            verified: true,
            verification_note: "verified".into(),
            scope: "Board".into(),
            mode: "Single ZIP".into(),
            part_count: 1,
            part_filenames: Vec::new(),
            contains_tor_hidden_service_keys: false,
            boards: Vec::new(),
            server_path: "/tmp/rustchan-data/backups/2026-04-07_1100_board-tech_ef34gh".into(),
            manifest_path:
                "/tmp/rustchan-data/backups/2026-04-07_1100_board-tech_ef34gh/manifest.json".into(),
            downloadable_archive: true,
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

    fn sample_ip_history_post() -> crate::models::Post {
        crate::models::Post {
            id: 42,
            thread_id: 9,
            board_id: 7,
            name: "mod scout".into(),
            tripcode: Some("!trip".into()),
            subject: None,
            body: "Needs a closer look".into(),
            body_html: "<p>Needs a closer look</p>".into(),
            ip_hash: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            ),
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            created_at: 1_775_560_000,
            deletion_token: "token".into(),
            is_op: false,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            edited_at: None,
            media_processing_state: None,
            media_processing_error: None,
        }
    }

    fn render_admin_panel_for_test(
        boards: &[Board],
        reports: &[ReportWithContext],
        themes: &[Theme],
        open_section: Option<&str>,
    ) -> String {
        render_admin_panel_for_test_with_backup_options(
            boards,
            reports,
            themes,
            open_section,
            true,
            true,
        )
    }

    fn render_admin_panel_for_test_with_backup_options(
        boards: &[Board],
        reports: &[ReportWithContext],
        themes: &[Theme],
        open_section: Option<&str>,
        tor_backup_available: bool,
        full_backup_has_tor_keys: bool,
    ) -> String {
        let mut full_backup = sample_full_backup();
        full_backup.contains_tor_hidden_service_keys = full_backup_has_tor_keys;
        let full_backups = vec![full_backup];
        let board_backups = vec![sample_board_backup()];
        let view = AdminPanelViewModel {
            csrf_token: "csrf",
            boards,
            current_theme: None,
            moderation: AdminPanelModerationView {
                bans: &[],
                filters: &[],
                reports,
                appeals: &[],
            },
            appearance: AdminPanelAppearanceView {
                site_name: "RustChan",
                site_subtitle: "select board to proceed",
                homepage_new_thread_badges_enabled: true,
                homepage_new_reply_badges_enabled: true,
                thread_new_reply_badges_enabled: true,
                default_theme: "terminal",
                banner_rotation_interval_minutes: 0,
                banner_external_links_enabled: false,
                themes,
                global_banners: &[],
                home_banners: &[],
                board_banners: &[],
            },
            backups: AdminPanelBackupsView {
                full_backups: &full_backups,
                board_backups: &board_backups,
                backup_status_line: "All saved backups verified.",
                backup_warning: None,
                auto_full_backup_interval_hours: 24,
                auto_full_backup_copies_to_keep: 7,
                auto_full_backup_include_tor_hidden_service_keys: false,
                auto_full_backup_storage_mode: "directory",
                auto_full_backup_split_zip_part_size_gib: 4,
                tor_hidden_service_key_backup_available: tor_backup_available,
            },
            maintenance: AdminPanelMaintenanceView {
                db_size_bytes: 4096,
                db_size_warning: false,
                ffmpeg_timeout_secs: crate::config::DEFAULT_FFMPEG_TIMEOUT_SECS,
                media_auto_prune_enabled: false,
                media_max_active_content_size_bytes: 0,
                media_detection: AdminMediaDetectionView {
                    ffmpeg: AdminDetectionStatus::Detected,
                    ffprobe: AdminDetectionStatus::Detected,
                    webp_encoder: AdminDetectionStatus::Detected,
                    vp9_pipeline: AdminDetectionStatus::Detected,
                    pdf_thumbnail_renderer: Some("pdftoppm".to_string()),
                },
            },
            tor_address: None,
            flash: None,
            open_section,
        };
        admin_panel_page(&view)
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
    fn board_settings_card_masks_board_password_input() {
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

        assert!(html.contains(
            r#"<input type="password" name="access_password" maxlength="256" autocomplete="off""#
        ));
    }

    #[test]
    fn public_board_with_saved_password_explains_password_is_unused() {
        let mut board = sample_board();
        board.access_mode = BoardAccessMode::Public;
        board.access_password_hash = "hashed".into();
        let html = render_board_settings_card(
            &board,
            0,
            std::slice::from_ref(&board),
            "csrf",
            &[sample_theme()],
            &[],
            None,
        );

        assert!(html.contains("A password is saved but unused while this board is public."));
    }

    #[test]
    fn admin_panel_theme_workshop_handles_guided_and_legacy_custom_themes() {
        let board = sample_board();
        let html = render_admin_panel_for_test(
            &[board],
            &[],
            &[
                sample_theme(),
                sample_builder_theme(),
                sample_legacy_theme(),
            ],
            Some("theme-catalog"),
        );

        assert!(html.contains("guided theme builder"));
        assert!(html.contains("Page and background"));
        assert!(html.contains("Posts/cards"));
        assert!(html.contains("Forms/buttons"));
        assert!(html.contains("Advanced/legacy CSS"));
        assert!(html.contains("data-theme-builder-color-for=\"background_color\""));
        assert!(html.contains("legacy custom CSS theme"));
        assert!(html.contains("Guided Forest"));
        assert!(html.contains("Legacy Sunset"));
    }

    #[test]
    fn admin_panel_builtin_theme_metadata_is_read_only() {
        let board = sample_board();
        let html = render_admin_panel_for_test(&[board], &[], &[sample_theme()], None);

        assert!(html.contains("Built-in theme metadata is managed by RustChan"));
        assert!(html.contains(r#"value="Terminal" maxlength="64" readonly aria-readonly="true""#));
        assert!(html.contains(r##"value="#7ab84e" disabled"##));
    }

    #[test]
    fn board_settings_card_renders_self_edit_and_self_delete_checkboxes_without_token_input() {
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

        assert!(html.contains(r#"name="allow_editing" value="1""#));
        assert!(
            html.contains("Allow users to edit their own posts during the 60-second grace window")
        );
        assert!(html.contains(r#"name="allow_self_delete" value="1""#));
        assert!(html
            .contains("Allow users to delete their own posts during the 60-second grace window"));
        assert!(!html.contains(r#"name="edit_window_secs""#));
        assert!(!html.contains("edit token"));
    }

    #[test]
    fn board_settings_card_renders_per_board_upload_limits() {
        let board = Board {
            max_image_size: 25 * 1024 * 1024,
            max_video_size: 500 * 1024 * 1024,
            max_audio_size: 300 * 1024 * 1024,
            ..sample_board()
        };
        let html = render_board_settings_card(
            &board,
            0,
            std::slice::from_ref(&board),
            "csrf",
            &[sample_theme()],
            &[],
            None,
        );

        assert!(html.contains(r#"name="max_image_size_mb""#));
        assert!(html.contains(r#"name="max_video_size_mb""#));
        assert!(html.contains(r#"name="max_audio_size_mb""#));
        assert!(html.contains(r#"value="25""#));
        assert!(html.contains(r#"value="500""#));
        assert!(html.contains(r#"value="300""#));
        assert!(!html.contains("Cannot exceed the site-wide"));
        assert!(!html.contains(r#"max="8""#));
        assert!(!html.contains(r#"max="50""#));
        assert!(!html.contains(r#"max="150""#));
        assert!(html.contains("PDF and any-file uploads still use the largest enabled cap"));
    }

    #[test]
    fn admin_panel_site_settings_renders_split_new_activity_controls_in_order() {
        let html = render_admin_panel_for_test(
            &[sample_board()],
            &[sample_report()],
            &[sample_theme()],
            Some("site-settings"),
        );

        assert!(html.contains(r#"<div class="board-settings-checks">"#));
        assert!(html.contains(r#"name="homepage_new_thread_badges_enabled" value="1" checked"#));
        assert!(html.contains("Homepage board-card new-thread badges"));
        assert!(html.contains(r#"name="homepage_new_reply_badges_enabled" value="1" checked"#));
        assert!(html.contains("Show new reply badges on homepage"));
        assert!(html.contains(r#"name="thread_new_reply_badges_enabled" value="1" checked"#));
        assert!(html.contains("Board/catalog thread-card new-reply badges"));
        assert!(html.contains(
            "Track newly created threads on the home page, new replies on the home page, and new replies inside board index/catalog cards independently."
        ));

        let theme_idx = html.find("Default theme").expect("theme control present");
        let homepage_idx = html
            .find("Homepage board-card new-thread badges")
            .expect("homepage control present");
        let thread_idx = html
            .find("Board/catalog thread-card new-reply badges")
            .expect("thread control present");
        let homepage_reply_idx = html
            .find("Show new reply badges on homepage")
            .expect("homepage reply control present");

        assert!(theme_idx < homepage_idx);
        assert!(homepage_idx < homepage_reply_idx);
        assert!(homepage_reply_idx < thread_idx);
    }

    #[test]
    fn admin_panel_media_prune_toggle_uses_checkbox_row_layout() {
        let html = render_admin_panel_for_test(
            &[sample_board()],
            &[sample_report()],
            &[sample_theme()],
            Some("media-settings"),
        );

        assert!(html.contains(
            r#"<div class="board-settings-checks">
    <label class="admin-inline-checkbox" title="Delete oldest full-size post media when active stored media exceeds the configured cap. Thumbnails are kept where practical.">
      <input type="checkbox" name="media_auto_prune_enabled" value="1">
      Enable automatic active content pruning
    </label>
  </div>"#
        ));
        assert!(!html.contains(
            r#"<span>
        <input type="checkbox" name="media_auto_prune_enabled""#
        ));

        let timeout_section_idx = html
            .find("// ffmpeg timeout")
            .expect("timeout section present");
        let pruning_section_idx = html
            .find("// media pruning")
            .expect("pruning section present");
        let prune_toggle_idx = html
            .find("Enable automatic active content pruning")
            .expect("prune toggle present");
        assert!(timeout_section_idx < pruning_section_idx);
        assert!(pruning_section_idx < prune_toggle_idx);
    }

    #[test]
    fn board_appearance_card_keeps_nsfw_tag() {
        let mut board = sample_board();
        board.nsfw = true;
        let html = render_board_appearance_card(
            &board,
            std::slice::from_ref(&board),
            "csrf",
            &[sample_theme()],
            &[],
            None,
        );

        assert!(html.contains(
            r#"<summary>/tech/ — Technology <span class="tag nsfw-tag">NSFW</span></summary>"#
        ));
    }

    #[test]
    fn admin_login_page_uses_semantic_form_layout() {
        let board = sample_board();
        let html = admin_login_page(
            Some("bad login"),
            "csrf",
            std::slice::from_ref(&board),
            Some("blue-sky"),
        );

        assert!(
            html.contains(r#"<form method="POST" action="/admin/login" class="admin-login-form">"#)
        );
        assert!(html.contains("<h1>Admin Login</h1>"));
        assert!(
            html.contains(r#"<div class="error admin-login-error" role="alert">bad login</div>"#)
        );
        assert!(html.contains(r#"<input type="hidden" name="_csrf" value="csrf">"#));
        assert!(html.contains(r#"<label class="admin-login-field">Username"#));
        assert!(html.contains(
            r#"<input type="text" name="username" autofocus required autocomplete="username">"#
        ));
        assert!(html.contains(r#"<label class="admin-login-field">Password"#));
        assert!(html.contains(
            r#"<input type="password" name="password" required autocomplete="current-password">"#
        ));
        assert!(html.contains(r#"<button type="submit">authenticate</button>"#));
        assert!(!html.contains("admin-login-table"));
    }

    #[test]
    fn admin_panel_renders_compact_section_index_before_sections() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = render_admin_panel_for_test(std::slice::from_ref(&board), &[], &themes, None);

        let index = html
            .find(r#"<nav class="admin-section-index" aria-label="Admin panel sections">"#)
            .expect("section index");
        let overview = html
            .find(r#"class="admin-panel-overview" id="overview""#)
            .expect("overview section");

        assert!(index < overview);
        for target in [
            "#site-settings",
            "#boards",
            "#moderation",
            "#appearance",
            "#backups",
            "#maintenance",
        ] {
            assert!(html.contains(&format!(r#"href="{target}""#)));
        }
    }

    #[test]
    fn admin_db_result_pages_use_shared_status_surfaces() {
        let report = DbHealthReport {
            before: DbHealthSnapshot {
                integrity: DbCheckResult {
                    ok: false,
                    messages: vec!["row 1".into(), "row 2".into()],
                },
                foreign_keys: DbCheckResult {
                    ok: true,
                    messages: Vec::new(),
                },
            },
            repair_attempted: false,
            repair_backup: None,
            repair_backup_error: None,
            repair_summary: Vec::new(),
            repair_steps: Vec::new(),
            after: None,
        };
        let html = admin_db_health_result_page(&report, false, "csrf", None, Some("blue-sky"));
        let idle_html = admin_db_repair_idle_page("csrf", Some("blue-sky"));

        assert!(html.contains(r#"class="admin-result-card""#));
        assert!(html.contains(r#"class="admin-result-details""#));
        assert!(html.contains(r#"class="admin-status-error">Problem found"#));
        assert!(html.contains(r#"class="admin-muted-list-item">No repairs were run."#));
        assert!(idle_html.contains(r#"class="admin-result-card""#));
        assert!(
            !html.contains(r#"<div class="page-box" style="margin-top:0.75rem;max-width:760px">"#)
        );
        assert!(!idle_html
            .contains(r#"<div class="page-box" style="margin-top:0.75rem;max-width:760px">"#));
    }

    #[test]
    fn admin_panel_groups_board_and_backup_areas_by_task() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = render_admin_panel_for_test(std::slice::from_ref(&board), &[], &themes, None);

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
        assert!(html.contains(r#"name="allow_pdf" value="1"> Allow PDF uploads"#));
        assert!(html.contains("Enabling this makes posting require JavaScript on this board."));
        assert!(html.contains(r#"data-admin-dropdown-key="boards""#));
        assert!(html.contains("// board appearance overrides"));
        assert!(html.contains("id=\"board-appearance-tech\""));
        assert!(html.contains("save board appearance"));
        assert!(html.contains("id=\"board-backup-tech\""));
        assert!(html.contains("// create board backups"));
        assert!(html.contains("// automated full backups"));
        assert!(html.contains(r#"name="auto_full_backup_storage_mode" value="directory" checked"#));
        assert!(html.contains(r#"name="auto_full_backup_storage_mode" value="split_zip""#));
        assert!(html.contains(r#"name="auto_full_backup_split_zip_part_size_gib""#));
        assert!(html.contains("<summary>Manual backup</summary>"));
        assert!(html.contains(r#"class="backup-output-fieldset""#));
        assert!(html.contains(r#"type="radio" name="storage_mode" value="directory" checked"#));
        assert!(html.contains(r#"type="radio" name="storage_mode" value="split_zip""#));
        assert!(html.contains(r#"name="split_zip_part_size_gib""#));
        assert!(html.contains("// saved full backups"));
        assert!(html.contains("data-admin-dropdown-key=\"full-backup-restore\""));
        assert!(html.contains("single-board tools"));
        assert!(html.contains("// restore from local file"));
        assert!(html.contains("// saved board backups"));
        assert!(html.contains("advanced: board backup and restore"));
        assert!(!html.contains("<section class=\"admin-section admin-section-collapsible\" id=\"board-backup-restore\">"));
        assert!(html.contains("Single ZIP"));
        assert!(html.contains(r#"data-admin-dropdown-key="media-settings""#));
        assert!(html.contains(r#"data-admin-dropdown-key="database-maintenance""#));
        assert!(html.contains(
            r#"<section class="admin-section admin-section-collapsible" id="media-settings">
<details class="admin-dropdown" data-admin-dropdown-key="media-settings""#
        ));
        assert!(html.contains(
            r#"<section class="admin-section admin-section-collapsible" id="database-maintenance">
<details class="admin-dropdown" data-admin-dropdown-key="database-maintenance""#
        ));
        assert!(html.contains("// media settings"));
        assert!(html.contains("// media pipeline detection"));
        assert!(html.contains("video thumbnails, waveform jobs, and transcoding entrypoint"));
        assert!(html.contains("selected renderer: pdftoppm"));
        assert!(html.contains("Enable automatic active content pruning"));
        assert!(html.contains("name=\"media_max_active_content_size\""));
        assert!(html.contains("Maximum active content database/media size"));
        assert!(html.contains("save media settings"));

        let full_backup_start = html
            .find(r#"<section class="admin-section admin-section-collapsible" id="full-backup-restore">"#)
            .expect("full backup section");
        let full_backup_end = html[full_backup_start..]
            .find(r"</section>")
            .map(|offset| full_backup_start + offset)
            .expect("full backup section closes");
        let full_backup_html = &html[full_backup_start..full_backup_end];
        assert!(full_backup_html.contains(
            r#"<details class="admin-dropdown" data-admin-dropdown-key="full-backup-restore""#
        ));
        assert!(full_backup_html.contains(r#"class="backup-extract-details""#));
        assert!(full_backup_html.contains("advanced: board backup and restore"));
        assert!(full_backup_html.contains(r#"class="backup-manual-details""#));
        assert!(full_backup_html.contains(r#"name="split_zip_part_size_gib""#));
        assert!(full_backup_html.contains(r#"type="radio" name="storage_mode" value="split_zip""#));

        let maintenance_html = &html[maintenance..];
        assert!(!maintenance_html.contains("advanced: board backup and restore"));
    }

    #[test]
    fn admin_quick_create_board_form_matches_standardized_defaults() {
        let board = sample_board();
        let html =
            render_admin_panel_for_test(std::slice::from_ref(&board), &[], &[sample_theme()], None);

        let form_start = html
            .find(r#"action="/admin/board/create""#)
            .expect("quick-create form present");
        let form_end = html[form_start..]
            .find("</form>")
            .map(|offset| form_start + offset)
            .expect("quick-create form closes");
        let form_html = &html[form_start..form_end];

        assert!(form_html.contains(r#"name="allow_audio" value="1"> Enable audio uploads"#));
        assert!(!form_html.contains(r#"name="allow_audio" value="1" checked"#));
    }

    #[test]
    fn admin_panel_reports_only_render_resolve_action() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let report = sample_report();
        let html = render_admin_panel_for_test(
            std::slice::from_ref(&board),
            std::slice::from_ref(&report),
            &themes,
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
        let html = render_admin_panel_for_test(
            std::slice::from_ref(&board),
            std::slice::from_ref(&report),
            &themes,
            Some("reports"),
        );

        assert!(html.contains(
            r#"<details class="admin-dropdown" data-admin-dropdown-key="reports" open>"#
        ));
    }

    #[test]
    fn admin_panel_maintenance_orders_media_before_database() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = render_admin_panel_for_test(std::slice::from_ref(&board), &[], &themes, None);

        let media = html
            .find("// media settings")
            .expect("media settings section");
        let database = html
            .find("// database maintenance")
            .expect("database maintenance section");

        assert!(media < database);
    }

    #[test]
    fn admin_panel_boards_and_media_sections_honor_open_target() {
        let board = sample_board();
        let themes = vec![sample_theme()];

        let boards_html =
            render_admin_panel_for_test(std::slice::from_ref(&board), &[], &themes, Some("boards"));
        assert!(boards_html
            .contains(r#"<details class="admin-dropdown" data-admin-dropdown-key="boards" open>"#));

        let media_html = render_admin_panel_for_test(
            std::slice::from_ref(&board),
            &[],
            &themes,
            Some("media-settings"),
        );
        assert!(media_html.contains(
            r#"<details class="admin-dropdown" data-admin-dropdown-key="media-settings" open>"#
        ));
    }

    #[test]
    fn admin_panel_media_detection_statuses_render_missing_states() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = admin_panel_page(&AdminPanelViewModel {
            csrf_token: "csrf",
            boards: std::slice::from_ref(&board),
            current_theme: Some("blue-sky"),
            moderation: AdminPanelModerationView {
                bans: &[],
                filters: &[],
                reports: &[],
                appeals: &[],
            },
            appearance: AdminPanelAppearanceView {
                site_name: "RustChan",
                site_subtitle: "select board to proceed",
                homepage_new_thread_badges_enabled: true,
                homepage_new_reply_badges_enabled: true,
                thread_new_reply_badges_enabled: true,
                default_theme: "terminal",
                banner_rotation_interval_minutes: 0,
                banner_external_links_enabled: false,
                themes: &themes,
                global_banners: &[],
                home_banners: &[],
                board_banners: &[],
            },
            backups: AdminPanelBackupsView {
                full_backups: &[],
                board_backups: &[],
                backup_status_line: "Latest full backup: none saved.",
                backup_warning: None,
                auto_full_backup_interval_hours: 24,
                auto_full_backup_copies_to_keep: 7,
                auto_full_backup_include_tor_hidden_service_keys: false,
                auto_full_backup_storage_mode: "directory",
                auto_full_backup_split_zip_part_size_gib: 4,
                tor_hidden_service_key_backup_available: false,
            },
            maintenance: AdminPanelMaintenanceView {
                db_size_bytes: 4096,
                db_size_warning: false,
                ffmpeg_timeout_secs: crate::config::DEFAULT_FFMPEG_TIMEOUT_SECS,
                media_auto_prune_enabled: false,
                media_max_active_content_size_bytes: 0,
                media_detection: AdminMediaDetectionView {
                    ffmpeg: AdminDetectionStatus::Missing,
                    ffprobe: AdminDetectionStatus::Missing,
                    webp_encoder: AdminDetectionStatus::Missing,
                    vp9_pipeline: AdminDetectionStatus::Missing,
                    pdf_thumbnail_renderer: None,
                },
            },
            tor_address: None,
            flash: None,
            open_section: Some("media-settings"),
        });

        assert!(html.contains("using built-in generic PDF placeholder thumbnail"));
        assert!(html.contains(r#"admin-detection-pill admin-detection-pill-missing">missing"#));
    }

    #[test]
    fn admin_panel_live_log_renders_connection_status_surface() {
        let board = sample_board();
        let themes = vec![sample_theme()];
        let html = render_admin_panel_for_test(std::slice::from_ref(&board), &[], &themes, None);

        assert!(html.contains(r#"id="admin-live-log-status""#));
        assert!(html.contains("Connecting to live log"));
    }

    #[test]
    fn admin_panel_prefers_selected_theme_over_default_theme() {
        let board = sample_board();
        let themes = vec![
            sample_theme(),
            Theme {
                slug: "blue-sky".into(),
                display_name: "Blue Sky".into(),
                description: "Bright override".into(),
                swatch_hex: "#66aaff".into(),
                enabled: true,
                sort_order: 2,
                is_builtin: true,
                custom_css: String::new(),
            },
        ];
        crate::templates::set_live_default_theme("terminal");
        crate::templates::set_live_themes(themes.clone());

        let html = admin_panel_page(&AdminPanelViewModel {
            csrf_token: "csrf",
            boards: std::slice::from_ref(&board),
            current_theme: Some("blue-sky"),
            moderation: AdminPanelModerationView {
                bans: &[],
                filters: &[],
                reports: &[],
                appeals: &[],
            },
            appearance: AdminPanelAppearanceView {
                site_name: "RustChan",
                site_subtitle: "select board to proceed",
                homepage_new_thread_badges_enabled: true,
                homepage_new_reply_badges_enabled: true,
                thread_new_reply_badges_enabled: true,
                default_theme: "terminal",
                banner_rotation_interval_minutes: 0,
                banner_external_links_enabled: false,
                themes: &themes,
                global_banners: &[],
                home_banners: &[],
                board_banners: &[],
            },
            backups: AdminPanelBackupsView {
                full_backups: &[],
                board_backups: &[],
                backup_status_line: "",
                backup_warning: None,
                auto_full_backup_interval_hours: 24,
                auto_full_backup_copies_to_keep: 1,
                auto_full_backup_include_tor_hidden_service_keys: false,
                auto_full_backup_storage_mode: "directory",
                auto_full_backup_split_zip_part_size_gib: 4,
                tor_hidden_service_key_backup_available: false,
            },
            maintenance: AdminPanelMaintenanceView {
                db_size_bytes: 0,
                db_size_warning: false,
                ffmpeg_timeout_secs: crate::config::DEFAULT_FFMPEG_TIMEOUT_SECS,
                media_auto_prune_enabled: false,
                media_max_active_content_size_bytes: 0,
                media_detection: AdminMediaDetectionView {
                    ffmpeg: AdminDetectionStatus::Detected,
                    ffprobe: AdminDetectionStatus::Detected,
                    webp_encoder: AdminDetectionStatus::Detected,
                    vp9_pipeline: AdminDetectionStatus::Detected,
                    pdf_thumbnail_renderer: None,
                },
            },
            tor_address: None,
            flash: None,
            open_section: None,
        });

        assert!(html.contains(r#"data-default-theme="terminal""#));
        assert!(html.contains(r#"data-active-theme="blue-sky""#));
        assert!(html.contains(r#"data-theme="blue-sky""#));
    }

    #[test]
    fn admin_panel_falls_back_when_selected_theme_is_disabled() {
        let board = sample_board();
        let themes = vec![
            sample_theme(),
            Theme {
                slug: "blue-sky".into(),
                display_name: "Blue Sky".into(),
                description: "Disabled".into(),
                swatch_hex: "#66aaff".into(),
                enabled: false,
                sort_order: 2,
                is_builtin: true,
                custom_css: String::new(),
            },
        ];
        crate::templates::set_live_default_theme("terminal");
        crate::templates::set_live_themes(themes.clone());

        let html = admin_panel_page(&AdminPanelViewModel {
            csrf_token: "csrf",
            boards: std::slice::from_ref(&board),
            current_theme: Some("blue-sky"),
            moderation: AdminPanelModerationView {
                bans: &[],
                filters: &[],
                reports: &[],
                appeals: &[],
            },
            appearance: AdminPanelAppearanceView {
                site_name: "RustChan",
                site_subtitle: "select board to proceed",
                homepage_new_thread_badges_enabled: true,
                homepage_new_reply_badges_enabled: true,
                thread_new_reply_badges_enabled: true,
                default_theme: "terminal",
                banner_rotation_interval_minutes: 0,
                banner_external_links_enabled: false,
                themes: &themes,
                global_banners: &[],
                home_banners: &[],
                board_banners: &[],
            },
            backups: AdminPanelBackupsView {
                full_backups: &[],
                board_backups: &[],
                backup_status_line: "",
                backup_warning: None,
                auto_full_backup_interval_hours: 24,
                auto_full_backup_copies_to_keep: 1,
                auto_full_backup_include_tor_hidden_service_keys: false,
                auto_full_backup_storage_mode: "directory",
                auto_full_backup_split_zip_part_size_gib: 4,
                tor_hidden_service_key_backup_available: false,
            },
            maintenance: AdminPanelMaintenanceView {
                db_size_bytes: 0,
                db_size_warning: false,
                ffmpeg_timeout_secs: crate::config::DEFAULT_FFMPEG_TIMEOUT_SECS,
                media_auto_prune_enabled: false,
                media_max_active_content_size_bytes: 0,
                media_detection: AdminMediaDetectionView {
                    ffmpeg: AdminDetectionStatus::Detected,
                    ffprobe: AdminDetectionStatus::Detected,
                    webp_encoder: AdminDetectionStatus::Detected,
                    vp9_pipeline: AdminDetectionStatus::Detected,
                    pdf_thumbnail_renderer: None,
                },
            },
            tor_address: None,
            flash: None,
            open_section: None,
        });

        assert!(html.contains(r#"data-default-theme="terminal""#));
        assert!(html.contains(r#"data-active-theme="terminal""#));
        assert!(!html.contains(r#"data-theme="blue-sky""#));
    }

    #[test]
    fn admin_panel_full_backup_form_shows_tor_backup_checkbox_only_when_available() {
        let board = sample_board();
        let themes = vec![sample_theme()];

        let with_tor = render_admin_panel_for_test_with_backup_options(
            std::slice::from_ref(&board),
            &[],
            &themes,
            None,
            true,
            true,
        );
        assert!(with_tor.contains(r#"name="include_tor_hidden_service_keys" value="1""#));
        assert!(with_tor.contains("Include Tor hidden service keys"));
        assert!(!with_tor.contains(r#"name="include_tor_hidden_service_keys" value="1" checked"#));

        let without_tor = render_admin_panel_for_test_with_backup_options(
            std::slice::from_ref(&board),
            &[],
            &themes,
            None,
            false,
            true,
        );
        assert!(!without_tor.contains(r#"name="include_tor_hidden_service_keys" value="1""#));
    }

    #[test]
    fn admin_panel_saved_backup_restore_only_offers_tor_key_restore_when_backup_has_keys() {
        let board = sample_board();
        let themes = vec![sample_theme()];

        let with_tor = render_admin_panel_for_test_with_backup_options(
            std::slice::from_ref(&board),
            &[],
            &themes,
            None,
            true,
            true,
        );
        assert!(with_tor.contains("includes Tor hidden service keys"));
        assert!(with_tor.contains(r#"name="restore_tor_hidden_service_keys" value="1""#));
        assert!(!with_tor.contains(r#"name="restore_tor_hidden_service_keys" value="1" checked"#));

        let without_tor = render_admin_panel_for_test_with_backup_options(
            std::slice::from_ref(&board),
            &[],
            &themes,
            None,
            true,
            false,
        );
        assert!(without_tor.contains("no Tor hidden service keys"));
        assert!(!without_tor
            .contains("Replaces the current onion identity with the one from this backup."));
    }

    #[test]
    fn admin_ip_history_page_uses_shared_report_modal_and_requested_button_text() {
        let board = sample_board();
        let post = sample_ip_history_post();
        let pagination = crate::models::Pagination::new(1, 50, 1);
        let ip_hash = post.ip_hash.clone().expect("hash");
        let html = super::admin_ip_history_page(
            &ip_hash,
            &[(post, "tech".into())],
            &pagination,
            std::slice::from_ref(&board),
            "csrf123",
            Some("/tech/thread/9"),
            Some("blue-sky"),
        );

        assert!(html.contains(r#"id="report-modal""#));
        assert!(html.contains(r#"data-action="open-report""#));
        assert!(html.contains(r#"data-report-action="/admin/ip/report""#));
        assert!(html.contains(
            r#"data-report-ip-hash="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef""#
        ));
        assert!(html.contains("Go to admin pannel"));
        assert!(html.contains("Back to thread"));
    }
}
