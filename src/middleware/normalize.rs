use axum::{
    extract::Request,
    http::Uri,
    middleware::Next,
    response::{IntoResponse as _, Response},
};

/// Redirects non-root paths with trailing slashes to their canonical form.
pub async fn normalize_trailing_slash(req: Request, next: Next) -> Response {
    let uri = req.uri();
    let path = uri.path();

    if path.len() > 1 && path.ends_with('/') {
        let stripped = path.trim_end_matches('/');
        // A path of only slashes strips to nothing; collapse it to `/` so the
        // Location header can never be empty (which browsers resolve back to
        // the request URL and loop).
        let canonical_path = if stripped.is_empty() { "/" } else { stripped };
        let new_path_and_query = uri.query().map_or_else(
            || canonical_path.to_owned(),
            |query| format!("{canonical_path}?{query}"),
        );

        if new_path_and_query.parse::<Uri>().is_ok() {
            return (
                axum::http::StatusCode::PERMANENT_REDIRECT,
                [(axum::http::header::LOCATION, new_path_and_query)],
            )
                .into_response();
        }
    }

    next.run(req).await
}

#[cfg(test)]
/// Trailing-slash canonicalization regression tests.
mod tests {
    use super::normalize_trailing_slash;
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    /// Send one request through a router wrapped in the normalization layer.
    async fn response_for(uri: &str) -> anyhow::Result<axum::response::Response> {
        let app = Router::new()
            .route("/", get(|| async { "root" }))
            .route("/board", get(|| async { "board" }))
            .layer(axum::middleware::from_fn(normalize_trailing_slash));
        let request = Request::builder().uri(uri).body(Body::empty())?;
        Ok(app.oneshot(request).await?)
    }

    #[tokio::test]
    /// Canonicalizes a trailing slash while preserving the query string.
    async fn redirects_trailing_slash_to_canonical_path() -> anyhow::Result<()> {
        let response = response_for("/board/?q=a%2Fb").await?;
        anyhow::ensure!(
            response.status() == StatusCode::PERMANENT_REDIRECT,
            "a trailing slash must redirect"
        );
        anyhow::ensure!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                == Some("/board?q=a%2Fb"),
            "trailing-slash redirect must preserve the query string"
        );
        Ok(())
    }

    #[tokio::test]
    /// Leaves the root path and canonical paths untouched.
    async fn serves_root_and_canonical_paths_directly() -> anyhow::Result<()> {
        for uri in ["/", "/board"] {
            let response = response_for(uri).await?;
            anyhow::ensure!(
                response.status() == StatusCode::OK,
                "{uri} must not be redirected"
            );
        }
        Ok(())
    }

    #[tokio::test]
    /// Never emits an empty Location for an all-slash path.
    async fn collapses_all_slash_paths_to_root() -> anyhow::Result<()> {
        let response = response_for("//").await?;
        anyhow::ensure!(
            response.status() == StatusCode::PERMANENT_REDIRECT,
            "an all-slash path must redirect"
        );
        anyhow::ensure!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                == Some("/"),
            "an all-slash path must redirect to the root, not an empty Location"
        );
        Ok(())
    }
}
