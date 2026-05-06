use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

fn favicon_content_type(file_name: &str) -> &'static str {
    match file_name {
        "favicon.ico" => "image/x-icon",
        _ => "image/png",
    }
}

async fn serve_named_global_favicon(
    file_name: &'static str,
    req: axum::extract::Request,
) -> Response {
    let Some(path) = crate::favicon::global_favicon_file(file_name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(path) =
        crate::utils::fs_security::canonical_child_of(&crate::favicon::global_favicon_dir(), &path)
            .and_then(|path| {
                crate::utils::fs_security::assert_regular_file_no_symlink(&path)?;
                Ok(path)
            })
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let has_version = req
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part.starts_with("v=")));
    let req = req.map(|_| axum::body::Body::empty());
    ServeFile::new(path).oneshot(req).await.map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(favicon_content_type(file_name)),
            );
            crate::cache::set_cache_control(
                resp.headers_mut(),
                cache_control_for_favicon(has_version),
            );
            resp.into_response()
        },
    )
}

const fn cache_control_for_favicon(has_version: bool) -> &'static str {
    if has_version {
        crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA
    } else {
        crate::cache::CACHE_CONTROL_STATIC_SHORT
    }
}

pub async fn serve_favicon_ico(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("favicon.ico", req).await
}

pub async fn serve_favicon_16(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("favicon-16x16.png", req).await
}

pub async fn serve_favicon_32(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("favicon-32x32.png", req).await
}

pub async fn serve_apple_touch_icon(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("apple-touch-icon.png", req).await
}

pub async fn serve_android_chrome_192(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("android-chrome-192x192.png", req).await
}

pub async fn serve_android_chrome_512(req: axum::extract::Request) -> Response {
    serve_named_global_favicon("android-chrome-512x512.png", req).await
}

#[cfg(test)]
mod tests {
    use super::cache_control_for_favicon;

    #[test]
    fn versioned_favicons_are_safe_to_cache_long_term() {
        assert_eq!(
            cache_control_for_favicon(true),
            crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA
        );
    }

    #[test]
    fn unversioned_favicons_must_revalidate() {
        assert_eq!(
            cache_control_for_favicon(false),
            crate::cache::CACHE_CONTROL_STATIC_SHORT
        );
    }
}
