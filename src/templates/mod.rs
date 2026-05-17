// Shared HTML rendering helpers and page fragments.

use crate::config::CONFIG;
use crate::models::{Board, Pagination, Theme, SEARCH_QUERY_MAX_CHARS};
use crate::utils::sanitize::escape_html;
use chrono::{Local, TimeZone as _};
use parking_lot::RwLock;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

pub mod admin;
pub mod board;
pub mod forms;
pub mod thread;

pub use admin::*;
pub use board::*;
pub use thread::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredBoardView {
    Catalog,
    Index,
}

impl PreferredBoardView {
    #[must_use]
    pub const fn is_catalog(self) -> bool {
        matches!(self, Self::Catalog)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserPreferences {
    pub hide_nsfw_boards: bool,
    pub video_audio_muted: bool,
    pub preferred_board_view: PreferredBoardView,
    pub show_activity_badges: bool,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            hide_nsfw_boards: false,
            video_audio_muted: false,
            preferred_board_view: PreferredBoardView::Catalog,
            show_activity_badges: true,
        }
    }
}

impl UserPreferences {
    #[must_use]
    pub fn etag_fragment(self) -> String {
        format!(
            "u{}{}{}{}",
            i32::from(self.hide_nsfw_boards),
            i32::from(self.video_audio_muted),
            if self.preferred_board_view.is_catalog() {
                "c"
            } else {
                "i"
            },
            i32::from(self.show_activity_badges)
        )
    }
}

// ─── Live site name (DB-overridable, falls back to CONFIG.forum_name) ─────────
//
// parking_lot::RwLock is used instead of std::sync::RwLock for two reasons:
//  1. It never poisons — no need to handle poisoned-lock errors on the hot path.
//  2. Arc<str> reduces the per-read allocation to a single atomic increment
//     instead of a full String::clone(), which matters under high concurrency.

static LIVE_SITE_NAME: LazyLock<RwLock<Arc<str>>> =
    LazyLock::new(|| RwLock::new(Arc::from(CONFIG.forum_name.as_str())));
static LIVE_SITE_SUBTITLE: LazyLock<RwLock<Arc<str>>> =
    LazyLock::new(|| RwLock::new(Arc::from("select board to proceed")));

/// In-memory cache for the admin-configured default theme.
/// Updated immediately when admin saves site settings so pages reflect the
/// change without requiring a server restart or extra DB round-trip per request.
static LIVE_DEFAULT_THEME: LazyLock<RwLock<Arc<str>>> =
    LazyLock::new(|| RwLock::new(Arc::from("")));
static LIVE_THEMES: LazyLock<RwLock<Arc<Vec<Theme>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(Vec::new())));

/// In-memory cache of the current board list, used by standalone pages (error
/// pages, ban pages) that don't have DB access at render time.  Updated by
/// every handler that creates, deletes, or restores boards.
///
/// Stores a snapshot; stale for at most one request after a board change, but
/// that one request is itself the mutating POST which redirects anyway.
static LIVE_BOARDS: LazyLock<RwLock<Arc<Vec<crate::models::Board>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(Vec::new())));

/// Monotonically-increasing counter incremented every time the board list
/// changes.  Included in thread-page `ETags` so that adding or deleting a board
/// correctly invalidates cached thread pages (fixing stale nav bars).
static LIVE_BOARDS_VERSION: AtomicU64 = AtomicU64::new(0);
static LIVE_BOARD_NAV: LazyLock<RwLock<(u64, Arc<str>)>> =
    LazyLock::new(|| RwLock::new((0, Arc::from(""))));
static STATIC_ASSET_VERSION: LazyLock<String> = LazyLock::new(compute_static_asset_version);

/// Replace the in-memory board list.  Call after any board create / delete /
/// restore operation so that `error_page()` renders the correct top-bar links.
pub fn set_live_boards(boards: Vec<crate::models::Board>) {
    *LIVE_BOARDS.write() = Arc::new(boards);
    // Bump the version so thread-page ETags incorporate board-list changes.
    // This ensures adding/deleting a board invalidates cached thread pages,
    // fixing the stale nav bug where deleted boards persisted in the browser
    // cache until an unrelated reply bumped the thread.
    LIVE_BOARDS_VERSION.fetch_add(1, Ordering::Relaxed);
    rebuild_live_board_nav();
}

pub fn live_boards() -> Arc<Vec<crate::models::Board>> {
    Arc::clone(&*LIVE_BOARDS.read())
}

/// Public snapshot of the live board list.
///
/// Used by the thread-updates handler to include current nav HTML in polling
/// responses so the JS can refresh the nav bar when boards change while a
/// thread is open.
pub fn live_boards_snapshot() -> Arc<Vec<crate::models::Board>> {
    Arc::clone(&*LIVE_BOARDS.read())
}

/// Current board-list version.  Included in thread-page `ETags` so that board
/// mutations invalidate cached thread HTML (and thus stale nav bars).
pub fn live_boards_version() -> u64 {
    LIVE_BOARDS_VERSION.load(Ordering::Relaxed)
}

fn compute_static_asset_version() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or_else(
            || env!("CARGO_PKG_VERSION").to_owned(),
            |duration| duration.as_secs().to_string(),
        )
}

#[must_use]
pub fn static_asset_url(path: &str) -> String {
    format!("{path}?v={}", *STATIC_ASSET_VERSION)
}

#[must_use]
pub fn static_asset_version_matches(version: &str) -> bool {
    version == STATIC_ASSET_VERSION.as_str()
}

/// Call this at startup (after first DB read) and after admin saves a new name.
pub fn set_live_site_name(name: &str) {
    let val: Arc<str> = if name.trim().is_empty() {
        Arc::from(CONFIG.forum_name.as_str())
    } else {
        Arc::from(name)
    };
    *LIVE_SITE_NAME.write() = val;
}

pub fn set_live_site_subtitle(subtitle: &str) {
    let val: Arc<str> = if subtitle.trim().is_empty() {
        Arc::from("select board to proceed")
    } else {
        Arc::from(subtitle)
    };
    *LIVE_SITE_SUBTITLE.write() = val;
}

/// Update the in-memory default theme cache.
/// Pass an empty string to clear the admin override and fall back to the hard default.
pub fn set_live_default_theme(theme: &str) {
    *LIVE_DEFAULT_THEME.write() = Arc::from(theme);
}

/// Read the current live default theme slug.
pub fn live_default_theme() -> Arc<str> {
    Arc::clone(&*LIVE_DEFAULT_THEME.read())
}

pub fn set_live_themes(themes: Vec<Theme>) {
    *LIVE_THEMES.write() = Arc::new(themes);
}

pub fn live_themes() -> Arc<Vec<Theme>> {
    Arc::clone(&*LIVE_THEMES.read())
}

/// Read the current live site name.
pub fn live_site_name() -> Arc<str> {
    Arc::clone(&*LIVE_SITE_NAME.read())
}

pub fn live_site_subtitle() -> Arc<str> {
    Arc::clone(&*LIVE_SITE_SUBTITLE.read())
}

#[must_use]
pub fn normalize_theme_slug(theme: &str) -> Option<String> {
    live_themes()
        .iter()
        .find(|candidate| candidate.enabled && candidate.slug.eq_ignore_ascii_case(theme.trim()))
        .map(|candidate| candidate.slug.clone())
}

fn fallback_theme_slug() -> String {
    let themes = live_themes();
    normalize_theme_slug(crate::theme::HARD_DEFAULT_THEME)
        .or_else(|| {
            themes
                .iter()
                .find(|theme| theme.enabled)
                .map(|theme| theme.slug.clone())
        })
        .unwrap_or_else(|| crate::theme::HARD_DEFAULT_THEME.to_owned())
}

fn resolve_page_default_theme(board_default_theme: Option<&str>) -> String {
    board_default_theme
        .and_then(normalize_theme_slug)
        .or_else(|| normalize_theme_slug(&live_default_theme()))
        .unwrap_or_else(fallback_theme_slug)
}

#[must_use]
pub fn page_theme_etag_fragment(
    current_theme: Option<&str>,
    board_default_theme: Option<&str>,
) -> String {
    let default_theme = resolve_page_default_theme(board_default_theme);
    let active_theme = current_theme
        .and_then(normalize_theme_slug)
        .unwrap_or(default_theme);
    crate::utils::crypto::sha256_hex(active_theme.as_bytes())
        .chars()
        .take(12)
        .collect()
}

fn theme_css_href(theme: &str) -> String {
    format!(
        "/theme-css/{}?v={}",
        escape_html(theme),
        *STATIC_ASSET_VERSION
    )
}

fn board_nav_groups(boards: &[Board]) -> (Vec<&Board>, Vec<&Board>) {
    let sfw = boards
        .iter()
        .filter(|board| !board.nsfw)
        .collect::<Vec<_>>();
    let nsfw = boards.iter().filter(|board| board.nsfw).collect::<Vec<_>>();
    (sfw, nsfw)
}

fn board_href(short_name: &str, preferences: UserPreferences) -> String {
    if preferences.preferred_board_view.is_catalog() {
        format!("/{}/catalog", escape_html(short_name))
    } else {
        format!("/{}", escape_html(short_name))
    }
}

fn board_nav_group_html(
    boards: &[&Board],
    preferences: UserPreferences,
    is_nsfw: bool,
) -> Option<String> {
    if boards.is_empty() {
        return None;
    }
    let nsfw_attr = if is_nsfw {
        r#" data-board-nsfw="1""#
    } else {
        ""
    };
    let inner = boards
        .iter()
        .map(|board| {
            format!(
                r#"<a href="{href}">{short}</a>"#,
                href = board_href(&board.short_name, preferences),
                short = escape_html(&board.short_name),
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    Some(format!(
        r#"<span class="board-list-group"{nsfw_attr}>[ {inner} ]</span>"#
    ))
}

fn mobile_board_group_html(
    title: &str,
    boards: &[&Board],
    preferences: UserPreferences,
    is_nsfw: bool,
) -> String {
    if boards.is_empty() {
        return String::new();
    }
    let nsfw_attr = if is_nsfw {
        r#" data-board-nsfw="1""#
    } else {
        ""
    };
    let mut items = String::new();
    for board in boards {
        let short = escape_html(&board.short_name);
        let href = board_href(&board.short_name, preferences);
        let _ = write!(
            items,
            r#"<a class="mobile-board-link" href="{href}">/{short}/</a>"#,
        );
    }
    format!(
        r#"<div class="mobile-board-group"{nsfw_attr}><div class="mobile-board-group-title">{title}</div>{items}</div>"#
    )
}

fn rebuild_live_board_nav() {
    let boards = live_boards_snapshot();
    let nav_html = board_nav_html_for_preferences(boards.as_slice(), UserPreferences::default());
    let nav_html: Arc<str> = if nav_html.is_empty() {
        Arc::from("")
    } else {
        Arc::from(nav_html)
    };
    let version = live_boards_version();
    *LIVE_BOARD_NAV.write() = (version, nav_html);
}

#[must_use]
pub fn board_nav_html_for_preferences(boards: &[Board], preferences: UserPreferences) -> String {
    let (sfw_boards, nsfw_boards_all) = board_nav_groups(boards);
    let nsfw_boards = if preferences.hide_nsfw_boards {
        Vec::new()
    } else {
        nsfw_boards_all
    };
    [
        board_nav_group_html(&sfw_boards, preferences, false),
        board_nav_group_html(&nsfw_boards, preferences, true),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

// ─── Auto-compress modal ──────────────────────────────────────────────────────

/// Returns the compress-modal overlay HTML.
/// Dynamic size limits are embedded as data-max-image / data-max-video attributes
/// on the modal element and read by main.js at runtime ().
#[must_use]
pub fn compress_modal_script(max_image_bytes: usize, max_video_bytes: usize) -> String {
    format!(
        r#"
<!-- Auto-compress modal — shared by new-thread and reply forms on this page -->
<div id="compress-modal" class="compress-modal" style="display:none" role="dialog" aria-modal="true" aria-labelledby="compress-modal-title"
     data-max-image="{max_image_bytes}" data-max-video="{max_video_bytes}">
  <div class="compress-modal-box">
    <div class="compress-modal-title" id="compress-modal-title">&#9888; File Too Large</div>
    <div class="compress-modal-info" id="compress-info"></div>
    <div class="compress-progress" id="compress-progress" style="display:none">
      <div class="compress-progress-track"><div class="compress-progress-bar" id="compress-progress-bar"></div></div>
      <div class="compress-progress-text" id="compress-progress-text">Preparing…</div>
    </div>
    <div class="compress-modal-actions" id="compress-actions">
      <button class="compress-cancel-btn" data-action="dismiss-compress">Cancel</button>
      <button class="compress-do-btn" id="compress-do-btn" data-action="start-compress">&#9881; Auto-compress</button>
    </div>
    <div class="compress-done-actions" id="compress-done-actions" style="display:none">
      <button class="compress-cancel-btn" data-action="dismiss-compress">Close</button>
    </div>
  </div>
</div>"#,
    )
}

#[must_use]
pub const fn confirmation_modal_script() -> &'static str {
    r#"
<div id="confirm-modal" class="compress-modal" style="display:none" role="dialog" aria-modal="true" aria-labelledby="confirm-modal-title">
  <div class="compress-modal-box confirm-modal-box">
    <div class="compress-modal-title" id="confirm-modal-title">Confirm action</div>
    <div class="compress-modal-info confirm-modal-info" id="confirm-modal-message"></div>
    <div class="compress-modal-actions">
      <button type="button" class="compress-cancel-btn" id="confirm-modal-cancel">Cancel</button>
      <button type="button" class="compress-do-btn" id="confirm-modal-continue">Continue</button>
    </div>
  </div>
</div>"#
}

// ─── Report modal ─────────────────────────────────────────────────────────────

/// Returns the report overlay HTML. Injected once per thread page.
// JS functions live in /static/main.js.
#[must_use]
pub const fn report_modal_script() -> &'static str {
    r#"
<div id="report-modal" class="compress-modal" style="display:none" role="dialog" aria-modal="true" aria-labelledby="report-modal-title">
  <div class="compress-modal-box">
    <div class="compress-modal-title" id="report-modal-title">Report Thread/Post</div>
    <form method="POST" action="/report" id="report-form">
      <input type="hidden" name="_csrf"     id="report-csrf">
      <input type="hidden" name="post_id"   id="report-post-id">
      <input type="hidden" name="thread_id" id="report-thread-id">
      <input type="hidden" name="board"     id="report-board">
      <input type="hidden" name="ip_hash"   id="report-ip-hash">
      <div class="compress-modal-info confirm-modal-info" id="report-info"></div>
      <input type="text" name="reason" id="report-reason"
             placeholder="reason (optional)" maxlength="256"
             style="width:100%;background:var(--bg-input);border:1px solid var(--border);
                    color:var(--text);padding:5px 8px;font-family:var(--font);font-size:0.82rem;
                    box-sizing:border-box;margin-bottom:0.75rem">
      <div class="compress-modal-actions">
        <button type="button" class="compress-cancel-btn" data-action="close-report">Cancel</button>
        <button type="submit" class="compress-do-btn" id="report-submit-btn">Submit Report</button>
      </div>
    </form>
  </div>
</div>"#
}

// ─── Thread auto-update script ────────────────────────────────────────────────

// All auto-update logic lives in /static/main.js.
#[must_use]
pub const fn thread_autoupdate_script() -> &'static str {
    ""
}

// ─── Timestamp helpers ────────────────────────────────────────────────────────

#[must_use]
pub fn fmt_ts(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        _ => "unknown".to_owned(),
    }
}

#[must_use]
pub fn fmt_ts_short(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%m/%d/%y(%a)%H:%M:%S").to_string(),
        _ => "?".to_owned(),
    }
}

// ─── Embed thumbnail helper ───────────────────────────────────────────────────

/// Scan raw post body text for a video embed URL and return its thumbnail URL.
/// Used by catalog and board-index summaries when the OP has no uploaded file.
#[must_use]
pub fn embed_thumb_from_body(body: &str) -> Option<String> {
    for token in body.split_whitespace() {
        let clean = token.trim_end_matches(['.', ',', ')', ';', '\'']);
        if let Some((embed_type, id)) = crate::utils::sanitize::extract_video_embed(clean) {
            if embed_type == "youtube" {
                return Some(format!("https://img.youtube.com/vi/{id}/mqdefault.jpg"));
            }
        }
    }
    None
}

// ─── Pagination helper ────────────────────────────────────────────────────────

#[must_use]
pub fn render_pagination(p: &Pagination, base_url: &str) -> String {
    if p.total_pages() <= 1 {
        return String::new();
    }
    let sep = if base_url.contains('?') { "&" } else { "?" };
    // escape base_url once here so every href it appears in is safe,
    // regardless of what any caller passes.  All current callers pass trusted
    // values, but this makes the helper defensively correct for future callers.
    let safe_base = escape_html(base_url);
    let mut html = String::from(r#"<div class="pagination">"#);

    if p.has_prev() {
        let _ = write!(
            html,
            r#"<a href="{}{sep}page={}">[prev]</a> "#,
            safe_base,
            p.page.saturating_sub(1),
            sep = sep
        );
    }
    let _ = write!(html, "page {} / {}", p.page, p.total_pages());
    if p.has_next() {
        let _ = write!(
            html,
            r#" <a href="{}{sep}page={}">[next]</a>"#,
            safe_base,
            p.page.saturating_add(1),
            sep = sep
        );
    }

    html.push_str("</div>");
    html
}

// Encode each UTF-8 *byte*, not each Unicode codepoint.
// RFC 3986 percent-encoding operates on bytes.
#[must_use]
pub fn urlencoding_simple(s: &str) -> String {
    crate::utils::redirect::encode_form_query_component(s)
}

// ─── Base layout ─────────────────────────────────────────────────────────────

// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[expect(clippy::too_many_arguments)]
#[must_use]
pub fn base_layout(
    title: &str,
    board_short: Option<&str>,
    body: &str,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    board_default_theme: Option<&str>,
    collapse_greentext: bool,
    current_path: &str,
) -> String {
    base_layout_with_preferences(
        title,
        board_short,
        body,
        csrf_token,
        boards,
        current_theme,
        board_default_theme,
        collapse_greentext,
        current_path,
        UserPreferences::default(),
    )
}

#[must_use]
#[expect(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn base_layout_with_preferences(
    title: &str,
    board_short: Option<&str>,
    body: &str,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    board_default_theme: Option<&str>,
    collapse_greentext: bool,
    current_path: &str,
    preferences: UserPreferences,
) -> String {
    let (sfw_boards, nsfw_boards_all) = board_nav_groups(boards);
    let nsfw_boards = if preferences.hide_nsfw_boards {
        Vec::new()
    } else {
        nsfw_boards_all
    };
    let board_links = board_nav_html_for_preferences(boards, preferences);
    let board_menu = if boards.is_empty() {
        String::new()
    } else {
        let items = format!(
            "{}{}",
            mobile_board_group_html("Boards", &sfw_boards, preferences, false),
            mobile_board_group_html("NSFW", &nsfw_boards, preferences, true)
        );
        format!(
            r#"<details class="mobile-board-menu">
  <summary class="mobile-board-menu-btn">Boards</summary>
  <nav class="mobile-board-menu-panel">{items}</nav>
</details>"#
        )
    };

    let search_bar = board_short.map_or_else(String::new, |b| {
        format!(
            r#"<form class="search-form" method="GET" action="/{b}/search">
<input type="text" name="q" placeholder="search /{b}/…" maxlength="{max_len}">
<button type="submit">go</button>
</form>"#,
            b = escape_html(b),
            max_len = SEARCH_QUERY_MAX_CHARS
        )
    });

    let enabled_themes = live_themes();
    let enabled_theme_slugs = enabled_themes
        .iter()
        .filter(|theme| theme.enabled)
        .map(|theme| theme.slug.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let default_theme = resolve_page_default_theme(board_default_theme);
    let active_theme = current_theme
        .and_then(normalize_theme_slug)
        .unwrap_or_else(|| default_theme.clone());
    let default_theme_attr = format!(r#" data-default-theme="{}""#, escape_html(&default_theme));
    let theme_slugs_attr = format!(
        r#" data-theme-slugs="{}""#,
        escape_html(&enabled_theme_slugs)
    );
    let active_theme_attr = if active_theme == "terminal" {
        String::new()
    } else {
        format!(r#" data-theme="{}""#, escape_html(&active_theme))
    };
    let active_theme_value_attr = format!(r#" data-active-theme="{}""#, escape_html(&active_theme));
    let theme_href = |theme: &str| {
        format!(
            "/theme/{}?return_to={}",
            escape_html(theme),
            urlencoding_simple(current_path)
        )
    };
    let stylesheet_href = static_asset_url("/static/style.css");
    let admin_stylesheet_href = static_asset_url("/static/admin.css");
    let theme_init_src = static_asset_url("/static/theme-init.js");
    let main_js_src = static_asset_url("/static/main.js");
    let admin_js_src = static_asset_url("/static/admin.js");
    let is_admin_page = current_path.starts_with("/admin");
    let theme_stylesheet_href = if active_theme == "terminal" {
        String::new()
    } else {
        theme_css_href(&active_theme)
    };
    let theme_stylesheet_link = if active_theme == "terminal" {
        String::new()
    } else {
        format!(
            r#"<link rel="stylesheet" id="active-theme-stylesheet" href="{theme_stylesheet_href}">"#
        )
    };
    let admin_stylesheet_link = if is_admin_page {
        format!(r#"<link rel="stylesheet" href="{admin_stylesheet_href}">"#)
    } else {
        String::new()
    };
    let admin_script_tag = if is_admin_page {
        format!(r#"<script src="{admin_js_src}" defer></script>"#)
    } else {
        String::new()
    };
    let mut theme_picker_fallback = String::new();
    let mut theme_picker_panel = String::new();
    let mut theme_select_options = String::new();
    for theme in enabled_themes.iter().filter(|theme| theme.enabled) {
        let href = theme_href(&theme.slug);
        let selected_attr = if theme.slug == active_theme {
            " selected"
        } else {
            ""
        };
        let _ = write!(
            theme_select_options,
            r#"<option value="{slug}"{selected}>{label}</option>"#,
            slug = escape_html(&theme.slug),
            selected = selected_attr,
            label = escape_html(&theme.display_name),
        );
        let _ = write!(
            theme_picker_fallback,
            r#" <a href="{href}">{label}</a>"#,
            href = href,
            label = escape_html(&theme.display_name)
        );
        let _ = write!(
            theme_picker_panel,
            r#"<a class="tp-option" data-action="set-theme" data-theme="{slug}" href="{href}" title="{description}">
    <span class="tp-swatch" style="background:{swatch};"></span>{label}
  </a>"#,
            slug = escape_html(&theme.slug),
            href = href,
            description = escape_html(&theme.description),
            swatch = escape_html(&theme.swatch_hex),
            label = escape_html(&theme.display_name)
        );
    }
    let hide_nsfw_checked = if preferences.hide_nsfw_boards {
        " checked"
    } else {
        ""
    };
    let audio_on_checked = if preferences.video_audio_muted {
        ""
    } else {
        " checked"
    };
    let audio_muted_checked = if preferences.video_audio_muted {
        " checked"
    } else {
        ""
    };
    let catalog_checked = if preferences.preferred_board_view.is_catalog() {
        " checked"
    } else {
        ""
    };
    let index_checked = if preferences.preferred_board_view.is_catalog() {
        ""
    } else {
        " checked"
    };
    let badges_checked = if preferences.show_activity_badges {
        " checked"
    } else {
        ""
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en" class="no-js"{default_theme_attr}{theme_slugs_attr}{active_theme_value_attr}{active_theme_attr}>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>{title}</title>
{favicon_head}
<link rel="stylesheet" href="{stylesheet_href}">
{admin_stylesheet_link}
{theme_stylesheet_link}
<script src="{theme_init_src}"></script>
</head>
<body{collapse_attr}>
<header class="site-header">
  <span class="site-name">{forum_name}</span>
  <a class="home-btn" href="/">&#8962; Home</a>
  {board_menu}
  <nav class="board-list">
    {board_links}
  </nav>
  <div class="header-search">{search_bar}</div>
  <a class="admin-header-link" href="/admin">[Admin]</a>
</header>
<main>
{body}
</main>
<footer class="site-footer">
  <p class="site-footer-copy">{forum_name} &mdash; <a href="/">home</a></p>
  <div class="site-footer-theme">
    <nav class="theme-picker-fallback" aria-label="Theme selector">
      <span class="theme-picker-fallback-title">Theme:</span>
      {theme_picker_fallback}
    </nav>
    <details class="user-preferences-panel">
      <summary id="theme-picker-btn" class="user-preferences-summary">&#9881; User Preferences</summary>
      <form class="user-preferences-form" method="POST" action="/preferences">
        <button type="button" class="user-preferences-mobile-close" aria-label="Close preferences">&times;</button>
        <input type="hidden" name="preferences_form" value="1">
        <input type="hidden" name="_csrf" value="{csrf_token}">
        <input type="hidden" name="return_to" value="{current_path}">
        <label>Theme
          <select name="theme">{theme_select_options}</select>
        </label>
        <input type="hidden" name="hide_nsfw_boards_present" value="1">
        <label><input type="checkbox" name="hide_nsfw_boards" value="1"{hide_nsfw_checked}> Hide NSFW boards</label>
        <fieldset>
          <legend>Video audio by default</legend>
          <label><input type="radio" name="video_audio" value="on"{audio_on_checked}> On</label>
          <label><input type="radio" name="video_audio" value="mute"{audio_muted_checked}> Mute</label>
        </fieldset>
        <fieldset>
          <legend>Board links</legend>
          <label><input type="radio" name="preferred_board_view" value="catalog"{catalog_checked}> Prefer catalog</label>
          <label><input type="radio" name="preferred_board_view" value="index"{index_checked}> Prefer index</label>
        </fieldset>
        <input type="hidden" name="show_activity_badges_present" value="1">
        <label><input type="checkbox" name="show_activity_badges" value="1"{badges_checked}> Show new activity badges</label>
      </form>
    </details>
    <div id="theme-picker-panel">
      <div class="tp-title">// SELECT THEME</div>
      {theme_picker_panel}
    </div>
  </div>
</footer>

{confirmation_modal}
<input type="hidden" id="csrf_global" value="{csrf_token}">
<script src="{main_js_src}" defer></script>
{admin_script_tag}
</body>
</html>"#,
        title = escape_html(title),
        favicon_head = crate::favicon::favicon_head_html(board_short),
        stylesheet_href = stylesheet_href,
        admin_stylesheet_link = admin_stylesheet_link,
        theme_stylesheet_link = theme_stylesheet_link,
        theme_init_src = theme_init_src,
        board_links = board_links,
        search_bar = search_bar,
        board_menu = board_menu,
        forum_name = escape_html(&live_site_name()),
        body = body,
        confirmation_modal = confirmation_modal_script(),
        csrf_token = escape_html(csrf_token),
        main_js_src = main_js_src,
        admin_script_tag = admin_script_tag,
        default_theme_attr = default_theme_attr,
        theme_slugs_attr = theme_slugs_attr,
        active_theme_value_attr = active_theme_value_attr,
        active_theme_attr = active_theme_attr,
        theme_picker_fallback = theme_picker_fallback,
        theme_select_options = theme_select_options,
        theme_picker_panel = theme_picker_panel,
        current_path = escape_html(current_path),
        hide_nsfw_checked = hide_nsfw_checked,
        audio_on_checked = audio_on_checked,
        audio_muted_checked = audio_muted_checked,
        catalog_checked = catalog_checked,
        index_checked = index_checked,
        badges_checked = badges_checked,
        collapse_attr = if collapse_greentext {
            " data-collapse-greentext=\"1\""
        } else {
            ""
        },
    )
}

// ─── Standalone error/ban pages (no board context) ────────────────────────────

// ban_page must accept a csrf_token so the appeal form works.
// Previously the field was always empty and every appeal was rejected by the
// server's CSRF check, making the appeal feature completely non-functional.
#[must_use]
pub fn ban_page(reason: &str, csrf_token: &str) -> String {
    let enabled_theme_slugs = live_themes()
        .iter()
        .filter(|theme| theme.enabled)
        .map(|theme| theme.slug.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let configured_default = resolve_page_default_theme(None);
    let default_theme_attr = format!(
        r#" data-default-theme="{}""#,
        escape_html(&configured_default)
    );
    let theme_slugs_attr = format!(
        r#" data-theme-slugs="{}""#,
        escape_html(&enabled_theme_slugs)
    );
    let active_theme_attr = if configured_default == "terminal" {
        String::new()
    } else {
        format!(r#" data-theme="{}""#, escape_html(&configured_default))
    };
    let stylesheet_href = static_asset_url("/static/style.css");
    let theme_init_src = static_asset_url("/static/theme-init.js");
    let main_js_src = static_asset_url("/static/main.js");
    let theme_stylesheet_link = if configured_default == "terminal" {
        String::new()
    } else {
        format!(
            r#"<link rel="stylesheet" id="active-theme-stylesheet" href="{}">"#,
            theme_css_href(&configured_default)
        )
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en" class="no-js"{default_theme_attr}{theme_slugs_attr}{active_theme_attr}>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>You Are Banned</title>
<link rel="stylesheet" href="{stylesheet_href}">
{theme_stylesheet_link}
<script src="{theme_init_src}"></script>
</head>
<body>
<div class="page-box error-page">
<h1>you are banned</h1>
<p style="color:var(--text-dim)">reason: <strong>{reason}</strong></p>
<p style="margin-top:1.5rem;font-size:0.9rem">if you believe this ban was made in error, you may submit an appeal below.<br>
appeals are reviewed by site staff. one appeal per 24 hours.</p>
<form method="POST" action="/appeal" class="appeal-form">
<input type="hidden" name="_csrf" id="appeal-csrf-field" value="{csrf}">
<textarea name="reason" rows="4" maxlength="512"
  placeholder="Briefly explain why you believe this ban should be lifted…"
  style="width:100%;box-sizing:border-box;margin:0.75rem 0;background:var(--bg-post);color:var(--text);border:1px solid var(--border);padding:0.5rem;resize:vertical"></textarea>
<button type="submit" style="margin-top:0.25rem">submit appeal</button>
</form>
<p style="margin-top:1.5rem"><a href="/">return home</a></p>
</div>
<!-- Global CSRF token consumed by main.js for fetch-based requests -->
<input type="hidden" id="csrf_global" value="{csrf}">
<script src="{main_js_src}" defer></script>
</body>
</html>"#,
        default_theme_attr = default_theme_attr,
        theme_slugs_attr = theme_slugs_attr,
        active_theme_attr = active_theme_attr,
        stylesheet_href = stylesheet_href,
        theme_stylesheet_link = theme_stylesheet_link,
        theme_init_src = theme_init_src,
        reason = escape_html(reason),
        csrf = escape_html(csrf_token),
        main_js_src = main_js_src,
    )
}

#[must_use]
pub fn error_page(code: u16, message: &str) -> String {
    // Use base_layout so the error page has the same header, theme picker,
    // and board navigation as every other page.  live_boards() is always
    // up-to-date because every board mutation refreshes the cache.
    let boards = live_boards();
    let body = format!(
        r#"<div class="page-box error-page">
<h1>error {code}</h1>
<p>{message}</p>
<p><a href="/">return home</a></p>
</div>"#,
        code = code,
        message = escape_html(message),
    );
    base_layout(
        &format!("Error {code}"),
        None,
        &body,
        "",
        &boards,
        None,
        None,
        false,
        "/",
    )
}

#[cfg(test)]
mod tests {
    use super::{
        base_layout, base_layout_with_preferences, fmt_ts, fmt_ts_short, set_live_default_theme,
        set_live_themes, PreferredBoardView, UserPreferences,
    };
    use crate::models::{Board, Theme};

    fn builtin_theme(slug: &str, display_name: &str, sort_order: i64) -> Theme {
        Theme {
            slug: slug.to_owned(),
            display_name: display_name.to_owned(),
            description: format!("{display_name} description"),
            swatch_hex: "#123456".to_owned(),
            enabled: true,
            sort_order,
            is_builtin: true,
            custom_css: String::new(),
        }
    }

    #[test]
    fn base_layout_uses_forest_default_and_featured_theme_order() {
        set_live_default_theme("forest");
        set_live_themes(vec![
            builtin_theme("forest", "Forest", 10),
            builtin_theme("blue-sky", "Blue Sky", 20),
            builtin_theme("deep-orbit", "Deep Orbit", 30),
            builtin_theme("terminal", "Terminal", 40),
            builtin_theme("dorfic", "DORFic", 50),
        ]);

        let html = base_layout("Home", None, "<p>body</p>", "", &[], None, None, false, "/");

        assert!(html.contains(r#"data-default-theme="forest""#));

        let forest_idx = html.find(">Forest</a>").expect("forest option");
        let blue_sky_idx = html.find(">Blue Sky</a>").expect("blue sky option");
        let deep_orbit_idx = html.find(">Deep Orbit</a>").expect("deep orbit option");
        let terminal_idx = html.find(">Terminal</a>").expect("terminal option");
        let dorfic_idx = html.find(">DORFic</a>").expect("dorfic option");

        assert!(forest_idx < blue_sky_idx);
        assert!(blue_sky_idx < deep_orbit_idx);
        assert!(deep_orbit_idx < terminal_idx);
        assert!(terminal_idx < dorfic_idx);
    }

    #[test]
    fn base_layout_preferences_are_plain_html_and_selected() {
        set_live_default_theme("forest");
        set_live_themes(vec![
            builtin_theme("forest", "Forest", 10),
            builtin_theme("blue-sky", "Blue Sky", 20),
        ]);
        let preferences = UserPreferences {
            hide_nsfw_boards: true,
            video_audio_muted: true,
            preferred_board_view: PreferredBoardView::Index,
            show_activity_badges: false,
        };

        let html = base_layout_with_preferences(
            "Home",
            None,
            "<p>body</p>",
            "csrf",
            &[],
            Some("blue-sky"),
            None,
            false,
            "/",
            preferences,
        );

        assert!(html.contains(r#"<details class="user-preferences-panel">"#));
        assert!(html.contains(r#"method="POST" action="/preferences""#));
        assert!(html.contains(r#"data-active-theme="blue-sky""#));
        assert!(html.contains(r#"name="_csrf" value="csrf""#));
        assert!(html.contains(r#"name="preferences_form" value="1""#));
        assert!(html.contains(r#"class="user-preferences-mobile-close""#));
        assert!(html.contains(r#"aria-label="Close preferences""#));
        assert!(html.contains("User Preferences"));
        assert!(html.contains(r#"<option value="blue-sky" selected>Blue Sky</option>"#));
        assert!(html.contains(r#"name="hide_nsfw_boards_present" value="1""#));
        assert!(html.contains(r#"name="hide_nsfw_boards" value="1" checked"#));
        assert!(html.contains(r#"name="video_audio" value="mute" checked"#));
        assert!(html.contains(r#"name="preferred_board_view" value="index" checked"#));
        assert!(html.contains(r#"name="show_activity_badges_present" value="1""#));
        assert!(!html.contains(r#"name="show_activity_badges" value="1" checked"#));
        assert!(!html.contains("Save preferences"));
        assert!(!html.contains(r#"type="submit">Save preferences"#));
    }

    #[test]
    fn base_layout_hides_nsfw_nav_and_uses_index_links_when_requested() {
        let sfw = Board {
            short_name: "tech".into(),
            nsfw: false,
            ..crate::test_fixtures::sample_board()
        };
        let nsfw = Board {
            id: 2,
            short_name: "x".into(),
            nsfw: true,
            ..crate::test_fixtures::sample_board()
        };
        let preferences = UserPreferences {
            hide_nsfw_boards: true,
            preferred_board_view: PreferredBoardView::Index,
            ..UserPreferences::default()
        };

        let html = base_layout_with_preferences(
            "Home",
            None,
            "<p>body</p>",
            "csrf",
            &[sfw, nsfw],
            None,
            None,
            false,
            "/",
            preferences,
        );

        assert!(html.contains(r#"<a href="/tech">tech</a>"#));
        assert!(!html.contains(r#"<a href="/tech/catalog">tech</a>"#));
        assert!(!html.contains(r">x</a>"));
    }

    #[test]
    fn base_layout_marks_nsfw_nav_groups_for_client_preference_toggling() {
        let sfw = Board {
            short_name: "tech".into(),
            nsfw: false,
            ..crate::test_fixtures::sample_board()
        };
        let nsfw = Board {
            id: 2,
            short_name: "x".into(),
            nsfw: true,
            ..crate::test_fixtures::sample_board()
        };

        let html = base_layout_with_preferences(
            "Home",
            None,
            "<p>body</p>",
            "csrf",
            &[sfw, nsfw],
            Some("forest"),
            None,
            false,
            "/",
            UserPreferences::default(),
        );

        assert!(html.contains(r#"<span class="board-list-group" data-board-nsfw="1">[ <a href="/x/catalog">x</a> ]</span>"#));
        assert!(html.contains(r#"<div class="mobile-board-group" data-board-nsfw="1"><div class="mobile-board-group-title">NSFW</div><a class="mobile-board-link" href="/x/catalog">/x/</a></div>"#));
    }

    #[test]
    fn timestamp_helpers_do_not_force_utc_suffix() {
        let full = fmt_ts(1_700_000_000);
        let short = fmt_ts_short(1_700_000_000);

        assert!(!full.contains("UTC"));
        assert!(!short.contains("UTC"));
        assert_ne!(full, "unknown");
        assert_ne!(short, "?");
    }

    #[test]
    fn timestamp_helpers_handle_out_of_range_epoch_values() {
        assert_eq!(fmt_ts(i64::MAX), "unknown");
        assert_eq!(fmt_ts_short(i64::MAX), "?");
    }
}
