// src/server/server/assets.rs

use axum::{
    http::{header, HeaderValue, StatusCode},
    response::IntoResponse,
};

static STYLE_CSS: &str = include_str!("../../../static/style.css");
static MAIN_JS: &str = include_str!("../../../static/main.js");
static ADMIN_CSS: &str = include_str!("../../../static/admin.css");
static ADMIN_JS: &str = include_str!("../../../static/admin.js");
static THEME_INIT_JS: &str = include_str!("../../../static/theme-init.js");

fn valid_version_query(req: &axum::extract::Request) -> bool {
    req.uri().query().is_some_and(|query| {
        query.split('&').any(|part| {
            part.strip_prefix("v=")
                .is_some_and(crate::templates::static_asset_version_matches)
        })
    })
}

fn static_asset_response(
    req: &axum::extract::Request,
    body: &'static str,
    content_type: &'static str,
) -> impl IntoResponse {
    let cache_control = if valid_version_query(req) {
        crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA
    } else {
        crate::cache::CACHE_CONTROL_STATIC_SHORT
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
        ],
        body,
    )
}

pub(super) async fn serve_css(req: axum::extract::Request) -> impl IntoResponse {
    static_asset_response(&req, STYLE_CSS, "text/css; charset=utf-8")
}

pub(super) async fn serve_main_js(req: axum::extract::Request) -> impl IntoResponse {
    static_asset_response(&req, MAIN_JS, "application/javascript; charset=utf-8")
}

pub(super) async fn serve_admin_css(req: axum::extract::Request) -> impl IntoResponse {
    static_asset_response(&req, ADMIN_CSS, "text/css; charset=utf-8")
}

pub(super) async fn serve_admin_js(req: axum::extract::Request) -> impl IntoResponse {
    static_asset_response(&req, ADMIN_JS, "application/javascript; charset=utf-8")
}

pub(super) async fn serve_theme_init_js(req: axum::extract::Request) -> impl IntoResponse {
    static_asset_response(&req, THEME_INIT_JS, "application/javascript; charset=utf-8")
}

#[cfg(test)]
mod tests {
    use super::{MAIN_JS, STYLE_CSS};

    #[test]
    fn stylesheet_centers_mobile_user_preferences_panel_and_caps_viewport_usage() {
        assert!(STYLE_CSS.contains(".user-preferences-form {\n"));
        assert!(STYLE_CSS.contains("max-height: calc(100vh - 24px);"));
        assert!(STYLE_CSS.contains("overflow-y: auto;"));
        assert!(STYLE_CSS.contains("@media (max-width: 700px) {\n  .user-preferences-panel[open] .user-preferences-form {\n    position: fixed;"));
        assert!(STYLE_CSS.contains("top: max(12px, env(safe-area-inset-top));"));
        assert!(STYLE_CSS.contains("bottom: max(12px, env(safe-area-inset-bottom));"));
        assert!(STYLE_CSS.contains("margin: auto;"));
        assert!(STYLE_CSS.contains("transform: none;"));
        assert!(STYLE_CSS.contains("max-width: calc(100vw - 24px);"));
        assert!(STYLE_CSS.contains("max-height: calc(100dvh - 24px);"));
    }

    #[test]
    fn main_js_progressively_persists_user_preference_changes() {
        assert!(MAIN_JS.contains("function initUserPreferencesForms()"));
        assert!(MAIN_JS.contains("x-rustchan-background"));
        assert!(MAIN_JS.contains("new URLSearchParams(new FormData(form))"));
        assert!(MAIN_JS.contains("control.name === 'theme'"));
        assert!(MAIN_JS.contains("control.name === 'hide_nsfw_boards'"));
        assert!(MAIN_JS.contains("data-hide-nsfw-boards"));
        assert!(
            !MAIN_JS.contains("var firstControl = panel.querySelector('select, input, button');")
        );
    }
}
