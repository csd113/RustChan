// Shared helpers for internal redirects and query-string construction.

#[must_use]
/// Percent-encode a value for use as a URI query component.
pub fn encode_query_component(input: &str) -> String {
    encode_query_component_with_space(input, "%20")
}

#[must_use]
/// Form-encode a value for use in an `application/x-www-form-urlencoded` query.
pub fn encode_form_query_component(input: &str) -> String {
    encode_query_component_with_space(input, "+")
}

/// Encode a query component using the caller-selected space representation.
fn encode_query_component_with_space(input: &str, space: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push_str(space),
            _ => {
                encoded.push('%');
                encoded.push(hex_digit(byte >> 4));
                encoded.push(hex_digit(byte & 0x0f));
            }
        }
    }
    encoded
}

/// Convert a hexadecimal nibble to its uppercase ASCII digit.
fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0'.saturating_add(nibble)),
        10..=15 => char::from(b'A'.saturating_add(nibble.saturating_sub(10))),
        _ => '?',
    }
}

#[must_use]
/// Return whether `path` has the minimum shape required for an internal URL.
pub fn is_basic_safe_internal_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\")
}

#[must_use]
/// Return whether `path` is an internal URL without traversal-like components.
pub fn is_strict_safe_internal_path(path: &str) -> bool {
    is_basic_safe_internal_path(path)
        && !path.contains("//")
        && !path.contains("..")
        && !path.contains('\\')
        && !path.to_ascii_lowercase().contains("%5c")
}

#[must_use]
/// Select a minimally validated internal path, or return `fallback`.
pub fn safe_internal_path_or<'a>(path: Option<&'a str>, fallback: &'a str) -> &'a str {
    path.filter(|value| is_basic_safe_internal_path(value))
        .unwrap_or(fallback)
}

#[must_use]
/// Select a strictly validated internal path, or return `fallback`.
pub fn strict_safe_internal_path_or<'a>(path: Option<&'a str>, fallback: &'a str) -> &'a str {
    path.filter(|value| is_strict_safe_internal_path(value))
        .unwrap_or(fallback)
}
