use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};

static STYLE_CSS: &str = include_str!("../../../static/style.css");
static MAIN_JS: &str = include_str!("../../../static/main.js");
static THEME_INIT_JS: &str = include_str!("../../../static/theme-init.js");

pub(super) async fn serve_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        STYLE_CSS,
    )
}

pub(super) async fn serve_main_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        MAIN_JS,
    )
}

pub(super) async fn serve_theme_init_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        THEME_INIT_JS,
    )
}
