use axum::http::{header, HeaderMap, HeaderValue};

// Built-in CSS/JS URLs are stable rather than fingerprinted, so they get only a
// short public cache. Uploaded media uses unique storage paths and should not
// mutate at the same URL. Dynamic/admin/CSRF-bearing HTML must revalidate to
// avoid stale UI, ownership, session, and form-token state.
/// Revalidation policy for publicly cacheable dynamic responses.
pub const CACHE_CONTROL_DYNAMIC_PUBLIC: &str = "no-cache";
/// Revalidation policy for private dynamic responses.
pub const CACHE_CONTROL_PRIVATE_NO_CACHE: &str = "private, no-cache, must-revalidate";
/// No-store policy for sensitive private responses.
pub const CACHE_CONTROL_PRIVATE_NO_STORE: &str =
    "private, no-cache, no-store, must-revalidate, no-transform";
/// Short-lived caching policy for stable built-in assets.
pub const CACHE_CONTROL_STATIC_SHORT: &str = "public, max-age=3600";
/// Long-lived caching policy for content-addressed uploaded media.
pub const CACHE_CONTROL_IMMUTABLE_MEDIA: &str = "public, max-age=31536000, immutable";

/// Insert `Cache-Control` only when the response does not already define it.
pub fn insert_cache_control_if_absent(headers: &mut HeaderMap, value: &'static str) {
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(value));
}

/// Replace the response's `Cache-Control` header with `value`.
pub fn set_cache_control(headers: &mut HeaderMap, value: &'static str) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
}

/// Ensure a response's `Vary` header includes `Cookie`.
pub fn insert_vary_cookie(headers: &mut HeaderMap) {
    let Some(existing) = headers
        .get(header::VARY)
        .and_then(|value| value.to_str().ok())
    else {
        headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
        return;
    };

    if existing
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("cookie") || part.trim() == "*")
    {
        return;
    }

    let combined = format!("{existing}, Cookie");
    if let Ok(value) = HeaderValue::from_str(&combined) {
        headers.insert(header::VARY, value);
    }
}
