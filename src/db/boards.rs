// db/boards.rs — Board-level queries and site settings.
//
// Covers: site_settings table, boards CRUD, delete_board (with file-safety
// guard via super::paths_safe_to_delete), and aggregate site statistics.
//
use crate::models::{Board, BoardStats, SiteStats};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;

fn parse_bool_setting(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn validate_board_text_fields(short: &str, name: &str, description: &str) -> Result<()> {
    if short.trim().is_empty() {
        return Err(anyhow!("Board short name cannot be empty"));
    }
    if name.trim().is_empty() {
        return Err(anyhow!("Board name cannot be empty"));
    }
    if description.len() > i32::MAX as usize {
        return Err(anyhow!("Board description is too long"));
    }
    Ok(())
}

fn validate_board_update_fields(name: &str, description: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("Board name cannot be empty"));
    }
    if description.len() > i32::MAX as usize {
        return Err(anyhow!("Board description is too long"));
    }
    Ok(())
}

fn validate_board_settings_ranges(
    bump_limit: i64,
    max_threads: i64,
    edit_window_secs: i64,
    post_cooldown_secs: i64,
) -> Result<()> {
    if bump_limit < 0 {
        return Err(anyhow!("bump_limit cannot be negative"));
    }
    if max_threads < 0 {
        return Err(anyhow!("max_threads cannot be negative"));
    }
    if edit_window_secs < 0 {
        return Err(anyhow!("edit_window_secs cannot be negative"));
    }
    if post_cooldown_secs < 0 {
        return Err(anyhow!("post_cooldown_secs cannot be negative"));
    }
    Ok(())
}

pub(super) fn map_board(row: &rusqlite::Row<'_>) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get("id")?,
        short_name: row.get("short_name")?,
        name: row.get("name")?,
        description: row.get("description")?,
        nsfw: row.get::<_, i32>("nsfw")? != 0,
        max_threads: row.get("max_threads")?,
        bump_limit: row.get("bump_limit")?,
        allow_images: row.get::<_, i32>("allow_images")? != 0,
        allow_video: row.get::<_, i32>("allow_video")? != 0,
        allow_audio: row.get::<_, i32>("allow_audio")? != 0,
        allow_tripcodes: row.get::<_, i32>("allow_tripcodes")? != 0,
        edit_window_secs: row.get("edit_window_secs")?,
        allow_editing: row.get::<_, i32>("allow_editing")? != 0,
        allow_archive: row.get::<_, i32>("allow_archive")? != 0,
        allow_video_embeds: row.get::<_, i32>("allow_video_embeds")? != 0,
        allow_captcha: row.get::<_, i32>("allow_captcha")? != 0,
        post_cooldown_secs: row.get("post_cooldown_secs")?,
        created_at: row.get("created_at")?,
    })
}

/// Read a site-wide setting by key. Returns `None` if the key has never been set.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_site_setting(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT value FROM site_settings WHERE key = ?1")
        .context("Failed to prepare get_site_setting statement")?;
    stmt.query_row(params![key], |row| row.get::<_, String>(0))
        .optional()
        .context("Failed to query site setting")
}

/// Write (upsert) a site-wide setting.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn set_site_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    let _rows = conn
        .execute(
            "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .context("Failed to upsert site setting")?;
    Ok(())
}

/// Returns the admin-configured site name, or falls back to `CONFIG.forum_name`.
pub fn get_site_name(conn: &rusqlite::Connection) -> String {
    match get_site_setting(conn, "site_name") {
        Ok(Some(value)) if !value.trim().is_empty() => value,
        Ok(_) | Err(_) => crate::config::CONFIG.forum_name.clone(),
    }
}

/// Returns the admin-configured site subtitle, or a default when unset/empty.
pub fn get_site_subtitle(conn: &rusqlite::Connection) -> String {
    match get_site_setting(conn, "site_subtitle") {
        Ok(Some(value)) if !value.trim().is_empty() => value,
        Ok(_) | Err(_) => String::from("select board to proceed"),
    }
}

/// Returns the default UI theme. Empty or whitespace-only values map to `"terminal"`.
pub fn get_default_user_theme(conn: &rusqlite::Connection) -> String {
    match get_site_setting(conn, "default_theme") {
        Ok(Some(value)) if !value.trim().is_empty() => value,
        Ok(_) | Err(_) => String::from("terminal"),
    }
}

/// Returns whether collapsible greentext is enabled. Defaults to `false`.
pub fn get_collapse_greentext(conn: &rusqlite::Connection) -> bool {
    match get_site_setting(conn, "collapse_greentext") {
        Ok(Some(value)) => parse_bool_setting(&value),
        Ok(None) | Err(_) => false,
    }
}

/// Returns all boards ordered by ascending id.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_all_boards(conn: &rusqlite::Connection) -> Result<Vec<Board>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT
                 id,
                 short_name,
                 name,
                 description,
                 nsfw,
                 max_threads,
                 bump_limit,
                 allow_images,
                 allow_video,
                 allow_audio,
                 allow_tripcodes,
                 edit_window_secs,
                 allow_editing,
                 allow_archive,
                 allow_video_embeds,
                 allow_captcha,
                 post_cooldown_secs,
                 created_at
             FROM boards
             ORDER BY id ASC",
        )
        .context("Failed to prepare get_all_boards statement")?;
    let rows = stmt
        .query_map([], map_board)
        .context("Failed to map boards")?;
    let boards = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect boards")?;
    Ok(boards)
}

/// Like `get_all_boards` but also returns live thread count for each board.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_all_boards_with_stats(conn: &rusqlite::Connection) -> Result<Vec<BoardStats>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT
                 b.id,
                 b.short_name,
                 b.name,
                 b.description,
                 b.nsfw,
                 b.max_threads,
                 b.bump_limit,
                 b.allow_images,
                 b.allow_video,
                 b.allow_audio,
                 b.allow_tripcodes,
                 b.edit_window_secs,
                 b.allow_editing,
                 b.allow_archive,
                 b.allow_video_embeds,
                 b.allow_captcha,
                 b.post_cooldown_secs,
                 b.created_at,
                 COALESCE(tc.thread_count, 0) AS thread_count
             FROM boards AS b
             LEFT JOIN (
                 SELECT board_id, COUNT(*) AS thread_count
                 FROM threads
                 WHERE archived = 0
                 GROUP BY board_id
             ) AS tc
                 ON tc.board_id = b.id
             ORDER BY b.id ASC",
        )
        .context("Failed to prepare get_all_boards_with_stats statement")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(BoardStats {
                board: map_board(row)?,
                thread_count: row.get("thread_count")?,
            })
        })
        .context("Failed to map board stats")?;
    let stats = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect board stats")?;
    Ok(stats)
}

/// Returns a board by its short name.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_board_by_short(conn: &rusqlite::Connection, short: &str) -> Result<Option<Board>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT
                 id,
                 short_name,
                 name,
                 description,
                 nsfw,
                 max_threads,
                 bump_limit,
                 allow_images,
                 allow_video,
                 allow_audio,
                 allow_tripcodes,
                 edit_window_secs,
                 allow_editing,
                 allow_archive,
                 allow_video_embeds,
                 allow_captcha,
                 post_cooldown_secs,
                 created_at
             FROM boards
             WHERE short_name = ?1",
        )
        .context("Failed to prepare get_board_by_short statement")?;
    stmt.query_row(params![short], map_board)
        .optional()
        .context("Failed to query board by short name")
}

/// Create a board with default media flags.
///
/// # Errors
/// Returns an error if the database operation fails or input is invalid.
pub fn create_board(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<i64> {
    validate_board_text_fields(short, name, description)?;

    conn.query_row(
        "INSERT INTO boards (
             short_name,
             name,
             description,
             nsfw,
             allow_images,
             allow_video,
             allow_audio
         )
         VALUES (?1, ?2, ?3, ?4, 1, 1, 0)
         RETURNING id",
        params![short, name, description, i32::from(nsfw)],
        |row| row.get(0),
    )
    .context("Failed to create board")
}

/// Create a board with explicit per-media-type toggles.
///
/// # Errors
/// Returns an error if the database operation fails or input is invalid.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn create_board_with_media_flags(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
    allow_images: bool,
    allow_video: bool,
    allow_audio: bool,
) -> Result<i64> {
    validate_board_text_fields(short, name, description)?;

    conn.query_row(
        "INSERT INTO boards (
             short_name,
             name,
             description,
             nsfw,
             allow_images,
             allow_video,
             allow_audio
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         RETURNING id",
        params![
            short,
            name,
            description,
            i32::from(nsfw),
            i32::from(allow_images),
            i32::from(allow_video),
            i32::from(allow_audio)
        ],
        |row| row.get(0),
    )
    .context("Failed to create board with media flags")
}

/// Update a board's basic metadata.
///
/// # Errors
/// Returns an error if the database operation fails, input is invalid, or the board is not found.
#[allow(dead_code)]
pub fn update_board(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<()> {
    validate_board_update_fields(name, description)?;

    let rows_affected = conn
        .execute(
            "UPDATE boards
             SET name = ?1, description = ?2, nsfw = ?3
             WHERE id = ?4",
            params![name, description, i32::from(nsfw), id],
        )
        .context("Failed to update board")?;
    if rows_affected == 0 {
        return Err(anyhow!("Board id {id} not found"));
    }
    Ok(())
}

/// Update all per-board settings from the admin panel.
///
/// # Errors
/// Returns an error if the database operation fails, input is invalid, or the board is not found.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn update_board_settings(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
    bump_limit: i64,
    max_threads: i64,
    allow_images: bool,
    allow_video: bool,
    allow_audio: bool,
    allow_tripcodes: bool,
    edit_window_secs: i64,
    allow_editing: bool,
    allow_archive: bool,
    allow_video_embeds: bool,
    allow_captcha: bool,
    post_cooldown_secs: i64,
) -> Result<()> {
    validate_board_update_fields(name, description)?;
    validate_board_settings_ranges(
        bump_limit,
        max_threads,
        edit_window_secs,
        post_cooldown_secs,
    )?;

    let rows_affected = conn
        .execute(
            "UPDATE boards
             SET
                 name = ?1,
                 description = ?2,
                 nsfw = ?3,
                 bump_limit = ?4,
                 max_threads = ?5,
                 allow_images = ?6,
                 allow_video = ?7,
                 allow_audio = ?8,
                 allow_tripcodes = ?9,
                 edit_window_secs = ?10,
                 allow_editing = ?11,
                 allow_archive = ?12,
                 allow_video_embeds = ?13,
                 allow_captcha = ?14,
                 post_cooldown_secs = ?15
             WHERE id = ?16",
            params![
                name,
                description,
                i32::from(nsfw),
                bump_limit,
                max_threads,
                i32::from(allow_images),
                i32::from(allow_video),
                i32::from(allow_audio),
                i32::from(allow_tripcodes),
                edit_window_secs,
                i32::from(allow_editing),
                i32::from(allow_archive),
                i32::from(allow_video_embeds),
                i32::from(allow_captcha),
                post_cooldown_secs,
                id
            ],
        )
        .context("Failed to update board settings")?;
    if rows_affected == 0 {
        return Err(anyhow!("Board id {id} not found"));
    }
    Ok(())
}

/// Returns how many seconds have elapsed since `ip_hash` last posted on `board_id`.
/// Returns `None` if they have never posted on this board.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_seconds_since_last_post(
    conn: &rusqlite::Connection,
    board_id: i64,
    ip_hash: &str,
) -> Result<Option<i64>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT unixepoch() - MAX(created_at)
             FROM posts
             WHERE board_id = ?1 AND ip_hash = ?2",
        )
        .context("Failed to prepare get_seconds_since_last_post statement")?;
    stmt.query_row(params![board_id, ip_hash], |row| {
        row.get::<_, Option<i64>>(0)
    })
    .context("Failed to query seconds since last post")
}

/// Delete a board and return on-disk paths that are now safe to remove.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_board transaction")?;

    let result = (|| -> Result<Vec<String>> {
        let foreign_keys_enabled = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .context("Failed to verify foreign key enforcement")?;
        if foreign_keys_enabled == 0 {
            return Err(anyhow!(
                "Foreign key enforcement is disabled; delete_board requires `PRAGMA foreign_keys = ON`"
            ));
        }

        let board_exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM boards WHERE id = ?1)",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .context("Failed to verify board existence")?;
        if board_exists == 0 {
            return Err(anyhow!("Board id {id} not found"));
        }

        let mut stmt = conn
            .prepare_cached(
                "SELECT file_path, thumb_path, audio_file_path
                 FROM posts
                 WHERE board_id = ?1",
            )
            .context("Failed to prepare delete_board file path query")?;

        let rows = stmt
            .query_map(params![id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .context("Failed to query delete_board file paths")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to collect delete_board file paths")?;

        let mut candidates = HashSet::<String>::new();
        for (file_path, thumb_path, audio_file_path) in rows {
            if let Some(path) = file_path {
                let _inserted = candidates.insert(path);
            }
            if let Some(path) = thumb_path {
                let _inserted = candidates.insert(path);
            }
            if let Some(path) = audio_file_path {
                let _inserted = candidates.insert(path);
            }
        }

        drop(stmt);

        let rows_affected = conn
            .execute("DELETE FROM boards WHERE id = ?1", params![id])
            .context("Failed to delete board")?;
        if rows_affected == 0 {
            return Err(anyhow!("Board id {id} not found"));
        }

        let safe_paths =
            super::paths_safe_to_delete(conn, candidates.into_iter().collect::<Vec<String>>())
                .context("Failed to determine safe paths to delete")?;

        Ok(safe_paths)
    })();

    match result {
        Ok(safe_paths) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_board transaction")?;
            Ok(safe_paths)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// Per-board thread and post counts for terminal stats display.
pub fn get_per_board_stats(conn: &rusqlite::Connection) -> Vec<(String, i64, i64)> {
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT
             b.short_name,
             COALESCE(t.thread_count, 0) AS thread_count,
             COALESCE(p.post_count, 0) AS post_count
         FROM boards AS b
         LEFT JOIN (
             SELECT board_id, COUNT(*) AS thread_count
             FROM threads
             GROUP BY board_id
         ) AS t
             ON t.board_id = b.id
         LEFT JOIN (
             SELECT board_id, COUNT(*) AS post_count
             FROM posts
             GROUP BY board_id
         ) AS p
             ON p.board_id = b.id
         ORDER BY b.short_name",
    ) else {
        return Vec::new();
    };

    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    }) else {
        return Vec::new();
    };

    rows.filter_map(std::result::Result::ok).collect()
}

/// Gather aggregate site-wide statistics for the home page.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_site_stats(conn: &rusqlite::Connection) -> Result<SiteStats> {
    conn.query_row(
        "SELECT
             COUNT(*) AS total_posts,
             SUM(CASE WHEN media_type = 'image' THEN 1 ELSE 0 END) AS total_images,
             SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END) AS total_videos,
             SUM(CASE WHEN media_type = 'audio' THEN 1 ELSE 0 END) AS total_audio,
             COALESCE(
                 SUM(
                     CASE
                         WHEN file_path IS NOT NULL AND file_size IS NOT NULL THEN file_size
                         ELSE 0
                     END
                 ),
                 0
             ) + COALESCE(
                 SUM(
                     CASE
                         WHEN audio_file_path IS NOT NULL AND audio_file_size IS NOT NULL
                             THEN audio_file_size
                         ELSE 0
                     END
                 ),
                 0
             ) AS active_bytes
         FROM posts",
        [],
        |row| {
            Ok(SiteStats {
                total_posts: row.get(0)?,
                total_images: row.get(1)?,
                total_videos: row.get(2)?,
                total_audio: row.get(3)?,
                active_bytes: row.get(4)?,
            })
        },
    )
    .context("Failed to query site stats")
}
