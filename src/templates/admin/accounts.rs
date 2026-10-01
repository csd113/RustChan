//! Secret-free administrator list and dedicated credential workflows.

use std::fmt::Write as _;

use crate::utils::sanitize::escape_html;

/// Render full-privilege account management with mandatory reauthentication.
pub(super) fn render(users: &[(i64, String, i64)], csrf: &str) -> String {
    let mut rows = String::new();
    let mut choices = String::new();
    for (id, name, created) in users {
        let name = escape_html(name);
        let created = escape_html(&crate::templates::fmt_ts(*created));
        let _ = write!(
            rows,
            "<tr><td>{id}</td><td>{name}</td><td>{created}</td></tr>"
        );
        let _ = write!(choices, "<option value=\"{name}\">{name}</option>");
    }
    format!(r#"<section class="admin-section admin-settings-section admin-task" id="accounts" aria-labelledby="accounts-title"><h2 id="accounts-title">Accounts</h2><p>All administrators have full site privileges. Existing passwords and hashes are never displayed. Every change requires your current password; failed checks share the administrator login lockout.</p><div class="admin-table-wrap" tabindex="0" role="region" aria-label="Administrator accounts"><table class="admin-table"><thead><tr><th>ID</th><th>Username</th><th>Created</th></tr></thead><tbody>{rows}</tbody></table></div><div class="admin-settings-grid">{}{}</div></section>"#,
    form("create", "Create administrator", "<label for=\"account-create-name\">New username</label><input type=\"text\" name=\"username\" id=\"account-create-name\" required maxlength=\"64\" autocomplete=\"off\">", csrf),
    form("password", "Change or reset password", &format!("<label for=\"account-password-name\">Account</label><select name=\"username\" id=\"account-password-name\">{choices}</select><p>Reset revokes every session for the selected account, including your current session if selected.</p>"), csrf))
}

/// Render one credential form with distinct accessible field IDs.
fn form(action: &str, title: &str, target: &str, csrf: &str) -> String {
    format!(
        r#"<form method="POST" action="/admin/accounts/{action}" id="admin-account-{action}" class="admin-settings-group"><h3>{title}</h3><input type="hidden" name="_csrf" value="{}">{target}<label for="account-{action}-current">Your current password</label><input type="password" id="account-{action}-current" name="current_password" required maxlength="1024" autocomplete="current-password"><label for="account-{action}-new">New password (at least 8 characters)</label><input type="password" id="account-{action}-new" name="new_password" required minlength="8" maxlength="1024" autocomplete="new-password"><label for="account-{action}-confirm">Confirm new password</label><input type="password" id="account-{action}-confirm" name="confirm_password" required minlength="8" maxlength="1024" autocomplete="new-password"><button type="submit">{title}</button></form>"#,
        escape_html(csrf)
    )
}
