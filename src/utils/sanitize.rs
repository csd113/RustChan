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

use once_cell::sync::Lazy;
use regex::Regex;

static RE_REPLY:      Lazy<Regex> = Lazy::new(|| Regex::new(r"&gt;&gt;(\d+)").unwrap());
static RE_CROSSTHREAD: Lazy<Regex> = Lazy::new(|| Regex::new(r"&gt;&gt;&gt;/([a-z0-9]+)/(\d+)").unwrap());
static RE_CROSSBOARD:  Lazy<Regex> = Lazy::new(|| Regex::new(r"&gt;&gt;&gt;/([a-z0-9]+)/").unwrap());
static RE_URL:        Lazy<Regex> = Lazy::new(|| Regex::new(r"(https?://[^\s&<>]{3,300})").unwrap());
static RE_BOLD:       Lazy<Regex> = Lazy::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
static RE_ITALIC:     Lazy<Regex> = Lazy::new(|| Regex::new(r"__([^_]+)__").unwrap());
static RE_SPOILER:    Lazy<Regex> = Lazy::new(|| Regex::new(r"\[spoiler\]([\s\S]*?)\[/spoiler\]").unwrap());

/// Emoji shortcode table — :name: → Unicode glyph
fn apply_emoji(text: &str) -> String {
    // Common shortcodes. Extend as desired.
    const CODES: &[(&str, &str)] = &[
        (":smile:",   "😊"), (":lol:",    "😂"), (":kek:",    "🤣"),
        (":rage:",    "😡"), (":cry:",    "😢"), (":think:",  "🤔"),
        (":eyes:",    "👀"), (":fire:",   "🔥"), (":check:",  "✅"),
        (":x:",       "❌"), (":heart:",  "❤️"), (":ok:",    "👌"),
        (":cool:",    "😎"), (":skull:",  "💀"), (":shrug:",  "🤷"),
        (":pray:",    "🙏"), (":nerd:",   "🤓"), (":clown:",  "🤡"),
        (":100:",     "💯"), (":gg:",     "🎮"), (":rip:",    "⚰️"),
        (":based:",   "🗿"), (":ngmi:",   "😬"), (":gm:",     "🌅"),
        (":uwu:",     "🥺"), (":owo:",    "👁️👄👁️"),
    ];
    let mut out = text.to_string();
    for (code, emoji) in CODES {
        out = out.replace(code, emoji);
    }
    out
}

/// Escape HTML special characters to prevent XSS.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Apply word filters to raw (unescaped) text.
pub fn apply_word_filters(text: &str, filters: &[(String, String)]) -> String {
    let mut result = text.to_string();
    for (pattern, replacement) in filters {
        if !pattern.is_empty() {
            result = result.replace(pattern.as_str(), replacement.as_str());
        }
    }
    result
}

/// Convert plain escaped post body into HTML with imageboard markup.
/// Input: HTML-escaped user text.  Output: HTML with markup applied.
pub fn render_post_body(escaped: &str) -> String {
    let lines: Vec<&str> = escaped.lines().collect();
    let mut html = String::with_capacity(escaped.len() * 2);
    let mut i = 0;

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

            for ql in &group {
                    html.push_str(&format!(
                        "<span class=\"quote\">{}</span><br>",
                        render_inline(ql)
                    ));
                }
            i = j;
        } else {
            html.push_str(&render_line(line));
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

fn render_line(line: &str) -> String {
    render_inline(line)
}

fn render_inline(text: &str) -> String {
    let mut result = text.to_string();

    // Cross-board thread links: >>>/board/123  (check BEFORE >>)
    result = RE_CROSSTHREAD
        .replace_all(&result, |caps: &regex::Captures| {
            let board = &caps[1];
            let tid   = &caps[2];
            format!(
                r#"<a href="/{board}/thread/{tid}" class="quotelink crosslink">&gt;&gt;&gt;/{board}/{tid}</a>"#,
            )
        })
        .into_owned();

    // Cross-board links: >>>/board/
    result = RE_CROSSBOARD
        .replace_all(&result, |caps: &regex::Captures| {
            let board = &caps[1];
            format!(
                r#"<a href="/{board}/" class="quotelink crosslink">&gt;&gt;&gt;/{board}/</a>"#,
            )
        })
        .into_owned();

    // >>N reply links
    result = RE_REPLY
        .replace_all(&result, r##"<a href="#p$1" class="quotelink">&gt;&gt;$1</a>"##)
        .into_owned();

    // URLs
    result = RE_URL
        .replace_all(&result, |caps: &regex::Captures| {
            let url = &caps[1];
            let clean_url = url.trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ';' | '\''));
            let trailing = &url[clean_url.len()..];
            format!(
                r#"<a href="{}" rel="nofollow noopener" target="_blank">{}</a>{}"#,
                clean_url, clean_url, trailing
            )
        })
        .into_owned();

    // [spoiler]…[/spoiler]
    result = RE_SPOILER
        .replace_all(&result, |caps: &regex::Captures| {
            format!(r#"<span class="spoiler" onclick="this.classList.toggle('revealed')">{}</span>"#, &caps[1])
        })
        .into_owned();

    // **bold**
    result = RE_BOLD
        .replace_all(&result, "<strong>$1</strong>")
        .into_owned();

    // __italic__
    result = RE_ITALIC
        .replace_all(&result, "<em>$1</em>")
        .into_owned();

    // Emoji shortcodes (applied last, after HTML transforms)
    result = apply_emoji(&result);

    result
}

/// Sanitize a file name: keep only safe characters.
pub fn sanitize_filename(name: &str) -> String {
    let name = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'], "_");
    name.chars().take(100).collect()
}

/// Validate and truncate post body.
pub fn validate_body(body: &str) -> Result<&str, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err("Post body cannot be empty.".into());
    }
    if trimmed.len() > 4096 {
        return Err("Post body exceeds 4096 characters.".into());
    }
    Ok(trimmed)
}

/// Validate the body when a file attachment may substitute for text.
///
/// Rules:
///   • If `has_file` is true, an empty body is allowed — the file is enough.
///   • If `has_file` is false, the body must not be blank (no empty posts).
///   • Body length is still capped at 4096 regardless.
///
/// Returns the trimmed body (may be empty when a file is present).
pub fn validate_body_with_file(body: &str, has_file: bool) -> Result<String, String> {
    let trimmed = body.trim();
    if trimmed.len() > 4096 {
        return Err("Post body exceeds 4096 characters.".into());
    }
    if trimmed.is_empty() && !has_file {
        return Err("Post must include either text or an attached file.".into());
    }
    Ok(trimmed.to_string())
}

/// Validate and truncate a name field.
pub fn validate_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Anonymous".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Validate and truncate subject field.
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
        assert_eq!(escape_html("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(escape_html("a & b"), "a &amp; b");
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
        let raw = ">line1\n>line2\n>line3";
        let escaped = escape_html(raw);
        let html = render_post_body(&escaped);
        assert!(html.contains("<details"));
        assert!(html.contains("3 lines"));
    }

    #[test]
    fn test_spoiler() {
        let escaped = escape_html("[spoiler]secret[/spoiler]");
        // spoiler tag is NOT html-escaped (it's our markup)
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
    fn test_crossthread_link() {
        let escaped = escape_html(">>>/tech/42");
        let html = render_post_body(&escaped);
        assert!(html.contains("class=\"quotelink crosslink\""));
        assert!(html.contains("/tech/thread/42"));
    }

    #[test]
    fn test_sanitize_filename_multibyte() {
        let cjk: String = std::iter::repeat('日').take(50).collect();
        let long_name = format!("{}.jpg", cjk);
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
}

