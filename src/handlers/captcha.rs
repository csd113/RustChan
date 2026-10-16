use crate::error::{AppError, Result};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(in crate::server) struct CaptchaImageQuery {
    board: String,
}

pub(in crate::server) async fn serve_captcha_image(
    State(state): State<crate::middleware::AppState>,
    Path(captcha_id): Path<String>,
    Query(query): Query<CaptchaImageQuery>,
) -> Result<Response> {
    let permit = state.captcha_work_gate.try_begin()?;
    let png = tokio::task::spawn_blocking(move || {
        let _captcha_permit = permit;
        crate::captcha::generate_captcha_image(&query.board, &captcha_id)
    })
    .await
    .map_err(|error| AppError::Internal(error.into()))?
    .map_err(|error| match error {
        crate::captcha::CaptchaImageError::InvalidRequest => {
            AppError::BadRequest("Invalid CAPTCHA request.".to_owned())
        }
        crate::captcha::CaptchaImageError::GenerationFailed => {
            AppError::Internal(anyhow::anyhow!("failed to generate captcha image"))
        }
        crate::captcha::CaptchaImageError::Busy => AppError::DbBusy,
    })?;

    let mut headers = HeaderMap::new();
    let _previous_value =
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    drop(headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, no-cache, max-age=0"),
    ));
    drop(headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache")));
    drop(headers.insert(header::EXPIRES, HeaderValue::from_static("0")));

    Ok((StatusCode::OK, headers, png).into_response())
}
