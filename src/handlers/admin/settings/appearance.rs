use super::{
    admin_panel_error_redirect_anchor, admin_panel_redirect_anchor,
    admin_panel_redirect_anchor_open, check_admin_csrf_jar, format_favicon_upload_error,
    read_limited_upload_bytes, read_text_field, require_admin_post_origin_and_csrf,
    require_admin_session_sid, require_same_origin_request, AppError, AppState, CookieJar, Form,
    HeaderMap, Multipart, Response, Result, State, MAX_FAVICON_UPLOAD_BYTES, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;

#[derive(Deserialize)]
/// Form fields accepted by the clear board favicon request.
pub(crate) struct ClearBoardFaviconForm {
    /// The board identifier.
    board_id: i64,
    #[serde(rename = "_csrf")]
    /// The submitted CSRF token, if present.
    csrf: Option<String>,
}

/// Handles the clear board favicon override request.
pub(crate) async fn clear_board_favicon_override(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ClearBoardFaviconForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let board_short = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![form.board_id],
                |row| row.get(0),
            )?;
            crate::favicon::clear_board_favicon(&board_short)?;
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(admin_panel_redirect_anchor_open(
        &format!("Board /{board_short}/ favicon override cleared."),
        &format!("board-appearance-{board_short}"),
        "board-banners",
    )
    .into_response())
}

/// Handles the update site favicon request.
pub(crate) async fn update_site_favicon(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_same_origin_request(&headers, Some(peer))?;

    let mut csrf = None;
    let mut favicon_bytes: Option<Vec<u8>> = None;

    loop {
        let next_field = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let Some(field) = next_field else {
            break;
        };
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("favicon") => {
                let bytes = read_limited_upload_bytes(field, MAX_FAVICON_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    favicon_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    check_admin_csrf_jar(&jar, csrf.as_deref())?;
    let favicon_bytes =
        favicon_bytes.ok_or_else(|| AppError::BadRequest("No favicon file uploaded.".into()))?;

    let favicon_result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            crate::favicon::write_favicon_set(
                crate::favicon::FaviconScope::Global,
                &favicon_bytes,
            )?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match favicon_result {
        Ok(()) => Ok(
            admin_panel_redirect_anchor("Global favicon updated.", "site-settings").into_response(),
        ),
        Err(AppError::Internal(error)) => Ok(admin_panel_error_redirect_anchor(
            &format_favicon_upload_error(&error),
            "site-settings",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

/// Handles the update board favicon request.
pub(crate) async fn update_board_favicon(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    mut multipart: Multipart,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_same_origin_request(&headers, Some(peer))?;

    let mut csrf = None;
    let mut board_id = None;
    let mut favicon_bytes: Option<Vec<u8>> = None;

    loop {
        let next_field = multipart
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(e.to_string()))?;
        let Some(field) = next_field else {
            break;
        };
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("board_id") => {
                board_id = read_text_field(field).await?.trim().parse::<i64>().ok();
            }
            Some("favicon") => {
                let bytes = read_limited_upload_bytes(field, MAX_FAVICON_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    favicon_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    check_admin_csrf_jar(&jar, csrf.as_deref())?;
    let board_id = board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
    let favicon_bytes =
        favicon_bytes.ok_or_else(|| AppError::BadRequest("No favicon file uploaded.".into()))?;

    let favicon_result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            let board_short: String = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get(0),
            )?;
            crate::favicon::write_favicon_set(
                crate::favicon::FaviconScope::Board(&board_short),
                &favicon_bytes,
            )?;
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    match favicon_result {
        Ok(board_short) => Ok(admin_panel_redirect_anchor_open(
            &format!("Board /{board_short}/ favicon updated."),
            &format!("board-appearance-{board_short}"),
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(admin_panel_error_redirect_anchor(
            &format_favicon_upload_error(&error),
            "site-settings",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

// ─── POST /admin/site/settings ────────────────────────────────────────────────
