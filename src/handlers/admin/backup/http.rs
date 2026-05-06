use super::*;

pub async fn backup_request_logging_middleware(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    if uri.path() == "/admin/backup/progress" {
        return next.run(req).await;
    }
    let headers = req.headers().clone();
    let response = next.run(req).await;
    let status = response.status();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        method = %method,
        uri = %uri,
        status = status.as_u16(),
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        "Admin backup request completed"
    );

    response
}

pub(super) fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("XMLHttpRequest"))
}

pub(super) fn admin_xhr_error_response(error: &AppError) -> Response {
    let handled = match error {
        AppError::NotFound(message) => Some((StatusCode::NOT_FOUND, message.clone())),
        AppError::BadRequest(message) => Some((StatusCode::BAD_REQUEST, message.clone())),
        AppError::Forbidden(message) => Some((StatusCode::FORBIDDEN, message.clone())),
        AppError::BannedUser { reason, .. } => Some((
            StatusCode::FORBIDDEN,
            format!("You are banned. Reason: {reason}"),
        )),
        AppError::UploadTooLarge(message) => Some((StatusCode::PAYLOAD_TOO_LARGE, message.clone())),
        AppError::InvalidMediaType(message) => {
            Some((StatusCode::UNSUPPORTED_MEDIA_TYPE, message.clone()))
        }
        AppError::Conflict(message) => Some((StatusCode::CONFLICT, message.clone())),
        AppError::DbBusy => Some((
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is temporarily busy. Please try again in a moment.".to_string(),
        )),
        AppError::Internal(error) => {
            tracing::error!("Internal admin restore XHR error: {:?}", error);
            None
        }
        AppError::Tls(message) => {
            tracing::error!("TLS admin restore XHR error: {message}");
            None
        }
    };

    if let Some((status, message)) = handled {
        return crate::handlers::board::xhr_handled_error_response(status, &message)
            .unwrap_or_else(|response_error| response_error.into_response());
    }

    let (status, message) = match error {
        AppError::Internal(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        AppError::Tls(message) => (StatusCode::INTERNAL_SERVER_ERROR, message.clone()),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Unexpected admin restore error.".to_string(),
        ),
    };

    crate::handlers::board::xhr_error_response(status, &message)
        .unwrap_or_else(|response_error| response_error.into_response())
}

pub(super) fn redirect_page_response(target: &str, message: &str) -> Response {
    let escaped_target = crate::utils::sanitize::escape_html(target);
    let escaped_message = crate::utils::sanitize::escape_html(message);
    let body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="0;url={escaped_target}">
<title>Redirecting</title>
</head>
<body>
<p>{escaped_message}</p>
<p><a href="{escaped_target}">Continue</a></p>
</body>
</html>"#
    );

    let mut resp = Response::new(axum::body::Body::from(body));
    *resp.status_mut() = StatusCode::OK;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::HeaderName::from_static("refresh"),
        HeaderValue::from_str(&format!("0; url={target}"))
            .unwrap_or_else(|_| HeaderValue::from_static("0; url=/admin/panel")),
    );
    resp
}

#[derive(Clone, Copy)]
pub(super) enum RestoreKind {
    Full,
    Board,
}

impl RestoreKind {
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Full => "Full restore",
            Self::Board => "Board restore",
        }
    }

    pub(super) const fn route(self) -> &'static str {
        match self {
            Self::Full => "/admin/restore",
            Self::Board => "/admin/board/restore",
        }
    }

    pub(super) const fn maintenance_label(self) -> &'static str {
        match self {
            Self::Full => "Full restore",
            Self::Board => "Board restore",
        }
    }

    const fn start_failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore could not start.",
            Self::Board => "Board restore could not start.",
        }
    }

    const fn upload_failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore upload failed.",
            Self::Board => "Board restore upload failed.",
        }
    }

    const fn failure_message(self) -> &'static str {
        match self {
            Self::Full => "Restore failed.",
            Self::Board => "Board restore failed.",
        }
    }

    const fn open_section(self) -> &'static str {
        match self {
            Self::Full => FULL_BACKUP_RESTORE_SECTION,
            Self::Board => BOARD_BACKUP_RESTORE_SECTION,
        }
    }

    const fn anchor(self) -> &'static str {
        self.open_section()
    }
}

pub(super) struct StreamedRestoreUpload {
    pub temp_file: tempfile::NamedTempFile,
    pub form_csrf: Option<String>,
    pub restore_tor_hidden_service_keys: bool,
    pub uploaded_filename: Option<String>,
    pub uploaded_content_type: Option<String>,
    pub uploaded_bytes: u64,
}

pub(super) fn restore_start_response(
    kind: RestoreKind,
    xhr_request: bool,
    error: &impl std::fmt::Display,
) -> Response {
    if xhr_request {
        return crate::handlers::board::xhr_handled_error_response(
            StatusCode::CONFLICT,
            &error.to_string(),
        )
        .unwrap_or_else(|response_error| response_error.into_response());
    }
    redirect_page_response(
        &restore_error_redirect_target(kind, &error.to_string()),
        kind.start_failure_message(),
    )
}

pub(super) fn restore_upload_parse_response(
    kind: RestoreKind,
    xhr_request: bool,
    error: &impl std::fmt::Display,
) -> Response {
    let message = format!("Upload parsing failed: {error}");
    if xhr_request {
        return crate::handlers::board::xhr_handled_error_response(
            StatusCode::BAD_REQUEST,
            &message,
        )
        .unwrap_or_else(|response_error| response_error.into_response());
    }
    redirect_page_response(
        &restore_error_redirect_target(kind, &message),
        kind.upload_failure_message(),
    )
}

pub(super) fn restore_failure_response(
    kind: RestoreKind,
    xhr_request: bool,
    error: &AppError,
) -> Response {
    if xhr_request {
        return admin_xhr_error_response(error);
    }
    redirect_page_response(
        &restore_error_redirect_target(kind, &error.to_string()),
        kind.failure_message(),
    )
}

pub(super) fn restore_success_redirect_target(
    kind: RestoreKind,
    board_short: Option<&str>,
) -> String {
    let target = restore_admin_panel_target(kind, board_short);
    match kind {
        RestoreKind::Full => format!(
            "/admin/panel?restored=1&open={}#{}",
            target.open_section_value().unwrap_or_default(),
            target.anchor_value().unwrap_or_default()
        ),
        RestoreKind::Board => {
            let board_short = board_short.expect("board restore success requires board short");
            format!(
                "/admin/panel?flash={}&open={}#board-backup-{}",
                crate::utils::redirect::encode_form_query_component(&format!(
                    "Board /{board_short}/ restored."
                )),
                target.open_section_value().unwrap_or_default(),
                board_short
            )
        }
    }
}

pub(super) fn restore_error_redirect_target(kind: RestoreKind, message: &str) -> String {
    let target = restore_admin_panel_target(kind, None);
    format!(
        "/admin/panel?restore_error={}&open={}#{}",
        crate::utils::redirect::encode_form_query_component(message),
        target.open_section_value().unwrap_or_default(),
        target.anchor_value().unwrap_or_default()
    )
}

fn restore_admin_panel_target(
    kind: RestoreKind,
    board_short: Option<&str>,
) -> super::AdminPanelTarget<'_> {
    match kind {
        RestoreKind::Full => {
            super::AdminPanelTarget::anchor_open(kind.anchor(), kind.open_section())
        }
        RestoreKind::Board => {
            if let Some(board_short) = board_short {
                super::AdminPanelTarget::owned_anchor_open(
                    format!("board-backup-{board_short}"),
                    kind.open_section(),
                )
            } else {
                super::AdminPanelTarget::anchor_open(kind.anchor(), kind.open_section())
            }
        }
    }
}

pub(super) fn log_restore_upload_started(kind: RestoreKind, headers: &HeaderMap, jar: &CookieJar) {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok());

    tracing::info!(
        target: "admin",
        route = kind.route(),
        content_type = content_type.unwrap_or(""),
        content_length = content_length.unwrap_or(""),
        has_session_cookie = jar.get(super::SESSION_COOKIE).is_some(),
        has_csrf_cookie = jar.get("csrf_token").is_some(),
        "{} upload started",
        kind.title()
    );
}

pub(super) async fn restore_auth_preflight(
    state: &AppState,
    headers: &HeaderMap,
    jar: &CookieJar,
    peer: Option<std::net::SocketAddr>,
) -> Result<Option<String>> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(headers, peer)?;

    {
        let pool = state.db.clone();
        let session_id_for_task = session_id.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id_for_task.as_deref())?;
            Ok(())
        })
        .await
        .map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Admin auth preflight task failed: {error}"))
        })??;
    }

    Ok(session_id)
}

pub(super) async fn stream_restore_upload_to_tempfile(
    kind: RestoreKind,
    multipart: &mut Multipart,
) -> Result<StreamedRestoreUpload> {
    const RESTORE_CSRF_FIELD_MAX_BYTES: usize = 4 * 1024;
    const RESTORE_CONTROL_FIELD_MAX_BYTES: usize = 1024;

    let mut temp_file: Option<tempfile::NamedTempFile> = None;
    let mut form_csrf: Option<String> = None;
    let mut restore_tor_hidden_service_keys = false;
    let mut seen_restore_tor_hidden_service_keys = false;
    let mut uploaded_filename: Option<String> = None;
    let mut uploaded_content_type: Option<String> = None;
    let mut uploaded_bytes = 0u64;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::BadRequest(format!("Multipart error: {error}")))?
    {
        let field_name = field.name().unwrap_or("<unnamed>").to_string();
        match field.name() {
            Some("_csrf") => {
                if form_csrf.is_some() {
                    return Err(AppError::BadRequest("Duplicate restore CSRF field.".into()));
                }
                tracing::debug!(
                    target: "admin",
                    route = kind.route(),
                    field = "_csrf",
                    "{} received CSRF field",
                    kind.title()
                );
                form_csrf =
                    Some(read_restore_text_field(field, RESTORE_CSRF_FIELD_MAX_BYTES).await?);
            }
            Some("restore_tor_hidden_service_keys") => {
                if seen_restore_tor_hidden_service_keys {
                    return Err(AppError::BadRequest(
                        "Duplicate restore control field.".into(),
                    ));
                }
                seen_restore_tor_hidden_service_keys = true;
                let value = read_restore_text_field(field, RESTORE_CONTROL_FIELD_MAX_BYTES).await?;
                restore_tor_hidden_service_keys = matches!(value.as_str(), "1" | "true" | "on");
            }
            Some("backup_file") => {
                uploaded_filename = field.file_name().map(str::to_string);
                uploaded_content_type = field.content_type().map(str::to_string);
                tracing::info!(
                    target: "admin",
                    route = kind.route(),
                    field = field_name,
                    filename = uploaded_filename.as_deref().unwrap_or("<missing>"),
                    mime = uploaded_content_type.as_deref().unwrap_or("<missing>"),
                    "{} received backup file field",
                    kind.title()
                );
                let tmp = tempfile::NamedTempFile::new()
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Tempfile: {error}")))?;
                let std_clone = tmp
                    .as_file()
                    .try_clone()
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Clone fd: {error}")))?;
                let async_file = tokio::fs::File::from_std(std_clone);
                let mut writer = tokio::io::BufWriter::new(async_file);
                let mut field = field;
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|error| AppError::BadRequest(error.to_string()))?
                {
                    uploaded_bytes = uploaded_bytes
                        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
                    ensure_restore_upload_within_budget(
                        kind,
                        uploaded_bytes,
                        super::common::RESTORE_UPLOAD_MAX_BYTES,
                    )?;
                    writer.write_all(&chunk).await.map_err(|error| {
                        AppError::Internal(anyhow::anyhow!("Write chunk: {error}"))
                    })?;
                }
                writer
                    .flush()
                    .await
                    .map_err(|error| AppError::Internal(anyhow::anyhow!("Flush: {error}")))?;
                temp_file = Some(tmp);
            }
            _ => {
                tracing::debug!(
                    target: "admin",
                    route = kind.route(),
                    field = field_name,
                    "{} ignored unexpected multipart field",
                    kind.title()
                );
                crate::handlers::discard_unknown_multipart_field(field).await?;
            }
        }
    }

    let temp_file =
        temp_file.ok_or_else(|| AppError::BadRequest("No backup file uploaded.".into()))?;

    Ok(StreamedRestoreUpload {
        temp_file,
        form_csrf,
        restore_tor_hidden_service_keys,
        uploaded_filename,
        uploaded_content_type,
        uploaded_bytes,
    })
}

async fn read_restore_text_field(
    mut field: axum::extract::multipart::Field<'_>,
    max_bytes: usize,
) -> Result<String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|error| AppError::BadRequest(error.to_string()))?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(AppError::UploadTooLarge(
                "Restore control field is too large.".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| AppError::BadRequest("Restore control field is not valid UTF-8.".into()))
}

pub(super) fn validate_streamed_restore_upload(
    kind: RestoreKind,
    jar: &CookieJar,
    upload: &StreamedRestoreUpload,
) -> Result<u64> {
    let has_csrf_cookie = jar.get("csrf_token").is_some();
    if super::check_admin_csrf_jar(jar, upload.form_csrf.as_deref()).is_err() {
        tracing::warn!(
            target: "admin",
            route = kind.route(),
            has_csrf_cookie,
            has_form_csrf = upload.form_csrf.is_some(),
            "{} failed CSRF validation",
            kind.title()
        );
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let file_size = upload
        .temp_file
        .as_file()
        .seek(std::io::SeekFrom::End(0))
        .map_err(|error| AppError::Internal(anyhow::anyhow!("Seek check: {error}")))?;
    if file_size == 0 {
        return Err(AppError::BadRequest(
            "Uploaded backup file is empty.".into(),
        ));
    }
    ensure_restore_upload_within_budget(kind, file_size, super::common::RESTORE_UPLOAD_MAX_BYTES)?;

    tracing::info!(
        target: "admin",
        route = kind.route(),
        filename = upload.uploaded_filename.as_deref().unwrap_or("<missing>"),
        mime = upload.uploaded_content_type.as_deref().unwrap_or("<missing>"),
        streamed_bytes = upload.uploaded_bytes,
        temp_file_size = file_size,
        "{} upload streamed to disk",
        kind.title()
    );

    Ok(file_size)
}

pub(super) fn sanitize_backup_zip_filename(filename: &str) -> Result<String> {
    let safe_filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if safe_filename != filename
        || safe_filename.contains("..")
        || !Path::new(&safe_filename)
            .extension()
            .is_some_and(|e: &std::ffi::OsStr| e.eq_ignore_ascii_case("zip"))
    {
        return Err(AppError::BadRequest("Invalid filename.".into()));
    }
    Ok(safe_filename)
}

fn ensure_restore_upload_within_budget(
    kind: RestoreKind,
    uploaded_bytes: u64,
    budget: u64,
) -> Result<()> {
    if uploaded_bytes > budget {
        return Err(AppError::UploadTooLarge(format!(
            "{} upload exceeds the {} MiB restore budget.",
            kind.title(),
            budget / 1024 / 1024
        )));
    }

    Ok(())
}

pub(super) fn sanitize_board_short_value(board_short: &str) -> Result<String> {
    let safe_board = board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();
    if safe_board.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }
    Ok(safe_board)
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_restore_upload_within_budget, stream_restore_upload_to_tempfile, RestoreKind,
    };
    use axum::{
        body::Body,
        extract::Multipart,
        http::{header, Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt as _;

    #[test]
    fn restore_upload_budget_rejects_oversized_upload() {
        let error = ensure_restore_upload_within_budget(RestoreKind::Full, 6, 5)
            .expect_err("oversized restore upload rejected");

        assert!(error.to_string().contains("restore budget"));
    }

    async fn parse_full_restore_upload(
        mut multipart: Multipart,
    ) -> crate::error::Result<&'static str> {
        stream_restore_upload_to_tempfile(RestoreKind::Full, &mut multipart).await?;
        Ok("ok")
    }

    fn restore_multipart_body(fields: &[(&str, &str)]) -> (String, Vec<u8>) {
        let boundary = "rustchan-restore-test-boundary".to_string();
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            b"Content-Disposition: form-data; name=\"backup_file\"; filename=\"backup.zip\"\r\n",
        );
        body.extend_from_slice(b"Content-Type: application/zip\r\n\r\nPK\x05\x06restore\r\n");
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        (boundary, body)
    }

    #[tokio::test]
    async fn restore_upload_rejects_oversized_csrf_text_field() {
        let router = Router::new().route("/restore", post(parse_full_restore_upload));
        let oversized = "x".repeat(4097);
        let (boundary, body) = restore_multipart_body(&[("_csrf", &oversized)]);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/restore")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn restore_upload_rejects_duplicate_control_fields() {
        let router = Router::new().route("/restore", post(parse_full_restore_upload));
        let (boundary, body) = restore_multipart_body(&[("_csrf", "one"), ("_csrf", "two")]);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/restore")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
