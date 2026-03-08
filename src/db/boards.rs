// db/boards.rs — Board-level queries and site settings.
//
// Covers: site_settings table, boards CRUD, delete_board (with file-safety
// guard via super::paths_safe_to_delete), and aggregate site statistics.

use crate::models::*;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

// ─── Row mapper ───────────────────────────────────────────────────────────────

pub(super) fn map_board(row: &rusqlite::Row<'_>) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get(0)?,
        short_name: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        nsfw: row.get::<_, i32>(4)? != 0,
        max_threads: row.get(5)?,
        bump_limit: row.get(6)?,
        allow_images: row.get::<_, i32>(7)? != 0,
        allow_video: row.get::<_, i32>(8)? != 0,
        allow_audio: row.get::<_, i32>(9)? != 0,
        allow_tripcodes: row.get::<_, i32>(10)? != 0,
        edit_window_secs: row.get(11)?,
        allow_editing: row.get::<_, i32>(12)? != 0,
        allow_archive: row.get::<_, i32>(13)? != 0,
        allow_video_embeds: row.get::<_, i32>(14)? != 0,
        allow_captcha: row.get::<_, i32>(15)? != 0,
        post_cooldown_secs: row.get(16)?,
        created_at: row.get(17)?,
    })
}

// ─── Site settings ────────────────────────────────────────────────────────────

/// Read a site-wide setting by key. Returns None if the key has never been set.
pub fn get_site_setting(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT value FROM site_settings WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(result)
}

/// Write (upsert) a site-wide setting.
pub fn set_site_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Returns the admin-configured site name, or falls back to CONFIG.forum_name.
pub fn get_site_name(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "site_name")
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| crate::config::CONFIG.forum_name.clone())
}

pub fn get_site_subtitle(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "site_subtitle")
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "select board to proceed".to_string())
}

/// Convenience: read the collapsible-greentext toggle (default: false).
pub fn get_collapse_greentext(conn: &rusqlite::Connection) -> bool {
    get_site_setting(conn, "collapse_greentext")
        .ok()
        .flatten()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ─── Board queries ────────────────────────────────────────────────────────────

pub fn get_all_boards(conn: &rusqlite::Connection) -> Result<Vec<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                post_cooldown_secs, created_at
         FROM boards ORDER BY id ASC",
    )?;
    let boards = stmt
        .query_map([], map_board)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(boards)
}

/// Like get_all_boards but also returns live thread count for each board.
pub fn get_all_boards_with_stats(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::BoardStats>> {
    let boards = get_all_boards(conn)?;
    let mut out = Vec::with_capacity(boards.len());
    for board in boards {
        let thread_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
            params![board.id],
            |r| r.get(0),
        )?;
        out.push(crate::models::BoardStats {
            board,
            thread_count,
        });
    }
    Ok(out)
}

pub fn get_board_by_short(conn: &rusqlite::Connection, short: &str) -> Result<Option<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                allow_editing, allow_archive, allow_video_embeds, allow_captcha,
                post_cooldown_secs, created_at
         FROM boards WHERE short_name = ?1",
    )?;
    Ok(stmt.query_row(params![short], map_board).optional()?)
}

pub fn create_board(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<i64> {
    // New boards default to images and video enabled; audio off by default.
    conn.execute(
        "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
         VALUES (?1, ?2, ?3, ?4, 1, 1, 0)",
        params![short, name, description, nsfw as i32],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a board with explicit per-media-type toggles.
/// Used by the CLI `--no-images / --no-videos / --no-audio` flags.
#[allow(clippy::too_many_arguments)]
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
    conn.execute(
        "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            short, name, description, nsfw as i32,
            allow_images as i32, allow_video as i32, allow_audio as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(dead_code)]
pub fn update_board(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE boards SET name=?1, description=?2, nsfw=?3 WHERE id=?4",
        params![name, description, nsfw as i32, id],
    )?;
    Ok(())
}

/// Update all per-board settings from the admin panel.
#[allow(clippy::too_many_arguments)]
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
    conn.execute(
        "UPDATE boards SET name=?1, description=?2, nsfw=?3,
         bump_limit=?4, max_threads=?5,
         allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
         edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
         allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
         WHERE id=?16",
        params![
            name,
            description,
            nsfw as i32,
            bump_limit,
            max_threads,
            allow_images as i32,
            allow_video as i32,
            allow_audio as i32,
            allow_tripcodes as i32,
            edit_window_secs,
            allow_editing as i32,
            allow_archive as i32,
            allow_video_embeds as i32,
            allow_captcha as i32,
            post_cooldown_secs,
            id,
        ],
    )?;
    Ok(())
}

/// Returns how many seconds have elapsed since `ip_hash` last posted on `board_id`.
/// Returns None if they have never posted on this board.
pub fn get_seconds_since_last_post(
    conn: &rusqlite::Connection,
    board_id: i64,
    ip_hash: &str,
) -> Result<Option<i64>> {
    let result = conn
        .query_row(
            "SELECT unixepoch() - MAX(created_at) FROM posts
             WHERE board_id = ?1 AND ip_hash = ?2",
            params![board_id, ip_hash],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten();
    Ok(result)
}

pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    // Collect every file path that belongs to this board before deletion.
    // The CASCADE on boards→threads→posts handles DB row removal, but the
    // on-disk files must be cleaned up by the caller.
    let mut stmt = conn.prepare(
        "SELECT p.file_path, p.thumb_path, p.audio_file_path
         FROM posts p
         JOIN threads t ON p.thread_id = t.id
         WHERE t.board_id = ?1",
    )?;
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
        .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut candidates = Vec::new();
    for (f, t, a) in rows {
        if let Some(p) = f {
            candidates.push(p);
        }
        if let Some(p) = t {
            candidates.push(p);
        }
        if let Some(p) = a {
            candidates.push(p);
        }
    }

    // Cascade deletes threads, posts, polls, etc.
    conn.execute("DELETE FROM boards WHERE id = ?1", params![id])?;
    // A board deletion removes every post on the board, but a file may be
    // shared with a post on a different board via deduplication; protect those.
    Ok(super::paths_safe_to_delete(conn, candidates))
}

// ─── Site statistics ──────────────────────────────────────────────────────────

/// Gather aggregate site-wide statistics for the home page.
///
/// Uses a single pass over the posts table to count totals by media_type,
/// plus a SUM of file_size for posts that still have a file on disk.
pub fn get_site_stats(conn: &rusqlite::Connection) -> Result<crate::models::SiteStats> {
    let total_posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;

    let total_images: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'image'",
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'video'",
        [],
        |r| r.get(0),
    )?;
    let total_audio: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'audio'",
        [],
        |r| r.get(0),
    )?;
    let active_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(file_size), 0) FROM posts WHERE file_path IS NOT NULL AND file_size IS NOT NULL",
        [], |r| r.get(0),
    )?;

    Ok(crate::models::SiteStats {
        total_posts,
        total_images,
        total_videos,
        total_audio,
        active_bytes,
    })
}
