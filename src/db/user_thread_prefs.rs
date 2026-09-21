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
    )?;
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
    )?;
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
    )?;
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
                pinned: row.get::<_, i32>(0)? != 0,
                hidden: row.get::<_, i32>(1)? != 0,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Fetch all saved thread preferences for one user on one board.
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
           AND t.archived = 0",
    )?;

    let rows = stmt.query_map(params![user_hash, board_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            UserThreadPreference {
                pinned: row.get::<_, i32>(1)? != 0,
                hidden: row.get::<_, i32>(2)? != 0,
            },
        ))
    })?;

    let mut prefs = HashMap::new();
    for row in rows {
        let (thread_id, pref) = row?;
        prefs.insert(thread_id, pref);
    }
    Ok(prefs)
}

#[cfg(test)]
mod tests {
    use super::{get_thread_preference, set_thread_hidden, set_thread_pinned};
    use anyhow::{Context as _, Result};
    use std::sync::{Arc, Barrier};

    fn seeded_thread(pool: &crate::db::DbPool) -> Result<i64> {
        let conn = pool.get()?;
        let board_id = crate::db::create_board(&conn, "prefs", "Preferences", "", false)?;
        conn.query_row(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'preferences') RETURNING id",
            [board_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn independent_preference_updates_preserve_the_other_flag() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let thread_id = seeded_thread(&pool)?;
        let conn = pool.get()?;

        set_thread_pinned(&conn, "viewer", thread_id, true)?;
        set_thread_hidden(&conn, "viewer", thread_id, true)?;
        let preference = get_thread_preference(&conn, "viewer", thread_id)?
            .context("preference row should exist")?;

        assert!(
            preference.pinned,
            "hidden update must preserve pinned state"
        );
        assert!(preference.hidden, "hidden state should be persisted");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn concurrent_preference_updates_do_not_lose_flags() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let thread_id = seeded_thread(&pool)?;
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let pinned_pool = pool.clone();
            let pinned_barrier = Arc::clone(&barrier);
            let pinned = scope.spawn(move || -> Result<()> {
                let conn = pinned_pool.get()?;
                pinned_barrier.wait();
                set_thread_pinned(&conn, "viewer", thread_id, true)
            });

            let hidden_pool = pool.clone();
            let hidden_barrier = Arc::clone(&barrier);
            let hidden = scope.spawn(move || -> Result<()> {
                let conn = hidden_pool.get()?;
                hidden_barrier.wait();
                set_thread_hidden(&conn, "viewer", thread_id, true)
            });

            pinned
                .join()
                .map_err(|_| anyhow::anyhow!("pinned preference thread panicked"))??;
            hidden
                .join()
                .map_err(|_| anyhow::anyhow!("hidden preference thread panicked"))??;
            Result::<()>::Ok(())
        })?;

        let conn = pool.get()?;
        let preference = get_thread_preference(&conn, "viewer", thread_id)?
            .context("preference row should exist")?;
        assert!(preference.pinned, "concurrent pin should not be lost");
        assert!(preference.hidden, "concurrent hide should not be lost");
        Ok(())
    }
}
