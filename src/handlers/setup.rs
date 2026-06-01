#![allow(clippy::too_many_lines)]

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::AppState,
    templates,
    utils::{crypto, sanitize::escape_html},
};
use axum::{
    extract::{Form, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

const SETUP_CSRF_SCOPE: &str = "first-run-setup";
const SETUP_PENDING_ADMIN_HASH_COOKIE: &str = "setup_pending_admin_hash";
const MIB: u64 = 1024 * 1024;
const MIB_I64: i64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupPreset {
    Public,
    Private,
    Local,
}

impl SetupPreset {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Local => "local",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Public => "Public instance",
            Self::Private => "Private instance",
            Self::Local => "Local/testing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "preset defaults mirror independent setup toggles shown in the form"
)]
pub struct PresetDefaults {
    pub site_name: &'static str,
    pub board_slug: &'static str,
    pub board_name: &'static str,
    pub board_description: &'static str,
    pub board_visibility: &'static str,
    pub allow_uploads: bool,
    pub allow_video: bool,
    pub allow_audio: bool,
    pub allow_pdf: bool,
    pub allow_captcha: bool,
    pub post_cooldown_secs: i64,
    pub hide_nsfw_default: bool,
}

#[must_use]
pub const fn preset_defaults(preset: SetupPreset) -> PresetDefaults {
    match preset {
        SetupPreset::Public => PresetDefaults {
            site_name: "RustChan",
            board_slug: "b",
            board_name: "Random",
            board_description: "General discussion",
            board_visibility: "public",
            allow_uploads: true,
            allow_video: true,
            allow_audio: false,
            allow_pdf: false,
            allow_captcha: true,
            post_cooldown_secs: 10,
            hide_nsfw_default: false,
        },
        SetupPreset::Private => PresetDefaults {
            site_name: "Private RustChan",
            board_slug: "home",
            board_name: "Home",
            board_description: "Private board",
            board_visibility: "view_password",
            allow_uploads: true,
            allow_video: false,
            allow_audio: false,
            allow_pdf: false,
            allow_captcha: false,
            post_cooldown_secs: 0,
            hide_nsfw_default: true,
        },
        SetupPreset::Local => PresetDefaults {
            site_name: "Local RustChan",
            board_slug: "test",
            board_name: "Testing",
            board_description: "Local testing board",
            board_visibility: "public",
            allow_uploads: true,
            allow_video: true,
            allow_audio: true,
            allow_pdf: true,
            allow_captcha: false,
            post_cooldown_secs: 0,
            hide_nsfw_default: false,
        },
    }
}

fn parse_preset(value: &str) -> SetupPreset {
    match value.trim() {
        "private" => SetupPreset::Private,
        "local" => SetupPreset::Local,
        _ => SetupPreset::Public,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupWizardForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub preset: String,
    pub site_name: String,
    pub site_subtitle: Option<String>,
    pub default_theme: Option<String>,
    pub admin_username: Option<String>,
    pub admin_password: Option<String>,
    pub admin_password_confirm: Option<String>,
    pub admin_password_token: Option<String>,
    pub enable_tor: Option<String>,
    pub tor_only: Option<String>,
    pub public_url: Option<String>,
    pub https_cookies: Option<String>,
    pub behind_proxy: Option<String>,
    pub board_slug: String,
    pub board_name: String,
    pub board_description: Option<String>,
    pub board_nsfw: Option<String>,
    pub board_visibility: Option<String>,
    pub allow_posting: Option<String>,
    pub allow_uploads: Option<String>,
    pub allow_video: Option<String>,
    pub allow_audio: Option<String>,
    pub allow_pdf: Option<String>,
    pub allow_video_embeds: Option<String>,
    pub allow_thread_editing: Option<String>,
    pub allow_self_delete: Option<String>,
    pub allow_archive: Option<String>,
    pub image_limit_mib: String,
    pub video_limit_mib: String,
    pub audio_limit_mib: String,
    pub pdf_limit_mib: String,
    pub allow_captcha: Option<String>,
    pub captcha_type: Option<String>,
    pub post_cooldown_secs: Option<String>,
    pub homepage_new_thread_badges_enabled: Option<String>,
    pub homepage_new_reply_badges_enabled: Option<String>,
    pub thread_new_reply_badges_enabled: Option<String>,
    pub hide_nsfw_default: Option<String>,
    pub auto_backup_enabled: Option<String>,
    pub backup_retention: Option<String>,
    pub include_tor_keys_in_backups: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "parsed setup data carries independent persisted toggles from the review form"
)]
pub struct ParsedSetup {
    pub preset: SetupPreset,
    pub site_name: String,
    pub site_subtitle: String,
    pub default_theme: String,
    pub admin_username: Option<String>,
    pub admin_password: Option<String>,
    pub board_slug: String,
    pub board_name: String,
    pub board_description: String,
    pub board_nsfw: bool,
    pub board_visibility: String,
    pub allow_posting: bool,
    pub allow_uploads: bool,
    pub allow_video: bool,
    pub allow_audio: bool,
    pub allow_pdf: bool,
    pub allow_video_embeds: bool,
    pub allow_thread_editing: bool,
    pub allow_self_delete: bool,
    pub allow_archive: bool,
    pub image_limit_bytes: i64,
    pub video_limit_bytes: i64,
    pub audio_limit_bytes: i64,
    pub pdf_limit_bytes: i64,
    pub allow_captcha: bool,
    pub captcha_type: String,
    pub post_cooldown_secs: i64,
    pub homepage_new_thread_badges_enabled: bool,
    pub homepage_new_reply_badges_enabled: bool,
    pub thread_new_reply_badges_enabled: bool,
    pub hide_nsfw_default: bool,
    pub enable_tor: bool,
    pub tor_only: bool,
    pub public_url: String,
    pub https_cookies: bool,
    pub behind_proxy: bool,
    pub auto_backup_interval_hours: u64,
    pub backup_retention: u64,
    pub include_tor_keys_in_backups: bool,
}

#[must_use]
pub fn validate_board_slug(raw: &str) -> Option<String> {
    let slug = raw.trim().to_ascii_lowercase();
    let valid = !slug.is_empty()
        && slug.len() <= 8
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    valid.then_some(slug)
}

#[must_use]
pub fn parse_upload_limit_mib(raw: &str) -> Option<i64> {
    let mib = raw.trim().parse::<u64>().ok()?;
    if !(1..=4096).contains(&mib) {
        return None;
    }
    i64::try_from(mib.saturating_mul(MIB)).ok()
}

#[must_use]
pub fn validate_password_confirmation(password: &str, confirmation: &str) -> bool {
    password == confirmation && password.chars().count() >= 12 && password.chars().count() <= 1024
}

fn checkbox(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value, "1" | "true" | "on"))
}

fn checked(value: Option<&str>) -> &'static str {
    if checkbox(value) {
        " checked"
    } else {
        ""
    }
}

fn pending_admin_hash_signature(token: &str, password_hash_hex: &str) -> String {
    crypto::sha256_hex(
        format!(
            "{}:setup-admin-password:{token}:{password_hash_hex}",
            CONFIG.cookie_secret
        )
        .as_bytes(),
    )
}

fn make_pending_admin_hash_cookie(
    token: &str,
    password_hash: &str,
    headers: &HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> Cookie<'static> {
    let password_hash_hex = hex::encode(password_hash.as_bytes());
    let signature = pending_admin_hash_signature(token, &password_hash_hex);
    let mut cookie = Cookie::new(
        SETUP_PENDING_ADMIN_HASH_COOKIE,
        format!("{token}:{password_hash_hex}:{signature}"),
    );
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(crate::handlers::admin::should_set_secure_cookie(
        headers,
        secure_context,
    ));
    cookie
}

fn pending_admin_hash_from_cookie(jar: &CookieJar, token: Option<&str>) -> Result<Option<String>> {
    let Some(token) = token.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(cookie) = jar.get(SETUP_PENDING_ADMIN_HASH_COOKIE) else {
        return Ok(None);
    };
    let mut parts = cookie.value().splitn(3, ':');
    let Some(cookie_token) = parts.next() else {
        return Ok(None);
    };
    let Some(password_hash_hex) = parts.next() else {
        return Ok(None);
    };
    let Some(signature) = parts.next() else {
        return Ok(None);
    };
    if cookie_token != token {
        return Ok(None);
    }
    let expected = pending_admin_hash_signature(token, password_hash_hex);
    if signature != expected {
        return Err(AppError::Forbidden(
            "Setup admin secret token is invalid.".into(),
        ));
    }
    let password_hash_bytes = hex::decode(password_hash_hex)
        .map_err(|_error| AppError::BadRequest("Setup admin secret token is malformed.".into()))?;
    let password_hash = String::from_utf8(password_hash_bytes).map_err(|_error| {
        AppError::BadRequest("Setup admin secret token is not valid UTF-8.".into())
    })?;
    Ok(Some(password_hash))
}

fn trimmed_limited(value: Option<&str>, max_chars: usize) -> String {
    value
        .unwrap_or_default()
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

pub fn parse_setup_form(
    form: &SetupWizardForm,
    admin_count: i64,
) -> std::result::Result<ParsedSetup, Vec<String>> {
    parse_setup_form_inner(form, admin_count, false)
}

fn parse_setup_form_inner(
    form: &SetupWizardForm,
    admin_count: i64,
    admin_secret_available: bool,
) -> std::result::Result<ParsedSetup, Vec<String>> {
    let mut errors = Vec::new();
    let preset = parse_preset(&form.preset);
    let defaults = preset_defaults(preset);
    let site_name = form.site_name.trim().chars().take(64).collect::<String>();
    if site_name.is_empty() {
        errors.push("Site name is required.".to_owned());
    }
    let admin_username = if admin_count == 0 {
        let username = trimmed_limited(form.admin_username.as_deref(), 32);
        let username_valid = (3..=32).contains(&username.len())
            && username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !username_valid {
            errors.push(
                "Admin username must be 3-32 ASCII letters, numbers, underscores, or dashes."
                    .to_owned(),
            );
        }
        let password = form.admin_password.as_deref().unwrap_or_default();
        let confirmation = form.admin_password_confirm.as_deref().unwrap_or_default();
        if !admin_secret_available && !validate_password_confirmation(password, confirmation) {
            errors.push(
                "Admin password must be at least 12 characters and match confirmation.".to_owned(),
            );
        }
        Some(username)
    } else {
        None
    };
    let admin_password = if admin_count == 0 {
        form.admin_password.clone()
    } else {
        None
    };
    let board_slug = validate_board_slug(&form.board_slug).unwrap_or_else(|| {
        errors.push("Board slug must be 1-8 lowercase letters or digits.".to_owned());
        String::new()
    });
    let board_name = form.board_name.trim().chars().take(64).collect::<String>();
    if board_name.is_empty() {
        errors.push("Board name is required.".to_owned());
    }
    let board_visibility = match form
        .board_visibility
        .as_deref()
        .unwrap_or(defaults.board_visibility)
    {
        "public" | "view_password" | "post_password" => form
            .board_visibility
            .clone()
            .unwrap_or_else(|| defaults.board_visibility.to_owned()),
        _ => {
            errors.push("Board visibility mode is invalid.".to_owned());
            "public".to_owned()
        }
    };
    let image_limit_bytes = parse_upload_limit_mib(&form.image_limit_mib);
    let video_limit_bytes = parse_upload_limit_mib(&form.video_limit_mib);
    let audio_limit_bytes = parse_upload_limit_mib(&form.audio_limit_mib);
    let pdf_limit_bytes = parse_upload_limit_mib(&form.pdf_limit_mib);
    if image_limit_bytes.is_none()
        || video_limit_bytes.is_none()
        || audio_limit_bytes.is_none()
        || pdf_limit_bytes.is_none()
    {
        errors.push("Upload limits must be whole MiB values from 1 through 4096.".to_owned());
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let post_cooldown_secs = form
        .post_cooldown_secs
        .as_deref()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(defaults.post_cooldown_secs)
        .clamp(0, 3600);
    let backup_retention = form
        .backup_retention
        .as_deref()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1)
        .clamp(1, 1000);
    Ok(ParsedSetup {
        preset,
        site_name,
        site_subtitle: trimmed_limited(form.site_subtitle.as_deref(), 128),
        default_theme: form
            .default_theme
            .as_deref()
            .map(db::sanitize_theme_slug)
            .filter(|theme| !theme.is_empty())
            .unwrap_or_else(|| crate::theme::HARD_DEFAULT_THEME.to_owned()),
        admin_username,
        admin_password,
        board_slug,
        board_name,
        board_description: trimmed_limited(form.board_description.as_deref(), 256),
        board_nsfw: checkbox(form.board_nsfw.as_deref()),
        board_visibility,
        allow_posting: checkbox(form.allow_posting.as_deref()),
        allow_uploads: checkbox(form.allow_uploads.as_deref()),
        allow_video: checkbox(form.allow_video.as_deref()),
        allow_audio: checkbox(form.allow_audio.as_deref()),
        allow_pdf: checkbox(form.allow_pdf.as_deref()),
        allow_video_embeds: checkbox(form.allow_video_embeds.as_deref()),
        allow_thread_editing: checkbox(form.allow_thread_editing.as_deref()),
        allow_self_delete: checkbox(form.allow_self_delete.as_deref()),
        allow_archive: checkbox(form.allow_archive.as_deref()),
        image_limit_bytes: image_limit_bytes.unwrap_or(8 * MIB_I64),
        video_limit_bytes: video_limit_bytes.unwrap_or(50 * MIB_I64),
        audio_limit_bytes: audio_limit_bytes.unwrap_or(150 * MIB_I64),
        pdf_limit_bytes: pdf_limit_bytes.unwrap_or(8 * MIB_I64),
        allow_captcha: checkbox(form.allow_captcha.as_deref()),
        captcha_type: form
            .captcha_type
            .as_deref()
            .filter(|value| matches!(*value, "builtin" | "disabled"))
            .unwrap_or("builtin")
            .to_owned(),
        post_cooldown_secs,
        homepage_new_thread_badges_enabled: checkbox(
            form.homepage_new_thread_badges_enabled.as_deref(),
        ),
        homepage_new_reply_badges_enabled: checkbox(
            form.homepage_new_reply_badges_enabled.as_deref(),
        ),
        thread_new_reply_badges_enabled: checkbox(form.thread_new_reply_badges_enabled.as_deref()),
        hide_nsfw_default: checkbox(form.hide_nsfw_default.as_deref()),
        enable_tor: checkbox(form.enable_tor.as_deref()),
        tor_only: checkbox(form.tor_only.as_deref()),
        public_url: trimmed_limited(form.public_url.as_deref(), 256),
        https_cookies: checkbox(form.https_cookies.as_deref()),
        behind_proxy: checkbox(form.behind_proxy.as_deref()),
        auto_backup_interval_hours: if checkbox(form.auto_backup_enabled.as_deref()) {
            24
        } else {
            0
        },
        backup_retention,
        include_tor_keys_in_backups: checkbox(form.include_tor_keys_in_backups.as_deref()),
    })
}

fn ensure_setup_csrf(
    jar: CookieJar,
    headers: &HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> (CookieJar, String) {
    let token = jar
        .get("csrf_token")
        .map(|cookie| cookie.value().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(crypto::new_csrf_token);
    let mut cookie = Cookie::new("csrf_token", token.clone());
    cookie.set_http_only(false);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(crate::handlers::admin::should_set_secure_cookie(
        headers,
        secure_context,
    ));
    (
        jar.add(cookie),
        crypto::make_scoped_csrf_form_token(&token, &CONFIG.cookie_secret, SETUP_CSRF_SCOPE),
    )
}

fn validate_setup_csrf(
    jar: &CookieJar,
    headers: &HeaderMap,
    peer: Option<std::net::SocketAddr>,
    token: Option<&str>,
) -> Result<()> {
    let csrf_cookie = jar.get("csrf_token").map(Cookie::value);
    let csrf_valid = crate::middleware::validate_signed_csrf(
        csrf_cookie,
        Some(SETUP_CSRF_SCOPE),
        token.unwrap_or(""),
    );
    crate::handlers::admin::require_same_origin_or_valid_csrf(headers, peer, csrf_valid)?;
    if csrf_valid {
        Ok(())
    } else {
        Err(AppError::Forbidden("CSRF token mismatch.".into()))
    }
}

fn admin_session_id(jar: &CookieJar) -> Option<String> {
    jar.get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned())
}

async fn load_setup_state(
    state: &AppState,
    session_id: Option<String>,
) -> Result<(db::SetupState, bool)> {
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(db::SetupState, bool)> {
            let conn = pool.get()?;
            let setup_state = db::setup_state(&conn)?;
            if !setup_state.is_available() {
                let message = if setup_state.completed {
                    "Setup is already complete."
                } else {
                    "Setup wizard is not available."
                };
                return Err(AppError::NotFound(message.into()));
            }
            let is_admin = session_id
                .as_deref()
                .is_some_and(|sid| db::get_session(&conn, sid).ok().flatten().is_some());
            if setup_state.requires_admin_auth() && !is_admin {
                return Err(AppError::Forbidden(
                    "Current admin authentication is required to reopen setup.".into(),
                ));
            }
            Ok((setup_state, is_admin))
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))?
}

pub async fn setup_get(
    State(state): State<AppState>,
    Query(query): Query<SetupQuery>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
) -> Result<Response> {
    let (setup_state, _is_admin) = load_setup_state(&state, admin_session_id(&jar)).await?;
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_setup_csrf(jar, &headers, secure_context);
    let form =
        SetupWizardForm::defaults_for(parse_preset(query.preset.as_deref().unwrap_or("public")));
    let body = setup_form_page(
        &csrf,
        setup_state,
        &form,
        &[],
        request_transport_warning(&headers, secure_context).as_deref(),
        &state,
        current_theme.as_deref(),
    );
    Ok((jar, Html(body)).into_response())
}

#[derive(Deserialize)]
pub struct SetupQuery {
    preset: Option<String>,
}

pub async fn setup_review(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<SetupWizardForm>,
) -> Result<Response> {
    let (setup_state, _is_admin) = load_setup_state(&state, admin_session_id(&jar)).await?;
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    validate_setup_csrf(&jar, &headers, secure_context.peer, form.csrf.as_deref())?;
    let parsed = match parse_setup_form(&form, setup_state.admin_count) {
        Ok(parsed) => parsed,
        Err(errors) => {
            let (jar, csrf) = ensure_setup_csrf(jar, &headers, secure_context);
            return Ok((
                jar,
                (
                    StatusCode::BAD_REQUEST,
                    Html(setup_form_page(
                        &csrf,
                        setup_state,
                        &form,
                        &errors,
                        request_transport_warning(&headers, secure_context).as_deref(),
                        &state,
                        current_theme.as_deref(),
                    )),
                ),
            )
                .into_response());
        }
    };
    let (mut jar, csrf) = ensure_setup_csrf(jar, &headers, secure_context);
    let mut review_form = form.clone();
    if setup_state.admin_count == 0 {
        let password = parsed
            .admin_password
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("Initial admin password is required.".into()))?;
        let password_hash = crypto::hash_password(password)?;
        let token = crypto::random_hex(32);
        jar = jar.add(make_pending_admin_hash_cookie(
            &token,
            &password_hash,
            &headers,
            secure_context,
        ));
        review_form.admin_password = None;
        review_form.admin_password_confirm = None;
        review_form.admin_password_token = Some(token);
    }
    Ok((
        jar,
        Html(setup_review_page(
            &csrf,
            setup_state,
            &review_form,
            &parsed,
            current_theme.as_deref(),
        )),
    )
        .into_response())
}

pub async fn setup_finish(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<SetupWizardForm>,
) -> Result<Response> {
    let (setup_state, _is_admin) = load_setup_state(&state, admin_session_id(&jar)).await?;
    validate_setup_csrf(&jar, &headers, secure_context.peer, form.csrf.as_deref())?;
    let pending_admin_hash =
        pending_admin_hash_from_cookie(&jar, form.admin_password_token.as_deref())?;
    let parsed =
        parse_setup_form_inner(&form, setup_state.admin_count, pending_admin_hash.is_some())
            .map_err(|errors| AppError::BadRequest(errors.join(" ")))?;
    let board_slug = parsed.board_slug.clone();
    let auto_backup_settings = state.auto_full_backup_settings.clone();
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let mut conn = pool.get()?;
            let tx = conn.transaction()?;
            db::ensure_setup_available(&tx)
                .map_err(|_| AppError::NotFound("Setup wizard is not available.".into()))?;
            if db::admin_count(&tx)? == 0 {
                let username = parsed.admin_username.as_deref().ok_or_else(|| {
                    AppError::BadRequest("Initial admin username is required.".into())
                })?;
                let password_hash = if let Some(hash) = pending_admin_hash.as_deref() {
                    hash.to_owned()
                } else {
                    let password = parsed.admin_password.as_deref().ok_or_else(|| {
                        AppError::BadRequest("Initial admin password is required.".into())
                    })?;
                    crypto::hash_password(password)?
                };
                db::create_admin(&tx, username, &password_hash)?;
            }
            if db::board_slug_exists(&tx, &parsed.board_slug)? {
                return Err(AppError::Conflict(format!(
                    "Board /{}/ already exists.",
                    parsed.board_slug
                )));
            }
            let board_id = db::create_board_with_media_flags(
                &tx,
                &parsed.board_slug,
                &parsed.board_name,
                &parsed.board_description,
                parsed.board_nsfw,
                parsed.allow_uploads,
                parsed.allow_uploads && parsed.allow_video,
                parsed.allow_uploads && parsed.allow_audio,
            )?;
            tx.execute(
                "UPDATE boards SET
                 max_threads = ?1, max_archived_threads = ?2, bump_limit = ?3,
                 max_image_size = ?4, max_video_size = ?5, max_audio_size = ?6,
                 max_pdf_size = ?7, allow_pdf = ?8, allow_any_files = 0, allow_tripcodes = 1,
                 edit_window_secs = 0, allow_editing = ?9, allow_self_delete = ?10,
                 allow_archive = ?11, allow_video_embeds = ?12, allow_captcha = ?13,
                 show_poster_ids = 1, collapse_greentext = 0, post_cooldown_secs = ?14,
                 default_theme = ?15, banner_mode = 'inherit', access_mode = ?16,
                 access_password_hash = ''
                 WHERE id = ?17",
                rusqlite::params![
                    if parsed.allow_posting { 150 } else { 0 },
                    150,
                    500,
                    parsed.image_limit_bytes,
                    parsed.video_limit_bytes,
                    parsed.audio_limit_bytes,
                    parsed.pdf_limit_bytes,
                    i32::from(parsed.allow_uploads && parsed.allow_pdf),
                    i32::from(parsed.allow_thread_editing),
                    i32::from(parsed.allow_self_delete),
                    i32::from(parsed.allow_archive),
                    i32::from(parsed.allow_video_embeds),
                    i32::from(parsed.allow_captcha && parsed.captcha_type == "builtin"),
                    parsed.post_cooldown_secs,
                    parsed.default_theme,
                    parsed.board_visibility,
                    board_id,
                ],
            )?;
            db::set_site_setting(&tx, "site_name", &parsed.site_name)?;
            db::set_site_setting(&tx, "site_subtitle", &parsed.site_subtitle)?;
            db::set_site_setting(&tx, "default_theme", &parsed.default_theme)?;
            db::set_site_setting(
                &tx,
                "homepage_new_thread_badges_enabled",
                if parsed.homepage_new_thread_badges_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            db::set_site_setting(
                &tx,
                "homepage_new_reply_badges_enabled",
                if parsed.homepage_new_reply_badges_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            db::set_site_setting(
                &tx,
                "thread_new_reply_badges_enabled",
                if parsed.thread_new_reply_badges_enabled {
                    "1"
                } else {
                    "0"
                },
            )?;
            db::set_site_setting(
                &tx,
                "default_hide_nsfw_boards",
                if parsed.hide_nsfw_default { "1" } else { "0" },
            )?;
            db::set_site_setting(&tx, "setup_public_url", &parsed.public_url)?;
            db::set_site_setting(
                &tx,
                "setup_backup_destination",
                &crate::config::full_backups_dir().display().to_string(),
            )?;
            db::set_site_setting(
                &tx,
                "setup_pdf_upload_limit_bytes",
                &parsed.pdf_limit_bytes.to_string(),
            )?;
            crate::config::update_settings_file_setup(&crate::config::SetupSettingsFileUpdate {
                forum_name: &parsed.site_name,
                site_subtitle: &parsed.site_subtitle,
                homepage_new_thread_badges_enabled: parsed.homepage_new_thread_badges_enabled,
                homepage_new_reply_badges_enabled: parsed.homepage_new_reply_badges_enabled,
                thread_new_reply_badges_enabled: parsed.thread_new_reply_badges_enabled,
                default_theme: &parsed.default_theme,
                auto_full_backup_interval_hours: parsed.auto_backup_interval_hours,
                auto_full_backup_copies_to_keep: parsed.backup_retention,
                auto_full_backup_include_tor_hidden_service_keys: parsed
                    .include_tor_keys_in_backups,
                auto_full_backup_storage_mode: "directory",
                auto_full_backup_split_zip_part_size_gib:
                    crate::handlers::admin::backup::split_zip_part_size_gib(
                        CONFIG.auto_full_backup_split_zip_part_size_bytes,
                    ),
                runtime: crate::config::SetupRuntimeSettingsUpdate {
                    enable_tor_support: parsed.enable_tor,
                    tor_only: parsed.tor_only,
                    behind_proxy: parsed.behind_proxy,
                    https_cookies: parsed.https_cookies,
                    max_image_size_mb: u64::try_from(parsed.image_limit_bytes / MIB_I64)
                        .unwrap_or(8),
                    max_video_size_mb: u64::try_from(parsed.video_limit_bytes / MIB_I64)
                        .unwrap_or(50),
                    max_audio_size_mb: u64::try_from(parsed.audio_limit_bytes / MIB_I64)
                        .unwrap_or(150),
                },
            })?;
            db::mark_setup_complete(&tx)?;
            tx.commit()?;

            templates::set_live_site_name(&parsed.site_name);
            templates::set_live_site_subtitle(&parsed.site_subtitle);
            db::sync_live_theme_state(&conn)?;
            templates::set_live_boards(db::get_all_boards(&conn)?);
            auto_backup_settings.update(
                parsed.auto_backup_interval_hours,
                parsed.backup_retention,
                parsed.include_tor_keys_in_backups,
                "directory",
                CONFIG.auto_full_backup_split_zip_part_size_bytes,
            );
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    let jar = jar.remove(Cookie::from(SETUP_PENDING_ADMIN_HASH_COOKIE));
    Ok((jar, Redirect::to(&format!("/{board_slug}"))).into_response())
}

#[derive(Deserialize)]
pub struct ReopenSetupForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn admin_reopen_setup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ReopenSetupForm>,
) -> Result<Response> {
    let session_id = admin_session_id(&jar);
    crate::handlers::admin::require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.csrf.as_deref(),
    )?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let admin_id =
                crate::handlers::admin::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::reopen_setup(&conn, admin_id)?;
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;
    Ok(Redirect::to("/setup").into_response())
}

pub async fn admin_close_setup(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ReopenSetupForm>,
) -> Result<Response> {
    let session_id = admin_session_id(&jar);
    crate::handlers::admin::require_admin_post_origin_and_csrf(
        &jar,
        &headers,
        Some(peer),
        form.csrf.as_deref(),
    )?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            crate::handlers::admin::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::close_reopened_setup(&conn)?;
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;
    Ok(Redirect::to(
        "/admin/panel?flash=Setup+wizard+closed.&open=database-maintenance#database-maintenance",
    )
    .into_response())
}

impl SetupWizardForm {
    #[must_use]
    pub fn defaults_for(preset: SetupPreset) -> Self {
        let defaults = preset_defaults(preset);
        Self {
            csrf: None,
            preset: preset.as_str().to_owned(),
            site_name: defaults.site_name.to_owned(),
            site_subtitle: Some("select board to proceed".to_owned()),
            default_theme: Some(crate::theme::HARD_DEFAULT_THEME.to_owned()),
            admin_username: Some("admin".to_owned()),
            admin_password: None,
            admin_password_confirm: None,
            admin_password_token: None,
            enable_tor: Some("1".to_owned()),
            tor_only: None,
            public_url: None,
            https_cookies: None,
            behind_proxy: None,
            board_slug: defaults.board_slug.to_owned(),
            board_name: defaults.board_name.to_owned(),
            board_description: Some(defaults.board_description.to_owned()),
            board_nsfw: None,
            board_visibility: Some(defaults.board_visibility.to_owned()),
            allow_posting: Some("1".to_owned()),
            allow_uploads: defaults.allow_uploads.then(|| "1".to_owned()),
            allow_video: defaults.allow_video.then(|| "1".to_owned()),
            allow_audio: defaults.allow_audio.then(|| "1".to_owned()),
            allow_pdf: defaults.allow_pdf.then(|| "1".to_owned()),
            allow_video_embeds: Some("1".to_owned()),
            allow_thread_editing: Some("1".to_owned()),
            allow_self_delete: Some("1".to_owned()),
            allow_archive: Some("1".to_owned()),
            image_limit_mib: "8".to_owned(),
            video_limit_mib: "50".to_owned(),
            audio_limit_mib: "150".to_owned(),
            pdf_limit_mib: "8".to_owned(),
            allow_captcha: defaults.allow_captcha.then(|| "1".to_owned()),
            captcha_type: Some("builtin".to_owned()),
            post_cooldown_secs: Some(defaults.post_cooldown_secs.to_string()),
            homepage_new_thread_badges_enabled: Some("1".to_owned()),
            homepage_new_reply_badges_enabled: Some("1".to_owned()),
            thread_new_reply_badges_enabled: Some("1".to_owned()),
            hide_nsfw_default: defaults.hide_nsfw_default.then(|| "1".to_owned()),
            auto_backup_enabled: None,
            backup_retention: Some("1".to_owned()),
            include_tor_keys_in_backups: None,
        }
    }
}

fn request_transport_warning(
    headers: &HeaderMap,
    context: crate::middleware::SecureCookieContext,
) -> Option<String> {
    let secure_now = crate::handlers::admin::should_set_secure_cookie(headers, context);
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("this host");
    if CONFIG.https_cookies
        && !secure_now
        && !host.starts_with("localhost")
        && !host.starts_with("127.0.0.1")
    {
        Some("Secure cookies are enabled in configuration, but this request is not arriving as HTTPS. Admin login may fail until TLS or trusted proxy headers are configured.".to_owned())
    } else {
        None
    }
}

fn setup_form_page(
    csrf: &str,
    state: db::SetupState,
    form: &SetupWizardForm,
    errors: &[String],
    transport_warning: Option<&str>,
    app_state: &AppState,
    current_theme: Option<&str>,
) -> String {
    let mut alerts = String::new();
    if state.requires_admin_auth() {
        alerts.push_str(r#"<div class="setup-alert">Setup was reopened by an admin. Existing admin credentials will not be replaced.</div>"#);
    }
    if let Some(warning) = transport_warning {
        let _ = write!(
            alerts,
            r#"<div class="setup-alert setup-alert-warn">{}</div>"#,
            escape_html(warning)
        );
    }
    for error in errors {
        let _ = write!(
            alerts,
            r#"<div class="error" role="alert">{}</div>"#,
            escape_html(error)
        );
    }
    let admin_fields = if state.admin_count == 0 {
        r#"<section class="setup-section">
<h2>3. Admin account</h2>
<div class="setup-grid">
<label>Username <input name="admin_username" value="admin" required autocomplete="username"></label>
<label>Password <input type="password" name="admin_password" required autocomplete="new-password" minlength="12"></label>
<label>Confirm password <input type="password" name="admin_password_confirm" required autocomplete="new-password" minlength="12"></label>
</div>
</section>"#
            .to_owned()
    } else {
        r#"<section class="setup-section"><h2>3. Admin account</h2><p class="admin-copy">An admin account already exists. Continue as the currently authenticated admin; this wizard will not replace credentials.</p></section>"#.to_owned()
    };
    let ffmpeg = if app_state.ffmpeg_available {
        "detected"
    } else {
        "missing"
    };
    let ffprobe = if app_state.ffprobe_available {
        "detected"
    } else {
        "missing"
    };
    let body = format!(
        r#"<main class="setup-wizard">
<div class="setup-head">
<h1>RustChan setup</h1>
<p>Configure this local runtime before opening the instance to users. All controls work without JavaScript.</p>
</div>
{alerts}
<form method="POST" action="/setup/review">
<input type="hidden" name="_csrf" value="{csrf}">
<section class="setup-section"><h2>1. Instance mode</h2><div class="setup-choice-row">{preset_options}</div></section>
<section class="setup-section"><h2>2. Site basics</h2><div class="setup-grid">
<label>Site name <input name="site_name" value="{site_name}" maxlength="64" required></label>
<label>Subtitle <input name="site_subtitle" value="{site_subtitle}" maxlength="128"></label>
<label>Default theme <input name="default_theme" value="{default_theme}" maxlength="32"></label>
</div><p class="admin-copy">About, rules, and contact links can be added later from supported site surfaces.</p></section>
{admin_fields}
<section class="setup-section"><h2>4. Network / Tor</h2><div class="setup-grid">
<label>Public URL <input name="public_url" value="{public_url}" placeholder="https://example.com"></label>
<label class="setup-check"><input type="checkbox" name="enable_tor" value="1"{enable_tor}> Enable Tor/onion service</label>
<label class="setup-check"><input type="checkbox" name="tor_only" value="1"{tor_only}> Tor-only loopback binding after restart</label>
<label class="setup-check"><input type="checkbox" name="https_cookies" value="1"{https_cookies}> Use Secure cookies when request transport is HTTPS</label>
<label class="setup-check"><input type="checkbox" name="behind_proxy" value="1"{behind_proxy}> Instance is behind a trusted HTTPS proxy</label>
</div></section>
<section class="setup-section"><h2>5. Default board</h2><div class="setup-grid">
<label>Slug <input name="board_slug" value="{board_slug}" maxlength="8" required></label>
<label>Name <input name="board_name" value="{board_name}" maxlength="64" required></label>
<label>Description <input name="board_description" value="{board_description}" maxlength="256"></label>
<label>Visibility <select name="board_visibility">{visibility_options}</select></label>
<label class="setup-check"><input type="checkbox" name="allow_posting" value="1"{allow_posting}> Allow posting</label>
<label class="setup-check"><input type="checkbox" name="board_nsfw" value="1"{board_nsfw}> Mark board NSFW</label>
<label class="setup-check"><input type="checkbox" name="allow_thread_editing" value="1"{allow_thread_editing}> Allow post editing</label>
<label class="setup-check"><input type="checkbox" name="allow_self_delete" value="1"{allow_self_delete}> Allow self-delete</label>
<label class="setup-check"><input type="checkbox" name="allow_archive" value="1"{allow_archive}> Archive overflow threads</label>
</div></section>
<section class="setup-section"><h2>6. Uploads and media</h2>
<p class="admin-copy">ffmpeg: <strong>{ffmpeg}</strong>; ffprobe: <strong>{ffprobe}</strong>. PDF uploads use the PDF limit shown here; other file types use their matching media limits.</p>
<div class="setup-grid">
<label class="setup-check"><input type="checkbox" name="allow_uploads" value="1"{allow_uploads}> Allow image uploads</label>
<label class="setup-check"><input type="checkbox" name="allow_video" value="1"{allow_video}> Allow video uploads</label>
<label class="setup-check"><input type="checkbox" name="allow_audio" value="1"{allow_audio}> Allow audio uploads</label>
<label class="setup-check"><input type="checkbox" name="allow_pdf" value="1"{allow_pdf}> Allow PDF uploads</label>
<label>Image limit (MiB) <input type="number" name="image_limit_mib" value="{image_limit_mib}" min="1" max="4096" required></label>
<label>Video limit (MiB) <input type="number" name="video_limit_mib" value="{video_limit_mib}" min="1" max="4096" required></label>
<label>Audio limit (MiB) <input type="number" name="audio_limit_mib" value="{audio_limit_mib}" min="1" max="4096" required></label>
<label>PDF limit (MiB) <input type="number" name="pdf_limit_mib" value="{pdf_limit_mib}" min="1" max="4096" required></label>
<label class="setup-check"><input type="checkbox" name="allow_video_embeds" value="1"{allow_video_embeds}> Allow video embeds</label>
</div></section>
<section class="setup-section"><h2>7. Anti-spam and privacy</h2><div class="setup-grid">
<label class="setup-check"><input type="checkbox" name="allow_captcha" value="1"{allow_captcha}> Enable CAPTCHA on default board</label>
<label>CAPTCHA type <select name="captcha_type"><option value="builtin">Built-in</option><option value="disabled">Disabled</option></select></label>
<label>Posting cooldown seconds <input type="number" name="post_cooldown_secs" value="{post_cooldown_secs}" min="0" max="3600"></label>
<label class="setup-check"><input type="checkbox" name="homepage_new_thread_badges_enabled" value="1"{homepage_new_thread_badges_enabled}> Homepage new-thread badges</label>
<label class="setup-check"><input type="checkbox" name="homepage_new_reply_badges_enabled" value="1"{homepage_new_reply_badges_enabled}> Homepage new-reply badges</label>
<label class="setup-check"><input type="checkbox" name="thread_new_reply_badges_enabled" value="1"{thread_new_reply_badges_enabled}> Thread new-reply badges</label>
<label class="setup-check"><input type="checkbox" name="hide_nsfw_default" value="1"{hide_nsfw_default}> Hide NSFW boards by default</label>
</div></section>
<section class="setup-section"><h2>8. Backups</h2>
<p class="admin-copy">Default destination: <strong>{backup_dir}</strong>. Automatic backups stay disabled unless enabled here.</p>
<div class="setup-grid">
<label class="setup-check"><input type="checkbox" name="auto_backup_enabled" value="1"{auto_backup_enabled}> Enable automatic full backups every 24 hours</label>
<label>Retention count <input type="number" name="backup_retention" value="{backup_retention}" min="1" max="1000"></label>
<label class="setup-check"><input type="checkbox" name="include_tor_keys_in_backups" value="1"{include_tor_keys_in_backups}> Include Tor hidden-service keys in automatic backups</label>
</div><p class="admin-copy">Tor keys are excluded by default. Include them only if a backup should restore the same onion identity.</p></section>
<section class="setup-section"><h2>9. Review and finish</h2>
<p class="admin-copy">Review the next page before anything is written. Setup is marked complete only after all database writes succeed.</p>
<button type="submit">review setup</button>
</section>
</form>
</main>"#,
        alerts = alerts,
        csrf = escape_html(csrf),
        preset_options = preset_options(&form.preset),
        site_name = escape_html(&form.site_name),
        site_subtitle = escape_html(form.site_subtitle.as_deref().unwrap_or_default()),
        default_theme = escape_html(
            form.default_theme
                .as_deref()
                .unwrap_or(crate::theme::HARD_DEFAULT_THEME),
        ),
        admin_fields = admin_fields,
        public_url = escape_html(form.public_url.as_deref().unwrap_or_default()),
        enable_tor = checked(form.enable_tor.as_deref()),
        tor_only = checked(form.tor_only.as_deref()),
        https_cookies = checked(form.https_cookies.as_deref()),
        behind_proxy = checked(form.behind_proxy.as_deref()),
        board_slug = escape_html(&form.board_slug),
        board_name = escape_html(&form.board_name),
        board_description = escape_html(form.board_description.as_deref().unwrap_or_default()),
        visibility_options =
            visibility_options(form.board_visibility.as_deref().unwrap_or("public")),
        allow_posting = checked(form.allow_posting.as_deref()),
        board_nsfw = checked(form.board_nsfw.as_deref()),
        allow_thread_editing = checked(form.allow_thread_editing.as_deref()),
        allow_self_delete = checked(form.allow_self_delete.as_deref()),
        allow_archive = checked(form.allow_archive.as_deref()),
        ffmpeg = ffmpeg,
        ffprobe = ffprobe,
        allow_uploads = checked(form.allow_uploads.as_deref()),
        allow_video = checked(form.allow_video.as_deref()),
        allow_audio = checked(form.allow_audio.as_deref()),
        allow_pdf = checked(form.allow_pdf.as_deref()),
        allow_video_embeds = checked(form.allow_video_embeds.as_deref()),
        image_limit_mib = escape_html(&form.image_limit_mib),
        video_limit_mib = escape_html(&form.video_limit_mib),
        audio_limit_mib = escape_html(&form.audio_limit_mib),
        pdf_limit_mib = escape_html(&form.pdf_limit_mib),
        allow_captcha = checked(form.allow_captcha.as_deref()),
        post_cooldown_secs = escape_html(form.post_cooldown_secs.as_deref().unwrap_or("0")),
        homepage_new_thread_badges_enabled =
            checked(form.homepage_new_thread_badges_enabled.as_deref()),
        homepage_new_reply_badges_enabled =
            checked(form.homepage_new_reply_badges_enabled.as_deref()),
        thread_new_reply_badges_enabled = checked(form.thread_new_reply_badges_enabled.as_deref()),
        hide_nsfw_default = checked(form.hide_nsfw_default.as_deref()),
        backup_dir = escape_html(&crate::config::full_backups_dir().display().to_string()),
        auto_backup_enabled = checked(form.auto_backup_enabled.as_deref()),
        backup_retention = escape_html(form.backup_retention.as_deref().unwrap_or("1")),
        include_tor_keys_in_backups = checked(form.include_tor_keys_in_backups.as_deref()),
    );
    let boards = templates::live_boards();
    templates::base_layout(
        "setup",
        None,
        &body,
        csrf,
        boards.as_ref(),
        current_theme,
        form.default_theme.as_deref(),
        false,
        "/setup",
    )
}

fn setup_review_page(
    csrf: &str,
    state: db::SetupState,
    form: &SetupWizardForm,
    parsed: &ParsedSetup,
    current_theme: Option<&str>,
) -> String {
    let hidden = hidden_form_fields(csrf, form);
    let admin_line = if state.admin_count == 0 {
        format!(
            "Create initial admin account: {}",
            escape_html(parsed.admin_username.as_deref().unwrap_or("admin"))
        )
    } else {
        "Keep existing admin credentials; current admin authorization required.".to_owned()
    };
    let body = format!(
        r#"<main class="setup-wizard">
<div class="setup-head"><h1>Review setup</h1><p>Confirm these settings before RustChan writes setup state.</p></div>
<section class="setup-section">
<h2>Summary</h2>
<dl class="setup-review-list">
<dt>Preset</dt><dd>{preset}</dd>
<dt>Site</dt><dd>{site}</dd>
<dt>Admin</dt><dd>{admin}</dd>
<dt>Board</dt><dd>/{slug}/ - {board}</dd>
<dt>Uploads</dt><dd>image {image} MiB, video {video} MiB, audio {audio} MiB, PDF {pdf} MiB</dd>
<dt>Network</dt><dd>Tor {tor}; HTTPS cookies {https_cookies}; proxy {proxy}</dd>
<dt>Backups</dt><dd>{backup}</dd>
</dl>
<form method="POST" action="/setup/finish">
{hidden}
<button type="submit">finish setup</button>
<a class="button-secondary" href="/setup">edit settings</a>
</form>
</section>
</main>"#,
        preset = parsed.preset.label(),
        site = escape_html(&parsed.site_name),
        admin = admin_line,
        slug = escape_html(&parsed.board_slug),
        board = escape_html(&parsed.board_name),
        image = parsed.image_limit_bytes / i64::try_from(MIB).unwrap_or(1),
        video = parsed.video_limit_bytes / i64::try_from(MIB).unwrap_or(1),
        audio = parsed.audio_limit_bytes / i64::try_from(MIB).unwrap_or(1),
        pdf = parsed.pdf_limit_bytes / i64::try_from(MIB).unwrap_or(1),
        tor = if parsed.enable_tor {
            "enabled"
        } else {
            "disabled"
        },
        https_cookies = if parsed.https_cookies {
            "enabled when HTTPS"
        } else {
            "unchanged"
        },
        proxy = if parsed.behind_proxy {
            "configured after restart"
        } else {
            "off"
        },
        backup = if parsed.auto_backup_interval_hours == 0 {
            "automatic backups disabled".to_owned()
        } else {
            format!(
                "every {} hours, keep {}",
                parsed.auto_backup_interval_hours, parsed.backup_retention
            )
        },
        hidden = hidden,
    );
    let boards = templates::live_boards();
    templates::base_layout(
        "review setup",
        None,
        &body,
        csrf,
        boards.as_ref(),
        current_theme,
        Some(&parsed.default_theme),
        false,
        "/setup",
    )
}

fn preset_options(selected: &str) -> String {
    let mut out = String::new();
    for preset in [
        SetupPreset::Public,
        SetupPreset::Private,
        SetupPreset::Local,
    ] {
        let _ = write!(
            out,
            r#"<label class="setup-preset"><input type="radio" name="preset" value="{value}"{checked}> <span>{label}</span> <a href="/setup?preset={value}">prefill</a></label>"#,
            value = preset.as_str(),
            checked = if selected == preset.as_str() {
                " checked"
            } else {
                ""
            },
            label = preset.label(),
        );
    }
    out
}

fn visibility_options(selected: &str) -> String {
    let mut out = String::new();
    for (value, label) in [
        ("public", "Public"),
        ("view_password", "Require password to view"),
        ("post_password", "Require password to post"),
    ] {
        let _ = write!(
            out,
            r#"<option value="{value}"{selected_attr}>{label}</option>"#,
            selected_attr = if selected == value { " selected" } else { "" },
        );
    }
    out
}

fn hidden_field(name: &str, value: &str) -> String {
    format!(
        r#"<input type="hidden" name="{name}" value="{value}">"#,
        name = escape_html(name),
        value = escape_html(value)
    )
}

fn hidden_checkbox(name: &str, value: Option<&str>) -> String {
    if checkbox(value) {
        hidden_field(name, "1")
    } else {
        String::new()
    }
}

fn hidden_form_fields(csrf: &str, form: &SetupWizardForm) -> String {
    let mut out = String::new();
    out.push_str(&hidden_field("_csrf", csrf));
    for (name, value) in [
        ("preset", form.preset.as_str()),
        ("site_name", form.site_name.as_str()),
        (
            "site_subtitle",
            form.site_subtitle.as_deref().unwrap_or_default(),
        ),
        (
            "default_theme",
            form.default_theme.as_deref().unwrap_or_default(),
        ),
        (
            "admin_username",
            form.admin_username.as_deref().unwrap_or_default(),
        ),
        (
            "admin_password_token",
            form.admin_password_token.as_deref().unwrap_or_default(),
        ),
        ("public_url", form.public_url.as_deref().unwrap_or_default()),
        ("board_slug", form.board_slug.as_str()),
        ("board_name", form.board_name.as_str()),
        (
            "board_description",
            form.board_description.as_deref().unwrap_or_default(),
        ),
        (
            "board_visibility",
            form.board_visibility.as_deref().unwrap_or("public"),
        ),
        ("image_limit_mib", form.image_limit_mib.as_str()),
        ("video_limit_mib", form.video_limit_mib.as_str()),
        ("audio_limit_mib", form.audio_limit_mib.as_str()),
        ("pdf_limit_mib", form.pdf_limit_mib.as_str()),
        (
            "captcha_type",
            form.captcha_type.as_deref().unwrap_or("builtin"),
        ),
        (
            "post_cooldown_secs",
            form.post_cooldown_secs.as_deref().unwrap_or("0"),
        ),
        (
            "backup_retention",
            form.backup_retention.as_deref().unwrap_or("1"),
        ),
    ] {
        out.push_str(&hidden_field(name, value));
    }
    for (name, value) in [
        ("enable_tor", form.enable_tor.as_deref()),
        ("tor_only", form.tor_only.as_deref()),
        ("https_cookies", form.https_cookies.as_deref()),
        ("behind_proxy", form.behind_proxy.as_deref()),
        ("board_nsfw", form.board_nsfw.as_deref()),
        ("allow_posting", form.allow_posting.as_deref()),
        ("allow_uploads", form.allow_uploads.as_deref()),
        ("allow_video", form.allow_video.as_deref()),
        ("allow_audio", form.allow_audio.as_deref()),
        ("allow_pdf", form.allow_pdf.as_deref()),
        ("allow_video_embeds", form.allow_video_embeds.as_deref()),
        ("allow_thread_editing", form.allow_thread_editing.as_deref()),
        ("allow_self_delete", form.allow_self_delete.as_deref()),
        ("allow_archive", form.allow_archive.as_deref()),
        ("allow_captcha", form.allow_captcha.as_deref()),
        (
            "homepage_new_thread_badges_enabled",
            form.homepage_new_thread_badges_enabled.as_deref(),
        ),
        (
            "homepage_new_reply_badges_enabled",
            form.homepage_new_reply_badges_enabled.as_deref(),
        ),
        (
            "thread_new_reply_badges_enabled",
            form.thread_new_reply_badges_enabled.as_deref(),
        ),
        ("hide_nsfw_default", form.hide_nsfw_default.as_deref()),
        ("auto_backup_enabled", form.auto_backup_enabled.as_deref()),
        (
            "include_tor_keys_in_backups",
            form.include_tor_keys_in_backups.as_deref(),
        ),
    ] {
        out.push_str(&hidden_checkbox(name, value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    #[test]
    fn setup_validation_rejects_bad_slug_and_password_mismatch() {
        let mut form = SetupWizardForm::defaults_for(SetupPreset::Public);
        form.board_slug = "../bad".to_owned();
        form.admin_password = Some("long-enough-password".to_owned());
        form.admin_password_confirm = Some("different-password".to_owned());

        let errors = parse_setup_form(&form, 0).expect_err("validation errors");

        assert!(errors.iter().any(|error| error.contains("Board slug")));
        assert!(errors.iter().any(|error| error.contains("Admin password")));
    }

    #[test]
    fn upload_limit_parser_requires_clear_mib_units() {
        assert_eq!(parse_upload_limit_mib("8"), Some(8 * MIB_I64));
        assert_eq!(parse_upload_limit_mib("0"), None);
        assert_eq!(parse_upload_limit_mib("nope"), None);
    }

    #[test]
    fn preset_defaults_are_conservative_for_public_instances() {
        let defaults = preset_defaults(SetupPreset::Public);
        assert!(defaults.allow_uploads);
        assert!(!defaults.allow_audio);
        assert!(!defaults.allow_pdf);
        assert!(defaults.allow_captcha);
    }

    fn setup_form_body(csrf: &str, board_slug: &str, password: Option<&str>) -> String {
        let mut fields = vec![
            ("_csrf", csrf.to_owned()),
            ("preset", "public".to_owned()),
            ("site_name", "Test RustChan".to_owned()),
            ("site_subtitle", String::new()),
            ("default_theme", crate::theme::HARD_DEFAULT_THEME.to_owned()),
            ("admin_username", "admin".to_owned()),
            ("board_slug", board_slug.to_owned()),
            ("board_name", "Test Board".to_owned()),
            ("board_description", String::new()),
            ("board_visibility", "public".to_owned()),
            ("allow_posting", "1".to_owned()),
            ("allow_uploads", "1".to_owned()),
            ("allow_video", "1".to_owned()),
            ("allow_pdf", "1".to_owned()),
            ("allow_video_embeds", "1".to_owned()),
            ("allow_thread_editing", "1".to_owned()),
            ("allow_self_delete", "1".to_owned()),
            ("allow_archive", "1".to_owned()),
            ("image_limit_mib", "8".to_owned()),
            ("video_limit_mib", "50".to_owned()),
            ("audio_limit_mib", "150".to_owned()),
            ("pdf_limit_mib", "7".to_owned()),
            ("captcha_type", "builtin".to_owned()),
            ("post_cooldown_secs", "0".to_owned()),
            ("homepage_new_thread_badges_enabled", "1".to_owned()),
            ("homepage_new_reply_badges_enabled", "1".to_owned()),
            ("thread_new_reply_badges_enabled", "1".to_owned()),
            ("backup_retention", "1".to_owned()),
        ];
        if let Some(password) = password {
            fields.push(("admin_password", password.to_owned()));
            fields.push(("admin_password_confirm", password.to_owned()));
        }
        fields
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&")
    }

    fn setup_csrf_pair() -> (String, String) {
        let raw = "csrf123".to_owned();
        let form =
            crypto::make_scoped_csrf_form_token(&raw, &CONFIG.cookie_secret, SETUP_CSRF_SCOPE);
        (raw, form)
    }

    fn install_setup_theme_test_state() {
        crate::templates::set_live_default_theme("forest");
        crate::templates::set_live_themes(vec![
            crate::models::Theme {
                slug: "forest".to_owned(),
                display_name: "Forest".to_owned(),
                description: "Forest theme".to_owned(),
                swatch_hex: "#7ab84e".to_owned(),
                enabled: true,
                sort_order: 1,
                is_builtin: true,
                custom_css: String::new(),
            },
            crate::models::Theme {
                slug: "blue-sky".to_owned(),
                display_name: "Blue Sky".to_owned(),
                description: "Bright theme".to_owned(),
                swatch_hex: "#66aaff".to_owned(),
                enabled: true,
                sort_order: 2,
                is_builtin: true,
                custom_css: String::new(),
            },
        ]);
    }

    #[tokio::test]
    async fn initialized_instance_blocks_setup_route() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("conn");
            crate::db::create_admin(&conn, "admin", "hash").expect("admin");
        }
        let app = Router::new()
            .route("/setup", get(setup_get))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/setup")
                    .header(header::HOST, "localhost")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(body.contains("Setup wizard is not available"));
    }

    #[tokio::test]
    async fn initialized_instance_blocks_setup_post_routes() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("conn");
            crate::db::mark_setup_complete(&conn).expect("complete");
        }
        let app = Router::new()
            .route("/setup/review", post(setup_review))
            .route("/setup/finish", post(setup_finish))
            .with_state(state);
        let (_raw_csrf, form_csrf) = setup_csrf_pair();
        let body = setup_form_body(&form_csrf, "b", Some("long-enough-password"));

        for uri in ["/setup/review", "/setup/finish"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(header::HOST, "localhost")
                        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                        .header(header::COOKIE, "csrf_token=csrf123")
                        .extension(crate::test_support::connect_info())
                        .body(Body::from(body.clone()))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn setup_get_uses_current_theme_cookie() {
        install_setup_theme_test_state();
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/setup", get(setup_get))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/setup")
                    .header(header::HOST, "localhost")
                    .header(header::COOKIE, "rustchan_theme=blue-sky")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(body.contains(r#"data-active-theme="blue-sky""#));
        assert!(body.contains(r#"data-theme="blue-sky""#));
    }

    #[tokio::test]
    async fn setup_review_uses_selected_default_theme_without_js() {
        install_setup_theme_test_state();
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/setup/review", post(setup_review))
            .with_state(state);
        let (_raw_csrf, form_csrf) = setup_csrf_pair();
        let body = setup_form_body(&form_csrf, "b", Some("long-enough-password"))
            .replace("default_theme=forest", "default_theme=blue-sky");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/review")
                    .header(header::HOST, "localhost")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(body.contains(r#"data-active-theme="blue-sky""#));
        assert!(body.contains(r#"data-theme="blue-sky""#));
        assert!(body.contains(r#"name="default_theme" value="blue-sky""#));
    }

    #[tokio::test]
    async fn setup_review_does_not_echo_admin_password() {
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/setup/review", post(setup_review))
            .with_state(state);
        let (_raw_csrf, form_csrf) = setup_csrf_pair();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/review")
                    .header(header::HOST, "localhost")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(setup_form_body(
                        &form_csrf,
                        "b",
                        Some("long-enough-password"),
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body = String::from_utf8(body.to_vec()).expect("utf8");
        assert!(!body.contains("long-enough-password"));
        assert!(!body.contains("admin_password\""));
        assert!(body.contains("admin_password_token"));
    }

    #[tokio::test]
    async fn failed_setup_finish_does_not_mark_complete() {
        let state = crate::test_support::app_state();
        {
            let conn = state.db.get().expect("conn");
            crate::db::create_board(&conn, "b", "Existing", "", false).expect("board");
        }
        let app = Router::new()
            .route("/setup/finish", post(setup_finish))
            .with_state(state.clone());
        let (_raw_csrf, form_csrf) = setup_csrf_pair();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/finish")
                    .header(header::HOST, "localhost")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(setup_form_body(
                        &form_csrf,
                        "b",
                        Some("long-enough-password"),
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let conn = state.db.get().expect("conn");
        let setup_state = db::setup_state(&conn).expect("state");
        assert!(!setup_state.completed);
    }

    #[tokio::test]
    async fn setup_finish_marks_completion_durably_and_persists_pdf_limit() {
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/setup/finish", post(setup_finish))
            .with_state(state.clone());
        let (_raw_csrf, form_csrf) = setup_csrf_pair();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/finish")
                    .header(header::HOST, "localhost")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(setup_form_body(
                        &form_csrf,
                        "pdf",
                        Some("long-enough-password"),
                    )))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let conn = state.db.get().expect("conn");
        let setup_state = db::setup_state(&conn).expect("state");
        assert!(setup_state.completed);
        assert!(!setup_state.reopened);
        let board = db::get_board_by_short(&conn, "pdf")
            .expect("load board")
            .expect("board");
        assert_eq!(board.max_pdf_size, 7 * MIB_I64);
    }
}
