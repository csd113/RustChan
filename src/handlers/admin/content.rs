// handlers/admin/content.rs
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
    response::{IntoResponse, Redirect, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::info;

// ─── POST /admin/board/create ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateBoardForm {
    short_name: String,
    name: String,
    description: String,
    nsfw: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn create_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateBoardForm>,
) -> Result<Response> {
    // FIX[HIGH-3]: auth + DB write in spawn_blocking
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    let csrf_cookie = jar.get("csrf_token").map(|c| c.value().to_string());
    if !crate::middleware::validate_csrf(csrf_cookie.as_deref(), form.csrf.as_deref().unwrap_or(""))
    {
        return Err(AppError::Forbidden("CSRF token mismatch.".into()));
    }

    let short = form
        .short_name
        .trim()
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>();

    if short.is_empty() {
        return Err(AppError::BadRequest("Invalid board name.".into()));
    }

    let nsfw = form.nsfw.as_deref() == Some("1");
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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::create_board(&conn, &short, &name, &description, nsfw)?;
            info!("Admin created board /{short}/");
            // Refresh live board list so the top bar on any subsequent error
            // page includes the newly created board.
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/board/delete ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardIdForm {
    board_id: i64,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn delete_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BoardIdForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

            // Fetch the board's short_name before deletion so we can remove
            // its upload directory entirely after cleaning tracked files.
            let short_name: Option<String> = conn
                .query_row(
                    "SELECT short_name FROM boards WHERE id = ?1",
                    rusqlite::params![form.board_id],
                    |r| r.get(0),
                )
                .ok();

            // delete_board returns all file paths for posts in this board.
            let paths = db::delete_board(&conn, form.board_id)?;

            // Delete every tracked file and thumbnail from disk.
            for p in &paths {
                crate::utils::files::delete_file(&upload_dir, p);
            }

            // Remove the entire board upload directory — handles the thumbs/
            // sub-directory and any orphaned/untracked files too.
            if let Some(short) = short_name {
                let board_dir = PathBuf::from(&upload_dir).join(&short);
                if board_dir.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&board_dir) {
                        tracing::warn!("Could not remove board dir {:?}: {}", board_dir, e);
                    }
                }
            }

            info!(
                "Admin deleted board id={} ({} file(s) removed)",
                form.board_id,
                paths.len()
            );
            // Refresh live board list so the top bar immediately stops showing
            // the deleted board — important because error pages use this cache.
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to("/admin/panel").into_response())
}

// ─── POST /admin/thread/action ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ThreadActionForm {
    thread_id: i64,
    board: String,
    action: String, // "sticky", "unsticky", "lock", "unlock"
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn thread_action(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ThreadActionForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    // Validate action before spawning to give early error
    match form.action.as_str() {
        "sticky" | "unsticky" | "lock" | "unlock" | "archive" => {}
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    }

    let action = form.action.clone();
    let thread_id = form.thread_id;
    let board_for_log = form.board.clone();
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;
            match action.as_str() {
                "sticky" => db::set_thread_sticky(&conn, thread_id, true)?,
                "unsticky" => db::set_thread_sticky(&conn, thread_id, false)?,
                "lock" => db::set_thread_locked(&conn, thread_id, true)?,
                "unlock" => db::set_thread_locked(&conn, thread_id, false)?,
                "archive" => db::set_thread_archived(&conn, thread_id, true)?,
                _ => {}
            }
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                &action,
                "thread",
                Some(thread_id),
                &board_for_log,
                "",
            );
            info!("Admin {action} thread {thread_id}");
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // FIX[MEDIUM-10]: Use the board name from the DB (via the thread's board_id),
    // not the user-supplied form.board, to prevent path-confusion redirects.
    let redirect_url = {
        let pool = state.db.clone();
        let board_name = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = pool.get()?;
            let thread = db::get_thread(&conn, thread_id)?;
            if let Some(t) = thread {
                let boards = db::get_all_boards(&conn)?;
                if let Some(b) = boards.iter().find(|b| b.id == t.board_id) {
                    return Ok(b.short_name.clone());
                }
            }
            // Fallback: sanitize the user-supplied board name to prevent open-redirect.
            // Only allow alphanumeric characters (matching the board short_name format).
            let safe: String = form
                .board
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(8)
                .collect();
            Ok(safe)
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;
        // After archiving, send to the board archive; for all other actions
        // stay on the thread.
        if form.action == "archive" {
            format!("/{board_name}/archive")
        } else {
            format!("/{board_name}/thread/{}", form.thread_id)
        }
    };

    Ok(Redirect::to(&redirect_url).into_response())
}

// ─── POST /admin/post/delete ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AdminDeletePostForm {
    post_id: i64,
    board: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn admin_delete_post(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AdminDeletePostForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let post_id = form.post_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;

            let post = db::get_post(&conn, post_id)?
                .ok_or_else(|| AppError::NotFound("Post not found.".into()))?;

            // FIX[MEDIUM-10]: Resolve board name from DB, not user-supplied form field.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_all_boards(&conn)?
                .into_iter()
                .find(|b| b.id == post.board_id)
                .map_or_else(
                    || {
                        form.board
                            .chars()
                            .filter(char::is_ascii_alphanumeric)
                            .take(8)
                            .collect()
                    },
                    |b| b.short_name,
                );

            let thread_id = post.thread_id;
            let is_op = post.is_op;

            let paths = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, post_id)?
            };

            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            let action = if is_op {
                "delete_thread"
            } else {
                "delete_post"
            };
            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                action,
                "post",
                Some(post_id),
                &board_name,
                &post.body.chars().take(80).collect::<String>(),
            );
            info!("Admin deleted post {post_id}");
            // Return board_name + thread context so we can redirect back to the thread.
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

// ─── POST /admin/thread/delete ────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AdminDeleteThreadForm {
    thread_id: i64,
    board: String,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn admin_delete_thread(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<AdminDeleteThreadForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let upload_dir = CONFIG.upload_dir.clone();
    let thread_id = form.thread_id;

    let redirect_board = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            let (admin_id, admin_name) =
                super::require_admin_session_with_name(&conn, session_id.as_deref())?;

            // FIX[MEDIUM-10]: Resolve board name from DB.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let board_name = db::get_thread(&conn, thread_id)?
                .and_then(|t| {
                    db::get_all_boards(&conn)
                        .ok()?
                        .into_iter()
                        .find(|b| b.id == t.board_id)
                        .map(|b| b.short_name)
                })
                .unwrap_or_else(|| {
                    form.board
                        .chars()
                        .filter(char::is_ascii_alphanumeric)
                        .take(8)
                        .collect()
                });

            let paths = db::delete_thread(&conn, thread_id)?;
            for p in paths {
                crate::utils::files::delete_file(&upload_dir, &p);
            }

            let _ = db::log_mod_action(
                &conn,
                admin_id,
                &admin_name,
                "delete_thread",
                "thread",
                Some(thread_id),
                &board_name,
                "",
            );
            info!("Admin deleted thread {thread_id}");
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{redirect_board}")).into_response())
}
