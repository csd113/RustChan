/// Try to extract a (`embed_type`, `video_id`) pair from a URL.
///
/// Supports `YouTube` (youtube.com and youtu.be), any Invidious instance
/// (detected by the `/watch?v=` path), and Streamable.
/// Returns None for all other URLs.
#[must_use]
pub fn extract_video_embed(url: &str) -> Option<(&'static str, String)> {
    if url.contains("youtube.com") || url.contains("youtu.be") {
        if let Some(id) = extract_yt_id(url) {
            return Some(("youtube", id));
        }
    }
    if url.contains("streamable.com/") {
        if let Some(code) = extract_streamable_id(url) {
            return Some(("streamable", code));
        }
    }
    if !url.contains("youtube.com") && !url.contains("youtu.be") && url.contains("/watch") {
        if let Some(id) = extract_yt_id_from_watch_param(url) {
            return Some(("youtube", id));
        }
    }
    None
}

/// Extract a validated `YouTube` video identifier from a supported URL.
fn extract_yt_id(url: &str) -> Option<String> {
    if let Some(rest) = suffix_after(url, "youtu.be/") {
        let id: String = rest.chars().take(11).collect();
        if id.len() == 11
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Some(id);
        }
    }
    if let Some(rest) = suffix_after(url, "/shorts/") {
        let id: String = rest.chars().take(11).collect();
        if id.len() == 11
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Some(id);
        }
    }
    if let Some(rest) = suffix_after(url, "/embed/") {
        let id: String = rest.chars().take(11).collect();
        if id.len() == 11
            && id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            return Some(id);
        }
    }
    extract_yt_id_from_watch_param(url)
}

/// Extract the `v` parameter from a `YouTube` watch URL.
fn extract_yt_id_from_watch_param(url: &str) -> Option<String> {
    for prefix in ["?v=", "&v="] {
        if let Some(rest) = suffix_after(url, prefix) {
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

/// Extract a validated `Streamable` video identifier from a supported URL.
fn extract_streamable_id(url: &str) -> Option<String> {
    if let Some(rest) = suffix_after(url, "streamable.com/") {
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

/// Returns the suffix after the first exact marker occurrence.
fn suffix_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let suffix_start = text.find(marker)?.checked_add(marker.len())?;
    text.get(suffix_start..)
}

/// Maps a six-sided die value to its Unicode face.
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

/// Roll a bounded dice expression with operating-system randomness.
fn roll_dice(count: u32, sides: u32) -> (Vec<u32>, u32) {
    let mut rolls = Vec::new();
    let mut sum = 0u32;
    for _ in 0..count {
        let roll = (crate::utils::crypto::os_random_u32_or_exit("rolling dice markup") % sides) + 1;
        rolls.push(roll);
        sum = sum.saturating_add(roll);
    }
    (rolls, sum)
}

/// Replace validated dice expressions with a rendered roll result.
pub(super) fn apply_dice(text: &str, re_dice: &regex::Regex) -> String {
    re_dice
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let count: u32 = caps[1].parse().unwrap_or(1).clamp(1, 20);
            let sides: u32 = caps[2].parse().unwrap_or(6).clamp(2, 999);
            let (rolls, sum) = roll_dice(count, sides);

            let roll_str: Vec<String> = rolls
                .iter()
                .map(|&roll| {
                    if sides == 6 {
                        d6_face(roll).to_string()
                    } else {
                        format!("【{roll}】")
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

/// Replace recognized emoji shortcodes outside protected markup.
fn replace_emoji_shortcodes(text: &str) -> String {
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
    if !text.contains(':') {
        return text.to_owned();
    }
    let mut out = text.to_owned();
    for (code, emoji) in CODES {
        if out.contains(code) {
            out = out.replace(code, emoji);
        }
    }
    out
}

/// Apply emoji replacements only outside existing HTML tags.
pub(super) fn apply_emoji(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        let Some((before_tag, after_tag)) = rest.split_at_checked(start) else {
            break;
        };
        out.push_str(&replace_emoji_shortcodes(before_tag));

        let Some(end) = after_tag.find('>') else {
            out.push_str(&replace_emoji_shortcodes(after_tag));
            return out;
        };
        let Some((tag, after_tag)) = after_tag.split_at_checked(end + 1) else {
            out.push_str(&replace_emoji_shortcodes(after_tag));
            return out;
        };
        out.push_str(tag);
        rest = after_tag;
    }

    out.push_str(&replace_emoji_shortcodes(rest));
    out
}

#[cfg(test)]
mod tests {
    use super::extract_video_embed;

    #[test]
    fn extracts_youtube_embed() {
        assert_eq!(
            extract_video_embed("https://youtu.be/dQw4w9WgXcQ?t=43"),
            Some(("youtube", "dQw4w9WgXcQ".to_owned())),
            "supported YouTube short URL was not recognized"
        );
    }

    #[test]
    fn extracts_youtube_embed_from_supported_routes_with_extra_query_params() {
        for url in [
            "https://www.youtube.com/watch?v=zN9Cb-rNF9U",
            "https://www.youtube.com/watch?v=zN9Cb-rNF9U&amp;list=RDzN9Cb-rNF9U&amp;start_radio=1",
            "https://youtube.com/watch?v=zN9Cb-rNF9U&amp;list=RDzN9Cb-rNF9U",
            "https://www.youtube.com/watch?v=zN9Cb-rNF9U&amp;t=30s",
            "https://youtu.be/zN9Cb-rNF9U?si=abc123",
            "https://www.youtube.com/shorts/zN9Cb-rNF9U?feature=share",
            "https://www.youtube.com/embed/zN9Cb-rNF9U?start=30",
        ] {
            assert_eq!(
                extract_video_embed(url),
                Some(("youtube", "zN9Cb-rNF9U".to_owned())),
                "{url}"
            );
        }
    }
}
