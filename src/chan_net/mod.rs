// chan_net/mod.rs — ChanNet API module root.
//
// Runs on a second TCP listener (default 127.0.0.1:7070), separate from the
// main forum port. Activated with the --chan-net CLI flag.
//
// Two independent layers:
//   Layer 1 — Federation sync  (Phases 1–6): node-to-node ZIP exchange
//   Layer 2 — RustWave gateway (Phase 7):    JSON command in, ZIP package out
//
// Rate-limit middleware is intentionally excluded — all traffic on this
// listener is machine-to-machine.
//
// Step 1.4

pub mod command;
pub mod export;
pub mod import;
pub mod poll;
pub mod refresh;
pub mod selective_snapshot;
pub mod snapshot;
pub mod status;

use crate::config::CONFIG;
use crate::error::AppError;
use crate::middleware::AppState;
use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

// ── ChanError ─────────────────────────────────────────────────────────────────
//
// All `/chan/*` routes are machine-to-machine. They must never return the HTML
// error pages that `AppError::into_response` renders for browser-facing routes.
// `ChanError` wraps `AppError` and overrides `IntoResponse` to emit JSON:
//
//   { "error": "<message>" }
//
// with the same HTTP status code that `AppError` would have produced.

/// JSON-rendering error type for all `/chan/*` handlers.
pub struct ChanError(pub AppError);

impl From<AppError> for ChanError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

// Forward the common conversions that handler code uses with `?`.
impl From<r2d2::Error> for ChanError {
    fn from(e: r2d2::Error) -> Self {
        Self(AppError::from(e))
    }
}

impl From<rusqlite::Error> for ChanError {
    fn from(e: rusqlite::Error) -> Self {
        Self(AppError::from(e))
    }
}

impl From<anyhow::Error> for ChanError {
    fn from(e: anyhow::Error) -> Self {
        Self(AppError::Internal(e))
    }
}

impl IntoResponse for ChanError {
    fn into_response(self) -> Response {
        let (status, message) = match self.0 {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            AppError::BannedUser { reason, .. } => (StatusCode::FORBIDDEN, reason),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AppError::UploadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg),
            AppError::InvalidMediaType(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg),
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "Posting too fast.".to_string(),
            ),
            AppError::DbBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Database busy — retry shortly.".to_string(),
            ),
            AppError::Internal(e) => {
                tracing::error!("ChanNet internal error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal error occurred.".to_string(),
                )
            }
            AppError::Api {
                status,
                detail,
                endpoint,
            } => {
                tracing::error!(
                    status,
                    endpoint = endpoint.as_deref().unwrap_or("unknown"),
                    "ChanNet API error: {detail}",
                );
                (
                    StatusCode::BAD_GATEWAY,
                    format!("API error {status}: {detail}"),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}

// ── Body-limit JSON middleware ─────────────────────────────────────────────────
//
// `DefaultBodyLimit` rejects oversized bodies before the handler runs, and its
// built-in rejection renders plain text (StatusCode 413, body:
// "Failed to buffer request body: …"). That bypasses our `ChanError` JSON
// rendering. This middleware sits *outside* the body-limit layer and
// intercepts any 413 response, replacing it with a proper JSON error body.

async fn json_body_limit_error(req: axum::http::Request<axum::body::Body>, next: Next) -> Response {
    let response = next.run(req).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "Request body too large" })),
        )
            .into_response();
    }
    response
}

/// Build the `ChanNet` router.
///
/// All `/chan/*` routes are wired here. `DefaultBodyLimit` is applied
/// per-route so that the tight 8 KiB JSON command limit does not
/// accidentally apply to the ZIP import route and vice-versa.
pub fn chan_router(state: AppState) -> Router {
    Router::new()
        // ── Status ──────────────────────────────────────────────────────────
        .route("/chan/status", get(status::chan_status))
        // ── RustWave gateway — raw JSON in, ZIP data package out ─────────────
        //
        // The json_body_limit_error middleware is applied *outside* the
        // DefaultBodyLimit layer so that 413 rejections are rendered as JSON
        // instead of the default plain-text "Failed to buffer request body".
        .route(
            "/chan/command",
            post(command::chan_command)
                .layer(DefaultBodyLimit::max(CONFIG.chan_net_command_max_body))
                .layer(middleware::from_fn(json_body_limit_error)),
        )
        // ── Federation sync — ZIP in, ZIP out ────────────────────────────────
        .route("/chan/export", post(export::chan_export))
        .route(
            "/chan/import",
            post(import::chan_import).layer(DefaultBodyLimit::max(CONFIG.chan_net_max_body)),
        )
        .route("/chan/refresh", post(refresh::chan_refresh))
        .route("/chan/poll", post(poll::chan_poll))
        .with_state(state)
}
