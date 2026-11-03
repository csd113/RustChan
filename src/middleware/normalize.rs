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
