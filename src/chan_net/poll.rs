// chan_net/poll.rs — Federation incoming handler.
// Fully implemented in Phase 5 (Step 5.1).
//
// POST /chan/poll drains RustWave's broadcast queue by fetching and importing
// up to MAX_POLL_ITERATIONS (50) snapshots per call. Skips AppError::Conflict
// errors (already-seen tx_ids) silently. Propagates all other errors.
//
// Queue-empty detection: if RustWave responds with Content-Type
// application/json and a body of {"status":"empty"}, the loop exits cleanly.
//
// Implementation note: the response body is always consumed via .bytes()
// before any JSON inspection. Calling .json() and then .bytes() on the same
// reqwest::Response would fail — the body stream is single-pass.
//
// Phase 8 fix: two AppError::Internal calls passed String values (via format!())
// instead of anyhow::Error. Fixed by using anyhow::anyhow!() in both cases.

use super::import::do_import;
use super::refresh::HTTP_CLIENT;
use crate::{config::CONFIG, error::AppError, middleware::AppState};
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;

const MAX_POLL_ITERATIONS: usize = 50;

/// POST /chan/poll
///
/// Drains `RustWave`'s incoming broadcast queue. Each iteration performs a GET
/// to `{rustwave_url}/broadcast/incoming`. The loop exits when:
///
/// - `RustWave` returns a non-2xx status (peer-side error or empty queue signal)
/// - `RustWave` returns `Content-Type: application/json` with `{"status":"empty"}`
/// - `MAX_POLL_ITERATIONS` snapshots have been fetched
/// - A non-Conflict import error is encountered (propagated to the caller)
///
/// Returns `{"imported": N}` where N is the total number of posts imported
/// across all snapshots in this drain cycle. Posts skipped by INSERT OR IGNORE
/// are not counted. Snapshots whose `tx_id` was already recorded in the `TxLedger`
/// contribute 0 to the count and are silently skipped.
pub async fn chan_poll(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    let url = format!("{}/broadcast/incoming", CONFIG.rustwave_url);
    let mut imported_count = 0usize;

    for _ in 0..MAX_POLL_ITERATIONS {
        // ── Fetch next item from the broadcast queue ─────────────────────
        let resp = HTTP_CLIENT
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("RustWave poll failed: {e}")))?;

        if !resp.status().is_success() {
            // Non-2xx: treat as end-of-queue or transient peer error.
            break;
        }

        // Capture Content-Type before consuming the body — header access is
        // not possible after calling .bytes().
        let content_type = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // ── Consume body exactly once ─────────────────────────────────────
        // IMPORTANT: reqwest::Response body is a single-pass stream. We read
        // the full body into `bytes` here and reuse that buffer for both the
        // JSON sentinel check and the ZIP import. Calling `.json()` before
        // `.bytes()` would leave the response in a moved / partially-read
        // state, making the subsequent `.bytes()` call fail.
        let bytes = resp.bytes().await.map_err(|e| {
            AppError::Internal(anyhow::anyhow!("Failed to read RustWave body: {e}"))
        })?;

        // ── Empty-queue sentinel check ────────────────────────────────────
        if content_type.contains("application/json") {
            // Parse without failing hard — a malformed sentinel is not a
            // fatal error; we fall through and let do_import reject the
            // non-ZIP bytes as a BadRequest.
            if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if body.get("status").and_then(|s| s.as_str()) == Some("empty") {
                    break;
                }
            }
            // JSON body that is NOT the sentinel: fall through. do_import
            // will reject it with AppError::BadRequest (not a valid ZIP).
        }

        // ── Import snapshot ───────────────────────────────────────────────
        match do_import(&state, bytes).await {
            Ok(count) => imported_count = imported_count.saturating_add(count),
            Err(AppError::Conflict(_)) => {
                // tx_id already in TxLedger — snapshot was seen before.
                // This is expected during normal operation; skip silently.
            }
            Err(e) => return Err(super::ChanError::from(e)),
        }
    }

    Ok(Json(json!({ "imported": imported_count })))
}
