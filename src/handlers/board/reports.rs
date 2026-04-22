#![allow(clippy::wildcard_imports)]

use super::*;

#[derive(serde::Deserialize)]
pub struct ReportForm {
    pub post_id: i64,
    pub thread_id: i64,
    pub board: String,
    pub reason: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn file_report(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<ReportForm>,
) -> Result<Response> {
    check_csrf_jar(&jar, form.csrf.as_deref())?;

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = form
        .reason
        .as_deref()
        .unwrap_or("")
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    let post_id = form.post_id;
    let board_raw = form
        .board
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_raw);

    let board_raw_closure = board_raw.clone();
    let db_thread_id = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<i64> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_raw_closure,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Err(AppError::Forbidden(
                    "This board requires a password.".into(),
                ));
            }
            let board = access_context.board;
            // Verify post exists and belongs to this board to prevent spoofed reports.
            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            if post.board_id != board.id {
                return Err(AppError::BadRequest(
                    "Post does not belong to this board.".into(),
                ));
            }
            if post.thread_id != form.thread_id {
                return Err(AppError::BadRequest(
                    "Reported thread does not match the selected post.".into(),
                ));
            }
            // Use the DB's thread_id for the redirect — not the user-submitted value.
            let authoritative_thread_id = post.thread_id;
            let _ = db::file_report(&conn, post_id, &reason, &ip_hash)?;
            Ok(authoritative_thread_id)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Redirect back to the thread using the DB-resolved IDs.
    // `board_raw` is already sanitised to alphanumeric earlier in this handler.
    Ok(Redirect::to(&format!(
        "/{board_raw}/thread/{db_thread_id}?reported=1#p{}",
        form.post_id
    ))
    .into_response())
}

// ─── GET /boards/{*media_path} — serve media with mp4→webm redirect ──────────
//

// ─── Content-Type helper for board media ─────────────────────────────────────

/// Return the correct `Content-Type` value for a board media file based solely
/// on its extension.  Used to override whatever `mime_guess` / `ServeFile`
/// produces, because some builds of `mime_guess` do not include `.webp`,
/// `.svg`, or audio formats in their database and fall back to
/// `application/octet-stream`, which causes browsers to download the file
/// rather than display or play it inline.

#[derive(serde::Deserialize)]
pub struct AppealForm {
    pub reason: String,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub async fn submit_appeal(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> impl axum::response::IntoResponse {
    use axum::response::Html;

    if check_csrf_jar(&jar, form.csrf.as_deref()).is_err() {
        return Html(crate::templates::error_page(403, "CSRF token mismatch.")).into_response();
    }

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return Html(crate::templates::error_page(
            400,
            "Appeal message cannot be empty.",
        ))
        .into_response();
    }

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<db::BanAppealSubmission> {
            let conn = pool.get()?;
            Ok(db::file_ban_appeal(&conn, &ip_hash, &reason)?)
        }
    })
    .await;

    let msg = match result {
        Ok(Ok(db::BanAppealSubmission::Filed)) => {
            "Your appeal has been submitted. An admin will review it."
        }
        Ok(Ok(db::BanAppealSubmission::AlreadyFiled)) => {
            "You have already filed an appeal in the last 24 hours."
        }
        Ok(Ok(db::BanAppealSubmission::NotBanned)) => "Your IP is not currently banned.",
        _ => "An error occurred. Please try again.",
    };

    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Appeal Submitted</title>
<link rel="stylesheet" href="{stylesheet_href}">
</head><body><div class="page-box error-page">
<h1>appeal submitted</h1>
<p>{msg}</p>
<p><a href="/">return home</a></p>
</div></body></html>"#,
        stylesheet_href = crate::templates::static_asset_url("/static/style.css"),
        msg = crate::utils::sanitize::escape_html(msg)
    );
    Html(html).into_response()
}
