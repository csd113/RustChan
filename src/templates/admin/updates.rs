//! Manual update links rendered in the authenticated administrator panel.

use crate::updates::{Discovery, Status, RELEASES_URL};
use crate::utils::sanitize::escape_html;

/// Render cached version information and a standard form usable without JavaScript.
pub(super) fn render(status: &Status, csrf: &str) -> String {
    let (latest, message, url) = match &status.discovery {
        Some(Discovery::Available(release)) => (
            release.version.as_str(),
            "A newer stable version is available.",
            release.url(),
        ),
        Some(Discovery::UpToDate(release)) => (
            release.version.as_str(),
            "No newer stable release was found.",
            release.url(),
        ),
        Some(Discovery::UnableToCheck(message)) => {
            ("Unavailable", message.as_str(), RELEASES_URL.to_owned())
        }
        None => (
            "Not checked",
            "Check the official releases for a newer version.",
            RELEASES_URL.to_owned(),
        ),
    };
    format!(
        r#"<section class="admin-section admin-settings-section" id="software-updates" aria-labelledby="software-updates-title"><h2 id="software-updates-title">Software Updates</h2><p class="admin-copy">Check for a newer stable RustChan release, then download and install it using your usual deployment tools.</p><dl class="admin-control-detail-list"><dt>Running version</dt><dd>{}</dd><dt>Latest stable version</dt><dd>{}</dd><dt>Last check</dt><dd>{}</dd></dl><p id="admin-update-message" role="status" aria-live="polite">{}</p><form method="POST" action="/admin/updates/check" id="admin-update-check" class="admin-settings-group"><input type="hidden" name="_csrf" value="{}"><button type="submit">Check for updates</button></form><p><a href="{}" rel="noreferrer">Download from GitHub releases</a></p><p class="admin-meta-note">Before replacing the binary, stop RustChan and back up the complete data directory and old executable. Keep the same data path when restarting. Container installs should replace the image and retain their data volume. See the <a href="https://github.com/csd113/RustChan/blob/main/docs/software-updates.md#manual-upgrade-of-a-source-install" rel="noreferrer">upgrade guide</a>.</p></section>"#,
        escape_html(crate::updates::VERSION),
        escape_html(latest),
        escape_html(
            status
                .checked_at
                .as_deref()
                .unwrap_or("Not checked since startup")
        ),
        escape_html(message),
        escape_html(csrf),
        escape_html(&url)
    )
}
