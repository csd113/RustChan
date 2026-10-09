use super::{
    board_access_cookie_from_jar, check_csrf_jar, db, hash_ip, identity_key,
    load_board_access_context, templates, AppError, AppState, CookieJar, Form, Redirect, Response,
    Result, State, ADMIN_SESSION_COOKIE, CONFIG,
};
use axum::response::IntoResponse as _;

#[derive(serde::Deserialize)]
pub(in crate::server) struct ReportForm {
    pub post_id: i64,
    pub thread_id: i64,
    pub board: String,
    pub reason: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

/// Bound optional Unicode report text while rejecting hidden control characters.
fn report_reason(raw: Option<&str>) -> Result<String> {
    let raw = raw.unwrap_or("");
    if raw.chars().any(char::is_control) {
        return Err(AppError::BadRequest(
            "Report reason contains control characters.".into(),
        ));
    }
    Ok(raw.trim().chars().take(256).collect())
}

pub(in crate::server) async fn file_report(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    req_headers: axum::http::HeaderMap,
    peer: crate::middleware::SecureCookieContext,
    Form(form): Form<ReportForm>,
) -> Result<Response> {
    super::check_menu_action_csrf(&jar, &req_headers, peer, form.csrf.as_deref())?;

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = report_reason(form.reason.as_deref())?;

    let post_id = form.post_id;
    if post_id <= 0 || form.thread_id <= 0 || !super::valid_action_board(&form.board) {
        return Err(AppError::BadRequest("Invalid report target.".into()));
    }
    let board_raw = form.board.clone();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_raw);

    let board_raw_closure = board_raw.clone();
    let csrf_cookie = jar
        .get("csrf_token")
        .map(|cookie| cookie.value().to_owned())
        .unwrap_or_default();
    let outcome = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<Option<(i64, db::ReportSubmission)>> {
            let conn = pool.get()?;
            let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
            let access_context = load_board_access_context(
                &tx,
                &board_raw_closure,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Err(AppError::Forbidden(
                    "This board requires a password.".into(),
                ));
            }
            // Banned identities must not file reports. CSRF has already been
            // validated above, so the ban notice can render its appeal form.
            super::ensure_actor_not_banned(&tx, &ip_hash, csrf_cookie)?;
            let board = access_context.board;
            // Verify post exists and belongs to this board to prevent spoofed reports.
            let post = db::get_post(&tx, post_id)?
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
            // The report ID does not affect the authoritative thread redirect.
            // Serialize the abuse budget and insertion. Duplicates remain
            // idempotent even when the reporter has exhausted the budget.
            let (recent, duplicate): (i64, bool) = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM reports WHERE reporter_hash = ?1 AND created_at > unixepoch() - 3600),
                        EXISTS(SELECT 1 FROM reports WHERE post_id = ?2 AND reporter_hash = ?1 AND status = 'open')",
                rusqlite::params![ip_hash, post_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if recent >= 20 && !duplicate {
                return Ok(None);
            }
            let submission = db::file_report(&tx, post_id, &reason, &ip_hash)?;
            tx.commit()?;
            Ok(Some((authoritative_thread_id, submission)))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let Some((db_thread_id, submission)) = outcome else {
        let message = "Too many reports. Please try again in an hour.";
        let mut response = (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::Extension(crate::error::ErrorPage::Message(message.to_owned())),
            axum::response::Html(templates::error_page(429, message)),
        )
            .into_response();
        drop(response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("3600"),
        ));
        return Ok(response);
    };
    let reported = match submission {
        db::ReportSubmission::Filed => "1",
        db::ReportSubmission::AlreadyFiled => "duplicate",
    };
    // Target and board were checked together under the write lock.
    Ok(Redirect::to(&format!(
        "/{board_raw}/thread/{db_thread_id}?reported={reported}#p{}",
        form.post_id
    ))
    .into_response())
}

// GET /boards/{*media_path} — serve media with mp4→webm redirect

// Content-Type helper for board media
/// Return the correct `Content-Type` value for a board media file based solely
/// on its extension.  Used to override whatever `mime_guess` / `ServeFile`
/// produces, because some builds of `mime_guess` do not include `.webp`,
/// `.svg`, or audio formats in their database and fall back to
/// `application/octet-stream`, which causes browsers to download the file
/// rather than display or play it inline.

#[derive(serde::Deserialize)]
pub(in crate::server) struct AppealForm {
    pub reason: String,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

pub(in crate::server) async fn submit_appeal(
    State(state): State<AppState>,
    crate::middleware::ClientIp(client_ip): crate::middleware::ClientIp,
    jar: CookieJar,
    Form(form): Form<AppealForm>,
) -> impl axum::response::IntoResponse {
    use axum::response::Html;

    if check_csrf_jar(&jar, form.csrf.as_deref()).is_err() {
        return AppError::Forbidden("CSRF token mismatch.".into()).into_response();
    }

    let ip_hash = hash_ip(&identity_key(&client_ip, &jar), &CONFIG.cookie_secret);
    let reason = form.reason.trim().chars().take(512).collect::<String>();
    if reason.is_empty() {
        return AppError::BadRequest("Appeal message cannot be empty.".into()).into_response();
    }

    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<db::BanAppealSubmission> {
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

    let body = format!(
        r#"<div class="page-box error-page"><h1>appeal submitted</h1>
<p>{}</p><p><a href="/">return home</a></p></div>"#,
        crate::utils::sanitize::escape_html(msg),
    );
    let theme = super::current_theme_from_jar(&jar);
    let boards = templates::live_boards();
    let html = templates::base_layout(
        "Appeal Submitted",
        None,
        &body,
        form.csrf.as_deref().unwrap_or(""),
        &boards,
        theme.as_deref(),
        None,
        false,
        "/",
    );
    Html(html).into_response()
}
