//! Server-rendered settings search, including no-JavaScript operation.

use crate::config::admin::{application, runtime, NETWORK_SETTINGS};
use crate::utils::sanitize::escape_html;

/// Render search and matching setting links without exposing any setting values.
pub(super) fn render(query: Option<&str>) -> String {
    let input = query
        .unwrap_or_default()
        .chars()
        .take(128)
        .collect::<String>();
    let mut results = String::new();
    if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
        let terms = query
            .split_whitespace()
            .take(16)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let sections =
            std::iter::once(("network-security", "Network & Security", NETWORK_SETTINGS))
                .chain(
                    runtime::SECTIONS
                        .iter()
                        .map(|s| (s.key(), s.label(), s.definitions())),
                )
                .chain(std::iter::once((
                    "configuration-state",
                    "Application state",
                    application::SETTINGS,
                )));
        let mut count = 0_usize;
        for (section, label, definitions) in sections {
            for definition in definitions {
                let haystack = format!(
                    "{} {} {} {}",
                    definition.key, definition.label, definition.help, definition.environment
                )
                .to_lowercase();
                if terms.iter().all(|term| haystack.contains(term)) {
                    count = count.saturating_add(1);
                    crate::templates::append_html(
                        &mut results,
                        format_args!(
                            "<li><a href=\"/admin/panel?open={section}#{}\">{} — {}</a></li>",
                            if section == "configuration-state" {
                                format!("application-{}", definition.key)
                            } else {
                                format!("setting-{}", definition.key)
                            },
                            escape_html(label),
                            escape_html(definition.label)
                        ),
                    );
                }
            }
        }
        for (section, label, synonyms) in [
            ("software-updates", "Software Updates", "software update upgrade release version rollback"),
            ("accounts", "Administrator accounts and passwords", "accounts administrator create reset change password"),
            ("storage", "Storage relocation and secret rotation", "storage paths database uploads data directory relocate migrate cookie secret rotate CSRF"),
            ("visitor-defaults", "Default NSFW visibility", "default hide NSFW boards visibility visitor preference"),
            ("boards", "Per-board limits and posting settings", "board upload limit image video audio PDF arbitrary files bump archive cooldown captcha self edit delete password"),
            ("board-banners", "Banner management", "appearance banners rotation external link home global board"),
            ("full-backup-restore", "Backup and restore workflows", "backup restore retention export import split ZIP verify"),
            ("maintenance", "Database tools and setup controls", "maintenance database schema integrity check repair vacuum setup reopen"),
            ("moderation", "Moderation", "moderation reports appeals bans filters words IP"),
        ] {
            let haystack = synonyms.to_lowercase();
            if terms.iter().all(|term| haystack.contains(term)) { count = count.saturating_add(1); crate::templates::append_html(&mut results, format_args!( "<li><a href=\"/admin/panel?open={section}#{section}\">{label}</a></li>")); }
        }
        if count == 0 {
            results.push_str("<li>No matching runtime settings. Try proxy, Serveo, slow down or upload limit.</li>");
        }
        results = format!("<p role=\"status\">{count} matching settings</p><ul class=\"admin-settings-search-results\">{results}</ul>");
    }
    format!(
        r#"<section class="admin-settings-search" aria-label="Settings search"><form method="GET" action="/admin/panel"><label for="admin-settings-query">Find a setting</label><input type="search" id="admin-settings-query" name="q" value="{}" maxlength="128" placeholder="Proxy, Serveo, slow down, upload limit"><button type="submit">Search settings</button></form>{results}</section>"#,
        escape_html(&input)
    )
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn synonyms_link_to_authoritative_controls_without_javascript() {
        for (query, target) in [
            ("Serveo", "setting-behind_proxy"),
            ("proxy", "setting-trusted_proxy_cidrs"),
            ("slow down", "setting-rate_limit_gets"),
            ("upload limit", "setting-max_image_size_mb"),
        ] {
            let html = render(Some(query));
            assert!(
                html.contains(target),
                "search synonym {query} must find {target}"
            );
            assert!(
                html.contains("method=\"GET\""),
                "search must work without JavaScript"
            );
        }
    }

    #[test]
    fn query_is_escaped_and_search_does_not_render_values_or_secrets() {
        let payload = ["<", "script>alert('query')</", "script>"].concat();
        let html = render(Some(&payload));
        assert!(!html.contains(&payload), "search input must be escaped");
        assert!(
            !html.contains("cookie_secret"),
            "ordinary search must not expose secret configuration"
        );
        assert!(
            html.contains("No matching runtime settings"),
            "missing matches need a useful result"
        );
    }
}
