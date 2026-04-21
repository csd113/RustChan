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

fn load_accessible_banner_asset(
    conn: &rusqlite::Connection,
    banner_id: i64,
    jar: &CookieJar,
    admin_session_id: Option<&str>,
) -> Result<crate::models::BannerAsset> {
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
    }
    Ok(asset)
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

    let asset = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let jar = jar.clone();
        move || -> Result<crate::models::BannerAsset> {
            let conn = pool.get()?;
            load_accessible_banner_asset(&conn, banner_id, &jar, admin_session_id.as_deref())
        }
    })
    .await;

    let asset = match asset {
        Ok(Ok(asset)) => asset,
        Ok(Err(AppError::NotFound(_))) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(AppError::Forbidden(_))) => return StatusCode::FORBIDDEN.into_response(),
        Ok(Err(_)) | Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let Ok(path) = banner::banner_asset_path(&asset) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = banner::banner_asset_content_type(&path);

    let req = req.map(|_| axum::body::Body::empty());
    ServeFile::new(path).oneshot(req).await.map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
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
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    let current_theme = current_theme_from_jar(&jar);
    let (jar, csrf) = ensure_csrf(jar);
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
            let asset =
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
    jar: CookieJar,
) -> Result<Response> {
    let admin_session_id = jar
        .get(crate::handlers::board::ADMIN_SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
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
            let asset =
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    fn insert_external_board_banner(state: &crate::middleware::AppState, board_short: &str) -> i64 {
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, board_short, "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .expect("update board access");
        crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Board,
            Some(board_id),
            "0123456789abcdef0123456789abcdef",
            468,
            60,
            1024,
            true,
            1,
            crate::models::BannerTargetType::ExternalUrl,
            "https://example.com",
            true,
            true,
        )
        .expect("insert banner asset")
    }

    #[tokio::test]
    async fn serve_banner_asset_returns_not_found_for_missing_banner_id() {
        let router = Router::new()
            .route("/banner/assets/{id}", get(super::serve_banner_asset))
            .with_state(crate::test_support::app_state());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/banner/assets/999")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn protected_board_banner_external_routes_require_board_access() {
        let state = crate::test_support::app_state();
        let banner_id = insert_external_board_banner(&state, "secret");

        let router = Router::new()
            .route(
                "/banner/external/{id}",
                get(super::external_banner_warning_page),
            )
            .route(
                "/banner/external/{id}/continue",
                get(super::external_banner_continue),
            )
            .with_state(state);

        for uri in [
            format!("/banner/external/{banner_id}"),
            format!("/banner/external/{banner_id}/continue"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
}
