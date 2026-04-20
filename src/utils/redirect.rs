// Shared helpers for internal redirects and query-string construction.

#[must_use]
pub fn encode_query_component(input: &str) -> String {
    encode_query_component_with_space(input, "%20")
}

#[must_use]
pub fn encode_form_query_component(input: &str) -> String {
    encode_query_component_with_space(input, "+")
}

fn encode_query_component_with_space(input: &str, space: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push_str(space),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

#[must_use]
pub fn is_basic_safe_internal_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\")
}

#[must_use]
pub fn is_strict_safe_internal_path(path: &str) -> bool {
    is_basic_safe_internal_path(path)
        && !path.contains("//")
        && !path.contains("..")
        && !path.contains('\\')
        && !path.to_ascii_lowercase().contains("%5c")
}

#[must_use]
pub fn safe_internal_path_or<'a>(path: Option<&'a str>, fallback: &'a str) -> &'a str {
    path.filter(|value| is_basic_safe_internal_path(value))
        .unwrap_or(fallback)
}

#[must_use]
pub fn strict_safe_internal_path_or<'a>(path: Option<&'a str>, fallback: &'a str) -> &'a str {
    path.filter(|value| is_strict_safe_internal_path(value))
        .unwrap_or(fallback)
}
