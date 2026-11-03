use anyhow::Result;
use rusqlite::{params, OptionalExtension as _};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default)]
/// Saved display preferences for one thread.
pub struct UserThreadPreference {
    /// Whether the thread is pinned for the user.
    pub pinned: bool,
    /// Whether the thread is hidden for the user.
    pub hidden: bool,
}

/// Remove a preference row when both values are at their defaults.
fn cleanup_if_default(conn: &rusqlite::Connection, user_hash: &str, thread_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM user_thread_preferences
         WHERE user_hash = ?1 AND thread_id = ?2 AND pinned = 0 AND hidden = 0",
        params![user_hash, thread_id],
    )
    .map(|_affected_rows| ())?;
    Ok(())
}

/// Mark a thread as hidden or visible for one anonymous user hash.
///
/// # Errors
/// Returns an error if the preference cannot be read or written in `SQLite`.
pub fn set_thread_hidden(
    conn: &rusqlite::Connection,
    user_hash: &str,
    thread_id: i64,
    hidden: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO user_thread_preferences (user_hash, thread_id, pinned, hidden, updated_at)
         VALUES (?1, ?2, 0, ?3, unixepoch())
         ON CONFLICT(user_hash, thread_id) DO UPDATE SET
            hidden = excluded.hidden,
            updated_at = unixepoch()",
        params![user_hash, thread_id, i32::from(hidden)],
    )
    .map(|_affected_rows| ())?;
    cleanup_if_default(conn, user_hash, thread_id)?;
    Ok(())
}

/// Mark a thread as pinned or unpinned for one anonymous user hash.
///
/// # Errors
/// Returns an error if the preference cannot be read or written in `SQLite`.
pub fn set_thread_pinned(
    conn: &rusqlite::Connection,
    user_hash: &str,
    thread_id: i64,
    pinned: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO user_thread_preferences (user_hash, thread_id, pinned, hidden, updated_at)
         VALUES (?1, ?2, ?3, 0, unixepoch())
         ON CONFLICT(user_hash, thread_id) DO UPDATE SET
            pinned = excluded.pinned,
            updated_at = unixepoch()",
        params![user_hash, thread_id, i32::from(pinned)],
    )
    .map(|_affected_rows| ())?;
    cleanup_if_default(conn, user_hash, thread_id)?;
    Ok(())
}

/// Fetch the saved thread preference for one user and thread pair.
///
/// # Errors
/// Returns an error if the preference lookup fails in `SQLite`.
pub fn get_thread_preference(
    conn: &rusqlite::Connection,
    user_hash: &str,
    thread_id: i64,
) -> Result<Option<UserThreadPreference>> {
    conn.query_row(
        "SELECT pinned, hidden
         FROM user_thread_preferences
         WHERE user_hash = ?1 AND thread_id = ?2",
        params![user_hash, thread_id],
        |row| {
            Ok(UserThreadPreference {
                pinned: row.get::<_, i32>(0)? != 0_i32,
                hidden: row.get::<_, i32>(1)? != 0_i32,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Fetch active preferences within the supported 1,000-thread board ceiling.
/// Oversized imported boards cannot force an unbounded metadata allocation.
///
/// # Errors
/// Returns an error if the board preference query fails in `SQLite`.
pub fn get_preferences_for_board(
    conn: &rusqlite::Connection,
    user_hash: &str,
    board_id: i64,
) -> Result<HashMap<i64, UserThreadPreference>> {
    let mut stmt = conn.prepare_cached(
        "SELECT utp.thread_id, utp.pinned, utp.hidden
         FROM user_thread_preferences utp
         JOIN threads t ON t.id = utp.thread_id
         WHERE utp.user_hash = ?1
           AND t.board_id = ?2
           AND t.archived = 0
         ORDER BY utp.thread_id DESC LIMIT 1000",
    )?;

    let rows = stmt.query_map(params![user_hash, board_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            UserThreadPreference {
                pinned: row.get::<_, i32>(1)? != 0_i32,
                hidden: row.get::<_, i32>(2)? != 0_i32,
            },
        ))
    })?;

    let mut prefs = HashMap::new();
    for row in rows {
        let (thread_id, pref) = row?;
        let _previous_value = prefs.insert(thread_id, pref);
    }
    Ok(prefs)
}
