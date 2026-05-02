// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use anyhow::Context as _;

struct ParsedBannerUpload {
    csrf: Option<String>,
    board_id: Option<i64>,
    target_type: String,
    target_value: Option<String>,
    target_board_value: Option<String>,
    target_thread_value: Option<String>,
    target_external_url: Option<String>,
    show_on_index: bool,
    show_on_catalog: bool,
    enabled: bool,
    banner_bytes: Vec<u8>,
}

async fn parse_banner_upload(mut multipart: Multipart) -> Result<ParsedBannerUpload> {
    let mut csrf = None;
    let mut board_id = None;
    let mut target_type = String::from("none");
    let mut target_value = None;
    let mut target_board_value = None;
    let mut target_thread_value = None;
    let mut target_external_url = None;
    let mut show_on_index = true;
    let mut show_on_catalog = true;
    let mut enabled = true;
    let mut banner_bytes = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.to_string()))?
    {
        match field.name() {
            Some("_csrf") => csrf = Some(read_text_field(field).await?),
            Some("board_id") => board_id = read_text_field(field).await?.trim().parse::<i64>().ok(),
            Some("target_type") => target_type = read_text_field(field).await?,
            Some("target_value") => target_value = Some(read_text_field(field).await?),
            Some("target_board_value") => target_board_value = Some(read_text_field(field).await?),
            Some("target_thread_value") => {
                target_thread_value = Some(read_text_field(field).await?)
            }
            Some("target_external_url") => {
                target_external_url = Some(read_text_field(field).await?)
            }
            Some("show_on_index") => show_on_index = read_checkbox_field(field).await?,
            Some("show_on_catalog") => show_on_catalog = read_checkbox_field(field).await?,
            Some("enabled") => enabled = read_checkbox_field(field).await?,
            Some("banner") => {
                let bytes = read_limited_upload_bytes(field, MAX_BANNER_UPLOAD_BYTES).await?;
                if !bytes.is_empty() {
                    banner_bytes = Some(bytes);
                }
            }
            _ => {}
        }
    }

    Ok(ParsedBannerUpload {
        csrf,
        board_id,
        target_type,
        target_value,
        target_board_value,
        target_thread_value,
        target_external_url,
        show_on_index,
        show_on_catalog,
        enabled,
        banner_bytes: banner_bytes
            .ok_or_else(|| AppError::BadRequest("No banner file uploaded.".into()))?,
    })
}

#[derive(Deserialize)]
pub struct BannerMetaForm {
    pub banner_id: i64,
    pub target_type: String,
    pub target_value: Option<String>,
    pub target_board_value: Option<String>,
    pub target_thread_value: Option<String>,
    pub target_external_url: Option<String>,
    pub enabled: Option<String>,
    pub show_on_index: Option<String>,
    pub show_on_catalog: Option<String>,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteBannerForm {
    pub banner_id: i64,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct MoveBannerForm {
    pub banner_id: i64,
    pub direction: String,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct ClearBoardBannerForm {
    pub board_id: i64,
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

async fn board_appearance_anchor_from_id(state: &AppState, board_id: i64) -> Result<String> {
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let board_short = conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![board_id],
                |row| row.get::<_, String>(0),
            )?;
            Ok(banner::board_appearance_anchor(&board_short))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}

fn resolve_banner_target_selection(
    target_type_raw: &str,
    target_value_raw: Option<&str>,
    target_board_value_raw: Option<&str>,
    target_thread_value_raw: Option<&str>,
    target_external_url_raw: Option<&str>,
    allow_external_links: bool,
) -> Result<(BannerTargetType, String)> {
    let selected_target_value = banner::select_banner_target_value(
        target_type_raw,
        target_value_raw,
        target_board_value_raw,
        target_thread_value_raw,
        target_external_url_raw,
    );
    banner::parse_banner_target(
        target_type_raw,
        &selected_target_value,
        allow_external_links,
    )
}

fn restore_board_banner_inheritance_if_empty(
    conn: &rusqlite::Connection,
    board_id: Option<i64>,
) -> Result<()> {
    let Some(board_id) = board_id else {
        return Ok(());
    };
    if db::list_banner_assets_for_board(conn, board_id)?.is_empty() {
        conn.execute(
            "UPDATE boards SET banner_mode = 'inherit' WHERE id = ?1 AND banner_mode = 'override'",
            rusqlite::params![board_id],
        )?;
    }
    Ok(())
}

fn banner_cleanup_payload(
    assets: &[crate::models::BannerAsset],
) -> Result<Option<crate::pending_fs::PendingFsOpInsert>> {
    if assets.is_empty() {
        return Ok(None);
    }
    let payload = crate::pending_fs::DeleteBannerAssetsPayload {
        assets: assets
            .iter()
            .map(|asset| crate::pending_fs::BannerAssetCleanupPayload {
                scope: asset.scope,
                board_short: asset.board_short.clone(),
                storage_key: asset.storage_key.clone(),
            })
            .collect(),
    };
    Ok(Some(crate::pending_fs::PendingFsOpInsert {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind: crate::pending_fs::DELETE_BANNER_ASSETS_KIND,
        payload_json: serde_json::to_string(&payload)
            .context("Serialize delete_banner_assets payload failed")?,
    }))
}

fn delete_banner_asset_safely(
    conn: &rusqlite::Connection,
    banner_id: i64,
) -> Result<crate::models::BannerAsset> {
    let tx = conn.unchecked_transaction()?;
    let asset = db::get_banner_asset(&tx, banner_id)?
        .ok_or_else(|| AppError::BadRequest("Banner not found.".into()))?;
    db::delete_banner_asset(&tx, banner_id)?;
    if asset.scope == BannerScope::Board {
        restore_board_banner_inheritance_if_empty(&tx, asset.board_id)?;
    }
    let pending_op = banner_cleanup_payload(std::slice::from_ref(&asset))?;
    if let Some(op) = pending_op.as_ref() {
        db::insert_pending_fs_op(&tx, op)?;
    }
    tx.commit()?;
    if let Some(op) = pending_op.as_ref() {
        let payload: crate::pending_fs::DeleteBannerAssetsPayload =
            serde_json::from_str(&op.payload_json).map_err(anyhow::Error::from)?;
        crate::pending_fs::finalize_delete_banner_assets_payload(conn, Some(&op.id), &payload)?;
    }
    Ok(asset)
}

fn clear_board_banner_assets_safely(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<(String, Vec<crate::models::BannerAsset>)> {
    let board_short: String = conn.query_row(
        "SELECT short_name FROM boards WHERE id = ?1",
        rusqlite::params![board_id],
        |row| row.get(0),
    )?;
    let tx = conn.unchecked_transaction()?;
    let assets = db::list_banner_assets_for_board(&tx, board_id)?;
    let pending_op = banner_cleanup_payload(&assets)?;
    db::delete_board_banner_assets(&tx, board_id)?;
    tx.execute(
        "UPDATE boards SET banner_mode = 'inherit' WHERE id = ?1 AND banner_mode = 'override'",
        rusqlite::params![board_id],
    )?;
    if let Some(op) = pending_op.as_ref() {
        db::insert_pending_fs_op(&tx, op)?;
    }
    tx.commit()?;
    if let Some(op) = pending_op.as_ref() {
        let payload: crate::pending_fs::DeleteBannerAssetsPayload =
            serde_json::from_str(&op.payload_json).map_err(anyhow::Error::from)?;
        crate::pending_fs::finalize_delete_banner_assets_payload(conn, Some(&op.id), &payload)?;
    }
    Ok((board_short, assets))
}

async fn upload_banner_for_scope(
    state: AppState,
    session_id: Option<String>,
    scope: BannerScope,
    board_id: Option<i64>,
    parsed: ParsedBannerUpload,
) -> Result<String> {
    tokio::task::spawn_blocking(move || -> Result<String> {
        let mut conn = state.db.get()?;
        super::require_admin_session_sid(&conn, session_id.as_deref())?;
        let (target_type, target_value) = resolve_banner_target_selection(
            &parsed.target_type,
            parsed.target_value.as_deref(),
            parsed.target_board_value.as_deref(),
            parsed.target_thread_value.as_deref(),
            parsed.target_external_url.as_deref(),
            db::get_banner_external_links_enabled(&conn),
        )?;

        let board_short = if scope == BannerScope::Board {
            let id = board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
            Some(conn.query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get::<_, String>(0),
            )?)
        } else {
            None
        };

        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let draft_asset = crate::models::BannerAsset {
            id: 0,
            scope,
            board_id,
            board_short: board_short.clone(),
            storage_key: storage_key.clone(),
            width: 0,
            height: 0,
            file_size: 0,
            enabled: parsed.enabled,
            sort_order: 1,
            target_type,
            target_value: target_value.clone(),
            show_on_index: parsed.show_on_index,
            show_on_catalog: parsed.show_on_catalog,
            created_at: chrono::Utc::now().timestamp(),
        };
        let (width, height, file_size) =
            banner::write_banner_asset(&draft_asset, &parsed.banner_bytes)?;

        let result = (|| -> Result<String> {
            let tx = conn.transaction()?;
            let sort_order = db::next_banner_sort_order(&tx, scope, board_id)?;
            let banner_id = db::insert_banner_asset(
                &tx,
                scope,
                board_id,
                &storage_key,
                i64::from(width),
                i64::from(height),
                i64::try_from(file_size)
                    .map_err(|_| AppError::BadRequest("Banner file size is too large.".into()))?,
                parsed.enabled,
                sort_order,
                target_type,
                &target_value,
                if scope == BannerScope::Home {
                    false
                } else {
                    parsed.show_on_index
                },
                if scope == BannerScope::Home {
                    false
                } else {
                    parsed.show_on_catalog
                },
            )?;
            if scope == BannerScope::Board {
                let board_id =
                    board_id.ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
                let affected = tx.execute(
                    "UPDATE boards SET banner_mode = 'override' WHERE id = ?1",
                    rusqlite::params![board_id],
                )?;
                if affected == 0 {
                    return Err(AppError::BadRequest(format!(
                        "Board id {board_id} not found"
                    )));
                }
            }
            tx.commit()?;
            let anchor = banner::banner_admin_anchor(scope, board_short.as_deref());
            tracing::info!(
                target: "admin",
                banner_id,
                scope = %scope,
                "Banner uploaded"
            );
            Ok(anchor)
        })();

        if result.is_err() {
            let _ = banner::delete_banner_asset_file(&draft_asset);
        }
        result
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
}

pub async fn upload_global_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers, Some(peer))?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_admin_csrf_jar(&jar, parsed.csrf.as_deref())?;
    match upload_banner_for_scope(state, session_id, BannerScope::Global, None, parsed).await {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Global banner uploaded.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "global-banners",
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            "global-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn upload_home_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers, Some(peer))?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_admin_csrf_jar(&jar, parsed.csrf.as_deref())?;
    match upload_banner_for_scope(state, session_id, BannerScope::Home, None, parsed).await {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Home page banner uploaded.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "home-banners",
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            "home-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn upload_board_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    multipart: Multipart,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_same_origin_request(&headers, Some(peer))?;
    let parsed = parse_banner_upload(multipart).await?;
    super::check_admin_csrf_jar(&jar, parsed.csrf.as_deref())?;
    let board_id = parsed
        .board_id
        .ok_or_else(|| AppError::BadRequest("Missing board id.".into()))?;
    let board_anchor = board_appearance_anchor_from_id(&state, board_id).await?;
    match upload_banner_for_scope(
        state,
        session_id,
        BannerScope::Board,
        Some(board_id),
        parsed,
    )
    .await
    {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Board banner saved.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            &board_anchor,
            "board-banners",
        )
        .into_response()),
        Err(AppError::Internal(error)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &format_banner_upload_error(&error),
            &board_anchor,
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn update_banner_meta(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BannerMetaForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let result = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = db::get_banner_asset(&conn, form.banner_id)?
                .ok_or_else(|| AppError::BadRequest("Banner not found.".into()))?;
            let (target_type, target_value) = resolve_banner_target_selection(
                &form.target_type,
                form.target_value.as_deref(),
                form.target_board_value.as_deref(),
                form.target_thread_value.as_deref(),
                form.target_external_url.as_deref(),
                db::get_banner_external_links_enabled(&conn),
            )?;
            db::update_banner_asset_meta(
                &conn,
                form.banner_id,
                checkbox_is_on(form.enabled.as_deref()),
                target_type,
                &target_value,
                if asset.scope == BannerScope::Home {
                    false
                } else {
                    checkbox_is_on(form.show_on_index.as_deref())
                },
                if asset.scope == BannerScope::Home {
                    false
                } else {
                    checkbox_is_on(form.show_on_catalog.as_deref())
                },
            )?;
            Ok(banner::banner_admin_anchor(
                asset.scope,
                asset.board_short.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    match result {
        Ok(anchor) => Ok(super::admin_panel_redirect_anchor_open(
            "Banner settings saved.",
            &anchor,
            banner::banner_open_section(&anchor),
        )
        .into_response()),
        Err(AppError::BadRequest(message)) => Ok(super::admin_panel_error_redirect_anchor_open(
            &message,
            "board-banners",
            "board-banners",
        )
        .into_response()),
        Err(error) => Err(error),
    }
}

pub async fn delete_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<DeleteBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let anchor = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = delete_banner_asset_safely(&conn, form.banner_id)?;
            Ok(banner::banner_admin_anchor(
                asset.scope,
                asset.board_short.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor_open(
        "Banner deleted.",
        &anchor,
        banner::banner_open_section(&anchor),
    )
    .into_response())
}

pub async fn move_banner(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<MoveBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let move_up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => {
            return Err(AppError::BadRequest(
                "Invalid banner move direction.".into(),
            ))
        }
    };
    let anchor = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let asset = db::get_banner_asset(&conn, form.banner_id)?
                .ok_or_else(|| AppError::BadRequest("Banner not found.".into()))?;
            db::move_banner_asset(&mut conn, form.banner_id, move_up)?;
            Ok(banner::banner_admin_anchor(
                asset.scope,
                asset.board_short.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor_open(
        "Banner order updated.",
        &anchor,
        banner::banner_open_section(&anchor),
    )
    .into_response())
}

pub async fn clear_board_banner_override(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ClearBoardBannerForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;
    let board_short = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            let (board_short, _assets) = clear_board_banner_assets_safely(&conn, form.board_id)?;
            Ok(board_short)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
    Ok(super::admin_panel_redirect_anchor_open(
        &format!("Board /{board_short}/ banner override cleared."),
        &banner::board_appearance_anchor(&board_short),
        "board-banners",
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        clear_board_banner_assets_safely, delete_banner_asset_safely,
        restore_board_banner_inheritance_if_empty,
    };

    fn board_banner_mode(conn: &rusqlite::Connection, board_id: i64) -> String {
        conn.query_row(
            "SELECT banner_mode FROM boards WHERE id = ?1",
            rusqlite::params![board_id],
            |row| row.get(0),
        )
        .expect("board banner mode")
    }

    #[test]
    fn restores_inherit_when_board_banner_set_is_empty() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET banner_mode = 'override' WHERE id = ?1",
            rusqlite::params![board_id],
        )
        .expect("set override mode");

        restore_board_banner_inheritance_if_empty(&conn, Some(board_id))
            .expect("restore inheritance");

        assert_eq!(board_banner_mode(&conn, board_id), "inherit");
    }

    #[test]
    fn keeps_override_when_board_banner_set_still_has_assets() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "b", "Random", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET banner_mode = 'override' WHERE id = ?1",
            rusqlite::params![board_id],
        )
        .expect("set override mode");
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
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert board banner");

        restore_board_banner_inheritance_if_empty(&conn, Some(board_id)).expect("keep override");

        assert_eq!(board_banner_mode(&conn, board_id), "override");
    }

    fn banner_asset_path(
        scope: crate::models::BannerScope,
        board_short: Option<&str>,
        storage_key: &str,
    ) -> std::path::PathBuf {
        crate::banner::banner_storage_path(scope, board_short, storage_key)
            .expect("banner storage path")
    }

    #[test]
    fn failed_global_banner_file_delete_keeps_pending_cleanup_for_retry() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let path = banner_asset_path(crate::models::BannerScope::Global, None, &storage_key);
        std::fs::create_dir_all(path.parent().expect("banner parent")).expect("create parent");
        std::fs::write(&path, b"webp").expect("write webp");
        let gif_path = path.with_extension("gif");
        std::fs::create_dir_all(&gif_path).expect("create undeletable gif dir");

        let banner_id = crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Global,
            None,
            &storage_key,
            468,
            60,
            4,
            true,
            1,
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert banner");

        let error =
            delete_banner_asset_safely(&conn, banner_id).expect_err("gif dir delete should fail");
        assert!(error.to_string().contains("remove"));
        assert!(crate::db::get_banner_asset(&conn, banner_id)
            .expect("reload banner")
            .is_none());
        let pending = crate::db::list_pending_fs_ops(&conn).expect("list pending ops");
        assert_eq!(pending.len(), 1);
        let pending_op = pending.first().expect("pending op");

        std::fs::remove_dir_all(&gif_path).expect("remove gif dir");
        std::fs::write(&gif_path, b"gif").expect("write gif");
        let payload: crate::pending_fs::DeleteBannerAssetsPayload =
            serde_json::from_str(&pending_op.payload_json).expect("pending payload");
        crate::pending_fs::finalize_delete_banner_assets_payload(
            &conn,
            Some(&pending_op.id),
            &payload,
        )
        .expect("retry delete banner files");
        assert!(!path.exists());
        assert!(!gif_path.exists());
        assert!(crate::db::list_pending_fs_ops(&conn)
            .expect("list pending ops")
            .is_empty());
    }

    #[test]
    fn failed_board_banner_clear_keeps_pending_cleanup_for_retry() {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "bb", "Board", "", false).expect("create board");
        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let path = banner_asset_path(crate::models::BannerScope::Board, Some("bb"), &storage_key);
        std::fs::create_dir_all(path.parent().expect("banner parent")).expect("create parent");
        std::fs::write(&path, b"webp").expect("write webp");
        let gif_path = path.with_extension("gif");
        std::fs::create_dir_all(&gif_path).expect("create undeletable gif dir");

        let banner_id = crate::db::insert_banner_asset(
            &conn,
            crate::models::BannerScope::Board,
            Some(board_id),
            &storage_key,
            468,
            60,
            4,
            true,
            1,
            crate::models::BannerTargetType::None,
            "",
            true,
            true,
        )
        .expect("insert board banner");

        clear_board_banner_assets_safely(&conn, board_id).expect_err("gif dir delete should fail");
        assert!(crate::db::get_banner_asset(&conn, banner_id)
            .expect("reload banner")
            .is_none());
        assert_eq!(board_banner_mode(&conn, board_id), "inherit");
        let pending = crate::db::list_pending_fs_ops(&conn).expect("list pending ops");
        assert_eq!(pending.len(), 1);
        let pending_op = pending.first().expect("pending op");

        std::fs::remove_dir_all(&gif_path).expect("remove gif dir");
        std::fs::write(&gif_path, b"gif").expect("write gif");
        let payload: crate::pending_fs::DeleteBannerAssetsPayload =
            serde_json::from_str(&pending_op.payload_json).expect("pending payload");
        crate::pending_fs::finalize_delete_banner_assets_payload(
            &conn,
            Some(&pending_op.id),
            &payload,
        )
        .expect("retry clear board banner files");
        assert!(!path.exists());
        assert!(!gif_path.exists());
        assert!(crate::db::list_pending_fs_ops(&conn)
            .expect("list pending ops")
            .is_empty());
    }
}
