use crate::error::{AppError, Result};
use axum::{
    extract::{Path, Query},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct CaptchaImageQuery {
    board: String,
}

pub(crate) async fn serve_captcha_image(
    Path(captcha_id): Path<String>,
    Query(query): Query<CaptchaImageQuery>,
) -> Result<Response> {
    let png =
        crate::captcha::generate_captcha_image(&query.board, &captcha_id).map_err(
            |err| match err {
                crate::captcha::CaptchaImageError::InvalidRequest => {
                    AppError::BadRequest("Invalid CAPTCHA request.".to_owned())
                }
                crate::captcha::CaptchaImageError::GenerationFailed => {
                    AppError::Internal(anyhow::anyhow!("failed to generate captcha image"))
                }
            },
        )?;

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, no-cache, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::EXPIRES, HeaderValue::from_static("0"));

    Ok((StatusCode::OK, headers, png).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{ensure, Context as _};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn captcha_image_route_returns_png_with_private_no_cache_headers() -> anyhow::Result<()> {
        const CAPTCHA_ROUTE: &str = concat!("/captcha/", "{id}");

        let id = "00000000000000000000000000000006";
        let app = Router::new().route(CAPTCHA_ROUTE, get(serve_captcha_image));

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/captcha/{id}?board=test"))
                    .body(Body::empty())
                    .context("build CAPTCHA image request")?,
            )
            .await
            .context("receive CAPTCHA image response")?;

        ensure!(
            response.status() == StatusCode::OK,
            "CAPTCHA image route returned {}",
            response.status()
        );
        ensure!(
            response.headers().get(header::CONTENT_TYPE)
                == Some(&HeaderValue::from_static("image/png")),
            "CAPTCHA image response omitted its PNG content type"
        );
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .context("CAPTCHA image response omitted valid cache-control")?;
        ensure!(
            cache_control.contains("private"),
            "CAPTCHA response cache-control was not private"
        );
        ensure!(
            cache_control.contains("no-store"),
            "CAPTCHA response cache-control allowed storage"
        );
        ensure!(
            crate::captcha::testing::challenge_exists_for_test(id),
            "serving the image removed the CAPTCHA challenge"
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read CAPTCHA image response body")?;
        ensure!(
            body.starts_with(b"\x89PNG\r\n\x1a\n"),
            "CAPTCHA image body did not start with the PNG signature"
        );
        Ok(())
    }
}
