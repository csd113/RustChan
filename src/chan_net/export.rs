// chan_net/export.rs — Federation export handler.
//
// Step 2.4
//
// POST /chan/export builds a full snapshot of all boards and active
// (non-archived) threads via snapshot::build_snapshot and returns the ZIP
// bytes with Content-Type: application/zip.

use crate::{error::AppError, middleware::AppState};
use axum::{extract::State, http::header, response::IntoResponse};

pub async fn chan_export(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    let conn = state
        .db
        .get()
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let (zip_bytes, _tx_id) =
        tokio::task::spawn_blocking(move || super::snapshot::build_snapshot(&conn))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    Ok((
        axum::http::StatusCode::OK,
        [(header::CONTENT_TYPE, "application/zip")],
        zip_bytes,
    ))
}
