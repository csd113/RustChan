use axum::http::{header, HeaderMap, HeaderValue};

// Built-in CSS/JS URLs are stable rather than fingerprinted, so they get only a
// short public cache. Uploaded media uses unique storage paths and should not
// mutate at the same URL. Dynamic/admin/CSRF-bearing HTML must revalidate to
// avoid stale UI, ownership, session, and form-token state.
pub const CACHE_CONTROL_DYNAMIC_PUBLIC: &str = "no-cache";
pub const CACHE_CONTROL_PRIVATE_NO_CACHE: &str = "private, no-cache, must-revalidate";
pub const CACHE_CONTROL_STATIC_SHORT: &str = "public, max-age=3600";
pub const CACHE_CONTROL_IMMUTABLE_MEDIA: &str = "public, max-age=31536000, immutable";

pub fn insert_cache_control_if_absent(headers: &mut HeaderMap, value: &'static str) {
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(value));
}

pub fn set_cache_control(headers: &mut HeaderMap, value: &'static str) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
}
