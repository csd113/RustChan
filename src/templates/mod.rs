// Shared HTML rendering helpers and page fragments.

use crate::config::CONFIG;
use crate::models::{Board, Pagination, Theme, SEARCH_QUERY_MAX_CHARS};
use crate::utils::sanitize::escape_html;
use chrono::{Local, TimeZone};
use parking_lot::RwLock;
use std::fmt::Write;
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
            || env!("CARGO_PKG_VERSION").to_string(),
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

pub fn live_board_nav() -> (u64, Arc<str>) {
    let guard = LIVE_BOARD_NAV.read();
    (guard.0, Arc::clone(&guard.1))
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
        .unwrap_or_else(|| crate::theme::HARD_DEFAULT_THEME.to_string())
}

fn resolve_page_default_theme(board_default_theme: Option<&str>) -> String {
    board_default_theme
        .and_then(normalize_theme_slug)
        .or_else(|| normalize_theme_slug(&live_default_theme()))
        .unwrap_or_else(fallback_theme_slug)
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

fn board_nav_group_html(boards: &[&Board]) -> Option<String> {
    if boards.is_empty() {
        return None;
    }
    let inner = boards
        .iter()
        .map(|board| {
            format!(
                r#"<a href="/{short}/catalog">{short}</a>"#,
                short = escape_html(&board.short_name)
            )
        })
        .collect::<Vec<_>>()
        .join(" / ");
    Some(format!(
        r#"<span class="board-list-group">[ {inner} ]</span>"#
    ))
}

fn mobile_board_group_html(title: &str, boards: &[&Board]) -> String {
    if boards.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    for board in boards {
        let short = escape_html(&board.short_name);
        let _ = write!(
            items,
            r#"<a class="mobile-board-link" href="/{short}/catalog">/{short}/</a>"#
        );
    }
    format!(
        r#"<div class="mobile-board-group"><div class="mobile-board-group-title">{title}</div>{items}</div>"#
    )
}

fn rebuild_live_board_nav() {
    let boards = live_boards_snapshot();
    let (sfw, nsfw) = board_nav_groups(boards.as_slice());
    let nav_html = [board_nav_group_html(&sfw), board_nav_group_html(&nsfw)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let nav_html: Arc<str> = if nav_html.is_empty() {
        Arc::from("")
    } else {
        Arc::from(nav_html)
    };
    let version = live_boards_version();
    *LIVE_BOARD_NAV.write() = (version, nav_html);
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
        _ => "unknown".to_string(),
    }
}

#[must_use]
pub fn fmt_ts_short(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%m/%d/%y(%a)%H:%M:%S").to_string(),
        _ => "?".to_string(),
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

#[allow(clippy::too_many_lines)]
// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
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
    let (sfw_boards, nsfw_boards) = board_nav_groups(boards);
    let board_links = [
        board_nav_group_html(&sfw_boards),
        board_nav_group_html(&nsfw_boards),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let board_menu = if boards.is_empty() {
        String::new()
    } else {
        let items = format!(
            "{}{}",
            mobile_board_group_html("Boards", &sfw_boards),
            mobile_board_group_html("NSFW", &nsfw_boards)
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
    for theme in enabled_themes.iter().filter(|theme| theme.enabled) {
        let href = theme_href(&theme.slug);
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

    format!(
        r#"<!DOCTYPE html>
<html lang="en" class="no-js"{default_theme_attr}{theme_slugs_attr}{active_theme_attr}>
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
    <button id="theme-picker-btn" data-action="toggle-theme-picker" title="Select Theme">&#127912; Theme</button>
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
        active_theme_attr = active_theme_attr,
        theme_picker_fallback = theme_picker_fallback,
        theme_picker_panel = theme_picker_panel,
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
    use super::{base_layout, fmt_ts, fmt_ts_short, set_live_default_theme, set_live_themes};
    use crate::models::Theme;

    fn builtin_theme(slug: &str, display_name: &str, sort_order: i64) -> Theme {
        Theme {
            slug: slug.to_string(),
            display_name: display_name.to_string(),
            description: format!("{display_name} description"),
            swatch_hex: "#123456".to_string(),
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
