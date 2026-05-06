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

const VERSIONED_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_IMMUTABLE_MEDIA;
const UNVERSIONED_CACHE_CONTROL: &str = crate::cache::CACHE_CONTROL_STATIC_SHORT;

#[derive(Deserialize, Default)]
pub struct ExternalBannerQuery {
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
    ServeFile::new(path).oneshot(req).await.map_or_else(
        |_| StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        |resp| {
            let mut resp = resp.map(axum::body::Body::new);
            resp.headers_mut()
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
        boards.as_slice(),
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

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
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

    fn insert_missing_global_banner(state: &crate::middleware::AppState) -> i64 {
        let conn = state.db.get().expect("db connection");
        crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Global,
            None,
            "0123456789abcdef0123456789abcdef",
            468,
            60,
            1024,
            true,
            1,
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert banner asset")
    }

    fn insert_existing_global_banner(
        state: &crate::middleware::AppState,
    ) -> (i64, std::path::PathBuf) {
        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let path = crate::banner::banner_storage_path(
            crate::models::BannerScope::Global,
            None,
            &storage_key,
        )
        .expect("banner path");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create banner dir");
        }
        std::fs::write(&path, b"banner bytes").expect("write banner file");
        let conn = state.db.get().expect("db connection");
        let banner_id = crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Global,
            None,
            &storage_key,
            468,
            60,
            12,
            true,
            1,
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert banner asset");
        (banner_id, path)
    }

    fn insert_existing_protected_board_banner(
        state: &crate::middleware::AppState,
    ) -> (i64, std::path::PathBuf, String) {
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "securebanner", "Secret", "", false).expect("board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .expect("protect board");
        let admin_hash = crate::utils::crypto::hash_password("hunter2").expect("hash admin");
        let admin_id = crate::db::create_admin(&conn, "admin", &admin_hash).expect("admin");
        crate::db::create_session(&conn, "banner-session", admin_id, i64::MAX).expect("session");

        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let path = crate::banner::banner_storage_path(
            crate::models::BannerScope::Board,
            Some("securebanner"),
            &storage_key,
        )
        .expect("banner path");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create banner dir");
        }
        std::fs::write(&path, b"banner bytes").expect("write banner file");
        let banner_id = crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Board,
            Some(board_id),
            &storage_key,
            468,
            60,
            12,
            true,
            1,
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert banner asset");
        (
            banner_id,
            path,
            format!(
                "{}=banner-session",
                crate::handlers::board::ADMIN_SESSION_COOKIE
            ),
        )
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
    async fn serve_banner_asset_returns_not_found_for_missing_file() {
        let state = crate::test_support::app_state();
        let banner_id = insert_missing_global_banner(&state);
        let router = Router::new()
            .route("/banner/assets/{id}", get(super::serve_banner_asset))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/banner/assets/{banner_id}?v=1"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().get(header::CACHE_CONTROL).is_none());
    }

    #[tokio::test]
    async fn serve_banner_asset_sets_cache_control_by_version_query() {
        let state = crate::test_support::app_state();
        let (banner_id, path) = insert_existing_global_banner(&state);
        let router = Router::new()
            .route("/banner/assets/{id}", get(super::serve_banner_asset))
            .with_state(state);

        let unversioned = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/banner/assets/{banner_id}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unversioned.status(), StatusCode::OK);
        assert_eq!(
            unversioned
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(super::UNVERSIONED_CACHE_CONTROL)
        );

        let versioned = router
            .oneshot(
                Request::builder()
                    .uri(format!("/banner/assets/{banner_id}?v=123"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(versioned.status(), StatusCode::OK);
        assert_eq!(
            versioned
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(super::VERSIONED_CACHE_CONTROL)
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn protected_board_banner_asset_is_not_public_cacheable() {
        let state = crate::test_support::app_state();
        let (banner_id, path, cookie) = insert_existing_protected_board_banner(&state);
        let router = Router::new()
            .route("/banner/assets/{id}", get(super::serve_banner_asset))
            .with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/banner/assets/{banner_id}?v=123"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_CACHE)
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn serve_banner_asset_rejects_malformed_id_path() {
        let router = Router::new()
            .route("/banner/assets/{id}", get(super::serve_banner_asset))
            .with_state(crate::test_support::app_state());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/banner/assets/not-a-number")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
