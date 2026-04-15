#![allow(clippy::redundant_pub_crate)]

// templates/board.rs
//
// Page templates for board-level views:
//   index_page       — site home (list of all boards)
//   board_page       — board thread index with pagination
//   catalog_page     — grid catalog view
//   archive_page     — archived threads list
//   search_page      — search results

use crate::models::{Board, Pagination, Post, Thread, ThreadSummary, SEARCH_QUERY_MAX_CHARS};
use crate::utils::sanitize::escape_html;
use std::collections::HashSet;
use std::fmt::Write;

use super::{
    base_layout, compress_modal_script, embed_thumb_from_body, fmt_ts, fmt_ts_short,
    live_site_name, live_site_subtitle, render_pagination, report_modal_script, urlencoding_simple,
    TOGGLE_SCRIPT,
};

// ─── Site index (board list) ──────────────────────────────────────────────────

fn board_reorder_controls(
    board: &Board,
    csrf_token: &str,
    return_to: &str,
    is_first: bool,
    is_last: bool,
) -> String {
    format!(
        r#"<details class="board-reorder-menu">
  <summary class="board-reorder-toggle" aria-label="Reorder /{short}/">&#8645;</summary>
  <div class="board-reorder-controls">
    <form method="POST" action="/admin/board/reorder">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="board_id" value="{board_id}">
      <input type="hidden" name="direction" value="up">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit"{up_disabled} aria-label="Move /{short}/ earlier">&#8593;</button>
    </form>
    <form method="POST" action="/admin/board/reorder">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="board_id" value="{board_id}">
      <input type="hidden" name="direction" value="down">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit"{down_disabled} aria-label="Move /{short}/ later">&#8595;</button>
    </form>
  </div>
</details>"#,
        csrf = escape_html(csrf_token),
        board_id = board.id,
        return_to = escape_html(return_to),
        short = escape_html(&board.short_name),
        up_disabled = if is_first { " disabled" } else { "" },
        down_disabled = if is_last { " disabled" } else { "" },
    )
}

#[allow(clippy::fn_params_excessive_bools)]
fn render_board_card(
    stats: &crate::models::BoardStats,
    nsfw_consent: bool,
    csrf_token: &str,
    show_reorder_controls: bool,
    is_first: bool,
    is_last: bool,
) -> String {
    let board = &stats.board;
    let description_preview = preview_text(&board.description, 88);
    let nsfw_badge = if board.nsfw {
        r#"<span class="nsfw-badge">NSFW</span>"#
    } else {
        ""
    };
    let board_href = if board.access_mode.requires_view_password() {
        format!("/{}/unlock", escape_html(&board.short_name))
    } else {
        format!("/{}/catalog", escape_html(&board.short_name))
    };
    let href = if board.nsfw && !nsfw_consent {
        format!("/?nsfw={}", urlencoding_simple(&board.short_name))
    } else {
        board_href
    };
    let action_attr = if board.nsfw && !nsfw_consent {
        " data-action=\"open-nsfw-disclaimer\""
    } else {
        ""
    };
    let return_to_attr = if board.nsfw {
        format!(
            r#" data-return-to="/{}" data-board-label="/{}/""#,
            if board.access_mode.requires_view_password() {
                format!("{}/unlock", escape_html(&board.short_name))
            } else {
                format!("{}/catalog", escape_html(&board.short_name))
            },
            escape_html(&board.short_name)
        )
    } else {
        String::new()
    };
    let thread_word = if stats.thread_count == 1 {
        "thread"
    } else {
        "threads"
    };
    let reorder_controls = if show_reorder_controls {
        board_reorder_controls(board, csrf_token, "/", is_first, is_last)
    } else {
        String::new()
    };
    let access_badge = board_access_badge(board);

    format!(
        r#"<div class="board-card">
  {reorder_controls}
  <a class="board-card-link" href="{href}"{action_attr}{return_to_attr}>
    <div class="board-card-short">/{sh}/{nsfw}{access_badge}</div>
    <div class="board-card-name">{name}</div>
    <div class="board-card-desc">{description}</div>
    <div class="board-card-stats">{thread_count} {thread_word}</div>
  </a>
</div>"#,
        reorder_controls = reorder_controls,
        href = href,
        action_attr = action_attr,
        return_to_attr = return_to_attr,
        sh = escape_html(&board.short_name),
        name = escape_html(&board.name),
        nsfw = nsfw_badge,
        access_badge = access_badge,
        description = escape_html(&description_preview),
        thread_count = stats.thread_count,
        thread_word = thread_word,
    )
}

pub(crate) fn board_access_badge(board: &Board) -> String {
    match board.access_mode {
        crate::models::BoardAccessMode::Public => String::new(),
        crate::models::BoardAccessMode::ViewPassword => {
            r#" <span class="tag locked">PASSWORD</span>"#.to_string()
        }
        crate::models::BoardAccessMode::PostPassword => {
            r#" <span class="tag sticky">POST PASSWORD</span>"#.to_string()
        }
    }
}

const fn board_access_copy(board: &Board) -> (&'static str, &'static str, &'static str) {
    match board.access_mode {
        crate::models::BoardAccessMode::Public => (
            "public board",
            "This board does not require a password.",
            "continue",
        ),
        crate::models::BoardAccessMode::ViewPassword => (
            "password protected board",
            "You need the board password to view this board, its threads, search, archive, and media.",
            "unlock board",
        ),
        crate::models::BoardAccessMode::PostPassword => (
            "posting is password protected",
            "Viewing is public, but creating threads and replies on this board requires the board password.",
            "unlock posting",
        ),
    }
}

pub(crate) fn render_post_access_gate(
    board: &Board,
    csrf_token: &str,
    return_to: &str,
    title: &str,
) -> String {
    let (_, description, button_label) = board_access_copy(board);
    format!(
        r#"<div class="post-form-container board-access-gate" id="board-access-gate">
<div class="post-form-title">[ {title} ]</div>
<form class="post-form" method="POST" action="/{board}/unlock">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="return_to" value="{return_to}">
  <table>
    <tr><td>status</td>
        <td><span style="font-size:0.8rem;color:var(--text-dim)">{description}</span></td></tr>
    <tr><td>password</td>
        <td><input type="password" name="password" maxlength="256" autocomplete="current-password">
            <button type="submit">{button_label}</button></td></tr>
  </table>
</form>
</div>"#,
        title = escape_html(title),
        board = escape_html(&board.short_name),
        csrf = escape_html(csrf_token),
        return_to = escape_html(return_to),
        description = escape_html(description),
        button_label = escape_html(button_label),
    )
}

#[must_use]
pub fn board_access_page(
    board: &Board,
    csrf_token: &str,
    boards: &[Board],
    return_to: &str,
    error: Option<&str>,
    current_theme: Option<&str>,
    collapse_greentext: bool,
) -> String {
    let (eyebrow, description, button_label) = board_access_copy(board);
    let error_html = error.map_or_else(String::new, |message| {
        format!(
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(message)
        )
    });
    let board_description = if board.description.trim().is_empty() {
        String::new()
    } else {
        format!(
            r#"<p class="board-desc" style="margin-top:0.75rem">{}</p>"#,
            escape_html(&board.description)
        )
    };
    let body = format!(
        r#"{error_html}<div class="page-box board-access-page">
<h1>/{short}/ — {name}{badge}</h1>
<p style="color:var(--text-dim)">{eyebrow}</p>
<p>{description}</p>
{board_description}
<form method="POST" action="/{short}/unlock" class="board-access-form" style="margin-top:1rem">
  <input type="hidden" name="_csrf" value="{csrf}">
  <input type="hidden" name="return_to" value="{return_to}">
  <table class="admin-login-table">
    <tr><td>password</td><td><input type="password" name="password" maxlength="256" autocomplete="current-password" autofocus required></td></tr>
    <tr><td></td><td><button type="submit">{button_label}</button></td></tr>
  </table>
</form>
<p style="margin-top:1rem"><a href="/">return home</a></p>
</div>"#,
        error_html = error_html,
        short = escape_html(&board.short_name),
        name = escape_html(&board.name),
        badge = board_access_badge(board),
        eyebrow = escape_html(eyebrow),
        description = escape_html(description),
        board_description = board_description,
        csrf = escape_html(csrf_token),
        return_to = escape_html(return_to),
        button_label = escape_html(button_label),
    );

    base_layout(
        &format!("/{}/ access", board.short_name),
        None,
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!("/{}/unlock", board.short_name),
    )
}

fn preview_text(input: &str, max_chars: usize) -> String {
    let mut preview = String::new();
    let mut chars = input.chars();

    for ch in chars.by_ref().take(max_chars) {
        preview.push(ch);
    }

    if chars.next().is_some() {
        preview.push_str("...");
    }

    preview
}

fn render_catalog_thumb(thread: &Thread) -> String {
    let badges = super::thread::render_thread_state_badges(thread.sticky, thread.locked);
    let media = thread.op_thumb.as_ref().map_or_else(
        || {
            thread
                .op_body
                .as_deref()
                .and_then(embed_thumb_from_body)
                .map_or_else(
                    || r#"<div class="catalog-no-image">no img</div>"#.to_string(),
                    |embed_thumb| {
                        format!(
                            r#"<img class="catalog-thumb embed-catalog-thumb" src="{}" loading="lazy" alt="video thumbnail">"#,
                            escape_html(&embed_thumb)
                        )
                    },
                )
        },
        |thumb| {
            format!(
                r#"<img class="catalog-thumb" src="/boards/{}" loading="lazy" alt="">"#,
                escape_html(thumb)
            )
        },
    );

    format!(r#"<div class="catalog-card-media">{media}{badges}</div>"#)
}

#[allow(clippy::too_many_arguments)]
fn render_catalog_actions(
    board_short: &str,
    thread: &Thread,
    csrf_token: &str,
    pin_action: &str,
    pin_label: &str,
    hide_action: &str,
    hide_label: &str,
    return_to: &str,
) -> String {
    format!(
        r#"<div class="catalog-card-actions">
  <button type="button" class="catalog-thread-menu-toggle" data-action="toggle-thread-menu" aria-haspopup="true" aria-expanded="false" aria-label="Thread actions"></button>
  <div class="catalog-thread-menu" hidden>
    <button type="button" class="catalog-thread-menu-item" data-action="open-report" data-pid="{post_id}" data-tid="{thread_id}" data-board="{board}" data-csrf="{csrf}" data-report-label="Reporting thread No.{thread_id}">Report thread</button>
    <form method="POST" action="/{board}/thread-preference">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="thread_id" value="{thread_id}">
      <input type="hidden" name="board" value="{board}">
      <input type="hidden" name="action" value="{pin_action}">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit" class="catalog-thread-menu-item">{pin_label}</button>
    </form>
    <form method="POST" action="/{board}/thread-preference">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="thread_id" value="{thread_id}">
      <input type="hidden" name="board" value="{board}">
      <input type="hidden" name="action" value="{hide_action}">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit" class="catalog-thread-menu-item">{hide_label}</button>
    </form>
  </div>
</div>"#,
        post_id = thread.op_id.unwrap_or(thread.id),
        thread_id = thread.id,
        board = escape_html(board_short),
        csrf = escape_html(csrf_token),
        pin_action = pin_action,
        pin_label = pin_label,
        hide_action = hide_action,
        hide_label = hide_label,
        return_to = escape_html(return_to),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_catalog_card(
    board: &Board,
    thread: &Thread,
    is_pinned: bool,
    csrf_token: &str,
    pin_action: &str,
    pin_label: &str,
    hide_action: &str,
    hide_label: &str,
    return_to: &str,
) -> String {
    let subject_preview: String = thread
        .subject
        .as_deref()
        .map_or_else(String::new, |subject| preview_text(subject, 44));
    let comment_preview: String = thread
        .op_body
        .as_deref()
        .map_or_else(String::new, |body| preview_text(body, 88));
    let subject_html = if subject_preview.is_empty() {
        String::new()
    } else {
        format!(
            r#"<p class="catalog-subject">{}</p>"#,
            escape_html(&subject_preview)
        )
    };
    let comment_html = if comment_preview.is_empty() {
        String::new()
    } else {
        format!(
            r#"<p class="catalog-comment">{}</p>"#,
            escape_html(&comment_preview)
        )
    };
    let actions_html = render_catalog_actions(
        &board.short_name,
        thread,
        csrf_token,
        pin_action,
        pin_label,
        hide_action,
        hide_label,
        return_to,
    );

    format!(
        r#"<div class="catalog-item{sticky}{pinned_class}" data-replies="{replies}" data-created="{created}" data-bumped="{bumped}" data-sticky="{is_sticky}" data-pinned="{is_pinned}">
<a class="catalog-card-link" href="/{board}/thread/{thread_id}">
  {thumb}
</a>
<div class="catalog-meta-row">
  <span class="catalog-replies">R: {replies} / F: {images}</span>
  {actions}
</div>
<a class="catalog-card-link" href="/{board}/thread/{thread_id}">
  <div class="catalog-info">
    {subject}
    {comment}
  </div>
</a>
</div>"#,
        sticky = if thread.sticky { " sticky" } else { "" },
        pinned_class = if is_pinned { " is-pinned" } else { "" },
        replies = thread.reply_count,
        created = thread.created_at,
        bumped = thread.bumped_at,
        is_sticky = if thread.sticky { "1" } else { "0" },
        is_pinned = if is_pinned { "1" } else { "0" },
        board = escape_html(&board.short_name),
        thread_id = thread.id,
        thumb = render_catalog_thumb(thread),
        images = thread.image_count,
        actions = actions_html,
        subject = subject_html,
        comment = comment_html,
    )
}

fn render_archive_row(board_short: &str, thread: &Thread) -> String {
    let preview: String = thread
        .op_body
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    let subject_html = thread.subject.as_ref().map_or_else(String::new, |subject| {
        format!(
            r#"<span class="archive-thread-subj">{}</span> - "#,
            escape_html(subject)
        )
    });
    let thumb_html = thread.op_thumb.as_ref().map_or_else(String::new, |thumb| {
        format!(
            r#"<div class="archive-row-media"><img src="/boards/{}" class="archive-thumb" alt="thumb" loading="lazy"></div>"#,
            escape_html(thumb),
        )
    });
    let thread_state_badges = super::thread::render_archive_state_badges(thread.sticky);

    format!(
        r#"<a href="/{board}/thread/{thread_id}" class="archive-row archive-thread-link">
  {thumb}
  <div class="archive-row-info">
    <span class="archive-thread-link-text">
      {subject}<span class="archive-preview">{preview}</span>
    </span>
    <span class="archive-meta">No.{thread_id}{state_badges} - {replies} replies - {created_at}</span>
  </div>
</a>"#,
        board = escape_html(board_short),
        thread_id = thread.id,
        thumb = thumb_html,
        subject = subject_html,
        preview = escape_html(&preview),
        state_badges = thread_state_badges,
        replies = thread.reply_count,
        created_at = fmt_ts(thread.created_at),
    )
}

fn board_cards(
    list: &[&crate::models::BoardStats],
    nsfw_consent: bool,
    csrf_token: &str,
    show_reorder_controls: bool,
) -> String {
    let mut out = String::new();
    for (index, s) in list.iter().enumerate() {
        out.push_str(&render_board_card(
            s,
            nsfw_consent,
            csrf_token,
            show_reorder_controls,
            index == 0,
            index + 1 == list.len(),
        ));
    }
    out
}

#[must_use]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
pub fn index_page(
    board_stats: &[crate::models::BoardStats],
    site_stats: Option<&crate::models::SiteStats>,
    csrf_token: &str,
    onion_address: Option<&str>,
    home_banner_html: &str,
    current_theme: Option<&str>,
    nsfw_prompt_board: Option<&Board>,
    nsfw_consent: bool,
    is_admin: bool,
) -> String {
    let all_boards: Vec<Board> = board_stats.iter().map(|s| s.board.clone()).collect();

    let sfw: Vec<&crate::models::BoardStats> =
        board_stats.iter().filter(|s| !s.board.nsfw).collect();
    let nsfw: Vec<&crate::models::BoardStats> =
        board_stats.iter().filter(|s| s.board.nsfw).collect();

    let sfw_sec = if sfw.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"index-section\"><h2 class=\"index-section-title\">// Boards</h2><div class=\"board-cards\">{}</div></div>",
            board_cards(&sfw, nsfw_consent, csrf_token, is_admin)
        )
    };

    let nsfw_sec = if nsfw.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"index-section\"><h2 class=\"index-section-title\">// Adult Boards <span class=\"nsfw-badge\">NSFW</span></h2><div class=\"board-cards\">{}</div></div>",
            board_cards(&nsfw, nsfw_consent, csrf_token, is_admin)
        )
    };

    let empty = if board_stats.is_empty() {
        "<p class=\"index-empty\">no boards yet — admin must create boards first.</p>"
    } else {
        ""
    };

    let stats_sec = site_stats.map_or_else(
        || {
            r#"<div class="index-section index-stats-section">
<h2 class="index-section-title">// Stats</h2>
<p class="index-stats-unavailable">site statistics are temporarily unavailable.</p>
</div>"#
                .to_string()
        },
        |site_stats| {
            #[allow(clippy::cast_precision_loss)]
            let active_gb = site_stats.active_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            format!(
                r#"<div class="index-section index-stats-section">
<h2 class="index-section-title">// Stats</h2>
<div class="index-stats-grid">
  <div class="index-stat"><span class="index-stat-value">{tp}</span><span class="index-stat-label">total posts</span></div>
  <div class="index-stat"><span class="index-stat-value">{ti}</span><span class="index-stat-label">images uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{tv}</span><span class="index-stat-label">videos uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{ta}</span><span class="index-stat-label">audio files uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{gb:.2} GB</span><span class="index-stat-label">active content</span></div>
</div>
</div>"#,
                tp = site_stats.total_posts,
                ti = site_stats.total_images,
                tv = site_stats.total_videos,
                ta = site_stats.total_audio,
                gb = active_gb,
            )
        },
    );

    let mut access_links = String::new();
    if let Some(addr) = onion_address {
        let _ = write!(
            access_links,
            r#"<p class="index-onion"><code class="onion-addr">{}</code></p>"#,
            escape_html(addr)
        );
    }
    let onion_html = if access_links.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="index-section index-onion-section">
{access_links}
</div>"#
        )
    };

    let nsfw_overlay = if nsfw.is_empty() {
        String::new()
    } else {
        let open_class = if nsfw_prompt_board.is_some() {
            " is-open"
        } else {
            ""
        };
        let hidden_attr = if nsfw_prompt_board.is_some() {
            ""
        } else {
            " hidden"
        };
        let board_label = nsfw_prompt_board
            .map(|b| format!("/{}/", escape_html(&b.short_name)))
            .unwrap_or_default();
        let return_to = nsfw_prompt_board
            .map(|b| {
                if b.access_mode.requires_view_password() {
                    format!("/{}/unlock", escape_html(&b.short_name))
                } else {
                    format!("/{}/catalog", escape_html(&b.short_name))
                }
            })
            .unwrap_or_default();
        format!(
            r#"<div id="nsfw-disclaimer-overlay" class="compress-modal nsfw-disclaimer-overlay{open_class}"{hidden_attr}>
  <div class="compress-modal-box nsfw-disclaimer-box">
    <div class="compress-modal-title">Disclaimer</div>
    <div class="compress-modal-info">
      <p class="nsfw-disclaimer-intro">To access this section, you understand and agree to the following:</p>
      <ol class="nsfw-disclaimer-list">
        <li>The content of this website is for mature audiences only and may not be suitable for minors. If you are a minor or it is illegal for you to access mature images and language, do not proceed.</li>
        <li>This website is presented to you AS IS, with no warranty, express or implied. By clicking &quot;I Agree,&quot; you agree not to hold this website responsible for any damages from your use of the platform, and you understand that the content posted is not owned or generated by the website, but rather by its users.</li>
        <li>As a condition of using this website, you agree to comply with the &quot;Rules&quot; of the threads you access.</li>
      </ol>
    </div>
    <div class="compress-modal-actions">
      <form method="POST" action="/nsfw/accept" class="nsfw-disclaimer-form">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" id="nsfw-return-to" name="return_to" value="{return_to}">
        <button type="submit" class="compress-do-btn">I Agree</button>
      </form>
      <a class="compress-cancel-btn btn" href="/" data-action="close-nsfw-disclaimer">Cancel</a>
    </div>
    <div id="nsfw-board-label" class="nsfw-disclaimer-board">{board_label}</div>
  </div>
</div>"#,
            open_class = open_class,
            hidden_attr = hidden_attr,
            csrf = escape_html(csrf_token),
            return_to = return_to,
            board_label = board_label,
        )
    };

    let body = format!(
        r#"<div class="index-hero">
<h1 class="index-title">[ {name} ]</h1>
<p class="index-subtitle">{subtitle}</p>
</div>
{home_banner_html}
{sfw}{nsfw}{empty}{stats}{onion}{nsfw_overlay}"#,
        name = escape_html(&live_site_name()),
        subtitle = escape_html(&live_site_subtitle()),
        home_banner_html = home_banner_html,
        sfw = sfw_sec,
        nsfw = nsfw_sec,
        empty = empty,
        stats = stats_sec,
        onion = onion_html,
        nsfw_overlay = nsfw_overlay,
    );

    base_layout(
        &live_site_name(),
        None,
        &body,
        csrf_token,
        &all_boards,
        current_theme,
        None,
        false,
        "/",
    )
}

// ─── Board index ──────────────────────────────────────────────────────────────

#[must_use]
#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub fn board_page(
    board: &Board,
    summaries: &[ThreadSummary],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    error: Option<&str>,
    new_thread_prefill: Option<&super::forms::PostFormState>,
    board_banner_html: &str,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
) -> String {
    let mut body = String::new();

    if let Some(msg) = error {
        let _ = write!(
            body,
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(msg)
        );
    }

    if is_admin {
        let _ = write!(
            body,
            r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
            csrf = escape_html(csrf_token),
            board = escape_html(&board.short_name)
        );
    }

    {
        let short = escape_html(&board.short_name);
        let name = escape_html(&board.name);
        let desc = escape_html(&board.description);
        let access_badge = board_access_badge(board);
        let nav_archive = if board.allow_archive {
            format!(r#"<a class="board-nav-link" href="/{short}/archive">[Archive]</a>"#)
        } else {
            String::new()
        };
        let _ = write!(
            body,
            r#"<div class="board-header board-index-header"><h1>/{short}/  — {name}{access_badge}</h1><p class="board-desc">{desc}</p></div>
{board_banner_html}
<div class="board-nav"><a class="board-nav-link active" href="/{short}">[Index]</a><a class="board-nav-link" href="/{short}/catalog">[Catalog]</a>{nav_archive}</div>"#
        );
    }

    if can_post {
        let show_post_form = error.is_some() || new_thread_prefill.is_some();
        let _ = write!(
            body,
            r##"<div class="post-toggle-bar centered catalog-toggle-bar">
  <a class="post-toggle-btn" href="#post-form-wrap" data-action="toggle-post-form">[ Post a New Thread ]</a>
</div>
<div class="{post_form_class}" id="post-form-wrap" style="{post_form_style}">
  {}
</div>"##,
            super::forms::new_thread_form(&board.short_name, csrf_token, board, new_thread_prefill,),
            post_form_class = if show_post_form {
                "post-form-wrap is-open"
            } else {
                "post-form-wrap is-collapsed"
            },
            post_form_style = if show_post_form {
                "display:block"
            } else {
                "display:none"
            },
        );
    } else if board.access_mode.requires_post_password() {
        body.push_str(&render_post_access_gate(
            board,
            csrf_token,
            &format!("/{}", board.short_name),
            "unlock posting",
        ));
    }

    for summary in summaries {
        body.push_str(&render_thread_summary(
            summary,
            &board.short_name,
            csrf_token,
            is_admin,
            board.show_poster_ids,
            board.collapse_greentext,
        ));
    }

    // escape_html on board.short_name before embedding in the URL.
    body.push_str(&render_pagination(
        pagination,
        &format!("/{}", escape_html(&board.short_name)),
    ));

    body.push_str(TOGGLE_SCRIPT);
    body.push_str(&compress_modal_script(
        crate::config::CONFIG.max_image_size,
        crate::config::CONFIG.max_video_size,
    ));

    base_layout(
        &format!("/{}/ — {} - Index", board.short_name, board.name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!("/{}", board.short_name),
    )
}

// ─── Thread summary (used by board_page) ─────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn render_thread_summary(
    summary: &ThreadSummary,
    board_short: &str,
    csrf_token: &str,
    is_admin: bool,
    show_poster_ids: bool,
    collapse_greentext: bool,
) -> String {
    let t = &summary.thread;
    let mut html = String::new();

    let sticky_label = if t.sticky {
        r#"<span class="tag sticky">STICKY</span> "#
    } else {
        ""
    };
    let locked_label = if t.locked {
        r#"<span class="tag locked">LOCKED</span> "#
    } else {
        ""
    };

    let _ = write!(
        html,
        r#"<div class="thread" id="t{tid}">
<div class="op post" id="p{op_id}">"#,
        tid = t.id,
        op_id = t.op_id.unwrap_or(0)
    );

    let thread_state_badges = super::thread::render_thread_state_badges(t.sticky, t.locked);

    if let (Some(_file), Some(thumb)) = (&t.op_file, &t.op_thumb) {
        let _ = write!(
            html,
            r#"<div class="file-container thread-summary-thumb-wrap"><a href="/{board}/thread/{tid}"><img class="thumb" src="/boards/{th}" loading="lazy" alt="image"></a>{badges}</div>"#,
            board = escape_html(board_short),
            tid = t.id,
            th = escape_html(thumb),
            badges = thread_state_badges
        );
    } else if let Some(embed_thumb) = t.op_body.as_deref().and_then(embed_thumb_from_body) {
        let _ = write!(
            html,
            r#"<div class="file-container thread-summary-thumb-wrap"><a href="/{board}/thread/{tid}"><img class="thumb embed-index-thumb" src="{src}" loading="lazy" alt="video thumbnail"></a>{badges}</div>"#,
            board = escape_html(board_short),
            tid = t.id,
            src = escape_html(&embed_thumb),
            badges = thread_state_badges
        );
    }

    let _ = write!(
        html,
        r#"<div class="post-meta">
{sticky}{locked}
<strong class="name">{name}</strong>
<span class="post-time" data-utc="{ts}">{time}</span>
<a class="post-num" href="/{board}/thread/{tid}">No.{op_id}</a>
<a class="thread-id-link" href="/{board}/thread/{tid}" title="Thread #{tid}">[ #{tid} ]</a>
</div>"#,
        sticky = sticky_label,
        locked = locked_label,
        name = escape_html(t.op_name.as_deref().unwrap_or("Anonymous")),
        ts = t.created_at,
        time = fmt_ts_short(t.created_at),
        board = escape_html(board_short),
        tid = t.id,
        op_id = t.op_id.unwrap_or(0)
    );

    if let Some(subject) = &t.subject {
        let _ = write!(
            html,
            r#"<div class="subject"><a href="/{b}/thread/{tid}"><strong>{s}</strong></a></div>"#,
            b = escape_html(board_short),
            tid = t.id,
            s = escape_html(subject)
        );
    }

    if let Some(body) = &t.op_body {
        // Count and slice by character, not by byte.
        // body[..300] panics on any post whose 300th byte falls inside a
        // multi-byte codepoint (emoji, CJK, Arabic, etc.).
        let char_count = body.chars().count();
        let truncated = if char_count > 300 {
            let safe: String = body.chars().take(300).collect();
            format!(
                r#"{} <a href="/{b}/thread/{tid}">…[Read more]</a>"#,
                escape_html(&safe),
                b = escape_html(board_short),
                tid = t.id,
            )
        } else {
            escape_html(body)
        };
        let _ = write!(html, r#"<div class="post-body">{truncated}</div>"#);
    }

    let _ = write!(
        html,
        r#"<div class="thread-footer">
<a href="/{board}/thread/{tid}">[reply] ({n} {word})</a>"#,
        board = escape_html(board_short),
        tid = t.id,
        n = t.reply_count,
        word = if t.reply_count == 1 {
            "reply"
        } else {
            "replies"
        }
    );

    if is_admin {
        let sticky_act = if t.sticky { "unsticky" } else { "sticky" };
        let sticky_lbl = if t.sticky {
            "&#128204; unsticky"
        } else {
            "&#128204; sticky"
        };
        let lock_act = if t.locked { "unlock" } else { "lock" };
        let lock_lbl = if t.locked {
            "&#128275; unlock"
        } else {
            "&#128274; lock"
        };
        let _ = write!(
            html,
            r#" <form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf"      value="{csrf}">
<input type="hidden" name="thread_id"  value="{tid}">
<input type="hidden" name="board"      value="{board}">
<input type="hidden" name="action"     value="{sticky_act}">
<button type="submit" class="admin-del-btn">{sticky_lbl}</button>
</form>
<form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf"      value="{csrf}">
<input type="hidden" name="thread_id"  value="{tid}">
<input type="hidden" name="board"      value="{board}">
<input type="hidden" name="action"     value="{lock_act}">
<button type="submit" class="admin-del-btn">{lock_lbl}</button>
</form>
<form method="POST" action="/admin/thread/delete" style="display:inline">
<input type="hidden" name="_csrf"      value="{csrf}">
<input type="hidden" name="thread_id"  value="{tid}">
<input type="hidden" name="board"      value="{board}">
<button type="submit" class="admin-del-btn"
        data-confirm="Delete thread No.{tid} and all its posts?">&#x2715; del</button>
</form>"#,
            csrf = escape_html(csrf_token),
            tid = t.id,
            board = escape_html(board_short),
            sticky_act = sticky_act,
            sticky_lbl = sticky_lbl,
            lock_act = lock_act,
            lock_lbl = lock_lbl
        );
    }

    html.push_str("</div>\n</div>");

    if summary.omitted > 0 {
        let _ = write!(
            html,
            r#"<div class="omitted">{} posts omitted. <a href="/{b}/thread/{tid}">view thread</a></div>"#,
            summary.omitted,
            b = escape_html(board_short),
            tid = t.id
        );
    }

    for post in &summary.preview_posts {
        html.push_str(&super::thread::render_post(
            post,
            board_short,
            csrf_token,
            super::thread::RenderPostOpts {
                show_delete: false,
                is_admin,
                show_media: true,
                allow_editing: false, // no edit link on board index previews
                show_poster_ids,
                collapse_greentext,
                thread_state: None,
                thread_op_id: summary.thread.op_id,
            },
            0,
        ));
    }

    html.push_str("<hr class=\"thread-sep\">");
    html
}

// ─── Catalog page ─────────────────────────────────────────────────────────────

#[must_use]
#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::implicit_hasher
)]
pub fn catalog_page(
    board: &Board,
    threads: &[Thread],
    pinned_ids: &HashSet<i64>,
    hidden_count: usize,
    hidden_view: bool,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    board_banner_html: &str,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
) -> String {
    let bs = escape_html(&board.short_name);
    let bn = escape_html(&board.name);

    let mut body = String::new();

    if is_admin {
        let _ = write!(
            body,
            r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}/catalog">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
            csrf = escape_html(csrf_token),
            board = escape_html(&board.short_name)
        );
    }

    let nav_archive = if board.allow_archive {
        format!(r#"<a class="board-nav-link" href="/{bs}/archive">[Archive]</a>"#)
    } else {
        String::new()
    };
    let hidden_nav = if hidden_count > 0 {
        let active_class = if hidden_view { " active" } else { "" };
        format!(
            r#"<span class="board-nav-hidden">Hidden Threads: {hidden_count} <a class="board-nav-link{active_class}" href="/{bs}/hidden">[Show]</a></span>"#,
        )
    } else {
        String::new()
    };
    let title_suffix = if hidden_view { " hidden threads" } else { "" };
    let empty_message = if hidden_view {
        "No hidden threads right now."
    } else {
        "No threads yet."
    };
    let access_badge = board_access_badge(board);

    let _ = write!(
        body,
        r#"<div class="board-header board-catalog-header">
  <div class="catalog-header-left board-catalog-header">
    <h1>/{bs}/  — {bn}{access_badge}{title_suffix}</h1>
    <p class="board-desc">{desc}</p>
  </div>
</div>
{board_banner_html}
<div class="catalog-controls">
  <div class="catalog-control-group">
    <label class="catalog-sort-label" for="catalog-sort">Sort By:</label>
    <select id="catalog-sort" class="catalog-sort-select" data-action="sort-catalog">
    <option value="bump" selected>bump order</option>
    <option value="replies">reply count</option>
    <option value="created">creation date</option>
    <option value="last_reply">last reply</option>
    </select>
  </div>
  <div class="catalog-control-group">
    <label class="catalog-sort-label" for="catalog-show-comment">Show OP Comment:</label>
    <select id="catalog-show-comment" class="catalog-sort-select" data-action="catalog-show-comment">
      <option value="on">On</option>
      <option value="off" selected>Off</option>
    </select>
  </div>
</div>
<div class="board-nav"><a class="board-nav-link" href="/{bs}">[Index]</a><a class="board-nav-link{catalog_active}" href="/{bs}/catalog">[Catalog]</a>{nav_archive}{hidden_nav}</div>"#,
        bs = bs,
        bn = bn,
        access_badge = access_badge,
        title_suffix = title_suffix,
        desc = escape_html(&board.description),
        board_banner_html = board_banner_html,
        catalog_active = if hidden_view { "" } else { " active" },
        nav_archive = nav_archive,
        hidden_nav = hidden_nav,
    );
    if can_post {
        let _ = write!(
            body,
            r##"<div class="post-toggle-bar centered catalog-toggle-bar">
  <a class="post-toggle-btn" href="#post-form-wrap" data-action="toggle-post-form">[ Start a New Thread ]</a>
</div>
<div class="post-form-wrap" id="post-form-wrap" style="display:none">
  {form}
</div>"##,
            form = super::forms::new_thread_form(&board.short_name, csrf_token, board, None)
        );
    } else if board.access_mode.requires_post_password() {
        body.push_str(&render_post_access_gate(
            board,
            csrf_token,
            &if hidden_view {
                format!("/{}/hidden", board.short_name)
            } else {
                format!("/{}/catalog", board.short_name)
            },
            "unlock posting",
        ));
    }
    body.push_str(r#"<div class="catalog-grid" id="catalog-grid">"#);

    for t in threads {
        let is_pinned = pinned_ids.contains(&t.id);
        let menu_hide_action = if hidden_view { "unhide" } else { "hide" };
        let menu_hide_label = if hidden_view {
            "Unhide thread"
        } else {
            "Hide thread"
        };
        let pin_action = if is_pinned { "unpin" } else { "pin" };
        let pin_label = if is_pinned {
            "Unpin thread"
        } else {
            "Pin thread"
        };
        let return_to = if hidden_view && menu_hide_action == "unhide" {
            format!("/{}/catalog", board.short_name)
        } else if hidden_view {
            format!("/{}/hidden", board.short_name)
        } else {
            format!("/{}/catalog", board.short_name)
        };
        body.push_str(&render_catalog_card(
            board,
            t,
            is_pinned,
            csrf_token,
            pin_action,
            pin_label,
            menu_hide_action,
            menu_hide_label,
            &return_to,
        ));
    }

    if threads.is_empty() {
        let _ = write!(
            body,
            r#"<p class="catalog-empty-state">{}</p>"#,
            escape_html(empty_message)
        );
    }

    body.push_str("</div>");
    body.push_str(report_modal_script());
    // sortCatalog moved to /static/main.js
    body.push_str(TOGGLE_SCRIPT);
    body.push_str(&compress_modal_script(
        crate::config::CONFIG.max_image_size,
        crate::config::CONFIG.max_video_size,
    ));
    base_layout(
        &format!(
            "/{}/ — {} - {}",
            board.short_name,
            board.name,
            if hidden_view {
                "Hidden Threads"
            } else {
                "Catalog"
            }
        ),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &if hidden_view {
            format!("/{}/hidden", board.short_name)
        } else {
            format!("/{}/catalog", board.short_name)
        },
    )
}

// ─── Search results ───────────────────────────────────────────────────────────

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn search_page(
    board: &Board,
    query: &str,
    posts: &[Post],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    collapse_greentext: bool,
) -> String {
    let result_label = if pagination.total == 1 {
        "1 result".to_string()
    } else {
        format!("{} results", pagination.total)
    };
    let mut body = format!(
        r#"<div class="page-box">
<div class="board-search-header">
  <h2 class="board-search-title">Search /{}/</h2>
  <p class="board-search-summary">Showing results for "{}".</p>
</div>
<form method="GET" action="/{}/search" class="search-form board-search-form">
  <label class="catalog-sort-label board-search-label" for="board-search-input">Query:</label>
  <input id="board-search-input" type="text" name="q" value="{}" maxlength="{}">
  <button type="submit">search</button>
</form>"#,
        escape_html(&board.short_name),
        escape_html(query),
        escape_html(&board.short_name),
        escape_html(query),
        SEARCH_QUERY_MAX_CHARS,
    );

    if posts.is_empty() {
        body.push_str(
            r#"<p class="catalog-empty-state board-search-empty">no results found. try a different query.</p>"#,
        );
    } else {
        let _ = write!(
            body,
            r#"<p class="board-search-summary board-search-summary-results">{}</p>"#,
            escape_html(&result_label)
        );
        for post in posts {
            body.push_str(&super::thread::render_post(
                post,
                &board.short_name,
                csrf_token,
                super::thread::RenderPostOpts {
                    show_delete: false,
                    is_admin: false,
                    show_media: true,
                    allow_editing: false, // no edit link on search results
                    show_poster_ids: board.show_poster_ids,
                    collapse_greentext: board.collapse_greentext,
                    thread_state: None,
                    thread_op_id: None,
                },
                0,
            ));
        }
        body.push_str(&render_pagination(
            pagination,
            &format!(
                "/{}/search?q={}",
                escape_html(&board.short_name),
                urlencoding_simple(query)
            ),
        ));
    }

    body.push_str("</div>");
    base_layout(
        &format!("search — /{}/", board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!(
            "/{}/search?q={}",
            board.short_name,
            urlencoding_simple(query)
        ),
    )
}

// ─── Archive page ─────────────────────────────────────────────────────────────

#[must_use]
pub fn archive_page(
    board: &Board,
    threads: &[Thread],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    collapse_greentext: bool,
) -> String {
    let bs = escape_html(&board.short_name);
    let bn = escape_html(&board.name);

    let mut body = format!(
        r#"<div class="board-header board-index-header"><h1>/{bs}/  — {bn}</h1><p class="board-desc">{desc}</p></div>
<div class="board-nav">
  <a class="board-nav-link" href="/{bs}">[Index]</a>
  <a class="board-nav-link" href="/{bs}/catalog">[Catalog]</a>
  <a class="board-nav-link active" href="/{bs}/archive">[Archive]</a>
</div>
<div class="page-box">
<p class="archive-subtext">Threads cycled off the board index — read-only, retained up to this board's archive limit.</p>
</div>"#,
        bs = bs,
        bn = bn,
        desc = escape_html(&board.description),
    );

    if threads.is_empty() {
        body.push_str(
            r#"<div class="page-box"><p style="color:var(--text-dim)">no archived threads yet.</p></div>"#,
        );
    } else {
        body.push_str(r#"<div class="archive-list">"#);
        for t in threads {
            body.push_str(&render_archive_row(&board.short_name, t));
        }
        body.push_str("</div>");
        // escape before embedding in pagination URL.
        body.push_str(&render_pagination(
            pagination,
            &format!("/{}/archive", escape_html(&board.short_name)),
        ));
    }

    base_layout(
        &format!("/{}/  archive", board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!("/{}/archive", board.short_name),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        archive_page, board_cards, board_page, catalog_page, index_page, render_catalog_card,
    };
    use crate::models::{Board, BoardStats, SiteStats, Thread};
    use crate::templates::forms::PostFormState;
    use std::collections::HashSet;

    fn sample_board() -> Board {
        Board {
            display_order: 0,
            description: "Board description".into(),
            bump_limit: 300,
            allow_video_embeds: true,
            default_theme: String::new(),
            created_at: 1_700_000_000,
            ..crate::test_fixtures::sample_board()
        }
    }

    fn sample_thread() -> Thread {
        Thread {
            id: 87,
            board_id: 1,
            subject: Some("Thread subject".into()),
            created_at: 1_700_000_000,
            bumped_at: 1_700_000_100,
            locked: true,
            sticky: true,
            archived: false,
            reply_count: 12,
            image_count: 3,
            op_body: Some("Thread body preview".into()),
            op_file: Some("test/image.webp".into()),
            op_thumb: Some("test/thumbs/image.webp".into()),
            op_name: Some("anon".into()),
            op_tripcode: None,
            op_id: Some(87),
        }
    }

    #[test]
    fn board_cards_render_reorder_controls_only_when_enabled() {
        let board = sample_board();
        let stats = BoardStats {
            board,
            thread_count: 4,
        };

        let html_without_controls = board_cards(&[&stats], true, "csrf", false);
        assert!(html_without_controls.contains("board-card-link"));
        assert!(!html_without_controls.contains("board-reorder-menu"));

        let html_with_controls = board_cards(&[&stats], true, "csrf", true);
        assert!(html_with_controls.contains("board-reorder-menu"));
    }

    #[test]
    fn index_page_surfaces_unavailable_stats_without_fake_zeroes() {
        crate::templates::set_live_site_name("TestChan");
        crate::templates::set_live_site_subtitle("banner subtitle");

        let html = index_page(&[], None, "csrf", None, "", None, None, true, false);

        assert!(html.contains("site statistics are temporarily unavailable."));
        assert!(!html.contains("0.00 GB"));
        assert!(!html.contains("audio files uploaded</span></div>"));
    }

    #[test]
    fn index_page_renders_stats_when_available() {
        crate::templates::set_live_site_name("TestChan");
        crate::templates::set_live_site_subtitle("banner subtitle");

        let stats = SiteStats {
            total_posts: 12,
            total_images: 8,
            total_videos: 2,
            total_audio: 3,
            active_bytes: 2 * 1024 * 1024 * 1024,
        };

        let html = index_page(&[], Some(&stats), "csrf", None, "", None, None, true, false);

        assert!(html.contains("audio files uploaded"));
        assert!(html.contains(">3</span><span class=\"index-stat-label\">audio files uploaded"));
        assert!(html.contains("2.00 GB"));
    }

    #[test]
    fn catalog_page_renders_componentized_card_with_state_badges() {
        let board = sample_board();
        let thread = sample_thread();
        let mut pinned_ids = HashSet::new();
        pinned_ids.insert(thread.id);

        let html = catalog_page(
            &board,
            &[thread],
            &pinned_ids,
            0,
            false,
            "csrf",
            std::slice::from_ref(&board),
            false,
            "",
            None,
            false,
            true,
        );

        assert!(html.contains("catalog-card-link"));
        assert!(html.contains("catalog-card-media"));
        assert!(html.contains("thread-state-badge-pin"));
        assert!(html.contains("thread-state-badge-lock"));
        assert!(html.contains(r#"data-pinned="1""#));
    }

    #[test]
    fn catalog_actions_render_outside_card_link() {
        let board = sample_board();
        let thread = sample_thread();

        let html = render_catalog_card(
            &board,
            &thread,
            false,
            "csrf",
            "pin",
            "Pin thread",
            "hide",
            "Hide thread",
            "/test/catalog",
        );

        let actions_idx = html
            .find("catalog-card-actions")
            .expect("catalog actions should exist");
        let link_close_idx = html.find("</a>").expect("catalog link should close");
        assert!(
            actions_idx > link_close_idx,
            "interactive actions should render after the card link"
        );
    }

    #[test]
    fn catalog_reply_counter_renders_between_thumbnail_and_body() {
        let board = sample_board();
        let thread = sample_thread();

        let html = render_catalog_card(
            &board,
            &thread,
            false,
            "csrf",
            "pin",
            "Pin thread",
            "hide",
            "Hide thread",
            "/test/catalog",
        );

        let thumb_idx = html
            .find("catalog-card-media")
            .expect("thumbnail should exist");
        let meta_idx = html
            .find("catalog-meta-row")
            .expect("meta row should exist");
        let info_idx = html.find("catalog-info").expect("body block should exist");

        assert!(
            thumb_idx < meta_idx && meta_idx < info_idx,
            "reply counter should render directly under the thumbnail before the body text"
        );
    }

    #[test]
    fn archive_page_renders_state_badges_and_media_wrapper() {
        let board = sample_board();
        let thread = sample_thread();

        let html = archive_page(
            &board,
            &[thread],
            &crate::models::Pagination::new(1, 10, 1),
            "csrf",
            std::slice::from_ref(&board),
            None,
            false,
        );

        assert!(html.contains("archive-row-media"));
        assert!(html.contains("thread-state-badge-pin"));
        assert!(html.contains("thread-state-badge-archive"));
        assert!(!html.contains("thread-state-badge-lock"));
        assert!(html.contains("archive-meta"));
    }

    #[test]
    fn board_page_reopens_new_thread_form_when_error_state_exists() {
        let board = sample_board();
        let state = PostFormState {
            body: "retry".into(),
            ..PostFormState::default()
        };

        let html = board_page(
            &board,
            &[],
            &crate::models::Pagination::new(1, 10, 0),
            "csrf",
            std::slice::from_ref(&board),
            false,
            Some("Post must include either text or an attached file."),
            Some(&state),
            "",
            None,
            false,
            true,
        );

        assert!(html.contains(r#"class="post-form-wrap is-open""#));
        assert!(html.contains(r#"style="display:block""#));
        assert!(html.contains(">retry</textarea>"));
    }
}
