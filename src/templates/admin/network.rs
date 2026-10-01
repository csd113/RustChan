//! Dedicated Network & Security forms with honest startup configuration state.

use std::fmt::Write as _;

use crate::config::admin::{InputKind, SettingField};
use crate::utils::sanitize::escape_html;

/// Render network controls, or fail closed when the settings file cannot be read.
pub(super) fn render(fields: &Result<Vec<SettingField>, String>, csrf: &str) -> String {
    render_section(
        fields,
        csrf,
        "network-security",
        "Network & Security",
        "/admin/network/settings",
        "Configure proxies, visitor request limits, authentication and public monitoring.",
    )
}

/// Render a related runtime section using the same accessible form patterns.
pub(super) fn render_section(
    fields: &Result<Vec<SettingField>, String>,
    csrf: &str,
    key: &str,
    label: &str,
    action: &str,
    description: &str,
) -> String {
    let Ok(fields) = fields else {
        return format!("<section class=\"admin-section\" id=\"{key}\"><h2>{}</h2><p role=\"alert\">Settings could not be read safely. Repair settings.toml or its permissions before saving.</p></section>", escape_html(label));
    };
    let mut controls = render_control_groups(fields, key);
    if key == "https" {
        controls.push_str("<p><label><input type=\"checkbox\" name=\"confirm_https_access\" value=\"1\"> I have verified native HTTPS access at the active HTTPS port before requiring HTTPS.</label> Enable native HTTPS and restart first; verify access before disabling plaintext application access. Keep a service-host rollback path.</p>");
    }
    let capabilities = if key == "https" {
        format!("<p class=\"admin-meta-note\">Build support: ACME {}; self-signed development certificates {}. Manual PEM certificates are supported. Saving does not open a listener or contact an issuer. Verify DNS, firewall, launcher and HTTPS reachability before applying an HTTPS-only configuration.</p>", if cfg!(feature = "tls-acme") { "available" } else { "unavailable" }, if cfg!(feature = "tls-self-signed") { "available" } else { "unavailable" })
    } else {
        String::new()
    };
    format!(
        r#"<section class="admin-section admin-settings-section admin-task" id="{key}" aria-labelledby="{key}-title">
<h2 id="{key}-title">{label}</h2>
<p>{description}</p>{capabilities}
<p class="admin-meta-note">Save writes settings.toml atomically. These settings require a restart; running listeners and workers keep their active values. Environment and launcher overrides take precedence on restart.</p>
<form method="POST" action="{action}" id="admin-{form_key}-settings">
<input type="hidden" name="_csrf" value="{csrf}">
{controls}
<div class="admin-settings-save"><button type="submit">Save for next restart</button><span>Review saved values and overrides before restarting your service.</span></div>
</form></section>"#,
        label = escape_html(label),
        description = escape_html(description),
        csrf = escape_html(csrf),
        form_key = if key == "network-security" {
            "network"
        } else {
            key
        }
    )
}

/// Use shallow, task-specific groups so related controls stay easy to scan.
fn render_control_groups(fields: &[SettingField], section: &str) -> String {
    let groups: &[(&str, &[&str])] = match section {
        "network-security" => &[
            (
                "Proxy and visitor identity",
                &[
                    "behind_proxy",
                    "trusted_proxy_cidrs",
                    "public_hosts",
                    "https_cookies",
                ],
            ),
            ("Listener", &["port", "bind_addr"]),
            (
                "Browsing limits and sessions",
                &[
                    "rate_limit_gets",
                    "rate_limit_window",
                    "rate_limit_policy",
                    "session_duration",
                ],
            ),
            (
                "Public monitoring",
                &["public_readiness_details", "public_metrics_enabled"],
            ),
        ],
        "media" => &[
            (
                "New-board defaults and global gates",
                &[
                    "max_image_size_mb",
                    "max_video_size_mb",
                    "max_audio_size_mb",
                    "enable_any_file_uploads_feature",
                    "archive_before_prune",
                ],
            ),
            (
                "Media tools and processing",
                &[
                    "require_ffmpeg",
                    "ffmpeg_path",
                    "thumb_size",
                    "job_queue_capacity",
                    "waveform_cache_max_mb",
                ],
            ),
            (
                "Managed-media reconciliation",
                &[
                    "media_reconcile_repair_enabled",
                    "media_reconcile_interval_hours",
                    "media_reconcile_files_per_pass",
                    "media_reconcile_database_rows_per_pass",
                    "media_reconcile_hash_bytes_per_pass",
                    "media_reconcile_repairs_per_pass",
                ],
            ),
        ],
        _ => &[],
    };
    if groups.is_empty() {
        return format!(
            "<div class=\"admin-settings-grid\">{}</div>",
            fields.iter().map(render_field).collect::<String>()
        );
    }
    let mut output = String::new();
    for (label, keys) in groups {
        let controls = fields
            .iter()
            .filter(|f| keys.contains(&f.definition.key))
            .map(render_field)
            .collect::<String>();
        let _ = write!(output, "<fieldset class=\"admin-settings-group\"><legend>{label}</legend><div class=\"admin-settings-grid\">{controls}</div></fieldset>");
    }
    output
}

/// Render one accessible control with separate saved and active values.
fn render_field(field: &SettingField) -> String {
    let definition = field.definition;
    let key = definition.key;
    let input = escape_html(&field.input);
    let control = match definition.kind {
        InputKind::Boolean => {
            let mut options = String::new();
            if key == "https_cookies" {
                options.push_str(&option("", "Automatic", &field.input));
            }
            options.push_str(&option("true", "Enabled", &field.input));
            options.push_str(&option("false", "Disabled", &field.input));
            format!(
                r#"<select id="setting-{key}" name="{key}" aria-describedby="help-{key} state-{key}">{options}</select>"#
            )
        }
        InputKind::Number(min, max) => format!(
            r#"<input id="setting-{key}" name="{key}" type="number" min="{min}" max="{max}" step="1" value="{input}" required aria-describedby="help-{key} state-{key}">"#
        ),
        InputKind::List => format!(
            r#"<textarea id="setting-{key}" name="{key}" rows="3" maxlength="4096" aria-describedby="help-{key} state-{key}">{input}</textarea>"#
        ),
        InputKind::Text | InputKind::OptionalText => format!(
            r#"<input id="setting-{key}" name="{key}" type="text" value="{input}" maxlength="4096" aria-describedby="help-{key} state-{key}">"#
        ),
        InputKind::Address => format!(
            r#"<input id="setting-{key}" name="{key}" type="text" value="{input}" placeholder="Automatic interface + primary port" maxlength="4096" aria-describedby="help-{key} state-{key}">"#
        ),
        InputKind::Policy => format!(
            r#"<select id="setting-{key}" name="{key}" aria-describedby="help-{key} state-{key}">{}{}</select>"#,
            option("legacy", "Legacy: all methods", &field.input),
            option("reads", "Reads: GET and HEAD", &field.input)
        ),
    };
    let state = if field.overridden {
        "Override prevents the saved value taking effect; manage the environment or launcher"
    } else if field.pending {
        "Saved configuration differs — restart required; check overrides"
    } else {
        "Saved configuration matches active value"
    };
    format!(
        r#"<div class="admin-setting" data-setting-search="{} {} {}">
<label for="setting-{key}">{}</label>{control}
<p id="help-{key}" class="admin-setting-help">{}</p>
<dl id="state-{key}" class="admin-setting-state"><dt>Active</dt><dd>{}</dd><dt>Saved</dt><dd>{}</dd><dt>Next restart</dt><dd>{}</dd><dt>Source</dt><dd>{}</dd><dt>Application</dt><dd>{state}</dd></dl>
</div>"#,
        escape_html(key),
        escape_html(definition.label),
        escape_html(definition.help),
        escape_html(definition.label),
        escape_html(definition.help),
        escape_html(&field.active),
        escape_html(&field.saved),
        escape_html(&field.next_start),
        escape_html(&field.source)
    )
}

/// Render a fixed selection option with explicit selected state.
fn option(value: &str, label: &str, selected: &str) -> String {
    format!(
        r#"<option value="{value}"{}>{label}</option>"#,
        if value == selected { " selected" } else { "" }
    )
}
