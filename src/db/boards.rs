use crate::models::{Board, BoardAccessMode, BoardBannerMode};
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use std::collections::{HashMap, HashSet};

/// Stable ordering for all boards across safe-for-work and adult groups.
const BOARD_ORDER_SQL: &str = "nsfw ASC, display_order ASC, id ASC";
/// Stable ordering within one board content-rating group.
const BOARD_GROUP_ORDER_SQL: &str = "display_order ASC, id ASC";
/// Shared projection used to decode a board without a table alias.
const BOARD_SELECT_COLUMNS: &str = "id, display_order, short_name, name, description, nsfw, \
    max_threads, max_archived_threads, bump_limit, allow_images, allow_video, allow_audio, \
    max_image_size, max_video_size, max_audio_size, max_pdf_size, allow_pdf, allow_any_files, allow_tripcodes, \
    edit_window_secs, allow_editing, allow_self_delete, allow_archive, \
    allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext, \
    post_cooldown_secs, default_theme, banner_mode, access_mode, access_password_hash, created_at";
/// Shared projection used to decode a board selected with alias `b`.
const BOARD_SELECT_COLUMNS_WITH_ALIAS: &str = "b.id, b.display_order, b.short_name, b.name, \
    b.description, b.nsfw, b.max_threads, b.max_archived_threads, b.bump_limit, \
    b.allow_images, b.allow_video, b.allow_audio, b.max_image_size, b.max_video_size, b.max_audio_size, \
    b.max_pdf_size, b.allow_pdf, b.allow_any_files, b.allow_tripcodes, b.edit_window_secs, b.allow_editing, \
    b.allow_self_delete, b.allow_archive, b.allow_video_embeds, b.allow_captcha, \
    b.show_poster_ids, b.collapse_greentext, b.post_cooldown_secs, \
    b.default_theme, b.banner_mode, b.access_mode, b.access_password_hash, b.created_at";

/// Decode a board from the shared board-column projection.
pub(super) fn map_board(row: &rusqlite::Row<'_>) -> rusqlite::Result<Board> {
    let short_name: String = row.get(2)?;
    let banner_mode_raw: String = row.get(29)?;
    let access_mode_raw: String = row.get(30)?;
    let banner_mode = BoardBannerMode::from_db_str(&banner_mode_raw).unwrap_or_else(|| {
        tracing::warn!(
            target: "db",
            board = %short_name,
            banner_mode = %banner_mode_raw,
            "Invalid boards.banner_mode value; falling back to inherit"
        );
        BoardBannerMode::Inherit
    });
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
        max_image_size: row.get(12)?,
        max_video_size: row.get(13)?,
        max_audio_size: row.get(14)?,
        max_pdf_size: row.get(15)?,
        allow_pdf: row.get::<_, i32>(16)? != 0,
        allow_any_files: row.get::<_, i32>(17)? != 0,
        allow_tripcodes: row.get::<_, i32>(18)? != 0,
        edit_window_secs: row.get(19)?,
        allow_editing: row.get::<_, i32>(20)? != 0,
        allow_self_delete: row.get::<_, i32>(21)? != 0,
        allow_archive: row.get::<_, i32>(22)? != 0,
        allow_video_embeds: row.get::<_, i32>(23)? != 0,
        allow_captcha: row.get::<_, i32>(24)? != 0,
        show_poster_ids: row.get::<_, i32>(25)? != 0,
        collapse_greentext: row.get::<_, i32>(26)? != 0,
        post_cooldown_secs: row.get(27)?,
        default_theme: row.get(28)?,
        banner_mode,
        access_mode,
        access_password_hash: row.get(31)?,
        created_at: row.get(32)?,
    })
}

/// Compute the next display order within a content-rating group.
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

/// Rewrite one content-rating group's display order into a contiguous sequence.
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

// Site settings
/// Read a site-wide setting by key. Returns `None` if the key has never been set.
///
/// The statement is cached because convenience helpers call this on every page render.
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

/// Site-setting key controlling automatic media pruning.
pub const MEDIA_AUTO_PRUNE_ENABLED_KEY: &str = "media_auto_prune_enabled";
/// Site-setting key containing the active-media byte limit.
pub const MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES_KEY: &str = "media_max_active_content_size_bytes";

/// Return whether automatic media pruning is enabled.
#[must_use]
pub fn get_media_auto_prune_enabled(conn: &rusqlite::Connection) -> bool {
    parse_site_bool(
        get_site_setting(conn, MEDIA_AUTO_PRUNE_ENABLED_KEY)
            .ok()
            .flatten(),
    )
    .unwrap_or(crate::config::CONFIG.initial_media_auto_prune_enabled)
}

/// Return the configured maximum active-media size in bytes.
#[must_use]
pub fn get_media_max_active_content_size_bytes(conn: &rusqlite::Connection) -> u64 {
    get_site_setting(conn, MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(crate::config::CONFIG.initial_media_max_active_content_size_bytes)
}

/// # Errors
/// Returns an error if the database write fails.
pub fn set_media_prune_settings(
    conn: &rusqlite::Connection,
    enabled: bool,
    max_size_bytes: u64,
) -> Result<()> {
    set_site_setting(conn, MEDIA_AUTO_PRUNE_ENABLED_KEY, &enabled.to_string())?;
    set_site_setting(
        conn,
        MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES_KEY,
        &max_size_bytes.to_string(),
    )
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

/// Return the configured site subtitle or its default.
#[must_use]
pub fn get_site_subtitle(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "site_subtitle")
        .unwrap_or_else(|error| {
            tracing::warn!(target: "db", %error, "Failed to read site_subtitle setting");
            None
        })
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "select board to proceed".to_owned())
}

/// Convenience: read the admin-configured default UI theme.
pub fn get_default_user_theme(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "default_theme")
        .unwrap_or_else(|error| {
            tracing::warn!(target: "db", %error, "Failed to read default_theme setting");
            None
        })
        .unwrap_or_default()
}

/// Parse the accepted textual site-setting boolean forms.
fn parse_site_bool(value: Option<String>) -> Option<bool> {
    let value = value?;
    let trimmed = value.trim();
    match trimmed {
        "1" | "true" | "TRUE" | "True" => Some(true),
        "0" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// Read a boolean setting, falling back through its legacy key and a default.
fn get_site_bool_with_legacy_fallback(
    conn: &rusqlite::Connection,
    key: &str,
    legacy_key: &str,
    default: bool,
) -> bool {
    parse_site_bool(get_site_setting(conn, key).unwrap_or_else(|error| {
        tracing::warn!(target: "db", %error, setting = key, "Failed to read site setting");
        None
    }))
    .or_else(|| {
        parse_site_bool(get_site_setting(conn, legacy_key).unwrap_or_else(|error| {
            tracing::warn!(
                target: "db",
                %error,
                setting = legacy_key,
                "Failed to read legacy site setting"
            );
            None
        }))
    })
    .unwrap_or(default)
}

/// Return whether new-thread badges are enabled on the homepage.
#[must_use]
pub fn get_homepage_new_thread_badges_enabled(conn: &rusqlite::Connection) -> bool {
    get_site_bool_with_legacy_fallback(
        conn,
        "homepage_new_thread_badges_enabled",
        "new_activity_notifications_enabled",
        crate::config::CONFIG.initial_homepage_new_thread_badges_enabled,
    )
}

/// Return whether new-reply badges are enabled on the homepage.
#[must_use]
pub fn get_homepage_new_reply_badges_enabled(conn: &rusqlite::Connection) -> bool {
    get_site_bool_with_legacy_fallback(
        conn,
        "homepage_new_reply_badges_enabled",
        "new_activity_notifications_enabled",
        crate::config::CONFIG.initial_homepage_new_reply_badges_enabled,
    )
}

/// Return whether new-reply badges are enabled on thread pages.
#[must_use]
pub fn get_thread_new_reply_badges_enabled(conn: &rusqlite::Connection) -> bool {
    get_site_bool_with_legacy_fallback(
        conn,
        "thread_new_reply_badges_enabled",
        "new_activity_notifications_enabled",
        crate::config::CONFIG.initial_thread_new_reply_badges_enabled,
    )
}

// Board queries
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

/// Return every board with its live thread count in one joined query.
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
            let thread_count: i64 = row.get(33)?;
            Ok(crate::models::BoardStats {
                board,
                thread_count,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Last-seen marker used to count newly created threads for one board.
pub struct BoardActivityCountInput {
    /// Board whose threads should be counted.
    pub board_id: i64,
    /// Creation timestamp of the newest previously seen thread.
    pub seen_thread_created_at: i64,
    /// Identifier used to break equal-timestamp ties.
    pub seen_thread_id: i64,
}

/// Count newly created, currently visible threads for each board marker.
///
/// Exact semantics: count non-archived threads whose `(created_at, id)` tuple is
/// strictly newer than the per-board marker stored in the browser cookie.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn count_new_threads_for_boards(
    conn: &rusqlite::Connection,
    markers: &[BoardActivityCountInput],
) -> Result<HashMap<i64, i64>> {
    if markers.is_empty() {
        return Ok(HashMap::new());
    }

    let values_sql = markers
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let base = index * 3;
            format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH seen(board_id, seen_thread_created_at, seen_thread_id) AS (
             VALUES {values_sql}
         )
         SELECT t.board_id, COUNT(*)
         FROM threads t
         JOIN seen s ON s.board_id = t.board_id
         WHERE t.archived = 0
           AND (
               t.created_at > s.seen_thread_created_at
               OR (
                   t.created_at = s.seen_thread_created_at
                   AND t.id > s.seen_thread_id
               )
           )
         GROUP BY t.board_id"
    );

    let mut params = Vec::with_capacity(markers.len() * 3);
    for marker in markers {
        params.push(rusqlite::types::Value::Integer(marker.board_id));
        params.push(rusqlite::types::Value::Integer(
            marker.seen_thread_created_at.max(0),
        ));
        params.push(rusqlite::types::Value::Integer(
            marker.seen_thread_id.max(0),
        ));
    }

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut counts = HashMap::new();
    for row in rows {
        let (board_id, count) = row?;
        counts.insert(board_id, count);
    }
    Ok(counts)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Last-seen marker used to count new replies for one thread.
pub struct BoardReplyActivityCountInput {
    /// Thread whose replies should be counted.
    pub thread_id: i64,
    /// Reply count previously observed by the user.
    pub seen_reply_count: i64,
}

/// Count new replies on visible, existing threads and aggregate them by board.
///
/// Exact semantics: for each retained per-thread marker, count
/// `thread.reply_count - seen_reply_count` when positive, excluding archived
/// threads.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn count_new_replies_for_boards(
    conn: &rusqlite::Connection,
    markers: &[BoardReplyActivityCountInput],
) -> Result<HashMap<i64, i64>> {
    if markers.is_empty() {
        return Ok(HashMap::new());
    }

    let values_sql = markers
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let base = index * 2;
            format!("(?{}, ?{})", base + 1, base + 2)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH seen(thread_id, seen_reply_count) AS (
             VALUES {values_sql}
         )
         SELECT t.board_id, SUM(MAX(t.reply_count - s.seen_reply_count, 0))
         FROM threads t
         JOIN seen s ON s.thread_id = t.id
         WHERE t.archived = 0
         GROUP BY t.board_id"
    );

    let mut params = Vec::with_capacity(markers.len() * 2);
    for marker in markers {
        params.push(rusqlite::types::Value::Integer(marker.thread_id));
        params.push(rusqlite::types::Value::Integer(
            marker.seen_reply_count.max(0),
        ));
    }

    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;

    let mut counts = HashMap::new();
    for row in rows {
        let (board_id, count) = row?;
        if count > 0 {
            counts.insert(board_id, count);
        }
    }
    Ok(counts)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn get_board_by_short(conn: &rusqlite::Connection, short: &str) -> Result<Option<Board>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BOARD_SELECT_COLUMNS} FROM boards WHERE short_name = ?1"
    ))?;
    Ok(stmt.query_row(params![short], map_board).optional()?)
}

/// Create a test board and return its database identifier.
///
/// # Errors
/// Returns an error if the database operation fails.
#[cfg(test)]
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
            "INSERT INTO boards (
                 display_order, short_name, name, description, nsfw,
                 allow_images, allow_video, allow_audio,
                 max_image_size, max_video_size, max_audio_size, max_pdf_size
             )
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, 0, ?6, ?7, ?8, ?9)
             RETURNING id",
            params![
                display_order,
                short,
                name,
                description,
                i32::from(nsfw),
                i64::try_from(crate::config::CONFIG.max_image_size)
                    .context("max_image_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_video_size)
                    .context("max_video_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_audio_size)
                    .context("max_audio_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_image_size)
                    .context("max_pdf_size does not fit in i64")?,
            ],
            |r| r.get(0),
        )
        .context("Failed to create board")?;
    Ok(id)
}

/// Create a board with explicit per-media-type toggles.
/// Used by the CLI and console board bootstrap paths.
///
/// # Errors
/// Returns an error if the database operation fails.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the signature mirrors independent media toggles stored on a board"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the signature mirrors data passed between the existing route and CLI layers"
)]
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
            "INSERT INTO boards (
                 display_order, short_name, name, description, nsfw,
                 allow_images, allow_video, allow_audio,
                 max_image_size, max_video_size, max_audio_size, max_pdf_size,
                 allow_tripcodes, allow_editing, allow_self_delete, allow_archive,
                 allow_video_embeds, allow_captcha, show_poster_ids,
                 collapse_greentext, post_cooldown_secs, default_theme,
                 banner_mode, access_mode, access_password_hash
             )
             VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                 1, 1, 1, 1,
                 1, 0, 1,
                 0, 0, '', 'inherit', 'public', ''
             )
             RETURNING id",
            params![
                display_order,
                short,
                name,
                description,
                i32::from(nsfw),
                i32::from(allow_images),
                i32::from(allow_video),
                i32::from(allow_audio),
                i64::try_from(crate::config::CONFIG.max_image_size)
                    .context("max_image_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_video_size)
                    .context("max_video_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_audio_size)
                    .context("max_audio_size does not fit in i64")?,
                i64::try_from(crate::config::CONFIG.max_image_size)
                    .context("max_pdf_size does not fit in i64")?,
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

/// Update all per-board settings from the admin panel.
///
/// # Errors
/// Returns an error if the database operation fails or the board id is not found.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the signature mirrors independent boolean fields in the admin edit form"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the signature mirrors the existing admin form and persisted board columns"
)]
#[expect(
    clippy::too_many_lines,
    reason = "keeping the SQL branches together makes the reorder transaction auditable"
)]
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
    max_image_size: i64,
    max_video_size: i64,
    max_audio_size: i64,
    max_pdf_size: i64,
    allow_pdf: bool,
    allow_any_files: bool,
    allow_tripcodes: bool,
    edit_window_secs: i64,
    allow_editing: bool,
    allow_self_delete: bool,
    allow_archive: bool,
    allow_video_embeds: bool,
    allow_captcha: bool,
    show_poster_ids: bool,
    collapse_greentext: bool,
    post_cooldown_secs: i64,
    default_theme: &str,
    banner_mode: BoardBannerMode,
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
             allow_images=?7, allow_video=?8, allow_audio=?9,
             max_image_size=?10, max_video_size=?11, max_audio_size=?12, max_pdf_size=?13,
             allow_pdf=?14, allow_any_files=?15, allow_tripcodes=?16, edit_window_secs=?17,
             allow_editing=?18, allow_self_delete=?19, allow_archive=?20, allow_video_embeds=?21,
             allow_captcha=?22, show_poster_ids=?23, collapse_greentext=?24, post_cooldown_secs=?25,
             default_theme=?26, banner_mode=?27, access_mode=?28, access_password_hash=?29
             WHERE id=?30",
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
                max_image_size,
                max_video_size,
                max_audio_size,
                max_pdf_size,
                i32::from(allow_pdf),
                i32::from(allow_any_files),
                i32::from(allow_tripcodes),
                edit_window_secs,
                i32::from(allow_editing),
                i32::from(allow_self_delete),
                i32::from(allow_archive),
                i32::from(allow_video_embeds),
                i32::from(allow_captcha),
                i32::from(show_poster_ids),
                i32::from(collapse_greentext),
                post_cooldown_secs,
                default_theme,
                banner_mode.as_str(),
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
             allow_images=?8, allow_video=?9, allow_audio=?10,
             max_image_size=?11, max_video_size=?12, max_audio_size=?13, max_pdf_size=?14,
             allow_pdf=?15, allow_any_files=?16, allow_tripcodes=?17, edit_window_secs=?18,
             allow_editing=?19, allow_self_delete=?20, allow_archive=?21, allow_video_embeds=?22,
             allow_captcha=?23, show_poster_ids=?24, collapse_greentext=?25, post_cooldown_secs=?26,
             default_theme=?27, banner_mode=?28, access_mode=?29, access_password_hash=?30
             WHERE id=?31",
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
                max_image_size,
                max_video_size,
                max_audio_size,
                max_pdf_size,
                i32::from(allow_pdf),
                i32::from(allow_any_files),
                i32::from(allow_tripcodes),
                edit_window_secs,
                i32::from(allow_editing),
                i32::from(allow_self_delete),
                i32::from(allow_archive),
                i32::from(allow_video_embeds),
                i32::from(allow_captcha),
                i32::from(show_poster_ids),
                i32::from(collapse_greentext),
                post_cooldown_secs,
                default_theme,
                banner_mode.as_str(),
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
/// The statement is cached because this runs for every submission on boards
/// with a posting cooldown.
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
/// Path collection and the cascading delete share a transaction so concurrent
/// inserts cannot make a collected path live again before deletion. Paths are
/// selected directly by `posts.board_id`, including any orphaned posts.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<super::DeletePathsResult> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin delete_board transaction")?;

    let result: Result<super::DeletePathsResult> = (|| {
        let board_short: String = conn
            .query_row(
                "SELECT short_name FROM boards WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .context("Failed to load board before delete")?;

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
        let pending_fs_op = super::build_delete_files_and_dirs_pending_op(
            &safe,
            std::slice::from_ref(&board_short),
        )?;
        if let Some(op) = pending_fs_op.as_ref() {
            super::insert_pending_fs_op(conn, op)?;
        }
        Ok(super::DeletePathsResult {
            paths: safe,
            pending_fs_op_id: pending_fs_op.map(|op| op.id),
        })
    })();

    match result {
        Ok(safe) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit delete_board transaction")?;
            Ok(safe)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

// Per-board stats (terminal display)
/// Per-board thread and post counts for the terminal stats display.
///
/// # Errors
/// Returns an error if any board statistic cannot be read.
pub fn get_per_board_stats(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<Vec<(String, i64, i64)>> {
    // Replace N+1 correlated subqueries (2 subqueries × boards)
    // with a single LEFT JOIN … GROUP BY pass. For a forum with 20 boards the
    // old query executed 41 SQL statements; this executes 1.
    let mut stmt = conn.prepare(
        "SELECT b.short_name, \
                COUNT(DISTINCT t.id) AS tc, \
                COUNT(DISTINCT p.id) AS pc \
         FROM boards b \
         LEFT JOIN threads t ON t.board_id = b.id \
         LEFT JOIN posts   p ON p.thread_id = t.id \
         GROUP BY b.id \
         ORDER BY b.short_name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    rows.collect()
}

// Site statistics
/// Gather aggregate site-wide statistics for the home page in one table scan.
/// `active_bytes` includes both primary and audio attachment sizes.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_site_stats(conn: &rusqlite::Connection) -> Result<crate::models::SiteStats> {
    let columns = post_table_columns(conn)?;
    let has_media_type = columns.contains("media_type");
    let has_audio_file_path = columns.contains("audio_file_path");
    let has_audio_file_size = columns.contains("audio_file_size");
    let has_mime_type = columns.contains("mime_type");
    let thread_columns = table_columns(conn, "threads")?;
    let has_thread_archive_flag = thread_columns.contains("archived");

    let total_images_expr = if has_media_type {
        "SUM(CASE WHEN media_type = 'image' THEN 1 ELSE 0 END)"
    } else {
        "0"
    };
    let total_videos_expr = if has_media_type {
        "SUM(CASE WHEN media_type = 'video' THEN 1 ELSE 0 END)"
    } else {
        "0"
    };

    let mut total_audio_checks = Vec::new();
    if has_media_type {
        total_audio_checks.push("media_type = 'audio'");
    }
    if has_audio_file_path {
        total_audio_checks.push("audio_file_path IS NOT NULL");
    }
    if has_mime_type {
        total_audio_checks.push("mime_type LIKE 'audio/%'");
    }
    let total_audio_expr = if total_audio_checks.is_empty() {
        "0".to_owned()
    } else {
        format!(
            "SUM(CASE WHEN {} THEN 1 ELSE 0 END)",
            total_audio_checks.join(" OR ")
        )
    };

    let active_content_join = if has_thread_archive_flag {
        " LEFT JOIN threads t ON t.id = posts.thread_id"
    } else {
        ""
    };
    let active_file_bytes_filter = if has_thread_archive_flag {
        " AND COALESCE(t.archived, 0) = 0"
    } else {
        ""
    };
    let active_audio_bytes_filter = if has_thread_archive_flag {
        " AND COALESCE(t.archived, 0) = 0"
    } else {
        ""
    };
    let active_audio_bytes_expr = if has_audio_file_path && has_audio_file_size {
        format!(
            "SUM(CASE WHEN audio_file_path IS NOT NULL AND audio_file_size IS NOT NULL{active_audio_bytes_filter}
                  THEN audio_file_size ELSE 0 END)"
        )
    } else {
        "0".to_owned()
    };

    let query = format!(
        "SELECT
             COUNT(*)                                                           AS total_posts,
             {total_images_expr}                                                AS total_images,
             {total_videos_expr}                                                AS total_videos,
             {total_audio_expr}                                                 AS total_audio,
             COALESCE(
                 SUM(CASE WHEN file_path IS NOT NULL AND file_size IS NOT NULL{active_file_bytes_filter}
                          THEN file_size ELSE 0 END),
                 0
             ) + COALESCE(
                 {active_audio_bytes_expr},
                 0
             )                                                                  AS active_bytes
         FROM posts{active_content_join}"
    );

    conn.query_row(&query, [], |r| {
        Ok(crate::models::SiteStats {
            total_posts: r.get(0)?,
            total_images: r.get(1)?,
            total_videos: r.get(2)?,
            total_audio: r.get(3)?,
            active_bytes: r.get(4)?,
        })
    })
    .context("Failed to query site stats")
}

/// Read the column names currently present on an `SQLite` table.
fn table_columns(conn: &rusqlite::Connection, table_name: &str) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .with_context(|| format!("Prepare {table_name} table info query"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()
        .with_context(|| format!("Read {table_name} table columns"))?;
    Ok(columns)
}

/// Read the column names currently present on the posts table.
fn post_table_columns(conn: &rusqlite::Connection) -> Result<HashSet<String>> {
    table_columns(conn, "posts")
}

#[cfg(test)]
mod tests {
    use super::{
        create_board, create_board_with_media_flags, delete_board, get_all_boards_with_stats,
        get_board_by_short, get_site_stats,
    };
    use anyhow::{Context as _, Result};
    use rusqlite::Connection;

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn board_stats_use_live_thread_count_instead_of_board_timestamp() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        conn.execute(
            "INSERT INTO boards (id, short_name, name, created_at)
             VALUES (1, 'test', 'Test', 1_700_000_000)",
            [],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived) VALUES
             (1, 1, 'visible one', 0),
             (2, 1, 'visible two', 0),
             (3, 1, 'archived', 1)",
            [],
        )?;

        let stats = get_all_boards_with_stats(&conn)?;
        let board_stats = stats
            .first()
            .context("board stats should include test board")?;

        assert_eq!(stats.len(), 1, "exactly one board should be returned");
        assert_eq!(
            board_stats.thread_count, 2,
            "only visible threads should be counted"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn site_stats_count_audio_primary_and_combo_uploads() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        conn.execute(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'test', 'Test')",
            [],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject) VALUES (1, 1, 'test thread')",
            [],
        )?;

        conn.execute(
            "INSERT INTO posts (
                 id, thread_id, board_id, body, body_html, deletion_token, is_op,
                 file_path, file_name, file_size, mime_type, media_type
             ) VALUES
             (1, 1, 1, 'audio post', '<p>audio</p>', 'tok1', 0,
              'test/track.mp3', 'track.mp3', 1234, 'audio/mpeg', 'audio'),
             (2, 1, 1, 'combo post', '<p>combo</p>', 'tok2', 0,
              'test/cover.png', 'cover.png', 4321, 'image/png', 'image')",
            [],
        )?;
        conn.execute(
            "UPDATE posts
             SET audio_file_path = 'test/track.flac',
                 audio_file_name = 'track.flac',
                 audio_file_size = 5678,
                 audio_mime_type = 'audio/flac'
             WHERE id = 2",
            [],
        )?;

        let stats = get_site_stats(&conn)?;
        assert_eq!(
            stats.total_audio, 2,
            "primary and companion audio should both be counted"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn site_stats_active_bytes_exclude_archived_thread_media() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        conn.execute(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'test', 'Test')",
            [],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived) VALUES
             (1, 1, 'live thread', 0),
             (2, 1, 'archived thread', 1)",
            [],
        )?;
        conn.execute(
            "INSERT INTO posts (
                 id, thread_id, board_id, body, body_html, deletion_token, is_op,
                 file_path, file_name, file_size
             ) VALUES
             (1, 1, 1, 'live', '<p>live</p>', 'tok1', 0, 'live.webp', 'live.webp', 100),
             (2, 2, 1, 'archived', '<p>archived</p>', 'tok2', 0, 'archived.webp', 'archived.webp', 900)",
            [],
        )?;

        let stats = get_site_stats(&conn)?;
        assert_eq!(
            stats.active_bytes, 100,
            "archived-thread media should not contribute active bytes"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn site_stats_fall_back_when_audio_columns_are_missing() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE posts (
                id INTEGER PRIMARY KEY,
                file_path TEXT,
                file_size INTEGER,
                mime_type TEXT
            );
            INSERT INTO posts (id, file_path, file_size, mime_type) VALUES
                (1, 'a.webp', 123, 'image/webp'),
                (2, 'b.mp3', 456, 'audio/mpeg');",
        )?;

        let stats = get_site_stats(&conn)?;
        assert_eq!(stats.total_posts, 2, "both legacy posts should be counted");
        assert_eq!(
            stats.total_images, 0,
            "legacy schema should use the conservative image fallback"
        );
        assert_eq!(
            stats.total_videos, 0,
            "legacy schema should use the conservative video fallback"
        );
        assert_eq!(
            stats.total_audio, 1,
            "primary audio should still be identified by MIME type"
        );
        assert_eq!(
            stats.active_bytes, 579,
            "all primary-media bytes should remain countable"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn create_board_with_media_flags_persists_audio_toggle() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        create_board_with_media_flags(
            &conn,
            "audio",
            "Audio",
            "Audio uploads",
            false,
            true,
            true,
            true,
        )?;

        let board = get_board_by_short(&conn, "audio")?.context("audio board should exist")?;
        assert!(board.allow_images, "image uploads should be enabled");
        assert!(board.allow_video, "video uploads should be enabled");
        assert!(board.allow_audio, "audio uploads should be enabled");
        assert!(!board.allow_pdf, "PDF uploads should retain their default");
        assert_eq!(
            board.max_pdf_size,
            i64::try_from(crate::config::CONFIG.max_image_size)?,
            "the default PDF limit should follow the image limit"
        );
        assert!(
            board.allow_video_embeds,
            "standard video-embed default should be enabled"
        );
        assert!(
            board.show_poster_ids,
            "standard poster-ID default should be enabled"
        );
        assert!(
            board.allow_editing,
            "standard editing default should be enabled"
        );
        assert!(
            board.allow_self_delete,
            "standard self-delete default should be enabled"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn create_board_uses_standardized_defaults() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        create_board(&conn, "fresh", "Fresh", "", false)?;

        let board = get_board_by_short(&conn, "fresh")?.context("fresh board should exist")?;
        assert_eq!(
            board.allow_audio,
            crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_AUDIO,
            "audio default should match the shared fixture"
        );
        assert_eq!(
            board.allow_video_embeds,
            crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS,
            "video-embed default should match the shared fixture"
        );
        assert_eq!(
            board.show_poster_ids,
            crate::test_fixtures::DEFAULT_NEW_BOARD_SHOW_POSTER_IDS,
            "poster-ID default should match the shared fixture"
        );
        assert_eq!(
            board.allow_editing,
            crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_EDITING,
            "editing default should match the shared fixture"
        );
        assert_eq!(
            board.allow_self_delete,
            crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE,
            "self-delete default should match the shared fixture"
        );
        assert_eq!(
            board.max_image_size,
            i64::try_from(crate::config::CONFIG.max_image_size)?,
            "image limit should match configuration"
        );
        assert_eq!(
            board.max_video_size,
            i64::try_from(crate::config::CONFIG.max_video_size)?,
            "video limit should match configuration"
        );
        assert_eq!(
            board.max_audio_size,
            i64::try_from(crate::config::CONFIG.max_audio_size)?,
            "audio limit should match configuration"
        );
        assert_eq!(
            board.max_pdf_size,
            i64::try_from(crate::config::CONFIG.max_image_size)?,
            "PDF limit should match the configured image limit"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn delete_board_records_durable_board_directory_cleanup() -> Result<()> {
        let _runtime_guard = crate::config::RUNTIME_LAYOUT_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let temp_dir = tempfile::tempdir()?;
        let upload_dir = temp_dir.path().join("uploads");
        let board_dir = upload_dir.join("gone");
        std::fs::create_dir_all(board_dir.join("thumbs"))?;
        std::fs::write(board_dir.join("file.webp"), b"file")?;
        std::fs::write(board_dir.join("thumbs/file.webp"), b"thumb")?;
        std::fs::write(board_dir.join("orphan.bin"), b"orphan")?;

        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let board_id = create_board(&conn, "gone", "Gone", "", false)?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject) VALUES (1, ?1, 'delete me')",
            rusqlite::params![board_id],
        )?;
        conn.execute(
            "INSERT INTO posts (
                 id, thread_id, board_id, body, body_html, deletion_token, is_op,
                 file_path, file_name, file_size, thumb_path
             ) VALUES
             (1, 1, ?1, 'body', '<p>body</p>', 'tok', 1,
              'gone/file.webp', 'file.webp', 4, 'gone/thumbs/file.webp')",
            rusqlite::params![board_id],
        )?;

        let deleted = delete_board(&conn, board_id)?;
        assert_eq!(
            deleted.paths.len(),
            2,
            "both referenced media paths should be returned"
        );
        assert!(
            deleted.pending_fs_op_id.is_some(),
            "board deletion should enqueue durable cleanup"
        );
        assert!(
            board_dir.exists(),
            "simulated crash window leaves directory for startup cleanup"
        );

        let pending = crate::db::list_pending_fs_ops(&conn)?;
        assert_eq!(pending.len(), 1, "one cleanup operation should be queued");
        let pending_op = pending.first().context("cleanup operation should exist")?;
        let payload: crate::pending_fs::DeleteFilesPayload =
            serde_json::from_str(&pending_op.payload_json)?;
        assert_eq!(
            payload.dirs,
            vec!["gone".to_owned()],
            "cleanup payload should include the board directory"
        );

        crate::pending_fs::reconcile_pending_fs_ops(
            &pool,
            upload_dir
                .to_str()
                .context("temporary path should be UTF-8")?,
        )?;
        assert!(
            !board_dir.exists(),
            "startup reconciliation should remove the board directory"
        );
        assert!(
            crate::db::list_pending_fs_ops(&conn)?.is_empty(),
            "completed cleanup should be removed from the durable queue"
        );
        Ok(())
    }
}
