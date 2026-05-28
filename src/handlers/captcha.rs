use crate::error::{AppError, Result};
use axum::{
    extract::{Path, Query},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CaptchaImageQuery {
    board: String,
}

pub async fn serve_captcha_image(
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
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn captcha_image_route_returns_png_with_private_no_cache_headers() {
        const CAPTCHA_ROUTE: &str = concat!("/captcha/", "{id}");

        let id = "00000000000000000000000000000006";
        let app = Router::new().route(CAPTCHA_ROUTE, get(serve_captcha_image));

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/captcha/{id}?board=test"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("image/png"))
        );
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .expect("cache-control");
        assert!(cache_control.contains("private"));
        assert!(cache_control.contains("no-store"));
        assert!(crate::captcha::testing::challenge_exists_for_test(id));

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
