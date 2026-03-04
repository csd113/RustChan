// middleware/mod.rs
//
// Two middleware systems:
//
// Rate Limiter — In-memory sliding window per IP address.
//   • Uses DashMap (lock-free concurrent HashMap) to track (count, window_start).
//   • On each POST request, we check if IP has exceeded CONFIG.rate_limit_posts
//     within the last CONFIG.rate_limit_window seconds.
//   • Memory: ~200 bytes per IP entry. 10,000 concurrent IPs = ~2 MiB.
//   • Resets on restart (acceptable; no persistent attack state needed).
//   • FIX[MEDIUM-4]: cleanup now also runs on a time-based cadence to prevent
//     unbounded growth under sustained attacks with rotating IPs.
//
// CSRF Protection — Double-submit cookie pattern.
//   • On every page load (GET), we set a "csrf_token" cookie if absent.
//   • Every POST form includes a hidden "_csrf" field with the same token value.
//   • On POST, middleware verifies hidden field == cookie value.
//   • Since cookies are same-site, a cross-origin request can't read the cookie.
//   • Cookie is SameSite=Strict.
//
// IP Extraction — FIX[HIGH-1]:
//   When CHAN_BEHIND_PROXY=true, we read X-Real-IP (set by nginx to
//   $remote_addr, which cannot be forged by the client). We do NOT trust
//   X-Forwarded-For's leftmost entry, which is client-controlled and trivially
//   forgeable. Trusting the leftmost XFF entry allows an attacker to bypass
//   rate limiting and IP bans by cycling through spoofed IPs.

use crate::config::CONFIG;
use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Global rate limit table: ip_key → (request_count, window_start_secs)
static RATE_TABLE: Lazy<DashMap<String, (u32, u64)>> = Lazy::new(DashMap::new);

/// FIX[MEDIUM-4]: Track the last time we ran a full cleanup so we can also
/// clean on a time basis, not just when the table exceeds a size threshold.
static LAST_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

/// Shared state for extracting the DB pool in middleware
#[derive(Clone)]
pub struct AppState {
    pub db:               crate::db::DbPool,
    /// True when ffmpeg was detected at startup (set by detect::detect_ffmpeg).
    /// Passed to file handling to enable/disable video thumbnail generation.
    pub ffmpeg_available: bool,
}

/// Get current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Rate limit middleware — applied to ALL POST routes.
/// Blocks requests from IPs that exceed the configured threshold.
pub async fn rate_limit_middleware(
    req: Request,
    next: Next,
) -> Response {
    // Only rate-limit POST requests (thread/reply creation, etc.)
    if req.method() != axum::http::Method::POST {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    // Hash the IP so raw addresses are never kept in process memory.
    let ip_key = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(ip.as_bytes());
        hex::encode(h.finalize())
    };
    let now = now_secs();
    let window = CONFIG.rate_limit_window;
    let limit = CONFIG.rate_limit_posts;

    // Check and update rate limit counter
    let blocked = {
        let mut entry = RATE_TABLE.entry(ip_key.clone()).or_insert((0, now));
        let (count, window_start) = entry.value_mut();

        if now.saturating_sub(*window_start) > window {
            // Window has expired, reset
            *count = 1;
            *window_start = now;
            false
        } else {
            *count += 1;
            *count > limit
        }
    };

    if blocked {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::RETRY_AFTER, "60"),
            ],
            crate::templates::error_page(429, "You are posting too fast. Please wait before posting again."),
        )
            .into_response();
    }

    // FIX[MEDIUM-4]: Clean old entries when the table grows large OR at least
    // once every 10 minutes (600 seconds), whichever comes first.
    let last_cleanup = LAST_CLEANUP_SECS.load(Ordering::Relaxed);
    let should_clean = RATE_TABLE.len() > 5000
        || now.saturating_sub(last_cleanup) > 600;

    if should_clean {
        // Use compare_exchange to avoid concurrent threads all cleaning simultaneously
        if LAST_CLEANUP_SECS
            .compare_exchange(last_cleanup, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            RATE_TABLE.retain(|_, (_, window_start)| {
                now.saturating_sub(*window_start) <= window * 2
            });
        }
    }

    next.run(req).await
}

/// Extract client IP, respecting proxy headers when configured.
///
/// FIX[HIGH-1]: When behind_proxy=true, we now prefer X-Real-IP (set by nginx
/// to $remote_addr — the actual TCP peer — and not modifiable by the client).
/// We explicitly do NOT use the leftmost X-Forwarded-For entry because it is
/// client-supplied and trivially forgeable, enabling rate-limit and ban bypass.
///
/// If X-Real-IP is absent but X-Forwarded-For is present, we take the
/// rightmost entry (the last proxy in the chain), which is also not
/// client-controlled when the chain passes through a trusted proxy.
pub fn extract_ip(req: &Request) -> String {
    if CONFIG.behind_proxy {
        // Prefer X-Real-IP (set by nginx to $remote_addr; unforgeable)
        if let Some(real_ip) = req.headers().get("x-real-ip") {
            if let Ok(val) = real_ip.to_str() {
                let trimmed = val.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }

        // Fall back to the RIGHTMOST X-Forwarded-For entry (added by the
        // trusted proxy, not by the client).
        if let Some(fwd) = req.headers().get("x-forwarded-for") {
            if let Ok(val) = fwd.to_str() {
                if let Some(ip) = val.split(',').next_back() {
                    let trimmed = ip.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }

    // Direct connection IP (not behind proxy, or proxy headers absent)
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Validate CSRF token from a form against the cookie.
/// Returns true if valid, false if CSRF check fails.
pub fn validate_csrf(cookie_token: Option<&str>, form_token: &str) -> bool {
    match cookie_token {
        Some(cookie) => !cookie.is_empty() && !form_token.is_empty() && cookie == form_token,
        None => false,
    }
}
