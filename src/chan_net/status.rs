// chan_net/status.rs — ChanNet health check handler.
// Step 6.1 — Fully implemented.
//
// GET /chan/status returns service name, version, board count, and post count
// as JSON. Used by operators and RustWave to verify connectivity.

use crate::{error::AppError, middleware::AppState};
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

pub async fn chan_status(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    let conn = state
        .db
        .get()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    let (boards, posts) = tokio::task::spawn_blocking(move || -> rusqlite::Result<(i64, i64)> {
        let boards: i64 = conn.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
        let posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;
        Ok((boards, posts))
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("spawn_blocking panic: {e}")))?
    .map_err(AppError::from)?;

    Ok(Json(json!({
        "service":  "chan-net",
        "chan_net": true,
        "version":  env!("CARGO_PKG_VERSION"),
        "boards":   boards,
        "posts":    posts,
    })))
}
