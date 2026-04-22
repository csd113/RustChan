// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

fn media_content_type(path: &std::path::Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ico") => Some("image/x-icon"),
        Some("webp") => Some("image/webp"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("heic") => Some("image/heic"),
        Some("heif") => Some("image/heif"),
        Some("bmp") => Some("image/bmp"),
        Some("tiff" | "tif") => Some("image/tiff"),
        // SVG is intentionally omitted: serving SVG inline allows stored XSS via
        // embedded <script> tags. SVGs are not accepted as uploads (detect_mime_type
        // rejects image/svg+xml) so this arm would never match, but the explicit
        // absence here documents the security decision.
        Some("webm") => Some("video/webm"),
        Some("mp4") => Some("video/mp4"),
        Some("mp3") => Some("audio/mpeg"),
        Some("ogg") => Some("audio/ogg"),
        Some("flac") => Some("audio/flac"),
        Some("wav") => Some("audio/wav"),
        Some("m4a") => Some("audio/mp4"),
        Some("aac") => Some("audio/aac"),
        _ => None,
    }
}

// Replaces the former nest_service(ServeDir) so we can intercept stale .mp4

// links (created before the background transcoder replaced them with .webm)
// and issue a permanent redirect. All other paths are served via ServeFile.

pub async fn serve_board_media(
    State(state): State<AppState>,
    Path(media_path): Path<String>,
    jar: CookieJar,
    req: axum::extract::Request,
) -> Response {
    use axum::http::header::CACHE_CONTROL;
    use axum::http::StatusCode;
    use std::path::PathBuf;
    use tower::ServiceExt;
    use tower_http::services::ServeFile;

    // Reject path-traversal attempts and absolute-path escapes.
    if media_path.contains("..") || media_path.starts_with('/') {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let Some(board_short) = media_path.split('/').next().filter(|part| !part.is_empty()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, board_short);
    let access_context = match tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.to_string();
        move || -> Result<BoardAccessContext> {
            let conn = pool.get()?;
            load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )
        }
    })
    .await
    {
        Ok(Ok(context)) => context,
        Ok(Err(AppError::NotFound(_))) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(_)) | Err(_) => return StatusCode::FORBIDDEN.into_response(),
    };

    if !access_context.can_view {
        return StatusCode::FORBIDDEN.into_response();
    }

    let base = PathBuf::from(&CONFIG.upload_dir);
    let target = base.join(&media_path);
    let has_version = req
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part.starts_with("v=")));
    let is_board_favicon = std::path::Path::new(&media_path)
        .components()
        .nth(1)
        .is_some_and(|part| part.as_os_str() == "_favicon");

    // Verify the resolved path is still inside the upload directory.
    // This catches any edge cases that slip past the string checks above
    // (e.g. symlinks, exotic percent-encoding handled by the OS).
    if !target.starts_with(&base) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    if target.exists() {
        // File present — forward the real request (with Range, ETag, etc.) to
        // ServeFile so it can respond with 206 Partial Content when needed.
        // iOS Safari requires Range request support to play video — dropping
        // the request headers caused it to receive 200 instead of 206 and
        // refuse playback on videos it tried to stream in chunks.
        let req = req.map(|_| axum::body::Body::empty());
        ServeFile::new(&target).oneshot(req).await.map_or_else(
            |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            |resp| {
                use axum::http::header::{
                    HeaderValue, CONTENT_DISPOSITION, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS,
                };
                let mut resp = resp.map(axum::body::Body::new);
                if is_board_favicon {
                    resp.headers_mut().insert(
                        CACHE_CONTROL,
                        HeaderValue::from_static(board_media_cache_control(has_version)),
                    );
                }
                if let Some(ct) = media_content_type(&target) {
                    resp.headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static(ct));
                } else {
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("application/octet-stream"),
                    );
                    resp.headers_mut()
                        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
                    let filename = target
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("download.bin")
                        .replace(['\\', '"'], "_");
                    if let Ok(value) =
                        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    {
                        resp.headers_mut().insert(CONTENT_DISPOSITION, value);
                    }
                }
                resp.into_response()
            },
        )
    } else if std::path::Path::new(&media_path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        // MP4 was transcoded away — redirect permanently to the .webm sibling.
        let webm_path_str = format!("{}.webm", &media_path[..media_path.len().saturating_sub(4)]);
        let webm_abs = base.join(&webm_path_str);
        if webm_abs.exists() {
            Redirect::permanent(&format!("/boards/{webm_path_str}")).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

const fn board_media_cache_control(has_version: bool) -> &'static str {
    if has_version {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache, must-revalidate"
    }
}

// ─── GET /api/post/{board}/{post_id} ──────────────────────────────────────────
//
// Lightweight JSON endpoint for cross-board quotelink hover previews.
//
// `post_id` is the **global** post ID (the AUTOINCREMENT primary key of the
// `posts` table).  The board name is used only to validate ownership — a link
// like >>>/tech/12345 will 404 if post 12345 actually lives on /b/, preventing
// cross-board information leakage.
//
// Response on success:
//   { "html": "<div class=\"post …\">…</div>", "thread_id": 42 }
// The `thread_id` field lets the client update the link's href to the canonical
// /{board}/thread/{thread_id}#p{post_id} URL after the first hover.
//
// Response on failure: 404 { "error": "not found" }

pub async fn api_post_preview(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> crate::error::Result<Option<(String, i64)>> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Ok(None);
            }

            // Fetch the post, validating it belongs to this board.
            let board = access_context.board;
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            match post {
                None => Ok(None),
                Some(p) => {
                    let thread_id = p.thread_id;
                    let html = crate::templates::render_post(
                        &p,
                        &board_short,
                        "",
                        crate::templates::thread::RenderPostOpts {
                            show_delete: false,
                            is_admin: false,
                            show_media: true,
                            allow_editing: false, // no edit link in read-only preview
                            show_poster_ids: false,
                            collapse_greentext: board.collapse_greentext,
                            thread_state: None,
                            thread_op_id: None,
                        },
                        0, // no edit window
                    );
                    Ok(Some((html, thread_id)))
                }
            }
        }
    })
    .await;

    let json_ct = [(header::CONTENT_TYPE, "application/json")];

    match result {
        Ok(Ok(Some((html, thread_id)))) => {
            let body =
                serde_json::to_string(&serde_json::json!({ "html": html, "thread_id": thread_id }))
                    .unwrap_or_else(|_| r#"{"html":"","thread_id":0}"#.to_string());
            (axum::http::StatusCode::OK, json_ct, body).into_response()
        }
        Ok(Ok(None)) => {
            let body = r#"{"error":"not found"}"#.to_string();
            (axum::http::StatusCode::NOT_FOUND, json_ct, body).into_response()
        }
        _ => {
            let body = r#"{"error":"internal error"}"#.to_string();
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, json_ct, body).into_response()
        }
    }
}

// ─── GET /{board}/post/{post_id} ──────────────────────────────────────────────
//
// Canonical redirect for `>>>/board/N` links.  Resolves the global post ID to
// its containing thread and issues a 302 to /{board}/thread/{thread_id}#p{post_id}.
//
// Users clicking a cross-board quotelink land here on the first click; after
// the first hover preview the JS upgrades the href in-place so subsequent
// clicks go directly to the thread anchor without a server round-trip.

pub async fn redirect_to_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    use axum::response::Redirect;

    let board_short_for_url = board_short.clone();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> crate::error::Result<(Option<i64>, bool)> {
            let conn = pool.get()?;
            let access_context = load_board_access_context(
                &conn,
                &board_short,
                admin_session_id.as_deref(),
                access_cookie.as_deref(),
            )?;
            if !access_context.can_view {
                return Ok((None, true));
            }
            let post = db::get_post_on_board(&conn, &board_short, post_id)?;
            Ok((post.map(|p| p.thread_id), false))
        }
    })
    .await;

    if let Ok(Ok((Some(thread_id), _))) = result {
        let url = format!("/{board_short_for_url}/thread/{thread_id}#p{post_id}");
        Redirect::to(&url).into_response()
    } else if let Ok(Ok((None, true))) = result {
        Redirect::to(&unlock_redirect_url(
            &board_short_for_url,
            &format!("/{board_short_for_url}/post/{post_id}"),
        ))
        .into_response()
    } else {
        // Post not found or wrong board — render the error page template
        // so the user gets a readable message instead of a blank HTTP 404.
        // This is the fallback path when JavaScript is disabled or when
        // a user manually navigates to a quotelink URL after a board
        // restore that assigned new IDs to the restored posts.
        let html = crate::templates::error_page(
            404,
            &format!("Post #{post_id} not found. It may have been deleted or the board was restored from a backup."),
        );
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::response::Html(html),
        )
            .into_response()
    }
}

// ─── POST /appeal ─────────────────────────────────────────────────────────────
// Banned users submit a brief appeal message here.
// Appeals appear in the admin panel under // ban appeals.
