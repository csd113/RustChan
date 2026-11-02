//! Shared validation and body reconstruction for restored database snapshots.

use super::safety::{self, render_restored_body_html, validate_board_short_name};
use crate::error::{AppError, Result};
use rusqlite::params;
use std::collections::HashMap;

#[expect(
    clippy::too_many_lines,
    reason = "all restored-database trust checks remain contiguous before any live-state mutation"
)]
/// Validates full restore database trust boundary.
pub(super) fn validate_full_restore_db_trust_boundary(conn: &rusqlite::Connection) -> Result<()> {
    let mut valid_board_shorts = std::collections::HashSet::new();
    let mut board_ids_to_shorts = HashMap::new();
    let mut stmt = conn
        .prepare("SELECT id, short_name FROM boards")
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in rows {
        let (board_id, short_name) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid board row: {error}"
            ))
        })?;
        validate_board_short_name(&short_name)?;
        let _completed_value = valid_board_shorts.insert(short_name.clone());
        let _previous_value = board_ids_to_shorts.insert(board_id, short_name);
    }

    let mut post_stmt = conn
        .prepare(
            "SELECT id, board_id, file_path, thumb_path, audio_file_path
             FROM posts
             WHERE file_path IS NOT NULL OR thumb_path IS NOT NULL OR audio_file_path IS NOT NULL",
        )
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let post_rows = post_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in post_rows {
        let (post_id, board_id, file_path, thumb_path, audio_file_path) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid post media row: {error}"
            ))
        })?;
        let expected_board_short = board_ids_to_shorts.get(&board_id).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Restored post {post_id} points to unknown board_id {board_id}."
            ))
        })?;
        for (label, path) in [
            ("file_path", file_path.as_deref()),
            ("thumb_path", thumb_path.as_deref()),
            ("audio_file_path", audio_file_path.as_deref()),
        ] {
            let Some(path) = path else {
                continue;
            };
            let board_short = safety::validate_restored_media_path(
                path,
                &format!("Restored post {post_id} {label}"),
            )?;
            if !valid_board_shorts.contains(&board_short) {
                return Err(AppError::BadRequest(format!(
                    "Restored post {post_id} {label} points to unknown board /{board_short}/."
                )));
            }
            if &board_short != expected_board_short {
                return Err(AppError::BadRequest(format!(
                    "Restored post {post_id} {label} escapes its board /{expected_board_short}/."
                )));
            }
        }
    }

    let mut file_hash_stmt = conn
        .prepare("SELECT sha256, file_path, thumb_path FROM file_hashes")
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;
    let file_hash_rows = file_hash_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| AppError::BadRequest(format!("Restored database is invalid: {error}")))?;

    for row in file_hash_rows {
        let (sha256, file_path, thumb_path) = row.map_err(|error| {
            AppError::BadRequest(format!(
                "Restored database has an invalid file_hash row: {error}"
            ))
        })?;
        let file_board_short = safety::validate_restored_media_path(
            &file_path,
            &format!("Restored file_hash {sha256} file_path"),
        )?;
        if !valid_board_shorts.contains(&file_board_short) {
            return Err(AppError::BadRequest(format!(
                "Restored file_hash {sha256} file_path points to unknown board /{file_board_short}/."
            )));
        }
        if !thumb_path.is_empty() {
            let thumb_board_short = safety::validate_restored_media_path(
                &thumb_path,
                &format!("Restored file_hash {sha256} thumb_path"),
            )?;
            if !valid_board_shorts.contains(&thumb_board_short) {
                return Err(AppError::BadRequest(format!(
                    "Restored file_hash {sha256} thumb_path points to unknown board /{thumb_board_short}/."
                )));
            }
            if thumb_board_short != file_board_short {
                return Err(AppError::BadRequest(format!(
                    "Restored file_hash {sha256} mixes boards between file_path and thumb_path."
                )));
            }
        }
    }

    Ok(())
}

pub(super) fn recompute_restored_post_body_html(conn: &rusqlite::Connection) -> Result<()> {
    // Keyset pages prevent an 8 GiB snapshot from materializing every body.
    // Incremental reads bound bytes before allocation and count NUL bytes as
    // Rust scalars, unlike SQLite text length/substr functions.
    let mut before: Option<i64> = None;
    loop {
        let sql = if before.is_none() {
            "SELECT id, typeof(body) FROM posts WHERE id >= ?1 ORDER BY id LIMIT 128"
        } else {
            "SELECT id, typeof(body) FROM posts WHERE id > ?1 ORDER BY id LIMIT 128"
        };
        let posts = conn
            .prepare_cached(sql)?
            .query_map([before.unwrap_or(i64::MIN)], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if posts.is_empty() {
            break;
        }
        for (post_id, body_type) in posts {
            if body_type != "text" {
                return Err(AppError::BadRequest(
                    "Restored database contains an invalid post body.".into(),
                ));
            }
            let blob = conn.blob_open("main", "posts", "body", post_id, true)?;
            if blob.len() > 4 * 4096 {
                return Err(AppError::BadRequest(
                    "Restored database contains an oversized post body.".into(),
                ));
            }
            let mut bytes = vec![0; blob.len()];
            blob.read_at_exact(&mut bytes, 0)?;
            drop(blob);
            let body = String::from_utf8(bytes).map_err(|_error| {
                AppError::BadRequest("Restored post bodies must be valid UTF-8.".into())
            })?;
            if body.chars().count() > 4096 {
                return Err(AppError::BadRequest(
                    "Restored database contains an oversized post body.".into(),
                ));
            }
            let body_html = render_restored_body_html(&body);
            conn.execute(
                "UPDATE posts SET body_html=?1 WHERE id=?2",
                params![body_html, post_id],
            )
            .map(|_rows| ())?;
            before = Some(post_id);
        }
    }

    Ok(())
}
