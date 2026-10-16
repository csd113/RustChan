use super::{
    board_access_cookie_from_jar, db, load_board_access_context, templates, unlock_redirect_url,
    user_preferences_from_jar, AppError, AppState, CookieJar, HeaderMap, Path, Redirect, Response,
    Result, State, StatusCode, ADMIN_SESSION_COOKIE, CONFIG,
};
use axum::http::header::{
    HeaderValue, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::response::IntoResponse as _;

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
        Some("mkv") => Some("video/x-matroska"),
        Some("mp3") => Some("audio/mpeg"),
        Some("ogg" | "oga") => Some("audio/ogg"),
        Some("opus") => Some("audio/opus"),
        Some("flac") => Some("audio/flac"),
        Some("wav") => Some("audio/wav"),
        Some("m4a") => Some("audio/mp4"),
        Some("aac") => Some("audio/aac"),
        Some("pdf") => Some("application/pdf"),
        _ => None,
    }
}

fn is_generated_svg_placeholder_thumb(media_path: &str) -> bool {
    let path = std::path::Path::new(media_path);
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
        && path
            .components()
            .nth(1)
            .is_some_and(|part| part.as_os_str() == "thumbs")
}

fn safe_board_media_file(
    base: &std::path::Path,
    media_path: &str,
) -> anyhow::Result<std::path::PathBuf> {
    crate::utils::fs_security::existing_regular_file_child(base, media_path)
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn stale_webm_redirect_path(base: &std::path::Path, media_path: &str) -> Option<String> {
    let path = std::path::Path::new(media_path);
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4") || ext.eq_ignore_ascii_case("mkv"))
    {
        return None;
    }
    let stem = media_path.get(..media_path.len().saturating_sub(4))?;
    let webm_path = format!("{stem}.webm");
    safe_board_media_file(base, &webm_path)
        .ok()
        .map(|_completed_value| ())?;
    let location = format!("/boards/{webm_path}");
    HeaderValue::from_str(&location)
        .ok()
        .map(|_completed_value| ())?;
    axum::http::Uri::try_from(location.as_str())
        .ok()
        .map(|_completed_value| ())?;
    Some(location)
}

/// A page can still request its SVG just after the worker replaces that file.
/// Resolve only generated thumbnail paths and validate the PNG under the root.
fn stale_waveform_redirect_path(base: &std::path::Path, media_path: &str) -> Option<String> {
    if !is_generated_svg_placeholder_thumb(media_path) {
        return None;
    }
    let png_path = std::path::Path::new(media_path).with_extension("png");
    let relative = png_path.to_str()?;
    safe_board_media_file(base, relative)
        .ok()
        .map(|_completed_value| ())?;
    let location = format!("/boards/{relative}");
    // Validate untrusted stored paths before constructing an HTTP redirect.
    HeaderValue::from_str(&location)
        .ok()
        .map(|_completed_value| ())?;
    axum::http::Uri::try_from(location.as_str())
        .ok()
        .map(|_completed_value| ())?;
    Some(location)
}

/// Validated delivery target, resolved on a blocking worker after authorization.
enum BoardMediaTarget {
    File(std::path::PathBuf),
    Redirect(Redirect),
    Missing,
}

/// Preserve legacy URLs without doing filesystem work on the async executor.
fn stale_media_redirect(base: &std::path::Path, media_path: &str) -> Option<Redirect> {
    stale_waveform_redirect_path(base, media_path)
        .map(|path| Redirect::temporary(&path))
        .or_else(|| {
            stale_webm_redirect_path(base, media_path).map(|path| Redirect::permanent(&path))
        })
}

/// Validate every component; only an absent file may resolve to a legacy sibling.
fn resolve_board_media_target(base: &std::path::Path, media_path: &str) -> BoardMediaTarget {
    match safe_board_media_file(base, media_path) {
        Ok(path) => BoardMediaTarget::File(path),
        Err(error) if is_not_found_error(&error) => stale_media_redirect(base, media_path)
            .map_or(BoardMediaTarget::Missing, BoardMediaTarget::Redirect),
        Err(_) => BoardMediaTarget::Missing,
    }
}

/// A missing/generated-later file or denied request must not be negatively cached.
fn media_error_response(status: StatusCode) -> Response {
    let mut response = status.into_response();
    crate::cache::set_cache_control(
        response.headers_mut(),
        crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE,
    );
    response
}

// Legacy `.mp4` and `.mkv` links redirect to transcoded `.webm` files; all
// other validated paths are served directly.

#[expect(
    clippy::too_many_lines,
    reason = "path validation, access policy, legacy redirect handling, and file response form one request"
)]
pub(in crate::server) async fn serve_board_media(
    State(state): State<AppState>,
    Path(media_path): Path<String>,
    jar: CookieJar,
    req: axum::extract::Request,
) -> Response {
    use tower::ServiceExt as _;
    use tower_http::services::ServeFile;

    // Reject path-traversal attempts and absolute-path escapes.
    if media_path.contains("..") || media_path.starts_with('/') {
        return media_error_response(StatusCode::BAD_REQUEST);
    }

    let Some(board_short) = media_path.split('/').next().filter(|part| !part.is_empty()) else {
        return media_error_response(StatusCode::NOT_FOUND);
    };

    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, board_short);
    let (is_protected_board, target) = match tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.to_owned();
        let media_path = media_path.clone();
        move || -> Result<(bool, BoardMediaTarget)> {
            let conn = pool.get()?;
            let board = db::get_board_by_short(&conn, &board_short)?
                .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
            // Public/post-password boards and valid unlock cookies already grant
            // viewing. Only a remaining password gate needs a session lookup.
            if !super::can_view_board(&board, false, access_cookie.as_deref())
                && !super::posting::is_admin_session(&conn, admin_session_id.as_deref())
            {
                return Err(AppError::Forbidden("Board access denied".to_owned()));
            }
            let is_protected_board = board.access_mode.requires_view_password();
            // Release the pooled connection before the synchronous path checks.
            drop(conn);
            let target =
                resolve_board_media_target(std::path::Path::new(&CONFIG.upload_dir), &media_path);
            Ok((is_protected_board, target))
        }
    })
    .await
    {
        Ok(Ok(context)) => context,
        Ok(Err(AppError::NotFound(_))) => return media_error_response(StatusCode::NOT_FOUND),
        Ok(Err(_)) | Err(_) => return media_error_response(StatusCode::FORBIDDEN),
    };

    let has_version = req
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part.starts_with("v=")));
    let is_board_favicon = std::path::Path::new(&media_path)
        .components()
        .nth(1)
        .is_some_and(|part| part.as_os_str() == "_favicon");
    let updated_target = match target {
        BoardMediaTarget::File(path) => path,
        BoardMediaTarget::Redirect(redirect) => return redirect.into_response(),
        BoardMediaTarget::Missing => return media_error_response(StatusCode::NOT_FOUND),
    };
    let is_pdf = updated_target
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));

    // File present — forward the real request (with Range, ETag, etc.) to
    // ServeFile so it can respond with 206 Partial Content when needed.
    // iOS Safari requires Range request support to play video — dropping
    // the request headers caused it to receive 200 instead of 206 and
    // refuse playback on videos it tried to stream in chunks.
    let req = req.map(|_| axum::body::Body::empty());
    let response = ServeFile::new(&updated_target).oneshot(req).await;
    // A worker may replace the SVG/video between validation and open.
    if response
        .as_ref()
        .is_ok_and(|resp| resp.status() == StatusCode::NOT_FOUND)
    {
        let stale_media_path = media_path.clone();
        let redirect = tokio::task::spawn_blocking(move || {
            stale_media_redirect(std::path::Path::new(&CONFIG.upload_dir), &stale_media_path)
        })
        .await;
        if let Ok(Some(redirect)) = redirect {
            return redirect.into_response();
        }
    }
    response.map_or_else(
        |_| media_error_response(StatusCode::INTERNAL_SERVER_ERROR),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            // Do not give a failed open (including a delete race) a year's TTL.
            let cache_control =
                if resp.status().is_success() || resp.status() == StatusCode::NOT_MODIFIED {
                    board_media_cache_control(is_protected_board, is_board_favicon, has_version)
                } else {
                    crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE
                };
            crate::cache::set_cache_control(resp.headers_mut(), cache_control);
            if is_generated_svg_placeholder_thumb(&media_path) {
                drop(
                    resp.headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static("image/svg+xml")),
                );
                drop(
                    resp.headers_mut()
                        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
                );
                drop(resp.headers_mut().insert(
                    CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("default-src 'none'; script-src 'none'"),
                ));
            } else if let Some(ct) = media_content_type(&updated_target) {
                drop(
                    resp.headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static(ct)),
                );
            } else {
                drop(resp.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                ));
                drop(
                    resp.headers_mut()
                        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
                );
                let filename = updated_target
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("download.bin")
                    .replace(['\\', '"'], "_");
                let disposition = format!("attachment; filename=\"{filename}\"");
                let parsed_disposition = HeaderValue::from_str(&disposition);
                if let Ok(value) = parsed_disposition {
                    drop(resp.headers_mut().insert(CONTENT_DISPOSITION, value));
                }
            }
            if is_pdf {
                apply_pdf_embed_headers(resp.headers_mut());
            }
            resp.into_response()
        },
    )
}

const fn board_media_cache_control(
    is_protected_board: bool,
    is_replaceable_asset: bool,
    has_version: bool,
) -> &'static str {
    if is_protected_board {
        return crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE;
    }
    if is_replaceable_asset && !has_version {
        crate::cache::CACHE_CONTROL_STATIC_SHORT
    } else {
        crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA
    }
}

fn apply_pdf_embed_headers(headers: &mut HeaderMap) {
    drop(headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN")));
    drop(headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; frame-ancestors 'self'; sandbox allow-same-origin allow-scripts",
        ),
    ));
}

// GET /api/post/{board}/{post_id}
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

pub(in crate::server) async fn api_post_preview(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    let user_preferences = user_preferences_from_jar(&jar);
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let board_short = board_short.clone();
        move || -> Result<Option<(String, i64)>> {
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
                    let html = templates::render_post(
                        &p,
                        &board_short,
                        "",
                        templates::thread::RenderPostOpts {
                            show_delete: false,
                            is_admin: false,
                            admin_csrf_token: None,
                            show_media: true,
                            allow_editing: false, // no edit link in read-only preview
                            allow_self_delete: false,
                            owned_post_controls: None,
                            show_poster_ids: false,
                            collapse_greentext: board.collapse_greentext,
                            thread_state: None,
                            thread_op_id: None,
                            video_audio_muted: user_preferences.video_audio_muted,
                        },
                        0, // no edit window
                    );
                    Ok(Some((html, thread_id)))
                }
            }
        }
    })
    .await;

    let json_ct = [(CONTENT_TYPE, "application/json")];

    match result {
        Ok(Ok(Some((html, thread_id)))) => {
            let body =
                serde_json::to_string(&serde_json::json!({ "html": html, "thread_id": thread_id }))
                    .unwrap_or_else(|_| r#"{"html":"","thread_id":0}"#.to_owned());
            (StatusCode::OK, json_ct, body).into_response()
        }
        Ok(Ok(None)) => {
            let body = r#"{"error":"not found"}"#.to_owned();
            (StatusCode::NOT_FOUND, json_ct, body).into_response()
        }
        _ => {
            let body = r#"{"error":"internal error"}"#.to_owned();
            (StatusCode::INTERNAL_SERVER_ERROR, json_ct, body).into_response()
        }
    }
}

// GET /{board}/post/{post_id}
// Canonical redirect for `>>>/board/N` links.  Resolves the global post ID to
// its containing thread and issues a 302 to /{board}/thread/{thread_id}#p{post_id}.
//
// Users clicking a cross-board quotelink land here on the first click; after
// the first hover preview the JS upgrades the href in-place so subsequent
// clicks go directly to the thread anchor without a server round-trip.

pub(in crate::server) async fn redirect_to_post(
    State(state): State<AppState>,
    Path((board_short, post_id)): Path<(String, i64)>,
    jar: CookieJar,
) -> impl axum::response::IntoResponse {
    let board_short_for_url = board_short.clone();
    let admin_session_id = jar
        .get(ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let access_cookie = board_access_cookie_from_jar(&jar, &board_short);
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<(Option<i64>, bool)> {
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
        let url = format!("/{board_short_for_url}/thread/{thread_id}?post={post_id}#p{post_id}");
        Redirect::to(&url).into_response()
    } else if matches!(result, Ok(Ok((None, true)))) {
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
        AppError::NotFound(format!("Post #{post_id} not found. It may have been deleted or the board was restored from a backup."))
        .into_response()
    }
}
