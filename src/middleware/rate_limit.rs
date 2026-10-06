use crate::config::{RateLimitPolicy, CONFIG};
use axum::{
    extract::Request,
    http::Method,
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{ip::extract_ip, RateTable};

/// Browsing state is capped independently of action-specific request budgets.
static RATE_TABLE: LazyLock<RateTable> = LazyLock::new(RateTable::default);
/// Each key hashes a client and a fixed action class, never a board or request path.
static ACTION_TABLE: LazyLock<RateTable> = LazyLock::new(RateTable::default);

/// Cost-sensitive request classes, independent of board cooldown configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    /// Thread creation across all boards.
    Thread,
    /// Replies across all threads and boards, including sage.
    Reply,
    /// Reports and appeals, including failed or duplicate requests.
    Report,
    /// Full-text count/sort work.
    Search,
    /// Login, board unlock, and first-run password hashing.
    Password,
    /// CAPTCHA image generation.
    Captcha,
    /// Other state-changing requests.
    Mutation,
}

impl Action {
    /// Per-minute request allowance and domain-separated identity label.
    const fn policy(self) -> (&'static str, u32) {
        match self {
            Self::Thread => ("thread", 6),
            Self::Reply => ("reply", 30),
            Self::Report => ("report", 30),
            Self::Search => ("search", 20),
            Self::Password => ("password", 20),
            Self::Captcha => ("captcha", 20),
            Self::Mutation => ("mutation", 120),
        }
    }
}

/// Classify route shapes without creating state for attacker-controlled path values.
fn request_action(method: &Method, path: &str) -> Option<Action> {
    let mut parts = path.trim_matches('/').split('/');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    let third = parts.next();
    let fourth = parts.next();
    let no_more = parts.next().is_none();
    if *method == Method::GET || *method == Method::HEAD {
        return if second == Some("search") && third.is_none() {
            Some(Action::Search)
        } else if first == "captcha" && second.is_some() && third.is_none() {
            Some(Action::Captcha)
        } else {
            None
        };
    }
    if *method != Method::POST {
        return None;
    }
    Some(
        if matches!(first, "report" | "appeal") && second.is_none() {
            Action::Report
        } else if matches!(first, "vote" | "preferences") && second.is_none() {
            Action::Mutation
        } else if path == "/admin/login"
            || path.starts_with("/setup/")
            || (second == Some("unlock") && third.is_none())
        {
            Action::Password
        } else if (!first.is_empty()
            && first.len() <= 8
            && first.bytes().all(|byte| byte.is_ascii_alphanumeric()))
            && second.is_none()
        {
            Action::Thread
        } else if second == Some("thread") && third.is_some() && fourth.is_none() && no_more {
            Action::Reply
        } else {
            Action::Mutation
        },
    )
}

/// Hash one client/action key without retaining raw addresses or request values.
fn budget_key(ip: &str, action: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(ip.as_bytes());
    hasher.update(b":");
    hasher.update(action.as_bytes());
    hex::encode(hasher.finalize())
}

/// Cheap retryable rejection; denied requests never allocate a form or query the DB.
fn action_limit_response(retry_after: u64) -> Response {
    let mut response = (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        "Too many requests for this action. Please wait and try again.",
    )
        .into_response();
    if let Ok(value) = axum::http::HeaderValue::from_str(&retry_after.to_string()) {
        drop(
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value),
        );
    }
    drop(response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    ));
    response
}

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

/// Apply the configured per-visitor browsing request limit.
pub async fn rate_limit_middleware(req: Request, next: Next) -> Response {
    let ip = extract_ip(&req);
    let now = now_secs();
    if let Some(action) = request_action(req.method(), req.uri().path()) {
        let (label, allowance) = action.policy();
        let key = budget_key(&ip, label);
        if ACTION_TABLE.record(&key, now, 60) > allowance {
            return action_limit_response(
                ACTION_TABLE.retry_after(&key, now, allowance).unwrap_or(60),
            );
        }
    }
    if !counts_request(req.method(), req.uri().path(), CONFIG.rate_limit_policy) {
        return next.run(req).await;
    }
    let ip_key = budget_key(&ip, "browse");
    let blocked =
        RATE_TABLE.record(&ip_key, now, CONFIG.rate_limit_window) > CONFIG.rate_limit_gets;

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

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::{counts_request, request_action, Action, Method, RateLimitPolicy};

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
    fn action_budgets_cover_all_posting_and_expensive_entry_points() {
        for (method, path, expected) in [
            (Method::POST, "/b", Action::Thread),
            (Method::POST, "/tech/", Action::Thread),
            (Method::POST, "/b/thread/1", Action::Reply),
            (Method::POST, "/b/thread/999999999999999999", Action::Reply),
            (Method::POST, "/report", Action::Report),
            (Method::POST, "/appeal", Action::Report),
            (Method::GET, "/b/search", Action::Search),
            (Method::HEAD, "/b/search", Action::Search),
            (Method::GET, "/captcha/id", Action::Captcha),
            (Method::POST, "/admin/login", Action::Password),
            (Method::POST, "/b/unlock", Action::Password),
            (Method::POST, "/setup/review", Action::Password),
            (Method::POST, "/vote", Action::Mutation),
        ] {
            assert_eq!(
                request_action(&method, path),
                Some(expected),
                "{method} {path}"
            );
        }
        assert_eq!(request_action(&Method::GET, "/b/thread/1/updates"), None);
        assert_eq!(request_action(&Method::GET, "/boards/b/test.webp"), None);
    }
}
