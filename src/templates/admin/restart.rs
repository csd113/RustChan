//! Compact configuration restart status within the existing administrator panel.

use crate::{restart::View, utils::sanitize::escape_html};

/// Preserve a fully usable POST form and refresh link with JavaScript disabled.
pub(super) fn render(view: &View, csrf: &str) -> String {
    let action = if view.pending && view.supported && !view.in_progress {
        format!(
            r#"<form method="POST" action="/admin/restart" id="admin-restart-form"><input type="hidden" name="_csrf" value="{}"><button type="submit">Restart RustChan</button><span class="admin-meta-note">Stops gracefully and verifies startup. Unsaved form edits are not included.</span></form>"#,
            escape_html(csrf)
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="admin-section admin-restart" id="settings-restart" aria-labelledby="settings-restart-title" data-restart-instance="{}" data-restart-pending="{}" data-restart-in-progress="{}"><h2 id="settings-restart-title">Configuration restart</h2><p role="status" id="admin-restart-status">{}</p>{action}<p><a href="/admin/panel#settings-restart">Refresh restart status</a></p></section>"#,
        view.instance,
        view.pending,
        view.in_progress,
        escape_html(&view.message)
    )
}
