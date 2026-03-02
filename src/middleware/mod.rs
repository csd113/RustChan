// middleware/mod.rs
//
// Two middleware systems:
//
// Rate Limiter — In-memory sliding window per IP address.
//   • Uses DashMap (lock-free concurrent HashMap) to track (count, window_start).
//   • On each POST request, we check if IP has exceeded CONFIG.rate_limit_posts
//     within the last CONFIG.rate_limit_window seconds.
//   • Memory: ~200 bytes per IP entry. 10,000 concurrent IPs = ~2 MiB. Fine for Pi.
//   • Resets on restart (acceptable for LAN; no persistent attack state).
//
// CSRF Protection — Double-submit cookie pattern.
//   • On every page load (GET), we set a "csrf_token" cookie if absent.
//   • Every POST form includes a hidden "_csrf" field with the same token value.
//   • On POST, middleware verifies hidden field == cookie value.
//   • Since cookies are same-site, a cross-origin request can't read the cookie.
//   • Cookie is SameSite=Strict + Secure (when behind HTTPS proxy).

use crate::config::CONFIG;
use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use hex;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// Global rate limit table: ip_hash → (request_count, window_start_secs)
static RATE_TABLE: Lazy<DashMap<String, (u32, u64)>> = Lazy::new(DashMap::new);

/// Shared state for extracting the DB pool in middleware
#[derive(Clone)]
pub struct AppState {
    pub db: crate::db::DbPool,
}

/// Get current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Rate limit middleware — applied to POST routes.
/// Blocks requests from IPs that exceed the configured threshold.
pub async fn rate_limit_middleware(
    req: Request,
    next: Next,
) -> Response {
    // Only rate-limit POST requests (thread/reply creation)
    if req.method() != axum::http::Method::POST {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    // Hash the IP so raw addresses are never kept in process memory
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

        if now - *window_start > window {
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

    // Periodically clean old entries to prevent unbounded memory growth.
    // Simple heuristic: clean when table gets large.
    if RATE_TABLE.len() > 5000 {
        RATE_TABLE.retain(|_, (_, window_start)| now - *window_start <= window * 2);
    }

    next.run(req).await
}

/// Extract client IP, respecting X-Forwarded-For when behind proxy.
pub fn extract_ip(req: &Request) -> String {
    if CONFIG.behind_proxy {
        if let Some(fwd) = req.headers().get("x-forwarded-for") {
            if let Ok(val) = fwd.to_str() {
                // X-Forwarded-For may be a comma-separated list; take leftmost
                if let Some(ip) = val.split(',').next() {
                    return ip.trim().to_string();
                }
            }
        }
    }
    // Fall back to connection IP (from extensions set by axum)
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
