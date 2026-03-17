// chan_net/refresh.rs — Federation outgoing handler.
// Fully implemented in Phase 4 (Step 4.1).
//
// POST /chan/refresh builds a full snapshot and pushes it to RustWave
// /broadcast/transmit as multipart. Holds the shared HTTP_CLIENT static
// (LazyLock<reqwest::Client>) reused by poll.rs.
//
// Phase 8 fix: all AppError::Internal calls in this file previously passed
// String values (via .to_string() or format!()) to AppError::Internal, which
// takes anyhow::Error. These have been corrected to use anyhow::anyhow!() or
// direct ? propagation where a From impl exists.

use crate::{config::CONFIG, error::AppError, middleware::AppState};
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;
use std::sync::LazyLock;

/// Shared reqwest client — initialised once, reused for all outgoing calls
/// (refresh + poll). The 30-second timeout covers slow `RustWave` responses
/// during high-load broadcast operations.
#[allow(clippy::expect_used)] // LazyLock static init — no error-propagation path available
pub static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build reqwest client")
});

/// POST /chan/refresh
///
/// Builds a full in-memory snapshot ZIP of all boards and active posts, then
/// pushes it to `RustWave`'s `/broadcast/transmit` endpoint as a multipart POST.
///
/// On success, returns both the local snapshot `tx_id` and the broadcast `tx_id`
/// echoed back by `RustWave`:
/// ```json
/// { "status": "ok", "local_tx_id": "...", "broadcast_tx_id": "..." }
/// ```
///
/// Returns `500 Internal Server Error` if the snapshot build fails, if
/// `RustWave` is unreachable, or if `RustWave` responds with a non-2xx status.
pub async fn chan_refresh(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    // ── Build snapshot on a blocking thread ──────────────────────────────
    // Use ? directly — AppError implements From<r2d2::Error>.
    let conn = state.db.get()?;

    let (zip_bytes, tx_id) =
        tokio::task::spawn_blocking(move || super::snapshot::build_snapshot(&conn))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))? // JoinError
            .map_err(AppError::Internal)?; // anyhow::Error from build_snapshot

    // ── Assemble multipart form ───────────────────────────────────────────
    let part = reqwest::multipart::Part::bytes(zip_bytes)
        .file_name("snapshot.zip")
        .mime_str("application/zip")
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let form = reqwest::multipart::Form::new().part("snapshot", part);

    // ── POST to RustWave ─────────────────────────────────────────────────
    let url = format!("{}/broadcast/transmit", CONFIG.rustwave_url);

    let resp = HTTP_CLIENT
        .post(&url)
        .multipart(form)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("RustWave unreachable: {e}")))?;

    if !resp.status().is_success() {
        return Err(
            AppError::Internal(anyhow::anyhow!("RustWave returned {}", resp.status())).into(),
        );
    }

    // ── Parse broadcast tx_id from RustWave response ─────────────────────
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let broadcast_tx_id = body
        .get("tx_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Json(json!({
        "status":          "ok",
        "local_tx_id":     tx_id.to_string(),
        "broadcast_tx_id": broadcast_tx_id,
    })))
}
