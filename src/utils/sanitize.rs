// utils/sanitize.rs
//
// XSS Prevention: User input NEVER goes to templates unescaped.
// Every piece of user text passes through `escape_html` before insertion.
//
// Post markup pipeline (after HTML-escaping):
//   • Lines starting with ">" → greentext (3+ consecutive → collapsible block)
//   • >>12345 → clickable reply link
//   • >>>/board/123 → cross-board thread link
//   • >>>/board/ → cross-board link
//   • URLs → hyperlinks (http/https only)
//   • **bold** → <strong>
//   • __italic__ → <em>
//   • [spoiler]text[/spoiler] → hidden spoiler span
//   • :emoji: shortcodes → Unicode emoji
//
// Word filters: applied on raw text BEFORE HTML escaping.

use rand_core::{OsRng, RngCore};
use regex::Regex;
use std::sync::LazyLock;

#[allow(clippy::expect_used)]
static RE_REPLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&gt;&gt;(\d+)").expect("RE_REPLY is valid"));
#[allow(clippy::expect_used)]
static RE_CROSSLINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&gt;&gt;&gt;/([a-z0-9]+)/(\d+)?").expect("RE_CROSSLINK is valid")
});
#[allow(clippy::expect_used)]
static RE_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(https?://[^\s&<>]{3,300})").expect("RE_URL is valid"));
#[allow(clippy::expect_used)]
static RE_BOLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").expect("RE_BOLD is valid"));
#[allow(clippy::expect_used)]
static RE_ITALIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"__([^_]+)__").expect("RE_ITALIC is valid"));
#[allow(clippy::expect_used)]
static RE_SPOILER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[spoiler\]([\s\S]*?)\[/spoiler\]").expect("RE_SPOILER is valid")
});
#[allow(clippy::expect_used)]
static RE_DICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[dice (\d{1,2})d(\d{1,3})\]").expect("RE_DICE is valid"));

// ─── Video embed URL detection ────────────────────────────────────────────────
// These extract a canonical video ID from supported platforms so the client-side
// embed script can build the appropriate iframe/thumbnail without a server round-trip.

/// Try to extract a (`embed_type`, `video_id`) pair from a URL.
///
/// Supports `YouTube` (youtube.com and youtu.be), any Invidious instance
/// (detected by the `/watch?v=` path), and Streamable.
/// Returns None for all other URLs.
#[must_use]
pub fn extract_video_embed(url: &str) -> Option<(&'static str, String)> {
    // YouTube — youtube.com/watch?v=ID or youtu.be/ID or youtube.com/shorts/ID
    if url.contains("youtube.com") || url.contains("youtu.be") {
        if let Some(id) = extract_yt_id(url) {
            return Some(("youtube", id));
        }
    }
    // Streamable — streamable.com/CODE
    if url.contains("streamable.com/") {
        if let Some(code) = extract_streamable_id(url) {
            return Some(("streamable", code));
        }
    }
    // Invidious — any domain serving /watch?v=ID (11-char YouTube-style ID).
    // FIX[INVIDIOUS]: The previous code matched ANY URL containing ?v= or &v=,
    // meaning a completely ordinary link like https://example.com/article?v=dQw4w9WgXcQ
    // would be silently replaced with a YouTube embed widget.  We now require
    // the URL path to contain "/watch" (case-sensitive, matching real Invidious
    // instances) before treating the ?v= parameter as a video ID.
    if !url.contains("youtube.com") && !url.contains("youtu.be") && url.contains("/watch") {
        if let Some(id) = extract_yt_id_from_watch_param(url) {
            return Some(("youtube", id));
        }
    }
    None
}

#[allow(clippy::arithmetic_side_effects)]
fn extract_yt_id(url: &str) -> Option<String> {
    // youtu.be/ID
    if let Some(pos) = url.find("youtu.be/") {
        let rest = &url[pos + 9..];
        let id: String = rest.chars().take(11).collect();
        if id.len() == 11
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Some(id);
        }
    }
    // youtube.com/shorts/ID
    if let Some(pos) = url.find("/shorts/") {
        let rest = &url[pos + 8..];
        let id: String = rest.chars().take(11).collect();
        if id.len() == 11
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Some(id);
        }
    }
    // ?v=ID or &v=ID
    extract_yt_id_from_watch_param(url)
}

#[allow(clippy::arithmetic_side_effects)]
fn extract_yt_id_from_watch_param(url: &str) -> Option<String> {
    for prefix in &["?v=", "&v="] {
        if let Some(pos) = url.find(prefix) {
            let rest = &url[pos + prefix.len()..];
            let id: String = rest.chars().take(11).collect();
            if id.len() == 11
                && id
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                return Some(id);
            }
        }
    }
    None
}

#[allow(clippy::arithmetic_side_effects)]
fn extract_streamable_id(url: &str) -> Option<String> {
    // streamable.com/CODE — code is alphanumeric, typically 6 chars
    if let Some(pos) = url.find("streamable.com/") {
        let rest = &url[pos + 15..];
        // Strip any query/fragment
        let code: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !code.is_empty() && code.len() <= 16 {
            return Some(code);
        }
    }
    None
}

/// Unicode die-face characters for d6 results (⚀–⚅, value 1–6).
const fn d6_face(n: u32) -> char {
    match n {
        1 => '⚀',
        2 => '⚁',
        3 => '⚂',
        4 => '⚃',
        5 => '⚄',
        6 => '⚅',
        _ => '🎲',
    }
}

/// Roll `count` dice each with `sides` faces, return (rolls, sum).
#[allow(clippy::arithmetic_side_effects)]
fn roll_dice(count: u32, sides: u32) -> (Vec<u32>, u32) {
    let mut rolls = Vec::with_capacity(count as usize);
    let mut sum = 0u32;
    for _ in 0..count {
        // next_u32 % sides gives a value 0..sides-1; add 1 for 1..=sides.
        // Modulo bias is negligible for dice-sized ranges.
        let roll = (OsRng.next_u32() % sides) + 1;
        rolls.push(roll);
        sum = sum.saturating_add(roll);
    }
    (rolls, sum)
}

/// Replace [dice `NdM`] tags in HTML-escaped post text with their rolled results.
/// Called once per post at creation time — the result is stored in `body_html` so
/// the same rolls are shown to every reader forever.
fn apply_dice(text: &str) -> String {
    RE_DICE
        .replace_all(text, |caps: &regex::Captures| {
            let count: u32 = caps[1].parse().unwrap_or(1).clamp(1, 20);
            let sides: u32 = caps[2].parse().unwrap_or(6).clamp(2, 999);
            let (rolls, sum) = roll_dice(count, sides);

            // Build the individual roll display
            let roll_str: Vec<String> = rolls
                .iter()
                .map(|&r| {
                    if sides == 6 {
                        d6_face(r).to_string()
                    } else {
                        format!("【{r}】")
                    }
                })
                .collect();

            format!(
                r#"<span class="dice-roll" title="{}d{} roll">🎲 {}d{} ▸ {} = {}</span>"#,
                count,
                sides,
                count,
                sides,
                roll_str.join(" "),
                sum,
            )
        })
        .into_owned()
}

/// Emoji shortcode table — :name: → Unicode glyph
fn apply_emoji(text: &str) -> String {
    // Common shortcodes. Extend as desired.
    const CODES: &[(&str, &str)] = &[
        (":smile:", "😊"),
        (":lol:", "😂"),
        (":kek:", "🤣"),
        (":rage:", "😡"),
        (":cry:", "😢"),
        (":think:", "🤔"),
        (":eyes:", "👀"),
        (":fire:", "🔥"),
        (":check:", "✅"),
        (":x:", "❌"),
        (":heart:", "❤️"),
        (":ok:", "👌"),
        (":cool:", "😎"),
        (":skull:", "💀"),
        (":shrug:", "🤷"),
        (":pray:", "🙏"),
        (":nerd:", "🤓"),
        (":clown:", "🤡"),
        (":100:", "💯"),
        (":gg:", "🎮"),
        (":rip:", "⚰️"),
        (":based:", "🗿"),
        (":ngmi:", "😬"),
        (":gm:", "🌅"),
        (":uwu:", "🥺"),
        (":owo:", "👁️👄👁️"),
    ];
    // Early-exit if no colon is present — avoids all string allocations for posts
    // that contain no emoji shortcodes (the common case).
    if !text.contains(':') {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (code, emoji) in CODES {
        // Skip the replace call entirely when the shortcode is absent, avoiding
        // an allocation for each of the 26 patterns on every post render.
        if out.contains(code) {
            out = out.replace(code, emoji);
        }
    }
    out
}

/// Escape HTML special characters to prevent XSS.
///
/// Single-pass implementation: builds the output in one scan without any
/// intermediate allocations, unlike the chained `.replace()` approach which
/// produces up to five heap-allocated intermediates per call.
#[must_use]
#[allow(clippy::arithmetic_side_effects)]
pub fn escape_html(s: &str) -> String {
    // Pre-allocate with a small headroom for the most common entities.
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Apply word filters to raw (unescaped) text.
#[must_use]
pub fn apply_word_filters(text: &str, filters: &[(String, String)]) -> String {
    let mut result = text.to_string();
    for (pattern, replacement) in filters {
        if !pattern.is_empty() {
            result = result.replace(pattern.as_str(), replacement.as_str());
        }
    }
    result
}

/// Maximum post body length accepted by the sanitizer pipeline.
/// Input longer than this is rejected before any regex runs, preventing
/// theoretical `ReDoS` on deeply-nested or pathological patterns.
const MAX_BODY_BYTES: usize = 32 * 1024; // 32 KiB

/// Convert plain escaped post body into HTML with imageboard markup.
/// Input: HTML-escaped user text.  Output: HTML with markup applied.
///
/// Returns an error notice if the input exceeds `MAX_BODY_BYTES` to prevent `DoS`
/// via extremely large inputs through the regex pipeline.
///
/// When 3 or more consecutive greentext lines appear they are wrapped in a
/// `<details open>` block — expanded by default.  The admin site-settings
/// panel can enable "collapse greentext walls", which is handled purely on
/// the client side (JS removes the `open` attribute when the page-level
/// `data-collapse-greentext` attribute is present on `<body>`).
#[must_use]
#[allow(clippy::arithmetic_side_effects)]
pub fn render_post_body(escaped: &str) -> String {
    // Hard length guard before touching any regex. Must be enforced here
    // (not only at the HTTP layer) because the sanitizer is also called
    // from background workers and tests.
    if escaped.len() > MAX_BODY_BYTES {
        return format!(
            "<em>[Post body too large — truncated at {} KiB]</em>",
            MAX_BODY_BYTES / 1024
        );
    }
    // Dice tags are resolved first — rolls are seeded from OsRng at post creation
    // time and stored in body_html, making them immutable for all future readers.
    let escaped = apply_dice(escaped);
    let lines: Vec<&str> = escaped.lines().collect();
    let mut html = String::with_capacity(escaped.len() * 2);
    let mut i = 0;

    #[allow(clippy::indexing_slicing)] // i < lines.len() and j < lines.len() are invariants
    while i < lines.len() {
        let line = lines[i];

        // Greentext block: lines starting with &gt; that aren't reply links
        if line.starts_with("&gt;") && !line.starts_with("&gt;&gt;") {
            // Collect all consecutive greentext lines
            let mut group = vec![line];
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j];
                if next.starts_with("&gt;") && !next.starts_with("&gt;&gt;") {
                    group.push(next);
                    j += 1;
                } else {
                    break;
                }
            }

            // 3+ consecutive greentext lines → collapsible block, open by default.
            // The `open` attribute keeps it expanded; the admin "collapse walls"
            // setting removes it client-side via JS without changing stored HTML.
            #[allow(clippy::items_after_statements)]
            if group.len() >= 3 {
                let count = group.len();
                use std::fmt::Write as _;
                let _ = write!(html, "<details open class=\"greentext-block\"><summary class=\"quote\">&gt; {count} lines</summary>");
                for ql in &group {
                    let _ = write!(
                        html,
                        "<span class=\"quote\">{}</span><br>",
                        render_inline(ql)
                    );
                }
                html.push_str("</details>");
            } else {
                use std::fmt::Write as _;
                for ql in &group {
                    let _ = write!(
                        html,
                        "<span class=\"quote\">{}</span><br>",
                        render_inline(ql)
                    );
                }
            }
            i = j;
        } else {
            html.push_str(&render_inline(line));
            html.push_str("<br>");
            i += 1;
        }
    }

    // Remove trailing <br>
    if html.ends_with("<br>") {
        html.truncate(html.len() - 4);
    }

    html
}

/// Apply all inline markup transformations to a single line of HTML-escaped text.
fn render_inline(text: &str) -> String {
    let mut result = text.to_string();

    // >>>/board/POST_ID → crosspost link (post redirect + hover preview data attrs)
    // >>>/board/        → board index link
    // Both handled in one pass by RE_CROSSLINK so there is no second-pass corruption.
    result = RE_CROSSLINK
        .replace_all(&result, |caps: &regex::Captures| {
            let board = &caps[1];
            caps.get(2).map_or_else(
                || format!(r#"<a href="/{board}" class="quotelink crosslink">&gt;&gt;&gt;/{board}/</a>"#),
                |pid| {
                    let pid = pid.as_str();
                    format!(
                        r#"<a href="/{board}/post/{pid}" class="quotelink crosslink" data-crossboard="{board}" data-pid="{pid}">&gt;&gt;&gt;/{board}/{pid}</a>"#,
                    )
                },
            )
        })
        .into_owned();

    // >>N reply links
    result = RE_REPLY
        .replace_all(&result, |caps: &regex::Captures| {
            let n = &caps[1];
            format!(r##"<a href="#p{n}" class="quotelink" data-pid="{n}">&gt;&gt;{n}</a>"##)
        })
        .into_owned();

    // URLs — also append a video embed placeholder when the URL is a known video link.
    // The placeholder is an empty <span> with data attributes; the client-side embed
    // script replaces it with a thumbnail + iframe when embeds are enabled for the board.
    result = RE_URL
        .replace_all(&result, |caps: &regex::Captures| {
            let url = &caps[1];
            let clean_url = url.trim_end_matches(['.', ',', ')', ';', '\'']);
            let trailing = &url[clean_url.len()..];
            // Escape clean_url in both href and display text.
            // RE_URL excludes & < > so escaping is safe today, but applying
            // escape_html() consistently guards against future call-site changes.
            let escaped_url = escape_html(clean_url);
            let link = format!(
                r#"<a href="{escaped_url}" rel="nofollow noopener" target="_blank">{escaped_url}</a>{trailing}"#
            );
            // Check for supported video embed URLs. Emit only the embed span —
            // the URL becomes a data attribute and the span text, not a hyperlink.
            // The client-side buildEmbed() function replaces the span with a
            // thumbnail+iframe widget positioned before the post body (like a webm).
            if let Some((embed_type, embed_id)) = extract_video_embed(clean_url) {
                format!(
                    r#"<span class="video-unfurl" data-embed-type="{etype}" data-embed-id="{eid}" data-url="{url}">{display}{trail}</span>"#,
                    etype   = embed_type,
                    eid     = embed_id,
                    url     = escape_html(clean_url),
                    display = escape_html(clean_url),
                    trail   = trailing,
                )
            } else {
                link
            }
        })
        .into_owned();

    // [spoiler]…[/spoiler]
    result = RE_SPOILER
        .replace_all(&result, |caps: &regex::Captures| {
            format!(
                r#"<span class="spoiler" onclick="this.classList.toggle('revealed')">{}</span>"#,
                &caps[1]
            )
        })
        .into_owned();

    // **bold**
    result = RE_BOLD
        .replace_all(&result, "<strong>$1</strong>")
        .into_owned();

    // __italic__
    result = RE_ITALIC.replace_all(&result, "<em>$1</em>").into_owned();

    // Emoji shortcodes (applied last, after HTML transforms)
    result = apply_emoji(&result);

    result
}

/// Sanitize a file name: keep only safe characters.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    let name = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'], "_");
    name.chars().take(100).collect()
}

/// Validate and truncate post body.
///
/// # Errors
/// Returns `Err` if the body is empty or exceeds 4096 characters.
pub fn validate_body(body: &str) -> Result<&str, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("Post body cannot be empty.".into());
    }
    // Use .chars().count() rather than .len() (byte count) so that multi-byte
    // characters (e.g. CJK) are measured correctly.  A post of 1,366 CJK
    // characters is 4,098 UTF-8 bytes and would be wrongly rejected by a
    // byte-length check despite being well within the 4,096-character limit.
    if trimmed.chars().count() > 4096 {
        return Err("Post body exceeds 4096 characters.".into());
    }
    Ok(trimmed)
}

/// Validate the body when a file attachment may substitute for text.
///
/// Rules:
///   • If `has_file` is true, an empty body is allowed — the file is enough.
///   • If `has_file` is false, the body must not be blank (no empty posts).
///   • Body length is still capped at 4096 characters regardless.
///
/// Returns the trimmed body (may be empty when a file is present).
///
/// # Errors
/// Returns `Err` if the body exceeds 4096 characters, or if it is empty and no file is present.
pub fn validate_body_with_file(body: &str, has_file: bool) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.chars().count() > 4096 {
        return Err("Post body exceeds 4096 characters.".into());
    }
    if trimmed.is_empty() && !has_file {
        return Err("Post must include either text or an attached file.".into());
    }
    Ok(trimmed.to_string())
}

/// Validate and truncate a name field.
#[must_use]
pub fn validate_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Anonymous".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Validate and truncate subject field.
#[must_use]
pub fn validate_subject(subject: &str) -> Option<String> {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(128).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_html() {
        assert_eq!(
            escape_html("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(escape_html("a & b"), "a &amp; b");
    }

    #[test]
    fn test_escape_html_single_pass_idempotent() {
        // Verify the single-pass implementation handles all five entities correctly.
        let input = r#"<>"'&"#;
        let escaped = escape_html(input);
        assert_eq!(escaped, "&lt;&gt;&quot;&#x27;&amp;");
        // Escaping the already-escaped string must not corrupt the entities.
        let double = escape_html(&escaped);
        assert!(
            double.contains("&amp;amp;"),
            "& in entity must be escaped again"
        );
    }

    #[test]
    fn test_greentext() {
        let escaped = escape_html(">be me");
        let html = render_post_body(&escaped);
        assert!(html.contains("class=\"quote\""));
    }

    #[test]
    fn test_reply_link() {
        let escaped = escape_html(">>12345 nice post");
        let html = render_post_body(&escaped);
        assert!(html.contains("class=\"quotelink\""));
        assert!(html.contains("#p12345"));
    }

    #[test]
    fn test_collapsible_greentext() {
        // 3+ consecutive greentext lines are always wrapped in <details open>.
        // The `open` attribute keeps them expanded by default; the admin toggle
        // removes it client-side via JS without touching server-rendered HTML.
        let raw = ">line1\n>line2\n>line3";
        let escaped = escape_html(raw);
        let html = render_post_body(&escaped);
        assert!(
            html.contains("<details"),
            "3+ greentext lines should produce a <details> block"
        );
        assert!(
            html.contains("3 lines"),
            "summary should state the line count"
        );
        assert!(
            html.contains("class=\"quote\""),
            "lines should render as quote spans"
        );
        // Expanded by default — the open attribute must be present
        assert!(
            html.contains("open"),
            "<details> should carry the open attribute by default"
        );
    }

    #[test]
    fn test_short_greentext_no_collapse() {
        // Fewer than 3 lines must NOT produce a <details> block.
        let raw = ">one line only";
        let escaped = escape_html(raw);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("<details"),
            "1–2 greentext lines should not be wrapped in <details>"
        );
        assert!(html.contains("class=\"quote\""));
    }

    #[test]
    fn test_spoiler() {
        // [spoiler] is our own markup, not HTML — pass the raw tag directly.
        let html = render_post_body("[spoiler]secret[/spoiler]");
        assert!(html.contains("class=\"spoiler\""));
        assert!(html.contains("secret"));
    }

    #[test]
    fn test_emoji_shortcode() {
        let html = render_post_body(":fire: hot take");
        assert!(html.contains("🔥"));
    }

    #[test]
    fn test_emoji_no_colon_fast_path() {
        // Input with no colon must not trigger any replace calls.
        let input = "plain text without any colons";
        let result = apply_emoji(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_crosspost_link() {
        let escaped = escape_html(">>>/tech/42");
        let html = render_post_body(&escaped);
        assert!(html.contains("class=\"quotelink crosslink\""));
        // href now resolves via the post-redirect endpoint, not a raw thread URL
        assert!(html.contains("/tech/post/42"));
        assert!(html.contains("data-crossboard=\"tech\""));
        assert!(html.contains("data-pid=\"42\""));
    }

    #[test]
    fn test_crosspost_not_corrupted_by_crossboard() {
        // RE_CROSSBOARD must not re-match the display text inside an already-replaced
        // crosspost anchor and produce a double-wrapped or href-corrupted link.
        let escaped = escape_html(">>>/b/12345");
        let html = render_post_body(&escaped);
        // Exactly one anchor tag
        assert_eq!(
            html.matches("<a ").count(),
            1,
            "must produce exactly one anchor"
        );
        // href must point to the post redirect, not the board index
        assert!(
            html.contains("href=\"/b/post/12345\""),
            "href must be the post link"
        );
        assert!(
            !html.contains("href=\"/b/\""),
            "href must not be the board index"
        );
    }

    #[test]
    fn test_crossboard_link_no_post_id() {
        // >>>/board/ (no post number) should produce a board-index link.
        // href uses the canonical slash-free form; the trailing-slash middleware
        // redirects any /b/ URLs at runtime so both forms resolve correctly.
        let escaped = escape_html(">>>/b/");
        let html = render_post_body(&escaped);
        assert!(html.contains("href=\"/b\""));
    }

    #[test]
    fn test_sanitize_filename_multibyte() {
        let cjk: String = "日".repeat(50);
        let long_name = format!("{cjk}.jpg");
        let result = sanitize_filename(&long_name);
        assert!(result.chars().count() <= 100);
    }

    #[test]
    fn test_word_filter_before_escape() {
        let raw = "this is bad&word";
        let filters = vec![("bad&word".to_string(), "filtered".to_string())];
        let filtered = apply_word_filters(raw, &filters);
        assert_eq!(filtered, "this is filtered");
        let escaped = escape_html(&filtered);
        assert!(escaped.contains("filtered"));
    }

    #[test]
    fn test_url_trailing_punct() {
        let escaped = escape_html("see https://example.com/foo. and https://example.com/bar,");
        let html = render_post_body(&escaped);
        assert!(!html.contains("href=\"https://example.com/foo.\""));
        assert!(!html.contains("href=\"https://example.com/bar,\""));
    }

    // ─── XSS vector tests ────────────────────────────────────────────────────

    #[test]
    fn test_xss_script_tag() {
        let input = "<script>alert(1)</script>";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("<script>"),
            "raw <script> must not appear in output"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "script tags must be entity-escaped"
        );
    }

    #[test]
    fn test_xss_event_attribute() {
        let input = "<img onerror=alert(1)>";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("<img"),
            "raw HTML tags must not pass through"
        );
    }

    #[test]
    fn test_xss_javascript_url() {
        // javascript: URLs must not become clickable hrefs
        let input = "javascript:alert(1)";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        // RE_URL only matches http:// and https:// — javascript: must not be linked
        assert!(
            !html.contains("href=\"javascript:"),
            "javascript: URL must not be linkified"
        );
    }

    #[test]
    fn test_xss_data_uri() {
        let input = "data:text/html,<script>alert(1)</script>";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("href=\"data:"),
            "data: URI must not be linkified"
        );
    }

    #[test]
    fn test_xss_style_attribute() {
        let input = "<p style=\"background:url(javascript:alert(1))\">x</p>";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(!html.contains("<p "), "raw p tag must not appear");
    }

    #[test]
    fn test_xss_entity_encoded_script() {
        // &lt;script&gt; in the raw input should stay double-escaped after processing
        let input = "&lt;script&gt;alert(1)&lt;/script&gt;";
        // Already escaped by a hypothetical upstream — escape_html again to simulate
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("<script>"),
            "double-encoded script must not become executable"
        );
    }

    // ─── Edge case tests ─────────────────────────────────────────────────────

    #[test]
    fn test_max_body_length_guard() {
        // Input exceeding MAX_BODY_BYTES must be rejected without panicking
        let huge = "A".repeat(MAX_BODY_BYTES + 1);
        let result = render_post_body(&huge);
        assert!(
            result.contains("too large"),
            "oversized input must produce a truncation notice"
        );
    }

    #[test]
    fn test_deeply_nested_spoilers() {
        // Deeply nested spoilers should not panic or produce runaway output
        let depth = 50;
        let input = "[spoiler]".repeat(depth) + "x" + &"[/spoiler]".repeat(depth);
        let result = render_post_body(&input);
        // Must complete without panic; output length should be bounded
        assert!(
            result.len() < input.len() * 10,
            "output must not grow unboundedly"
        );
    }

    #[test]
    fn test_malformed_crossboard_link() {
        // Invalid board slug characters must not produce broken HTML
        let escaped = escape_html(">>>/BOARD_WITH_CAPS/123");
        let html = render_post_body(&escaped);
        // Should not match RE_CROSSLINK (which only accepts [a-z0-9]+)
        assert!(
            !html.contains("crosslink"),
            "uppercase board slug must not be linkified"
        );
    }

    #[test]
    fn test_reply_link_no_numeric_overflow() {
        // Extremely large post IDs should not cause integer overflow
        let escaped = escape_html(">>99999999999999999999");
        let html = render_post_body(&escaped);
        // Must render as a link regardless of numeric size
        assert!(
            html.contains("quotelink"),
            "large post ID should still produce a link"
        );
    }

    #[test]
    fn test_only_gt_chars_does_not_panic() {
        // Input consisting entirely of > characters should not panic or loop
        let input: String = ">".repeat(1000);
        let escaped = escape_html(&input);
        let _html = render_post_body(&escaped); // must not panic
    }

    #[test]
    fn test_long_greentext_chain_collapsible() {
        // 100 consecutive greentext lines should produce exactly one <details> block
        let raw = (0..100)
            .map(|i| format!(">line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let escaped = escape_html(&raw);
        let html = render_post_body(&escaped);
        assert_eq!(
            html.matches("<details").count(),
            1,
            "100 lines = exactly one details block"
        );
        assert!(
            html.contains("100 lines"),
            "summary should reflect the actual line count"
        );
    }

    #[test]
    fn test_empty_input_does_not_panic() {
        let html = render_post_body("");
        // Empty input → empty output (no crash)
        assert!(html.is_empty() || html.len() < 10);
    }

    #[test]
    fn test_bold_and_italic_do_not_xss() {
        // **<script>** and __<script>__ must not inject HTML
        let input = "**<script>alert(1)</script>**";
        let escaped = escape_html(input);
        let html = render_post_body(&escaped);
        assert!(
            !html.contains("<script>"),
            "bold markup must not bypass XSS escaping"
        );
        assert!(html.contains("<strong>"), "bold markup should still render");
    }
}
