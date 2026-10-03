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

/// Require one initialized connection before startup; fill spare capacity asynchronously.
fn build_pool(manager: SqliteConnectionManager, pool_size: u32) -> Result<DbPool> {
    // r2d2's build() waits for max_size connections by default. That couples
    // first-run startup to every spare initializer's scheduling even when a
    // usable connection is ready. Keep eager refill and the one-second request
    // deadline, but validate readiness through a real checkout instead.
    let pool = Pool::builder()
        .max_size(pool_size)
        .event_handler(Box::new(super::diagnostics::PoolEvents::new()))
        .connection_timeout(POOL_CONNECTION_TIMEOUT)
        .build_unchecked(manager);
    drop(pool.get().context("Failed to initialize database pool")?);
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

#[cfg(test)]
mod tests {
    use anyhow::Result;

    /// Holds spare connection initializers until the test releases them.
    #[derive(Debug, Default)]
    struct InitializationGate {
        /// Whether spare initializers can finish.
        released: parking_lot::Mutex<bool>,
        /// Wakes blocked initializers when the test has observed startup.
        wake: parking_lot::Condvar,
    }

    /// Releases blocked initializers even when the startup regression fails.
    #[derive(Debug)]
    struct ReleaseInitializers(std::sync::Arc<InitializationGate>);

    impl Drop for ReleaseInitializers {
        fn drop(&mut self) {
            *self.0.released.lock() = true;
            let _waiting_threads = self.0.wake.notify_all();
        }
    }

    #[test]
    fn startup_uses_a_ready_connection_while_spare_initializers_are_blocked() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let gate = std::sync::Arc::new(InitializationGate::default());
        let release = ReleaseInitializers(std::sync::Arc::clone(&gate));
        let openings = std::sync::atomic::AtomicUsize::new(0);
        let manager =
            r2d2_sqlite::SqliteConnectionManager::file(directory.path().join("startup.db"))
                .with_init(move |conn| {
                    if openings.fetch_add(1, std::sync::atomic::Ordering::Relaxed) != 0 {
                        let mut released = gate.released.lock();
                        while !*released {
                            gate.wake.wait(&mut released);
                        }
                    }
                    conn.execute_batch(super::CONNECTION_PRAGMAS)
                });
        let result = super::build_pool(manager, 4);
        drop(release);
        let pool = result?;
        anyhow::ensure!(pool.max_size() == 4, "startup changed pool capacity");
        anyhow::ensure!(
            pool.connection_timeout() == super::POOL_CONNECTION_TIMEOUT,
            "startup changed request checkout timeout"
        );
        let conn = pool.get()?;
        super::install_or_migrate_schema(&conn)?;
        crate::db::verify_database_schema(&conn)?;
        Ok(())
    }

    #[test]
    fn startup_rejects_a_pool_with_no_usable_connection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let manager = r2d2_sqlite::SqliteConnectionManager::file(directory.path());
        anyhow::ensure!(
            super::build_pool(manager, 4).is_err(),
            "startup accepted an unusable database"
        );
        Ok(())
    }

    #[test]
    fn every_pool_connection_enforces_wal_full_and_foreign_keys() -> Result<()> {
        let pool = super::init_test_pool()?;
        let connections = (0_i32..4_i32)
            .map(|_| pool.get())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for conn in connections {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            let synchronous: i64 = conn.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
            let foreign_keys: bool = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
            anyhow::ensure!(
                mode == "wal" && synchronous == 2 && foreign_keys,
                "pooled connection weakened durability or relationship enforcement"
            );
        }
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn sqlite_busy_wait_is_bounded_for_overload_responses() -> Result<()> {
        let pool = super::init_test_pool()?;
        let conn = pool.get()?;
        let busy_timeout_ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;

        assert_eq!(
            busy_timeout_ms, 1_000,
            "SQLite busy timeout should match the configured one-second bound"
        );
        assert_eq!(
            super::POOL_CONNECTION_TIMEOUT,
            std::time::Duration::from_secs(1),
            "pool checkout timeout should remain one second"
        );
        Ok(())
    }

    #[test]
    fn exhausted_pool_returns_a_bounded_retryable_error() -> Result<()> {
        let pool = super::init_test_pool()?;
        let held = (0_i32..4_i32)
            .map(|_| pool.get())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let started = std::time::Instant::now();
        let error = pool
            .get()
            .err()
            .ok_or_else(|| anyhow::anyhow!("exhausted pool supplied a connection"))?;
        anyhow::ensure!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "pool timeout exceeded its bound"
        );
        anyhow::ensure!(
            matches!(
                crate::error::AppError::from(error),
                crate::error::AppError::DbBusy
            ),
            "pool exhaustion must be retryable"
        );
        drop(held);
        anyhow::ensure!(
            pool.get()?.is_autocommit(),
            "returned connection inherited a transaction"
        );
        Ok(())
    }

    #[test]
    fn wal_readers_remain_usable_and_writer_contention_is_bounded() -> Result<()> {
        let pool = super::init_test_pool()?;
        let writer = pool.get()?;
        writer.execute_batch("BEGIN IMMEDIATE")?;
        writer
            .execute(
                "INSERT INTO site_settings(key,value) VALUES('uncommitted','value')",
                [],
            )
            .map(|_affected_rows| ())?;
        let reader = pool.get()?;
        let visible: i64 = reader.query_row(
            "SELECT COUNT(*) FROM site_settings WHERE key='uncommitted'",
            [],
            |row| row.get(0),
        )?;
        anyhow::ensure!(visible == 0, "reader observed an uncommitted write");
        let started = std::time::Instant::now();
        let error = reader
            .execute(
                "INSERT INTO site_settings(key,value) VALUES('contended','value')",
                [],
            )
            .err()
            .ok_or_else(|| anyhow::anyhow!("competing write unexpectedly succeeded"))?;
        anyhow::ensure!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "busy wait exceeded its bound"
        );
        anyhow::ensure!(
            matches!(
                crate::error::AppError::from(error),
                crate::error::AppError::DbBusy
            ),
            "busy writer must be retryable"
        );
        writer.execute_batch("ROLLBACK")?;
        anyhow::ensure!(
            reader.is_autocommit(),
            "busy failure left a transaction active"
        );
        reader
            .execute(
                "INSERT INTO site_settings(key,value) VALUES('recovered','value')",
                [],
            )
            .map(|_affected_rows| ())?;
        Ok(())
    }

    #[test]
    fn active_read_snapshot_bounds_checkpoint_and_wal_backup_stays_complete() -> Result<()> {
        let pool = super::init_test_pool()?;
        let writer = pool.get()?;
        crate::db::create_board(&writer, "wal", "WAL", "", false).map(|_created_id| ())?;
        let reader = pool.get()?;
        reader.execute_batch("BEGIN; SELECT COUNT(*) FROM boards;")?;
        crate::db::create_board(&writer, "next", "Next", "", false).map(|_created_id| ())?;
        let (log, checkpointed, busy) = crate::db::run_wal_checkpoint(&writer)?;
        anyhow::ensure!(
            busy == 1 && checkpointed < log,
            "active reader must prevent a complete truncate checkpoint"
        );
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.sqlite3");
        writer
            .execute(
                "VACUUM INTO ?1",
                [path
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("backup path"))?],
            )
            .map(|_affected_rows| ())?;
        let snapshot = rusqlite::Connection::open(&path)?;
        let boards: i64 = snapshot.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
        anyhow::ensure!(boards == 2, "WAL snapshot missed a committed board");
        crate::db::verify_database_schema(&snapshot)?;
        reader.execute_batch("ROLLBACK")?;
        let (_, _, recovered_busy) = crate::db::run_wal_checkpoint(&writer)?;
        anyhow::ensure!(
            recovered_busy == 0,
            "checkpoint did not recover after reader release"
        );
        Ok(())
    }
}
