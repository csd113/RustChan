// Request handlers.

/// Original tracing target, retained for existing `RUST_LOG` filters.
const LOG_TARGET: &str = concat!(env!("CARGO_CRATE_NAME"), "::handlers");

pub(crate) mod admin;
pub(crate) mod banner;
pub(crate) mod board;
pub(crate) mod captcha;
pub(crate) mod favicon;
pub(crate) mod posting;
pub(crate) mod render;
pub(crate) mod setup;
pub(crate) mod thread;

mod multipart;

#[expect(
    unused_imports,
    reason = "retain the existing crate-visible envelope limit path even without an external caller"
)]
pub(crate) use multipart::PUBLIC_MULTIPART_ENVELOPE_MAX_BYTES;
pub(crate) use multipart::{
    discard_unknown_multipart_field, enforce_public_multipart_envelope, parse_post_multipart,
    PostFormData, PostMultipartContext, TempUpload, PUBLIC_MULTIPART_REQUEST_MAX_BYTES,
    PUBLIC_UPLOAD_TIMEOUT,
};

#[expect(
    unused_imports,
    reason = "retain existing crate-visible upload helper paths even when only called inside their private owner"
)]
pub(crate) use posting::{
    classify_upload_error, process_audio_combo, process_audio_first_uploads, process_primary_upload,
};

use crate::error::AppError;
use axum::{
    body::Body,
    response::{IntoResponse as _, Response},
};

/// Classify enhanced requests; authentication and CSRF checks remain independent.
#[must_use]
pub(crate) fn is_xml_http_request(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

/// Keeps native posting failures actionable without changing their status or security checks.
pub(crate) fn native_post_error_response(
    error: AppError,
    prefill: Option<&crate::templates::forms::PostFormState>,
) -> Response {
    let mut response = error.into_response();
    let Some(crate::error::ErrorPage::Message(message)) =
        response.extensions().get::<crate::error::ErrorPage>()
    else {
        // Ban pages retain their dedicated explanation and appeal form.
        return response;
    };
    let body = crate::templates::forms::post_error_recovery_body(message, prefill);
    let html = crate::templates::base_layout(
        "Posting error",
        None,
        &body,
        "",
        &crate::templates::live_boards(),
        None,
        None,
        false,
        "/",
    );
    let _previous_value = response
        .extensions_mut()
        .insert(crate::error::ErrorPage::Content {
            title: "Posting error".into(),
            body,
        });
    *response.body_mut() = Body::from(html);
    response
}

#[cfg(test)]
#[path = "tests/page_csrf.rs"]
pub(crate) mod csrf_test_support;
