//! Database health reports, integrity checks, and maintenance operations.

use anyhow::{Context as _, Result};

#[derive(Debug, Clone)]
/// Result of one database health check.
pub struct DbCheckResult {
    /// Whether the check passed.
    pub ok: bool,
    /// Human-readable check results.
    pub messages: Vec<String>,
}

impl DbCheckResult {
    /// Join the check messages into an operator-facing status string.
    #[must_use]
    pub fn output(&self) -> String {
        if self.messages.is_empty() {
            return "ok".to_owned();
        }
        self.messages.join(" | ")
    }
}

#[derive(Debug, Clone)]
/// Results of all database health checks at one point in time.
pub struct DbHealthSnapshot {
    /// Baseline-schema verification result.
    pub schema: DbCheckResult,
    /// `SQLite` integrity-check result.
    pub integrity: DbCheckResult,
    /// `SQLite` foreign-key-check result.
    pub foreign_keys: DbCheckResult,
}

impl DbHealthSnapshot {
    /// Return whether every health check passed.
    #[must_use]
    pub const fn ok(&self) -> bool {
        self.schema.ok && self.integrity.ok && self.foreign_keys.ok
    }
}

#[derive(Debug, Clone)]
/// Verified backup created before a database repair attempt.
pub struct DbRepairBackup {
    /// Stable backup identifier.
    pub backup_id: String,
    /// Backup format or scope label.
    pub backup_type: String,
    /// Filesystem path containing the backup.
    pub backup_path: String,
    /// Whether backup verification succeeded.
    pub verified: bool,
}

#[derive(Debug, Clone)]
/// Operator-facing report for a database health or repair run.
pub struct DbHealthReport {
    /// Health snapshot captured before repair.
    pub before: DbHealthSnapshot,
    /// Whether repair actions were attempted.
    pub repair_attempted: bool,
    /// Verified pre-repair backup, when available.
    pub repair_backup: Option<DbRepairBackup>,
    /// Backup failure that prevented repair, when applicable.
    pub repair_backup_error: Option<String>,
    /// High-level outcome messages.
    pub repair_summary: Vec<String>,
    /// Individual maintenance and repair actions.
    pub repair_steps: Vec<String>,
    /// Health snapshot captured after repair, when repair ran.
    pub after: Option<DbHealthSnapshot>,
}

// Database maintenance
/// Run PRAGMA `wal_checkpoint(TRUNCATE)` and return (`log_pages`, `checkpointed_pages`, busy).
///
/// The raw PRAGMA `wal_checkpoint` pragma returns three columns in this order:
///   col 0 — busy:         1 if a checkpoint could not complete due to an active reader/writer
///   col 1 — log:          total pages in the WAL file
///   col 2 — checkpointed: pages actually written back to the database.
///
/// This function returns `(log_pages, checkpointed_pages, busy)` — intentionally
/// reordered so the two informational values come first and the error flag last.
/// This is NOT the same order as the raw PRAGMA columns; do not destructure
/// based on PRAGMA documentation without consulting this signature.
///
/// TRUNCATE mode: after a complete checkpoint, the WAL file is truncated to
/// zero bytes, reclaiming disk space immediately.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn run_wal_checkpoint(conn: &rusqlite::Connection) -> Result<(i64, i64, i64)> {
    let (busy, log_pages, checkpointed) =
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
    Ok((log_pages, checkpointed, busy))
}

/// Return the current on-disk size of the database in bytes
/// (`page_count` × `page_size`, as reported by `SQLite`).
///
/// Note that this does NOT include the WAL file size. When the
/// database is in WAL mode, the total on-disk footprint is this value plus the
/// size of the .db-wal file. Call `run_wal_checkpoint` before `get_db_size_bytes`
/// if you need a reliable post-checkpoint size.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_db_size_bytes(conn: &rusqlite::Connection) -> Result<i64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(page_count.saturating_mul(page_size))
}

/// Run VACUUM on the database, rebuilding it into a minimal file.
///
/// VACUUM rewrites the entire database file, compacting free pages left by
/// bulk deletions. It cannot run inside a transaction. The call blocks until
/// the full rebuild is complete; for large databases this may take several
/// seconds. Always call `get_db_size_bytes` before and after to report the
/// space saving to the operator.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn run_vacuum(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// Run `SQLite`'s full integrity check and collect every diagnostic row.
fn integrity_check_status(conn: &rusqlite::Connection) -> DbCheckResult {
    let mut stmt = match conn.prepare("PRAGMA integrity_check") {
        Ok(stmt) => stmt,
        Err(error) => {
            return DbCheckResult {
                ok: false,
                messages: vec![format!("integrity_check failed: {error}")],
            };
        }
    };

    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(error) => {
            return DbCheckResult {
                ok: false,
                messages: vec![format!("integrity_check failed: {error}")],
            };
        }
    };

    let mut messages = Vec::new();
    for row in rows {
        match row {
            Ok(message) => messages.push(message),
            Err(error) => {
                messages.push(format!("integrity_check row failed: {error}"));
                break;
            }
        }
    }

    if messages.is_empty() {
        return DbCheckResult {
            ok: false,
            messages: vec!["integrity_check returned no rows".to_owned()],
        };
    }

    let ok = matches!(messages.as_slice(), [message] if message.eq_ignore_ascii_case("ok"));
    DbCheckResult { ok, messages }
}

/// Run `SQLite`'s foreign-key check and collect every violation.
fn foreign_key_check_status(conn: &rusqlite::Connection) -> DbCheckResult {
    let mut stmt = match conn.prepare("PRAGMA foreign_key_check") {
        Ok(stmt) => stmt,
        Err(error) => {
            return DbCheckResult {
                ok: false,
                messages: vec![format!("foreign_key_check failed: {error}")],
            };
        }
    };

    let rows = match stmt.query_map([], |row| {
        let table: String = row.get(0)?;
        let rowid: Option<i64> = row.get(1)?;
        let parent: String = row.get(2)?;
        let fkid: i64 = row.get(3)?;
        let rowid = rowid.map_or_else(|| "unknown".to_owned(), |value| value.to_string());
        Ok(format!(
            "table={table} rowid={rowid} parent={parent} fkid={fkid}"
        ))
    }) {
        Ok(rows) => rows,
        Err(error) => {
            return DbCheckResult {
                ok: false,
                messages: vec![format!("foreign_key_check failed: {error}")],
            };
        }
    };

    let mut messages = Vec::new();
    for row in rows {
        match row {
            Ok(message) => messages.push(message),
            Err(error) => {
                messages.push(format!("foreign_key_check row failed: {error}"));
                break;
            }
        }
    }

    if messages.is_empty() {
        return DbCheckResult {
            ok: true,
            messages: vec!["ok".to_owned()],
        };
    }

    DbCheckResult {
        ok: false,
        messages,
    }
}

/// Capture the current schema, integrity, and foreign-key health.
fn db_health_snapshot(conn: &rusqlite::Connection) -> DbHealthSnapshot {
    DbHealthSnapshot {
        schema: schema_check_status(conn),
        integrity: integrity_check_status(conn),
        foreign_keys: foreign_key_check_status(conn),
    }
}

/// Verify the `RustChan` schema baseline as a health-check result.
fn schema_check_status(conn: &rusqlite::Connection) -> DbCheckResult {
    match super::schema::verify_database_schema(conn) {
        Ok(()) => DbCheckResult {
            ok: true,
            messages: vec![format!(
                "{} baseline verified",
                super::schema::baseline_schema_version()
            )],
        },
        Err(error) => DbCheckResult {
            ok: false,
            messages: vec![format!(
                "{} baseline mismatch: {error}",
                super::schema::baseline_schema_version()
            )],
        },
    }
}

/// Recreate the full-text search table and its synchronization triggers.
fn rebuild_posts_fts(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        r"
        DROP TRIGGER IF EXISTS posts_ai;
        DROP TRIGGER IF EXISTS posts_ad;
        DROP TRIGGER IF EXISTS posts_au;
        DROP TABLE IF EXISTS posts_fts;
        ",
    )
    .and_then(|()| super::schema::create_posts_search_objects(conn, false))
    .and_then(|()| conn.execute_batch("INSERT INTO posts_fts(posts_fts) VALUES('rebuild');"))
    .context("Failed to recreate posts_fts search index")
}

/// Check database health without making repairs.
#[must_use]
pub fn check_db_health(conn: &rusqlite::Connection) -> DbHealthReport {
    let before = db_health_snapshot(conn);
    DbHealthReport {
        before,
        repair_attempted: false,
        repair_backup: None,
        repair_backup_error: None,
        repair_summary: Vec::new(),
        repair_steps: Vec::new(),
        after: None,
    }
}

/// Attempt safe database maintenance and return before-and-after health.
#[must_use]
pub fn attempt_db_repair(
    conn: &rusqlite::Connection,
    repair_backup: Option<DbRepairBackup>,
) -> DbHealthReport {
    let before = db_health_snapshot(conn);
    let mut repair_summary = Vec::new();
    let mut repair_steps = Vec::new();

    if let Some(backup) = &repair_backup {
        repair_summary.push(format!(
            "Created pre-repair {} backup: {}.",
            backup.backup_type, backup.backup_id
        ));
    }

    if before.ok() {
        repair_summary.push(
            "No database health problems were detected before the maintenance run.".to_owned(),
        );
        repair_summary.push(
            "No corruption-specific fixes were required; the system only ran maintenance and index rebuild steps.".to_owned(),
        );
    } else {
        repair_summary.push(
            "The initial database health check reported a problem, so repair steps were attempted."
                .to_owned(),
        );
    }

    match conn.execute_batch("REINDEX;") {
        Ok(()) => repair_steps.push("Rebuilt SQLite indexes.".to_owned()),
        Err(error) => repair_steps.push(format!("Could not rebuild SQLite indexes: {error}")),
    }

    match rebuild_posts_fts(conn) {
        Ok(()) => repair_steps
            .push("Rebuilt the post search index and recreated its update triggers.".to_owned()),
        Err(error) => repair_steps.push(format!(
            "Could not rebuild the post search index and triggers: {error}"
        )),
    }

    match conn.execute_batch("PRAGMA optimize;") {
        Ok(()) => repair_steps.push("Optimized SQLite query-planner statistics.".to_owned()),
        Err(error) => repair_steps.push(format!(
            "Could not optimize SQLite query-planner statistics: {error}"
        )),
    }

    let after = db_health_snapshot(conn);

    if before.ok() && after.ok() {
        repair_summary.push(
            "The final database health check still passed, confirming that no additional repairs were needed.".to_owned(),
        );
    } else if after.ok() {
        repair_summary.push(
            "The final database health check passed after the repair run, so the detected problem was cleared.".to_owned(),
        );
    } else {
        repair_summary.push(
            "The repair run finished, but the final database health check still reports a problem."
                .to_owned(),
        );
    }

    DbHealthReport {
        before,
        repair_attempted: true,
        repair_backup,
        repair_backup_error: None,
        repair_summary,
        repair_steps,
        after: Some(after),
    }
}

/// Build a report for a repair aborted because its safety backup failed.
#[must_use]
pub fn db_repair_aborted_for_backup_failure(
    conn: &rusqlite::Connection,
    backup_error: &str,
) -> DbHealthReport {
    DbHealthReport {
        before: db_health_snapshot(conn),
        repair_attempted: false,
        repair_backup: None,
        repair_backup_error: Some(backup_error.to_owned()),
        repair_summary: vec![
            format!("Pre-repair backup failed: {backup_error}"),
            "No repair or maintenance actions were run.".to_owned(),
        ],
        repair_steps: Vec::new(),
        after: None,
    }
}
