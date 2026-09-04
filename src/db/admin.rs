use crate::models::{AdminSession, AdminUser, Ban, WordFilter};
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Outcome of submitting a ban appeal.
pub enum BanAppealSubmission {
    /// A new appeal was recorded.
    Filed,
    /// A recent appeal already exists.
    AlreadyFiled,
    /// The submitting address is not currently banned.
    NotBanned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Outcome of submitting a post report.
pub enum ReportSubmission {
    /// A new report was recorded.
    Filed,
    /// The reporter already has an open report for the post.
    AlreadyFiled,
}

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

// Admin user queries
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_admin_by_username(
    conn: &rusqlite::Connection,
    username: &str,
) -> Result<Option<AdminUser>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, username, password_hash, created_at FROM admin_users WHERE username = ?1",
    )?;
    Ok(stmt
        .query_row(params![username], |r| {
            Ok(AdminUser {
                id: r.get(0)?,
                username: r.get(1)?,
                password_hash: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .optional()?)
}

/// Create an administrator and return the row id from the same statement.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn create_admin(conn: &rusqlite::Connection, username: &str, hash: &str) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO admin_users (username, password_hash) VALUES (?1, ?2) RETURNING id",
            params![username, hash],
            |r| r.get(0),
        )
        .context("Failed to create admin user")?;
    Ok(id)
}

/// Update an administrator password, failing if the username does not exist.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn update_admin_password(
    conn: &rusqlite::Connection,
    username: &str,
    hash: &str,
) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE admin_users SET password_hash = ?1 WHERE username = ?2",
            params![hash, username],
        )
        .context("Failed to update admin password")?;
    if n == 0 {
        anyhow::bail!("Admin user '{username}' not found");
    }
    Ok(())
}

/// List all administrator users for CLI tooling.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, username, created_at FROM admin_users ORDER BY id ASC")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String, i64)>>>()?;
    Ok(rows)
}

/// Retrieve admin username by `admin_id` (used when building log entries).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_admin_name_by_id(conn: &rusqlite::Connection, admin_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT username FROM admin_users WHERE id = ?1",
            params![admin_id],
            |r| r.get(0),
        )
        .optional()?)
}

// Session queries
/// # Errors
/// Returns an error if the database operation fails.
pub fn create_session(
    conn: &rusqlite::Connection,
    session_id: &str,
    admin_id: i64,
    expires_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO admin_sessions (id, admin_id, expires_at) VALUES (?1, ?2, ?3)",
        params![session_id, admin_id, expires_at],
    )
    .context("Failed to create admin session")?;
    Ok(())
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn get_session(conn: &rusqlite::Connection, session_id: &str) -> Result<Option<AdminSession>> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn.prepare_cached(
        "SELECT id, admin_id, created_at, expires_at FROM admin_sessions
         WHERE id = ?1 AND expires_at > ?2",
    )?;
    Ok(stmt
        .query_row(params![session_id, now], |r| {
            Ok(AdminSession {
                id: r.get(0)?,
                admin_id: r.get(1)?,
                created_at: r.get(2)?,
                expires_at: r.get(3)?,
            })
        })
        .optional()?)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn delete_session(conn: &rusqlite::Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM admin_sessions WHERE id = ?1",
        params![session_id],
    )?;
    Ok(())
}

/// Clean up expired sessions (called periodically).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn purge_expired_sessions(conn: &rusqlite::Connection) -> Result<usize> {
    let now = chrono::Utc::now().timestamp();
    let n = conn.execute(
        "DELETE FROM admin_sessions WHERE expires_at <= ?1",
        params![now],
    )?;
    Ok(n)
}

// Ban queries
/// Check whether `ip_hash` is currently banned. Returns the ban reason if so.
///
/// The statement is cached because this check runs on every post submission.
///
/// ORDER BY `expires_at` DESC NULLS FIRST ensures a permanent
/// ban (NULL `expires_at`) always surfaces before any timed ban.
///
/// Note: NULLS FIRST requires `SQLite` ≥ 3.30.0 (released 2019-10-04).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn is_banned(conn: &rusqlite::Connection, ip_hash: &str) -> Result<Option<String>> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn.prepare_cached(
        "SELECT reason FROM bans WHERE ip_hash = ?1
         AND (expires_at IS NULL OR expires_at > ?2)
         ORDER BY expires_at DESC NULLS FIRST
         LIMIT 1",
    )?;
    let result: Option<Option<String>> = stmt
        .query_row(params![ip_hash, now], |r| r.get(0))
        .optional()?;
    // A ban with no reason still maps to an empty reason string.
    Ok(result.map(Option::unwrap_or_default))
}

/// Add a ban and return its database identifier.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn add_ban(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    reason: &str,
    expires_at: Option<i64>,
) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO bans (ip_hash, reason, expires_at) VALUES (?1, ?2, ?3) RETURNING id",
            params![ip_hash, reason, expires_at],
            |r| r.get(0),
        )
        .context("Failed to insert ban")?;
    Ok(id)
}

/// Returns an error when the target ban row does not exist,
/// making double-removes and stale ban-ids visible rather than silently succeeding.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn remove_ban(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn
        .execute("DELETE FROM bans WHERE id = ?1", params![id])
        .context("Failed to remove ban")?;
    if n == 0 {
        anyhow::bail!("Ban id {id} not found");
    }
    Ok(())
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn list_bans(conn: &rusqlite::Connection) -> Result<Vec<Ban>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, ip_hash, reason, expires_at, created_at FROM bans ORDER BY created_at DESC",
    )?;
    let bans = stmt
        .query_map([], |r| {
            Ok(Ban {
                id: r.get(0)?,
                ip_hash: r.get(1)?,
                reason: r.get(2)?,
                expires_at: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(bans)
}

// Word filter queries
/// Return all word filters using a statement cached for the submission hot path.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_word_filters(conn: &rusqlite::Connection) -> Result<Vec<WordFilter>> {
    let mut stmt = conn.prepare_cached("SELECT id, pattern, replacement FROM word_filters")?;
    let filters = stmt
        .query_map([], |r| {
            Ok(WordFilter {
                id: r.get(0)?,
                pattern: r.get(1)?,
                replacement: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(filters)
}

/// Add a word filter and return its database identifier.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn add_word_filter(
    conn: &rusqlite::Connection,
    pattern: &str,
    replacement: &str,
) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO word_filters (pattern, replacement) VALUES (?1, ?2) RETURNING id",
            params![pattern, replacement],
            |r| r.get(0),
        )
        .context("Failed to insert word filter")?;
    Ok(id)
}

/// # Errors
/// Returns an error if the database operation fails.
pub fn remove_word_filter(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM word_filters WHERE id = ?1", params![id])?;
    Ok(())
}

// Reports
/// Return whether an `SQLite` error represents the open-report uniqueness guard.
fn is_open_report_unique_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(inner, message) => {
            inner.code == rusqlite::ErrorCode::ConstraintViolation
                && message.as_deref().is_some_and(|text| {
                    text.contains("idx_reports_open_unique")
                        || (text.contains("reports.post_id")
                            && text.contains("reports.reporter_hash"))
                })
        }
        _ => false,
    }
}

/// File a new report against a post.
///
/// Duplicate open reports from the same reporter are blocked by the
/// `idx_reports_open_unique` partial unique index.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn file_report(
    conn: &rusqlite::Connection,
    post_id: i64,
    reason: &str,
    reporter_hash: &str,
) -> Result<ReportSubmission> {
    match conn.query_row(
        "INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash)
         SELECT p.id, p.thread_id, p.board_id, ?2, ?3
         FROM posts p
         WHERE p.id = ?1
         RETURNING id",
        params![post_id, reason, reporter_hash],
        |r| r.get::<_, i64>(0),
    ) {
        Ok(_id) => Ok(ReportSubmission::Filed),
        Err(error) if is_open_report_unique_violation(&error) => Ok(ReportSubmission::AlreadyFiled),
        Err(rusqlite::Error::QueryReturnedNoRows) => anyhow::bail!("Post id {post_id} not found"),
        Err(error) => Err(error).context("Failed to insert report"),
    }
}

/// Return all open reports enriched with board name and post preview.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_open_reports(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::ReportWithContext>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.id, r.post_id, r.thread_id, r.board_id, r.reason,
                r.reporter_hash, r.status, r.created_at, r.resolved_at, r.resolved_by,
                b.short_name, p.body, p.ip_hash
         FROM reports r
         JOIN boards b ON b.id = r.board_id
         JOIN posts  p ON p.id = r.post_id
         WHERE r.status = 'open'
         ORDER BY r.created_at DESC
         LIMIT 200",
    )?;
    let rows = stmt.query_map([], |row| {
        let report = crate::models::Report {
            id: row.get(0)?,
            post_id: row.get(1)?,
            thread_id: row.get(2)?,
            board_id: row.get(3)?,
            reason: row.get(4)?,
            reporter_hash: row.get(5)?,
            status: row.get(6)?,
            created_at: row.get(7)?,
            resolved_at: row.get(8)?,
            resolved_by: row.get(9)?,
        };
        let board_short: String = row.get(10)?;
        let body: String = row.get(11)?;
        let ip_hash: Option<String> = row.get(12)?;
        let preview: String = body.chars().take(120).collect();
        Ok(crate::models::ReportWithContext {
            report,
            board_short,
            post_preview: preview,
            post_ip_hash: ip_hash,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Resolve a report by marking it closed.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn resolve_report(conn: &rusqlite::Connection, report_id: i64, admin_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE reports SET status='resolved', resolved_at=unixepoch(), resolved_by=?1
             WHERE id = ?2 AND status = 'open'",
            params![admin_id, report_id],
        )
        .context("Failed to resolve report")?;
    if n == 0 {
        anyhow::bail!("Report id {report_id} not found or already resolved");
    }
    Ok(())
}

// Moderation log
/// Append one entry to the moderation action log.
///
/// # Errors
/// Returns an error if the database operation fails.
#[expect(
    clippy::too_many_arguments,
    reason = "the audit-log row is intentionally written as one atomic record"
)]
pub fn log_mod_action(
    conn: &rusqlite::Connection,
    admin_id: i64,
    admin_name: &str,
    action: &str,
    target_type: &str,
    target_id: Option<i64>,
    board_short: &str,
    detail: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mod_log
             (admin_id, admin_name, action, target_type, target_id, board_short, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            admin_id,
            admin_name,
            action,
            target_type,
            target_id,
            board_short,
            detail
        ],
    )?;
    Ok(())
}

/// Retrieve a page of moderation log entries, newest first.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_mod_log(
    conn: &rusqlite::Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::ModLogEntry>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, admin_id, admin_name, action, target_type, target_id,
                board_short, detail, created_at
         FROM mod_log
         ORDER BY created_at DESC, id DESC
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt.query_map(params![limit, offset], |row| {
        Ok(crate::models::ModLogEntry {
            id: row.get(0)?,
            admin_id: row.get(1)?,
            admin_name: row.get(2)?,
            action: row.get(3)?,
            target_type: row.get(4)?,
            target_id: row.get(5)?,
            board_short: row.get(6)?,
            detail: row.get(7)?,
            created_at: row.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Total count of `mod_log` entries (for pagination).
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn count_mod_log(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM mod_log", [], |r| r.get(0))?)
}

// Ban appeals
/// File a ban appeal atomically while enforcing the 24-hour duplicate guard.
///
/// Uses `BEGIN IMMEDIATE` so the "is this IP banned / has it appealed recently"
/// checks and the eventual INSERT all see a consistent write-locked view.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn file_ban_appeal(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    reason: &str,
) -> Result<BanAppealSubmission> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin ban-appeal transaction")?;

    let result: Result<BanAppealSubmission> = (|| {
        if is_banned(conn, ip_hash)?.is_none() {
            return Ok(BanAppealSubmission::NotBanned);
        }
        if has_recent_appeal(conn, ip_hash)? {
            return Ok(BanAppealSubmission::AlreadyFiled);
        }

        let _: i64 = conn
            .query_row(
                "INSERT INTO ban_appeals (ip_hash, reason) VALUES (?1, ?2) RETURNING id",
                params![ip_hash, reason],
                |row| row.get(0),
            )
            .context("Failed to insert ban appeal")?;
        Ok(BanAppealSubmission::Filed)
    })();

    match result {
        Ok(outcome) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit ban-appeal transaction")?;
            Ok(outcome)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Return all open ban appeals, newest first.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_open_ban_appeals(conn: &rusqlite::Connection) -> Result<Vec<crate::models::BanAppeal>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, ip_hash, reason, status, created_at
         FROM ban_appeals WHERE status = 'open'
         ORDER BY created_at DESC LIMIT 200",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(crate::models::BanAppeal {
            id: r.get(0)?,
            ip_hash: r.get(1)?,
            reason: r.get(2)?,
            status: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Dismiss a ban appeal without removing its ban.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn dismiss_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE ban_appeals SET status='dismissed' WHERE id=?1 AND status='open'",
            params![appeal_id],
        )
        .context("Failed to dismiss ban appeal")?;
    if n == 0 {
        anyhow::bail!("Ban appeal id {appeal_id} not found or already handled");
    }
    Ok(())
}

/// Accept an appeal and lift bans for the address stored on that appeal.
///
/// Accepted appeals now set status='accepted' (not 'dismissed')
/// so the moderation history accurately distinguishes denied vs granted appeals.
/// The valid status values for `BanAppeal` are: "open" | "dismissed" | "accepted".
///
/// # Errors
/// Returns the appealed address hash, or an error if the row is missing,
/// already handled, or the database operation fails.
pub fn accept_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<String> {
    // Both updates must succeed atomically; IMMEDIATE prevents write contention.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin accept-appeal transaction")?;

    let result: Result<String> = (|| {
        let ip_hash = conn
            .query_row(
                "UPDATE ban_appeals
                 SET status = 'accepted'
                 WHERE id = ?1 AND status = 'open'
                 RETURNING ip_hash",
                params![appeal_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("Failed to accept ban appeal")?
            .with_context(|| format!("Ban appeal id {appeal_id} not found or already handled"))?;
        conn.execute("DELETE FROM bans WHERE ip_hash=?1", params![ip_hash])
            .context("Failed to lift ban during appeal acceptance")?;
        Ok(ip_hash)
    })();

    match result {
        Ok(ip_hash) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Failed to commit accept-appeal transaction");
            }
            Ok(ip_hash)
        }
        Err(e) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(e)
        }
    }
}

/// Check if an appeal has already been filed from this `ip_hash` (any status)
/// within the last 24 hours, to prevent spam.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn has_recent_appeal(conn: &rusqlite::Connection, ip_hash: &str) -> Result<bool> {
    let cutoff = chrono::Utc::now().timestamp().saturating_sub(86400);
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ban_appeals WHERE ip_hash=?1 AND created_at > ?2",
        params![ip_hash, cutoff],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

// IP history
/// Count total posts by IP hash across all boards.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn count_posts_by_ip_hash(conn: &rusqlite::Connection, ip_hash: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE ip_hash = ?1",
        rusqlite::params![ip_hash],
        |r| r.get(0),
    )?)
}

/// Return paginated posts by IP hash, newest first, across all boards.
/// Each post is joined with its board `short_name` for display.
///
/// Posts join directly to boards so orphaned posts are not hidden by a missing
/// thread row. The statement is cached for repeated moderation lookups.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_posts_by_ip_hash(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<(crate::models::Post, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT p.id, p.thread_id, p.board_id, p.name, p.tripcode, p.subject,
                p.body, p.body_html, p.ip_hash, p.file_path, p.file_name,
                p.file_size, p.thumb_path, p.mime_type, p.created_at,
                p.deletion_token, p.is_op, p.media_type,
                p.audio_file_path, p.audio_file_name, p.audio_file_size, p.audio_mime_type,
                p.edited_at, p.media_processing_state, p.media_processing_error,
                b.short_name
         FROM posts p
         JOIN boards b ON b.id = p.board_id
         WHERE p.ip_hash = ?1
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let rows = stmt.query_map(rusqlite::params![ip_hash, limit, offset], |row| {
        // map_post reads columns 0–24 (the 25 canonical post columns).
        // Column 25 is b.short_name, appended only by this query.
        let post = super::posts::map_post(row)?;
        let board_short: String = row.get(25)?;
        Ok((post, board_short))
    })?;

    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// Database maintenance
/// Run PRAGMA `wal_checkpoint(TRUNCATE)` and return (`log_pages`, `checkpointed_pages`, busy).
///
/// The raw PRAGMA `wal_checkpoint` pragma returns three columns in this order:
///   col 0 — busy:         1 if a checkpoint could not complete due to an active reader/writer
///   col 1 — log:          total pages in the WAL file
///   col 2 — checkpointed: pages actually written back to the database
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

        CREATE VIRTUAL TABLE posts_fts
        USING fts5(body, content='posts', content_rowid='id', tokenize='unicode61');

        CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN
            INSERT INTO posts_fts(rowid, body) VALUES (new.id, new.body);
        END;

        CREATE TRIGGER posts_ad AFTER DELETE ON posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, body) VALUES('delete', old.id, old.body);
        END;

        CREATE TRIGGER posts_au AFTER UPDATE OF body ON posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, body) VALUES('delete', old.id, old.body);
            INSERT INTO posts_fts(rowid, body) VALUES (new.id, new.body);
        END;

        INSERT INTO posts_fts(posts_fts) VALUES('rebuild');
        ",
    )
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

#[cfg(test)]
mod tests {
    use super::{
        accept_ban_appeal, attempt_db_repair, check_db_health,
        db_repair_aborted_for_backup_failure, dismiss_ban_appeal, file_ban_appeal, file_report,
        get_posts_by_ip_hash, resolve_report, BanAppealSubmission, DbRepairBackup,
        ReportSubmission,
    };
    use crate::db::{create_board, create_thread_with_optional_poll, get_board_by_short, NewPost};
    use anyhow::{Context as _, Result};

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn ban_appeal_submission_is_deduplicated_within_window() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        crate::db::add_ban(&conn, "hash1", "reason", None)?;

        let first = file_ban_appeal(&conn, "hash1", "please unban")?;
        let second = file_ban_appeal(&conn, "hash1", "second try")?;

        assert_eq!(
            first,
            BanAppealSubmission::Filed,
            "the first active-ban appeal should be filed"
        );
        assert_eq!(
            second,
            BanAppealSubmission::AlreadyFiled,
            "a second appeal in the window should be deduplicated"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn ban_appeal_submission_requires_active_ban() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        let result = file_ban_appeal(&conn, "hash2", "please unban")?;
        assert_eq!(
            result,
            BanAppealSubmission::NotBanned,
            "unbanned addresses should not create appeals"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn appeal_resolution_uses_the_stored_address_and_is_single_transition() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        crate::db::add_ban(&conn, "appealed-hash", "appealed", None)?;
        crate::db::add_ban(&conn, "other-hash", "other", None)?;
        assert_eq!(
            file_ban_appeal(&conn, "appealed-hash", "please unban")?,
            BanAppealSubmission::Filed
        );
        let appeal_id: i64 = conn.query_row(
            "SELECT id FROM ban_appeals WHERE ip_hash = 'appealed-hash'",
            [],
            |row| row.get(0),
        )?;

        let accepted_hash = accept_ban_appeal(&conn, appeal_id)?;

        assert_eq!(accepted_hash, "appealed-hash");
        assert!(
            crate::db::is_banned(&conn, "appealed-hash")?.is_none(),
            "acceptance should lift the ban named by the appeal row"
        );
        assert!(
            crate::db::is_banned(&conn, "other-hash")?.is_some(),
            "acceptance must not lift an unrelated caller-selected ban"
        );
        assert!(
            accept_ban_appeal(&conn, appeal_id).is_err(),
            "an accepted appeal must not transition a second time"
        );
        assert!(
            dismiss_ban_appeal(&conn, appeal_id).is_err(),
            "an accepted appeal must not be overwritten as dismissed"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn report_resolution_is_a_single_state_transition() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let admin_id = crate::db::create_admin(&conn, "moderator", "password-hash")?;
        let board_id = create_board(&conn, "reports", "Reports", "", false)?;
        let thread_id: i64 = conn.query_row(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'reported') RETURNING id",
            [board_id],
            |row| row.get(0),
        )?;
        let post_id: i64 = conn.query_row(
            "INSERT INTO posts (
                 thread_id, board_id, body, body_html, deletion_token, is_op
             ) VALUES (?1, ?2, 'body', '<p>body</p>', 'token', 1)
             RETURNING id",
            rusqlite::params![thread_id, board_id],
            |row| row.get(0),
        )?;
        assert_eq!(
            file_report(&conn, post_id, "reason", "reporter")?,
            ReportSubmission::Filed
        );
        let report_id: i64 = conn.query_row(
            "SELECT id FROM reports WHERE post_id = ?1",
            [post_id],
            |row| row.get(0),
        )?;

        resolve_report(&conn, report_id, admin_id)?;

        assert!(
            resolve_report(&conn, report_id, admin_id).is_err(),
            "a resolved report must not transition a second time"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn db_health_check_reports_ok_for_clean_test_db() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        let report = check_db_health(&conn);
        assert!(
            report.before.ok(),
            "a clean test database should be healthy"
        );
        assert_eq!(
            report.before.integrity.output(),
            "ok",
            "integrity check should pass"
        );
        assert_eq!(
            report.before.foreign_keys.output(),
            "ok",
            "foreign-key check should pass"
        );
        assert!(
            !report.repair_attempted,
            "a health check should not attempt repair"
        );
        assert!(
            report.repair_summary.is_empty(),
            "a health check should not create a repair summary"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn db_health_repair_noops_when_db_is_already_clean() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        let report = attempt_db_repair(
            &conn,
            Some(DbRepairBackup {
                backup_id: "2026-05-06_1215_pre-repair-db_c81f20".to_owned(),
                backup_type: "DB + config".to_owned(),
                backup_path: "/tmp/rustchan-data/backups/2026-05-06_1215_pre-repair-db_c81f20"
                    .to_owned(),
                verified: true,
            }),
        );
        assert!(report.before.ok(), "pre-repair health should pass");
        assert_eq!(
            report.after.as_ref().map(|after| after.integrity.output()),
            Some("ok".to_owned()),
            "post-maintenance integrity should pass"
        );
        assert_eq!(
            report.after.as_ref().map(super::DbHealthSnapshot::ok),
            Some(true),
            "post-maintenance health should pass"
        );
        assert_eq!(
            report
                .repair_backup
                .as_ref()
                .map(|backup| backup.backup_id.as_str()),
            Some("2026-05-06_1215_pre-repair-db_c81f20"),
            "repair report should retain its safety backup"
        );
        assert!(
            report
                .repair_summary
                .iter()
                .any(|line| line.contains("No corruption-specific fixes were required")),
            "a clean database should report that no corruption repair was needed"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn db_health_repair_aborts_when_backup_fails() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;

        let report = db_repair_aborted_for_backup_failure(&conn, "disk full");

        assert!(
            !report.repair_attempted,
            "repair must not run without a safety backup"
        );
        assert_eq!(
            report.repair_backup_error.as_deref(),
            Some("disk full"),
            "the backup failure should be retained"
        );
        assert!(
            report.after.is_none(),
            "an aborted repair should have no after snapshot"
        );
        assert!(
            report.repair_steps.is_empty(),
            "an aborted repair should run no steps"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn db_health_check_reports_foreign_key_violations() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        conn.execute_batch(
            r"
            CREATE TABLE fk_health_parent(id INTEGER PRIMARY KEY);
            CREATE TABLE fk_health_child(
                id INTEGER PRIMARY KEY,
                parent_id INTEGER NOT NULL REFERENCES fk_health_parent(id)
            );
            PRAGMA foreign_keys = OFF;
            INSERT INTO fk_health_child(id, parent_id) VALUES (1, 999);
            PRAGMA foreign_keys = ON;
            ",
        )?;

        let report = check_db_health(&conn);

        assert!(
            report.before.integrity.ok,
            "the structural integrity check should still pass"
        );
        assert!(
            !report.before.foreign_keys.ok,
            "the foreign-key check should report the injected violation"
        );
        assert!(
            report
                .before
                .foreign_keys
                .output()
                .contains("fk_health_child"),
            "the violation should identify the child table"
        );
        assert!(
            !report.before.ok(),
            "a foreign-key violation should fail aggregate health"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn get_posts_by_ip_hash_maps_posts_with_media_processing_columns() -> Result<()> {
        let pool = crate::db::init_test_pool()?;
        let conn = pool.get()?;
        let ip_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        create_board(&conn, "test", "Test", "", false)?;
        let board = get_board_by_short(&conn, "test")?.context("test board should exist")?;
        let post = NewPost {
            thread_id: 0,
            board_id: board.id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "body".to_owned(),
            body_html: "<p>body</p>".to_owned(),
            ip_hash: Some(ip_hash.to_owned()),
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "token".to_owned(),
            is_op: true,
        };

        let (_, post_id, _) =
            create_thread_with_optional_poll(&conn, board.id, None, &post, "", None, None)?;
        crate::db::set_post_media_processing_state(
            &conn,
            post_id,
            Some("pending"),
            Some("transcoding"),
        )?;

        let posts = get_posts_by_ip_hash(&conn, ip_hash, 25, 0)?;
        let (post, board_short) = posts
            .first()
            .context("IP history should contain the post")?;

        assert_eq!(posts.len(), 1, "exactly one post should match the IP hash");
        assert_eq!(post.id, post_id, "the created post should be returned");
        assert_eq!(
            post.media_processing_state.as_deref(),
            Some("pending"),
            "media processing state should be decoded"
        );
        assert_eq!(
            post.media_processing_error.as_deref(),
            Some("transcoding"),
            "media processing error should be decoded"
        );
        assert_eq!(board_short, "test", "the board slug should be included");
        Ok(())
    }
}
