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

#[cfg(test)]
mod tests {
    use super::{get_thread_preference, set_thread_hidden, set_thread_pinned};
    use anyhow::{Context as _, Result};
    use std::sync::{Arc, Barrier};

    #[test]
    fn board_preference_collection_is_bounded_without_losing_individual_preferences() -> Result<()>
    {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let board = crate::db::create_board(&conn, "bound", "Bound", "", false)?;
        conn.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO threads(id,board_id) SELECT i,?1 FROM n", [board]).map(|_rows| ())?;
        conn.execute("INSERT INTO user_thread_preferences(user_hash,thread_id,pinned,hidden) SELECT 'viewer',id,1,0 FROM threads", []).map(|_rows| ())?;
        let exact = super::get_preferences_for_board(&conn, "viewer", board)?;
        anyhow::ensure!(exact.len() == 1000 && exact.contains_key(&1));
        conn.execute("INSERT INTO threads(id,board_id) VALUES(1001,?1)", [board])
            .map(|_rows| ())?;
        set_thread_pinned(&conn, "viewer", 1001, true)?;
        let over = super::get_preferences_for_board(&conn, "viewer", board)?;
        anyhow::ensure!(over.len() == 1000 && over.contains_key(&1001) && !over.contains_key(&1));
        anyhow::ensure!(get_thread_preference(&conn, "viewer", 1)?.is_some_and(|pref| pref.pinned));
        Ok(())
    }

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
                let _barrier_state = pinned_barrier.wait();
                set_thread_pinned(&conn, "viewer", thread_id, true)
            });

            let hidden_pool = pool.clone();
            let hidden_barrier = Arc::clone(&barrier);
            let hidden = scope.spawn(move || -> Result<()> {
                let conn = hidden_pool.get()?;
                let _barrier_state = hidden_barrier.wait();
                set_thread_hidden(&conn, "viewer", thread_id, true)
            });

            pinned.join().map_err(|panic_payload| {
                anyhow::anyhow!(
                    "pinned preference thread panicked: {}",
                    crate::media::process::panic_message(panic_payload.as_ref())
                )
            })??;
            hidden.join().map_err(|panic_payload| {
                anyhow::anyhow!(
                    "hidden preference thread panicked: {}",
                    crate::media::process::panic_message(panic_payload.as_ref())
                )
            })??;
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
