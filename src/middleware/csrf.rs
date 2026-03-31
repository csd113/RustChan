pub fn validate_csrf(cookie_token: Option<&str>, form_token: &str) -> bool {
    match cookie_token {
        Some(cookie) => {
            if cookie.is_empty() || form_token.is_empty() {
                return false;
            }
            constant_time_eq(cookie.as_bytes(), form_token.as_bytes())
        }
        None => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, validate_csrf};

    #[test]
    fn csrf_matching_tokens_pass() {
        assert!(validate_csrf(Some("abc123"), "abc123"));
    }

    #[test]
    fn csrf_mismatched_tokens_fail() {
        assert!(!validate_csrf(Some("abc123"), "abc124"));
    }

    #[test]
    fn csrf_missing_cookie_fails() {
        assert!(!validate_csrf(None, "abc123"));
    }

    #[test]
    fn csrf_empty_cookie_fails() {
        assert!(!validate_csrf(Some(""), "abc123"));
    }

    #[test]
    fn csrf_empty_form_token_fails() {
        assert!(!validate_csrf(Some("abc123"), ""));
    }

    #[test]
    fn constant_time_eq_equal_slices() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }
}
