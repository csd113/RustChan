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
use std::path::{Path, PathBuf};

fn sanitize_board_short_value(board_short: &str) -> String {
    board_short
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect()
}

fn resolve_board_short_name(
    boards: Option<&[crate::models::Board]>,
    board_id: i64,
    fallback_board: &str,
) -> String {
    boards
        .and_then(|boards| boards.iter().find(|board| board.id == board_id))
        .map_or_else(
            || sanitize_board_short_value(fallback_board),
            |board| board.short_name.clone(),
        )
}

fn validate_board_short_for_filesystem(short: &str) -> Result<()> {
    let is_valid = !short.is_empty()
        && short.len() <= 8
        && short
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !is_valid {
        return Err(AppError::Internal(anyhow::anyhow!(
            "Refusing to delete board upload directory for unsafe stored board short_name {short:?}"
        )));
    }
    Ok(())
}

fn checked_board_upload_dir(upload_dir: &str, short: &str) -> Result<PathBuf> {
    validate_board_short_for_filesystem(short)?;
    let upload_root = Path::new(upload_dir);
    let checked_root = if upload_root.exists() {
        upload_root.canonicalize().map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Canonicalize upload directory {} failed: {error}",
                upload_root.display()
            ))
        })?
    } else {
        upload_root.to_path_buf()
    };
    let board_dir = checked_root.join(short);
    if !board_dir.exists() {
        return Ok(board_dir);
    }
    let canonical_board_dir = board_dir.canonicalize().map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Canonicalize board upload directory {} failed: {error}",
            board_dir.display()
        ))
    })?;
    if !canonical_board_dir.starts_with(&checked_root) {
        return Err(AppError::Internal(anyhow::anyhow!(
            "Refusing to delete board upload directory {} because it escapes upload root {}",
            canonical_board_dir.display(),
            checked_root.display()
        )));
    }
    Ok(canonical_board_dir)
}

// ─── POST /admin/board/create ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateBoardForm {
    short_name: String,
    name: String,
    description: String,
    nsfw: Option<String>,
    allow_audio: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn create_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CreateBoardForm>,
) -> Result<Response> {
    // auth + DB write in spawn_blocking
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

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
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::create_board_with_media_flags(
                &conn,
                &short,
                &name,
                &description,
                nsfw,
                true,
                true,
                allow_audio,
            )?;
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

// ─── POST /admin/board/delete ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BoardIdForm {
    board_id: i64,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct ReorderBoardForm {
    board_id: i64,
    direction: String,
    return_to: Option<String>,
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

fn safe_return_to(path: Option<&str>) -> &str {
    crate::utils::redirect::safe_internal_path_or(path, "/admin/panel")
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
            if let Some(ref short) = short_name {
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
            let board_upload_dir = short_name
                .as_deref()
                .map(|short| checked_board_upload_dir(&upload_dir, short))
                .transpose()?;

            // delete_board returns all file paths for posts in this board.
            let paths = db::delete_board(&conn, form.board_id).map_err(|error| {
                let chain = error
                    .chain()
                    .map(std::string::ToString::to_string)
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

            // Delete every tracked file and thumbnail from disk.
            for p in &paths {
                crate::utils::files::delete_file(&upload_dir, p);
            }

            // Remove the entire board upload directory — handles the thumbs/
            // sub-directory and any orphaned/untracked files too.
            if let Some(board_dir) = board_upload_dir {
                if board_dir.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&board_dir) {
                        tracing::warn!("Could not remove board dir {:?}: {}", board_dir, e);
                    }
                }
            }

            tracing::info!(target: "admin", board_id = form.board_id, files_removed = paths.len(), "Board deleted");
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

// ─── POST /admin/board/reorder ───────────────────────────────────────────────

pub async fn reorder_board(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ReorderBoardForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let move_up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => {
            return Err(AppError::BadRequest(
                "Unknown board reorder direction.".into(),
            ))
        }
    };
    let return_to = safe_return_to(form.return_to.as_deref()).to_string();
    let board_id = form.board_id;

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let mut conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            db::move_board(&mut conn, board_id, move_up)?;
            crate::templates::set_live_boards(db::get_all_boards(&conn)?);
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&return_to).into_response())
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
            tracing::info!(target: "admin", action = %action, thread_id = thread_id, "Thread action");
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    // Use the board name from the DB (via the thread's board_id),
    // not the user-supplied form.board, to prevent path-confusion redirects.
    let redirect_url = {
        let pool = state.db.clone();
        let board_name = tokio::task::spawn_blocking(move || -> Result<String> {
            let conn = pool.get()?;
            let thread = db::get_thread(&conn, thread_id)?;
            let boards = db::get_all_boards(&conn).ok();
            if let Some(t) = thread {
                return Ok(resolve_board_short_name(
                    boards.as_deref(),
                    t.board_id,
                    &form.board,
                ));
            }
            // Fallback: sanitize the user-supplied board name to prevent open-redirect.
            // Only allow alphanumeric characters (matching the board short_name format).
            Ok(sanitize_board_short_value(&form.board))
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

            // Resolve board name from DB, not user-supplied form field.
            // Fallback sanitizes the user-supplied value to alphanumeric only.
            let boards = db::get_all_boards(&conn).ok();
            let board_name =
                resolve_board_short_name(boards.as_deref(), post.board_id, &form.board);

            let thread_id = post.thread_id;
            let is_op = post.is_op;

            let deleted = if post.is_op {
                db::delete_thread(&conn, post.thread_id)?
            } else {
                db::delete_post(&conn, post_id)?
            };

            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn,
                &upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
            ) {
                tracing::warn!(
                    target: "admin",
                    post_id = post_id,
                    error = %error,
                    "deleted post but file cleanup did not fully complete"
                );
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
            tracing::info!(target: "admin", post_id = post_id, "Post deleted");
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

            // Resolve board name from DB.
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

            let deleted = db::delete_thread(&conn, thread_id)?;
            if let Err(error) = crate::pending_fs::finalize_delete_files_payload(
                &conn,
                &upload_dir,
                deleted.pending_fs_op_id.as_deref(),
                &deleted.paths,
            ) {
                tracing::warn!(
                    target: "admin",
                    thread_id = thread_id,
                    error = %error,
                    "deleted thread but file cleanup did not fully complete"
                );
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
            tracing::info!(target: "admin", thread_id = thread_id, "Thread deleted");
            Ok(board_name)
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(Redirect::to(&format!("/{redirect_board}")).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header, StatusCode};
    use axum_extra::extract::cookie::{Cookie, CookieJar};

    fn build_admin_jar() -> CookieJar {
        CookieJar::new()
            .add(Cookie::new(super::super::SESSION_COOKIE, "session123"))
            .add(Cookie::new("csrf_token", "csrf123"))
    }

    fn seed_admin_data() -> (crate::middleware::AppState, i64, i64, i64) {
        let state = crate::test_support::app_state();
        let conn = state.db.get().expect("db connection");
        let password_hash = crate::utils::crypto::hash_password("hunter2").expect("hash password");
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .expect("create session");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        let board = crate::db::get_board_by_short(&conn, "test")
            .expect("load board")
            .expect("board exists");

        let op = crate::db::NewPost {
            thread_id: 0,
            board_id: board.id,
            name: "anon".to_string(),
            tripcode: None,
            subject: None,
            body: "op body".to_string(),
            body_html: "<p>op body</p>".to_string(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token-op".to_string(),
            is_op: true,
        };
        let (thread_id, _, _) =
            crate::db::create_thread_with_optional_poll(&conn, board.id, None, &op, "", None, None)
                .expect("create thread");

        let reply = crate::db::NewPost {
            thread_id,
            board_id: board.id,
            name: "anon".to_string(),
            tripcode: None,
            subject: None,
            body: "reply body".to_string(),
            body_html: "<p>reply body</p>".to_string(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token-reply".to_string(),
            is_op: false,
        };
        let reply_id = crate::db::create_reply_with_thread_update(&conn, &reply, "", true, None)
            .expect("create reply");

        (state, thread_id, reply_id, board.id)
    }

    #[test]
    fn resolve_board_short_name_falls_back_when_boards_missing() {
        assert_eq!(
            resolve_board_short_name(None, 123, "fallback-board!"),
            "fallback"
        );
    }

    #[test]
    fn resolve_board_short_name_prefers_matching_board() {
        let board = crate::models::Board {
            id: 7,
            display_order: 0,
            short_name: "tech".to_string(),
            name: "Technology".to_string(),
            description: String::new(),
            nsfw: false,
            max_threads: 100,
            max_archived_threads: 100,
            bump_limit: 500,
            allow_images: true,
            allow_video: true,
            allow_audio: true,
            allow_pdf: false,
            allow_any_files: false,
            allow_tripcodes: true,
            allow_editing: false,
            allow_self_delete: false,
            edit_window_secs: 300,
            allow_archive: true,
            allow_video_embeds: true,
            allow_captcha: false,
            show_poster_ids: false,
            collapse_greentext: false,
            post_cooldown_secs: 0,
            default_theme: String::new(),
            banner_mode: crate::models::BoardBannerMode::Inherit,
            access_mode: crate::models::BoardAccessMode::Public,
            access_password_hash: String::new(),
            created_at: 0,
        };

        assert_eq!(
            resolve_board_short_name(Some(std::slice::from_ref(&board)), 7, "fallback"),
            "tech"
        );
    }

    #[test]
    fn checked_board_upload_dir_rejects_traversal_short_name_without_touching_sentinel() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let sentinel_dir = temp_dir.path().join("sentinel");
        std::fs::create_dir_all(&upload_dir).expect("create uploads");
        std::fs::create_dir_all(&sentinel_dir).expect("create sentinel");
        std::fs::write(sentinel_dir.join("keep.txt"), "keep").expect("write sentinel");

        let error =
            checked_board_upload_dir(upload_dir.to_str().expect("utf8 upload dir"), "../sentinel")
                .expect_err("traversal short name rejected");

        assert!(error.to_string().contains("unsafe stored board short_name"));
        assert_eq!(
            std::fs::read_to_string(sentinel_dir.join("keep.txt")).expect("read sentinel"),
            "keep"
        );
    }

    #[test]
    fn checked_board_upload_dir_allows_valid_board_under_upload_root_only() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let upload_dir = temp_dir.path().join("uploads");
        let board_dir = upload_dir.join("tech");
        let other_dir = upload_dir.join("other");
        std::fs::create_dir_all(&board_dir).expect("create board dir");
        std::fs::create_dir_all(&other_dir).expect("create other dir");
        std::fs::write(board_dir.join("old.txt"), "old").expect("write board file");
        std::fs::write(other_dir.join("keep.txt"), "keep").expect("write other file");

        let checked =
            checked_board_upload_dir(upload_dir.to_str().expect("utf8 upload dir"), "tech")
                .expect("valid board path");
        std::fs::remove_dir_all(&checked).expect("remove checked board dir");

        assert!(!board_dir.exists());
        assert_eq!(
            std::fs::read_to_string(other_dir.join("keep.txt")).expect("read other file"),
            "keep"
        );
    }

    #[tokio::test]
    async fn admin_delete_post_uses_fallback_board_when_lookup_breaks() {
        let (state, thread_id, reply_id, _board_id) = seed_admin_data();
        let conn = state.db.get().expect("db connection");
        conn.execute_batch("ALTER TABLE boards RENAME COLUMN short_name TO short_name_broken")
            .expect("break board lookup");

        let response = admin_delete_post(
            State(state),
            build_admin_jar(),
            Form(AdminDeletePostForm {
                post_id: reply_id,
                board: "fallback".to_string(),
                csrf: Some("csrf123".to_string()),
            }),
        )
        .await
        .expect("handler response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("location header");
        assert_eq!(location, format!("/fallback/thread/{thread_id}"));
    }

    #[tokio::test]
    async fn thread_action_uses_fallback_board_when_lookup_breaks() {
        let (state, thread_id, _reply_id, _board_id) = seed_admin_data();
        let conn = state.db.get().expect("db connection");
        conn.execute_batch("ALTER TABLE boards RENAME COLUMN short_name TO short_name_broken")
            .expect("break board lookup");

        let response = thread_action(
            State(state),
            build_admin_jar(),
            Form(ThreadActionForm {
                thread_id,
                board: "fallback".to_string(),
                action: "lock".to_string(),
                csrf: Some("csrf123".to_string()),
            }),
        )
        .await
        .expect("handler response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("location header");
        assert_eq!(location, format!("/fallback/thread/{thread_id}"));
    }
}
