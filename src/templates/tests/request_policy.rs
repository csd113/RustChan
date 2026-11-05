//! The shared document must retain provenance for native same-origin form posts.

/// Validate actual rendered action and preferences fields against their intended scopes.
pub(crate) fn check_action_page_csrf(
    page: &str,
    raw: &str,
    scope: &str,
    action: &str,
) -> anyhow::Result<String> {
    use crate::middleware::{validate_csrf, validate_signed_csrf};
    use anyhow::{ensure, Context as _};

    let forms = regex::Regex::new(r#"<form\b[^>]*action="([^"]+)"[^>]*>([\s\S]*?)</form>"#)?;
    let field = regex::Regex::new(r#"name="_csrf"\s+value="([^"]+)""#)?;
    let mut preferences_count = 0usize;
    let mut action_token = None;
    for form in forms.captures_iter(page) {
        let route = form.get(1).context("form action missing")?.as_str();
        if route != action && route != "/preferences" {
            continue;
        }
        let body = form.get(2).context("form body missing")?.as_str();
        let token = field
            .captures(body)
            .and_then(|capture| capture.get(1))
            .context("CSRF field missing")?
            .as_str();
        ensure!(!validate_csrf(Some("another-browser"), token));
        ensure!(!validate_signed_csrf(
            Some(raw),
            Some("another-session"),
            token
        ));
        if route == action {
            ensure!(validate_signed_csrf(Some(raw), Some(scope), token));
            ensure!(!validate_csrf(Some(raw), token));
            ensure!(action_token.replace(token.to_owned()).is_none());
        } else {
            ensure!(validate_csrf(Some(raw), token));
            ensure!(!validate_signed_csrf(Some(raw), Some(scope), token));
            preferences_count = preferences_count.saturating_add(1);
        }
    }
    ensure!(
        preferences_count == 6,
        "expected JS and all five native preferences forms"
    );
    action_token.context("action form missing")
}

#[test]
fn native_forms_retain_same_origin_referrers() -> anyhow::Result<()> {
    let page = super::base_layout("Test", None, "", "", &[], None, None, false, "/");
    anyhow::ensure!(super::REFERRER_POLICY == "same-origin");
    anyhow::ensure!(page.contains("<meta name=\"referrer\" content=\"same-origin\">"));
    anyhow::ensure!(!page.contains("content=\"no-referrer\""));
    Ok(())
}
