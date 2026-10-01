use crate::config::{RateLimitPolicy, CONFIG};
use axum::{
    extract::Request,
    http::Method,
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

/// Decide which requests consume the browsing budget without weakening
/// independent posting cooldowns or password-attempt protections.
fn counts_request(method: &Method, path: &str, policy: RateLimitPolicy) -> bool {
    let exempt = path.starts_with("/static/")
        || path.starts_with("/theme-css/")
        || path.starts_with("/boards/")
        || path == "/admin/log/live"
        || path == "/admin/backup/progress";
    !exempt
        && (policy == RateLimitPolicy::Legacy || method == Method::GET || method == Method::HEAD)
}

/// Advance one visitor's counter using the historical whole-second boundary.
const fn consume_budget(counter: &mut (u32, u64), now: u64, window: u64, limit: u32) -> bool {
    let (count, window_start) = counter;
    if now.saturating_sub(*window_start) > window {
        *count = 1;
        *window_start = now;
        false
    } else {
        *count = count.saturating_add(1);
        *count > limit
    }
}

/// Apply the configured per-visitor browsing request limit.
pub async fn rate_limit_middleware(req: Request, next: Next) -> Response {
    if !counts_request(req.method(), req.uri().path(), CONFIG.rate_limit_policy) {
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
        let blocked = consume_budget(binding.value_mut(), now, window, limit);
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
        RATE_TABLE.retain(|_, (_, window_start)| {
            now.saturating_sub(*window_start) <= window.saturating_mul(2)
        });
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::{consume_budget, counts_request, Method, RateLimitPolicy};

    #[test]
    fn legacy_counts_writes_polling_and_nonexempt_assets() {
        for method in [Method::GET, Method::HEAD, Method::POST, Method::DELETE] {
            for path in [
                "/",
                "/test",
                "/api/thread/1",
                "/favicon.ico",
                "/banner/assets/1",
            ] {
                assert!(
                    counts_request(&method, path, RateLimitPolicy::Legacy),
                    "legacy must count {method} {path}"
                );
            }
            for path in [
                "/static/main.js",
                "/theme-css/forest",
                "/boards/test/file.png",
                "/admin/log/live",
                "/admin/backup/progress",
            ] {
                assert!(
                    !counts_request(&method, path, RateLimitPolicy::Legacy),
                    "existing exemption changed: {path}"
                );
                assert!(
                    !counts_request(&method, path, RateLimitPolicy::Reads),
                    "reads exemption changed: {path}"
                );
            }
        }
    }

    #[test]
    fn reads_policy_counts_get_and_head_only() {
        for method in [Method::GET, Method::HEAD] {
            assert!(
                counts_request(&method, "/api/thread/1", RateLimitPolicy::Reads),
                "reads must count polling"
            );
        }
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS] {
            assert!(
                !counts_request(&method, "/test", RateLimitPolicy::Reads),
                "reads must not count {method}"
            );
        }
    }

    #[test]
    fn budget_boundary_and_independent_visitors_preserve_defaults() {
        let mut visitor_a = (0, 100);
        let mut visitor_b = (0, 100);
        assert!(
            !consume_budget(&mut visitor_a, 100, 60, 2),
            "first request allowed"
        );
        assert!(
            !consume_budget(&mut visitor_a, 100, 60, 2),
            "second request allowed"
        );
        assert!(
            consume_budget(&mut visitor_a, 160, 60, 2),
            "equal boundary retains historical limit"
        );
        assert!(
            !consume_budget(&mut visitor_b, 160, 60, 2),
            "other visitor remains independent"
        );
        assert!(
            !consume_budget(&mut visitor_a, 161, 60, 2),
            "greater boundary resets"
        );
        assert_eq!(visitor_a, (1, 161), "rollover consumes one request");
    }

    #[test]
    fn counter_cannot_wrap_to_bypass_limit() {
        let mut visitor = (u32::MAX, 100);
        assert!(
            consume_budget(&mut visitor, 101, 60, 60),
            "saturated counter remains blocked"
        );
    }
}
