use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

/// Site-setting key containing the initial setup completion timestamp.
pub const SETUP_COMPLETED_AT_KEY: &str = "setup_completed_at";
/// Site-setting key containing the most recent setup reopen timestamp.
pub const SETUP_REOPENED_AT_KEY: &str = "setup_reopened_at";
/// Site-setting key containing the administrator who reopened setup.
pub const SETUP_REOPENED_BY_KEY: &str = "setup_reopened_by";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Access state of the setup wizard.
pub enum SetupAccess {
    /// No administrator exists and setup has never completed.
    Fresh,
    /// An administrator explicitly reopened setup.
    Reopened,
    /// Setup is complete and unavailable.
    Initialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Database-derived state used to authorize setup access.
pub struct SetupState {
    /// Current setup access classification.
    pub access: SetupAccess,
    /// Number of configured administrator accounts.
    pub admin_count: i64,
    /// Number of configured boards.
    pub board_count: i64,
    /// Whether the setup completion marker exists.
    pub completed: bool,
    /// Whether setup is currently marked as reopened.
    pub reopened: bool,
}

impl SetupState {
    /// Return whether the setup wizard may currently be opened.
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self.access, SetupAccess::Fresh | SetupAccess::Reopened)
    }

    /// Return whether entering setup requires administrator authentication.
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

/// Read a boolean setup marker from site settings.
fn setup_flag(conn: &rusqlite::Connection, key: &str) -> Result<bool> {
    let value = super::get_site_setting(conn, key)?;
    Ok(value
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty()))
}

/// Count rows in one of the setup-state tables.
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
    conn.execute_batch("SAVEPOINT reopen_setup")
        .context("begin reopen setup savepoint")?;
    let result = (|| {
        let now = chrono::Utc::now().timestamp().to_string();
        super::set_site_setting(conn, SETUP_REOPENED_AT_KEY, &now)?;
        super::set_site_setting(conn, SETUP_REOPENED_BY_KEY, &admin_id.to_string())?;
        Ok(())
    })();
    finish_savepoint(
        conn,
        "RELEASE SAVEPOINT reopen_setup",
        "ROLLBACK TO SAVEPOINT reopen_setup; RELEASE SAVEPOINT reopen_setup",
        "release reopen setup savepoint",
        result,
    )
}

/// # Errors
/// Returns an error if the reopen marker cannot be cleared.
pub fn close_reopened_setup(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM site_settings WHERE key IN (?1, ?2)",
        params![SETUP_REOPENED_AT_KEY, SETUP_REOPENED_BY_KEY],
    )
    .context("clear setup reopen marker")
    .map(|_affected_rows| ())?;
    Ok(())
}

/// # Errors
/// Returns an error if the setup completion marker cannot be persisted.
pub fn mark_setup_complete(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("SAVEPOINT mark_setup_complete")
        .context("begin setup completion savepoint")?;
    let result = (|| {
        let now = chrono::Utc::now().timestamp().to_string();
        conn.execute(
            "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SETUP_COMPLETED_AT_KEY, now],
        )
        .context("write setup completion marker")
        .map(|_affected_rows| ())?;
        close_reopened_setup(conn)
    })();
    finish_savepoint(
        conn,
        "RELEASE SAVEPOINT mark_setup_complete",
        "ROLLBACK TO SAVEPOINT mark_setup_complete; RELEASE SAVEPOINT mark_setup_complete",
        "release setup completion savepoint",
        result,
    )
}

/// Release a successful savepoint or roll all of its writes back on error.
fn finish_savepoint<T>(
    conn: &rusqlite::Connection,
    release_sql: &'static str,
    rollback_sql: &'static str,
    release_context: &'static str,
    result: Result<T>,
) -> Result<T> {
    match result {
        Ok(value) => {
            if let Err(error) = conn.execute_batch(release_sql) {
                drop(conn.execute_batch(rollback_sql));
                return Err(error).context(release_context);
            }
            Ok(value)
        }
        Err(error) => {
            drop(conn.execute_batch(rollback_sql));
            Err(error)
        }
    }
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
