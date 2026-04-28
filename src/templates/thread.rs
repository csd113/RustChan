// templates/thread.rs
//
// Page templates for thread-level views:
//   thread_page  — full thread with all posts, reply form, poll
//   render_post  — single post HTML (also used by board.rs for index previews)
//   render_poll  — poll widget (private, embedded in thread_page)

use crate::models::{Board, Post, Thread};
use crate::utils::{
    files::format_file_size, redirect::encode_query_component, sanitize::escape_html,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;

use super::{
    base_layout, compress_modal_script, fmt_ts, fmt_ts_short, report_modal_script,
    thread_autoupdate_script, TOGGLE_SCRIPT,
};

const SELF_ACTION_WINDOW_SECS: i64 = 60;
const SELF_ACTION_WINDOW_HINT: &str = "available for up to 60 seconds after posting";

#[derive(Debug, Clone)]
pub struct OwnedPostControls {
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct EditOverlayState {
    pub post_id: i64,
    pub body: String,
    pub error: Option<String>,
}

fn render_self_action_window_hint(expires_at: i64) -> String {
    format!(
        r#"<span class="self-action-window-note self-delete-countdown" data-role="self-action-countdown" aria-live="polite" data-action-expiry="{expires_at}">{SELF_ACTION_WINDOW_HINT}</span>"#
    )
}

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
            show_media: true,
            allow_editing: false,
            allow_self_delete: false,
            owned_post_controls: None,
            show_poster_ids: false,
            collapse_greentext: true,
            thread_state: None,
            thread_op_id,
        },
        SELF_ACTION_WINDOW_SECS,
    )
}

#[must_use]
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
        let _ = write!(
            body,
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(msg)
        );
    }

    let _ = write!(
        body,
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
        <td><textarea name="body" rows="8" maxlength="4096" required>{body_text}</textarea></td></tr>
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
        let _ = write!(
            body,
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(msg)
        );
    }

    let _ = write!(
        body,
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

fn render_thread_nav(board: &Board, reply_count: i64, is_bottom: bool) -> String {
    let jump_link = if is_bottom { "#top" } else { "#bottom" };
    let jump_label = if is_bottom { "Top" } else { "Bottom" };
    let nav_class = if is_bottom {
        "board-header thread-nav thread-nav-bottom"
    } else {
        "board-header thread-nav"
    };
    format!(
        r#"<div class="{nav_class}">
  <a href="/{board_short}">[ Return ]</a>
  <a href="/{board_short}/catalog">[ Catalog ]</a>
  <a href="{jump_link}">[ {jump_label} ]</a>
  <button class="thread-nav-btn" type="button" data-action="fetch-updates" data-busy-label="[ Updating… ]">[ Update ]</button>
  <label class="autoupdate-label">
    <input type="checkbox" data-role="autoupdate-toggle" data-action="autoupdate-toggle">
    Auto
  </label>
  <span class="autoupdate-status" data-role="autoupdate-status" role="status" aria-live="polite"></span>
  <span class="thread-reply-stat">R: <span data-role="thread-reply-count">{reply_count}</span></span>
</div>
"#,
        nav_class = nav_class,
        board_short = escape_html(&board.short_name),
        jump_link = jump_link,
        jump_label = jump_label,
        reply_count = reply_count,
    )
}

#[must_use]
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
pub fn render_thread_state_badges(sticky: bool, locked: bool) -> String {
    render_thread_state_badges_full(sticky, locked, false)
}

#[must_use]
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

// ─── Thread page ──────────────────────────────────────────────────────────────

#[must_use]
// This function/module is intentionally long; splitting it further would make the routing or template flow harder to follow.
#[allow(clippy::too_many_lines)]
// The signature mirrors the data passed between layers, so a wrapper would add more noise than clarity.
#[allow(clippy::too_many_arguments)]
pub fn thread_page(
    board: &Board,
    thread: &Thread,
    posts: &[Post],
    owned_post_controls: &BTreeMap<i64, OwnedPostControls>,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    poll: Option<&crate::models::PollData>,
    error: Option<&str>,
    success: Option<&str>,
    reply_prefill: Option<&super::forms::PostFormState>,
    edit_overlay_state: Option<&EditOverlayState>,
    current_theme: Option<&str>,
    collapse_greentext: bool,
    can_post: bool,
) -> String {
    let mut body = String::new();
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
<button type="submit" class="admin-toolbar-btn">{sticky_lbl}</button>
</form>
<form method="POST" action="/admin/thread/action" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="action" value="{lock_act}">
<input type="hidden" name="board" value="{board}">
<button type="submit" class="admin-toolbar-btn">{lock_lbl}</button>
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
            csrf = escape_html(csrf_token),
            tid = thread.id,
            board = escape_html(&board.short_name),
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
                    csrf = escape_html(csrf_token),
                    tid = thread.id,
                    board = escape_html(&board.short_name),
                )
            }
        )
    } else {
        String::new()
    };

    if let Some(msg) = success {
        let _ = write!(
            body,
            r#"<div class="post-success-banner">{}</div>"#,
            escape_html(msg)
        );
    }

    if let Some(msg) = error {
        let _ = write!(
            body,
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(msg)
        );
    }

    let thread_notice = if thread.archived {
        r#"<div class="notice locked-notice">This thread is archived. - You cannot reply anymore.</div>"#
    } else if thread.locked {
        r#"<div class="notice locked-notice">this thread is locked — no new replies allowed</div>"#
    } else {
        ""
    };

    let _ = write!(
        body,
        r#"<div id="top"></div>
<div class="thread-board-banner board-thread-header">/{s}/ — {bn}{access_badge}</div>
{admin_toolbar}
{top_nav}"#,
        s = escape_html(&board.short_name),
        bn = escape_html(&board.name),
        access_badge = super::board::board_access_badge(board),
        admin_toolbar = admin_toolbar,
        top_nav = render_thread_nav(board, thread.reply_count, false)
    );
    body.push_str(thread_notice);

    if let Some(pd) = poll {
        body.push_str(&render_poll(pd, thread.id, &board.short_name, csrf_token));
    }

    let last_post_id = posts.iter().map(|p| p.id).max().unwrap_or(0);
    let _ = write!(
        body,
        r#"<div id="thread-posts" data-thread-id="{tid}" data-board="{board}" data-last-id="{last}">"#,
        tid = thread.id,
        board = escape_html(&board.short_name),
        last = last_post_id
    );
    for post in posts {
        body.push_str(&render_post(
            post,
            &board.short_name,
            csrf_token,
            RenderPostOpts {
                show_delete: true,
                is_admin,
                show_media: true,
                allow_editing: board.allow_editing,
                allow_self_delete: board.allow_self_delete,
                owned_post_controls: owned_post_controls.get(&post.id).cloned(),
                show_poster_ids: board.show_poster_ids,
                collapse_greentext: board.collapse_greentext,
                thread_state: Some((thread.sticky, thread.locked, thread.archived)),
                thread_op_id: thread.op_id,
            },
            SELF_ACTION_WINDOW_SECS,
        ));
    }

    body.push_str("</div><!-- #thread-posts -->\n");
    body.push_str(&render_edit_overlay(
        board,
        thread.id,
        csrf_token,
        posts,
        owned_post_controls,
        edit_overlay_state,
    ));

    if !thread.locked && !thread.archived && can_post {
        let form_html = super::forms::reply_form(
            &board.short_name,
            thread.id,
            csrf_token,
            board,
            reply_prefill,
        );
        let show_post_form = error.is_some() || reply_prefill.is_some();
        let _ = write!(
            body,
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
    body.push_str("<div id=\"bottom\"></div>\n");
    body.push_str(&render_thread_nav(board, thread.reply_count, true));

    body.push_str(TOGGLE_SCRIPT);
    body.push_str(&compress_modal_script(
        crate::config::CONFIG.max_image_size,
        crate::config::CONFIG.max_video_size,
    ));
    body.push_str(report_modal_script());
    body.push_str(thread_autoupdate_script());

    // The previous approach used inline <script> blocks to inject
    // board-specific values (EMBED_ENABLED, DRAFT_KEY) at render time.  Inline
    // scripts are blocked by the CSP `script-src 'self'` directive (which
    // deliberately omits 'unsafe-inline'), so neither buildEmbed nor the draft
    // autosave ever executed in the browser — breaking YouTube thumbnail display
    // and inline playback entirely.
    //
    // The fix: pass the board-specific values as data-* attributes on a small
    // config element and let the static main.js read them.  No inline script
    // execution is required, the CSP remains unchanged, and embeds work again.

    // ── Video embed + draft autosave config (data attributes only) ─────────
    let embed_enabled_attr = if board.allow_video_embeds { "1" } else { "0" };
    let draft_key = format!("rustchan_draft_{}_{}", board.short_name, thread.id);
    let _ = write!(
        body,
        r#"<div id="thread-config"
     data-embed-enabled="{embed_enabled}"
     data-draft-key="{draft_key}"
     style="display:none" aria-hidden="true"></div>"#,
        embed_enabled = embed_enabled_attr,
        draft_key = escape_html(&draft_key)
    );

    base_layout(
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
    )
}

// ─── Poll renderer ────────────────────────────────────────────────────────────

fn render_poll(
    pd: &crate::models::PollData,
    thread_id: i64,
    board_short: &str,
    csrf_token: &str,
) -> String {
    let now = chrono::Utc::now().timestamp();
    let time_left = pd.poll.expires_at.saturating_sub(now);
    let expires_str = if pd.is_expired {
        "closed".to_string()
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
    };

    let show_results = pd.is_expired || pd.user_voted_option.is_some();

    let mut html = format!(
        r#"<div class="poll-container">
<div class="poll-header">
  <span class="poll-icon">📊</span>
  <span class="poll-question">{q}</span>
  <span class="poll-status {status_class}">[{expires}]</span>
</div>"#,
        q = escape_html(&pd.poll.question),
        status_class = if pd.is_expired {
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
            // This cast is a local display or math conversion, and the values are already bounded by surrounding invariants.
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let pct = (opt.vote_count as f64 / total as f64 * 100.0).round() as i64;
            let is_voted = pd.user_voted_option == Some(opt.id);
            let _ = write!(
                html,
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
            );
        }
        let _ = write!(
            html,
            r#"<div class="poll-total">{} total vote{}</div></div>"#,
            pd.total_votes,
            if pd.total_votes == 1 { "" } else { "s" }
        );
    } else {
        let _ = write!(
            html,
            r#"<form class="poll-vote-form" method="POST" action="/vote">
<input type="hidden" name="_csrf"     value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="board"     value="{board}">"#,
            csrf = escape_html(csrf_token),
            tid = thread_id,
            board = escape_html(board_short)
        );
        for opt in &pd.options {
            let _ = write!(
                html,
                r#"<label class="poll-vote-option">
  <input type="radio" name="option_id" value="{id}" required>
  <span class="poll-opt-text">{text}</span>
</label>"#,
                id = opt.id,
                text = escape_html(&opt.text)
            );
        }
        html.push_str(
            r#"<button type="submit" class="poll-vote-btn">[ Cast Vote ]</button></form>"#,
        );
    }

    html.push_str("</div>");
    html
}

// ─── Single post renderer ─────────────────────────────────────────────────────

/// Options that control which controls are rendered for a post.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Default)]
pub struct RenderPostOpts {
    pub show_delete: bool,
    pub is_admin: bool,
    pub show_media: bool,
    pub allow_editing: bool,
    pub allow_self_delete: bool,
    pub owned_post_controls: Option<OwnedPostControls>,
    pub show_poster_ids: bool,
    pub collapse_greentext: bool,
    pub thread_state: Option<(bool, bool, bool)>,
    pub thread_op_id: Option<i64>,
}

const POSTER_ID_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn poster_id_char(index: u8) -> char {
    POSTER_ID_ALPHABET
        .get(usize::from(index))
        .copied()
        .map_or('A', char::from)
}

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

fn poster_id_chip_style(poster_id: &str) -> String {
    const POSTER_CHIP_HUES: [u16; 18] = [
        0, 210, 122, 32, 282, 168, 338, 52, 196, 96, 16, 248, 146, 308, 72, 184, 228, 356,
    ];

    let mut hasher = Sha256::new();
    hasher.update(b":poster-id-chip:");
    hasher.update(poster_id.as_bytes());
    let digest = hasher.finalize();
    let hue_index = digest
        .first()
        .map_or(0, |byte| usize::from(*byte) % POSTER_CHIP_HUES.len());
    let hue = POSTER_CHIP_HUES.get(hue_index).copied().unwrap_or(200);
    let accent_lightness = 60 + digest.get(1).map_or(0, |byte| u16::from(*byte) % 8);
    let background_lightness = 19 + digest.get(2).map_or(0, |byte| u16::from(*byte) % 8);
    let shadow_strength = 34 + digest.get(3).map_or(0, |byte| u16::from(*byte) % 18);
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

fn annotate_op_quotelinks(body_html: &str, thread_op_id: Option<i64>) -> String {
    let Some(op_id) = thread_op_id else {
        return body_html.to_string();
    };
    let target = format!(
        r##"<a href="#p{op_id}" class="quotelink" data-pid="{op_id}">&gt;&gt;{op_id}</a>"##
    );
    let replacement = format!(
        r##"<a href="#p{op_id}" class="quotelink" data-pid="{op_id}">&gt;&gt;{op_id}<span class="quotelink-op-label">(OP)</span></a>"##
    );
    body_html.replace(&target, &replacement)
}

const FILE_NAME_STEM_PREFIX_DISPLAY_CHARS: usize = 20;
const FILE_NAME_TRUNCATION_MARKER: &str = "(...)";

fn render_media_thumb(
    img_class: &str,
    fallback_class: &str,
    src: &str,
    alt: &str,
    loading: &str,
    fallback_text: &str,
) -> String {
    format!(
        r#"<img class="{img_class}" src="/boards/{src}" loading="{loading}" alt="{alt}" data-media-thumb="1">
<div class="{fallback_class} media-thumb-fallback" hidden>{fallback_text}</div>"#,
        img_class = escape_html(img_class),
        fallback_class = escape_html(fallback_class),
        src = escape_html(src),
        loading = escape_html(loading),
        alt = escape_html(alt),
        fallback_text = escape_html(fallback_text),
    )
}

fn truncate_file_name_stem(input: &str) -> String {
    if input.chars().count() <= FILE_NAME_STEM_PREFIX_DISPLAY_CHARS {
        return input.to_string();
    }

    let prefix: String = input
        .chars()
        .take(FILE_NAME_STEM_PREFIX_DISPLAY_CHARS)
        .collect();
    format!("{prefix}{FILE_NAME_TRUNCATION_MARKER}")
}

fn display_file_name(name: &str) -> String {
    match name.rfind('.') {
        Some(dot_idx) if dot_idx > 0 => {
            let (stem, ext) = name.split_at(dot_idx);
            if stem.chars().count() > FILE_NAME_STEM_PREFIX_DISPLAY_CHARS {
                format!("{}{}", truncate_file_name_stem(stem), ext)
            } else {
                name.to_string()
            }
        }
        _ => truncate_file_name_stem(name),
    }
}

fn render_file_link(file_path: &str, file_name: &str) -> String {
    let display_name = display_file_name(file_name);
    format!(
        r#"<a href="/boards/{file_path}" target="_blank" rel="noreferrer" title="{full_name}">{display_name}</a>"#,
        file_path = escape_html(file_path),
        full_name = escape_html(file_name),
        display_name = escape_html(&display_name),
    )
}

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

/// Render a single post as HTML.
/// `pub` because board.rs uses this for thread-summary preview posts and
/// search results; all other call-sites are within this module.
///
/// # Trust boundary
/// `post.body_html` is inserted **raw** (unescaped) because it is pre-rendered,
/// sanitised HTML produced by the markup pipeline before storage. Every other
/// user-supplied string in this function must continue to pass through
/// `escape_html()`. Do not change the `body_html` insertion without ensuring
/// the upstream sanitiser is still in place.
#[allow(clippy::too_many_lines)]
pub fn render_post(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    opts: RenderPostOpts,
    _edit_window_secs: i64,
) -> String {
    let RenderPostOpts {
        show_delete,
        is_admin,
        show_media,
        allow_editing,
        allow_self_delete,
        owned_post_controls,
        show_poster_ids,
        collapse_greentext,
        thread_state,
        thread_op_id,
    } = opts;
    let poster_id = render_poster_id(post, show_poster_ids);
    let poster_id_html = poster_id.as_ref().map_or_else(String::new, |poster_id| {
        let chip_style = poster_id_chip_style(poster_id);
        format!(
            r#" <button type="button" class="poster-id-btn" style="{chip_style}" data-action="toggle-poster-highlight" data-thread-id="{thread_id}" data-poster-id="{poster_id}">ID: {poster_id}</button>"#,
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
            r#" <span class="post-edited" title="Background media processing is still running.">(processing media)</span>"#
                .to_string()
        }
        Some("failed") => {
            let title = post
                .media_processing_error
                .as_deref()
                .map_or_else(
                    || "Background media processing failed.".to_string(),
                    escape_html,
                );
            format!(
                r#" <span class="post-edited" title="{title}">(media processing failed)</span>"#
            )
        }
        _ => String::new(),
    };
    let media_processing_state_attr = post
        .media_processing_state
        .as_deref()
        .map(|state| format!(r#" data-media-processing-state="{}""#, escape_html(state)))
        .unwrap_or_default();

    let mut html = format!(
        r##"<div class="post{op_class}" id="p{id}" data-thread-id="{thread_id}"{poster_attr}{media_processing_state_attr}>
<div class="post-meta">
{subject_html}<strong class="name">{name}</strong>{tripcode}{poster_id_html}
<span class="post-time" data-utc="{ts}">{time}</span>
<a class="post-num" href="#p{id}" data-action="append-reply" data-id="{id}">No.{id}</a>{post_state_badges}{media_processing_badge}
<span class="backrefs" id="backrefs-{id}"></span>
</div>"##,
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

    // Image / Video / Audio
    if show_media {
        if let (Some(file), Some(thumb)) = (&post.file_path, &post.thumb_path) {
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
                let _ = write!(
                    html,
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
                        "eager",
                        "preview unavailable",
                    ),
                    orig = escape_html(name_str),
                    sz = escape_html(&size_str),
                    mime = escape_html(mime)
                );
            } else if is_video {
                let _ = write!(
                    html,
                    r#"<div class="file-container">
<div class="file-info">
  File: {file_link} ({sz})
  <button class="media-close-btn" data-action="collapse-media" style="display:none">&#x2715; close</button>
</div>
<a class="media-preview" data-action="expand-media" href="/boards/{f}" title="click to play">
  {thumb_html}
  <div class="media-expand-overlay">&#9654;</div>
</a>
<video class="media-expanded media-expanded-video" controls preload="none" playsinline webkit-playsinline style="display:none">
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
                        "eager",
                        "preview unavailable",
                    ),
                    sz = escape_html(&size_str),
                    mime = escape_html(mime)
                );
            } else if is_pdf {
                let _ = write!(
                    html,
                    r#"<div class="file-container pdf-container">
<div class="file-info">
  File: {file_link} ({sz}) <span class="post-edited">Open PDF</span>
  <button class="media-close-btn" data-action="collapse-media" style="display:none">&#x2715; close</button>
</div>
<a class="media-preview" data-action="expand-media" href="/boards/{f}" title="open PDF inline">
  {thumb_html}
  <div class="media-expand-overlay">PDF</div>
</a>
<iframe class="media-expanded media-expanded-pdf" src="about:blank" data-src="/boards/{f}" title="{orig}" style="display:none"></iframe>
</div>"#,
                    file_link = file_link,
                    f = escape_html(file),
                    thumb_html = render_media_thumb(
                        "thumb",
                        "thumb",
                        thumb,
                        "PDF thumbnail",
                        "eager",
                        "Open PDF",
                    ),
                    sz = escape_html(&size_str),
                    orig = escape_html(name_str)
                );
            } else {
                // Image
                // Keep the preview as an inline expansion control rather than
                // a new tab so a slow JS load or missed handler does not
                // strand the user in a raw-file window.
                let _ = write!(
                    html,
                    r#"<div class="file-container{combo_class}">
<div class="file-info">
  File: {file_link} ({sz})
  <button class="media-close-btn" data-action="collapse-media" style="display:none">&#x2715; close</button>
</div>
<a class="media-preview" data-action="expand-media" href="/boards/{f}" title="click to expand">
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
                        "eager",
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
                );
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
            let _ = write!(
                html,
                r#"<div class="file-container">
<div class="file-info">
  File: {file_link} ({sz})
  <span class="post-edited" title="{status}">{status}</span>
</div>
</div>"#,
                file_link = file_link,
                sz = escape_html(&size_str),
                status = escape_html(status_note),
            );
        }
    }

    if show_media && matches!(&post.media_type, Some(crate::models::MediaType::Other)) {
        if let Some(file) = &post.file_path {
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let name_str = post.file_name.as_deref().unwrap_or("download");
            let file_link = render_file_link(file, name_str);
            let _ = write!(
                html,
                r#"<div class="file-container file-download">
<div class="file-info">
  File: {file_link} ({sz})
</div>
</div>"#,
                file_link = file_link,
                sz = escape_html(&size_str)
            );
        }
    }

    // Secondary audio fallback for legacy rows that store a separate audio attachment.
    if show_media && !matches!(primary_media_type, crate::models::MediaType::Image) {
        if let (Some(aud_file), Some(aud_mime)) = (&post.audio_file_path, &post.audio_mime_type) {
            let aud_name = post.audio_file_name.as_deref().unwrap_or("audio");
            let aud_size = post
                .audio_file_size
                .map(format_file_size)
                .unwrap_or_default();
            let audio_link = render_file_link(aud_file, aud_name);
            let _ = write!(
                html,
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
            );
        }
    }

    // Post body (pre-rendered, sanitised HTML)
    let body_html =
        crate::utils::sanitize::normalize_greentext_blocks(&post.body_html, collapse_greentext);
    let body_html = annotate_op_quotelinks(&body_html, thread_op_id);
    let _ = write!(html, r#"<div class="post-body">{body_html}</div>"#);

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

        let _ = write!(
            html,
            r#"<div class="post-controls">{self_action_controls}{report_btn}</div>"#
        );
    }

    // Admin delete button + IP history/report links
    if is_admin {
        let is_op_val = if post.is_op { "1" } else { "0" };
        let return_to = format!("/{}/thread/{}", board_short, post.thread_id);
        let _ = write!(
            html,
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
            csrf = escape_html(csrf_token),
            pid = post.id,
            board = escape_html(board_short),
            ip_hash = escape_html(post.ip_hash.as_deref().unwrap_or("")),
            tid = post.thread_id,
            return_to = encode_query_component(&return_to),
            is_op = is_op_val
        );
    }

    html.push_str("</div>\n");
    html
}

fn render_edit_overlay(
    board: &Board,
    thread_id: i64,
    csrf_token: &str,
    posts: &[Post],
    owned_post_controls: &BTreeMap<i64, OwnedPostControls>,
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
    let can_edit_any = posts.iter().any(|post| {
        owned_post_controls.contains_key(&post.id)
            && chrono::Utc::now()
                .timestamp()
                .saturating_sub(post.created_at)
                <= SELF_ACTION_WINDOW_SECS
    });
    let aria_hidden = if edit_overlay_state.is_some() {
        "false"
    } else {
        "true"
    };
    let hidden_attr = if can_edit_any || edit_overlay_state.is_some() {
        ""
    } else {
        " hidden"
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
            <td><textarea id="edit-modal-body" name="body" rows="6" maxlength="4096">{current_body}</textarea></td></tr>
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

#[cfg(test)]
mod tests {
    use super::{
        delete_post_page, display_file_name, edit_post_page, render_post, thread_page,
        EditOverlayState, OwnedPostControls, RenderPostOpts,
    };
    use crate::models::{BoardAccessMode, MediaType, Post, Thread};

    fn sample_post() -> Post {
        Post {
            id: 1,
            thread_id: 1,
            board_id: 1,
            name: "anon".into(),
            tripcode: None,
            subject: None,
            body: "body".into(),
            body_html: "body".into(),
            ip_hash: Some("hash".into()),
            file_path: Some("test/image.webp".into()),
            file_name: Some("image.webp".into()),
            file_size: Some(1024),
            thumb_path: Some("test/thumbs/image.webp".into()),
            mime_type: Some("image/webp".into()),
            media_type: Some(MediaType::Image),
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            created_at: 1_700_000_000,
            deletion_token: "token".into(),
            is_op: false,
            edited_at: None,
            media_processing_state: None,
            media_processing_error: None,
        }
    }

    fn sample_thread() -> Thread {
        Thread {
            id: 87,
            board_id: 1,
            subject: Some("Thread subject".into()),
            created_at: 1_700_000_000,
            bumped_at: 1_700_000_100,
            locked: false,
            sticky: false,
            archived: false,
            reply_count: 12,
            image_count: 3,
            op_body: Some("Thread body preview".into()),
            op_file: Some("test/image.webp".into()),
            op_thumb: Some("test/thumbs/image.webp".into()),
            op_name: Some("anon".into()),
            op_tripcode: None,
            op_id: Some(1),
        }
    }

    #[test]
    fn thread_page_renders_thread_nav_links_and_reply_open_action() {
        let board = crate::test_fixtures::sample_board();
        let thread = sample_thread();
        let posts = vec![Post {
            is_op: true,
            ..sample_post()
        }];

        let html = thread_page(
            &board,
            &thread,
            &posts,
            &std::collections::BTreeMap::new(),
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            true,
        );

        assert!(html.contains(r#"href="/test">[ Return ]</a>"#));
        assert!(html.contains(r#"href="/test/catalog">[ Catalog ]</a>"#));
        assert!(html.contains(r##"href="#bottom">[ Bottom ]</a>"##));
        assert!(html.contains(r##"href="#top">[ Top ]</a>"##));
        assert!(html.contains(r#"data-action="toggle-post-form""#));
    }

    #[test]
    fn thread_page_renders_access_gate_when_posting_is_locked_behind_password() {
        let board = crate::models::Board {
            access_mode: BoardAccessMode::PostPassword,
            access_password_hash: "hash".into(),
            ..crate::test_fixtures::sample_board()
        };
        let thread = sample_thread();
        let posts = vec![Post {
            is_op: true,
            ..sample_post()
        }];

        let html = thread_page(
            &board,
            &thread,
            &posts,
            &std::collections::BTreeMap::new(),
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            false,
        );

        assert!(html.contains(r#"href="/test">[ Return ]</a>"#));
        assert!(html.contains(r#"href="/test/catalog">[ Catalog ]</a>"#));
        assert!(html.contains(r#"id="board-access-gate""#));
        assert!(html.contains(
            r#"name="password" maxlength="256" autocomplete="current-password" required"#
        ));
    }

    #[test]
    fn image_audio_combo_renders_single_media_box_with_inline_audio() {
        let mut post = sample_post();
        post.audio_file_path = Some("test/song.flac".into());
        post.audio_file_name = Some("song.flac".into());
        post.audio_file_size = Some(2048);
        post.audio_mime_type = Some("audio/flac".into());

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html.contains("file-container image-audio-combo"));
        assert!(html.contains(r#"Audio: <a href="/boards/test/song.flac""#));
        assert!(html.contains(r#"data-artwork-src="/boards/test/thumbs/image.webp""#));
        assert!(!html.contains("file-container audio-container audio-combo"));
    }

    #[test]
    fn contradictory_image_mime_prefers_combo_render_over_standalone_audio_boxes() {
        let mut post = sample_post();
        post.file_path = Some("test/confused.png".into());
        post.file_name = Some("confused.png".into());
        post.thumb_path = Some("test/thumbs/confused.png".into());
        post.mime_type = Some("image/png".into());
        post.media_type = Some(MediaType::Audio);
        post.audio_file_path = Some("test/song.mp3".into());
        post.audio_file_name = Some("song.mp3".into());
        post.audio_file_size = Some(2048);
        post.audio_mime_type = Some("audio/mpeg".into());

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html.contains("file-container image-audio-combo"));
        assert!(html.contains(r#"Audio: <a href="/boards/test/song.mp3""#));
        assert!(html.contains(r#"class="audio-player audio-player-combo""#));
        assert!(!html.contains("file-container audio-container audio-combo"));
        assert!(!html.contains(r#"class="audio-player" data-audio-title="song.mp3""#));
    }

    #[test]
    fn display_file_name_truncates_long_stems_and_keeps_extension() {
        assert_eq!(
            display_file_name("A412BB86-098B-48D1-7DG12GNY78KS.jpg"),
            "A412BB86-098B-48D1-7(...).jpg"
        );
        assert_eq!(display_file_name("short.webp"), "short.webp");
        assert_eq!(
            display_file_name("1234567890123456789012345"),
            "12345678901234567890(...)"
        );
    }

    #[test]
    fn render_post_uses_truncated_filename_with_full_title() {
        let mut post = sample_post();
        post.file_name = Some("supercalifragilisticx.webp".into());

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html
            .contains(r#"title="supercalifragilisticx.webp">supercalifragilistic(...).webp</a>"#));
    }

    #[test]
    fn op_post_uses_archive_badge_instead_of_lock_badge_for_archived_threads() {
        let mut post = sample_post();
        post.is_op = true;

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: false,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: Some((true, true, true)),
                thread_op_id: Some(post.id),
            },
            0,
        );

        assert!(html.contains("thread-state-badge-pin"));
        assert!(html.contains("thread-state-badge-archive"));
        assert!(!html.contains("thread-state-badge-lock"));
    }

    #[test]
    fn media_processing_failure_renders_fallback_when_thumb_is_missing() {
        let mut post = sample_post();
        post.thumb_path = None;
        post.media_processing_state = Some("failed".into());
        post.media_processing_error = Some("Queue exhausted retries".into());

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html.contains("media processing failed"));
        assert!(html.contains("Preview generation failed; original file is still available."));
        assert!(html.contains(r#"href="/boards/test/image.webp""#));
    }

    #[test]
    fn media_thumb_markup_includes_hidden_fallback_for_missing_assets() {
        let post = sample_post();

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html.contains(r#"data-media-thumb="1""#));
        assert!(html.contains("media-thumb-fallback"));
    }

    #[test]
    fn pdf_post_renders_thumbnail_direct_link_and_inline_hooks() {
        let mut post = sample_post();
        post.file_path = Some("test/doc.pdf".into());
        post.file_name = Some("doc.pdf".into());
        post.thumb_path = Some("test/thumbs/doc.webp".into());
        post.mime_type = Some("application/pdf".into());
        post.media_type = Some(MediaType::Pdf);

        let html = render_post(
            &post,
            "test",
            "csrf",
            RenderPostOpts {
                show_delete: false,
                is_admin: false,
                show_media: true,
                allow_editing: false,
                allow_self_delete: false,
                owned_post_controls: None,
                show_poster_ids: false,
                collapse_greentext: true,
                thread_state: None,
                thread_op_id: Some(1),
            },
            0,
        );

        assert!(html.contains(r#"href="/boards/test/doc.pdf""#));
        assert!(html.contains(r#"src="/boards/test/thumbs/doc.webp""#));
        assert!(html.contains(r#"data-action="expand-media""#));
        assert!(html.contains(r#"data-action="collapse-media""#));
        assert!(html.contains(r#"<iframe class="media-expanded media-expanded-pdf""#));
        assert!(html.contains(r#"data-src="/boards/test/doc.pdf""#));
        assert!(html.contains("Open PDF"));
    }

    #[test]
    fn thread_page_reopens_reply_form_with_prefill_on_error() {
        let board = crate::test_fixtures::sample_board();
        let thread = sample_thread();
        let posts = vec![Post {
            is_op: true,
            ..sample_post()
        }];
        let reply_prefill = crate::templates::forms::PostFormState {
            body: "retry reply".into(),
            sage: true,
            ..crate::templates::forms::PostFormState::default()
        };

        let html = thread_page(
            &board,
            &thread,
            &posts,
            &std::collections::BTreeMap::new(),
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            Some("Wait before posting"),
            None,
            Some(&reply_prefill),
            None,
            None,
            false,
            true,
        );

        assert!(html.contains(r#"class="post-form-wrap is-open""#));
        assert!(html.contains(">retry reply</textarea>"));
        assert!(html.contains(r#"name="sage" value="1" checked"#));
    }

    #[test]
    fn thread_page_renders_edit_overlay_with_submitted_body_and_error() {
        let board = crate::test_fixtures::sample_board();
        let post = sample_post();
        let mut owned = std::collections::BTreeMap::new();
        owned.insert(
            post.id,
            OwnedPostControls {
                expires_at: i64::MAX,
            },
        );

        let html = thread_page(
            &board,
            &sample_thread(),
            std::slice::from_ref(&post),
            &owned,
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            None,
            None,
            None,
            Some(&EditOverlayState {
                post_id: post.id,
                body: "edited draft".into(),
                error: Some("Edit failed.".into()),
            }),
            None,
            false,
            true,
        );

        assert!(html.contains(r#"id="edit-modal""#));
        assert!(html.contains(r#"class="edit-modal is-open""#));
        assert!(html.contains(">edited draft</textarea>"));
        assert!(html.contains("Edit failed."));
    }

    #[test]
    fn render_post_uses_in_page_edit_action_without_tokenized_redirect() {
        let mut board = crate::test_fixtures::sample_board();
        board.allow_editing = true;
        board.allow_self_delete = true;
        let mut post = sample_post();
        post.created_at = chrono::Utc::now().timestamp();

        let html = thread_page(
            &board,
            &sample_thread(),
            std::slice::from_ref(&post),
            &std::collections::BTreeMap::from([(
                post.id,
                OwnedPostControls {
                    expires_at: i64::MAX,
                },
            )]),
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            true,
        );

        assert!(html.contains(r#"href="/test/post/1/edit""#));
        assert!(html.contains(r#"class="self-action-controls""#));
        assert!(html.contains(r#"class="edit-btn""#));
        assert!(html.contains(r#"class="del-btn""#));
        assert!(html.contains(r#"data-action="open-edit-modal""#));
        assert!(html.contains(r#"data-edit-expiry=""#));
        assert!(html.contains(r#"data-delete-csrf=""#));
        assert!(html.contains(r#"data-role="self-action-countdown""#));
        assert!(html.contains(r#"href="/test/post/1/delete""#));
        assert!(html.contains("available for up to 60 seconds after posting"));
        assert!(!html.contains("?token="));

        let expiry_attr = r#"data-action-expiry=""#;
        let expiry_start = html.find(expiry_attr).expect("expiry attr");
        let expiry_value_start = expiry_start + expiry_attr.len();
        let expiry_value_end = html[expiry_value_start..]
            .find('"')
            .map(|offset| expiry_value_start + offset)
            .expect("expiry attr closing quote");
        let expiry_value = &html[expiry_value_start..expiry_value_end];
        let expiry = expiry_value.parse::<i64>().expect("parseable expiry");
        assert!(expiry > 0);
    }

    #[test]
    fn edit_post_page_renders_normal_form_and_fallback_hint() {
        let board = crate::test_fixtures::sample_board();
        let thread = sample_thread();
        let post = sample_post();

        let html = edit_post_page(
            &board,
            &thread,
            &post,
            "csrf",
            std::slice::from_ref(&board),
            None,
            None,
        );

        assert!(html.contains(r#"method="POST" action="/test/post/1/edit""#));
        assert!(html.contains(r#"name="_csrf" value="csrf""#));
        assert!(html.contains(r#"name="body" rows="8" maxlength="4096" required"#));
        assert!(html.contains("available for up to 60 seconds after posting"));
        assert!(html.contains(r#"href="/test/thread/87#p1""#));
    }

    #[test]
    fn delete_post_page_renders_normal_form_and_fallback_hint() {
        let board = crate::test_fixtures::sample_board();
        let thread = sample_thread();
        let post = sample_post();

        let html = delete_post_page(
            &board,
            &thread,
            &post,
            "csrf",
            std::slice::from_ref(&board),
            None,
            None,
        );

        assert!(html.contains(r#"method="POST" action="/test/post/1/delete""#));
        assert!(html.contains(r#"name="_csrf" value="csrf""#));
        assert!(html.contains(r#"class="del-btn">delete post</button>"#));
        assert!(html.contains("available for up to 60 seconds after posting"));
        assert!(html.contains(r#"href="/test/thread/87#p1""#));
    }

    #[test]
    fn render_post_hides_edited_badge_for_edited_posts() {
        let mut board = crate::test_fixtures::sample_board();
        board.allow_editing = true;
        let mut post = sample_post();
        post.created_at = chrono::Utc::now().timestamp();
        post.edited_at = Some(post.created_at + 5);

        let html = thread_page(
            &board,
            &sample_thread(),
            std::slice::from_ref(&post),
            &std::collections::BTreeMap::from([(
                post.id,
                OwnedPostControls {
                    expires_at: i64::MAX,
                },
            )]),
            "csrf",
            std::slice::from_ref(&board),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            true,
        );

        assert!(!html.contains("(edited"));
    }
}
