// templates/thread.rs
//
// Page templates for thread-level views:
//   thread_page  — full thread with all posts, reply form, poll
//   render_post  — single post HTML (also used by board.rs for index previews)
//   render_poll  — poll widget (private, embedded in thread_page)

use crate::models::*;
use crate::utils::{files::format_file_size, sanitize::escape_html};

use super::{
    base_layout, compress_modal_script, fmt_ts, fmt_ts_short, report_modal_script,
    thread_autoupdate_script, TOGGLE_SCRIPT,
};

// ─── Thread page ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn thread_page(
    board: &Board,
    thread: &Thread,
    posts: &[Post],
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    poll: Option<&crate::models::PollData>,
    error: Option<&str>,
    collapse_greentext: bool,
) -> String {
    let mut body = String::new();

    if let Some(msg) = error {
        body.push_str(&format!(
            r#"<div class="post-error-banner">&#9888; {}</div>"#,
            escape_html(msg)
        ));
    }

    if is_admin {
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
        body.push_str(&format!(
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
            // Show "Archive Thread" only when the thread is not already archived.
            archive_btn = if !thread.archived {
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
            } else {
                String::new()
            },
        ));
    }

    let locked_notice = if thread.locked {
        r#"<div class="notice locked-notice">this thread is locked — no new replies allowed</div>"#
    } else {
        ""
    };

    body.push_str(&format!(
        r##"<div class="thread-board-banner board-thread-header">/{s}/ — {bn}</div>
<div class="board-header thread-nav">
  <a href="/{s}">[ Return ]</a>
  <a href="/{s}/catalog">[ Catalog ]</a>
  <a href="#bottom">[ Bottom ]</a>
  <button class="thread-nav-btn" data-action="fetch-updates">[ Update ]</button>
  <label class="autoupdate-label">
    <input type="checkbox" id="autoupdate-toggle-cb" data-action="autoupdate-toggle">
    Auto
  </label>
  <span class="autoupdate-status" id="autoupdate-status"></span>
  <span class="thread-reply-stat">R: <span id="thread-reply-count">{rc}</span></span>
</div>
"##,
        s = escape_html(&board.short_name),
        bn = escape_html(&board.name),
        rc = thread.reply_count,
    ));
    body.push_str(locked_notice);

    if let Some(pd) = poll {
        body.push_str(&render_poll(pd, thread.id, &board.short_name, csrf_token));
    }

    let last_post_id = posts.iter().map(|p| p.id).max().unwrap_or(0);
    body.push_str(&format!(
        r#"<div id="thread-posts" data-thread-id="{tid}" data-board="{board}" data-last-id="{last}">"#,
        tid   = thread.id,
        board = escape_html(&board.short_name),
        last  = last_post_id,
    ));
    for post in posts {
        body.push_str(&render_post(
            post,
            &board.short_name,
            csrf_token,
            true,
            is_admin,
            true,
            board.edit_window_secs,
        ));
    }

    body.push_str("</div><!-- #thread-posts -->\n");
    body.push_str("<div id=\"bottom\"></div>\n");

    if !thread.locked {
        let form_html = super::forms::reply_form(&board.short_name, thread.id, csrf_token, board);
        body.push_str(&format!(
            r#"<div class="post-toggle-bar reply">
  <button class="post-toggle-btn" data-action="toggle-post-form">[ Reply ]</button>
</div>
<div class="post-form-wrap" id="post-form-wrap" style="display:none">
  {form}
</div>

<!-- Mobile sticky reply drawer — visible only on small screens via CSS -->
<div class="mobile-reply-fab" id="mobile-reply-fab" data-action="toggle-mobile-drawer">
  ✏ Reply
</div>
<div class="mobile-reply-drawer" id="mobile-reply-drawer">
  <div class="mobile-drawer-header">
    <span>reply to thread</span>
    <button class="mobile-drawer-close" data-action="toggle-mobile-drawer">✕</button>
  </div>
  <div class="mobile-drawer-body">
    {form}
  </div>
</div>"#,
            form = form_html,
        ));
    }

    body.push_str(TOGGLE_SCRIPT);
    body.push_str(&compress_modal_script(
        crate::config::CONFIG.max_image_size,
        crate::config::CONFIG.max_video_size,
    ));
    body.push_str(report_modal_script());
    body.push_str(thread_autoupdate_script());

    // FIX[NEW-H1]: Quotelink script moved to /static/main.js

    // ── Inline ban+delete prompt ───────────────────────────────────────────
    if is_admin {
        // FIX[NEW-H1]: adminBanDelete moved to /static/main.js
    }

    // FIX[YT-EMBED]: The previous approach used inline <script> blocks to inject
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
    body.push_str(&format!(
        r#"<div id="thread-config"
     data-embed-enabled="{embed_enabled}"
     data-draft-key="{draft_key}"
     style="display:none" aria-hidden="true"></div>"#,
        embed_enabled = embed_enabled_attr,
        draft_key = escape_html(&draft_key),
    ));

    base_layout(
        &format!(
            "/{}/  {}",
            board.short_name,
            thread.subject.as_deref().unwrap_or("thread")
        ),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        collapse_greentext,
        board.allow_archive,
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
    let time_left = pd.poll.expires_at - now;
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
        expires = expires_str,
    );

    if show_results {
        let total = pd.total_votes.max(1);
        html.push_str(r#"<div class="poll-results">"#);
        for opt in &pd.options {
            let pct = (opt.vote_count as f64 / total as f64 * 100.0).round() as i64;
            let is_voted = pd.user_voted_option == Some(opt.id);
            html.push_str(&format!(
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
                pct = pct,
            ));
        }
        html.push_str(&format!(
            r#"<div class="poll-total">{} total vote{}</div></div>"#,
            pd.total_votes,
            if pd.total_votes == 1 { "" } else { "s" },
        ));
    } else {
        html.push_str(&format!(
            r#"<form class="poll-vote-form" method="POST" action="/vote">
<input type="hidden" name="_csrf"     value="{csrf}">
<input type="hidden" name="thread_id" value="{tid}">
<input type="hidden" name="board"     value="{board}">"#,
            csrf = escape_html(csrf_token),
            tid = thread_id,
            board = escape_html(board_short),
        ));
        for opt in &pd.options {
            html.push_str(&format!(
                r#"<label class="poll-vote-option">
  <input type="radio" name="option_id" value="{id}" required>
  <span class="poll-opt-text">{text}</span>
</label>"#,
                id = opt.id,
                text = escape_html(&opt.text),
            ));
        }
        html.push_str(
            r#"<button type="submit" class="poll-vote-btn">[ Cast Vote ]</button></form>"#,
        );
    }

    html.push_str("</div>");
    html
}

// ─── Single post renderer ─────────────────────────────────────────────────────

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
pub fn render_post(
    post: &Post,
    board_short: &str,
    csrf_token: &str,
    show_delete: bool,
    is_admin: bool,
    show_media: bool,
    edit_window_secs: i64,
) -> String {
    let tripcode_html = post
        .tripcode
        .as_ref()
        .map(|t| format!(r#"<span class="tripcode">!{}</span>"#, escape_html(t)))
        .unwrap_or_default();

    // "edited" badge — shown when the post body was modified after creation.
    let edited_html = post
        .edited_at
        .map(|ts| {
            format!(
                r#" <span class="post-edited" title="last edited {full}">(edited {short})</span>"#,
                full = fmt_ts(ts),
                short = fmt_ts_short(ts),
            )
        })
        .unwrap_or_default();

    let op_class = if post.is_op { " op" } else { " reply" };

    let mut html = format!(
        r##"<div class="post{op_class}" id="p{id}">
<div class="post-meta">
<strong class="name">{name}</strong>{tripcode}
<span class="post-time">{time}</span>{edited}
<a class="post-num" href="#p{id}" data-action="append-reply" data-id="{id}">No.{id}</a>
<span class="backrefs" id="backrefs-{id}"></span>
</div>"##,
        op_class = op_class,
        id = post.id,
        name = escape_html(&post.name),
        tripcode = tripcode_html,
        time = fmt_ts_short(post.created_at),
        edited = edited_html,
    );

    if let Some(subject) = &post.subject {
        html.push_str(&format!(
            r#"<div class="subject"><strong>{}</strong></div>"#,
            escape_html(subject)
        ));
    }

    // Image / Video / Audio
    if show_media {
        if let (Some(file), Some(thumb)) = (&post.file_path, &post.thumb_path) {
            let size_str = post.file_size.map(format_file_size).unwrap_or_default();
            let name_str = post.file_name.as_deref().unwrap_or("file");
            let mime = post
                .mime_type
                .as_deref()
                .unwrap_or("application/octet-stream");
            let is_audio = matches!(&post.media_type, Some(crate::models::MediaType::Audio))
                || post
                    .mime_type
                    .as_deref()
                    .map(|m| m.starts_with("audio/"))
                    .unwrap_or(false);
            let is_video = !is_audio
                && (matches!(&post.media_type, Some(crate::models::MediaType::Video))
                    || post
                        .mime_type
                        .as_deref()
                        .map(|m| m.starts_with("video/"))
                        .unwrap_or(false));

            if is_audio {
                html.push_str(&format!(
                    r#"<div class="file-container audio-container">
<div class="file-info">
  <a href="/boards/{f}">{orig}</a> ({sz})
</div>
<div class="audio-thumb">
  <img class="thumb" src="/boards/{th}" loading="lazy" alt="audio">
</div>
<audio controls preload="none" class="audio-player">
  <source src="/boards/{f}" type="{mime}">
  Your browser does not support the audio element.
</audio>
</div>"#,
                    f = escape_html(file),
                    th = escape_html(thumb),
                    orig = escape_html(name_str),
                    sz = escape_html(&size_str),
                    mime = escape_html(mime),
                ));
            } else if is_video {
                html.push_str(&format!(
                    r#"<div class="file-container">
<div class="file-info">
  <a href="/boards/{f}">{orig}</a> ({sz})
  <button class="media-close-btn" data-action="collapse-media" style="display:none">&#x2715; close</button>
</div>
<div class="media-preview" data-action="expand-media" title="click to play">
  <img class="thumb" src="/boards/{th}" loading="lazy" alt="video thumbnail">
  <div class="media-expand-overlay">&#9654;</div>
</div>
<video class="media-expanded" controls preload="none" style="display:none">
  <source src="/boards/{f}" type="{mime}">
</video>
</div>"#,
                    f = escape_html(file),
                    th = escape_html(thumb),
                    orig = escape_html(name_str),
                    sz = escape_html(&size_str),
                    mime = escape_html(mime),
                ));
            } else {
                // Image
                html.push_str(&format!(
                    r#"<div class="file-container">
<div class="file-info">
  <a href="/boards/{f}">{orig}</a> ({sz})
  <button class="media-close-btn" data-action="collapse-media" style="display:none">&#x2715; close</button>
</div>
<div class="media-preview" data-action="expand-media" title="click to expand">
  <img class="thumb" src="/boards/{th}" loading="lazy" alt="image">
  <div class="media-expand-overlay">&#x2922;</div>
</div>
<img class="media-expanded" src="" data-src="/boards/{f}" style="display:none"
     alt="image" draggable="false">
</div>"#,
                    f = escape_html(file),
                    th = escape_html(thumb),
                    orig = escape_html(name_str),
                    sz = escape_html(&size_str),
                ));
            }
        }
    }

    // Secondary audio for image+audio combo posts
    if show_media {
        if let (Some(aud_file), Some(aud_mime)) = (&post.audio_file_path, &post.audio_mime_type) {
            let aud_name = post.audio_file_name.as_deref().unwrap_or("audio");
            let aud_size = post
                .audio_file_size
                .map(format_file_size)
                .unwrap_or_default();
            html.push_str(&format!(
                r#"<div class="file-container audio-container audio-combo">
<div class="file-info">
  <a href="/boards/{f}">{orig}</a> ({sz})
</div>
<audio controls preload="none" class="audio-player">
  <source src="/boards/{f}" type="{mime}">
  Your browser does not support the audio element.
</audio>
</div>"#,
                f = escape_html(aud_file),
                orig = escape_html(aud_name),
                sz = escape_html(&aud_size),
                mime = escape_html(aud_mime),
            ));
        }
    }

    // Post body (pre-rendered, sanitised HTML)
    html.push_str(&format!(
        r#"<div class="post-body">{}</div>"#,
        post.body_html
    ));

    // Edit link + report button (only on thread pages where show_delete=true)
    if show_delete {
        let now = chrono::Utc::now().timestamp();
        let within_edit_window = edit_window_secs > 0 && now - post.created_at <= edit_window_secs;
        let edit_link = if within_edit_window {
            format!(
                r#" <a class="edit-btn" href="/{board}/post/{pid}/edit" title="Edit post">edit</a>"#,
                board = escape_html(board_short),
                pid = post.id,
            )
        } else {
            String::new()
        };

        let report_btn = format!(
            r#" <button type="button" class="report-btn"
                data-action="open-report" data-pid="{pid}" data-tid="{tid}" data-board="{board}" data-csrf="{csrf}">report</button>"#,
            pid = post.id,
            tid = post.thread_id,
            board = escape_html(board_short),
            csrf = escape_html(csrf_token),
        );

        html.push_str(&format!(
            r#"<div class="post-controls">{edit_link}{report_btn}</div>"#,
            edit_link = edit_link,
            report_btn = report_btn,
        ));
    }

    // Admin delete button + IP history link
    if is_admin {
        let is_op_val = if post.is_op { "1" } else { "0" };
        html.push_str(&format!(
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
<a class="admin-ip-link" href="/admin/ip/{ip_hash}" title="View all posts from this IP hash">&#x1F50D; ip</a>
</div>"#,
            csrf    = escape_html(csrf_token),
            pid     = post.id,
            board   = escape_html(board_short),
            ip_hash = escape_html(&post.ip_hash),
            tid     = post.thread_id,
            is_op   = is_op_val,
        ));
    }

    html.push_str("</div>\n");
    html
}

// ─── Edit post page ───────────────────────────────────────────────────────────

pub fn edit_post_page(
    board: &Board,
    post: &Post,
    csrf_token: &str,
    boards: &[Board],
    prefill_token: &str,
    error: Option<&str>,
    collapse_greentext: bool,
) -> String {
    let error_html = error
        .map(|msg| {
            format!(
                r#"<div class="post-error-banner">&#9888; {}</div>"#,
                escape_html(msg)
            )
        })
        .unwrap_or_default();

    let body = format!(
        r#"{error_html}
<div class="board-header">
  <a href="/{board}/thread/{tid}#p{pid}">[ return to thread ]</a>
</div>
<div class="page-box">
<div class="post-form-container">
<div class="post-form-title">[ edit post No.{pid} ]</div>
<p style="font-size:0.8rem;color:var(--text-dim)">
  You can edit this post within the board's edit window.<br>
  Your edit token is required to confirm the edit.
</p>
<form class="post-form" method="POST" action="/{board}/post/{pid}/edit">
  <input type="hidden" name="_csrf" value="{csrf}">
  <table>
    <tr><td>body</td>
        <td><textarea name="body" rows="6" maxlength="4096">{current_body}</textarea></td></tr>
    <tr><td>edit token</td>
        <td><input type="text" name="deletion_token" value="{token}" placeholder="your edit token" maxlength="64"></td></tr>
    <tr><td></td>
        <td><button type="submit">save edit</button>
            <a href="/{board}/thread/{tid}#p{pid}" style="margin-left:1rem">cancel</a></td></tr>
  </table>
</form>
</div>
</div>"#,
        error_html = error_html,
        board = escape_html(&board.short_name),
        tid = post.thread_id,
        pid = post.id,
        csrf = escape_html(csrf_token),
        current_body = escape_html(&post.body),
        token = escape_html(prefill_token),
    );

    base_layout(
        &format!("edit post No.{} — /{}/", post.id, board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        collapse_greentext,
        board.allow_archive,
    )
}
