//! Confirmed setup failures must be rejected before review or supplied with a separate secret.

use super::{
    hidden_form_fields, make_pending_hash_cookie, parse_setup_form, parse_setup_form_inner,
    pending_hash_from_cookie, setup_upload_maximum, SetupPasswordKind, SetupPreset,
    SetupWizardForm, MIB_I64,
};
use axum::http::HeaderMap;
use axum_extra::extract::cookie::{Cookie, CookieJar};

#[test]
fn setup_rejects_limits_that_cannot_be_saved_or_uploaded() -> anyhow::Result<()> {
    let mut form = SetupWizardForm::defaults_for(SetupPreset::Local);
    for (key, maximum) in [
        ("max_image_size_mb", 100),
        ("max_video_size_mb", 512),
        ("max_audio_size_mb", 512),
    ] {
        anyhow::ensure!(setup_upload_maximum(key) == maximum);
    }
    form.image_limit_mib = "100".to_owned();
    form.video_limit_mib = "512".to_owned();
    form.audio_limit_mib = "512".to_owned();
    form.pdf_limit_mib = "512".to_owned();
    let parsed = parse_setup_form(&form, 1).map_err(|errors| anyhow::anyhow!(errors.join(" ")))?;
    anyhow::ensure!(parsed.image_limit_bytes == 100 * MIB_I64);
    anyhow::ensure!(parsed.video_limit_bytes == 512 * MIB_I64);
    for field in ["image", "video", "audio", "pdf"] {
        let mut invalid = form.clone();
        match field {
            "image" => invalid.image_limit_mib = "101".to_owned(),
            "video" => invalid.video_limit_mib = "513".to_owned(),
            "audio" => invalid.audio_limit_mib = "513".to_owned(),
            _ => invalid.pdf_limit_mib = "513".to_owned(),
        }
        anyhow::ensure!(parse_setup_form(&invalid, 1).is_err());
    }
    form.allow_posting = None;
    anyhow::ensure!(parse_setup_form(&form, 1).is_err());
    Ok(())
}

#[test]
fn protected_setup_requires_its_own_authenticated_password_handoff() -> anyhow::Result<()> {
    let mut form = SetupWizardForm::defaults_for(SetupPreset::Private);
    anyhow::ensure!(parse_setup_form(&form, 1).is_err());
    form.board_password = Some("separate-board-fixture-password".to_owned());
    form.board_password_confirm = Some("mismatched-password".to_owned());
    anyhow::ensure!(parse_setup_form(&form, 1).is_err());
    form.board_password_confirm.clone_from(&form.board_password);
    let parsed = parse_setup_form(&form, 1).map_err(|errors| anyhow::anyhow!(errors.join(" ")))?;
    anyhow::ensure!(parsed.board_password == form.board_password);

    let context = crate::middleware::SecureCookieContext::new(None, false);
    let token = "opaque-board-review-token";
    let hash = "synthetic-password-hash";
    let cookie = make_pending_hash_cookie(
        SetupPasswordKind::Board,
        token,
        hash,
        &HeaderMap::new(),
        context,
    );
    anyhow::ensure!(cookie.http_only() == Some(true));
    let mut impersonated_admin = cookie.clone();
    impersonated_admin.set_name(SetupPasswordKind::Admin.cookie_name());
    let jar = CookieJar::new().add(cookie).add(impersonated_admin);
    anyhow::ensure!(
        pending_hash_from_cookie(SetupPasswordKind::Board, &jar, Some(token))?.as_deref()
            == Some(hash)
    );
    anyhow::ensure!(pending_hash_from_cookie(SetupPasswordKind::Admin, &jar, Some(token)).is_err());
    anyhow::ensure!(
        pending_hash_from_cookie(SetupPasswordKind::Board, &jar, Some("stale"))?.is_none()
    );
    let tampered = CookieJar::new().add(Cookie::new(
        SetupPasswordKind::Board.cookie_name(),
        format!("{token}:{}:invalid-signature", hex::encode(hash)),
    ));
    anyhow::ensure!(
        pending_hash_from_cookie(SetupPasswordKind::Board, &tampered, Some(token)).is_err()
    );

    form.board_password = None;
    form.board_password_confirm = None;
    form.board_password_token = Some(token.to_owned());
    anyhow::ensure!(parse_setup_form_inner(&form, 1, false, true).is_ok());
    anyhow::ensure!(parse_setup_form_inner(&form, 1, false, false).is_err());
    let review = hidden_form_fields("csrf-fixture", &form);
    anyhow::ensure!(review.contains(token));
    anyhow::ensure!(!review.contains(hash));
    anyhow::ensure!(!review.contains("separate-board-fixture-password"));
    Ok(())
}

#[test]
fn setup_preferences_keep_public_tokens_through_review_and_errors() -> anyhow::Result<()> {
    use crate::{db, middleware::SecureCookieContext, templates::tests::check_action_page_csrf};
    use anyhow::ensure;
    use axum::http::{header, HeaderValue};

    let context = SecureCookieContext::default();
    let mut headers = HeaderMap::new();
    drop(headers.insert(header::HOST, HeaderValue::from_static("example.test")));
    drop(headers.insert(
        header::ORIGIN,
        HeaderValue::from_static("http://example.test"),
    ));
    let (jar, csrf) = super::ensure_setup_csrf(CookieJar::new(), &headers, context);
    let raw = jar
        .get("csrf_token")
        .ok_or_else(|| anyhow::anyhow!("setup cookie missing"))?
        .value();
    let state = db::SetupState {
        access: db::SetupAccess::Reopened,
        admin_count: 1,
        board_count: 1,
        completed: true,
        reopened: true,
    };
    let form = SetupWizardForm::defaults_for(SetupPreset::Local);
    let parsed = parse_setup_form(&form, 1).map_err(|errors| anyhow::anyhow!(errors.join(" ")))?;
    let pages = [
        super::setup_form_page(&csrf, state, &form, &[], None, false, None),
        super::setup_form_page(
            &csrf,
            state,
            &form,
            &["Invalid setting".to_owned()],
            None,
            false,
            None,
        ),
        super::setup_review_page(&csrf, state, &form, &parsed, None),
    ];
    for (page, action) in pages
        .iter()
        .zip(["/setup/review", "/setup/review", "/setup/finish"])
    {
        let token = check_action_page_csrf(page, raw, super::SETUP_CSRF_SCOPE, action)?;
        super::validate_setup_csrf(&jar, &headers, context, Some(&token))?;
        ensure!(
            super::validate_setup_csrf(&jar, &headers, context, Some(&csrf.preferences)).is_err()
        );
        let another_browser = jar
            .clone()
            .add(Cookie::new("csrf_token", "another-browser"));
        ensure!(
            super::validate_setup_csrf(&another_browser, &headers, context, Some(&token)).is_err()
        );
        for invalid in [None, Some("malformed")] {
            ensure!(super::validate_setup_csrf(&jar, &headers, context, invalid).is_err());
        }
        for origin in [
            None,
            Some("http://foreign.test"),
            Some("http://example.test/path"),
        ] {
            let mut foreign = headers.clone();
            drop(foreign.remove(header::ORIGIN));
            if let Some(origin) = origin {
                drop(foreign.insert(header::ORIGIN, HeaderValue::from_static(origin)));
            }
            ensure!(super::validate_setup_csrf(&jar, &foreign, context, Some(&token)).is_err());
        }
    }
    Ok(())
}
