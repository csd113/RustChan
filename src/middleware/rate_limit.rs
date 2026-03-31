// src/middleware/rate_limit.rs

use crate::config::CONFIG;
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use super::ip::extract_ip;

static RATE_TABLE: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);
static LAST_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[allow(clippy::arithmetic_side_effects)]
pub async fn rate_limit_middleware(req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::GET {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if path.starts_with("/static/")
        || path.starts_with("/boards/")
        || path.starts_with("/admin")
        || path.starts_with("/api/")
    {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    let ip_key = {
        use sha2::{Digest, Sha256};
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
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::response::Html(rate_limited_toast_page()),
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

fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

fn rate_limited_toast_page() -> String {
    let forum_name_escaped = html_escape(&crate::config::CONFIG.forum_name);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Slow down — {forum_name_escaped}</title>
<style>
  body {{
    margin: 0;
    background: #1a1a1a;
    font-family: sans-serif;
  }}
  .toast {{
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    background: #2a2a2a;
    border: 2px solid #c00;
    border-radius: 8px;
    padding: 28px 36px;
    text-align: center;
    color: #eee;
    box-shadow: 0 4px 24px rgba(0,0,0,.7);
    max-width: 380px;
    width: 90vw;
    z-index: 9999;
  }}
  .toast h2 {{
    margin: 0 0 12px;
    font-size: 1.2rem;
    color: #f55;
  }}
  .toast p {{
    margin: 0 0 18px;
    font-size: 0.95rem;
    color: #ccc;
  }}
  .toast .bar {{
    height: 4px;
    background: #c00;
    border-radius: 2px;
    animation: shrink 3s linear forwards;
  }}
  @keyframes shrink {{
    from {{ width: 100%; }}
    to   {{ width: 0%; }}
  }}
</style>
</head>
<body data-rate-limit-page="1">
<div class="toast">
  <h2>&#9888; Slow down</h2>
  <p>You are navigating too fast.<br>Taking you back in a moment…</p>
  <div class="bar"></div>
</div>
<script src="/static/main.js" defer></script>
</body>
</html>"#
    )
}
