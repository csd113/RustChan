//! Authenticated saves for restart-required runtime configuration groups.

use super::{
    admin_panel_redirect_anchor, require_admin_post_origin_and_csrf, require_admin_session_sid,
    AppError, AppState, CookieJar, Form, HeaderMap, Response, Result, State, SESSION_COOKIE,
};
use crate::config::admin::runtime::RuntimeSection;
use axum::{extract::Path, response::IntoResponse as _};
use std::collections::BTreeMap;

/// Validate and atomically save one complete related configuration group.
pub(in crate::server) async fn update_runtime_settings(
    State(state): State<AppState>,
    Path(section): Path<RuntimeSection>,
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
    let session_id = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    tokio::task::spawn_blocking(move || -> Result<Response> {
        let conn = state.db.get()?;
        require_admin_session_sid(&conn, session_id.as_deref())?;
        let save_result = crate::config::admin::runtime::save_section(section, &form);
        match save_result {
            Ok(()) => Ok(admin_panel_redirect_anchor("Configuration saved. Restart required; environment and launcher overrides still take precedence.", section.key()).into_response()),
            Err(error) => {
                let mut fields = crate::config::admin::runtime::section_snapshot(section).map_err(|_| "unavailable".to_owned());
                if let Ok(fields) = &mut fields {
                    for field in fields {
                        if let Some(value) = form.get(field.definition.key) { field.input.clone_from(value); }
                    }
                }
                let csrf = form.get("_csrf").map_or("", String::as_str);
                let body = format!("<div class=\"admin-panel\"><p role=\"alert\" class=\"admin-flash flash-error\">No changes saved: {}</p><p><a href=\"/admin/panel#{}\">Return to admin panel</a></p>{}</div>", crate::utils::sanitize::escape_html(&error.to_string()), section.key(), crate::templates::admin::render_runtime_settings(section, &fields, csrf));
                let html = crate::templates::base_layout("Configuration validation", None, &body, csrf, &[], None, None, false, "/admin/panel");
                Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, axum::response::Html(html)).into_response())
            }
        }
    }).await.map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
}
