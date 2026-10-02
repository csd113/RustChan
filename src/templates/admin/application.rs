//! Compact active/saved/source review for retained live controls.

use std::fmt::Write as _;

use crate::{config::admin::SettingField, utils::sanitize::escape_html};

/// Render separate active, saved and restart state for every retained control.
pub(super) fn render(fields: &Result<Vec<SettingField>, String>) -> String {
    let Ok(fields) = fields else {
        return "<section class=\"admin-section admin-task\" id=\"configuration-state\"><h2>Configuration application state</h2><p role=\"alert\">Cannot read configuration state safely.</p></section>".into();
    };
    let mut rows = String::new();
    for f in fields {
        let _ = write!(rows, "<tr id=\"application-{}\"><th scope=\"row\">{}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><span>{}</span><details class=\"admin-application-help\"><summary>Setting help</summary><p>{}</p></details></td></tr>", escape_html(f.definition.key), escape_html(f.definition.label), escape_html(&f.active), escape_html(&f.saved), escape_html(&f.next_start), escape_html(&f.source), if f.definition.application == crate::config::admin::ApplicationMode::Live && !f.overridden { "Applies live" } else if f.overridden { "Saved value is overridden on restart" } else if f.pending { "Restart would change the active value" } else { "Active matches persisted/effective state" }, escape_html(f.definition.help));
    }
    format!("<section class=\"admin-section admin-task\" id=\"configuration-state\"><h2>Configuration application state</h2><p>Existing live controls are reviewed here alongside the restart controls in each section. Appearance, themes and pruning use the database after seeding. FFmpeg timeout is committed with Media settings in the database. Automatic backups save to settings.toml before updating the scheduler. Startup aliases do not compete with an already configured database value.</p><div class=\"admin-table-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Configuration application state\"><table class=\"admin-table admin-state-table\"><thead><tr><th>Setting</th><th>Active</th><th>Saved</th><th>Next restart</th><th>Source</th><th>Application / help</th></tr></thead><tbody>{rows}</tbody></table></div></section>")
}
