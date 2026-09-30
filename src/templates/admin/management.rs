//! Actionable offline relocation and secret rotation without exposing material.

use std::fmt::Write as _;

use crate::config::admin::management::ManagementState;
use crate::utils::sanitize::escape_html;

/// Show actionable relocation/rotation workflows with secret-safe state.
pub(super) fn render(state: &Result<ManagementState, String>, csrf: &str) -> String {
    let Ok(s) = state else {
        return "<section class=\"admin-section admin-task\" id=\"storage\"><h2>Storage &amp; secrets</h2><p role=\"alert\">Cannot read settings safely; repair settings.toml before management operations.</p></section>".into();
    };
    let mut paths = String::new();
    for (label, active, saved, next, source) in [
        (
            "Data directory",
            &s.data_dir,
            &s.data_dir,
            &s.data_dir,
            s.data_source.as_str(),
        ),
        (
            "SQLite database",
            &s.database,
            &s.saved_database,
            &s.next_database,
            s.database_source.as_str(),
        ),
        (
            "Board media root",
            &s.uploads,
            &s.saved_uploads,
            &s.next_uploads,
            s.uploads_source.as_str(),
        ),
    ] {
        let _ = write!(paths, "<tr><th>{label}</th><td><code>{}</code></td><td><code>{}</code></td><td><code>{}</code></td><td>{source}<br>Restart required for changes; paths alone do not migrate data.</td></tr>", escape_html(active), escape_html(saved), escape_html(next), source = escape_html(source));
    }
    let saved_secret = if s.secret_saved {
        "configured material withheld"
    } else {
        "missing; use the rotation workflow to persist fresh material"
    };
    let source = if s.secret_override {
        "CHAN_COOKIE_SECRET (externally managed override)"
    } else {
        "settings.toml / generated startup material"
    };
    let application = if s.secret_pending {
        "A different file secret is staged. Restart required; an environment override takes priority."
    } else {
        "No staged file rotation detected."
    };
    let rotate = if s.secret_override {
        "<p>Rotate CHAN_COOKIE_SECRET through the service secret manager, then restart. The panel cannot replace an operator-managed override.</p>".to_owned()
    } else {
        format!(
            r#"<form method="POST" action="/admin/secrets/rotate" id="admin-secret-rotation"><input type="hidden" name="_csrf" value="{}"><label for="secret-current-password">Your current administrator password</label><input id="secret-current-password" name="current_password" type="password" maxlength="1024" required autocomplete="current-password"><label for="secret-confirmation">Type ROTATE to stage a fresh secret</label><input id="secret-confirmation" name="confirmation" required pattern="ROTATE" autocomplete="off"><button type="submit">Stage secret rotation</button></form>"#,
            escape_html(csrf)
        )
    };
    format!(
        r#"<section class="admin-section admin-task" id="storage"><h2>Storage &amp; secrets</h2><h3>Effective storage and relocation</h3><div class="admin-table-wrap"><table><thead><tr><th>Location</th><th>Active</th><th>Saved/default</th><th>Next restart</th><th>Source/application</th></tr></thead><tbody>{paths}</tbody></table></div><p>Storage relocation is an offline service-host operation. This panel keeps the running paths visible; it cannot move an open SQLite database or update external service arguments.</p><ol><li>Create and verify a full backup in Backups, including the identities you need. Record the active paths above and the service's CHAN_DB, CHAN_UPLOADS and --data-dir overrides.</li><li>Stop RustChan through your service manager and confirm the process has exited. Keep the original data intact.</li><li>Copy the complete data directory to a new absolute destination owned by the service user, preserving private permissions and all database/WAL, board media, runtime/Tor and certificate files. If the database or uploads are outside the root, copy those complete locations separately while stopped.</li><li>For a data-root move, update the service command to <code>rustchan-cli --data-dir /absolute/new/data serve</code>. Update any absolute CHAN_DB/CHAN_UPLOADS overrides; for separate locations, edit <code>database_path</code> and <code>upload_dir</code> in the copied settings.toml. Environment overrides still take priority.</li><li>Start with the new service configuration. Verify these active paths, database checks, media, backups, HTTPS and onion identity before retiring the original copy. If verification fails, stop the new instance and restore the original launcher/environment and intact paths.</li></ol><h3>Cookie, CSRF and visitor-hashing secret</h3><p>Active: configured (value withheld). Saved: {saved_secret}. Source: {source}. {application}</p><p>Rotation takes effect on restart. It invalidates signed board grants, self-action ownership and CSRF tokens. All administrator sessions are revoked when startup detects a rotation. IP hashes change, so existing IP bans no longer identify the same visitors. Keep a private verified backup and plan how to replace affected bans. No secret value is shown or accepted by this form.</p>{rotate}</section>"#
    )
}
