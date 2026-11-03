//! Manual settings-restart status within the existing administrator panel.

/// Render read-only guidance with a normal refresh link and no execution control.
pub(super) fn render(view: &crate::restart::View) -> String {
    format!(
        r#"<section class="admin-section admin-restart" id="settings-restart" aria-labelledby="settings-restart-title"><h2 id="settings-restart-title">Configuration restart</h2><p role="status" id="admin-restart-status">{}</p><p><a href="/admin/panel#settings-restart">Refresh settings status</a></p></section>"#,
        crate::utils::sanitize::escape_html(&view.message)
    )
}
