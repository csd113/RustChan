use crate::config::CONFIG;
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use super::ip::extract_ip;

/// Per-client request count and window start, keyed by a client-address hash.
static RATE_TABLE: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);
/// Unix timestamp of the most recent rate-table cleanup.
static LAST_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

/// Returns the current Unix timestamp in whole seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Applies the configured GET request limit to dynamic routes.
pub async fn rate_limit_middleware(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/static/")
        || path.starts_with("/theme-css/")
        || path.starts_with("/boards/")
        || path == "/admin/log/live"
        || path == "/admin/backup/progress"
    {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    let ip_key = {
        use sha2::{Digest as _, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(ip.as_bytes());
        hasher.update(b"G");
        hex::encode(hasher.finalize())
    };

    let now = now_secs();
    let window = CONFIG.rate_limit_window;
    let limit = CONFIG.rate_limit_gets;

    let blocked = {
        let mut binding = RATE_TABLE.entry(ip_key).or_insert((0, now));
        let (count, window_start) = binding.value_mut();
        let blocked = if now.saturating_sub(*window_start) > window {
            *count = 1;
            *window_start = now;
            false
        } else {
            *count += 1;
            *count > limit
        };
        drop(binding);
        blocked
    };

    if blocked {
        let jar = axum_extra::extract::CookieJar::from_headers(req.headers());
        let theme = jar
            .get("rustchan_theme")
            .map(axum_extra::extract::cookie::Cookie::value);
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::Extension(crate::error::ErrorPage::RateLimit),
            axum::response::Html(crate::templates::rate_limit_page_with_preferences(
                theme,
                None,
                "",
                crate::templates::UserPreferences::default(),
            )),
        )
            .into_response();
    }

    let last_cleanup = LAST_CLEANUP_SECS.load(Ordering::Relaxed);
    let should_clean = RATE_TABLE.len() > 5000 || now.saturating_sub(last_cleanup) > 600;
    if should_clean
        && LAST_CLEANUP_SECS
            .compare_exchange(last_cleanup, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
    {
        RATE_TABLE.retain(|_, (_, window_start)| now.saturating_sub(*window_start) <= window * 2);
    }

    next.run(req).await
}
