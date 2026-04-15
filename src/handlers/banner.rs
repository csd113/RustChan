use crate::{
    banner, db,
    error::{AppError, Result},
    handlers::board::{board_access_cookie_from_jar, current_theme_from_jar, ensure_csrf},
    middleware::AppState,
    templates,
};
use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const VERSIONED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const UNVERSIONED_CACHE_CONTROL: &str = "no-cache, must-revalidate";

#[derive(Deserialize, Default)]
pub struct ExternalBannerQuery {
    pub return_to: Option<String>,
}

pub async fn serve_banner_asset(
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
        .map(|cookie| cookie.value().to_string());

    let access = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar = jar.clone();
        move || -> Result<Option<(crate::models::BannerAsset, bool)>> {
            let conn = pool.get()?;
            let Some(asset) = db::get_banner_asset(&conn, banner_id)? else {
                return Ok(None);
            };
            if asset.scope == crate::models::BannerScope::Board {
                let Some(board_short) = asset.board_short.as_deref() else {
                    return Ok(None);
                };
                let access_cookie = board_access_cookie_from_jar(&jar, board_short);
                let access_context = crate::handlers::board::load_board_access_context(
                    &conn,
                    board_short,
                    admin_session_id.as_deref(),
                    access_cookie.as_deref(),
                )?;
                Ok(Some((asset, access_context.can_view)))
            } else {
                Ok(Some((asset, true)))
            }
        }
    })
    .await;

    let Ok(Ok(Some((asset, can_view)))) = access else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if !can_view {
        return StatusCode::FORBIDDEN.into_response();
    }

    let path = banner::banner_asset_path(&asset);
    if !path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let req = req.map(|_| axum::body::Body::empty());
    ServeFile::new(path).oneshot(req).await.map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(if has_version {
                    VERSIONED_CACHE_CONTROL
                } else {
                    UNVERSIONED_CACHE_CONTROL
                }),
            );
            resp.into_response()
        },
    )
}

pub async fn external_banner_warning_page(
    State(state): State<AppState>,
    Path(banner_id): Path<i64>,
    Query(query): Query<ExternalBannerQuery>,
    jar: CookieJar,
) -> Result<Response> {
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
    let return_to = banner::safe_return_to(query.return_to.as_deref().unwrap_or("/"));
    let asset = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<crate::models::BannerAsset> {
            let conn = pool.get()?;
            let settings = banner::load_site_banner_settings(&conn);
            if !settings.allow_external_links {
                return Err(AppError::Forbidden(
                    "External banner links are disabled.".into(),
                ));
            }
            let asset = db::get_banner_asset(&conn, banner_id)?
                .ok_or_else(|| AppError::NotFound("Banner not found.".into()))?;
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
        boards.as_slice(),
        current_theme.as_deref(),
        None,
        false,
        "/banner/external",
    );
    Ok((jar, Html(html)).into_response())
}

pub async fn external_banner_continue(
    State(state): State<AppState>,
    Path(banner_id): Path<i64>,
    Query(_query): Query<ExternalBannerQuery>,
) -> Result<Response> {
    let target = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let settings = banner::load_site_banner_settings(&conn);
            if !settings.allow_external_links {
                return Err(AppError::Forbidden(
                    "External banner links are disabled.".into(),
                ));
            }
            let asset = db::get_banner_asset(&conn, banner_id)?
                .ok_or_else(|| AppError::NotFound("Banner not found.".into()))?;
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
