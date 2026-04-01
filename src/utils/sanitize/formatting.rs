use rand_core::{OsRng, RngCore};

/// Try to extract a (`embed_type`, `embed_id`) pair from a URL.
///
/// Supports `YouTube` (youtube.com and youtu.be), any Invidious instance
/// (detected by the `/watch?v=` path), Streamable, X/Twitter status URLs,
/// and Instagram post/reel URLs.
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
    if url.contains("twitter.com/") || url.contains("x.com/") {
        if let Some(id) = extract_status_id(url) {
            return Some(("twitter", id));
        }
    }
    if url.contains("instagram.com/") {
        if let Some(id) = extract_instagram_shortcode(url) {
            return Some(("instagram", id));
        }
    }
    if !url.contains("youtube.com") && !url.contains("youtu.be") && url.contains("/watch") {
        if let Some(id) = extract_yt_id_from_watch_param(url) {
            return Some(("youtube", id));
        }
    }
    None
}

#[allow(clippy::arithmetic_side_effects)]
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
    extract_yt_id_from_watch_param(url)
}

#[allow(clippy::arithmetic_side_effects)]
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

#[allow(clippy::arithmetic_side_effects)]
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

fn extract_path_segment_after<'a>(url: &'a str, marker: &str) -> Option<&'a str> {
    let pos = url.find(marker)?;
    let rest = &url[pos + marker.len()..];
    let segment = rest
        .split(['/', '?', '#', '&'])
        .next()
        .unwrap_or_default();
    (!segment.is_empty()).then_some(segment)
}

fn is_safe_embed_id(id: &str, min_len: usize, max_len: usize) -> bool {
    let len = id.len();
    len >= min_len
        && len <= max_len
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn extract_status_id(url: &str) -> Option<String> {
    for marker in ["/status/", "/statuses/"] {
        if let Some(id) = extract_path_segment_after(url, marker) {
            if !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn extract_instagram_shortcode(url: &str) -> Option<String> {
    for marker in ["/p/", "/reel/", "/tv/"] {
        if let Some(id) = extract_path_segment_after(url, marker) {
            if is_safe_embed_id(id, 5, 32) {
                return Some(id.to_string());
            }
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

#[allow(clippy::arithmetic_side_effects)]
fn roll_dice(count: u32, sides: u32) -> (Vec<u32>, u32) {
    let mut rolls = Vec::with_capacity(count as usize);
    let mut sum = 0u32;
    for _ in 0..count {
        let roll = (OsRng.next_u32() % sides) + 1;
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

pub(super) fn apply_emoji(text: &str) -> String {
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
        return text.to_string();
    }
    let mut out = text.to_string();
    for (code, emoji) in CODES {
        if out.contains(code) {
            out = out.replace(code, emoji);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::extract_video_embed;

    #[test]
    fn extracts_youtube_embed() {
        assert_eq!(
            extract_video_embed("https://youtu.be/dQw4w9WgXcQ?t=43"),
            Some(("youtube", "dQw4w9WgXcQ".to_string()))
        );
    }

    #[test]
    fn extracts_twitter_status_embed() {
        assert_eq!(
            extract_video_embed("https://x.com/OpenAI/status/1890000000000000000"),
            Some(("twitter", "1890000000000000000".to_string()))
        );
    }

    #[test]
    fn extracts_instagram_post_embed() {
        assert_eq!(
            extract_video_embed("https://www.instagram.com/p/C7DcdExample/?img_index=1"),
            Some(("instagram", "C7DcdExample".to_string()))
        );
    }
}
