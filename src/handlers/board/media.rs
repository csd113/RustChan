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

#[cfg(test)]
#[path = "media/performance.rs"]
mod performance;

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
        let url = format!("/{board_short_for_url}/thread/{thread_id}#p{post_id}");
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::safe_board_media_file;
    use super::stale_webm_redirect_path;
    use anyhow::{ensure, Context as _, Result as AnyResult};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request},
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    /// Creates real files below the configured root and an isolated database.
    fn media_fixture() -> AnyResult<(crate::middleware::AppState, tempfile::TempDir, String)> {
        let state = crate::test_support::app_state();
        let directory = tempfile::Builder::new()
            .prefix("t")
            .tempdir_in(&super::CONFIG.upload_dir)?;
        let board = directory
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .context("fixture board name")?
            .to_owned();
        crate::db::create_board(&*state.db.get()?, &board, "Media", "", false).map(|_id| ())?;
        std::fs::create_dir(directory.path().join("thumbs"))?;
        std::fs::write(
            directory.path().join("thumbs/image.webp"),
            b"thumbnail bytes",
        )?;
        std::fs::write(directory.path().join("original.webp"), b"original bytes")?;
        Ok((state, directory, board))
    }

    /// Runs an actual handler request, optionally carrying access and validators.
    async fn request_media(
        router: &Router,
        uri: &str,
        cookie: Option<&str>,
        modified: Option<&str>,
    ) -> AnyResult<super::Response> {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        if let Some(modified) = modified {
            request = request.header(header::IF_MODIFIED_SINCE, modified);
        }
        Ok(router.clone().oneshot(request.body(Body::empty())?).await?)
    }

    #[tokio::test]
    async fn thumbnail_delivery_preserves_http_validators() -> AnyResult<()> {
        let (state, _directory, board) = media_fixture()?;
        let router = Router::new()
            .route("/boards/{*media_path}", get(super::serve_board_media))
            .with_state(state);
        let uri = format!("/boards/{board}/thumbs/image.webp");
        let response = request_media(&router, &uri, None, None).await?;
        ensure!(response.status() == super::StatusCode::OK);
        ensure!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .context("cache policy")?
                == crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA
        );
        ensure!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .context("content type")?
                == "image/webp"
        );
        let modified = response
            .headers()
            .get(header::LAST_MODIFIED)
            .context("validator")?
            .to_str()?
            .to_owned();
        let etag = response
            .headers()
            .get(header::ETAG)
            .context("etag")?
            .to_str()?
            .to_owned();
        ensure!(to_bytes(response.into_body(), 1024).await?.as_ref() == b"thumbnail bytes");
        let conditional = request_media(&router, &uri, None, Some(&modified)).await?;
        ensure!(conditional.status() == super::StatusCode::NOT_MODIFIED);
        ensure!(to_bytes(conditional.into_body(), 1024).await?.is_empty());
        let conditional_etag = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())?,
            )
            .await?;
        ensure!(conditional_etag.status() == super::StatusCode::NOT_MODIFIED);
        ensure!(
            conditional_etag
                .headers()
                .get(header::ETAG)
                .context("conditional etag")?
                .to_str()?
                == etag
        );
        ensure!(to_bytes(conditional_etag.into_body(), 1024)
            .await?
            .is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn thumbnail_delivery_preserves_head_ranges_and_missing_file_recovery() -> AnyResult<()> {
        let (state, directory, board) = media_fixture()?;
        let router = Router::new()
            .route("/boards/{*media_path}", get(super::serve_board_media))
            .with_state(state);
        let uri = format!("/boards/{board}/thumbs/image.webp");
        let head = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(&uri)
                    .body(Body::empty())?,
            )
            .await?;
        ensure!(head.status() == super::StatusCode::OK);
        ensure!(
            head.headers()
                .get(header::CONTENT_LENGTH)
                .context("head size")?
                == "15"
        );
        ensure!(to_bytes(head.into_body(), 1024).await?.is_empty());
        let range = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header(header::RANGE, "bytes=0-4")
                    .body(Body::empty())?,
            )
            .await?;
        ensure!(range.status() == super::StatusCode::PARTIAL_CONTENT);
        ensure!(to_bytes(range.into_body(), 1024).await?.as_ref() == b"thumb");
        let original_uri = format!("/boards/{board}/original.webp");
        let original = request_media(&router, &original_uri, None, None).await?;
        ensure!(to_bytes(original.into_body(), 1024).await?.as_ref() == b"original bytes");
        // No original file, post lookup, or runtime image generation is required.
        std::fs::remove_file(directory.path().join("original.webp"))?;
        ensure!(request_media(&router, &uri, None, None).await?.status() == super::StatusCode::OK);
        let missing = format!("/boards/{board}/thumbs/missing.webp");
        let missing_response = request_media(&router, &missing, None, None).await?;
        ensure!(missing_response.status() == super::StatusCode::NOT_FOUND);
        ensure!(
            missing_response
                .headers()
                .get(header::CACHE_CONTROL)
                .context("missing policy")?
                == crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE
        );
        std::fs::write(
            directory.path().join("thumbs/missing.webp"),
            b"generated later",
        )?;
        ensure!(
            request_media(&router, &missing, None, None).await?.status() == super::StatusCode::OK
        );
        Ok(())
    }

    #[tokio::test]
    async fn conditional_media_requests_recheck_unlock_and_session_revocation() -> AnyResult<()> {
        let (state, _directory, board) = media_fixture()?;
        let router = Router::new()
            .route("/boards/{*media_path}", get(super::serve_board_media))
            .with_state(state.clone());
        let uri = format!("/boards/{board}/thumbs/image.webp");
        let public = request_media(&router, &uri, None, None).await?;
        let modified = public
            .headers()
            .get(header::LAST_MODIFIED)
            .context("validator")?
            .to_str()?
            .to_owned();
        {
            let conn = state.db.get()?;
            conn.execute("UPDATE boards SET access_mode='view_password', access_password_hash='fixture-hash' WHERE short_name=?1", [&board]).map(|_rows| ())?;
            let admin = crate::db::create_admin(&conn, "media-admin", "fixture-hash")?;
            crate::db::create_session(&conn, "media-session", admin, i64::MAX)?;
        }
        let unlock_value = super::super::expected_board_access_cookie_value(&board, "fixture-hash")
            .context("unlock value")?;
        let unlock = format!(
            "{}={unlock_value}",
            super::super::board_access_cookie_name(&board)
        );
        let admin = format!("{}=media-session", super::ADMIN_SESSION_COOKIE);
        let invalid = format!("{}=invalid", super::ADMIN_SESSION_COOKIE);
        for cookie in [None, Some(invalid.as_str())] {
            ensure!(
                request_media(&router, &uri, cookie, Some(&modified))
                    .await?
                    .status()
                    == super::StatusCode::FORBIDDEN
            );
        }
        for cookie in [&unlock, &admin] {
            let response = request_media(&router, &uri, Some(cookie), Some(&modified)).await?;
            ensure!(response.status() == super::StatusCode::NOT_MODIFIED);
            ensure!(
                response
                    .headers()
                    .get(header::CACHE_CONTROL)
                    .context("private policy")?
                    == crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE
            );
        }
        state.db.get()?.execute_batch(
            "DELETE FROM admin_sessions; UPDATE boards SET access_password_hash='changed';",
        )?;
        for cookie in [&unlock, &admin] {
            ensure!(
                request_media(&router, &uri, Some(cookie), Some(&modified))
                    .await?
                    .status()
                    == super::StatusCode::FORBIDDEN
            );
        }
        state
            .db
            .get()?
            .execute_batch("UPDATE boards SET access_mode='post_password';")?;
        ensure!(
            request_media(&router, &uri, Some(&invalid), Some(&modified))
                .await?
                .status()
                == super::StatusCode::NOT_MODIFIED
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_media_redirects_preserve_status_and_safe_targets() -> AnyResult<()> {
        let (state, directory, board) = media_fixture()?;
        std::fs::write(directory.path().join("clip.webm"), b"video")?;
        std::fs::write(directory.path().join("thumbs/audio.png"), b"waveform")?;
        let router = Router::new()
            .route("/boards/{*media_path}", get(super::serve_board_media))
            .with_state(state);
        for (requested, target, status) in [
            (
                "clip.mp4",
                "clip.webm",
                super::StatusCode::PERMANENT_REDIRECT,
            ),
            (
                "clip.mkv",
                "clip.webm",
                super::StatusCode::PERMANENT_REDIRECT,
            ),
            (
                "thumbs/audio.svg",
                "thumbs/audio.png",
                super::StatusCode::TEMPORARY_REDIRECT,
            ),
        ] {
            let uri = format!("/boards/{board}/{requested}");
            let response = request_media(&router, &uri, None, None).await?;
            ensure!(response.status() == status);
            ensure!(
                response
                    .headers()
                    .get(header::LOCATION)
                    .context("redirect location")?
                    .to_str()?
                    == format!("/boards/{board}/{target}")
            );
        }
        Ok(())
    }

    #[test]
    fn stale_waveform_svg_redirects_only_to_an_existing_generated_png() -> AnyResult<()> {
        let dir = tempfile::tempdir()?;
        let thumbs = dir.path().join("test/thumbs");
        std::fs::create_dir_all(&thumbs)?;
        let path = "test/thumbs/clip.svg";
        ensure!(super::stale_waveform_redirect_path(dir.path(), path).is_none());
        std::fs::write(thumbs.join("clip.png"), b"fixture")?;
        ensure!(
            super::stale_waveform_redirect_path(dir.path(), path).as_deref()
                == Some("/boards/test/thumbs/clip.png")
        );
        ensure!(super::stale_waveform_redirect_path(dir.path(), "test/clip.svg").is_none());
        ensure!(
            super::stale_waveform_redirect_path(dir.path(), "../test/thumbs/clip.svg").is_none()
        );
        Ok(())
    }

    #[test]
    fn stale_mp4_redirect_path_accepts_valid_webm_sibling() -> AnyResult<()> {
        let tempdir = tempfile::tempdir().context("create temporary upload root")?;
        let upload_root = tempdir.path().join("uploads");
        let board_dir = upload_root.join("test");
        std::fs::create_dir_all(&board_dir).context("create board directory")?;
        std::fs::write(board_dir.join("clip.webm"), b"webm").context("write WebM fixture")?;

        for path in ["test/clip.mp4", "test/clip.mkv", "test/clip.MKV"] {
            ensure!(
                stale_webm_redirect_path(&upload_root, path).as_deref()
                    == Some("/boards/test/clip.webm")
            );
        }
        ensure!(stale_webm_redirect_path(&upload_root, "test/clip.wav").is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stale_mp4_redirect_path_rejects_symlink_fallback_escape() -> AnyResult<()> {
        use std::os::unix::fs as unix_fs;

        let tempdir = tempfile::tempdir().context("create temporary upload root")?;
        let upload_root = tempdir.path().join("uploads");
        let board_dir = upload_root.join("test");
        let outside = tempdir.path().join("outside");
        std::fs::create_dir_all(&board_dir).context("create board directory")?;
        std::fs::create_dir_all(&outside).context("create outside directory")?;
        std::fs::write(outside.join("clip.webm"), b"webm").context("write outside WebM fixture")?;
        unix_fs::symlink(&outside, board_dir.join("link")).context("create escaping symlink")?;

        ensure!(stale_webm_redirect_path(&upload_root, "test/link/clip.mp4").is_none());
        ensure!(stale_webm_redirect_path(&upload_root, "test/link/clip.mkv").is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn board_media_file_rejects_symlink_original_escape() -> AnyResult<()> {
        use std::os::unix::fs as unix_fs;

        let tempdir = tempfile::tempdir().context("create temporary upload root")?;
        let upload_root = tempdir.path().join("uploads");
        let board_dir = upload_root.join("test");
        let outside = tempdir.path().join("outside");
        std::fs::create_dir_all(&board_dir).context("create board directory")?;
        std::fs::create_dir_all(&outside).context("create outside directory")?;
        std::fs::write(outside.join("clip.mp4"), b"mp4").context("write outside MP4 fixture")?;
        unix_fs::symlink(&outside, board_dir.join("link")).context("create escaping symlink")?;

        ensure!(safe_board_media_file(&upload_root, "test/link/clip.mp4").is_err());
        Ok(())
    }
}
