use axum::{
    extract::Request,
    http::Uri,
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn normalize_trailing_slash(req: Request, next: Next) -> Response {
    let uri = req.uri();
    let path = uri.path();

    if path.len() > 1 && path.ends_with('/') {
        let stripped = path.trim_end_matches('/');
        let new_path_and_query = uri.query().map_or_else(
            || stripped.to_string(),
            |query| format!("{stripped}?{query}"),
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
