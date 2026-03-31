// src/db/pool.rs

use crate::config::CONFIG;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;

use super::schema::create_schema;
use super::types::DbPool;

pub fn init_pool() -> Result<DbPool> {
    let db_path = &CONFIG.database_path;

    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -32000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 67108864;
             PRAGMA busy_timeout = 10000;",
        )
    });

    let pool_size = CONFIG.db_pool_size;
    let pool = Pool::builder()
        .max_size(pool_size)
        .connection_timeout(std::time::Duration::from_secs(5))
        .build(manager)
        .context("Failed to build database pool")?;

    let conn = pool.get().context("Failed to get DB connection")?;
    create_schema(&conn)?;

    tracing::info!(target: "db", path = db_path, "Database initialised");
    Ok(pool)
}

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
