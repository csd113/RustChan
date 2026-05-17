// error.rs — Unified error type.
//
// Every handler returns Result<T, AppError>. AppError converts to an HTTP
// response automatically, so handlers never need to manually build error pages.
//
// Variants map 1-to-1 to HTTP status codes so the right code is always returned:
//   NotFound          → 404
//   BadRequest        → 400
//   Forbidden         → 403
//   UploadTooLarge    → 413  (Content Too Large)
//   InvalidMediaType  → 415  (Unsupported Media Type)
//   DbBusy            → 503  (Service Unavailable, with Retry-After)
//   Internal          → 500

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use thiserror::Error;
use tracing::error;

#[derive(Debug, Error)]
pub enum AppError {
    /// 404 — board or thread not found
    #[error("Not found: {0}")]
    NotFound(String),

    /// 400 — bad input from user
    #[error("Bad request: {0}")]
    BadRequest(String),

    /// 403 — forbidden (banned, CSRF failure, etc.)
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// 403 — user is banned; carries the ban reason and CSRF token so the
    /// appeal form can be rendered with a valid token.
    #[error("You are banned. Reason: {reason}")]
    BannedUser { reason: String, csrf_token: String },

    /// 413 — upload body too large
    #[error("Upload too large: {0}")]
    UploadTooLarge(String),

    /// 415 — MIME type not accepted
    #[error("Invalid media type: {0}")]
    InvalidMediaType(String),

    /// 409 — resource already exists or snapshot already imported
    #[error("Conflict: {0}")]
    Conflict(String),

    /// 503 — database write contention; client should retry
    #[error("Database busy — please retry")]
    DbBusy,

    /// 500 — internal error (database failure, IO error, etc.)
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),

    /// TLS initialisation or certificate error (startup-time only).
    #[error("TLS error: {0}")]
    Tls(String),
}

// Allow ? operator on rusqlite::Error — map SQLITE_BUSY to DbBusy (503) and
// everything else to Internal (500).
impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(fe, _) = &e {
            if fe.code == rusqlite::ErrorCode::DatabaseBusy
                || fe.code == rusqlite::ErrorCode::DatabaseLocked
            {
                return Self::DbBusy;
            }
        }
        Self::Internal(e.into())
    }
}

// Allow ? operator on r2d2::Error.
// Pool connection timeouts → DbBusy (503) so clients know to retry.
// Other pool errors (misconfiguration, driver failure) → Internal (500).
impl From<r2d2::Error> for AppError {
    fn from(e: r2d2::Error) -> Self {
        // r2d2 surfaces timeout as "timed out waiting for connection".
        // Match on the message rather than a private variant so this keeps
        // working across r2d2 minor versions.
        let msg = e.to_string();
        if msg.contains("timed out") || msg.contains("Timeout") {
            Self::DbBusy
        } else {
            Self::Internal(e.into())
        }
    }
}

// Allow ? operator on std::io::Error inside the TLS module.
// All IO errors at TLS startup are surfaced as Tls(msg) rather than Internal
// so they produce a clear message without a full anyhow backtrace.
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Tls(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            Self::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            Self::BannedUser { reason, csrf_token } => {
                let html = crate::templates::ban_page(reason, csrf_token);
                return (StatusCode::FORBIDDEN, Html(html)).into_response();
            }
            Self::UploadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg.clone()),
            Self::InvalidMediaType(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg.clone()),
            Self::DbBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "The server is temporarily busy. Please try again in a moment.".to_owned(),
            ),
            Self::Internal(e) => {
                error!("Internal error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal error occurred.".to_owned(),
                )
            }
            Self::Tls(msg) => {
                error!("TLS error: {msg}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "A TLS configuration error occurred.".to_owned(),
                )
            }
        };

        let html = crate::templates::error_page(status.as_u16(), &message);
        (status, Html(html)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::AppError;
    use axum::body::to_bytes;
    use axum::response::IntoResponse as _;

    #[test]
    fn tls_errors_hide_internal_messages_from_users() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let response = AppError::Tls("secret cert path".to_owned()).into_response();
        let body = runtime
            .block_on(to_bytes(response.into_body(), usize::MAX))
            .expect("body bytes");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");
        assert!(!body.contains("secret cert path"));
    }
}
