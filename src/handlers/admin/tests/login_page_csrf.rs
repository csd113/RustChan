//! Login and error pages keep action tokens out of shared preferences forms.

#[test]
fn login_preferences_use_public_tokens_from_the_login_cookie() -> anyhow::Result<()> {
    use super::{ensure_admin_login_csrf, ADMIN_LOGIN_CSRF_SCOPE};
    use axum::http::HeaderMap;
    use axum_extra::extract::cookie::{Cookie, CookieJar};

    for jar in [
        CookieJar::new(),
        CookieJar::new().add(Cookie::new("csrf_token", "existing-browser")),
    ] {
        let (jar, csrf) = ensure_admin_login_csrf(
            jar,
            &HeaderMap::new(),
            crate::middleware::SecureCookieContext::default(),
        );
        let raw = jar
            .get("csrf_token")
            .ok_or_else(|| anyhow::anyhow!("login cookie missing"))?
            .value();
        for error in [None, Some("Invalid username or password.")] {
            let page = crate::templates::admin_login_page(error, &csrf, &[], None);
            let token = crate::handlers::csrf_test_support::check_action_page_csrf(
                &page,
                raw,
                ADMIN_LOGIN_CSRF_SCOPE,
                "/admin/login",
            )?;
            anyhow::ensure!(token == csrf.action);
            anyhow::ensure!(!crate::middleware::validate_signed_csrf(
                Some(raw),
                Some("first-run-setup"),
                &token
            ));
            anyhow::ensure!(
                crate::handlers::board::check_csrf_jar(&jar, Some(&csrf.preferences)).is_ok()
            );
            anyhow::ensure!(crate::handlers::board::check_csrf_jar(&jar, Some(&token)).is_err());
        }
    }
    Ok(())
}
