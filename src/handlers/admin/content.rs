//
// Board, thread, and post management handlers.
// All routes require a valid admin session cookie.

use crate::{
    config::CONFIG,
    db,
    error::{AppError, Result},
    middleware::AppState,
};
use axum::{
    extract::{Form, State},
    response::{IntoResponse as _, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use rusqlite::OptionalExtension as _;
use serde::Deserialize;

/// Validates new board short name.
fn validate_new_board_short_name(raw_short_name: &str) -> Result<String> {
    let trimmed = raw_short_name.trim();
    if trimmed.is_empty() || trimmed.len() > 8 {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }
    if !trimmed.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }
    Ok(trimmed.to_lowercase())
}

// POST /admin/board/create
#[derive(Deserialize)]
pub(in crate::server) struct CreateBoardForm {
    short_name: String,
    name: String,
    description: String,
    nsfw: Option<String>,
    allow_audio: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub(in crate::server) async fn create_board(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<CreateBoardForm>,
) -> Result<Response> {
    // auth + DB write in spawn_blocking
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let short = validate_new_board_short_name(&form.short_name)?;
    let short_for_flash = short.clone();

    let nsfw = form.nsfw.as_deref() == Some("1");
    let allow_audio = form.allow_audio.as_deref() == Some("1");
    let name = form.name.trim().chars().take(64).collect::<String>();
    let description = form
        .description
        .trim()
        .chars()
        .take(256)
        .collect::<String>();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())
                .map(|_completed_value| ())?;
            if db::get_board_by_short(&conn, &short)?.is_some() {
                return Err(AppError::Conflict(format!(
                    "Board /{short}/ already exists."
                )));
            }
            db::create_board_with_media_flags(
                &conn,
                &short,
                &name,
                &description,
                nsfw,
                true,
                true,
                allow_audio,
            )
            .map(|_completed_value| ())?;
            tracing::info!(target: "admin", board = %short, "Created board");
            // Refresh live board list so the top bar on any subsequent error
            // page includes the newly created board.
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect(&format!("Board /{short_for_flash}/ created.")).into_response())
}

// POST /admin/board/delete
#[derive(Deserialize)]
pub(in crate::server) struct BoardIdForm {
    board_id: i64,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[derive(Deserialize)]
pub(in crate::server) struct ReorderBoardForm {
    board_id: i64,
    direction: String,
    return_to: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

fn safe_return_to(path: Option<&str>) -> &str {
    crate::utils::redirect::safe_internal_path_or(path, "/admin/panel")
}

pub(in crate::server) async fn delete_board(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<BoardIdForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;

            // Fetch the board's short_name before deletion so we can remove
            // its upload directory entirely after cleaning tracked files.
            // A lookup error must abort the destructive delete rather than
            // skip the health preflight below.
            let short_name: Option<String> = conn
                .query_row(
                    "SELECT short_name FROM boards WHERE id = ?1",
                    rusqlite::params![form.board_id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(short) = &short_name {
                let health = db::check_db_health(&conn);
                if !health.before.ok() {
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "Board delete aborted for /{short}/: live DB health check failed. \
                         integrity_check: {}; foreign_key_check: {}. \
                         Run database health check/repair from the admin panel first, or restore a known-good full backup.",
                        health.before.integrity.output(),
                        health.before.foreign_keys.output()
                    )));
                }
            }
            // delete_board records durable cleanup for tracked files and the
            // board media directory in the same transaction as the DB delete.
            let deleted = db::delete_board(&conn, form.board_id).map_err(|error| {
                let chain = error
                    .chain()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" | ");
                if chain.contains("database disk image is malformed") {
                    let board_label = short_name.as_deref().unwrap_or("unknown");
                    AppError::Internal(anyhow::anyhow!(
                        "Board delete failed for /{board_label}/: {chain}. \
                         The live database appears corrupted. Run database integrity check/repair from the admin panel, or restore a known-good full backup."
                    ))
                } else {
                    AppError::Internal(anyhow::anyhow!(error))
                }
            })?;

            let board_cleanup_dirs = short_name.into_iter().collect::<Vec<_>>();
            if let Err(error) = crate::pending_fs::finalize_delete_files_and_dirs_payload(
                &conn,
                &upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
                &board_cleanup_dirs,
            ) {
                tracing::warn!(
                    target: "admin",
                    board_id = form.board_id,
                    error = %error,
                    "deleted board but filesystem cleanup remains pending"
                );
            }

            tracing::info!(target: "admin", board_id = form.board_id, files_removed = deleted.paths.len(), "Board deleted");
            // Refresh live board list so the top bar immediately stops showing
            // the deleted board — important because error pages use this cache.
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(super::admin_panel_redirect("Board deleted.").into_response())
}

// POST /admin/board/reorder
pub(in crate::server) async fn reorder_board(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ReorderBoardForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let move_up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => {
            return Err(AppError::BadRequest(
                "Unknown board reorder direction.".into(),
            ))
        }
    };
    let return_to = safe_return_to(form.return_to.as_deref()).to_owned();
    let board_id = form.board_id;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())
                .map(|_completed_value| ())?;
            db::move_board(&mut conn, board_id, move_up)?;
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&return_to).into_response())
}

// POST /admin/thread/action
#[derive(Deserialize)]
pub(in crate::server) struct ThreadActionForm {
    thread_id: i64,
    board: String,
    action: String, // "sticky", "unsticky", "lock", "unlock"
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub(in crate::server) async fn thread_action(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<ThreadActionForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    match form.action.as_str() {
        "sticky" | "unsticky" | "lock" | "unlock" | "archive" => {}
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    }

    let action = form.action.clone();
    let thread_id = form.thread_id;
    let submitted_board = form.board.clone();
    let board_name = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let tx = rusqlite::Transaction::new_unchecked(
                &conn, rusqlite::TransactionBehavior::Immediate,
            )?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&tx, session_id.as_deref())?;
            let target = db::get_thread(&tx, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            let logged_board: String = tx.query_row(
                "SELECT short_name FROM boards WHERE id = ?1", [target.board_id], |row| row.get(0),
            )?;
            if submitted_board != logged_board {
                return Err(AppError::BadRequest("Board mismatch.".into()));
            }
            if target.archived && matches!(action.as_str(), "lock" | "unlock" | "sticky") {
                return Err(AppError::Conflict("This thread is archived.".into()));
            }
            match action.as_str() {
                "sticky" => db::set_thread_sticky(&tx, thread_id, true)?,
                "unsticky" => db::set_thread_sticky(&tx, thread_id, false)?,
                "lock" => db::set_thread_locked(&tx, thread_id, true)?,
                "unlock" => db::set_thread_locked(&tx, thread_id, false)?,
                "archive" => db::set_thread_archived(&tx, thread_id, true)?,
                _ => {}
            }
            let board_id = target.board_id;
            if matches!(action.as_str(), "archive" | "unsticky") {
                let _schedule = db::persist_thread_prune_intent_in_tx(&tx, board_id)?;
            }
            // Thread moderation and its audit record must commit together.
            db::log_mod_action(
                &tx, admin_id, &admin_name, &action, "thread", Some(thread_id),
                &logged_board, "",
            )?;
            tx.commit()?;
            tracing::info!(target: "admin", action = %action, thread_id = thread_id, "Thread action");
            Ok(logged_board)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let redirect_url = if form.action == "archive" {
        format!("/{board_name}/archive")
    } else {
        format!("/{board_name}/thread/{thread_id}")
    };
    Ok(Redirect::to(&redirect_url).into_response())
}

// POST /admin/post/delete
#[derive(Deserialize)]
pub(in crate::server) struct AdminDeletePostForm {
    post_id: i64,
    board: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub(in crate::server) async fn admin_delete_post(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AdminDeletePostForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let post_id = form.post_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
            let (admin_id, admin_name) = super::require_admin_session_with_name(&tx, session_id.as_deref())?;
            let post = db::get_post(&tx, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;
            let board_name: String = tx.query_row("SELECT short_name FROM boards WHERE id = ?1", [post.board_id], |row| row.get(0))?;
            if board_name != form.board { return Err(AppError::BadRequest("Board mismatch.".into())); }
            let thread_id = post.thread_id;
            let is_op = post.is_op;
            let deleted = if is_op { db::delete_thread_verified(&tx, thread_id)? }
                else { db::posts::delete_post_reply_in_tx(&tx, post_id)? };
            db::log_mod_action(&tx, admin_id, &admin_name,
                if is_op { "delete_thread" } else { "delete_post" }, "post", Some(post_id),
                &board_name, &post.body.chars().take(80).collect::<String>())?;
            tx.commit()?;
            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn, &upload_dir, deleted.pending_fs_op_id.as_deref(), &deleted.paths,
            ) {
                tracing::warn!(target: "admin", post_id, %error, "deletion committed; file cleanup remains pending");
            }
            tracing::info!(target: "admin", post_id = post_id, "Post deleted");
            // If the post was an OP, redirect to the board index (thread is gone).
            if is_op {
                Ok(format!("/{board_name}"))
            } else {
                Ok(format!("/{board_name}/thread/{thread_id}"))
            }
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&redirect_board).into_response())
}

// POST /admin/thread/delete
#[derive(Deserialize)]
pub(in crate::server) struct AdminDeleteThreadForm {
    thread_id: i64,
    board: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub(in crate::server) async fn admin_delete_thread(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<AdminDeleteThreadForm>,
) -> Result<Response> {
    let session_id = jar.get(super::SESSION_COOKIE).map(|c| c.value().to_owned());
    super::require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let thread_id = form.thread_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
            let (admin_id, admin_name) = super::require_admin_session_with_name(&tx, session_id.as_deref())?;
            let target = db::get_thread(&tx, thread_id)?
                .ok_or_else(|| AppError::NotFound("Thread not found.".into()))?;
            let board_name: String = tx.query_row("SELECT short_name FROM boards WHERE id = ?1", [target.board_id], |row| row.get(0))?;
            if board_name != form.board { return Err(AppError::BadRequest("Board mismatch.".into())); }
            let deleted = db::delete_thread_verified(&tx, thread_id)?;
            db::log_mod_action(&tx, admin_id, &admin_name, "delete_thread", "thread", Some(thread_id), &board_name, "")?;
            tx.commit()?;
            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn, &upload_dir, deleted.pending_fs_op_id.as_deref(), &deleted.paths,
            ) {
                tracing::warn!(target: "admin", thread_id, %error, "deletion committed; file cleanup remains pending");
            }
            tracing::info!(target: "admin", thread_id = thread_id, "Thread deleted");
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{redirect_board}")).into_response())
}
