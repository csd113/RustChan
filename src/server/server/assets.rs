// src/server/server/assets.rs

use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};

static STYLE_CSS: &str = include_str!("../../../static/style.css");
static MAIN_JS: &str = include_str!("../../../static/main.js");
static ADMIN_CSS: &str = include_str!("../../../static/admin.css");
static ADMIN_JS: &str = include_str!("../../../static/admin.js");
static THEME_INIT_JS: &str = include_str!("../../../static/theme-init.js");

fn static_asset_response(body: &'static str, content_type: &'static str) -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
}

pub(super) async fn serve_css() -> impl IntoResponse {
    static_asset_response(STYLE_CSS, "text/css; charset=utf-8")
}

pub(super) async fn serve_main_js() -> impl IntoResponse {
    static_asset_response(MAIN_JS, "application/javascript; charset=utf-8")
}

pub(super) async fn serve_admin_css() -> impl IntoResponse {
    static_asset_response(ADMIN_CSS, "text/css; charset=utf-8")
}

pub(super) async fn serve_admin_js() -> impl IntoResponse {
    static_asset_response(ADMIN_JS, "application/javascript; charset=utf-8")
}

pub(super) async fn serve_theme_init_js() -> impl IntoResponse {
    static_asset_response(THEME_INIT_JS, "application/javascript; charset=utf-8")
}
