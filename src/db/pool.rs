use crate::config::CONFIG;
use anyhow::{Context as _, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::{path::Path, time::Duration};

use super::schema::install_or_migrate_schema;
use super::types::DbPool;

/// Pragmas applied to every pooled `SQLite` connection.
pub(super) const CONNECTION_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = FULL;
    PRAGMA foreign_keys = ON;
    PRAGMA cache_size = -32000;
    PRAGMA temp_store = MEMORY;
    PRAGMA mmap_size = 67108864;
    PRAGMA busy_timeout = 1000;
";

/// Maximum time callers wait for a pooled connection.
const POOL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);

/// Bound first connection initialization separately from request checkout waits.
const POOL_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Require one initialized connection before startup; fill spare capacity asynchronously.
fn build_pool(manager: SqliteConnectionManager, pool_size: u32) -> Result<DbPool> {
    // r2d2's build() waits for max_size connections by default. That couples
    // first-run startup to every spare initializer's scheduling even when a
    // usable connection is ready. Keep eager refill and the one-second request
    // deadline, but validate readiness through a real checkout instead. The
    // first connection may need disk initialization and scheduler time under
    // concurrent startup load, so it gets a separate bounded startup deadline.
    let pool = Pool::builder()
        .max_size(pool_size)
        .event_handler(Box::new(super::diagnostics::PoolEvents::new()))
        .connection_timeout(POOL_CONNECTION_TIMEOUT)
        .build_unchecked(manager);
    drop(
        pool.get_timeout(POOL_STARTUP_TIMEOUT)
            .context("Failed to initialize database pool")?,
    );
    Ok(pool)
}

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
    let pool = build_pool(manager, pool_size)?;

    let conn = pool.get().context("Failed to get DB connection")?;
    install_or_migrate_schema(&conn)?;
    super::upsert_builtin_themes(&conn)?;

    tracing::info!(target: "db", path = db_path, pool_size,
        checkout_timeout_ms = %POOL_CONNECTION_TIMEOUT.as_millis(),
        "Database initialised");
    Ok(pool)
}

/// Emit first-run operator guidance when the site has not been configured yet.
///
/// # Errors
/// Returns an error if the database cannot be queried for board or admin counts.
pub fn first_run_check(pool: &DbPool) -> Result<()> {
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
            boards = 0_i32,
            admins = admin_count,
            "No boards found — create boards via admin panel or: rustchan-cli admin create-board"
        );
    }
    Ok(())
}

/// Return whether the database currently has no administrator accounts.
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
