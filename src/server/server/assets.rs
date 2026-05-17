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
    fn stylesheet_uses_mobile_sheet_for_user_preferences_panel() {
        assert!(STYLE_CSS.contains(".user-preferences-form {\n"));
        assert!(STYLE_CSS.contains("max-height: calc(100vh - 24px);"));
        assert!(STYLE_CSS.contains("overflow-y: auto;"));
        assert!(!STYLE_CSS.contains(".user-preferences-form button[type=\"submit\"]"));
        assert!(STYLE_CSS.contains(".user-preferences-panel[open]::before"));
        assert!(STYLE_CSS.contains("body.user-preferences-mobile-open {\n    overflow: hidden;"));
        assert!(STYLE_CSS.contains("background: rgba(0,0,0,0.42);"));
        assert!(STYLE_CSS.contains(".user-preferences-mobile-close {\n  display: none;"));
        assert!(STYLE_CSS.contains("visibility: hidden;"));
        assert!(STYLE_CSS.contains(
            ".user-preferences-panel[open] .user-preferences-form {\n    position: fixed;"
        ));
        assert!(STYLE_CSS.contains("bottom: 0;"));
        assert!(STYLE_CSS.contains("inset-block-end: 0;"));
        assert!(STYLE_CSS.contains("margin: 0 auto;"));
        assert!(STYLE_CSS.contains("transform: translate3d(0, 0, 0);"));
        assert!(STYLE_CSS.contains("max-width: 30rem;"));
        assert!(STYLE_CSS.contains(
            "max-height: min(68svh, calc(100svh - max(20px, env(safe-area-inset-top))));"
        ));
        assert!(STYLE_CSS.contains("overflow-x: hidden;"));
        assert!(STYLE_CSS.contains("border-radius: 18px 18px 0 0;"));
        assert!(STYLE_CSS.contains("position: sticky;"));
        assert!(STYLE_CSS.contains(".user-preferences-form > label {\n    min-height: 46px;"));
        assert!(STYLE_CSS.contains(".user-preferences-form input[type=\"checkbox\"],\n  .user-preferences-form input[type=\"radio\"] {\n    min-width: 24px;"));
    }

    #[test]
    fn main_js_progressively_persists_user_preference_changes() {
        assert!(MAIN_JS.contains("function initUserPreferencesForms()"));
        assert!(MAIN_JS.contains("function mirrorUserPreferencesToCookies(form)"));
        assert!(MAIN_JS.contains("setPublicPreferenceCookie('rustchan_theme', theme.value);"));
        assert!(MAIN_JS
            .contains("setPublicPreferenceCookie('rustchan_preferred_view', boardView.value);"));
        assert!(MAIN_JS.contains("x-rustchan-background"));
        assert!(MAIN_JS.contains("keepalive: true"));
        assert!(MAIN_JS.contains("new URLSearchParams(new FormData(form))"));
        assert!(MAIN_JS.contains(
            "form.addEventListener('submit', function (event) {\n        event.preventDefault();"
        ));
        assert!(MAIN_JS.contains("control.name === 'theme'"));
        assert!(MAIN_JS.contains("control.name === 'hide_nsfw_boards'"));
        assert!(MAIN_JS.contains("data-hide-nsfw-boards"));
        assert!(MAIN_JS
            .contains("var mobileClose = panel.querySelector('.user-preferences-mobile-close');"));
        assert!(MAIN_JS.contains("function syncUserPreferencesBackgroundScrollLock()"));
        assert!(MAIN_JS.contains("document.body.style.position = 'fixed';"));
        assert!(MAIN_JS.contains("window.scrollTo(0, scrollY);"));
        assert!(MAIN_JS.contains("panel.open = false;"));
        assert!(
            !MAIN_JS.contains("var firstControl = panel.querySelector('select, input, button');")
        );
    }

    #[test]
    fn stylesheet_keeps_mobile_dialogs_and_popups_inside_viewport() {
        assert!(STYLE_CSS.contains("@media (max-width: 700px) {\n  .quotelink-popup {"));
        assert!(STYLE_CSS.contains("max-width: calc(100vw - 16px);"));
        assert!(STYLE_CSS.contains("max-height: min(70vh, 26rem);"));
        assert!(STYLE_CSS.contains(".edit-modal,\n  .compress-modal {"));
        assert!(STYLE_CSS.contains("align-items: flex-start;"));
        assert!(STYLE_CSS.contains(
            "padding: max(12px, env(safe-area-inset-top)) 12px max(12px, env(safe-area-inset-bottom));"
        ));
        assert!(STYLE_CSS.contains(".edit-modal-box,\n  .compress-modal-box {"));
        assert!(STYLE_CSS.contains(
            "max-height: calc(100svh - 24px - env(safe-area-inset-top) - env(safe-area-inset-bottom));"
        ));
        assert!(STYLE_CSS.contains(".edit-modal-box .post-form td:last-child {"));
        assert!(STYLE_CSS.contains(".edit-modal-box .edit-btn[data-action=\"close-edit-modal\"]"));
    }

    #[test]
    fn main_js_positions_mobile_menus_and_popups_against_visual_viewport() {
        assert!(MAIN_JS.contains("function getThreadMenuBounds(gutter)"));
        assert!(MAIN_JS.contains("window.visualViewport && window.visualViewport.height"));
        assert!(MAIN_JS.contains("function clampPopupToViewport(anchor, popup)"));
        assert!(MAIN_JS.contains("visualViewport.offsetLeft"));
        assert!(MAIN_JS.contains("visualViewport.offsetTop"));
        assert!(MAIN_JS.contains("var minTop = viewportTop + gutter;"));
        assert!(MAIN_JS.contains("var maxTop = viewportTop + Math.max(gutter, vh - ph - gutter);"));
        assert!(MAIN_JS.contains("var position = clampPopupToViewport(anchor, popup);"));
        assert!(MAIN_JS.contains(
            "window.visualViewport.addEventListener('resize', repositionOpenThreadMenus);"
        ));
        assert!(MAIN_JS.contains(
            "window.visualViewport.addEventListener('scroll', repositionOpenThreadMenus);"
        ));
    }
}
