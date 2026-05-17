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

fn extract_yt_id(url: &str) -> Option<String> {
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
    if let Some(pos) = url.find("/embed/") {
        let rest = &url[pos + 7..];
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

fn extract_yt_id_from_watch_param(url: &str) -> Option<String> {
    for prefix in ["?v=", "&v="] {
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

fn extract_streamable_id(url: &str) -> Option<String> {
    if let Some(pos) = url.find("streamable.com/") {
        let rest = &url[pos + 15..];
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

fn roll_dice(count: u32, sides: u32) -> (Vec<u32>, u32) {
    let mut rolls = Vec::with_capacity(count as usize);
    let mut sum = 0u32;
    for _ in 0..count {
        let roll = (crate::utils::crypto::os_random_u32_or_exit("rolling dice markup") % sides) + 1;
        rolls.push(roll);
        sum = sum.saturating_add(roll);
    }
    (rolls, sum)
}

pub(super) fn apply_dice(text: &str, re_dice: &regex::Regex) -> String {
    re_dice
        .replace_all(text, |caps: &regex::Captures| {
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

pub(super) fn apply_emoji(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        out.push_str(&replace_emoji_shortcodes(&rest[..start]));

        let after_tag = &rest[start..];
        let Some(end) = after_tag.find('>') else {
            out.push_str(&replace_emoji_shortcodes(after_tag));
            return out;
        };
        out.push_str(&after_tag[..=end]);
        rest = &after_tag[end + 1..];
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
            Some(("youtube", "dQw4w9WgXcQ".to_owned()))
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
