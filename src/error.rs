// error.rs — Unified error type.
// Every handler returns Result<T, AppError>. AppError converts to an HTTP
// response automatically, so handlers never need to manually build error pages.

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use tracing::error;

#[derive(Debug)]
pub enum AppError {
    /// 404 — board or thread not found
    NotFound(String),
    /// 400 — bad input from user
    BadRequest(String),
    /// 403 — forbidden (banned, CSRF failure, etc.)
    Forbidden(String),
    /// 429 — rate limited
    #[allow(dead_code)]
    RateLimited,
    /// 500 — internal error (database failure, IO error, etc.)
    Internal(anyhow::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::NotFound(msg) => write!(f, "Not found: {}", msg),
            AppError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            AppError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            AppError::RateLimited => write!(f, "Rate limited"),
            AppError::Internal(e) => write!(f, "Internal error: {}", e),
        }
    }
}

// Allow ? operator on anyhow::Error
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

// Allow ? operator on rusqlite::Error
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(anyhow::Error::new(e))
    }
}

// Allow ? operator on r2d2::Error
impl From<r2d2::Error> for AppError {
    fn from(e: r2d2::Error) -> Self {
        AppError::Internal(anyhow::Error::new(e))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "You are posting too fast. Slow down.".to_string(),
            ),
            AppError::Internal(e) => {
                error!("Internal error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal error occurred.".to_string(),
                )
            }
        };

        // Render a richer ban page with an appeal form instead of the generic error page
        if status == StatusCode::FORBIDDEN && message.starts_with("You are banned") {
            let reason = message
                .strip_prefix("You are banned. Reason: ")
                .unwrap_or(&message);
            let html = crate::templates::ban_page(reason);
            return (status, Html(html)).into_response();
        }

        let html = crate::templates::error_page(status.as_u16(), &message);
        (status, Html(html)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
