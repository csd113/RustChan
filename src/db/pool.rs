use crate::config::CONFIG;
use anyhow::{Context as _, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;

use super::schema::install_or_migrate_schema;
use super::types::DbPool;

const CONNECTION_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA foreign_keys = ON;
    PRAGMA cache_size = -32000;
    PRAGMA temp_store = MEMORY;
    PRAGMA mmap_size = 67108864;
    PRAGMA busy_timeout = 1000;
";

const POOL_CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Initialise the `SQLite` connection pool and ensure the schema exists.
///
/// # Errors
/// Returns an error if the database directory cannot be created, the pool
/// cannot be built, or schema creation fails.
pub fn init_pool() -> Result<DbPool> {
    let db_path = &CONFIG.database_path;

    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path)
        .with_init(|conn| conn.execute_batch(CONNECTION_PRAGMAS));

    let pool_size = CONFIG.db_pool_size;
    let pool = Pool::builder()
        .max_size(pool_size)
        .connection_timeout(POOL_CONNECTION_TIMEOUT)
        .build(manager)
        .context("Failed to build database pool")?;

    let conn = pool.get().context("Failed to get DB connection")?;
    install_or_migrate_schema(&conn)?;
    super::upsert_builtin_themes(&conn)?;

    tracing::info!(target: "db", path = db_path, "Database initialised");
    Ok(pool)
}

#[cfg(test)]
/// Build an isolated in-memory `SQLite` pool with the full schema installed.
///
/// # Errors
/// Returns an error if the temporary pool cannot be created or initialised.
pub fn init_test_pool() -> Result<DbPool> {
    let test_db_dir = std::env::temp_dir().join("rustchan-test-dbs");
    std::fs::create_dir_all(&test_db_dir).context("Failed to create test DB directory")?;
    let test_db_path = test_db_dir.join(format!("{}.sqlite3", uuid::Uuid::new_v4().simple()));
    let manager = SqliteConnectionManager::file(test_db_path)
        .with_init(|conn| conn.execute_batch(CONNECTION_PRAGMAS));

    let pool = Pool::builder()
        .max_size(4)
        .connection_timeout(POOL_CONNECTION_TIMEOUT)
        .build(manager)
        .context("Failed to build test database pool")?;

    let conn = pool.get().context("Failed to get test DB connection")?;
    install_or_migrate_schema(&conn)?;
    super::upsert_builtin_themes(&conn)?;
    Ok(pool)
}

/// Emit first-run operator guidance when the site has not been configured yet.
///
/// # Errors
/// Returns an error if the database cannot be queried for board or admin counts.
pub fn first_run_check(pool: &DbPool) -> anyhow::Result<()> {
    let conn = pool.get()?;
    let board_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
        .context("Failed to count boards during first-run check")?;
    let admin_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM admin_users", [], |r| r.get(0))
        .context("Failed to count admin users during first-run check")?;

    if board_count == 0 {
        tracing::info!(
            target: "startup",
            boards = 0,
            admins = admin_count,
            "No boards found — create boards via admin panel or: rustchan-cli admin create-board"
        );
    }
    Ok(())
}

#[must_use]
pub fn has_no_admin(pool: &DbPool) -> bool {
    pool.get()
        .ok()
        .and_then(|conn| {
            conn.query_row("SELECT COUNT(*) FROM admin_users", [], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
        })
        .is_some_and(|count| count == 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sqlite_busy_wait_is_bounded_for_overload_responses() {
        let pool = super::init_test_pool().expect("test pool");
        let conn = pool.get().expect("connection");
        let busy_timeout_ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy timeout");

        assert_eq!(busy_timeout_ms, 1_000);
        assert_eq!(
            super::POOL_CONNECTION_TIMEOUT,
            std::time::Duration::from_secs(1)
        );
    }
}
