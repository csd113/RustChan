//! Federation export handler.
//
// POST /chan/export builds a full snapshot of all boards and active
// (non-archived) threads via snapshot::build_snapshot and returns the ZIP
// bytes with Content-Type: application/zip.

use crate::{error::AppError, middleware::AppState};
use axum::{extract::State, http::header, response::IntoResponse};

/// Builds and returns a full federation snapshot ZIP.
///
/// # Errors
///
/// Returns a [`super::ChanError`] when the database cannot be acquired, the
/// snapshot cannot be built, or the blocking task cannot be joined.
pub async fn chan_export(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, super::ChanError> {
    let conn = state.db.get()?;

    let (zip_bytes, _tx_id) =
        tokio::task::spawn_blocking(move || super::snapshot::build_snapshot(&conn))
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
            .map_err(AppError::from)?;

    Ok((
        axum::http::StatusCode::OK,
        [(header::CONTENT_TYPE, "application/zip")],
        zip_bytes,
    ))
}
