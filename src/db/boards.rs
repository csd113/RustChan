// db/boards.rs — Board-level queries and site settings.
//
// Covers: site_settings table, boards CRUD, delete_board (with file-safety
// guard via super::paths_safe_to_delete), and aggregate site statistics.
//
// FIX summary (from audit):
//              COUNT loop with a single query using a correlated subquery
//              single aggregate query pass
//              INSERT … RETURNING id replaces execute + last_insert_rowid()

use crate::models::{Board, BoardAccessMode};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

const BOARD_ORDER_SQL: &str = "nsfw ASC, display_order ASC, id ASC";
const BOARD_GROUP_ORDER_SQL: &str = "display_order ASC, id ASC";
const BOARD_SELECT_COLUMNS: &str = "id, display_order, short_name, name, description, nsfw, \
    max_threads, max_archived_threads, bump_limit, allow_images, allow_video, allow_audio, \
    allow_any_files, allow_tripcodes, edit_window_secs, allow_editing, allow_archive, \
    allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext, \
    post_cooldown_secs, default_theme, access_mode, access_password_hash, created_at";
const BOARD_SELECT_COLUMNS_WITH_ALIAS: &str = "b.id, b.display_order, b.short_name, b.name, \
    b.description, b.nsfw, b.max_threads, b.max_archived_threads, b.bump_limit, \
    b.allow_images, b.allow_video, b.allow_audio, b.allow_any_files, b.allow_tripcodes, \
    b.edit_window_secs, b.allow_editing, b.allow_archive, b.allow_video_embeds, \
    b.allow_captcha, b.show_poster_ids, b.collapse_greentext, b.post_cooldown_secs, \
    b.default_theme, b.access_mode, b.access_password_hash, b.created_at";

// ─── Row mapper ───────────────────────────────────────────────────────────────

pub(super) fn map_board(row: &rusqlite::Row<'_>) -> rusqlite::Result<Board> {
    let short_name: String = row.get(2)?;
    let access_mode_raw: String = row.get(23)?;
    let access_mode = BoardAccessMode::from_db_str(&access_mode_raw).unwrap_or_else(|| {
        tracing::warn!(
            target: "db",
            board = %short_name,
            access_mode = %access_mode_raw,
            "Invalid boards.access_mode value; forcing fail-closed view_password mode"
        );
        BoardAccessMode::ViewPassword
    });
    Ok(Board {
        id: row.get(0)?,
        display_order: row.get(1)?,
        short_name,
        name: row.get(3)?,
        description: row.get(4)?,
        nsfw: row.get::<_, i32>(5)? != 0,
        max_threads: row.get(6)?,
        max_archived_threads: row.get(7)?,
        bump_limit: row.get(8)?,
        allow_images: row.get::<_, i32>(9)? != 0,
        allow_video: row.get::<_, i32>(10)? != 0,
        allow_audio: row.get::<_, i32>(11)? != 0,
        allow_any_files: row.get::<_, i32>(12)? != 0,
        allow_tripcodes: row.get::<_, i32>(13)? != 0,
        edit_window_secs: row.get(14)?,
        allow_editing: row.get::<_, i32>(15)? != 0,
        allow_archive: row.get::<_, i32>(16)? != 0,
        allow_video_embeds: row.get::<_, i32>(17)? != 0,
        allow_captcha: row.get::<_, i32>(18)? != 0,
        show_poster_ids: row.get::<_, i32>(19)? != 0,
        collapse_greentext: row.get::<_, i32>(20)? != 0,
        post_cooldown_secs: row.get(21)?,
        default_theme: row.get(22)?,
        access_mode,
        access_password_hash: row.get(24)?,
        created_at: row.get(25)?,
    })
}

fn next_board_display_order(
    conn: &rusqlite::Connection,
    nsfw: bool,
    exclude_id: Option<i64>,
) -> Result<i64> {
    let nsfw = i32::from(nsfw);
    let next = if let Some(exclude_id) = exclude_id {
        conn.query_row(
            "SELECT COALESCE(MAX(display_order) + 1, 1)
             FROM boards
             WHERE nsfw = ?1 AND id != ?2",
            params![nsfw, exclude_id],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT COALESCE(MAX(display_order) + 1, 1)
             FROM boards
             WHERE nsfw = ?1",
            params![nsfw],
            |row| row.get(0),
        )?
    };
    Ok(next)
}

fn normalize_board_group_order(
    conn: &rusqlite::Connection,
    nsfw: bool,
    exclude_id: Option<i64>,
) -> Result<()> {
    let mut stmt = if exclude_id.is_some() {
        conn.prepare_cached(&format!(
            "SELECT id FROM boards
             WHERE nsfw = ?1 AND id != ?2
             ORDER BY {BOARD_GROUP_ORDER_SQL}"
        ))?
    } else {
        conn.prepare_cached(&format!(
            "SELECT id FROM boards
             WHERE nsfw = ?1
             ORDER BY {BOARD_GROUP_ORDER_SQL}"
        ))?
    };
    let ordered_ids = if let Some(exclude_id) = exclude_id {
        stmt.query_map(params![i32::from(nsfw), exclude_id], |row| {
            row.get::<_, i64>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map(params![i32::from(nsfw)], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    drop(stmt);

    let mut update = conn.prepare_cached("UPDATE boards SET display_order = ?1 WHERE id = ?2")?;
    for (position, board_id) in ordered_ids.iter().enumerate() {
        let display_order =
            i64::try_from(position).context("board display_order index must fit in i64")? + 1;
        update.execute(params![display_order, board_id])?;
    }
    Ok(())
}

// ─── Site settings ────────────────────────────────────────────────────────────

/// Read a site-wide setting by key. Returns None if the key has never been set.
///
/// Switched to `prepare_cached` — convenience helpers (`get_site_name`,
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
        .unwrap_or_else(|error| {
            tracing::warn!(target: "db", %error, "Failed to read site_name setting");
            None
        })
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| crate::config::CONFIG.forum_name.clone())
}

pub fn get_site_subtitle(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "site_subtitle")
        .unwrap_or_else(|error| {
            tracing::warn!(target: "db", %error, "Failed to read site_subtitle setting");
            None
        })
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "select board to proceed".to_string())
}

/// Convenience: read the admin-configured default UI theme (empty = "terminal").
pub fn get_default_user_theme(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "default_theme")
        .unwrap_or_else(|error| {
            tracing::warn!(target: "db", %error, "Failed to read default_theme setting");
            None
        })
        .unwrap_or_default()
}

// ─── Board queries ────────────────────────────────────────────────────────────

/// # Errors
/// Returns an error if the database operation fails.
pub fn get_all_boards(conn: &rusqlite::Connection) -> Result<Vec<Board>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BOARD_SELECT_COLUMNS} FROM boards ORDER BY {BOARD_ORDER_SQL}"
    ))?;
    let boards = stmt
        .query_map([], map_board)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(boards)
}

/// Like `get_all_boards` but also returns live thread count for each board.
///
/// Previously issued one COUNT(*) query per board (N+1). Replaced
/// with a single LEFT JOIN query that computes all counts in one pass.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_all_boards_with_stats(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::BoardStats>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BOARD_SELECT_COLUMNS_WITH_ALIAS}, COUNT(t.id) AS thread_count
         FROM boards b
         LEFT JOIN threads t ON t.board_id = b.id AND t.archived = 0
         GROUP BY b.id
         ORDER BY b.nsfw ASC, b.display_order ASC, b.id ASC"
    ))?;
    let out = stmt
        .query_map([], |row| {
            let board = map_board(row)?;
            let thread_count: i64 = row.get(26)?;
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BOARD_SELECT_COLUMNS} FROM boards WHERE short_name = ?1"
    ))?;
    Ok(stmt.query_row(params![short], map_board).optional()?)
}

/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
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
    let display_order = next_board_display_order(conn, nsfw, None)?;
    let id: i64 = conn
        .query_row(
            "INSERT INTO boards (display_order, short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, 0)
             RETURNING id",
            params![display_order, short, name, description, i32::from(nsfw)],
            |r| r.get(0),
        )
        .context("Failed to create board")?;
    Ok(id)
}

/// Create a board with explicit per-media-type toggles.
/// Used by the CLI `--no-images / --no-videos / --no-audio` flags.
///
/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
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
    let display_order = next_board_display_order(conn, nsfw, None)?;
    let id: i64 = conn
        .query_row(
            "INSERT INTO boards (display_order, short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             RETURNING id",
            params![
                display_order, short, name, description, i32::from(nsfw),
                i32::from(allow_images), i32::from(allow_video), i32::from(allow_audio),
            ],
            |r| r.get(0),
        )
        .context("Failed to create board with media flags")?;
    Ok(id)
}

/// Move a board one slot up or down inside its current SFW/NSFW group.
///
/// # Errors
/// Returns an error if the database operation fails or the board id is not found.
pub fn move_board(conn: &mut rusqlite::Connection, id: i64, move_up: bool) -> Result<()> {
    let tx = conn.transaction()?;
    let board_nsfw: bool = tx.query_row(
        "SELECT nsfw FROM boards WHERE id = ?1",
        params![id],
        |row| row.get::<_, i32>(0).map(|value| value != 0),
    )?;
    let mut stmt = tx.prepare_cached(&format!(
        "SELECT id FROM boards WHERE nsfw = ?1 ORDER BY {BOARD_GROUP_ORDER_SQL}"
    ))?;
    let mut ordered_ids = stmt
        .query_map(params![i32::from(board_nsfw)], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    let index = ordered_ids
        .iter()
        .position(|board_id| *board_id == id)
        .ok_or_else(|| anyhow::anyhow!("Board id {id} not found"))?;

    let swap_with = if move_up {
        index.checked_sub(1)
    } else if index + 1 < ordered_ids.len() {
        Some(index + 1)
    } else {
        None
    };

    let Some(target_index) = swap_with else {
        tx.commit()?;
        return Ok(());
    };

    ordered_ids.swap(index, target_index);

    {
        let mut update = tx.prepare_cached("UPDATE boards SET display_order = ?1 WHERE id = ?2")?;
        for (position, board_id) in ordered_ids.iter().enumerate() {
            let display_order =
                i64::try_from(position).context("board display_order index must fit in i64")? + 1;
            update.execute(params![display_order, board_id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Added rows-affected check — silently succeeding when the board
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
/// Added rows-affected check.
///
/// # Errors
/// Returns an error if the database operation fails or the board id is not found.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn update_board_settings(
    conn: &mut rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
    bump_limit: i64,
    max_threads: i64,
    max_archived_threads: i64,
    allow_images: bool,
    allow_video: bool,
    allow_audio: bool,
    allow_any_files: bool,
    allow_tripcodes: bool,
    edit_window_secs: i64,
    allow_editing: bool,
    allow_archive: bool,
    allow_video_embeds: bool,
    allow_captcha: bool,
    show_poster_ids: bool,
    collapse_greentext: bool,
    post_cooldown_secs: i64,
    default_theme: &str,
    access_mode: BoardAccessMode,
    access_password_hash: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    let current_nsfw: bool = tx.query_row(
        "SELECT nsfw FROM boards WHERE id = ?1",
        params![id],
        |row| row.get::<_, i32>(0).map(|value| value != 0),
    )?;

    let affected = if current_nsfw == nsfw {
        tx.execute(
            "UPDATE boards SET name=?1, description=?2, nsfw=?3,
             bump_limit=?4, max_threads=?5, max_archived_threads=?6,
             allow_images=?7, allow_video=?8, allow_audio=?9, allow_any_files=?10,
            allow_tripcodes=?11, edit_window_secs=?12, allow_editing=?13,
             allow_archive=?14, allow_video_embeds=?15, allow_captcha=?16,
             show_poster_ids=?17, collapse_greentext=?18, post_cooldown_secs=?19,
             default_theme=?20, access_mode=?21, access_password_hash=?22
             WHERE id=?23",
            params![
                name,
                description,
                i32::from(nsfw),
                bump_limit,
                max_threads,
                max_archived_threads,
                i32::from(allow_images),
                i32::from(allow_video),
                i32::from(allow_audio),
                i32::from(allow_any_files),
                i32::from(allow_tripcodes),
                edit_window_secs,
                i32::from(allow_editing),
                i32::from(allow_archive),
                i32::from(allow_video_embeds),
                i32::from(allow_captcha),
                i32::from(show_poster_ids),
                i32::from(collapse_greentext),
                post_cooldown_secs,
                default_theme,
                access_mode.as_str(),
                access_password_hash,
                id,
            ],
        )
    } else {
        let next_display_order = next_board_display_order(&tx, nsfw, Some(id))?;
        tx.execute(
            "UPDATE boards SET name=?1, description=?2, nsfw=?3, display_order=?4,
             bump_limit=?5, max_threads=?6, max_archived_threads=?7,
             allow_images=?8, allow_video=?9, allow_audio=?10, allow_any_files=?11,
             allow_tripcodes=?12, edit_window_secs=?13, allow_editing=?14,
             allow_archive=?15, allow_video_embeds=?16, allow_captcha=?17,
             show_poster_ids=?18, collapse_greentext=?19, post_cooldown_secs=?20,
             default_theme=?21, access_mode=?22, access_password_hash=?23
             WHERE id=?24",
            params![
                name,
                description,
                i32::from(nsfw),
                next_display_order,
                bump_limit,
                max_threads,
                max_archived_threads,
                i32::from(allow_images),
                i32::from(allow_video),
                i32::from(allow_audio),
                i32::from(allow_any_files),
                i32::from(allow_tripcodes),
                edit_window_secs,
                i32::from(allow_editing),
                i32::from(allow_archive),
                i32::from(allow_video_embeds),
                i32::from(allow_captcha),
                i32::from(show_poster_ids),
                i32::from(collapse_greentext),
                post_cooldown_secs,
                default_theme,
                access_mode.as_str(),
                access_password_hash,
                id,
            ],
        )
    }
    .context("Failed to update board settings")?;
    if affected == 0 {
        anyhow::bail!("Board id {id} not found");
    }
    if current_nsfw != nsfw {
        normalize_board_group_order(&tx, current_nsfw, None)?;
        normalize_board_group_order(&tx, nsfw, None)?;
    }
    tx.commit()?;
    Ok(())
}

/// Returns how many seconds have elapsed since `ip_hash` last posted on `board_id`.
/// Returns None if they have never posted on this board.
///
/// Switched to `prepare_cached` — this is on the hot path (called
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
/// Wrapped the entire operation in a transaction. Previously,
/// file paths were collected before the CASCADE DELETE with no transaction
/// guard, so a concurrent insert could race between the SELECT and the DELETE.
///
/// Replaced the three-way join (posts → threads → boards) with a
/// direct query on `posts.board_id`. Posts already carry `board_id` so the threads
/// join was both unnecessary and could hide orphaned posts.
///
/// Added an affected-rows check so callers see an error when
/// trying to delete a board that doesn't exist.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_board transaction")?;

    let result: anyhow::Result<Vec<String>> = (|| {
        // Collect every file path that belongs to this board before the CASCADE.
        // The ON DELETE CASCADE on boards→threads→posts handles DB row removal, but
        // on-disk files must be cleaned up by the caller.
        let candidates = {
            let mut stmt = conn.prepare_cached(
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
        let n = conn
            .execute("DELETE FROM boards WHERE id = ?1", params![id])
            .context("Failed to delete board")?;
        if n == 0 {
            anyhow::bail!("Board id {id} not found");
        }

        // paths_safe_to_delete runs inside the transaction so it sees the
        // post-delete state: any file exclusively used by this board's posts now
        // has zero remaining references and is safe to remove.
        let safe = super::paths_safe_to_delete(conn, candidates)?;
        Ok(safe)
    })();

    match result {
        Ok(safe) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_board transaction")?;
            Ok(safe)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ─── Per-board stats (terminal display) ──────────────────────────────────────

/// Per-board thread and post counts for the terminal stats display.
pub fn get_per_board_stats(conn: &rusqlite::Connection) -> Vec<(String, i64, i64)> {
    // Replace N+1 correlated subqueries (2 subqueries × boards)
    // with a single LEFT JOIN … GROUP BY pass. For a forum with 20 boards the
    // old query executed 41 SQL statements; this executes 1.
    let Ok(mut stmt) = conn.prepare(
        "SELECT b.short_name, \
                COUNT(DISTINCT t.id) AS tc, \
                COUNT(DISTINCT p.id) AS pc \
         FROM boards b \
         LEFT JOIN threads t ON t.board_id = b.id \
         LEFT JOIN posts   p ON p.thread_id = t.id \
         GROUP BY b.id \
         ORDER BY b.short_name",
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
/// Previously issued five separate full-table scans (one COUNT(*)
/// overall, three filtered COUNTs by `media_type`, one SUM). All five are now
/// computed in a single aggregate pass over the posts table.
///
/// `active_bytes` now sums both `file_size` and `audio_file_size` so
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
