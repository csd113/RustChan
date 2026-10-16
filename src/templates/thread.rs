//! Page templates for thread-level views and individual posts.

use crate::models::{Board, Post, Thread};
use crate::utils::{
    files::format_file_size, redirect::encode_query_component, sanitize::escape_html,
};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashSet};

use super::{
    admin_ban_delete_modal_script, base_layout, base_layout_with_preferences,
    compress_modal_script, fmt_ts, fmt_ts_short, report_modal_script, thread_autoupdate_script,
};

/// Navigation metadata for bounded thread pages.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ThreadWindow {
    /// Exclusive upper post ID for the next older page.
    pub(crate) older_before: Option<i64>,
    /// Historical pages retain their selected range rather than polling forward.
    pub(crate) historical: bool,
}

/// Render links that also work when JavaScript is disabled.
fn render_window_nav(board: &Board, thread: &Thread, window: ThreadWindow) -> String {
    let mut nav = String::new();
    let short = escape_html(&board.short_name);
    if let Some(before) = window.older_before {
        super::append_html(
            &mut nav,
            format_args!(
                r#"<a href="/{short}/thread/{}?before={before}">[ Older replies ]</a> "#,
                thread.id
            ),
        );
    }
    if window.historical {
        super::append_html(
            &mut nav,
            format_args!(
                r#"<a href="/{short}/thread/{}">[ Latest replies ]</a>"#,
                thread.id
            ),
        );
    }
    if nav.is_empty() {
        nav
    } else {
        format!(r#"<nav class="thread-window-nav" aria-label="Reply pages">{nav}</nav>"#)
    }
}

/// Number of seconds during which a poster may edit or delete a new post.
const SELF_ACTION_WINDOW_SECS: i64 = 60;
/// User-facing explanation of the edit and delete window.
const SELF_ACTION_WINDOW_HINT: &str = "available for up to 60 seconds after posting";

/// Time-limited controls available to the author of a post.
#[derive(Debug, Clone)]
pub struct OwnedPostControls {
    /// Unix timestamp at which the controls expire.
    pub expires_at: i64,
}

/// Values used to reopen the edit overlay after a validation error.
#[derive(Debug, Clone)]
pub struct EditOverlayState {
    /// Identifier of the post being edited.
    pub post_id: i64,
    /// Current body text shown in the editor.
    pub body: String,
    /// Optional validation error.
    pub error: Option<String>,
}

/// Renders the live countdown note for edit and delete controls.
fn render_self_action_window_hint(expires_at: i64) -> String {
    format!(
        r#"<span class="self-action-window-note self-delete-countdown" data-role="self-action-countdown" aria-live="polite" data-action-expiry="{expires_at}">{SELF_ACTION_WINDOW_HINT}</span>"#
    )
}

/// Renders a post without self-service or administrative controls.
fn render_post_preview(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    thread_op_id: Option<i64>,
) -> String {
    render_post(
        post,
        board_short,
        csrf_token,
        RenderPostOpts {
            show_delete: false,
            is_admin: false,
            admin_csrf_token: None,
            show_media: true,
            allow_editing: false,
            allow_self_delete: false,
            owned_post_controls: None,
            show_poster_ids: false,
            collapse_greentext: true,
            thread_state: None,
            thread_op_id,
            video_audio_muted: false,
        },
        SELF_ACTION_WINDOW_SECS,
    )
}

#[must_use]
/// Renders the standalone no-JavaScript edit-post page.
pub fn edit_post_page(
    board: &Board,
    thread: &Thread,
    post: &Post,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    error: Option<&str>,
) -> String {
    let mut body = String::new();
    if let Some(msg) = error {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="post-error-banner">&#9888; {}</div>"#,
                escape_html(msg)
            ),
        );
    }

    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div class="page-box self-action-page">
<div class="board-thread-header">/{board}/ — edit post No.{pid}</div>
<p class="self-action-page-note">{hint}</p>
<p><a href="/{board}/thread/{tid}#p{pid}">back to the thread</a></p>
<div class="self-action-preview">
{preview}
</div>
<form class="post-form self-action-form" method="POST" action="/{board}/post/{pid}/edit">
  <input type="hidden" name="_csrf" value="{csrf}">
  <table>
    <tr><td>body</td>
        <td><textarea name="body" aria-label="edit post body" rows="8" maxlength="4096" required>{body_text}</textarea></td></tr>
    <tr><td></td>
        <td><button type="submit">save edit</button>
            <a class="edit-btn" href="/{board}/thread/{tid}#p{pid}">cancel</a></td></tr>
  </table>
</form>
</div>"#,
            board = escape_html(&board.short_name),
            pid = post.id,
            tid = thread.id,
            hint = SELF_ACTION_WINDOW_HINT,
            preview = render_post_preview(post, &board.short_name, csrf_token, thread.op_id),
            csrf = escape_html(csrf_token),
            body_text = escape_html(&post.body),
        ),
    );

    base_layout(
        &format!("/{}/edit post No.{}", board.short_name, post.id),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        board.collapse_greentext,
        &format!("/{}/post/{}/edit", board.short_name, post.id),
    )
}

#[must_use]
/// Renders the standalone no-JavaScript delete-post confirmation page.
pub fn delete_post_page(
    board: &Board,
    thread: &Thread,
    post: &Post,
    csrf_token: &str,
    boards: &[Board],
    current_theme: Option<&str>,
    error: Option<&str>,
) -> String {
    let mut body = String::new();
    if let Some(msg) = error {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="post-error-banner">&#9888; {}</div>"#,
                escape_html(msg)
            ),
        );
    }

    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div class="page-box self-action-page">
<div class="board-thread-header">/{board}/ — delete post No.{pid}</div>
<p class="self-action-page-note">{hint}</p>
<p><a href="/{board}/thread/{tid}#p{pid}">back to the thread</a></p>
<div class="self-action-preview">
{preview}
</div>
<form class="post-form self-action-form" method="POST" action="/{board}/post/{pid}/delete">
  <input type="hidden" name="_csrf" value="{csrf}">
  <p class="self-action-confirm">delete this post permanently?</p>
  <button type="submit" class="del-btn">delete post</button>
  <a class="edit-btn" href="/{board}/thread/{tid}#p{pid}">cancel</a>
</form>
</div>"#,
            board = escape_html(&board.short_name),
            pid = post.id,
            tid = thread.id,
            hint = SELF_ACTION_WINDOW_HINT,
            preview = render_post_preview(post, &board.short_name, csrf_token, thread.op_id),
            csrf = escape_html(csrf_token),
        ),
    );

    base_layout(
        &format!("/{}/delete post No.{}", board.short_name, post.id),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        board.collapse_greentext,
        &format!("/{}/post/{}/delete", board.short_name, post.id),
    )
}

/// Renders the top or bottom thread navigation controls.
fn render_thread_nav(board: &Board, thread: &Thread, is_bottom: bool) -> String {
    let board_short = escape_html(&board.short_name);
    let position = if is_bottom { "bottom" } else { "top" };
    let jump_link = if is_bottom { "#top" } else { "#bottom" };
    let jump_label = if is_bottom { "Top" } else { "Bottom" };
    let return_suffix = if thread.archived { "/archive" } else { "" };
    let refresh_controls = if thread.archived {
        String::new()
    } else {
        format!(
            r#"  <div class="thread-nav-group thread-nav-refresh">
    <noscript><a class="thread-nav-control thread-nav-update" href="/{board_short}/thread/{thread_id}">[ Update now ]</a></noscript>
    <button class="thread-nav-control thread-nav-btn thread-nav-update" type="button" data-action="fetch-updates" data-busy-label="[ Updating… ]">[ Update now ]</button>
    <label class="autoupdate-label">
      <input type="checkbox" data-role="autoupdate-toggle" data-action="autoupdate-toggle">
      <span>Auto refresh</span>
    </label>
  </div>
"#,
            thread_id = thread.id,
        )
    };
    let update_status = if thread.archived {
        ""
    } else if is_bottom {
        r#"<span class="autoupdate-status" data-role="autoupdate-status"></span>"#
    } else {
        // Announce updates once; the bottom copy is visual feedback only.
        r#"<span class="autoupdate-status" data-role="autoupdate-status" role="status"></span>"#
    };
    let nav_class = if is_bottom {
        "board-header thread-nav thread-nav-bottom"
    } else {
        "board-header thread-nav"
    };
    format!(
        r#"<div class="{nav_class}" id="{position}" tabindex="-1" role="group" aria-label="Thread controls at {position}">
  <nav class="thread-nav-group thread-nav-links" aria-label="Thread navigation at {position}">
    <a class="thread-nav-control" href="/{board_short}{return_suffix}">[ Return ]</a>
    <a class="thread-nav-control" href="/{board_short}/catalog">[ Catalog ]</a>
    <a class="thread-nav-control" href="{jump_link}">[ {jump_label} ]</a>
  </nav>
{refresh_controls}  <div class="thread-nav-group thread-nav-state">
    {update_status}
    <span class="thread-reply-stat" title="Reply count"><span class="thread-reply-stat-label">Replies:</span> <span data-role="thread-reply-count">{reply_count}</span></span>
  </div>
</div>
"#,
        nav_class = nav_class,
        jump_link = jump_link,
        jump_label = jump_label,
        reply_count = thread.reply_count,
    )
}

#[must_use]
/// Renders sticky, locked, and archived state badges.
pub fn render_thread_state_badges_full(sticky: bool, locked: bool, archived: bool) -> String {
    let mut badges = String::new();

    if sticky {
        badges.push_str(
            r#"<span class="thread-state-badge thread-state-badge-pin" title="Pinned" aria-label="Pinned">&#128204;</span>"#,
        );
    }

    if archived {
        badges.push_str(
            r#"<span class="thread-state-badge thread-state-badge-archive" title="Archived" aria-label="Archived">&#128190;</span>"#,
        );
    } else if locked {
        badges.push_str(
            r#"<span class="thread-state-badge thread-state-badge-lock" title="Locked" aria-label="Locked">&#128274;</span>"#,
        );
    }

    if badges.is_empty() {
        String::new()
    } else {
        format!(r#"<span class="thread-state-badges">{badges}</span>"#)
    }
}

#[must_use]
/// Renders sticky and locked state badges for a live thread.
pub fn render_thread_state_badges(sticky: bool, locked: bool) -> String {
    render_thread_state_badges_full(sticky, locked, false)
}

#[must_use]
/// Renders the archived state badge and an optional sticky badge.
pub fn render_archive_state_badges(sticky: bool) -> String {
    let mut badges = String::new();

    if sticky {
        badges.push_str(
            r#"<span class="thread-state-badge thread-state-badge-pin" title="Pinned" aria-label="Pinned">&#128204;</span>"#,
        );
    }

    badges.push_str(
        r#"<span class="thread-state-badge thread-state-badge-archive" title="Archived" aria-label="Archived">&#128190;</span>"#,
    );

    format!(r#"<span class="thread-state-badges">{badges}</span>"#)
}

// Thread page
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "thread rendering consumes distinct moderation, poll, form, theme, and visitor contexts"
)]
/// Renders a complete thread page.
pub fn thread_page(
    board: &Board,
    thread: &Thread,
    posts: &[Post],
    owned_post_controls: &BTreeMap<i64, OwnedPostControls>,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    admin_csrf_token: Option<&str>,
    poll: Option<&crate::models::PollData>,
    error: Option<&str>,
    success: Option<&str>,
    reply_prefill: Option<&super::forms::PostFormState>,
    edit_overlay_state: Option<&EditOverlayState>,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
    thread_preference: crate::db::UserThreadPreference,
    user_preferences: crate::templates::UserPreferences,
) -> String {
    thread_page_window(
        board,
        thread,
        posts,
        owned_post_controls,
        csrf_token,
        boards,
        is_admin,
        admin_csrf_token,
        poll,
        error,
        success,
        reply_prefill,
        edit_overlay_state,
        current_theme,
        collapse_greentext,
        can_post,
        thread_preference,
        user_preferences,
        ThreadWindow::default(),
    )
}

/// Render a bounded thread window while preserving the normal document hooks.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the thread document keeps navigation, posts, poll, forms, and modals together"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "thread rendering consumes distinct moderation, poll, form, theme, and visitor contexts"
)]
pub(crate) fn thread_page_window(
    board: &Board,
    thread: &Thread,
    posts: &[Post],
    owned_post_controls: &BTreeMap<i64, OwnedPostControls>,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    admin_csrf_token: Option<&str>,
    poll: Option<&crate::models::PollData>,
    error: Option<&str>,
    success: Option<&str>,
    reply_prefill: Option<&super::forms::PostFormState>,
    edit_overlay_state: Option<&EditOverlayState>,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
    thread_preference: crate::db::UserThreadPreference,
    user_preferences: crate::templates::UserPreferences,
    window: ThreadWindow,
) -> String {
    let mut body = String::new();
    let admin_form_csrf = admin_csrf_token.unwrap_or(csrf_token);
    let admin_toolbar = if is_admin {
        let sticky_action = if thread.sticky {
            ("unsticky", "&#128204; Unsticky")
        } else {
            ("sticky", "&#128204; Sticky")
        };
        let lock_action = if thread.locked {
            ("unlock", "&#128275; Unlock")
        } else {
            ("lock", "&#128274; Lock")
        };
        format!(
            r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="action" value="{sticky_act}">
<input type="hidden" name="board" value="{board}">
<button type="submit" class="admin-toolbar-btn"{sticky_disabled}>{sticky_lbl}</button>
</form>
<form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="action" value="{lock_act}">
<input type="hidden" name="board" value="{board}">
<button type="submit" class="admin-toolbar-btn"{lock_disabled}>{lock_lbl}</button>
</form>
{archive_btn}
<form method="POST" action="/admin/thread/delete" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="board" value="{board}">
<button type="submit" class="admin-toolbar-btn admin-toolbar-danger"
        data-confirm="Delete this entire thread and all its posts?">&#x2715; delete thread</button>
</form>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}/thread/{tid}">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
            csrf = escape_html(admin_form_csrf),
            tid = thread.id,
            board = escape_html(&board.short_name),
            sticky_disabled = if thread.archived && !thread.sticky {
                " disabled"
            } else {
                ""
            },
            lock_disabled = if thread.archived { " disabled" } else { "" },
            sticky_act = sticky_action.0,
            sticky_lbl = sticky_action.1,
            lock_act = lock_action.0,
            lock_lbl = lock_action.1,
            archive_btn = if thread.archived {
                String::new()
            } else {
                format!(
                    r#"<form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="action" value="archive">
<input type="hidden" name="board" value="{board}">
<button type="submit" class="admin-toolbar-btn"
        data-confirm="Archive this thread? It will be locked and moved to the board archive.">
  &#128451; Archive Thread
</button>
</form>"#,
                    csrf = escape_html(admin_form_csrf),
                    tid = thread.id,
                    board = escape_html(&board.short_name),
                )
            }
        )
    } else {
        String::new()
    };

    if let Some(msg) = success {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="post-success-banner">{}</div>"#,
                escape_html(msg)
            ),
        );
    }

    if let Some(msg) = error {
        crate::templates::append_html(
            &mut body,
            format_args!(
                r#"<div class="post-error-banner">&#9888; {}</div>"#,
                escape_html(msg)
            ),
        );
    }

    let thread_notice = if thread.archived {
        r#"<div class="notice locked-notice">This thread is archived. - You cannot reply anymore.</div>"#
    } else if thread.locked {
        r#"<div class="notice locked-notice">this thread is locked — no new replies allowed</div>"#
    } else {
        ""
    };

    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div class="thread-board-banner board-thread-header">/{s}/ — {bn}{access_badge}</div>
{admin_toolbar}
{top_nav}"#,
            s = escape_html(&board.short_name),
            bn = escape_html(&board.name),
            access_badge = super::board::board_access_badge(board),
            admin_toolbar = admin_toolbar,
            top_nav = render_thread_nav(board, thread, false)
        ),
    );
    {
        body.push_str(r#"<div class="thread-actions-row">"#);
        if thread_preference.hidden {
            body.push_str(r#"<span class="notice">Hidden from your index and catalog. This direct link remains available.</span>"#);
        }
        body.push_str(&super::board::render_thread_actions(
            &board.short_name,
            thread,
            csrf_token,
            thread_preference,
            &format!("/{}/thread/{}", board.short_name, thread.id),
        ));
        body.push_str("</div>");
    }
    body.push_str(thread_notice);

    if let Some(pd) = poll {
        body.push_str(&render_poll(pd, thread, &board.short_name, csrf_token));
    }

    body.push_str(&render_window_nav(board, thread, window));
    let visible_ids = posts.iter().map(|post| post.id).collect::<HashSet<_>>();
    let last_post_id = posts.iter().map(|p| p.id).max().unwrap_or(0);
    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div id="thread-posts" data-activity-page="thread" data-thread-id="{tid}" data-board="{board}" data-last-id="{last}" data-history-page="{history}" data-locked="{locked}" data-sticky="{sticky}" data-archived="{archived}">"#,
            tid = thread.id,
            board = escape_html(&board.short_name),
            last = last_post_id,
            history = window.historical,
            locked = thread.locked,
            sticky = thread.sticky,
            archived = thread.archived,
        ),
    );
    for post in posts {
        let post_html = render_post(
            post,
            &board.short_name,
            csrf_token,
            RenderPostOpts {
                show_delete: true,
                is_admin,
                admin_csrf_token: admin_csrf_token.map(str::to_owned),
                show_media: true,
                allow_editing: board.allow_editing,
                allow_self_delete: board.allow_self_delete
                    && (!post.is_op || thread.reply_count == 0),
                owned_post_controls: if thread.locked || thread.archived {
                    None
                } else {
                    owned_post_controls.get(&post.id).cloned()
                },
                show_poster_ids: board.show_poster_ids,
                collapse_greentext: board.collapse_greentext,
                thread_state: Some((thread.sticky, thread.locked, thread.archived)),
                thread_op_id: thread.op_id,
                video_audio_muted: user_preferences.video_audio_muted,
            },
            SELF_ACTION_WINDOW_SECS,
        );
        body.push_str(&crate::utils::sanitize::route_missing_quote_links(
            &post_html,
            &board.short_name,
            &visible_ids,
        ));
    }

    body.push_str("</div>\n");
    body.push_str(&render_window_nav(board, thread, window));
    body.push_str(&render_edit_overlay(
        board,
        thread.id,
        csrf_token,
        edit_overlay_state,
    ));
    if is_admin {
        body.push_str(admin_ban_delete_modal_script());
    }

    if !thread.locked && !thread.archived && can_post {
        let form_html = super::forms::reply_form(
            &board.short_name,
            thread.id,
            csrf_token,
            board,
            reply_prefill,
        );
        let show_post_form = error.is_some() || reply_prefill.is_some();
        crate::templates::append_html(
            &mut body,
            format_args!(
                r##"<div class="post-toggle-bar reply">
  <a class="post-toggle-btn" href="#post-form-wrap" data-action="toggle-post-form">[ Reply ]</a>
</div>
<div class="{post_form_class}" id="post-form-wrap" style="{post_form_style}">
  {form_html}
</div>"##,
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
    } else if !thread.locked && !thread.archived && board.access_mode.requires_unlock_for_posting()
    {
        body.push_str(&super::board::render_post_access_gate(
            board,
            csrf_token,
            &format!("/{}/thread/{}", board.short_name, thread.id),
            "unlock posting",
        ));
    }
    body.push_str(&render_thread_nav(board, thread, true));

    body.push_str(&compress_modal_script(
        board.max_image_size_bytes(),
        board.max_video_size_bytes(),
    ));
    body.push_str(report_modal_script());
    body.push_str(thread_autoupdate_script());

    // Board-specific values use data attributes because the CSP intentionally
    // forbids inline scripts.

    // Video embed + draft autosave config (data attributes only)
    let embed_enabled_attr = if board.allow_video_embeds { "1" } else { "0" };
    let draft_key = format!("rustchan_draft_{}_{}", board.short_name, thread.id);
    crate::templates::append_html(
        &mut body,
        format_args!(
            r#"<div id="thread-config"
     data-embed-enabled="{embed_enabled}"
     data-draft-key="{draft_key}"
     style="display:none" aria-hidden="true"></div>"#,
            embed_enabled = embed_enabled_attr,
            draft_key = escape_html(&draft_key)
        ),
    );

    base_layout_with_preferences(
        &format!(
            "/{}/ - {}",
            board.short_name,
            thread.subject.as_deref().unwrap_or("thread")
        ),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        current_theme,
        Some(&board.default_theme),
        collapse_greentext,
        &format!("/{}/thread/{}", board.short_name, thread.id),
        user_preferences,
    )
}

// Poll renderer
/// Formats the remaining voting window for a poll's status label.
fn poll_expiry_label(pd: &crate::models::PollData) -> String {
    let now = chrono::Utc::now().timestamp();
    let time_left = pd.poll.expires_at.saturating_sub(now);
    if pd.is_expired {
        "closed".to_owned()
    } else if time_left < 3600 {
        format!("closes in {}m", time_left / 60)
    } else if time_left < 86400 {
        format!(
            "closes in {}h {}m",
            time_left / 3600,
            (time_left % 3600) / 60
        )
    } else {
        format!("closes {}", fmt_ts(pd.poll.expires_at))
    }
}

/// Renders a poll voting form or its results.
fn render_poll(
    pd: &crate::models::PollData,
    thread: &Thread,
    board_short: &str,
    csrf_token: &str,
) -> String {
    let expires_str = if thread.archived {
        "archived".to_owned()
    } else {
        poll_expiry_label(pd)
    };
    let show_results = thread.archived || pd.is_expired || pd.user_voted_option.is_some();

    let mut html = format!(
        r#"<div class="poll-container" id="poll">
<div class="poll-header">
  <span class="poll-icon">📊</span>
  <span class="poll-question">{q}</span>
  <span class="poll-status {status_class}">[{expires}]</span>
</div>"#,
        q = escape_html(&pd.poll.question),
        status_class = if thread.archived || pd.is_expired {
            "poll-closed"
        } else {
            "poll-open"
        },
        // escape_html for defensive correctness — expires_str is
        // derived from integer arithmetic and fmt_ts today, but this guard
        // ensures any future changes to expires_str can't inject HTML.
        expires = escape_html(&expires_str),
    );

    if show_results {
        let total = pd.total_votes.max(1);
        html.push_str(r#"<div class="poll-results">"#);
        for opt in &pd.options {
            let pct = opt
                .vote_count
                .max(0)
                .saturating_mul(100)
                .saturating_add(total / 2)
                .checked_div(total)
                .unwrap_or(0);
            let is_voted = pd.user_voted_option == Some(opt.id);
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<div class="poll-option-result{voted}">
  <div class="poll-option-label">
    {check}<span class="poll-opt-text">{text}</span>
    <span class="poll-opt-count">{votes} ({pct}%)</span>
  </div>
  <div class="poll-bar-track"><div class="poll-bar-fill" style="width:{pct}%"></div></div>
</div>"#,
                    voted = if is_voted { " user-voted" } else { "" },
                    check = if is_voted { "✓ " } else { "" },
                    text = escape_html(&opt.text),
                    votes = opt.vote_count,
                    pct = pct
                ),
            );
        }
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="poll-total">{} total vote{}</div></div>"#,
                pd.total_votes,
                if pd.total_votes == 1 { "" } else { "s" }
            ),
        );
    } else {
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<form class="poll-vote-form" method="POST" action="/vote">
<input type="hidden" name="_csrf"     value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="board"     value="{board}">"#,
                csrf = escape_html(csrf_token),
                tid = thread.id,
                board = escape_html(board_short)
            ),
        );
        for opt in &pd.options {
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<label class="poll-vote-option">
  <input type="radio" name="option_id" value="{id}" required>
  <span class="poll-opt-text">{text}</span>
</label>"#,
                    id = opt.id,
                    text = escape_html(&opt.text)
                ),
            );
        }
        html.push_str(
            r#"<button type="submit" class="poll-vote-btn">[ Cast Vote ]</button></form>"#,
        );
    }

    html.push_str("</div>");
    html
}

// Single post renderer
/// Options that control which controls are rendered for a post.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag independently controls a post-rendering capability"
)]
#[derive(Clone, Debug, Default)]
pub struct RenderPostOpts {
    /// Whether to show the legacy delete control.
    pub show_delete: bool,
    /// Whether administrative controls are available.
    pub is_admin: bool,
    /// CSRF token used by administrative forms.
    pub admin_csrf_token: Option<String>,
    /// Whether attached media should be displayed.
    pub show_media: bool,
    /// Whether the author may edit the post.
    pub allow_editing: bool,
    /// Whether the author may delete the post.
    pub allow_self_delete: bool,
    /// Expiring author controls for this post.
    pub owned_post_controls: Option<OwnedPostControls>,
    /// Whether to derive and display a per-thread poster ID.
    pub show_poster_ids: bool,
    /// Whether greentext blocks start collapsed.
    pub collapse_greentext: bool,
    /// Sticky, locked, and archived state for an opening post.
    pub thread_state: Option<(bool, bool, bool)>,
    /// Opening-post identifier used to annotate replies directed at the author.
    pub thread_op_id: Option<i64>,
    /// Whether video and audio elements start muted.
    pub video_audio_muted: bool,
}

/// Base64 alphabet used for compact poster identifiers.
const POSTER_ID_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Resolves a six-bit value to the corresponding poster-ID character.
fn poster_id_char(index: u8) -> char {
    POSTER_ID_ALPHABET
        .get(usize::from(index))
        .copied()
        .map_or('A', char::from)
}

/// Encodes six hash bytes as an eight-character poster identifier.
fn encode_poster_id(bytes: [u8; 6]) -> String {
    let mut out = String::with_capacity(8);
    let [b0, b1, b2, b3, b4, b5] = bytes;
    out.push(poster_id_char(b0 >> 2));
    out.push(poster_id_char(((b0 & 0x03) << 4) | (b1 >> 4)));
    out.push(poster_id_char(((b1 & 0x0f) << 2) | (b2 >> 6)));
    out.push(poster_id_char(b2 & 0x3f));
    out.push(poster_id_char(b3 >> 2));
    out.push(poster_id_char(((b3 & 0x03) << 4) | (b4 >> 4)));
    out.push(poster_id_char(((b4 & 0x0f) << 2) | (b5 >> 6)));
    out.push(poster_id_char(b5 & 0x3f));
    out
}

/// Derives a thread-scoped poster identifier when the feature is enabled.
fn render_poster_id(post: &Post, show_poster_ids: bool) -> Option<String> {
    if !show_poster_ids {
        return None;
    }
    let ip_hash = post.ip_hash.as_deref()?;
    let mut hasher = Sha256::new();
    hasher.update(crate::config::CONFIG.cookie_secret.as_bytes());
    hasher.update(b":poster-id:");
    hasher.update(post.thread_id.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(ip_hash.as_bytes());
    let digest = hasher.finalize();
    let mut short = [0u8; 6];
    let head = digest.get(..6)?;
    short.copy_from_slice(head);
    Some(encode_poster_id(short))
}

/// Derives deterministic CSS colors for a poster-ID chip.
fn poster_id_chip_style(poster_id: &str) -> String {
    const POSTER_CHIP_HUES: [u16; 18] = [
        0, 210, 122, 32, 282, 168, 338, 52, 196, 96, 16, 248, 146, 308, 72, 184, 228, 356,
    ];

    let mut hasher = Sha256::new();
    hasher.update(b":poster-id-chip:");
    hasher.update(poster_id.as_bytes());
    let digest = hasher.finalize();
    let hue_index = digest.first().map_or(0, |byte| {
        usize::from(*byte)
            .checked_rem(POSTER_CHIP_HUES.len())
            .unwrap_or(0)
    });
    let hue = POSTER_CHIP_HUES.get(hue_index).copied().unwrap_or(200);
    let accent_lightness =
        60_u16.saturating_add(digest.get(1).map_or(0, |byte| u16::from(*byte) % 8));
    let background_lightness =
        19_u16.saturating_add(digest.get(2).map_or(0, |byte| u16::from(*byte) % 8));
    let shadow_strength =
        34_u16.saturating_add(digest.get(3).map_or(0, |byte| u16::from(*byte) % 18));
    format!(
        concat!(
            "--poster-chip-accent:hsl({} 88% {}% / 0.98);",
            "--poster-chip-bg:hsl({} 64% {}% / 0.97);",
            "--poster-chip-fg:hsl({} 100% 97% / 0.99);",
            "--poster-chip-shadow:color-mix(in srgb, var(--poster-chip-accent) {}%, transparent);"
        ),
        hue, accent_lightness, hue, background_lightness, hue, shadow_strength
    )
}

/// Adds an `(OP)` label to same-thread links targeting the opening post.
fn annotate_op_quotelinks(body_html: &str, thread_op_id: Option<i64>) -> String {
    let Some(op_id) = thread_op_id else {
        return body_html.to_owned();
    };
    let target = format!(
        r##"<a href="#p{op_id}" class="quotelink" data-pid="{op_id}">&gt;&gt;{op_id}</a>"##
    );
    let replacement = format!(
        r##"<a href="#p{op_id}" class="quotelink" data-pid="{op_id}">&gt;&gt;{op_id}<span class="quotelink-op-label">(OP)</span></a>"##
    );
    body_html.replace(&target, &replacement)
}

/// Number of filename stem characters displayed before truncation.
const FILE_NAME_STEM_PREFIX_DISPLAY_CHARS: usize = 20;
/// Marker inserted between a truncated filename stem and its extension.
const FILE_NAME_TRUNCATION_MARKER: &str = "(...)";

/// Renders a media thumbnail with a client-side fallback.
fn render_media_thumb(
    img_class: &str,
    fallback_class: &str,
    src: &str,
    alt: &str,
    loading: &str,
    fallback_text: &str,
) -> String {
    format!(
        r#"<img class="{img_class}" src="/boards/{src}" loading="{loading}" decoding="async" alt="{alt}" data-media-thumb="1">
<div class="{fallback_class} media-thumb-fallback" hidden>{fallback_text}</div>"#,
        img_class = escape_html(img_class),
        fallback_class = escape_html(fallback_class),
        src = escape_html(src),
        loading = escape_html(loading),
        alt = escape_html(alt),
        fallback_text = escape_html(fallback_text),
    )
}

/// Truncates a filename stem without splitting Unicode scalar values.
fn truncate_file_name_stem(input: &str) -> String {
    if input.chars().count() <= FILE_NAME_STEM_PREFIX_DISPLAY_CHARS {
        return input.to_owned();
    }

    let prefix: String = input
        .chars()
        .take(FILE_NAME_STEM_PREFIX_DISPLAY_CHARS)
        .collect();
    format!("{prefix}{FILE_NAME_TRUNCATION_MARKER}")
}

/// Builds a bounded display filename while retaining its extension.
fn display_file_name(name: &str) -> String {
    match name.rfind('.') {
        Some(dot_idx) if dot_idx > 0 => {
            let (stem, ext) = name.split_at(dot_idx);
            if stem.chars().count() > FILE_NAME_STEM_PREFIX_DISPLAY_CHARS {
                format!("{}{}", truncate_file_name_stem(stem), ext)
            } else {
                name.to_owned()
            }
        }
        _ => truncate_file_name_stem(name),
    }
}

/// Renders an escaped download link with the full filename in its tooltip.
fn render_file_link(file_path: &str, file_name: &str) -> String {
    let display_name = display_file_name(file_name);
    format!(
        r#"<a href="/boards/{file_path}" target="_blank" rel="noreferrer" title="{full_name}">{display_name}</a>"#,
        file_path = escape_html(file_path),
        full_name = escape_html(file_name),
        display_name = escape_html(&display_name),
    )
}

/// Resolves media type from the normalized MIME type before stored metadata.
fn effective_media_type(post: &Post) -> crate::models::MediaType {
    if let Some(mime_media) = post
        .mime_type
        .as_deref()
        .map(crate::models::MediaType::from_mime)
        .filter(|media| *media != crate::models::MediaType::Other)
    {
        return mime_media;
    }

    post.media_type.unwrap_or(crate::models::MediaType::Other)
}

#[derive(Clone, Copy)]
/// Selects whether post links act within this page or open their thread.
enum PostLinkContext {
    /// Keeps fragment links and the inline reply action.
    Inline,
    /// Opens a complete thread from a partial set of search results.
    Thread,
}

/// Renders a post with inline quote and reply controls.
#[must_use]
pub fn render_post(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    opts: RenderPostOpts,
    edit_window_secs: i64,
) -> String {
    render_post_with_context(
        post,
        board_short,
        csrf_token,
        opts,
        edit_window_secs,
        PostLinkContext::Inline,
    )
}

/// Renders a search result with links back to the complete thread context.
pub(super) fn render_search_post(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    opts: RenderPostOpts,
) -> String {
    render_post_with_context(
        post,
        board_short,
        csrf_token,
        opts,
        0,
        PostLinkContext::Thread,
    )
}

/// Renders a single post as HTML in its current navigation context.
///
/// # Trust boundary
/// `post.body_html` is inserted **raw** (unescaped) because it is pre-rendered,
/// sanitised HTML produced by the markup pipeline before storage. Every other
/// user-supplied string in this function must continue to pass through
/// `escape_html()`. Do not change the `body_html` insertion without ensuring
/// the upstream sanitiser is still in place.
#[expect(
    clippy::cognitive_complexity,
    reason = "post rendering keeps its security-sensitive escaping and media branches together"
)]
#[expect(
    clippy::too_many_lines,
    reason = "post rendering keeps all media variants and security-sensitive escaping together"
)]
fn render_post_with_context(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    opts: RenderPostOpts,
    _edit_window_secs: i64,
    link_context: PostLinkContext,
) -> String {
    let RenderPostOpts {
        show_delete,
        is_admin,
        admin_csrf_token,
        show_media,
        allow_editing,
        allow_self_delete,
        owned_post_controls,
        show_poster_ids,
        collapse_greentext,
        thread_state,
        thread_op_id,
        video_audio_muted,
    } = opts;
    let poster_id = render_poster_id(post, show_poster_ids);
    let poster_id_html = poster_id.as_ref().map_or_else(String::new, |poster_id| {
        let chip_style = poster_id_chip_style(poster_id);
        format!(
            r#" <button type="button" class="poster-id-btn" style="{chip_style}" data-action="toggle-poster-highlight" data-thread-id="{thread_id}" data-poster-id="{poster_id}" disabled>ID: {poster_id}</button>"#,
            chip_style = chip_style,
            thread_id = post.thread_id,
            poster_id = escape_html(poster_id),
        )
    });
    let tripcode_html = post
        .tripcode
        .as_ref()
        .map(|t| format!(r#"<span class="tripcode">!{}</span>"#, escape_html(t)))
        .unwrap_or_default();

    let op_class = if post.is_op { " op" } else { " reply" };
    let poster_attr = poster_id.as_ref().map_or_else(String::new, |poster_id| {
        format!(r#" data-poster-id="{}""#, escape_html(poster_id))
    });

    let subject_html = post.subject.as_ref().map_or_else(String::new, |subject| {
        format!(
            r#"<span class="subject"><strong>{}</strong></span>"#,
            escape_html(subject)
        )
    });
    let post_state_badges = if post.is_op {
        thread_state
            .map(|(sticky, locked, archived)| {
                render_thread_state_badges_full(sticky, locked, archived)
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let media_processing_badge = match post.media_processing_state.as_deref() {
        Some("pending") => {
            r#" <span class="post-edited" title="Background media processing is still running.">(processing media)</span>"#.to_owned()
        }
        Some("failed") => {
            let title = post
                .media_processing_error
                .as_deref()
                .map_or_else(
                    || "Background media processing failed.".to_owned(),
                    escape_html,
                );
            format!(
                r#" <span class="post-edited" title="{title}">(media processing failed)</span>"#
            )
        }
        Some("pruned") => {
            r#" <span class="post-edited" title="Original file removed by active media pruning.">(original file removed)</span>"#.to_owned()
        }
        _ => String::new(),
    };
    let media_processing_state_attr = post
        .media_processing_state
        .as_deref()
        .map(|state| format!(r#" data-media-processing-state="{}""#, escape_html(state)))
        .unwrap_or_default();

    let post_link = match link_context {
        PostLinkContext::Inline => format!(
            r##"href="#p{}" data-action="append-reply" data-id="{}""##,
            post.id, post.id
        ),
        PostLinkContext::Thread => format!(
            r#"href="/{}/thread/{}#p{}""#,
            escape_html(board_short),
            post.thread_id,
            post.id
        ),
    };
    let mut html = format!(
        r#"<div class="post{op_class}" id="p{id}" data-thread-id="{thread_id}"{poster_attr}{media_processing_state_attr}>
<div class="post-meta">
{subject_html}<strong class="name">{name}</strong>{tripcode}{poster_id_html}
<span class="post-time" data-utc="{ts}">{time}</span>
<a class="post-num" {post_link}>No.{id}</a>{post_state_badges}{media_processing_badge}
<span class="backrefs" id="backrefs-{id}"></span>
</div>"#,
        op_class = op_class,
        id = post.id,
        thread_id = post.thread_id,
        poster_attr = poster_attr,
        media_processing_state_attr = media_processing_state_attr,
        subject_html = subject_html,
        name = escape_html(&post.name),
        tripcode = tripcode_html,
        poster_id_html = poster_id_html,
        ts = post.created_at,
        time = fmt_ts_short(post.created_at),
        post_state_badges = post_state_badges,
        media_processing_badge = media_processing_badge,
    );

    let primary_media_type = effective_media_type(post);
    let thumb_loading = if post.is_op { "eager" } else { "lazy" };
    let original_pruned =
        post.media_processing_state.as_deref() == Some(crate::db::MEDIA_ORIGINAL_PRUNED);

    // Image / Video / Audio
    if show_media {
        if original_pruned {
            let name_str = post.file_name.as_deref().unwrap_or("file");
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let thumb_html = post
                .thumb_path
                .as_deref()
                .map_or_else(String::new, |thumb| {
                    format!(
                        r#"<div class="media-preview media-preview-pruned">{}</div>"#,
                        render_media_thumb(
                            "thumb",
                            "thumb",
                            thumb,
                            "pruned media preview",
                            thumb_loading,
                            "original removed",
                        )
                    )
                });
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<div class="file-container media-pruned">
<div class="file-info">
  File: <span title="{orig}">{name}</span> ({sz})
  <span class="post-edited" title="Original full-size file was removed by active media pruning.">original file removed</span>
</div>
{thumb_html}
</div>"#,
                    orig = escape_html(name_str),
                    name = escape_html(&display_file_name(name_str)),
                    sz = escape_html(&size_str),
                    thumb_html = thumb_html,
                ),
            );
        } else if let (Some(file), Some(thumb)) = (&post.file_path, &post.thumb_path) {
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let name_str = post.file_name.as_deref().unwrap_or("file");
            let file_link = render_file_link(file, name_str);
            let mime = post
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let is_audio = matches!(primary_media_type, crate::models::MediaType::Audio);
            let is_video = matches!(primary_media_type, crate::models::MediaType::Video);
            let is_pdf = matches!(primary_media_type, crate::models::MediaType::Pdf);

            let combo_audio = if matches!(primary_media_type, crate::models::MediaType::Image) {
                match (&post.audio_file_path, &post.audio_mime_type) {
                    (Some(aud_file), Some(aud_mime)) => Some((
                        aud_file.as_str(),
                        aud_mime.as_str(),
                        post.audio_file_name.as_deref().unwrap_or("audio"),
                        post.audio_file_size
                            .map(format_file_size)
                            .unwrap_or_default(),
                    )),
                    _ => None,
                }
            } else {
                None
            };

            if is_audio {
                crate::templates::append_html(
                    &mut html,
                    format_args!(
                        r#"<div class="file-container audio-container">
<div class="file-info">
  File: {file_link} ({sz})
</div>
<div class="audio-thumb">
  {thumb_html}
</div>
<audio controls preload="none" class="audio-player" data-audio-title="{orig}">
  <source src="/boards/{f}" type="{mime}">
  Your browser does not support the audio element.
</audio>
</div>"#,
                        file_link = file_link,
                        f = escape_html(file),
                        thumb_html = render_media_thumb(
                            "thumb",
                            "thumb",
                            thumb,
                            "audio",
                            thumb_loading,
                            "preview unavailable",
                        ),
                        orig = escape_html(name_str),
                        sz = escape_html(&size_str),
                        mime = escape_html(mime)
                    ),
                );
            } else if is_video {
                crate::templates::append_html(
                    &mut html,
                    format_args!(
                        r#"<div class="file-container video-container">
<div class="file-info">
  File: {file_link} ({sz})
  <button type="button" class="media-close-btn" data-action="collapse-media" style="display:none" aria-label="Collapse media">&#x2715; close</button>
</div>
<a class="media-preview video-preview" data-action="expand-media" href="/boards/{f}" title="click to play" aria-expanded="false">
  {thumb_html}
  <div class="media-expand-overlay">&#9654;</div>
</a>
<video class="media-expanded media-expanded-video" controls preload="none" playsinline webkit-playsinline{muted_attr} style="display:none">
  <source src="/boards/{f}" type="{mime}">
</video>
</div>"#,
                        file_link = file_link,
                        f = escape_html(file),
                        thumb_html = render_media_thumb(
                            "thumb",
                            "thumb",
                            thumb,
                            "video thumbnail",
                            thumb_loading,
                            "preview unavailable",
                        ),
                        sz = escape_html(&size_str),
                        mime = escape_html(mime),
                        muted_attr = if video_audio_muted { " muted" } else { "" },
                    ),
                );
            } else if is_pdf {
                crate::templates::append_html(
                    &mut html,
                    format_args!(
                        r#"<div class="file-container pdf-container">
<div class="file-info">
  File: {file_link} ({sz})
  <button type="button" class="media-close-btn" data-action="collapse-media" style="display:none" aria-label="Collapse media">&#x2715; close</button>
</div>
<a class="media-preview pdf-preview" data-action="expand-media" href="/boards/{f}" title="click to expand" aria-expanded="false">
  {thumb_html}
  <div class="media-expand-overlay">&#x2922;</div>
</a>
<iframe class="media-expanded media-expanded-pdf" src="about:blank" data-src="/boards/{f}" title="{orig}" style="display:none"></iframe>
</div>"#,
                        file_link = file_link,
                        f = escape_html(file),
                        thumb_html = render_media_thumb(
                            "thumb",
                            "thumb",
                            thumb,
                            "pdf preview",
                            thumb_loading,
                            "Open PDF",
                        ),
                        sz = escape_html(&size_str),
                        orig = escape_html(name_str)
                    ),
                );
            } else {
                // Image
                // Keep the preview as an inline expansion control rather than
                // a new tab so a slow JS load or missed handler does not
                // strand the user in a raw-file window.
                crate::templates::append_html(&mut html, format_args!(
                    r#"<div class="file-container{combo_class}">
<div class="file-info">
  File: {file_link} ({sz})
  <button type="button" class="media-close-btn" data-action="collapse-media" style="display:none" aria-label="Collapse media">&#x2715; close</button>
</div>
<a class="media-preview image-preview" data-action="expand-media" href="/boards/{f}" title="click to expand" aria-expanded="false">
  {thumb_html}
  <div class="media-expand-overlay">&#x2922;</div>
</a>
<img class="media-expanded media-expanded-image" src="" data-src="/boards/{f}" style="display:none"
     alt="image" draggable="false">
{audio_combo_html}
</div>"#,
                    combo_class = if combo_audio.is_some() {
                        " image-audio-combo"
                    } else {
                        ""
                    },
                    file_link = file_link,
                    f = escape_html(file),
                    thumb_html = render_media_thumb(
                        "thumb",
                        "thumb",
                        thumb,
                        "image",
                        thumb_loading,
                        "preview unavailable",
                    ),
                    sz = escape_html(&size_str),
                    audio_combo_html = combo_audio.map_or_else(
                        String::new,
                        |(aud_file, aud_mime, aud_name, aud_size)| {
                            let audio_link = render_file_link(aud_file, aud_name);
                            format!(
                                r#"<div class="audio-combo audio-combo-inline">
<div class="file-info">
  Audio: {audio_link} ({sz})
</div>
<audio controls preload="none" class="audio-player audio-player-combo" data-audio-title="{orig}" data-artwork-src="/boards/{th}">
  <source src="/boards/{f}" type="{mime}">
  Your browser does not support the audio element.
</audio>
</div>"#,
                                audio_link = audio_link,
                                f = escape_html(aud_file),
                                th = escape_html(thumb),
                                orig = escape_html(aud_name),
                                sz = escape_html(&aud_size),
                                mime = escape_html(aud_mime)
                            )
                        }
                    )
                ));
            }
        } else if let Some(file) = &post.file_path {
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let name_str = post.file_name.as_deref().unwrap_or("file");
            let file_link = render_file_link(file, name_str);
            let status_note = match post.media_processing_state.as_deref() {
                Some("pending") => "Preview still processing.",
                Some("failed") => "Preview generation failed; original file is still available.",
                _ => "Preview unavailable.",
            };
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<div class="file-container">
<div class="file-info">
  File: {file_link} ({sz})
  <span class="post-edited" title="{status}">{status}</span>
</div>
</div>"#,
                    file_link = file_link,
                    sz = escape_html(&size_str),
                    status = escape_html(status_note),
                ),
            );
        }
    }

    if show_media
        && !original_pruned
        && matches!(&post.media_type, Some(crate::models::MediaType::Other))
    {
        if let Some(file) = &post.file_path {
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let name_str = post.file_name.as_deref().unwrap_or("download");
            let file_link = render_file_link(file, name_str);
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<div class="file-container file-download">
<div class="file-info">
  File: {file_link} ({sz})
</div>
</div>"#,
                    file_link = file_link,
                    sz = escape_html(&size_str)
                ),
            );
        }
    }

    // Secondary audio fallback for legacy rows that store a separate audio attachment.
    if show_media
        && !original_pruned
        && !matches!(primary_media_type, crate::models::MediaType::Image)
    {
        if let (Some(aud_file), Some(aud_mime)) = (&post.audio_file_path, &post.audio_mime_type) {
            let aud_name = post.audio_file_name.as_deref().unwrap_or("audio");
            let aud_size = post
                .audio_file_size
                .map(format_file_size)
                .unwrap_or_default();
            let audio_link = render_file_link(aud_file, aud_name);
            crate::templates::append_html(
                &mut html,
                format_args!(
                    r#"<div class="file-container audio-container audio-combo">
<div class="file-info">
  File: {audio_link} ({sz})
</div>
<audio controls preload="none" class="audio-player" data-audio-title="{orig}">
  <source src="/boards/{f}" type="{mime}">
  Your browser does not support the audio element.
</audio>
</div>"#,
                    audio_link = audio_link,
                    f = escape_html(aud_file),
                    orig = escape_html(aud_name),
                    sz = escape_html(&aud_size),
                    mime = escape_html(aud_mime)
                ),
            );
        }
    }

    // Post body (pre-rendered, sanitised HTML)
    let body_html = crate::utils::sanitize::safe_stored_post_html(
        &post.body,
        &post.body_html,
        collapse_greentext,
    );
    let body_html = annotate_op_quotelinks(&body_html, thread_op_id);
    let body_html = match link_context {
        PostLinkContext::Inline => body_html,
        // Only the sanitizer generates these fragment links. Search results
        // may omit the target, so resolve it through the board's post route.
        PostLinkContext::Thread => body_html
            .replace(
                "href=\"#p",
                &format!("href=\"/{}/post/", escape_html(board_short)),
            )
            .replace(
                "class=\"quotelink\"",
                &format!(
                    "class=\"quotelink crosslink\" data-crossboard=\"{}\"",
                    escape_html(board_short)
                ),
            ),
    };
    crate::templates::append_html(
        &mut html,
        format_args!(r#"<div class="post-body">{body_html}</div>"#),
    );

    // Edit link + report button (only on thread pages where show_delete=true)
    if show_delete {
        let now = chrono::Utc::now().timestamp();
        let self_action_controls = owned_post_controls
            .as_ref()
            .filter(|controls| controls.expires_at > now)
            .map_or_else(String::new, |controls| {
                let edit_button = if allow_editing {
                    format!(
                        r#"<a class="edit-btn" href="/{board}/post/{pid}/edit" data-action="open-edit-modal" data-edit-post-id="{pid}" data-edit-expiry="{expires_at}" title="Edit post" aria-haspopup="dialog">edit</a>
<textarea id="edit-body-{pid}" data-role="edit-body-source" hidden>{body}</textarea>"#,
                        board = escape_html(board_short),
                        pid = post.id,
                        expires_at = controls.expires_at,
                        body = escape_html(&post.body),
                    )
                } else {
                    String::new()
                };
                let delete_button = if allow_self_delete {
                    format!(
                        r#"<a class="del-btn" href="/{board}/post/{pid}/delete" data-confirm="Delete your post No.{pid}?" data-delete-csrf="{csrf}">delete</a>"#,
                        board = escape_html(board_short),
                        pid = post.id,
                        csrf = escape_html(csrf_token),
                    )
                } else {
                    String::new()
                };
                if edit_button.is_empty() && delete_button.is_empty() {
                    String::new()
                } else {
                    format!(
                        r#" <span class="self-action-controls" data-action-expiry="{expires_at}">{edit_button}{delete_button}{window_hint}</span>"#,
                        expires_at = controls.expires_at,
                        edit_button = edit_button,
                        delete_button = delete_button,
                        window_hint = render_self_action_window_hint(controls.expires_at),
                    )
                }
            });

        let report_btn = format!(
            r#" <button type="button" class="report-btn"
                data-action="open-report" data-pid="{pid}" data-tid="{tid}" data-board="{board}" data-csrf="{csrf}">report</button>"#,
            pid = post.id,
            tid = post.thread_id,
            board = escape_html(board_short),
            csrf = escape_html(csrf_token),
        );
        let report_fallback = super::report_fallback_form(
            board_short,
            post.id,
            post.thread_id,
            csrf_token,
            "submit report",
        );

        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="post-controls">{self_action_controls}{report_btn}{report_fallback}</div>"#
            ),
        );
    }

    // Admin delete button + IP history/report links
    if is_admin {
        let is_op_val = if post.is_op { "1" } else { "0" };
        let return_to = format!("/{}/thread/{}", board_short, post.thread_id);
        let admin_form_csrf = admin_csrf_token.as_deref().unwrap_or(csrf_token);
        crate::templates::append_html(
            &mut html,
            format_args!(
                r#"<div class="post-controls admin-post-controls">
<form method="POST" action="/admin/post/delete">
<input type="hidden" name="_csrf"   value="{csrf}">
<input type="hidden" name="post_id" value="{pid}">
<input type="hidden" name="board"   value="{board}">
<button type="submit" class="admin-del-btn"
        data-confirm="Admin delete post No.{pid}?">&#x2715; del</button>
</form>
<form method="POST" action="/admin/post/ban-delete"
      data-ban-delete-pid="{pid}">
<input type="hidden" name="_csrf"      value="{csrf}">
<input type="hidden" name="post_id"    value="{pid}">
<input type="hidden" name="ip_hash"    value="{ip_hash}">
<input type="hidden" name="board"      value="{board}">
<input type="hidden" name="thread_id"  value="{tid}">
<input type="hidden" name="is_op"      value="{is_op}">
<input type="hidden" name="reason"     id="ban-reason-{pid}" value="">
<input type="hidden" name="duration_hours" id="ban-dur-{pid}" value="0">
<button type="submit" class="admin-del-btn btn-danger">&#x26D4; ban+del</button>
</form>
<a class="admin-ip-link" href="/admin/ip/{ip_hash}?return_to={return_to}" title="View all posts from this hashed IP">&#x1F50D; ip</a>
</div>"#,
                csrf = escape_html(admin_form_csrf),
                pid = post.id,
                board = escape_html(board_short),
                ip_hash = escape_html(post.ip_hash.as_deref().unwrap_or("")),
                tid = post.thread_id,
                return_to = encode_query_component(&return_to),
                is_op = is_op_val
            ),
        );
    }

    html.push_str("</div>\n");
    html
}

/// Renders the shared in-page edit dialog and optional validation state.
fn render_edit_overlay(
    board: &Board,
    thread_id: i64,
    csrf_token: &str,
    edit_overlay_state: Option<&EditOverlayState>,
) -> String {
    let (post_id, current_body, error_html, modal_class) = edit_overlay_state.map_or_else(
        || {
            (
                0,
                "",
                String::from(
                    r#"<div class="post-error-banner edit-modal-error" data-role="edit-modal-error" hidden></div>"#,
                ),
                "edit-modal",
            )
        },
        |state| {
            (
            state.post_id,
            state.body.as_str(),
            state
                .error
                .as_deref()
                .map(|msg| {
                    format!(
                        r#"<div class="post-error-banner edit-modal-error" data-role="edit-modal-error">&#9888; {}</div>"#,
                        escape_html(msg)
                    )
                })
                .unwrap_or_default(),
            "edit-modal is-open",
            )
        },
    );
    let aria_hidden = if edit_overlay_state.is_some() {
        "false"
    } else {
        "true"
    };
    let hidden_attr = if edit_overlay_state.is_some() {
        ""
    } else {
        " hidden inert"
    };

    format!(
        r#"<div id="edit-modal" class="{modal_class}" data-thread-id="{thread_id}" data-board="{board}" aria-hidden="{aria_hidden}"{hidden_attr}>
  <div class="edit-modal-backdrop" data-action="close-edit-modal"></div>
  <div class="edit-modal-box" role="dialog" aria-modal="true" aria-labelledby="edit-modal-title">
    <div class="post-form-title" id="edit-modal-title">[ edit your post <span class="self-delete-countdown" data-role="edit-modal-countdown" aria-live="polite"></span> ]</div>
    {error_html}
    <form id="edit-modal-form" class="post-form" method="POST" action="/{board}/post/{post_id}/edit">
      <input type="hidden" name="_csrf" value="{csrf}">
      <input type="hidden" name="thread_id" value="{thread_id}">
      <table>
        <tr><td>body</td>
            <td><textarea id="edit-modal-body" name="body" aria-label="edit post body" rows="6" maxlength="4096">{current_body}</textarea></td></tr>
        <tr><td></td>
            <td><button type="submit">save edit</button>
                <button type="button" class="edit-btn" data-action="close-edit-modal" style="margin-left:1rem">cancel</button></td></tr>
      </table>
    </form>
  </div>
</div>"#,
        modal_class = modal_class,
        thread_id = thread_id,
        board = escape_html(&board.short_name),
        aria_hidden = aria_hidden,
        hidden_attr = hidden_attr,
        error_html = error_html,
        post_id = post_id,
        csrf = escape_html(csrf_token),
        current_body = escape_html(current_body),
    )
}
