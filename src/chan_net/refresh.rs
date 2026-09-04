//! Federation outgoing handler.
//
// POST /chan/refresh builds a full snapshot and pushes it to RustWave
// /broadcast/transmit as multipart. Holds the shared HTTP_CLIENT static
// (LazyLock<reqwest::Client>) reused by poll.rs.

use crate::{config::CONFIG, error::AppError, middleware::AppState};
use axum::{extract::State, response::IntoResponse, Json};
use serde_json::json;
use std::sync::LazyLock;
use tokio_util::bytes::Bytes;

/// Maximum JSON response retained from the configured `RustWave` gateway.
const RUSTWAVE_JSON_RESPONSE_MAX_BYTES: usize = 64 * 1024;

/// Shared reqwest client — initialised once, reused for all outgoing calls
/// (refresh + poll). The 30-second timeout covers slow `RustWave` responses
/// during high-load broadcast operations.
pub static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let mut builder = reqwest::Client::builder();
    builder = builder.timeout(std::time::Duration::from_secs(30));
    if let Ok(client) = builder.build() {
        return client;
    }

    tracing::error!("Failed to build reqwest client with configured timeout; falling back");
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
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
///
/// # Errors
///
/// Returns a [`super::ChanError`] for database, snapshot, multipart, transport,
/// peer-status, or response-decoding failures.
pub async fn chan_refresh(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    let pool = state.db.clone();
    let (zip_bytes, tx_id) = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        super::snapshot::build_snapshot(&conn)
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))? // JoinError
    .map_err(AppError::from)?; // anyhow::Error from build_snapshot

    // Assemble multipart form
    let part = reqwest::multipart::Part::bytes(zip_bytes)
        .file_name("snapshot.zip")
        .mime_str("application/zip")
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let form = reqwest::multipart::Form::new().part("snapshot", part);

    // POST to RustWave
    let url = format!("{}/broadcast/transmit", CONFIG.rustwave_url);

    let mut resp = HTTP_CLIENT
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

    let response_bytes = read_response_body_limited(
        &mut resp,
        RUSTWAVE_JSON_RESPONSE_MAX_BYTES,
        "RustWave refresh response",
    )
    .await
    .map_err(AppError::Internal)?;
    drop(resp);
    let body: serde_json::Value = serde_json::from_slice(&response_bytes)
        .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?;

    let broadcast_tx_id = body
        .get("tx_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    Ok(Json(json!({
        "status":          "ok",
        "local_tx_id":     tx_id.to_string(),
        "broadcast_tx_id": broadcast_tx_id,
    })))
}

/// Streams a gateway response into a bounded buffer.
pub(super) async fn read_response_body_limited(
    response: &mut reqwest::Response,
    max_bytes: usize,
    context: &str,
) -> anyhow::Result<Bytes> {
    if let Some(declared) = response.content_length() {
        let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        if declared > max_u64 {
            anyhow::bail!(
                "{context} declares {declared} bytes, exceeding the {max_bytes}-byte limit"
            );
        }
    }

    let initial_capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(max_bytes);
    let mut body = Vec::with_capacity(initial_capacity);
    loop {
        let next_chunk = response
            .chunk()
            .await
            .map_err(|error| anyhow::anyhow!("Failed to read {context}: {error}"))?;
        let Some(chunk) = next_chunk else {
            break;
        };
        append_response_chunk_limited(&mut body, &chunk, max_bytes, context)?;
    }
    Ok(Bytes::from(body))
}

/// Appends one response chunk only when the complete body remains in budget.
fn append_response_chunk_limited(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_bytes: usize,
    context: &str,
) -> anyhow::Result<()> {
    let remaining = max_bytes.saturating_sub(body.len());
    if chunk.len() > remaining {
        anyhow::bail!("{context} exceeds the {max_bytes}-byte limit");
    }
    body.extend_from_slice(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::append_response_chunk_limited;
    use anyhow::Result;

    #[test]
    fn gateway_response_buffer_rejects_chunk_beyond_remaining_budget() -> Result<()> {
        let mut body = b"1234".to_vec();
        append_response_chunk_limited(&mut body, b"56", 6, "test response")?;
        let error = match append_response_chunk_limited(&mut body, b"7", 6, "test response") {
            Ok(()) => anyhow::bail!("a chunk beyond the body budget was accepted"),
            Err(error) => error,
        };

        anyhow::ensure!(body == b"123456", "rejected chunk changed retained body");
        anyhow::ensure!(
            error.to_string().contains("exceeds the 6-byte limit"),
            "unexpected response limit error: {error}"
        );
        Ok(())
    }
}
