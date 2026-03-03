// utils/sanitize.rs
//
// XSS Prevention: User input NEVER goes to templates unescaped.
// Every piece of user text passes through `escape_html` before insertion.
//
// Post markup: After escaping, we apply simple pattern transforms:
//   • Lines starting with ">" become <span class="quote">
//   • >>12345 becomes a clickable reply link
//   • URLs become hyperlinks (restricted to http/https)
//   • **bold** → <strong>bold</strong>
//
// Word filters:
//   FIX[MEDIUM-8]: Word filters are now applied to the RAW text BEFORE HTML
//   escaping. This means filter patterns are written as plain text (e.g. "bad"
//   not "&amp;bad"). Applying filters after escaping caused patterns containing
//   &, <, >, ', " to never match, which was confusing and incorrect.

use once_cell::sync::Lazy;
use regex::Regex;

static RE_REPLY: Lazy<Regex> = Lazy::new(|| Regex::new(r"&gt;&gt;(\d+)").unwrap());
// FIX[LOW-6]: Strip common trailing punctuation from matched URLs.
// Previously the regex matched trailing ) . , ; which broke links in prose.
static RE_URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(https?://[^\s&<>]{3,300})").unwrap()
});
static RE_BOLD:   Lazy<Regex> = Lazy::new(|| Regex::new(r"\*\*([^*]+)\*\*").unwrap());
static RE_ITALIC: Lazy<Regex> = Lazy::new(|| Regex::new(r"__([^_]+)__").unwrap());

/// Escape HTML special characters to prevent XSS.
/// This must be called on ALL user-supplied strings before embedding in HTML.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Apply word filters to raw (unescaped) text.
///
/// FIX[MEDIUM-8]: Filters now run on the raw body text BEFORE HTML escaping,
/// so patterns are plain text strings (not HTML-entity-encoded). Call order:
///   1. apply_word_filters(raw_body)
///   2. escape_html(filtered_body)
///   3. render_post_body(escaped_body)
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
/// Input: HTML-escaped user text (safe to work with).
/// Output: HTML string with markup applied.
pub fn render_post_body(escaped: &str) -> String {
    let mut html = String::with_capacity(escaped.len() * 2);

    for line in escaped.lines() {
        let rendered = render_line(line);
        html.push_str(&rendered);
        html.push_str("<br>");
    }

    // Remove trailing <br>
    if html.ends_with("<br>") {
        html.truncate(html.len() - 4);
    }

    html
}

fn render_line(line: &str) -> String {
    // Greentext: lines starting with ">" (already HTML-escaped to "&gt;")
    if line.starts_with("&gt;") && !line.starts_with("&gt;&gt;") {
        let inner = render_inline(line);
        return format!("<span class=\"quote\">{}</span>", inner);
    }
    render_inline(line)
}

fn render_inline(text: &str) -> String {
    // Apply inline transforms:
    // 1. >>N  → reply links
    // 2. URLs → hyperlinks
    // 3. **text** → bold
    // 4. __text__ → italic (spoiler-style)

    let mut result = text.to_string();

    // >>N reply links (escaped form is &gt;&gt;N)
    result = RE_REPLY
        .replace_all(&result, r##"<a href="#p$1" class="quotelink">&gt;&gt;$1</a>"##)
        .into_owned();

    // URLs (http/https only — no javascript: or other schemes)
    // FIX[LOW-6]: Strip trailing punctuation characters that are not part of
    // the URL itself but commonly appear after URLs in prose sentences.
    result = RE_URL
        .replace_all(&result, |caps: &regex::Captures| {
            let url = &caps[1];
            // Strip trailing punctuation that is not URL-structural
            let clean_url = url.trim_end_matches(|c| matches!(c, '.' | ',' | ')' | ';' | '\''));
            let trailing = &url[clean_url.len()..];
            format!(
                r#"<a href="{}" rel="nofollow noopener" target="_blank">{}</a>{}"#,
                clean_url, clean_url, trailing
            )
        })
        .into_owned();

    // **bold**
    result = RE_BOLD
        .replace_all(&result, "<strong>$1</strong>")
        .into_owned();

    // __italic__ (spoiler)
    result = RE_ITALIC
        .replace_all(&result, "<em>$1</em>")
        .into_owned();

    result
}

/// Sanitize a file name: keep only safe characters.
/// Strips path separators and anything non-ASCII-safe.
///
/// FIX[MEDIUM-7]: The original used `name[..100]` which is a byte-index slice
/// and panics when byte 100 falls inside a multi-byte UTF-8 sequence (e.g. CJK
/// or emoji in filenames). This now uses char iteration which is always safe.
pub fn sanitize_filename(name: &str) -> String {
    let name = name
        .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'], "_");
    // Limit to 100 Unicode scalar values (not bytes), safe for all inputs.
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

    // FIX[MEDIUM-7]: Verify sanitize_filename never panics on multi-byte chars
    #[test]
    fn test_sanitize_filename_multibyte() {
        // 50 CJK chars = 150 UTF-8 bytes — would panic the old byte-slice version
        let cjk: String = std::iter::repeat('日').take(50).collect();
        let long_name = format!("{}.jpg", cjk);
        let result = sanitize_filename(&long_name);
        // Should not panic, and must be at most 100 chars
        assert!(result.chars().count() <= 100);
    }

    // FIX[MEDIUM-8]: Verify word filters match raw text, not escaped text
    #[test]
    fn test_word_filter_before_escape() {
        let raw = "this is bad&word";
        let filters = vec![("bad&word".to_string(), "filtered".to_string())];
        let filtered = apply_word_filters(raw, &filters);
        assert_eq!(filtered, "this is filtered");
        // Escaping afterward should not affect the substitution result
        let escaped = escape_html(&filtered);
        assert!(escaped.contains("filtered"));
    }

    // FIX[LOW-6]: URL should not include trailing punctuation
    #[test]
    fn test_url_trailing_punct() {
        let escaped = escape_html("see https://example.com/foo. and https://example.com/bar,");
        let html = render_post_body(&escaped);
        // The href should not end with '.' or ','
        assert!(!html.contains("href=\"https://example.com/foo.\""));
        assert!(!html.contains("href=\"https://example.com/bar,\""));
    }
}
