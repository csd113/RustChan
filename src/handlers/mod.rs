pub mod admin;
pub mod board;
pub mod thread;

// ─── Shared multipart form parsing ───────────────────────────────────────────
//
// Both create_thread and post_reply parse the same multipart fields.
// This helper consolidates that duplicated logic into one place.

use crate::error::{AppError, Result};
use crate::middleware::validate_csrf;
use axum::extract::Multipart;

/// Parsed fields from a post/thread creation multipart form.
pub struct PostFormData {
    pub csrf_verified: bool,
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    /// Raw bytes + original filename if a file was attached.
    pub file: Option<(Vec<u8>, String)>,
    // ── Poll fields (only used when creating a new thread) ────────────────
    pub poll_question: String,
    pub poll_options: Vec<String>,
    /// Duration in seconds (parsed from value + unit)
    pub poll_duration_secs: Option<i64>,
}

/// Drain all fields from a multipart form into [`PostFormData`].
/// `csrf_cookie` is the value from the browser cookie for CSRF verification.
pub async fn parse_post_multipart(
    mut multipart: Multipart,
    csrf_cookie: Option<&str>,
) -> Result<PostFormData> {
    let mut csrf_verified = false;
    let mut name = String::new();
    let mut subject = String::new();
    let mut body = String::new();
    let mut deletion_token = String::new();
    let mut file: Option<(Vec<u8>, String)> = None;
    let mut poll_question = String::new();
    let mut poll_options: Vec<String> = Vec::new();
    let mut poll_duration_value: Option<i64> = None;
    let mut poll_duration_unit = String::from("hours");

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => {
                let v = field.text().await.unwrap_or_default();
                if validate_csrf(csrf_cookie, &v) {
                    csrf_verified = true;
                }
            }
            Some("name")           => name           = field.text().await.unwrap_or_default(),
            Some("subject")        => subject         = field.text().await.unwrap_or_default(),
            Some("body")           => body            = field.text().await.unwrap_or_default(),
            Some("deletion_token") => deletion_token  = field.text().await.unwrap_or_default(),
            Some("poll_question")  => poll_question   = field.text().await.unwrap_or_default(),
            Some("poll_option") => {
                let v = field.text().await.unwrap_or_default();
                let trimmed = v.trim().to_string();
                if !trimmed.is_empty() {
                    poll_options.push(trimmed);
                }
            }
            Some("poll_duration_value") => {
                let v = field.text().await.unwrap_or_default();
                poll_duration_value = v.trim().parse::<i64>().ok();
            }
            Some("poll_duration_unit") => {
                poll_duration_unit = field.text().await.unwrap_or_default();
            }
            Some("file") => {
                let fname = field.file_name().unwrap_or("upload").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::BadRequest(format!("File read error: {e}")))?;
                if !bytes.is_empty() {
                    file = Some((bytes.to_vec(), fname));
                }
            }
            _ => { let _ = field.bytes().await; }
        }
    }

    // Convert duration value + unit → seconds
    let poll_duration_secs = if !poll_question.trim().is_empty() {
        poll_duration_value.map(|v| {
            if poll_duration_unit == "minutes" { v * 60 } else { v * 3600 }
        })
    } else {
        None
    };

    Ok(PostFormData {
        csrf_verified, name, subject, body, deletion_token, file,
        poll_question, poll_options, poll_duration_secs,
    })
}
