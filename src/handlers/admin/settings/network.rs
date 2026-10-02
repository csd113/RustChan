//! Authenticated, origin-checked network configuration saves.

use super::{
    admin_panel_redirect_anchor, require_admin_post_origin_and_csrf, require_admin_session_sid,
    AppError, AppState, CookieJar, Form, HeaderMap, Response, Result, State, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use std::collections::BTreeMap;

/// Save an entire related network configuration after authorization and validation.
pub(in crate::server) async fn update_network_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BTreeMap<String, String>>,
) -> Result<Response> {
    require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.get("_csrf").map(String::as_str),
    )?;
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    tokio::task::spawn_blocking(move || -> Result<Response> {
        let conn = state.db.get()?;
        require_admin_session_sid(&conn, session_id.as_deref())?;
        let save_result = crate::config::admin::save_network(&form);
        match save_result {
            Ok(()) => Ok(admin_panel_redirect_anchor(if crate::config::admin::restart_pending().unwrap_or(true) { "Settings saved. Restart required: use Restart RustChan to apply these changes." } else { "Settings saved. No restart is required for the effective configuration." }, "network-security").into_response()),
            Err(error) => {
                let mut fields = crate::config::admin::network_snapshot().map_err(|_| "unavailable".to_owned());
                if let Ok(fields) = &mut fields {
                    for field in fields {
                        if let Some(value) = form.get(field.definition.key) { field.input.clone_from(value); }
                    }
                }
                let csrf = form.get("_csrf").map_or("", String::as_str);
                let body = format!("<div class=\"admin-panel\"><p role=\"alert\" class=\"admin-flash flash-error\">No changes saved: {}</p><p><a href=\"/admin/panel#network-security\">Return to admin panel</a></p>{}</div>", crate::utils::sanitize::escape_html(&error.to_string()), crate::templates::admin::render_network_settings(&fields, csrf));
                let html = crate::templates::base_layout("Network settings validation", None, &body, csrf, &[], current_theme.as_deref(), None, false, "/admin/panel");
                Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, axum::response::Html(html)).into_response())
            }
        }
    }).await.map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
}
