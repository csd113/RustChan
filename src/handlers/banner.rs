use crate::{
    banner, db,
    error::{AppError, Result},
    handlers::board::{
        board_access_cookie_from_jar, current_theme_from_jar, ensure_csrf_for_request,
    },
    middleware::{AppState, SecureCookieContext},
    templates,
};
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use tower::ServiceExt as _;
use tower_http::services::ServeFile;

/// Versioned cache control used by this handler.
const VERSIONED_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA;
/// Unversioned cache control used by this handler.
const UNVERSIONED_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_STATIC_SHORT;

#[derive(Deserialize, Default)]
pub(in crate::server) struct ExternalBannerQuery {
    pub return_to: Option<String>,
}

fn load_accessible_banner_asset(
    conn: &rusqlite::Connection,
    banner_id: i64,
    jar: &CookieJar,
    admin_session_id: Option<&str>,
) -> Result<(crate::models::BannerAsset, bool)> {
    let asset = db::get_banner_asset(conn, banner_id)?
        .ok_or_else(|| AppError::NotFound("Banner not found.".into()))?;
    if asset.scope == crate::models::BannerScope::Board {
        let board_short = asset
            .board_short
            .as_deref()
            .ok_or_else(|| AppError::NotFound("Banner not found.".into()))?;
        let access_cookie = board_access_cookie_from_jar(jar, board_short);
        let access_context = crate::handlers::board::load_board_access_context(
            conn,
            board_short,
            admin_session_id,
            access_cookie.as_deref(),
        )?;
        if !access_context.can_view {
            return Err(AppError::Forbidden("Banner is not available.".into()));
        }
        return Ok((
            asset,
            access_context.board.access_mode.requires_view_password(),
        ));
    }
    Ok((asset, false))
}

pub(in crate::server) async fn serve_banner_asset(
    State(state): State<AppState>,
    Path(banner_id): Path<i64>,
    jar: CookieJar,
    req: Request,
) -> Response {
    let has_version = req
        .uri()
        .query()
        .is_some_and(|query| query.split('&').any(|part| part.starts_with("v=")));
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());

    let asset = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar = jar.clone();
        move || -> Result<(crate::models::BannerAsset, bool)> {
            let conn = pool.get()?;
            load_accessible_banner_asset(&conn, banner_id, &jar, admin_session_id.as_deref())
        }
    })
    .await;

    let (asset, is_protected_board_asset) = match asset {
        Ok(Ok(asset)) => asset,
        Ok(Err(AppError::NotFound(_))) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(AppError::Forbidden(_))) => return StatusCode::FORBIDDEN.into_response(),
        Ok(Err(_)) | Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let Ok(path) = banner::banner_asset_path(&asset) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let root = match asset.scope {
        crate::models::BannerScope::Global => banner::global_banner_dir(),
        crate::models::BannerScope::Home => banner::home_banner_dir(),
        crate::models::BannerScope::Board => {
            let Some(board_short) = asset.board_short.as_deref() else {
                return StatusCode::NOT_FOUND.into_response();
            };
            banner::board_banner_dir(board_short)
        }
    };
    let Ok(path) = crate::utils::fs_security::canonical_child_of(&root, &path).and_then(|path| {
        crate::utils::fs_security::assert_regular_file_no_symlink(&path)?;
        Ok(path)
    }) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = banner::banner_asset_content_type(&path);

    let req = req.map(|_| axum::body::Body::empty());
    let response = ServeFile::new(path).oneshot(req).await.map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            let _previous_value = resp
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            crate::cache::set_cache_control(
                resp.headers_mut(),
                if is_protected_board_asset {
                    crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE
                } else if has_version {
                    VERSIONED_CACHE_CONTROL
                } else {
                    UNVERSIONED_CACHE_CONTROL
                },
            );
            resp.into_response()
        },
    );
    response
}

pub(in crate::server) async fn external_banner_warning_page(
    State(state): State<AppState>,
    Path(banner_id): Path<i64>,
    Query(query): Query<ExternalBannerQuery>,
    jar: CookieJar,
    req_headers: HeaderMap,
    peer: SecureCookieContext,
) -> Result<Response> {
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf_for_request(jar, &req_headers, peer);
    let return_to = banner::safe_return_to(query.return_to.as_deref().unwrap_or("/"));
    let asset = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar = jar.clone();
        move || -> Result<crate::models::BannerAsset> {
            let conn = pool.get()?;
            let settings = banner::load_site_banner_settings(&conn);
            if !settings.allow_external_links {
                return Err(AppError::Forbidden(
                    "External banner links are disabled.".into(),
                ));
            }
            let (asset, _) =
                load_accessible_banner_asset(&conn, banner_id, &jar, admin_session_id.as_deref())?;
            if asset.target_type != crate::models::BannerTargetType::ExternalUrl
                || banner::normalize_external_url(&asset.target_value).is_none()
            {
                return Err(AppError::BadRequest(
                    "Banner does not point to a valid external URL.".into(),
                ));
            }
            Ok(asset)
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    let body = format!(
        r#"<div class="page-box external-banner-warning">
<h1>External Banner Link</h1>
<p>This link will redirect you to an external website.</p>
<p><code>{url}</code></p>
<div class="external-banner-actions">
  <a class="btn" href="/banner/external/{banner_id}/continue?return_to={return_to}">Continue</a>
  <a class="btn" href="{back}">Back</a>
</div>
</div>"#,
        url = crate::utils::sanitize::escape_html(&asset.target_value),
        banner_id = asset.id,
        return_to = templates::urlencoding_simple(&return_to),
        back = crate::utils::sanitize::escape_html(&return_to),
    );
    let boards = templates::live_boards();
    let html = templates::base_layout(
        "external banner link",
        None,
        &body,
        &csrf,
        boards.as_ref(),
        current_theme.as_deref(),
        None,
        false,
        "/banner/external",
    );
    let mut response = Html(html).into_response();
    crate::cache::set_cache_control(
        response.headers_mut(),
        crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE,
    );
    Ok((jar, response).into_response())
}

pub(in crate::server) async fn external_banner_continue(
    State(state): State<AppState>,
    Path(banner_id): Path<i64>,
    Query(_query): Query<ExternalBannerQuery>,
    jar: CookieJar,
) -> Result<Response> {
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_owned());
    let target = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar = jar.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let settings = banner::load_site_banner_settings(&conn);
            if !settings.allow_external_links {
                return Err(AppError::Forbidden(
                    "External banner links are disabled.".into(),
                ));
            }
            let (asset, _) =
                load_accessible_banner_asset(&conn, banner_id, &jar, admin_session_id.as_deref())?;
            if asset.target_type != crate::models::BannerTargetType::ExternalUrl {
                return Err(AppError::BadRequest("Banner is not external.".into()));
            }
            banner::normalize_external_url(&asset.target_value).ok_or_else(|| {
                AppError::BadRequest("Banner does not point to a valid external URL.".into())
            })
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;
    Ok(Redirect::to(&target).into_response())
}
