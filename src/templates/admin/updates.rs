//! Software update controls rendered only inside the authorized administration shell.

use crate::updates::{Discovery, Status};
use crate::utils::sanitize::escape_html;

/// Render native update status or a clear deployment-managed check-only workflow.
pub(super) fn render(status: &Status, csrf: &str, managed: bool, container: bool) -> String {
    let (latest, verification) = match &status.discovery {
        Some(Discovery::Available(release)) => (
            escape_html(&release.version),
            escape_html(&release.verification),
        ),
        Some(Discovery::UpToDate) => (
            "Up to date".to_owned(),
            "No newer stable release found.".to_owned(),
        ),
        Some(Discovery::UnableToCheck(message)) => ("Unavailable".to_owned(), escape_html(message)),
        None => (
            "Not checked".to_owned(),
            "Check the official stable releases to see availability.".to_owned(),
        ),
    };
    let active = status.phase.active();
    let progress = if active { "true" } else { "false" };
    let mut controls = format!(
        r#"<form method="POST" action="/admin/updates/check" id="admin-update-check" class="admin-settings-group"><input type="hidden" name="_csrf" value="{}"><button type="submit"{}>Check for updates</button></form>"#,
        escape_html(csrf),
        if active { " disabled" } else { "" }
    );
    if managed {
        if let Some(approval) = status.approval.as_deref().filter(|_| {
            !active
                && status
                    .available_release()
                    .is_some_and(|release| release.compatible)
        }) {
            crate::templates::append_html(
                &mut controls,
                format_args!(
                    r#"<form method="POST" action="/admin/updates/install" id="admin-update-install" class="admin-settings-group"><h3>Install verified update</h3><p>This installs the official prebuilt RustChan release and replaces the running executable, including any local source modifications. Your data and configuration are retained.</p><p>A verified backup is mandatory. RustChan will briefly restart. Failed upgrades restore the previous software, database, configuration and persistent files.</p><input type="hidden" name="_csrf" value="{}"><input type="hidden" name="approval" value="{}"><label for="update-current-password">Your current password</label><input type="password" name="current_password" id="update-current-password" required maxlength="1024" autocomplete="current-password"><label for="update-confirmation">Type INSTALL to confirm the restart</label><input type="text" name="confirmation" id="update-confirmation" required pattern="INSTALL" maxlength="7" autocomplete="off"><p class="admin-meta-note">Approval expires after 10 minutes and can be used once.</p><button type="submit">Install update</button></form>"#,
                    escape_html(csrf),
                    escape_html(approval)
                ),
            );
        }
    } else {
        let note = if container {
            "Container installation is deployment-managed. Pull the chosen image and recreate the container with its persistent data volume."
        } else {
            "Installation is deployment-managed on this system. Source builds are supported. Eligible Linux installations support built-in updates when the existing account owns the executable and data. For a manual upgrade, stop RustChan, back up the complete data directory, and replace the executable while keeping the same data directory. See the <a href=\"https://github.com/csd113/RustChan/blob/main/docs/software-updates.md#manual-upgrade-of-a-source-install\">upgrade guide</a> for preparation and rollback."
        };
        crate::templates::append_html(
            &mut controls,
            format_args!("<p class=\"admin-copy\">{note}</p>"),
        );
    }
    format!(
        r#"<section class="admin-section admin-settings-section" id="software-updates" aria-labelledby="software-updates-title" data-update-active="{progress}"><h2 id="software-updates-title">Software Updates</h2><p class="admin-copy">Stable releases from the official RustChan repository. Update checks and installation details are visible only to administrators.</p><dl class="admin-control-detail-list"><dt>Running version</dt><dd>{}</dd><dt>Latest stable version</dt><dd>{latest}</dd><dt>Last check</dt><dd>{}</dd><dt>Verification</dt><dd>{verification}</dd><dt>Previous result / current progress</dt><dd id="admin-update-message" role="status" aria-live="polite">{}</dd></dl><div class="admin-settings-grid">{controls}</div><p class="admin-meta-note">If the server restarts, reconnect to <a href="/admin/panel?open=software-updates#software-updates">Software Updates</a> to see the persisted result. JavaScript reconnects automatically while an installation is active.</p>{}</section>"#,
        escape_html(crate::updates::VERSION),
        escape_html(status.checked_at.as_deref().unwrap_or("Not checked")),
        escape_html(if status.message.is_empty() {
            "No installation attempted."
        } else {
            &status.message
        }),
        backup_summary(status)
    )
}

/// Keep backup metadata in the existing Backups task; updater-owned snapshots have no destructive web controls.
pub(super) fn backups(status: &Status) -> String {
    format!(
        r#"<section class="admin-section" id="pre-upgrade-backups"><h2>Pre-upgrade backups</h2><p class="admin-copy">Verified snapshots retained for automatic rollback. Manual and scheduled backups remain available above. Update snapshots include the database, configuration, media and private runtime state; the updater controls restoration and retention.</p>{}</section>"#,
        backup_summary(status)
    )
}

/// Render bounded, escaped backup history without leaking absolute storage paths.
fn backup_summary(status: &Status) -> String {
    if status.backups.is_empty() {
        return "<p class=\"admin-meta-note\">No pre-upgrade snapshots retained.</p>".to_owned();
    }
    let mut rows = String::new();
    for backup in status.backups.iter().take(20) {
        crate::templates::append_html(
            &mut rows,
            format_args!(
                "<tr><td>{}</td><td>{} → {}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&backup.created_at),
                escape_html(&backup.previous_version),
                escape_html(&backup.target_version),
                backup.size,
                if backup.verified {
                    "Verified pre-upgrade backup"
                } else {
                    "Verification failed"
                }
            ),
        );
    }
    format!("<div class=\"admin-table-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Pre-upgrade snapshots\"><table class=\"admin-table\"><thead><tr><th>Created</th><th>Upgrade</th><th>Bytes</th><th>Verification</th></tr></thead><tbody>{rows}</tbody></table></div>")
}
