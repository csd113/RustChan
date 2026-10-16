//! Page templates for board-level views.

use crate::models::{Board, Pagination, Post, Thread, ThreadSummary, SEARCH_QUERY_MAX_CHARS};
use crate::utils::sanitize::escape_html;
use std::collections::{HashMap, HashSet};

use super::{
    base_layout, base_layout_with_preferences, compress_modal_script, embed_thumb_from_body,
    fmt_ts, fmt_ts_short, live_site_name, live_site_subtitle, render_pagination,
    report_modal_script, urlencoding_simple,
};

// Site index (board list)
/// Renders administrator controls for moving a board within its content group.
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

/// Renders a positive new-activity count or an empty fragment.
fn render_new_activity_badge(count: i64, class_name: &str, label: &str) -> String {
    if count <= 0 {
        return String::new();
    }
    format!(
        r#"<span class="{class_name}"><span class="new-activity-dot" aria-hidden="true"></span>{count} {label}</span>"#,
        class_name = escape_html(class_name),
        count = count,
        label = escape_html(label),
    )
}

// These flags map directly to render or DB inputs, so bundling them would make the call sites less clear.
#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "the card consumes independent permissions, positions, and visitor preferences"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the board card keeps its navigation, badges, and reorder controls together"
)]
/// Renders one board directory card.
fn render_board_card(
    stats: &crate::models::BoardStats,
    unread_thread_count: Option<i64>,
    unread_reply_count: Option<i64>,
    nsfw_consent: bool,
    csrf_token: &str,
    admin_csrf_token: Option<&str>,
    show_reorder_controls: bool,
    is_first: bool,
    is_last: bool,
    user_preferences: crate::templates::UserPreferences,
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
    } else if user_preferences.preferred_board_view.is_catalog() {
        format!("/{}/catalog", escape_html(&board.short_name))
    } else {
        format!("/{}", escape_html(&board.short_name))
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
            } else if user_preferences.preferred_board_view.is_catalog() {
                format!("{}/catalog", escape_html(&board.short_name))
            } else {
                escape_html(&board.short_name)
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
        board_reorder_controls(
            board,
            admin_csrf_token.unwrap_or(csrf_token),
            "/",
            is_first,
            is_last,
        )
    } else {
        String::new()
    };
    let access_badge = board_access_badge(board);
    let thread_activity_badge = unread_thread_count
        .map(|count| {
            render_new_activity_badge(
                count,
                "new-activity-badge board-card-activity-badge board-card-new-thread-badge",
                "New Threads",
            )
        })
        .unwrap_or_default();
    let reply_activity_badge = unread_reply_count
        .map(|count| {
            render_new_activity_badge(
                count,
                "new-activity-badge board-card-activity-badge board-card-new-reply-badge",
                "New Replies",
            )
        })
        .unwrap_or_default();
    let activity_badges = if thread_activity_badge.is_empty() && reply_activity_badge.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="board-card-activity-row">{thread_activity_badge}{reply_activity_badge}</div>"#
        )
    };

    let nsfw_attr = if board.nsfw {
        r#" data-board-nsfw="1""#
    } else {
        ""
    };

    format!(
        r#"<div class="board-card"{nsfw_attr}>
  {reorder_controls}
  <a class="board-card-link" href="{href}"{action_attr}{return_to_attr}>
    <div class="board-card-short"><span class="board-card-slug">/{sh}/</span><span class="board-card-badges">{nsfw}{access_badge}</span></div>
    <div class="board-card-name">{name}</div>
    <div class="board-card-desc">{description}</div>
    <div class="board-card-stats">{thread_count} {thread_word}</div>
    {activity_badges}
  </a>
</div>"#,
        reorder_controls = reorder_controls,
        nsfw_attr = nsfw_attr,
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
        activity_badges = activity_badges,
    )
}

/// Renders the access-policy badge for a board.
pub(super) fn board_access_badge(board: &Board) -> String {
    match board.access_mode {
        crate::models::BoardAccessMode::Public => String::new(),
        crate::models::BoardAccessMode::ViewPassword => {
            r#" <span class="tag locked">PASSWORD</span>"#.to_owned()
        }
        crate::models::BoardAccessMode::PostPassword => {
            r#" <span class="tag sticky">POST PASSWORD</span>"#.to_owned()
        }
    }
}

/// Returns the heading, explanation, and action label for a board access policy.
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

/// Renders the password gate shown in place of a post form.
pub(super) fn render_post_access_gate(
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
        <td><input type="password" name="password" aria-label="board password" maxlength="256" autocomplete="current-password" required>
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
/// Renders a standalone board-password prompt.
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
    <tr><td>password</td><td><input type="password" name="password" aria-label="board password" maxlength="256" autocomplete="current-password" autofocus required></td></tr>
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

/// Truncates display text at a Unicode scalar boundary and appends an ellipsis.
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

/// Renders a catalog thumbnail with a text fallback.
fn render_catalog_media_thumb(
    class_name: &str,
    src: &str,
    alt: &str,
    fallback_text: &str,
) -> String {
    let img_src = if src.starts_with("http://") || src.starts_with("https://") {
        src.to_owned()
    } else {
        format!("/boards/{src}")
    };
    format!(
        r#"<img class="{class_name}" src="{src}" loading="lazy" decoding="async" alt="{alt}" data-media-thumb="1">
<div class="catalog-thumb-fallback media-thumb-fallback" hidden>{fallback_text}</div>"#,
        class_name = escape_html(class_name),
        src = escape_html(&img_src),
        alt = escape_html(alt),
        fallback_text = escape_html(fallback_text),
    )
}

/// Renders the media area and state badges for one catalog thread.
fn render_catalog_thumb(thread: &Thread) -> String {
    let badges = super::thread::render_thread_state_badges(thread.sticky, thread.locked);
    let media = thread.op_thumb.as_ref().map_or_else(
        || {
            thread
                .op_body
                .as_deref()
                .and_then(embed_thumb_from_body)
                .map_or_else(
                    || r#"<div class="catalog-no-image">no img</div>"#.to_owned(),
                    |embed_thumb| {
                        render_catalog_media_thumb(
                            "catalog-thumb embed-catalog-thumb",
                            &embed_thumb,
                            "video thumbnail",
                            "no img",
                        )
                    },
                )
        },
        |thumb| render_catalog_media_thumb("catalog-thumb", thumb, "", "no img"),
    );

    format!(r#"<div class="catalog-card-media">{media}{badges}</div>"#)
}

/// Render personal actions using the same authoritative labels on every active view.
pub(super) fn render_thread_actions(
    board_short: &str,
    thread: &Thread,
    csrf_token: &str,
    preference: crate::db::UserThreadPreference,
    return_to: &str,
) -> String {
    render_catalog_actions(
        board_short,
        thread,
        csrf_token,
        if preference.pinned { "unpin" } else { "pin" },
        if preference.pinned {
            "Unpin thread"
        } else {
            "Pin thread"
        },
        if preference.hidden { "unhide" } else { "hide" },
        if preference.hidden {
            "Unhide thread"
        } else {
            "Hide thread"
        },
        return_to,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the action menu accepts independent labels and form values for two actions"
)]
/// Renders report, pin, and hide controls for a catalog thread.
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
    let report_post_id = thread.op_id.unwrap_or(thread.id);
    let report_fallback =
        super::report_fallback_form(board_short, report_post_id, thread.id, csrf_token, "submit");
    format!(
        r#"<div class="catalog-card-actions">
  <button type="button" class="catalog-thread-menu-toggle" data-action="toggle-thread-menu" aria-haspopup="menu" aria-expanded="false" aria-controls="catalog-thread-menu-{thread_id}" aria-label="Thread actions"></button>
  <div class="catalog-thread-menu" id="catalog-thread-menu-{thread_id}" role="menu" aria-label="Thread actions" hidden inert aria-hidden="true">
    <button type="button" class="catalog-thread-menu-item" role="menuitem" tabindex="-1" data-action="open-report" data-pid="{post_id}" data-tid="{thread_id}" data-board="{board}" data-csrf="{csrf}" data-report-label="Reporting thread No.{thread_id}">Report thread</button>
    <form method="POST" action="/{board}/thread-preference">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="thread_id" value="{thread_id}">
      <input type="hidden" name="board" value="{board}">
      <input type="hidden" name="action" value="{pin_action}">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit" class="catalog-thread-menu-item" role="menuitem" tabindex="-1"{pin_disabled}>{pin_label}</button>
    </form>
    <form method="POST" action="/{board}/thread-preference">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="thread_id" value="{thread_id}">
      <input type="hidden" name="board" value="{board}">
      <input type="hidden" name="action" value="{hide_action}">
      <input type="hidden" name="return_to" value="{return_to}">
      <button type="submit" class="catalog-thread-menu-item" role="menuitem" tabindex="-1"{hide_disabled}>{hide_label}</button>
    </form>
  </div>
  <details class="catalog-thread-fallback-actions" aria-label="Thread actions" open>
    <summary class="catalog-thread-fallback-summary">actions</summary>
    <div class="catalog-thread-fallback-group">
      {report_fallback}
      <form class="catalog-thread-fallback-form" method="POST" action="/{board}/thread-preference">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="thread_id" value="{thread_id}">
        <input type="hidden" name="board" value="{board}">
        <input type="hidden" name="action" value="{pin_action}">
        <input type="hidden" name="return_to" value="{return_to}">
        <button type="submit" class="catalog-thread-fallback-submit"{pin_disabled}>{pin_label}</button>
      </form>
      <form class="catalog-thread-fallback-form" method="POST" action="/{board}/thread-preference">
        <input type="hidden" name="_csrf" value="{csrf}">
        <input type="hidden" name="thread_id" value="{thread_id}">
        <input type="hidden" name="board" value="{board}">
        <input type="hidden" name="action" value="{hide_action}">
        <input type="hidden" name="return_to" value="{return_to}">
        <button type="submit" class="catalog-thread-fallback-submit"{hide_disabled}>{hide_label}</button>
      </form>
    </div>
  </details>
</div>"#,
        post_id = report_post_id,
        thread_id = thread.id,
        board = escape_html(board_short),
        csrf = escape_html(csrf_token),
        pin_disabled = if thread.archived && pin_action == "pin" {
            " disabled"
        } else {
            ""
        },
        hide_disabled = if thread.archived && hide_action == "hide" {
            " disabled"
        } else {
            ""
        },
        pin_action = escape_html(pin_action),
        pin_label = escape_html(pin_label),
        hide_action = escape_html(hide_action),
        hide_label = escape_html(hide_label),
        return_to = escape_html(return_to),
        report_fallback = report_fallback,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the card combines thread state with independent action labels and values"
)]
/// Renders one thread card in the catalog.
fn render_catalog_card(
    board: &Board,
    thread: &Thread,
    is_pinned: bool,
    unread_reply_count: Option<i64>,
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
    let activity_badge = unread_reply_count
        .map(|count| {
            render_new_activity_badge(count, "new-activity-badge catalog-activity-badge", "New")
        })
        .unwrap_or_default();
    let activity_row = if activity_badge.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="catalog-activity-row">{activity_badge}</div>"#)
    };

    format!(
        r#"<div class="catalog-item{sticky}{pinned_class}" data-thread-id="{thread_id}" data-replies="{replies}" data-created="{created}" data-bumped="{bumped}" data-last-reply="{last_reply}" data-sticky="{is_sticky}" data-pinned="{is_pinned}">
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
{activity_row}
</div>"#,
        sticky = if thread.sticky { " sticky" } else { "" },
        pinned_class = if is_pinned { " is-pinned" } else { "" },
        replies = thread.reply_count,
        created = thread.created_at,
        bumped = thread.bumped_at,
        last_reply = thread.last_post_at,
        is_sticky = if thread.sticky { "1" } else { "0" },
        is_pinned = if is_pinned { "1" } else { "0" },
        board = escape_html(&board.short_name),
        thread_id = thread.id,
        thumb = render_catalog_thumb(thread),
        images = thread.image_count,
        actions = actions_html,
        subject = subject_html,
        comment = comment_html,
        activity_row = activity_row,
    )
}

/// Renders one archived-thread table row.
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
            r#"<div class="archive-row-media"><img src="/boards/{}" class="archive-thumb" alt="" loading="lazy" decoding="async"></div>"#,
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
    <span class="archive-meta">No.{thread_id}{state_badges} - {replies} replies - {attachments} attachments - Created {created_at} - Last bump {bumped_at}</span>
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
        bumped_at = fmt_ts(thread.bumped_at),
        attachments = thread.image_count,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the directory renderer consumes distinct badge, access, and admin contexts"
)]
/// Renders a sequence of board directory cards.
fn board_cards<S: std::hash::BuildHasher>(
    list: &[&crate::models::BoardStats],
    board_new_thread_badges: &HashMap<i64, i64, S>,
    board_new_reply_badges: &HashMap<i64, i64, S>,
    nsfw_consent: bool,
    csrf_token: &str,
    admin_csrf_token: Option<&str>,
    show_reorder_controls: bool,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    let mut out = String::new();
    for (index, s) in list.iter().enumerate() {
        out.push_str(&render_board_card(
            s,
            board_new_thread_badges.get(&s.board.id).copied(),
            board_new_reply_badges.get(&s.board.id).copied(),
            nsfw_consent,
            csrf_token,
            admin_csrf_token,
            show_reorder_controls,
            index == 0,
            index.checked_add(1) == Some(list.len()),
            user_preferences,
        ));
    }
    out
}

#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the homepage keeps its board groups, statistics, and modals together"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the homepage consumes distinct board, activity, consent, and admin contexts"
)]
/// Renders the site homepage and board directory.
pub fn index_page<S: std::hash::BuildHasher>(
    board_stats: &[crate::models::BoardStats],
    site_stats: Option<&crate::models::SiteStats>,
    csrf_token: &str,
    admin_csrf_token: Option<&str>,
    onion_address: Option<&str>,
    home_banner_html: &str,
    board_badges: &HashMap<i64, i64, S>,
    board_reply_badges: &HashMap<i64, i64, S>,
    current_theme: Option<&str>,
    nsfw_prompt_board: Option<&Board>,
    nsfw_consent: bool,
    is_admin: bool,
    user_preferences: crate::templates::UserPreferences,
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
            board_cards(
                &sfw,
                board_badges,
                board_reply_badges,
                nsfw_consent,
                csrf_token,
                admin_csrf_token,
                is_admin,
                user_preferences
            )
        )
    };

    let nsfw_sec = if user_preferences.hide_nsfw_boards || nsfw.is_empty() {
        String::new()
    } else {
        format!(
            "<div class=\"index-section\" data-board-nsfw=\"1\"><h2 class=\"index-section-title\">// Adult Boards <span class=\"nsfw-badge\">NSFW</span></h2><div class=\"board-cards\">{}</div></div>",
            board_cards(
                &nsfw,
                board_badges,
                board_reply_badges,
                nsfw_consent,
                csrf_token,
                admin_csrf_token,
                is_admin,
                user_preferences
            )
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
</div>"#.to_owned()
        },
        |site_stats| {
            const GIB: i64 = 1024 * 1024 * 1024;
            let active_gb_hundredths = site_stats
                .active_bytes
                .max(0)
                .saturating_mul(100)
                .saturating_add(GIB / 2)
                .checked_div(GIB)
                .unwrap_or(0);
            let active_gb_whole = active_gb_hundredths / 100;
            let active_gb_fraction = active_gb_hundredths % 100;
            format!(
                r#"<div class="index-section index-stats-section">
<h2 class="index-section-title">// Stats</h2>
<div class="index-stats-grid">
  <div class="index-stat"><span class="index-stat-value">{tp}</span><span class="index-stat-label">total posts</span></div>
  <div class="index-stat"><span class="index-stat-value">{ti}</span><span class="index-stat-label">images uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{tv}</span><span class="index-stat-label">videos uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{ta}</span><span class="index-stat-label">audio files uploaded</span></div>
  <div class="index-stat"><span class="index-stat-value">{active_gb_whole}.{active_gb_fraction:02} GB</span><span class="index-stat-label">active content</span></div>
</div>
</div>"#,
                tp = site_stats.total_posts,
                ti = site_stats.total_images,
                tv = site_stats.total_videos,
                ta = site_stats.total_audio,
                active_gb_whole = active_gb_whole,
                active_gb_fraction = active_gb_fraction,
            )
        },
    );

    let mut access_links = String::new();
    if let Some(addr) = onion_address {
        let escaped_addr = escape_html(addr);
        crate::templates::append_html(
            &mut access_links,
            format_args!(
                r#"<p class="index-onion"><code class="onion-addr">{escaped_addr}</code><button type="button" class="tor-copy-button" data-tor-address="{escaped_addr}" aria-label="Copy Tor address" hidden>Copy</button><span class="tor-copy-status" aria-live="polite"></span></p>"#
            ),
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
            r#" aria-hidden="false""#
        } else {
            r#" hidden inert aria-hidden="true""#
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
            r#"<div id="nsfw-disclaimer-overlay" class="compress-modal nsfw-disclaimer-overlay{open_class}" role="dialog" aria-modal="true" aria-labelledby="nsfw-disclaimer-title" aria-describedby="nsfw-disclaimer-info"{hidden_attr}>
  <div class="compress-modal-box nsfw-disclaimer-box">
    <div class="compress-modal-title" id="nsfw-disclaimer-title">Disclaimer</div>
    <div class="compress-modal-info" id="nsfw-disclaimer-info">
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
        r#"<div class="index-hero" data-activity-page="home">
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

    base_layout_with_preferences(
        &live_site_name(),
        None,
        &body,
        csrf_token,
        &all_boards,
        current_theme,
        None,
        false,
        "/",
        user_preferences,
    )
}

// Board index
#[must_use]
#[expect(
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools,
    reason = "the board index keeps its forms, navigation, and thread list together"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the board index consumes distinct paging, moderation, activity, and visitor contexts"
)]
/// Renders a board's paginated thread index.
pub fn board_page<S: std::hash::BuildHasher, P: std::hash::BuildHasher>(
    board: &Board,
    summaries: &[ThreadSummary],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    admin_csrf_token: Option<&str>,
    error: Option<&str>,
    new_thread_prefill: Option<&super::forms::PostFormState>,
    thread_badges: &HashMap<i64, i64, S>,
    new_activity_enabled: bool,
    board_banner_html: &str,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
    thread_preferences: &HashMap<i64, crate::db::UserThreadPreference, P>,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    let mut body = String::new();
    let admin_form_csrf = admin_csrf_token.unwrap_or(csrf_token);

    if let Some(msg) = error {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="post-error-banner">&#9888; {}</div>"#,
                escape_html(msg)
            ),
        );
    }

    if is_admin {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
                csrf = escape_html(admin_form_csrf),
                board = escape_html(&board.short_name)
            ),
        );
    }

    {
        let short = escape_html(&board.short_name);
        let name = escape_html(&board.name);
        let desc = escape_html(&board.description);
        let access_badge = board_access_badge(board);
        let hidden_count = thread_preferences
            .values()
            .filter(|pref| pref.hidden)
            .count();
        let nav_hidden = format!(
            r#"<a class="board-nav-link" href="/{short}/hidden">[Hidden ({hidden_count})]</a>"#
        );
        let nav_archive =
            format!(r#"<a class="board-nav-link" href="/{short}/archive">[Archive]</a>"#);
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="board-header board-index-header" data-activity-page="board-index"><h1>/{short}/  — {name}{access_badge}</h1><p class="board-desc">{desc}</p></div>
{board_banner_html}
<div class="board-nav"><a class="board-nav-link active" href="/{short}">[Index]</a><a class="board-nav-link" href="/{short}/catalog">[Catalog]</a>{nav_archive}{nav_hidden}</div>"#
            ),
        );
    }

    if can_post {
        let show_post_form = error.is_some() || new_thread_prefill.is_some();
        crate::templates::append_html(
            &mut body,
            format_args!(
                r##"<div class="post-toggle-bar centered catalog-toggle-bar">
  <a class="post-toggle-btn" href="#post-form-wrap" data-action="toggle-post-form">[ Post a New Thread ]</a>
</div>
<div class="{post_form_class}" id="post-form-wrap" style="{post_form_style}">
  {}
</div>"##,
                super::forms::new_thread_form(
                    &board.short_name,
                    csrf_token,
                    board,
                    new_thread_prefill,
                    &format!("/{}", board.short_name),
                ),
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
            ),
        );
    } else if board.access_mode.requires_unlock_for_posting() {
        body.push_str(&render_post_access_gate(
            board,
            csrf_token,
            &format!("/{}", board.short_name),
            "unlock posting",
        ));
    }

    let return_to = if pagination.page > 1 {
        format!("/{}?page={}", board.short_name, pagination.page)
    } else {
        format!("/{}", board.short_name)
    };
    for summary in summaries {
        body.push_str(r#"<div class="thread-list-entry">"#);
        let preference = thread_preferences
            .get(&summary.thread.id)
            .copied()
            .unwrap_or_default();
        body.push_str(&render_thread_actions(
            &board.short_name,
            &summary.thread,
            csrf_token,
            preference,
            &return_to,
        ));
        body.push_str(&render_thread_summary(
            summary,
            &board.short_name,
            csrf_token,
            admin_csrf_token,
            is_admin,
            board.show_poster_ids,
            board.collapse_greentext,
            if new_activity_enabled {
                thread_badges.get(&summary.thread.id).copied()
            } else {
                None
            },
            user_preferences,
        ));
        body.push_str("</div>");
    }

    // escape_html on board.short_name before embedding in the URL.
    body.push_str(&render_pagination(
        pagination,
        &format!("/{}", escape_html(&board.short_name)),
    ));

    body.push_str(report_modal_script());
    body.push_str(&compress_modal_script(
        board.max_image_size_bytes(),
        board.max_video_size_bytes(),
    ));

    base_layout_with_preferences(
        &format!("/{}/ — {} - Index", board.short_name, board.name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!("/{}", board.short_name),
        user_preferences,
    )
}

// Thread summary (used by board_page)
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "a summary combines thread metadata, preview posts, controls, and visitor state"
)]
/// Renders one thread summary on a board index.
fn render_thread_summary(
    summary: &ThreadSummary,
    board_short: &str,
    csrf_token: &str,
    admin_csrf_token: Option<&str>,
    is_admin: bool,
    show_poster_ids: bool,
    collapse_greentext: bool,
    unread_reply_count: Option<i64>,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    let t = &summary.thread;
    let mut html = String::new();
    let admin_form_csrf = admin_csrf_token.unwrap_or(csrf_token);

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

    crate::templates::append_html(
        &mut html,
        format_args!(
            r#"<div class="thread" id="t{tid}">
<div class="op post" id="p{op_id}">"#,
            tid = t.id,
            op_id = t.op_id.unwrap_or(0)
        ),
    );

    let thread_state_badges = super::thread::render_thread_state_badges(t.sticky, t.locked);

    if let (Some(_file), Some(thumb)) = (&t.op_file, &t.op_thumb) {
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="file-container thread-summary-thumb-wrap"><a href="/{board}/thread/{tid}"><img class="thumb" src="/boards/{th}" loading="lazy" decoding="async" alt="image"></a>{badges}</div>"#,
                board = escape_html(board_short),
                tid = t.id,
                th = escape_html(thumb),
                badges = thread_state_badges
            ),
        );
    } else if let Some(embed_thumb) = t.op_body.as_deref().and_then(embed_thumb_from_body) {
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="file-container thread-summary-thumb-wrap"><a href="/{board}/thread/{tid}"><img class="thumb embed-index-thumb" src="{src}" loading="lazy" decoding="async" alt="video thumbnail"></a>{badges}</div>"#,
                board = escape_html(board_short),
                tid = t.id,
                src = escape_html(&embed_thumb),
                badges = thread_state_badges
            ),
        );
    }

    crate::templates::append_html(
        &mut html,
        format_args!(
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
        ),
    );

    if let Some(subject) = &t.subject {
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="subject"><a href="/{b}/thread/{tid}"><strong>{s}</strong></a></div>"#,
                b = escape_html(board_short),
                tid = t.id,
                s = escape_html(subject)
            ),
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
        crate::templates::append_html(
            &mut html,
            format_args!(r#"<div class="post-body">{truncated}</div>"#),
        );
    }

    let activity_badge = unread_reply_count
        .map(|count| {
            render_new_activity_badge(
                count,
                "new-activity-badge thread-summary-activity-badge",
                "New",
            )
        })
        .unwrap_or_default();
    if !activity_badge.is_empty() {
        crate::templates::append_html(
            &mut html,
            format_args!(r#"<div class="thread-summary-activity-row">{activity_badge}</div>"#),
        );
    }

    crate::templates::append_html(
        &mut html,
        format_args!(
            r#"<div class="thread-footer">
<a href="/{board}/thread/{tid}">[reply] ({n} {word})</a>"#,
            board = escape_html(board_short),
            tid = t.id,
            n = t.reply_count,
            word = if t.reply_count == 1 {
                "reply"
            } else {
                "replies"
            },
        ),
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
        crate::templates::append_html(
            &mut html,
            format_args!(
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
                csrf = escape_html(admin_form_csrf),
                tid = t.id,
                board = escape_html(board_short),
                sticky_act = sticky_act,
                sticky_lbl = sticky_lbl,
                lock_act = lock_act,
                lock_lbl = lock_lbl
            ),
        );
    }

    html.push_str("</div>\n</div>");

    if summary.omitted > 0 {
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="omitted">{} posts omitted. <a href="/{b}/thread/{tid}">view thread</a></div>"#,
                summary.omitted,
                b = escape_html(board_short),
                tid = t.id
            ),
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
                admin_csrf_token: admin_csrf_token.map(str::to_owned),
                show_media: true,
                allow_editing: false, // no edit link on board index previews
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids,
                collapse_greentext,
                thread_state: None,
                thread_op_id: summary.thread.op_id,
                video_audio_muted: user_preferences.video_audio_muted,
            },
            0,
        ));
    }

    html.push_str("</div><hr class=\"thread-sep\">");
    html
}

// Catalog page
#[must_use]
// These flags map directly to render or DB inputs, so bundling them would make the call sites less clear.
#[expect(
    clippy::fn_params_excessive_bools,
    clippy::too_many_lines,
    reason = "the catalog keeps its card grid, forms, navigation, and hidden-view state together"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the catalog consumes distinct filtering, moderation, activity, and visitor contexts"
)]
/// Renders a board's catalog or hidden-thread view.
pub fn catalog_page<S: std::hash::BuildHasher>(
    board: &Board,
    threads: &[Thread],
    pinned_ids: &HashSet<i64, S>,
    hidden_count: usize,
    hidden_view: bool,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    admin_csrf_token: Option<&str>,
    thread_badges: &HashMap<i64, i64, S>,
    new_activity_enabled: bool,
    board_banner_html: &str,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    let bs = escape_html(&board.short_name);
    let bn = escape_html(&board.name);

    let mut body = String::new();
    let admin_form_csrf = admin_csrf_token.unwrap_or(csrf_token);

    if is_admin {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}/catalog">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
                csrf = escape_html(admin_form_csrf),
                board = escape_html(&board.short_name)
            ),
        );
    }

    let nav_archive = format!(r#"<a class="board-nav-link" href="/{bs}/archive">[Archive]</a>"#);
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

    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div class="board-header board-catalog-header" data-activity-page="catalog">
  <div class="catalog-header-left board-catalog-header">
    <h1>/{bs}/  — {bn}{access_badge}{title_suffix}</h1>
    <p class="board-desc">{desc}</p>
  </div>
</div>
{board_banner_html}
<div class="catalog-controls">
  <div class="catalog-control-group">
    <label class="catalog-sort-label" for="catalog-sort">Sort By:</label>
    <select id="catalog-sort" class="catalog-sort-select" data-action="sort-catalog" disabled>
    <option value="bump" selected>bump order</option>
    <option value="replies">reply count</option>
    <option value="created">creation date</option>
    <option value="last_reply">last reply</option>
    </select>
  </div>
  <div class="catalog-control-group">
    <label class="catalog-sort-label" for="catalog-show-comment">Show OP Comment:</label>
    <select id="catalog-show-comment" class="catalog-sort-select" data-action="catalog-show-comment" disabled>
      <option value="on" selected>On</option>
      <option value="off">Off</option>
    </select>
  </div>
  <noscript><p class="form-field-help">Sorting and comment toggles require JavaScript. Threads use bump order with comments shown.</p></noscript>
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
        ),
    );
    if can_post {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r##"<div class="post-toggle-bar centered catalog-toggle-bar">
  <a class="post-toggle-btn" href="#post-form-wrap" data-action="toggle-post-form">[ Start a New Thread ]</a>
</div>
<div class="post-form-wrap" id="post-form-wrap" style="display:none">
  {form}
</div>"##,
                form = super::forms::new_thread_form(
                    &board.short_name,
                    csrf_token,
                    board,
                    None,
                    &if hidden_view {
                        format!("/{}/hidden", board.short_name)
                    } else {
                        format!("/{}/catalog", board.short_name)
                    },
                )
            ),
        );
    } else if board.access_mode.requires_unlock_for_posting() {
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
            if new_activity_enabled {
                thread_badges.get(&t.id).copied()
            } else {
                None
            },
            csrf_token,
            pin_action,
            pin_label,
            menu_hide_action,
            menu_hide_label,
            &return_to,
        ));
    }

    if threads.is_empty() {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<p class="catalog-empty-state">{}</p>"#,
                escape_html(empty_message)
            ),
        );
    }

    body.push_str("</div>");
    body.push_str(report_modal_script());
    body.push_str(&compress_modal_script(
        board.max_image_size_bytes(),
        board.max_video_size_bytes(),
    ));
    base_layout_with_preferences(
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
        user_preferences,
    )
}

// Search results
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "search rendering consumes distinct result, paging, theme, and visitor contexts"
)]
/// Renders paginated search results for a board.
pub fn search_page(
    board: &Board,
    query: &str,
    posts: &[Post],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    collapse_greentext: bool,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    let result_label = if pagination.total >= 10_000 {
        "10,000+ results (refine your query to see more)".to_owned()
    } else if pagination.total == 1 {
        "1 result".to_owned()
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
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<p class="board-search-summary board-search-summary-results">{}</p>"#,
                escape_html(&result_label)
            ),
        );
        for post in posts {
            body.push_str(&super::thread::render_search_post(
                post,
                &board.short_name,
                csrf_token,
                super::thread::RenderPostOpts {
                    show_delete: false,
                    is_admin: false,
                    admin_csrf_token: None,
                    show_media: true,
                    allow_editing: false, // no edit link on search results
                    allow_self_delete: false,
                    owned_post_controls: None,
                    show_poster_ids: board.show_poster_ids,
                    collapse_greentext: board.collapse_greentext,
                    thread_state: None,
                    thread_op_id: None,
                    video_audio_muted: user_preferences.video_audio_muted,
                },
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
    base_layout_with_preferences(
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
        user_preferences,
    )
}

// Archive page
#[must_use]
/// Renders a board's paginated archived-thread list.
pub fn archive_page(
    board: &Board,
    threads: &[Thread],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    user_preferences: crate::templates::UserPreferences,
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
<p class="archive-subtext">Read-only threads, ordered by last bump. This board retains up to {archive_limit} archived threads.</p>
</div>"#,
        bs = bs,
        bn = bn,
        desc = escape_html(&board.description),
        archive_limit = board.max_archived_threads,
    );

    if threads.is_empty() {
        body.push_str(
            r#"<div class="page-box"><p style="color:var(--text-dim)">No archived threads yet.</p></div>"#,
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

    base_layout_with_preferences(
        &format!("/{}/  archive", board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        board.collapse_greentext,
        &format!("/{}/archive", board.short_name),
        user_preferences,
    )
}
