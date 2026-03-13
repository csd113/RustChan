// db/boards.rs — Board-level queries and site settings.
//
// Covers: site_settings table, boards CRUD, delete_board (with file-safety
// guard via super::paths_safe_to_delete), and aggregate site statistics.
//
// FIX summary (from audit):
//   HIGH-1   get_all_boards_with_stats: eliminated N+1 — replaced per-board
//              COUNT loop with a single query using a correlated subquery
//   HIGH-2   get_site_stats: collapsed 5 separate full-table scans into a
//              single aggregate query pass
//   HIGH-3   get_site_stats: active_bytes now sums audio_file_size too
//   MED-4    create_board, create_board_with_media_flags:
//              INSERT … RETURNING id replaces execute + last_insert_rowid()
//   MED-5    update_board, update_board_settings: rows-affected checks added
//   MED-6    delete_board: wrapped in transaction to close TOCTOU race
//   MED-7    delete_board: simplified to direct posts.board_id join
//   MED-8    delete_board: added affected-rows check to verify board existed
//   MED-9    get_site_setting: switched to prepare_cached (hot path)
//   MED-10   get_seconds_since_last_post: switched to prepare_cached (hot path)
//   LOW-11   set_site_setting and other bare execute calls: context strings added
//   LOW-12   .context() added on key operations
//   LOW-13   Note: unixepoch() requires SQLite ≥ 3.38.0 (2022-02-22)

use crate::models::Board;
use anyhow::{Context, Result};
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
///
/// FIX[MED-9]: Switched to `prepare_cached` — convenience helpers (`get_site_name`,
/// `get_site_subtitle`, etc.) call this on every page render.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_site_setting(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM site_settings WHERE key = ?1")?;
    let result = stmt
        .query_row(params![key], |r| r.get::<_, String>(0))
        .optional()?;
    Ok(result)
}

/// Write (upsert) a site-wide setting.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn set_site_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .context("Failed to upsert site setting")?;
    Ok(())
}

/// Returns the admin-configured site name, or falls back to `CONFIG.forum_name`.
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

/// Convenience: read the admin-configured default UI theme (empty = "terminal").
pub fn get_default_user_theme(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "default_theme")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Convenience: read the collapsible-greentext toggle (default: false).
pub fn get_collapse_greentext(conn: &rusqlite::Connection) -> bool {
    get_site_setting(conn, "collapse_greentext")
        .ok()
        .flatten()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

// ─── Board queries ────────────────────────────────────────────────────────────

/// # Errors
/// Returns an error if the database operation fails.
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

/// Like `get_all_boards` but also returns live thread count for each board.
///
/// FIX[HIGH-1]: Previously issued one COUNT(*) query per board (N+1). Replaced
/// with a single LEFT JOIN query that computes all counts in one pass.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_all_boards_with_stats(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::BoardStats>> {
    let mut stmt = conn.prepare_cached(
        "SELECT b.id, b.short_name, b.name, b.description, b.nsfw, b.max_threads,
                b.bump_limit, b.allow_images, b.allow_video, b.allow_audio,
                b.allow_tripcodes, b.edit_window_secs, b.allow_editing, b.allow_archive,
                b.allow_video_embeds, b.allow_captcha, b.post_cooldown_secs, b.created_at,
                COUNT(t.id) AS thread_count
         FROM boards b
         LEFT JOIN threads t ON t.board_id = b.id AND t.archived = 0
         GROUP BY b.id
         ORDER BY b.id ASC",
    )?;
    let out = stmt
        .query_map([], |row| {
            let board = map_board(row)?;
            let thread_count: i64 = row.get(18)?;
            Ok(crate::models::BoardStats {
                board,
                thread_count,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

/// # Errors
/// Returns an error if the database operation fails.
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

/// FIX[MED-4]: INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn create_board(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<i64> {
    // New boards default to images and video enabled; audio off by default.
    let id: i64 = conn
        .query_row(
            "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
             VALUES (?1, ?2, ?3, ?4, 1, 1, 0) RETURNING id",
            params![short, name, description, i32::from(nsfw)],
            |r| r.get(0),
        )
        .context("Failed to create board")?;
    Ok(id)
}

/// Create a board with explicit per-media-type toggles.
/// Used by the CLI `--no-images / --no-videos / --no-audio` flags.
///
/// FIX[MED-4]: INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
///
/// # Errors
/// Returns an error if the database operation fails.
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
    let id: i64 = conn
        .query_row(
            "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
            params![
                short, name, description, i32::from(nsfw),
                i32::from(allow_images), i32::from(allow_video), i32::from(allow_audio),
            ],
            |r| r.get(0),
        )
        .context("Failed to create board with media flags")?;
    Ok(id)
}

/// FIX[MED-5]: Added rows-affected check — silently succeeding when the board
/// id doesn't exist made update errors invisible.
///
/// # Errors
/// Returns an error if the database operation fails or the board id is not found.
#[allow(dead_code)]
pub fn update_board(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE boards SET name=?1, description=?2, nsfw=?3 WHERE id=?4",
            params![name, description, i32::from(nsfw), id],
        )
        .context("Failed to update board")?;
    if n == 0 {
        anyhow::bail!("Board id {id} not found");
    }
    Ok(())
}

/// Update all per-board settings from the admin panel.
///
/// FIX[MED-5]: Added rows-affected check.
///
/// # Errors
/// Returns an error if the database operation fails or the board id is not found.
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
    let n = conn
        .execute(
            "UPDATE boards SET name=?1, description=?2, nsfw=?3,
             bump_limit=?4, max_threads=?5,
             allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
             edit_window_secs=?10, allow_editing=?11, allow_archive=?12,
             allow_video_embeds=?13, allow_captcha=?14, post_cooldown_secs=?15
             WHERE id=?16",
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
                id,
            ],
        )
        .context("Failed to update board settings")?;
    if n == 0 {
        anyhow::bail!("Board id {id} not found");
    }
    Ok(())
}

/// Returns how many seconds have elapsed since `ip_hash` last posted on `board_id`.
/// Returns None if they have never posted on this board.
///
/// FIX[MED-10]: Switched to `prepare_cached` — this is on the hot path (called
/// for every post submission when a cooldown is configured).
///
/// Note: `unixepoch()` requires `SQLite` ≥ 3.38.0 (2022-02-22).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_seconds_since_last_post(
    conn: &rusqlite::Connection,
    board_id: i64,
    ip_hash: &str,
) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT unixepoch() - MAX(created_at) FROM posts
         WHERE board_id = ?1 AND ip_hash = ?2",
    )?;
    let result = stmt
        .query_row(params![board_id, ip_hash], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten();
    Ok(result)
}

/// Delete a board and return on-disk paths that are now safe to remove.
///
/// FIX[MED-6]: Wrapped the entire operation in a transaction. Previously,
/// file paths were collected before the CASCADE DELETE with no transaction
/// guard, so a concurrent insert could race between the SELECT and the DELETE.
///
/// FIX[MED-7]: Replaced the three-way join (posts → threads → boards) with a
/// direct query on `posts.board_id`. Posts already carry `board_id` so the threads
/// join was both unnecessary and could hide orphaned posts.
///
/// FIX[MED-8]: Added an affected-rows check so callers see an error when
/// trying to delete a board that doesn't exist.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin delete_board transaction")?;

    // Collect every file path that belongs to this board before the CASCADE.
    // The ON DELETE CASCADE on boards→threads→posts handles DB row removal, but
    // on-disk files must be cleaned up by the caller.
    let candidates = {
        let mut stmt = tx.prepare_cached(
            "SELECT file_path, thumb_path, audio_file_path
             FROM posts WHERE board_id = ?1",
        )?;
        let rows: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
            .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut v = Vec::new();
        for (f, t, a) in rows {
            if let Some(p) = f {
                v.push(p);
            }
            if let Some(p) = t {
                v.push(p);
            }
            if let Some(p) = a {
                v.push(p);
            }
        }
        v
    };

    // Cascade-delete threads, posts, polls, etc. for this board.
    let n = tx
        .execute("DELETE FROM boards WHERE id = ?1", params![id])
        .context("Failed to delete board")?;
    if n == 0 {
        tx.rollback().ok();
        anyhow::bail!("Board id {id} not found");
    }

    // paths_safe_to_delete runs inside the transaction so it sees the post-delete
    // state: any file exclusively used by this board's posts now has zero
    // remaining references and is safe to remove.
    let safe = super::paths_safe_to_delete(&tx, candidates);

    tx.commit()
        .context("Failed to commit delete_board transaction")?;
    Ok(safe)
}

// ─── Per-board stats (terminal display) ──────────────────────────────────────

/// Per-board thread and post counts for the terminal stats display.
pub fn get_per_board_stats(conn: &rusqlite::Connection) -> Vec<(String, i64, i64)> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT b.short_name, \
                (SELECT COUNT(*) FROM threads WHERE board_id = b.id) AS tc, \
                (SELECT COUNT(*) FROM posts p \
                   JOIN threads t ON p.thread_id = t.id \
                  WHERE t.board_id = b.id) AS pc \
         FROM boards b ORDER BY b.short_name",
    ) else {
        return vec![];
    };
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

// ─── Site statistics ──────────────────────────────────────────────────────────

/// Gather aggregate site-wide statistics for the home page.
///
/// FIX[HIGH-2]: Previously issued five separate full-table scans (one COUNT(*)
/// overall, three filtered COUNTs by `media_type`, one SUM). All five are now
/// computed in a single aggregate pass over the posts table.
///
/// FIX[HIGH-3]: `active_bytes` now sums both `file_size` and `audio_file_size` so
/// image+audio combo posts are fully accounted for. The previous query only
/// summed `file_size` and silently under-reported disk usage.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_site_stats(conn: &rusqlite::Connection) -> Result<crate::models::SiteStats> {
    conn.query_row(
        "SELECT
             COUNT(*)                                                           AS total_posts,
             SUM(CASE WHEN media_type = 'image' THEN 1 ELSE 0 END)             AS total_images,
             SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END)             AS total_videos,
             SUM(CASE WHEN media_type = 'audio' THEN 1 ELSE 0 END)             AS total_audio,
             COALESCE(
                 SUM(CASE WHEN file_path IS NOT NULL AND file_size IS NOT NULL
                          THEN file_size ELSE 0 END)
               + SUM(CASE WHEN audio_file_path IS NOT NULL AND audio_file_size IS NOT NULL
                          THEN audio_file_size ELSE 0 END),
             0)                                                                 AS active_bytes
         FROM posts",
        [],
        |r| {
            Ok(crate::models::SiteStats {
                total_posts: r.get(0)?,
                total_images: r.get(1)?,
                total_videos: r.get(2)?,
                total_audio: r.get(3)?,
                active_bytes: r.get(4)?,
            })
        },
    )
    .context("Failed to query site stats")
}
