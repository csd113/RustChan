use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

pub const SETUP_COMPLETED_AT_KEY: &str = "setup_completed_at";
pub const SETUP_REOPENED_AT_KEY: &str = "setup_reopened_at";
pub const SETUP_REOPENED_BY_KEY: &str = "setup_reopened_by";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAccess {
    Fresh,
    Reopened,
    Initialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupState {
    pub access: SetupAccess,
    pub admin_count: i64,
    pub board_count: i64,
    pub completed: bool,
    pub reopened: bool,
}

impl SetupState {
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self.access, SetupAccess::Fresh | SetupAccess::Reopened)
    }

    #[must_use]
    pub const fn requires_admin_auth(self) -> bool {
        matches!(self.access, SetupAccess::Reopened) || self.admin_count > 0
    }
}

/// # Errors
/// Returns an error if setup state cannot be loaded.
pub fn setup_state(conn: &rusqlite::Connection) -> Result<SetupState> {
    let completed = setup_flag(conn, SETUP_COMPLETED_AT_KEY)?;
    let reopened = setup_flag(conn, SETUP_REOPENED_AT_KEY)?;
    let admin_count = table_count(conn, "admin_users")?;
    let board_count = table_count(conn, "boards")?;
    let access = if reopened {
        SetupAccess::Reopened
    } else if completed || admin_count > 0 {
        SetupAccess::Initialized
    } else {
        SetupAccess::Fresh
    };
    Ok(SetupState {
        access,
        admin_count,
        board_count,
        completed,
        reopened,
    })
}

fn setup_flag(conn: &rusqlite::Connection, key: &str) -> Result<bool> {
    let value = super::get_site_setting(conn, key)?;
    Ok(value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty()))
}

fn table_count(conn: &rusqlite::Connection, table: &str) -> Result<i64> {
    let sql = match table {
        "admin_users" => "SELECT COUNT(*) FROM admin_users",
        "boards" => "SELECT COUNT(*) FROM boards",
        _ => anyhow::bail!("unsupported setup count table"),
    };
    conn.query_row(sql, [], |row| row.get(0))
        .context("count setup state table")
}

/// # Errors
/// Returns an error if setup is unavailable.
pub fn ensure_setup_available(conn: &rusqlite::Connection) -> Result<SetupState> {
    let state = setup_state(conn)?;
    if state.is_available() {
        Ok(state)
    } else {
        anyhow::bail!("setup wizard is not available on initialized instances")
    }
}

/// # Errors
/// Returns an error if the reopen marker cannot be persisted.
pub fn reopen_setup(conn: &rusqlite::Connection, admin_id: i64) -> Result<()> {
    let now = chrono::Utc::now().timestamp().to_string();
    super::set_site_setting(conn, SETUP_REOPENED_AT_KEY, &now)?;
    super::set_site_setting(conn, SETUP_REOPENED_BY_KEY, &admin_id.to_string())?;
    Ok(())
}

/// # Errors
/// Returns an error if the reopen marker cannot be cleared.
pub fn close_reopened_setup(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM site_settings WHERE key IN (?1, ?2)",
        params![SETUP_REOPENED_AT_KEY, SETUP_REOPENED_BY_KEY],
    )
    .context("clear setup reopen marker")?;
    Ok(())
}

/// # Errors
/// Returns an error if the setup completion marker cannot be persisted.
pub fn mark_setup_complete(conn: &rusqlite::Connection) -> Result<()> {
    let now = chrono::Utc::now().timestamp().to_string();
    conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SETUP_COMPLETED_AT_KEY, now],
    )
    .context("write setup completion marker")?;
    close_reopened_setup(conn)?;
    Ok(())
}

/// # Errors
/// Returns an error if the admin count query fails.
pub fn admin_count(conn: &rusqlite::Connection) -> Result<i64> {
    table_count(conn, "admin_users")
}

/// # Errors
/// Returns an error if the board lookup fails.
pub fn board_slug_exists(conn: &rusqlite::Connection, slug: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM boards WHERE short_name = ?1 LIMIT 1",
        params![slug],
        |_row| Ok(()),
    )
    .optional()
    .context("check board slug conflict")
    .map(|row| row.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_allows_setup() {
        let pool = crate::db::init_test_pool().expect("pool");
        let conn = pool.get().expect("conn");

        let state = setup_state(&conn).expect("state");

        assert_eq!(state.access, SetupAccess::Fresh);
        assert!(state.is_available());
        assert!(!state.requires_admin_auth());
    }

    #[test]
    fn admin_without_marker_blocks_setup_as_initialized() {
        let pool = crate::db::init_test_pool().expect("pool");
        let conn = pool.get().expect("conn");
        crate::db::create_admin(&conn, "admin", "hash").expect("admin");

        let state = setup_state(&conn).expect("state");

        assert_eq!(state.access, SetupAccess::Initialized);
        assert!(!state.is_available());
        assert!(state.requires_admin_auth());
    }

    #[test]
    fn admin_reopen_makes_setup_available_but_admin_authenticated() {
        let pool = crate::db::init_test_pool().expect("pool");
        let conn = pool.get().expect("conn");
        crate::db::create_admin(&conn, "admin", "hash").expect("admin");
        reopen_setup(&conn, 1).expect("reopen");

        let state = setup_state(&conn).expect("state");

        assert_eq!(state.access, SetupAccess::Reopened);
        assert!(state.is_available());
        assert!(state.requires_admin_auth());
    }

    #[test]
    fn completion_marker_blocks_setup_after_reopen_is_cleared() {
        let pool = crate::db::init_test_pool().expect("pool");
        let conn = pool.get().expect("conn");
        reopen_setup(&conn, 1).expect("reopen");
        mark_setup_complete(&conn).expect("complete");

        let state = setup_state(&conn).expect("state");

        assert_eq!(state.access, SetupAccess::Initialized);
        assert!(state.completed);
        assert!(!state.reopened);
    }

    #[test]
    fn close_reopened_setup_relocks_completed_instance_without_clearing_completion() {
        let pool = crate::db::init_test_pool().expect("pool");
        let conn = pool.get().expect("conn");
        mark_setup_complete(&conn).expect("complete");
        reopen_setup(&conn, 1).expect("reopen");

        close_reopened_setup(&conn).expect("close");
        let state = setup_state(&conn).expect("state");

        assert_eq!(state.access, SetupAccess::Initialized);
        assert!(state.completed);
        assert!(!state.reopened);
    }
}
