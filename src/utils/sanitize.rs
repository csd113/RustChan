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

use regex::Regex;
use std::sync::LazyLock;

/// Inline markup, embed, emoji, and dice formatting helpers.
mod formatting;

pub use formatting::extract_video_embed;
use formatting::{apply_dice, apply_emoji};

/// Matches escaped same-board post references such as `&gt;&gt;123`.
#[expect(
    clippy::expect_used,
    reason = "the reply-reference regex is a fixed source literal"
)]
static RE_REPLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&gt;&gt;([0-9]+)").expect("RE_REPLY is valid"));

/// Matches escaped cross-board links and optional thread identifiers.
#[expect(
    clippy::expect_used,
    reason = "the cross-board-link regex is a fixed source literal"
)]
static RE_CROSSLINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"&gt;&gt;&gt;/([a-z0-9]{1,8})/([0-9]+)?").expect("RE_CROSSLINK is valid")
});

/// Matches HTTP(S) URLs after HTML escaping.
#[expect(
    clippy::expect_used,
    reason = "the URL regex is a fixed source literal"
)]
static RE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(https?://(?:[^\s&<>"']|&amp;){3,300})"#).expect("RE_URL is valid")
});

/// Matches the supported bold markup delimiters.
#[expect(
    clippy::expect_used,
    reason = "the bold-markup regex is a fixed source literal"
)]
static RE_BOLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*([^*]+)\*\*").expect("RE_BOLD is valid"));

/// Matches the supported italic markup delimiters.
#[expect(
    clippy::expect_used,
    reason = "the italic-markup regex is a fixed source literal"
)]
static RE_ITALIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"__([^_]+)__").expect("RE_ITALIC is valid"));

/// Matches spoiler markup, including content spanning line breaks.
#[expect(
    clippy::expect_used,
    reason = "the spoiler-markup regex is a fixed source literal"
)]
static RE_SPOILER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[spoiler\]([\s\S]*?)\[/spoiler\]").expect("RE_SPOILER is valid")
});

/// Matches bounded dice expressions such as `[dice 2d20]`.
#[expect(
    clippy::expect_used,
    reason = "the dice-expression regex is a fixed source literal"
)]
static RE_DICE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[dice (\d{1,2})d(\d{1,3})\]").expect("RE_DICE is valid"));

/// Escape HTML special characters to prevent XSS.
///
/// Single-pass implementation: builds the output in one scan without any
/// intermediate allocations, unlike the chained `.replace()` approach which
/// produces up to five heap-allocated intermediates per call.
#[must_use]
pub fn escape_html(s: &str) -> String {
    // Pre-allocate with a small headroom for the most common entities.
    let mut out = String::with_capacity(s.len().checked_add(s.len() / 8).unwrap_or(s.len()));
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
    let mut result = text.to_owned();
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
/// When 3 or more consecutive greentext lines appear they can be wrapped in a
/// `<details open>` block — expanded by default. A board's admin settings
/// control whether the collapsible wrapper is emitted at all.
#[must_use]
pub fn render_post_body(escaped: &str, collapse_greentext: bool) -> String {
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
    let escaped = apply_dice(escaped, &RE_DICE);
    let mut html = String::with_capacity(escaped.len().checked_mul(2).unwrap_or(escaped.len()));
    let mut lines = escaped.lines().peekable();

    while let Some(line) = lines.next() {
        // Greentext block: lines starting with &gt; that aren't reply links
        if line.starts_with("&gt;") && !line.starts_with("&gt;&gt;") {
            let mut group = vec![line];
            while let Some(next) =
                lines.next_if(|next| next.starts_with("&gt;") && !next.starts_with("&gt;&gt;"))
            {
                group.push(next);
            }

            // 3+ consecutive greentext lines → collapsible block, open by default
            // when the board enables collapse_greentext. Otherwise we render the
            // quote lines plainly so no collapse UI exists at all.
            if collapse_greentext && group.len() >= 3 {
                let count = group.len();
                crate::templates::append_html(
                    &mut html,
                    format_args!(
                        "<details open class=\"greentext-block\"><summary class=\"quote\">&gt; {count} lines</summary>"
                    ),
                );
                for ql in &group {
                    crate::templates::append_html(
                        &mut html,
                        format_args!("<span class=\"quote\">{}</span><br>", render_inline(ql)),
                    );
                }
                html.push_str("</details>");
            } else {
                for ql in &group {
                    crate::templates::append_html(
                        &mut html,
                        format_args!("<span class=\"quote\">{}</span><br>", render_inline(ql)),
                    );
                }
            }
        } else {
            html.push_str(&render_inline(line));
            html.push_str("<br>");
        }
    }

    // Remove trailing <br>
    if html.ends_with("<br>") {
        html.truncate(html.len().saturating_sub(4));
    }

    html
}

/// Normalize stored post HTML to match the current greentext collapse setting.
///
/// When collapse is disabled, this strips the generated `<details>` wrapper
/// from any existing greentext blocks while keeping the quote lines intact.
#[must_use]
pub fn normalize_greentext_blocks(body_html: &str, collapse_greentext: bool) -> String {
    const OPEN_TAG: &str = "<details open class=\"greentext-block\">";
    const SUMMARY_SUFFIX: &str = "</summary>";
    const CLOSE_TAG: &str = "</details>";

    if collapse_greentext {
        return body_html.to_owned();
    }

    let mut out = String::with_capacity(body_html.len());
    let mut rest = body_html;

    while let Some((before_open, after_open)) = rest.split_once(OPEN_TAG) {
        out.push_str(before_open);

        let Some((_summary, after_summary)) = after_open.split_once(SUMMARY_SUFFIX) else {
            out.push_str(rest);
            return out;
        };

        let Some((inner, after_close)) = after_summary.split_once(CLOSE_TAG) else {
            out.push_str(rest);
            return out;
        };

        out.push_str(inner);
        rest = after_close;
    }

    out.push_str(rest);
    out
}

/// Apply all inline markup transformations to a single line of HTML-escaped text.
fn render_inline(text: &str) -> String {
    let mut result = text.to_owned();

    // >>>/board/POST_ID → crosspost link (post redirect + hover preview data attrs)
    // >>>/board/        → board index link
    // Both handled in one pass by RE_CROSSLINK so there is no second-pass corruption.
    result = RE_CROSSLINK
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let Some(whole) = caps.get(0) else {
                return String::new();
            };
            let invalid_prefix = result.get(..whole.start()).is_some_and(|prefix| prefix.ends_with("&gt;"));
            let invalid_suffix = result.get(whole.end()..).and_then(|suffix| suffix.chars().next())
                .is_some_and(|next| next.is_alphanumeric() || next == '_' || (caps.get(2).is_none() && next == '-'));
            if invalid_prefix || invalid_suffix {
                return whole.as_str().to_owned();
            }
            let Some(board) = caps.get(1).map(|value| value.as_str()) else {
                return caps
                    .get(0)
                    .map_or_else(String::new, |value| value.as_str().to_owned());
            };
            caps.get(2).map_or_else(
                || format!(r#"<a href="/{board}" class="quotelink crosslink">&gt;&gt;&gt;/{board}/</a>"#),
                |post_match| {
                    let raw_pid = post_match.as_str();
                    let Some(pid) = raw_pid.parse::<i64>().ok().filter(|id| *id > 0) else {
                        return whole.as_str().to_owned();
                    };
                    format!(
                        r#"<a href="/{board}/post/{pid}" class="quotelink crosslink" data-crossboard="{board}" data-pid="{pid}">&gt;&gt;&gt;/{board}/{pid}</a>"#,
                    )
                },
            )
        })
        .into_owned();

    // >>N reply links
    result = RE_REPLY
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let Some(n) = caps.get(1).map(|value| value.as_str()) else {
                return caps
                    .get(0)
                    .map_or_else(String::new, |value| value.as_str().to_owned());
            };
            let Some(whole) = caps.get(0) else {
                return String::new();
            };
            let valid_boundary = !result
                .get(..whole.start())
                .is_some_and(|prefix| prefix.ends_with("&gt;"))
                && !result
                    .get(whole.end()..)
                    .and_then(|suffix| suffix.chars().next())
                    .is_some_and(|next| next.is_alphanumeric() || next == '_');
            let Some(pid) = n.parse::<i64>().ok().filter(|id| *id > 0 && valid_boundary) else {
                return whole.as_str().to_owned();
            };
            format!(r##"<a href="#p{pid}" class="quotelink" data-pid="{pid}">&gt;&gt;{n}</a>"##)
        })
        .into_owned();

    // Format escaped text before generating URL attributes. Applying markup to
    // generated anchors could put markup's quotes inside a hostile href.
    // [spoiler]…[/spoiler]
    result = RE_SPOILER
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let spoiler = caps.get(1).map_or("", |value| value.as_str());
            format!(r#"<span class="spoiler" data-action="toggle-spoiler">{spoiler}</span>"#)
        })
        .into_owned();

    // **bold**
    result = RE_BOLD
        .replace_all(&result, "<strong>$1</strong>")
        .into_owned();

    // __italic__
    result = RE_ITALIC.replace_all(&result, "<em>$1</em>").into_owned();

    // URLs — also append a video embed placeholder when the URL is a known video link.
    // The placeholder is an empty <span> with data attributes; the client-side embed
    // script replaces it with a thumbnail + iframe when embeds are enabled for the board.
    result = RE_URL
        .replace_all(&result, |caps: &regex::Captures<'_>| {
            let Some(url) = caps.get(1).map(|value| value.as_str()) else {
                return caps
                    .get(0)
                    .map_or_else(String::new, |value| value.as_str().to_owned());
            };
            let clean_url = url.trim_end_matches(['.', ',', ')', ';', '\'']);
            let trailing = url.strip_prefix(clean_url).unwrap_or_default();
            // render_post_body receives already-escaped text. RE_URL only admits
            // raw URL characters plus escaped ampersand separators, so clean_url
            // is safe to place directly in href/text without double-escaping
            // query strings from `&amp;` to `&amp;amp;`.
            let link = format!(
                r#"<a href="{clean_url}" rel="nofollow noopener" target="_blank">{clean_url}</a>{trailing}"#
            );
            // Check for supported video embed URLs. Emit only the embed span —
            // the URL becomes a data attribute and the span text, not a hyperlink.
            // The client-side buildEmbed() function replaces the span with a
            // thumbnail+iframe widget positioned before the post body (like a webm).
            if let Some((embed_type, embed_id)) = extract_video_embed(clean_url) {
                let display_url = if embed_type == "youtube" {
                    format!("https://www.youtube.com/watch?v={embed_id}")
                } else {
                    clean_url.to_owned()
                };
                format!(
                    r#"<span class="video-unfurl" data-embed-type="{etype}" data-embed-id="{eid}" data-url="{url}">{display}{trail}</span>"#,
                    etype   = embed_type,
                    eid     = embed_id,
                    url     = display_url.as_str(),
                    display = display_url.as_str(),
                    trail   = trailing,
                )
            } else {
                link
            }
        })
        .into_owned();

    // Emoji shortcodes (applied last, after HTML transforms)
    result = apply_emoji(&result);

    result
}

/// Keeps safe filename characters and a bounded extension when truncating.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    const MAX_FILE_NAME_CHARS: usize = 100;
    let name = name.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'], "_");
    if let Some((stem, extension)) = name.rsplit_once('.') {
        let suffix_chars = extension.chars().count().saturating_add(1);
        if !stem.is_empty() && !extension.is_empty() && suffix_chars < MAX_FILE_NAME_CHARS {
            let prefix: String = stem
                .chars()
                .take(MAX_FILE_NAME_CHARS.saturating_sub(suffix_chars))
                .collect();
            return format!("{prefix}.{extension}");
        }
    }
    name.chars().take(MAX_FILE_NAME_CHARS).collect()
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
    Ok(trimmed.to_owned())
}

/// Validate and truncate a name field.
#[must_use]
pub fn validate_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Anonymous".to_owned()
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

/// Safely render legacy markup that predates the attribute-context correction.
/// Only malformed cached HTML falls back to escaped text, preserving valid dice rolls.
#[must_use]
pub fn safe_stored_post_html(body: &str, body_html: &str, collapse_greentext: bool) -> String {
    let malformed_url = body_html.len() > 256 * 1024
        || ["href=\"", "data-url=\""].iter().any(|attribute| {
            body_html.split(attribute).skip(1).any(|suffix| {
                suffix
                    .split('"')
                    .next()
                    .is_some_and(|value| value.contains('<'))
            })
        });
    if malformed_url {
        escape_html(&body.chars().take(4096).collect::<String>()).replace('\n', "<br>")
    } else {
        normalize_greentext_blocks(body_html, collapse_greentext)
    }
}

/// Keep in-page quotes local and route references outside the rendered window safely.
#[must_use]
pub fn route_missing_quote_links<S: std::hash::BuildHasher>(
    html: &str,
    board: &str,
    visible: &std::collections::HashSet<i64, S>,
) -> String {
    static QUOTE_HREF: LazyLock<Option<Regex>> =
        LazyLock::new(|| Regex::new(r##"href="#p([0-9]+)""##).ok());
    let Some(regex) = QUOTE_HREF.as_ref() else {
        return html.to_owned();
    };
    let board = escape_html(board);
    regex
        .replace_all(html, |caps: &regex::Captures<'_>| {
            let Some(pid) = caps
                .get(1)
                .and_then(|value| value.as_str().parse::<i64>().ok())
            else {
                return caps
                    .get(0)
                    .map_or_else(String::new, |value| value.as_str().to_owned());
            };
            if visible.contains(&pid) {
                format!(r##"href="#p{pid}""##)
            } else {
                format!(r#"href="/{board}/post/{pid}""#)
            }
        })
        .into_owned()
}
