// templates/board.rs
//
// Page templates for board-level views:
//   index_page       — site home (list of all boards)
//   board_page       — board thread index with pagination
//   catalog_page     — grid catalog view
//   archive_page     — archived threads list
//   search_page      — search results

use crate::models::*;
use crate::utils::sanitize::escape_html;

use super::{
    base_layout, compress_modal_script, embed_thumb_from_body, fmt_ts, fmt_ts_short,
    live_site_name, live_site_subtitle, render_pagination, urlencoding_simple, TOGGLE_SCRIPT,
};

// ─── Site index (board list) ──────────────────────────────────────────────────

pub fn index_page(
    board_stats: &[crate::models::BoardStats],
    site_stats: &crate::models::SiteStats,
    csrf_token: &str,
    onion_address: Option<&str>,
) -> String {
    let all_boards: Vec<Board> = board_stats.iter().map(|s| s.board.clone()).collect();

    let sfw: Vec<&crate::models::BoardStats> =
        board_stats.iter().filter(|s| !s.board.nsfw).collect();
    let nsfw: Vec<&crate::models::BoardStats> =
        board_stats.iter().filter(|s| s.board.nsfw).collect();

    fn board_cards(list: &[&crate::models::BoardStats]) -> String {
        list.iter()
            .map(|s| {
                let b = &s.board;
                let nsfw_badge = if b.nsfw {
                    r#"<span class="nsfw-badge">NSFW</span>"#
                } else {
                    ""
                };
                let thread_word = if s.thread_count == 1 {
                    "thread"
                } else {
                    "threads"
                };
                format!(
                    r#"<a class="board-card" href="/{sh}/catalog">
  <div class="board-card-short">/{sh}/</div>
  <div class="board-card-name">{n}{nsfw}</div>
  <div class="board-card-desc">{d}</div>
  <div class="board-card-stats">{tc} {tw}</div>
</a>"#,
                    sh = escape_html(&b.short_name),
                    n = escape_html(&b.name),
                    nsfw = nsfw_badge,
                    d = escape_html(&b.description),
                    tc = s.thread_count,
                    tw = thread_word,
                )
            })
            .collect()
    }

    let sfw_sec = if !sfw.is_empty() {
        format!(
            "<div class=\"index-section\"><h2 class=\"index-section-title\">// Boards</h2><div class=\"board-cards\">{}</div></div>",
            board_cards(&sfw)
        )
    } else {
        String::new()
    };

    let nsfw_sec = if !nsfw.is_empty() {
        format!(
            "<div class=\"index-section\"><h2 class=\"index-section-title\">// Adult Boards <span class=\"nsfw-badge\">NSFW</span></h2><div class=\"board-cards\">{}</div></div>",
            board_cards(&nsfw)
        )
    } else {
        String::new()
    };

    let empty = if board_stats.is_empty() {
        "<p class=\"index-empty\">no boards yet — admin must create boards first.</p>"
    } else {
        ""
    };

    let active_gb = site_stats.active_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let stats_sec = format!(
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
    );

    let onion_html = if let Some(addr) = onion_address {
        format!(
            r#"<div class="index-section index-onion-section">
<p class="index-onion"><code class="onion-addr">{}</code></p>
</div>"#,
            escape_html(addr)
        )
    } else {
        String::new()
    };

    let body = format!(
        r#"<div class="index-hero">
<h1 class="index-title">[ {name} ]</h1>
<p class="index-subtitle">{subtitle}</p>
</div>
{sfw}{nsfw}{empty}{stats}{onion}"#,
        name = escape_html(&live_site_name()),
        subtitle = escape_html(&live_site_subtitle()),
        sfw = sfw_sec,
        nsfw = nsfw_sec,
        empty = empty,
        stats = stats_sec,
        onion = onion_html,
    );

    base_layout(
        &live_site_name(),
        None,
        &body,
        csrf_token,
        &all_boards,
        false,
    )
}

// ─── Board index ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn board_page(
    board: &Board,
    summaries: &[ThreadSummary],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
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
        body.push_str(&format!(
            r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
            csrf = escape_html(csrf_token),
            board = escape_html(&board.short_name),
        ));
    }

    {
        let short = escape_html(&board.short_name);
        let name = escape_html(&board.name);
        let desc = escape_html(&board.description);
        let nav_archive = if board.allow_archive {
            format!(r#"<a class="board-nav-link" href="/{short}/archive">[Archive]</a>"#)
        } else {
            String::new()
        };
        body.push_str(&format!(
            r#"<div class="board-header board-index-header"><h1>/{short}/  — {name}</h1><p class="board-desc">{desc}</p></div>
<div class="board-nav"><a class="board-nav-link active" href="/{short}">[Index]</a><a class="board-nav-link" href="/{short}/catalog">[Catalog]</a>{nav_archive}</div>"#,
        ));
    }

    body.push_str(&format!(
        r#"<div class="post-toggle-bar centered catalog-toggle-bar">
  <button class="post-toggle-btn" data-action="toggle-post-form">[ Post a New Thread ]</button>
</div>
<div class="post-form-wrap" id="post-form-wrap" style="display:none">
  {}
</div>"#,
        super::forms::new_thread_form(&board.short_name, csrf_token, board),
    ));

    for summary in summaries {
        body.push_str(&render_thread_summary(
            summary,
            &board.short_name,
            csrf_token,
            is_admin,
        ));
    }

    // FIX[B-T2]: escape_html on board.short_name before embedding in the URL.
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
        &format!("/{}", board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        collapse_greentext,
    )
}

// ─── Thread summary (used by board_page) ─────────────────────────────────────

fn render_thread_summary(
    summary: &ThreadSummary,
    board_short: &str,
    csrf_token: &str,
    is_admin: bool,
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

    html.push_str(&format!(
        r#"<div class="thread" id="t{tid}">
<div class="op post" id="p{op_id}">"#,
        tid = t.id,
        op_id = t.op_id.unwrap_or(0),
    ));

    if let (Some(_file), Some(thumb)) = (&t.op_file, &t.op_thumb) {
        html.push_str(&format!(
            r#"<div class="file-container"><a href="/{board}/thread/{tid}"><img class="thumb" src="/boards/{th}" loading="lazy" alt="image"></a></div>"#,
            board = escape_html(board_short),
            tid = t.id,
            th = escape_html(thumb),
        ));
    } else if let Some(embed_thumb) = t.op_body.as_deref().and_then(embed_thumb_from_body) {
        html.push_str(&format!(
            r#"<div class="file-container"><a href="/{board}/thread/{tid}"><img class="thumb embed-index-thumb" src="{src}" loading="lazy" alt="video thumbnail"></a></div>"#,
            board = escape_html(board_short),
            tid = t.id,
            src = escape_html(&embed_thumb),
        ));
    }

    html.push_str(&format!(
        r#"<div class="post-meta">
{sticky}{locked}
<strong class="name">{name}</strong>
<span class="post-time">{time}</span>
<a class="post-num" href="/{board}/thread/{tid}">No.{op_id}</a>
<a class="thread-id-link" href="/{board}/thread/{tid}" title="Thread #{tid}">[ #{tid} ]</a>
</div>"#,
        sticky = sticky_label,
        locked = locked_label,
        name = escape_html(t.op_name.as_deref().unwrap_or("Anonymous")),
        time = fmt_ts_short(t.created_at),
        board = escape_html(board_short),
        tid = t.id,
        op_id = t.op_id.unwrap_or(0),
    ));

    if let Some(subject) = &t.subject {
        html.push_str(&format!(
            r#"<div class="subject"><a href="/{b}/thread/{tid}"><strong>{s}</strong></a></div>"#,
            b = escape_html(board_short),
            tid = t.id,
            s = escape_html(subject),
        ));
    }

    if let Some(body) = &t.op_body {
        // FIX[B-T1]: Count and slice by character, not by byte.
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
        html.push_str(&format!(r#"<div class="post-body">{}</div>"#, truncated));
    }

    html.push_str(&format!(
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
    ));

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
        html.push_str(&format!(
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
            lock_lbl = lock_lbl,
        ));
    }

    html.push_str("</div>\n</div>");

    if summary.omitted > 0 {
        html.push_str(&format!(
            r#"<div class="omitted">{} posts omitted. <a href="/{b}/thread/{tid}">view thread</a></div>"#,
            summary.omitted,
            b = escape_html(board_short),
            tid = t.id,
        ));
    }

    for post in &summary.preview_posts {
        html.push_str(&super::thread::render_post(
            post,
            board_short,
            csrf_token,
            false,
            is_admin,
            true,
            0, // no edit link on board index previews
        ));
    }

    html.push_str("<hr class=\"thread-sep\">");
    html
}

// ─── Catalog page ─────────────────────────────────────────────────────────────

pub fn catalog_page(
    board: &Board,
    threads: &[Thread],
    csrf_token: &str,
    boards: &[Board],
    is_admin: bool,
    collapse_greentext: bool,
) -> String {
    let bs = escape_html(&board.short_name);
    let bn = escape_html(&board.name);

    let mut body = String::new();

    if is_admin {
        body.push_str(&format!(
            r#"<div class="admin-toolbar">
<span class="admin-toolbar-label">&#9632; ADMIN</span>
<form method="POST" action="/admin/logout" style="display:inline">
<input type="hidden" name="_csrf" value="{csrf}">
<input type="hidden" name="return_to" value="/{board}/catalog">
<button type="submit" class="admin-toolbar-btn">logout</button>
</form>
</div>"#,
            csrf = escape_html(csrf_token),
            board = escape_html(&board.short_name),
        ));
    }

    let nav_archive = if board.allow_archive {
        format!(r#"<a class="board-nav-link" href="/{bs}/archive">[Archive]</a>"#)
    } else {
        String::new()
    };

    body.push_str(&format!(
        r#"<div class="board-header catalog-header-row">
  <div class="catalog-header-left board-catalog-header">
    <h1>/{bs}/  — {bn}</h1>
    <p class="board-desc">{desc}</p>
  </div>
  <div class="catalog-sort-wrap">
    <label class="catalog-sort-label" for="catalog-sort">sort:</label>
    <select id="catalog-sort" class="catalog-sort-select" data-action="sort-catalog">
      <option value="bump" selected>bump order</option>
      <option value="replies">reply count</option>
      <option value="created">creation date</option>
      <option value="last_reply">last reply</option>
    </select>
  </div>
</div>
<div class="board-nav"><a class="board-nav-link" href="/{bs}">[Index]</a><a class="board-nav-link active" href="/{bs}/catalog">[Catalog]</a>{nav_archive}</div>
<div class="post-toggle-bar centered catalog-toggle-bar">
  <button class="post-toggle-btn" data-action="toggle-post-form">[ Start a New Thread ]</button>
</div>
<div class="post-form-wrap" id="post-form-wrap" style="display:none">
  {form}
</div>
<div class="catalog-grid" id="catalog-grid">"#,
        bs = bs,
        bn = bn,
        desc = escape_html(&board.description),
        nav_archive = nav_archive,
        form = super::forms::new_thread_form(&board.short_name, csrf_token, board),
    ));

    for t in threads {
        let thumb_html = if let Some(th) = &t.op_thumb {
            format!(
                r#"<img class="catalog-thumb" src="/boards/{}" loading="lazy" alt="">"#,
                escape_html(th)
            )
        } else if let Some(embed_thumb) = t.op_body.as_deref().and_then(embed_thumb_from_body) {
            format!(
                r#"<img class="catalog-thumb embed-catalog-thumb" src="{}" loading="lazy" alt="video thumbnail">"#,
                escape_html(&embed_thumb)
            )
        } else {
            r#"<div class="catalog-no-image">no img</div>"#.to_string()
        };

        let subject = t
            .subject
            .as_deref()
            .unwrap_or_else(|| t.op_body.as_deref().unwrap_or(""));
        let preview: String = subject.chars().take(80).collect();

        body.push_str(&format!(
            r#"<div class="catalog-item{sticky}" data-replies="{replies}" data-created="{created}" data-bumped="{bumped}" data-sticky="{is_sticky}">
<a href="/{board}/thread/{tid}">
{thumb}
<div class="catalog-info">
<span class="catalog-replies">R: {replies} / F: {images}</span>
<p class="catalog-subject">{subj}</p>
</div>
</a>
</div>"#,
            sticky = if t.sticky { " sticky" } else { "" },
            is_sticky = if t.sticky { "1" } else { "0" },
            board = escape_html(&board.short_name),
            tid = t.id,
            thumb = thumb_html,
            replies = t.reply_count,
            images = t.image_count,
            subj = escape_html(&preview),
            created = t.created_at,
            bumped = t.bumped_at,
        ));
    }

    body.push_str("</div>");
    // FIX[NEW-H1]: sortCatalog moved to /static/main.js
    body.push_str(TOGGLE_SCRIPT);
    body.push_str(&compress_modal_script(
        crate::config::CONFIG.max_image_size,
        crate::config::CONFIG.max_video_size,
    ));
    base_layout(
        &format!("/{}/  catalog", board.short_name),
        Some(&board.short_name),
        &body,
        csrf_token,
        boards,
        collapse_greentext,
    )
}

// ─── Search results ───────────────────────────────────────────────────────────

pub fn search_page(
    board: &Board,
    query: &str,
    posts: &[Post],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
    collapse_greentext: bool,
) -> String {
    let mut body = format!(
        r#"<div class="page-box">
<h2 style="color:var(--green-pale);font-size:0.9rem;margin-bottom:8px">search: "{}" in /{}/</h2>
<form method="GET" action="/{}/search">
  <input type="text" name="q" value="{}" maxlength="64" style="background:var(--bg-input);border:1px solid var(--border);color:var(--green-pale);padding:4px 8px;font-family:var(--font)">
  <button type="submit">search</button>
</form>"#,
        escape_html(query),
        escape_html(&board.short_name),
        escape_html(&board.short_name),
        escape_html(query),
    );

    if posts.is_empty() {
        body.push_str(r#"<p style="color:var(--text-dim);margin-top:8px">no results found.</p>"#);
    } else {
        body.push_str(&format!(
            r#"<p style="color:var(--text-dim);font-size:0.8rem;margin-top:6px">{} result(s)</p>"#,
            pagination.total
        ));
        for post in posts {
            body.push_str(&super::thread::render_post(
                post,
                &board.short_name,
                csrf_token,
                false,
                false,
                true,
                0, // no edit link on search results
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
        collapse_greentext,
    )
}

// ─── Archive page ─────────────────────────────────────────────────────────────

pub fn archive_page(
    board: &Board,
    threads: &[Thread],
    pagination: &Pagination,
    csrf_token: &str,
    boards: &[Board],
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
<p class="archive-subtext">Threads cycled off the board index — read-only, preserved permanently.</p>
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
            let preview: String = t
                .op_body
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect();
            let subj = if let Some(s) = &t.subject {
                format!(
                    r#"<span class="archive-thread-subj">{}</span> — "#,
                    escape_html(s)
                )
            } else {
                String::new()
            };
            let thumb_html = if let Some(thumb) = &t.op_thumb {
                format!(
                    r#"<img src="/boards/{}" class="archive-thumb" alt="thumb" loading="lazy">"#,
                    escape_html(thumb)
                )
            } else {
                String::new()
            };
            let ts = fmt_ts(t.created_at);
            body.push_str(&format!(
                r#"<div class="archive-row">
  {thumb}
  <div class="archive-row-info">
    <a href="/{board}/thread/{tid}" class="archive-thread-link">
      {subj}<span class="archive-preview">{preview}</span>
    </a>
    <span class="archive-meta">No.{tid} &mdash; {replies} replies &mdash; {ts} &#128190;</span>
  </div>
</div>"#,
                thumb = thumb_html,
                board = bs,
                tid = t.id,
                subj = subj,
                preview = escape_html(&preview),
                replies = t.reply_count,
                ts = ts,
            ));
        }
        body.push_str("</div>");
        // FIX[B-T2]: escape before embedding in pagination URL.
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
        collapse_greentext,
    )
}
