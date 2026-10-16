//! Page templates for the administrative interface.

use crate::db::DbHealthReport;
use crate::models::{
    BackupInfo, Ban, BannerAsset, BannerTargetType, Board, BoardBannerMode, WordFilter,
};
use crate::utils::{files::format_file_size, sanitize::escape_html};
use std::collections::BTreeSet;

use super::{base_layout, fmt_ts, fmt_ts_short, render_pagination, urlencoding_simple};

// Admin login
#[must_use]
/// Renders the administrator login form and an optional authentication error.
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

// Admin panel
mod accounts;
/// Appearance-section rendering.
mod appearance;
mod application;
/// Backup-section rendering.
mod backups;
/// Board-section rendering.
mod boards;
/// Task-oriented Control Center rendering.
mod control_center;
/// Admin page layout and dashboard rendering.
mod layout;
/// Maintenance-section rendering.
mod maintenance;
mod management;
/// Moderation-section rendering.
mod moderation;
/// Network settings rendering.
mod network;
/// Settings restart banner and explicit form.
mod restart;
/// Server-rendered settings search.
mod search;
/// Site-health rendering.
mod site_health;
/// Software update and pre-upgrade backup views.
mod updates;

/// Render authenticated network configuration status and controls.
#[must_use]
pub fn render_network_settings(
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
    csrf: &str,
) -> String {
    network::render(fields, csrf)
}

/// Render a related restart-required process configuration section.
#[must_use]
pub fn render_runtime_settings(
    section: crate::config::admin::runtime::RuntimeSection,
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
    csrf: &str,
) -> String {
    network::render_section(fields, csrf, section.key(), section.label(), &format!("/admin/config/{}", section.route_key()), "Process configuration. Existing board preferences and live maintenance tools remain available in their sections.")
}

/// Complete input model for the administrator control panel.
#[derive(Debug)]
pub struct AdminPanelViewModel<'a> {
    /// CSRF token embedded in state-changing forms.
    pub csrf_token: &'a str,
    /// Boards visible to administrative controls and global navigation.
    pub boards: &'a [Board],
    /// Visitor-selected theme, when present.
    pub current_theme: Option<&'a str>,
    /// Authenticated network configuration controls.
    pub network_html: String,
    /// Authenticated grouped runtime settings.
    pub runtime_html: String,
    /// Optional runtime settings search query.
    pub settings_query: Option<&'a str>,
    /// Control-center dashboard data.
    pub dashboard: AdminPanelDashboardView<'a>,
    /// Moderation data.
    pub moderation: AdminPanelModerationView<'a>,
    /// Site appearance data.
    pub appearance: AdminPanelAppearanceView<'a>,
    /// Runtime health data.
    pub site_health: AdminPanelSiteHealthView<'a>,
    /// Backup configuration and saved backup data.
    pub backups: AdminPanelBackupsView<'a>,
    /// Database and media maintenance data.
    pub maintenance: AdminPanelMaintenanceView,
    /// Active onion address, when Tor is running.
    pub tor_address: Option<&'a str>,
    /// Optional result message displayed above the panel.
    pub flash: Option<AdminPanelFlash<'a>>,
    /// Section requested by the current URL fragment or redirect.
    pub open_section: Option<&'a str>,
}

/// Values displayed by the operational control-center dashboard.
#[derive(Debug)]
pub struct AdminPanelDashboardView<'a> {
    /// Application version.
    pub version: &'a str,
    /// Build identifier.
    pub build: &'a str,
    /// Compact setup-state label.
    pub setup_status: &'a str,
    /// Detailed setup-state explanation.
    pub setup_detail: &'a str,
    /// Severity of the setup state.
    pub setup_state: AdminDashboardState,
    /// Configured site title.
    pub site_title: &'a str,
    /// Configured public entry point.
    pub public_url: &'a str,
    /// Compact database-health label.
    pub db_status: &'a str,
    /// Detailed database-health explanation.
    pub db_detail: &'a str,
    /// Severity of the database state.
    pub db_state: AdminDashboardState,
    /// Compact backup-health label.
    pub backup_status: &'a str,
    /// Detailed backup-health explanation.
    pub backup_detail: &'a str,
    /// Severity of the backup state.
    pub backup_state: AdminDashboardState,
    /// Compact storage-health label.
    pub storage_status: &'a str,
    /// Detailed storage-health explanation.
    pub storage_detail: &'a str,
    /// Severity of the storage state.
    pub storage_state: AdminDashboardState,
    /// Compact Tor-health label.
    pub tor_status: &'a str,
    /// Detailed Tor-health explanation.
    pub tor_detail: &'a str,
    /// Severity of the Tor state.
    pub tor_state: AdminDashboardState,
    /// Compact media-dependency label.
    pub dependency_status: &'a str,
    /// Detailed media-dependency explanation.
    pub dependency_detail: &'a str,
    /// Severity of the dependency state.
    pub dependency_state: AdminDashboardState,
    /// Compact background-job label.
    pub job_status: &'a str,
    /// Detailed background-job explanation.
    pub job_detail: &'a str,
    /// Severity of the background-job state.
    pub job_state: AdminDashboardState,
    /// Human-readable board count.
    pub board_count: &'a str,
    /// Human-readable thread count.
    pub thread_count: &'a str,
    /// Human-readable post count.
    pub post_count: &'a str,
    /// Recent posting activity summary.
    pub recent_activity: &'a str,
    /// Active media summary.
    pub media_summary: &'a str,
    /// Compact report-queue label.
    pub report_status: &'a str,
    /// Detailed report-queue explanation.
    pub report_detail: &'a str,
    /// Severity of the report-queue state.
    pub report_state: AdminDashboardState,
}

/// Severity used to style and order dashboard signals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminDashboardState {
    /// Healthy or complete.
    Ok,
    /// Neutral context that does not require administrator action.
    Informational,
    /// Normal work is actively progressing.
    Pending,
    /// Degraded or worth reviewing.
    Warning,
    /// Requires administrator action.
    ActionNeeded,
    /// An operation or required capability failed.
    Failure,
    /// Intentionally unavailable.
    Disabled,
    /// State could not be determined.
    Unknown,
}

/// Moderation queues and rules displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelModerationView<'a> {
    /// Active bans.
    pub bans: &'a [Ban],
    /// Configured word filters.
    pub filters: &'a [WordFilter],
    /// Open reports enriched with post context.
    pub reports: &'a [crate::models::ReportWithContext],
    /// Pending ban appeals.
    pub appeals: &'a [crate::models::BanAppeal],
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "the view mirrors independent administrator appearance toggles"
)]
/// Appearance settings and assets displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelAppearanceView<'a> {
    /// Configured site name.
    pub site_name: &'a str,
    /// Configured site subtitle.
    pub site_subtitle: &'a str,
    /// Whether homepage new-thread badges are enabled.
    pub homepage_new_thread_badges_enabled: bool,
    /// Whether homepage new-reply badges are enabled.
    pub homepage_new_reply_badges_enabled: bool,
    /// Whether thread-page new-reply badges are enabled.
    pub thread_new_reply_badges_enabled: bool,
    /// Configured default theme slug.
    pub default_theme: &'a str,
    /// Banner rotation interval in minutes.
    pub banner_rotation_interval_minutes: i64,
    /// Whether banners may link to external sites.
    pub banner_external_links_enabled: bool,
    /// Available themes.
    pub themes: &'a [crate::models::Theme],
    /// Banners shared by board pages.
    pub global_banners: &'a [BannerAsset],
    /// Banners displayed on the homepage.
    pub home_banners: &'a [BannerAsset],
    /// Board-specific banners.
    pub board_banners: &'a [BannerAsset],
}

/// Backup settings and saved backup inventory displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelBackupsView<'a> {
    /// Saved full-site backups.
    pub full_backups: &'a [BackupInfo],
    /// Saved board-only backups.
    pub board_backups: &'a [BackupInfo],
    /// Current automatic-backup status.
    pub backup_status_line: &'a str,
    /// Optional backup warning.
    pub backup_warning: Option<&'a str>,
    /// Scheduled full-backup interval in hours.
    pub auto_full_backup_interval_hours: u64,
    /// Number of scheduled full backups retained.
    pub auto_full_backup_copies_to_keep: u64,
    /// Whether scheduled backups include onion-service keys.
    pub auto_full_backup_include_tor_hidden_service_keys: bool,
    /// Storage mode for scheduled full backups.
    pub auto_full_backup_storage_mode: &'a str,
    /// Part size used for split ZIP archives.
    pub auto_full_backup_split_zip_part_size_gib: u64,
    /// Whether onion-service keys are available to back up.
    pub tor_hidden_service_key_backup_available: bool,
}

/// Runtime and dependency health values displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelSiteHealthView<'a> {
    /// Overall server status.
    pub server_status: &'a str,
    /// Running `RustChan` version.
    pub rustchan_version: &'a str,
    /// Database schema status.
    pub database_schema_status: &'a str,
    /// Database integrity status.
    pub database_integrity_status: &'a str,
    /// Time of the last successful backup.
    pub last_successful_backup: &'a str,
    /// Time of the next scheduled backup.
    pub next_scheduled_backup: &'a str,
    /// Data-directory disk usage.
    pub data_dir_usage: &'a str,
    /// Upload-directory size.
    pub upload_dir_size: &'a str,
    /// Overall Tor status.
    pub tor_status: &'a str,
    /// Current onion address.
    pub tor_onion_address: Option<&'a str>,
    /// Onion-service runtime status.
    pub tor_service_status: &'a str,
    /// Configured Tor mode.
    pub tor_mode: &'a str,
    /// Safe Tor configuration summary.
    pub tor_config_summary: &'a str,
    /// Detailed Tor status.
    pub tor_detail: &'a str,
    /// Optional media dependency detection summary.
    pub dependency_summary: AdminSiteHealthDependencySummary,
    /// Number of currently running jobs.
    pub running_jobs: i64,
    /// Number of queued jobs.
    pub queued_jobs: i64,
    /// Number of recently completed jobs.
    pub recent_completed_jobs: i64,
    /// Number of failed jobs awaiting dismissal.
    pub failed_jobs: i64,
    /// Backup-job status summary.
    pub backup_jobs: &'a str,
    /// Restore-job status summary.
    pub restore_jobs: &'a str,
    /// Copyable diagnostic report.
    pub diagnostics_text: &'a str,
}

/// Detection summary for optional media-processing capabilities.
#[derive(Clone, Copy, Debug)]
pub struct AdminSiteHealthDependencySummary {
    /// `ffmpeg` detection state.
    pub ffmpeg: AdminDetectionStatus,
    /// AV1 decoder availability; independent of encoding.
    pub av1_decoder: AdminDetectionStatus,
    /// AV1 encoder availability; output policy still uses VP9.
    pub av1_encoder: AdminDetectionStatus,
    /// VP9 pipeline detection state.
    pub vp9: AdminDetectionStatus,
    /// Opus encoder detection state.
    pub opus: AdminDetectionStatus,
}

/// Database and media maintenance settings displayed in the panel.
#[derive(Debug)]
pub struct AdminPanelMaintenanceView {
    /// Current database file size in bytes.
    pub db_size_bytes: i64,
    /// Whether the database size exceeds its warning threshold.
    pub db_size_warning: bool,
    /// Setup wizard availability state.
    pub setup_status: AdminPanelSetupStatus,
    /// External media process timeout in seconds.
    pub ffmpeg_timeout_secs: u64,
    /// Whether automatic active-media pruning is enabled.
    pub media_auto_prune_enabled: bool,
    /// Maximum active database and media size before pruning.
    pub media_max_active_content_size_bytes: u64,
    /// Detected media-processing capabilities.
    pub media_detection: AdminMediaDetectionView,
}

/// Durable setup state relevant to administrator controls.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminPanelSetupStatus {
    /// Initial setup remains publicly available.
    Available,
    /// Initial setup completed normally.
    Complete,
    /// An administrator temporarily reopened setup.
    Reopened,
    /// Durable state exists even though no completion marker was found.
    Initialized,
}

/// Whether an optional executable or pipeline was detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminDetectionStatus {
    /// The capability is available.
    Detected,
    /// The capability is unavailable.
    Missing,
}

impl AdminDetectionStatus {
    /// Returns whether the capability was detected.
    #[must_use]
    pub const fn is_detected(self) -> bool {
        matches!(self, Self::Detected)
    }
}

/// Detailed media capability detection results.
#[derive(Debug)]
pub struct AdminMediaDetectionView {
    /// `ffmpeg` detection state.
    pub ffmpeg: AdminDetectionStatus,
    /// AV1 decoder availability; independent of encoding.
    pub av1_decoder: AdminDetectionStatus,
    /// AV1 encoder availability; output policy still uses VP9.
    pub av1_encoder: AdminDetectionStatus,
    /// VP9 and Opus pipeline detection state.
    pub vp9_pipeline: AdminDetectionStatus,
    /// Selected PDF thumbnail executable, when available.
    pub pdf_thumbnail_renderer: Option<String>,
}

/// Message displayed after an administrative action.
#[derive(Clone, Copy, Debug)]
pub struct AdminPanelFlash<'a> {
    /// Whether the message represents an error.
    pub is_error: bool,
    /// User-facing result text.
    pub message: &'a str,
}

/// Renders banner target-type options and marks the selected value.
fn banner_target_type_options(selected: BannerTargetType) -> String {
    let options = [
        (BannerTargetType::None, "No link"),
        (BannerTargetType::InternalBoard, "Open another board"),
        (BannerTargetType::InternalPath, "Open a specific thread"),
        (BannerTargetType::ExternalUrl, "Open another website"),
    ];
    let mut out = String::new();
    for (value, label) in options {
        crate::templates::append_html(
            &mut out,
            format_args!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = value.as_str(),
                selected = if value == selected { " selected" } else { "" },
                label = label,
            ),
        );
    }
    out
}

/// Renders a normalized banner image preview.
fn banner_preview_html(asset: &BannerAsset, alt: &str) -> String {
    format!(
        r#"<img class="board-banner-preview-image" src="{src}" alt="{alt}" width="{width}" height="{height}">"#,
        src = escape_html(&crate::banner::banner_asset_url(asset)),
        alt = escape_html(alt),
        width = crate::banner::DISPLAY_WIDTH,
        height = crate::banner::DISPLAY_HEIGHT,
    )
}

/// Renders board target options, retaining a missing saved target for repair.
fn banner_board_options(boards: &[Board], selected_value: &str) -> String {
    let trimmed_selected = selected_value.trim().trim_matches('/');
    let mut out = String::new();
    crate::templates::append_html(
        &mut out,
        format_args!(
            r#"<option value=""{}>Choose a board</option>"#,
            if trimmed_selected.is_empty() {
                " selected"
            } else {
                ""
            }
        ),
    );
    let board_exists = boards
        .iter()
        .any(|board| board.short_name == trimmed_selected);
    if !trimmed_selected.is_empty() && !board_exists {
        crate::templates::append_html(
            &mut out,
            format_args!(
                r#"<option value="{value}" selected>Missing board (/{value}/)</option>"#,
                value = escape_html(trimmed_selected),
            ),
        );
    }
    for board in boards {
        crate::templates::append_html(
            &mut out,
            format_args!(
                r#"<option value="{short}"{selected}>/{short}/ — {name}</option>"#,
                short = escape_html(&board.short_name),
                selected = if board.short_name == trimmed_selected {
                    " selected"
                } else {
                    ""
                },
                name = escape_html(&board.name),
            ),
        );
    }
    out
}

/// Renders the target controls for one banner editor.
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

/// Renders a banner upload form for the requested scope.
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
</div>"#.to_owned()
    } else {
        r#"<div class="admin-banner-toggle-group">
  <label class="admin-inline-checkbox"><input type="checkbox" name="enabled" value="1" checked> Enabled</label>
</div>"#.to_owned()
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

/// Renders one saved banner and its update, ordering, and delete controls.
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

/// Renders a collection of saved banners or an empty-state message.
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

/// Renders custom favicon controls for one board.
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

/// Renders banner mode, upload, and saved-banner controls for one board.
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

/// Renders backup creation and restore actions for one board.
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

/// Builds the settings form id shared by the board settings and appearance cards.
fn board_settings_form_id(board_id: i64) -> String {
    format!("board-settings-form-{board_id}")
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

/// Renders the appearance settings card for one board.
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
        crate::templates::append_html(
            &mut board_theme_options,
            format_args!(
                r#"<option value="{slug}"{selected}>{label}</option>"#,
                slug = escape_html(&theme.slug),
                selected = if theme.slug == board.default_theme {
                    " selected"
                } else {
                    ""
                },
                label = escape_html(&theme.display_name)
            ),
        );
    }
    let appearance_section = format!("board-appearance-{}", board.short_name);
    let open_attr = if open_section.is_some_and(|section| section == appearance_section) {
        " open"
    } else {
        ""
    };
    let form_id = board_settings_form_id(board.id);
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

/// Renders the backup tools card for one board.
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
/// Renders the main administrator control panel.
pub fn admin_panel_page(view: &AdminPanelViewModel<'_>) -> String {
    layout::render(view)
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

// VACUUM result
#[must_use]
/// Renders the space reclaimed by a completed `SQLite` `VACUUM`.
pub fn admin_vacuum_result_page(
    size_before: i64,
    size_after: i64,
    csrf_token: &str,
    current_theme: Option<&str>,
) -> String {
    let saved = size_before.saturating_sub(size_after);
    let pct = if size_before > 0 {
        saved
            .max(0)
            .saturating_mul(100)
            .checked_div(size_before)
            .unwrap_or(0)
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

#[expect(
    clippy::too_many_lines,
    reason = "the health report keeps one cohesive diagnostic result document"
)]
#[must_use]
/// Renders database health checks and an optional repair result.
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
            crate::templates::append_html(
                &mut repair_summary_html,
                format_args!(r"<li>{line}</li>", line = escape_html(line)),
            );
        }
    }

    let mut repair_steps_html = String::new();
    if report.repair_steps.is_empty() {
        repair_steps_html
            .push_str(r#"<li class="admin-muted-list-item">No maintenance steps were run.</li>"#);
    } else {
        for step in &report.repair_steps {
            crate::templates::append_html(
                &mut repair_steps_html,
                format_args!(r"<li>{step}</li>", step = escape_html(step)),
            );
        }
    }

    let backup_html = report.repair_backup.as_ref().map_or_else(
        || {
            report.repair_backup_error.as_ref().map_or_else(
                || r"<p><strong>Pre-repair backup:</strong> Not run</p>".to_owned(),
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
        || r"<p><strong>After:</strong> Not run</p>".to_owned(),
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
/// Renders the database-repair status page when no job is active.
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
</div>"#.to_owned();

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
/// Renders live progress for an active database-repair job.
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
/// Renders a stale database-repair job status request.
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
            || "<p>No maintenance rebuild is currently active.</p>".to_owned(),
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
/// Renders the terminal failure state of a database-repair job.
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

/// Renders schema, integrity, and foreign-key results for one health snapshot.
fn render_db_health_snapshot(snapshot: &crate::db::DbHealthSnapshot) -> String {
    format!(
        "{schema}{integrity}{foreign_keys}",
        schema = render_db_check_result("schema baseline", &snapshot.schema),
        integrity = render_db_check_result("integrity check", &snapshot.integrity),
        foreign_keys = render_db_check_result("foreign key check", &snapshot.foreign_keys),
    )
}

/// Renders one database check and its detailed output when needed.
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

/// Render administrator credential workflows without existing secret values.
#[must_use]
pub fn render_accounts(users: &[(i64, String, i64)], csrf: &str) -> String {
    accounts::render(users, csrf)
}

/// Render dedicated path/secret management without current secret material.
#[must_use]
pub fn render_management(
    state: &Result<crate::config::admin::management::ManagementState, String>,
    csrf: &str,
) -> String {
    management::render(state, csrf)
}

/// Render active/saved/source information for the retained live controls.
#[must_use]
pub fn render_application_state(
    fields: &Result<Vec<crate::config::admin::SettingField>, String>,
) -> String {
    application::render(fields)
}

/// Render administrator-only software status inside the existing admin workspace.
#[must_use]
pub fn render_software_updates(
    status: &crate::updates::Status,
    csrf: &str,
    managed: bool,
    container: bool,
) -> String {
    updates::render(status, csrf, managed, container)
}

/// Render pre-upgrade snapshot history inside the existing Backups workspace.
#[must_use]
pub fn render_update_backups(status: &crate::updates::Status) -> String {
    updates::backups(status)
}

/// Render durable configuration restart state and its POST-only action.
#[must_use]
pub fn render_restart(view: &crate::restart::View, csrf: &str) -> String {
    restart::render(view, csrf)
}
