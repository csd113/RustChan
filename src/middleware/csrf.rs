use crate::{
    config::CONFIG,
    utils::crypto::{sign_csrf_token, sign_scoped_csrf_token},
};

/// Validates a form CSRF token against either its cookie or its embedded signature.
#[must_use]
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
    if cookie_token.is_some_and(|cookie| !constant_time_eq(cookie.as_bytes(), raw.as_bytes())) {
        return false;
    }

    let expected = sign_csrf_token(raw, &CONFIG.cookie_secret);
    constant_time_eq(expected.as_bytes(), sig.as_bytes())
}

/// Validates a signed CSRF token for the supplied session scope.
#[must_use]
pub fn validate_signed_csrf(
    cookie_token: Option<&str>,
    scope: Option<&str>,
    form_token: &str,
) -> bool {
    let Some(cookie_token) = cookie_token.filter(|token| !token.is_empty()) else {
        return false;
    };
    let Some(scope) = scope.filter(|token| !token.is_empty()) else {
        return false;
    };
    let Some((raw, sig)) = form_token.rsplit_once('.') else {
        return false;
    };
    if raw.is_empty() || sig.is_empty() {
        return false;
    }
    if !constant_time_eq(raw.as_bytes(), cookie_token.as_bytes()) {
        return false;
    }

    let expected = sign_scoped_csrf_token(raw, &CONFIG.cookie_secret, scope);
    constant_time_eq(expected.as_bytes(), sig.as_bytes())
}

/// Compares equal-length byte slices without data-dependent early exits.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
