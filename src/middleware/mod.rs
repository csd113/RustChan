// middleware/mod.rs
//
// Two middleware systems:
//
// Rate Limiter — In-memory sliding window per IP address.
//   Applies ONLY to navigational GET requests (board index, catalog, thread,
//   archive, search).  The following are unconditionally excluded and never
//   counted against the limit:
//     • /static/*  — CSS/JS fetched automatically on every page load.
//     • /boards/*  — media thumbnails; one request per attachment per page.
//     • /admin*    — admin panel; operators must never be throttled.
//     • /api/*     — quote-hover preview calls; fired on every hover.
//     • Any request carrying a chan_admin_session cookie.
//   The GET limit is purely an anti-scraping / catalog-DoS safeguard.
//
//   POST rate limiting does NOT exist at the middleware level.
//   The ONLY post cooldown mechanism is the per-board post_cooldown_secs
//   setting (stored in the boards table, configurable via the admin panel).
//   When post_cooldown_secs = 0 on a board, there is zero cooldown for
//   posting on that board — no global override, no hidden limit.
//   Admins bypass the per-board cooldown entirely regardless of the value.
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
    http::Uri,
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Global rate limit table: `ip_key` → (`request_count`, `window_start_secs`)
static RATE_TABLE: LazyLock<DashMap<String, (u32, u64)>> = LazyLock::new(DashMap::new);

/// FIX[MEDIUM-4]: Track the last time we ran a full cleanup so we can also
/// clean on a time basis, not just when the table exceeds a size threshold.
static LAST_CLEANUP_SECS: AtomicU64 = AtomicU64::new(0);

// ─── Backup progress tracking ─────────────────────────────────────────────────
//
// A single global progress counter is sufficient because only one admin can
// run a backup at a time, and backups are serialised through spawn_blocking.
// Counter fields (files_done/total, bytes_done/total) use Ordering::Relaxed —
// the JS poller only needs eventual consistency for those.
// The `phase` field uses Release/Acquire so that counter zeroes written in
// `reset()` are guaranteed visible to any reader that subsequently loads the
// new phase value.

/// Phase codes stored in `BackupProgress::phase`.
pub mod backup_phase {
    pub const IDLE: u64 = 0;
    pub const SNAPSHOT_DB: u64 = 1;
    pub const COUNT_FILES: u64 = 2;
    pub const COMPRESS: u64 = 3;
    #[allow(dead_code)]
    pub const SAVE: u64 = 4;
    pub const DONE: u64 = 5;
}

/// Shared atomic progress state for backup operations.
/// Stored as Arc<BackupProgress> in `AppState` so admin handlers and the
/// progress endpoint can both access it without locking.
pub struct BackupProgress {
    pub phase: std::sync::atomic::AtomicU64,
    pub files_done: std::sync::atomic::AtomicU64,
    pub files_total: std::sync::atomic::AtomicU64,
    pub bytes_done: std::sync::atomic::AtomicU64,
    pub bytes_total: std::sync::atomic::AtomicU64,
}

impl BackupProgress {
    pub const fn new() -> Self {
        use std::sync::atomic::AtomicU64;
        Self {
            phase: AtomicU64::new(backup_phase::IDLE),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
        }
    }

    /// Reset all counters and set a new phase.
    ///
    /// FIX[ORANGE-3]: The previous implementation stored the new phase with
    /// `Relaxed`, giving no ordering guarantee between the counter-zero stores
    /// [1-4] and the phase store [5].  A JS poller that loaded the new phase
    /// could then read stale counter values from the previous run.
    /// The phase store now uses `Release` so all preceding Relaxed stores are
    /// guaranteed to be visible to any reader that loads `phase` with `Acquire`.
    pub fn reset(&self, phase: u64) {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        self.files_done.store(0, Relaxed);
        self.files_total.store(0, Relaxed);
        self.bytes_done.store(0, Relaxed);
        self.bytes_total.store(0, Relaxed);
        self.phase.store(phase, Release); // Release: makes counter zeroes visible before new phase
    }
}

/// Shared state for extracting the DB pool in middleware
#[derive(Clone)]
pub struct AppState {
    pub db: crate::db::DbPool,
    /// True when ffmpeg was detected at startup (set by `detect::detect_ffmpeg`).
    /// Passed to file handling to enable/disable video thumbnail generation.
    pub ffmpeg_available: bool,
    /// True when ffmpeg was detected AND the libwebp encoder is compiled in.
    /// Controls image→WebP conversion; independent of video/audio capabilities.
    pub ffmpeg_webp_available: bool,
    /// Background job queue — enqueue CPU-heavy work here instead of blocking
    /// the HTTP request path.
    pub job_queue: std::sync::Arc<crate::workers::JobQueue>,
    /// Live backup progress counters.  Polled by GET /admin/backup/progress.
    pub backup_progress: std::sync::Arc<BackupProgress>,
}

/// Get current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Rate limit middleware — applied only to navigational GET requests.
///
/// POST rate limiting is intentionally handled inside the individual posting
/// handlers (`create_thread`, `post_reply`) so that rate-limit errors can be
/// rendered inline on the board/thread page rather than redirecting the user
/// to a standalone 429 error page.  Admins are exempt at the handler level too.
///
/// For GET requests, three categories are unconditionally excluded from the
/// counter so that normal browsing never trips the limit:
///
///   • /static/*  — CSS, JS, theme-init.js; fetched automatically per page.
///   • /boards/*  — media files and thumbnails; one per attachment on the page.
///   • /admin/*   — admin panel routes; should never be throttled for operators.
///
/// Only "navigational" page routes (board index, catalog, thread, archive,
/// search, …) are counted.  The limit is intended to mitigate automated scraping
/// of catalog/search endpoints, not to interfere with legitimate browsing.
///
/// When the limit IS hit, the response is a lightweight HTML page that
/// shows an in-page toast notification and then navigates the browser back
/// to the previous page — matching the "inline" behaviour of the POST
/// cooldown errors rather than stranding the user on a bare error page.
#[allow(clippy::arithmetic_side_effects)]
pub async fn rate_limit_middleware(req: Request, next: Next) -> Response {
    // Only rate-limit GET; skip POST and all other methods entirely.
    if req.method() != axum::http::Method::GET {
        return next.run(req).await;
    }

    // Skip static assets, media files, admin routes, and API endpoints —
    // these must never be blocked by the GET rate limiter.
    // /static/*  — CSS, JS fetched automatically on every page load.
    // /boards/*  — thumbnails and media files, one per attachment per page.
    // /admin*    — admin panel; operators must never be throttled here.
    // /api/*     — post-hover preview calls, fired on every quote-link hover.
    let path = req.uri().path();
    if path.starts_with("/static/")
        || path.starts_with("/boards/")
        || path.starts_with("/admin")
        || path.starts_with("/api/")
    {
        return next.run(req).await;
    }

    // Skip if the request carries a valid-looking admin session cookie.
    // We only check presence here (no DB round-trip in middleware); the actual
    // session validation happens inside admin handlers.  This is sufficient
    // for rate-limiting purposes since the cookie is HttpOnly+SameSite=Strict.
    //
    // FIX[RED-1]: The previous bare `.contains("chan_admin_session=")` matched
    // any substring of the raw Cookie header, allowing two trivial bypasses:
    //   • `Cookie: x=chan_admin_session=forged`  (value embeds the string)
    //   • `Cookie: xchan_admin_session=anything` (name is a prefix)
    // We now split on ';', trim each pair, and require the segment to *start*
    // with exactly "chan_admin_session=" — an exact cookie-name match.
    let has_admin_cookie = req
        .headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .any(|pair| pair.trim().starts_with("chan_admin_session="))
        });
    if has_admin_cookie {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    // Hash the IP so raw addresses are never kept in process memory.
    let ip_key = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(ip.as_bytes());
        h.update(b"G");
        hex::encode(h.finalize())
    };
    let now = now_secs();
    let window = CONFIG.rate_limit_window;
    let limit = CONFIG.rate_limit_gets;

    // Check and update rate limit counter
    let blocked = {
        let mut binding = RATE_TABLE.entry(ip_key.clone()).or_insert((0, now));
        let (count, window_start) = binding.value_mut();
        let result = if now.saturating_sub(*window_start) > window {
            // Window has expired, reset
            *count = 1;
            *window_start = now;
            false
        } else {
            *count += 1;
            *count > limit
        };
        drop(binding);
        result
    };

    if blocked {
        // Return an in-page toast rather than a bare 429 error page.
        // The script shows a visible notification then navigates back so the
        // user stays in context instead of landing on a dead-end error screen.
        let html = rate_limited_toast_page();
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::response::Html(html),
        )
            .into_response();
    }

    // FIX[MEDIUM-4]: Clean old entries when the table grows large OR at least
    // once every 10 minutes (600 seconds), whichever comes first.
    let last_cleanup = LAST_CLEANUP_SECS.load(Ordering::Relaxed);
    let should_clean = RATE_TABLE.len() > 5000 || now.saturating_sub(last_cleanup) > 600;

    if should_clean {
        // Use compare_exchange to avoid concurrent threads all cleaning simultaneously
        if LAST_CLEANUP_SECS
            .compare_exchange(last_cleanup, now, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            RATE_TABLE
                .retain(|_, (_, window_start)| now.saturating_sub(*window_start) <= window * 2);
        }
    }

    next.run(req).await
}

/// Escape the five characters that are special in HTML text/attribute contexts.
/// Keeps config-sourced strings safe for interpolation into HTML output.
///
/// FIX[YELLOW-4]: `CONFIG.forum_name` is operator-supplied and could contain
/// `<`, `>`, `&`, `"`, or `'`.  Interpolating it raw produces malformed HTML
/// and, if the value ever reaches a visible element, a direct XSS vector.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Build a lightweight HTML page that shows an in-page toast notification and
/// then navigates the browser back to where it came from.
///
/// This is returned instead of the bare `AppError::RateLimited` 429 page so
/// that the user stays in context (they see the message overlaid on what looks
/// like their previous page) rather than landing on a dead-end error screen.
fn rate_limited_toast_page() -> String {
    // FIX[YELLOW-4]: escape before interpolation so special chars in the forum
    // name never produce malformed HTML (or a future XSS if copied elsewhere).
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
<body>
<div class="toast">
  <h2>&#9888; Slow down</h2>
  <p>You are navigating too fast.<br>Taking you back in a moment…</p>
  <div class="bar"></div>
</div>
<script>
  // Go back as soon as the animation finishes (3 s).
  setTimeout(function () {{
    if (document.referrer) {{
      window.location.href = document.referrer;
    }} else {{
      window.history.back();
    }}
  }}, 3000);
</script>
</body>
</html>"#
    )
}

/// Extract client IP, respecting proxy headers when configured.
///
/// FIX[HIGH-1]: When `behind_proxy=true`, we now prefer X-Real-IP (set by nginx
/// to `$remote_addr` — the actual TCP peer — and not modifiable by the client).
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
        .map_or_else(|| "unknown".to_string(), |ci| ci.0.ip().to_string())
}

/// Validate CSRF token from a form against the cookie.
/// Returns true if valid, false if CSRF check fails.
///
/// CRIT-7: Uses constant-time comparison to prevent timing side-channel attacks
/// that could leak token prefix information through response latency differences.
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

/// Constant-time byte-slice equality comparison.
/// Always visits every byte so the runtime does not depend on where the
/// strings first differ, closing the CRIT-7 timing side-channel.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Length check is not secret information — it is fine to branch here.
    if a.len() != b.len() {
        return false;
    }
    // XOR all byte pairs and OR the results; any mismatch sets a bit in `diff`.
    let diff: u8 = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

/// Trailing-slash normalization middleware.
///
/// Strips a trailing `/` from every path except the root `/` and issues a
/// 308 Permanent Redirect.  This makes routes like
///   /{board}/catalog/  →  /{board}/catalog
///   /{board}/thread/5/ →  /{board}/thread/5
///   /{board}/          →  /{board}
/// work correctly without 404s, regardless of whether the user typed the
/// slash, a browser added it, or an old bookmark included it.
///
/// FIX[ORANGE-2]: The previous `Redirect::permanent` issued a 301, which
/// permits user-agents to reissue the request as GET (RFC 7231 §6.4.2 —
/// and every major browser does).  A POST to a trailing-slash URL would
/// silently drop the form body.  We now use 308 Permanent Redirect
/// (RFC 7538), which explicitly mandates that the method and body are
/// preserved on redirect.
///
/// Query strings are preserved across the redirect.
pub async fn normalize_trailing_slash(req: Request, next: Next) -> Response {
    let uri = req.uri();
    let path = uri.path();

    // Only act on paths that have a trailing slash and are not just "/".
    if path.len() > 1 && path.ends_with('/') {
        let stripped = path.trim_end_matches('/');

        // Rebuild the URI, preserving any query string.
        let new_path_and_query = uri
            .query()
            .map_or_else(|| stripped.to_string(), |q| format!("{stripped}?{q}"));

        // Validate the rebuilt path before redirecting.
        if new_path_and_query.parse::<Uri>().is_ok() {
            // 308 Permanent Redirect: method + body are preserved (RFC 7538).
            return (
                axum::http::StatusCode::PERMANENT_REDIRECT,
                [(axum::http::header::LOCATION, new_path_and_query)],
            )
                .into_response();
        }

        // If URI reconstruction failed for any reason, fall through and let
        // the router handle the original request normally.
    }

    next.run(req).await
}

/// Proxy-aware client IP extractor for use in Axum handler signatures.
///
/// CRIT-2: Replaces direct use of `ConnectInfo<SocketAddr>` in post handlers.
/// When `CHAN_BEHIND_PROXY=true` this reads X-Real-IP (set by nginx to the
/// real TCP peer) rather than the raw socket address (which would always be
/// the proxy's IP, making IP bans and rate-limits ineffective).
pub struct ClientIp(pub String);

impl<S> axum::extract::FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        if CONFIG.behind_proxy {
            // Prefer X-Real-IP (nginx $remote_addr — not client-controlled).
            if let Some(val) = parts
                .headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Ok(Self(val.to_string()));
            }
            // Fall back to rightmost X-Forwarded-For entry (added by the
            // trusted proxy, not by the client).
            if let Some(val) = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next_back())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Ok(Self(val.to_string()));
            }
        }
        // Direct connection (or proxy headers absent).
        let ip = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map_or_else(|| "unknown".to_string(), |ci| ci.0.ip().to_string());
        Ok(Self(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_csrf ────────────────────────────────────────────────────────

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
    fn csrf_both_empty_fails() {
        assert!(!validate_csrf(Some(""), ""));
    }

    // ── constant_time_eq ─────────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_equal_slices() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_different_slices() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn constant_time_eq_empty_slices() {
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_one_byte_off() {
        // Differs only in the last byte — must still return false
        assert!(!constant_time_eq(b"token_a", b"token_b"));
    }

    // ── BackupProgress ───────────────────────────────────────────────────────

    #[test]
    fn backup_progress_initial_phase_is_idle() {
        use std::sync::atomic::Ordering::Acquire;
        let bp = BackupProgress::new();
        assert_eq!(bp.phase.load(Acquire), backup_phase::IDLE);
    }

    #[test]
    fn backup_progress_reset_clears_counters() {
        use std::sync::atomic::Ordering::{Acquire, Relaxed};
        let bp = BackupProgress::new();
        bp.files_done.store(10, Relaxed);
        bp.files_total.store(20, Relaxed);
        bp.bytes_done.store(1024, Relaxed);
        bp.bytes_total.store(2048, Relaxed);

        bp.reset(backup_phase::COMPRESS);

        assert_eq!(bp.phase.load(Acquire), backup_phase::COMPRESS);
        assert_eq!(bp.files_done.load(Relaxed), 0);
        assert_eq!(bp.files_total.load(Relaxed), 0);
        assert_eq!(bp.bytes_done.load(Relaxed), 0);
        assert_eq!(bp.bytes_total.load(Relaxed), 0);
    }
}
