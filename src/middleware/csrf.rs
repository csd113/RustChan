// src/middleware/csrf.rs

use crate::{config::CONFIG, utils::crypto::sign_csrf_token};

pub fn validate_csrf(cookie_token: Option<&str>, form_token: &str) -> bool {
    if form_token.is_empty() {
        return false;
    }

    if let Some(cookie) = cookie_token {
        if !cookie.is_empty() && constant_time_eq(cookie.as_bytes(), form_token.as_bytes()) {
            return true;
        }
    }

    let Some((raw, sig)) = form_token.rsplit_once('.') else {
        return false;
    };
    if raw.is_empty() || sig.is_empty() {
        return false;
    }

    let expected = sign_csrf_token(raw, &CONFIG.cookie_secret);
    constant_time_eq(expected.as_bytes(), sig.as_bytes())
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
    fn csrf_signed_form_token_passes_without_cookie() {
        let secret = &crate::config::CONFIG.cookie_secret;
        let signed = crate::utils::crypto::make_csrf_form_token("abc123", secret);
        assert!(validate_csrf(None, &signed));
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
