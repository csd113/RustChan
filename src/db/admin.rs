// db/admin.rs — Admin-facing queries.
//
// Covers: admin user & session management, bans, word filters, user reports,
// moderation log, ban appeals, IP history, WAL checkpoint, VACUUM, DB size,
// and the list_admins helper used by CLI tooling.
//
use crate::models::{AdminSession, AdminUser, Ban, WordFilter};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BanAppealSubmission {
    Filed,
    AlreadyFiled,
    NotBanned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportSubmission {
    Filed,
    AlreadyFiled,
}

#[derive(Debug, Clone)]
pub struct DbHealthReport {
    pub before_check: String,
    pub before_ok: bool,
    pub repair_attempted: bool,
    pub repair_summary: Vec<String>,
    pub repair_steps: Vec<String>,
    pub after_check: Option<String>,
    pub after_ok: Option<bool>,
}

// ─── Admin user queries ───────────────────────────────────────────────────────

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

/// Replaced execute + `last_insert_rowid()` with INSERT … RETURNING id
/// to retrieve the new row id atomically in the same statement.
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

/// Added rows-affected check — silently succeeding when the target
/// username doesn't exist made password-reset errors invisible to the operator.
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

/// List all admin users (for CLI tooling).
/// Switched from bare prepare to `prepare_cached`.
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

// ─── Session queries ──────────────────────────────────────────────────────────

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

// ─── Ban queries ──────────────────────────────────────────────────────────────

/// Check whether `ip_hash` is currently banned. Returns the ban reason if so.
///
/// Switched to `prepare_cached` — this is called on every post
/// submission and was recompiling the statement on every call.
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
    // Flatten: None = not banned; Some(r) = banned (r may be None if no reason was set)
    Ok(result.map(Option::unwrap_or_default))
}

/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
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

/// Switched from bare prepare to `prepare_cached`.
///
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

// ─── Word filter queries ──────────────────────────────────────────────────────

/// Switched to `prepare_cached` — called on every post submission
/// to apply word filters; recompiling the statement every time was wasteful.
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

/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
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

// ─── Reports ──────────────────────────────────────────────────────────────────

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
/// INSERT … RETURNING id replaces execute + `last_insert_rowid()`.
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
/// Switched from bare prepare to `prepare_cached`.
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

/// Resolve a report (mark it closed).
/// Added rows-affected check.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn resolve_report(conn: &rusqlite::Connection, report_id: i64, admin_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE reports SET status='resolved', resolved_at=unixepoch(), resolved_by=?1
             WHERE id = ?2",
            params![admin_id, report_id],
        )
        .context("Failed to resolve report")?;
    if n == 0 {
        anyhow::bail!("Report id {report_id} not found");
    }
    Ok(())
}

// ─── Moderation log ───────────────────────────────────────────────────────────

/// Append one entry to the moderation action log.
///
/// # Errors
/// Returns an error if the database operation fails.
#[allow(clippy::too_many_arguments)]
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

/// Retrieve a page of mod log entries, newest first.
/// Switched from bare prepare to `prepare_cached`.
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

// ─── Ban appeals ──────────────────────────────────────────────────────────────

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
            let _ = conn.execute_batch("ROLLBACK");
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

/// Dismiss a ban appeal (mark it closed without unbanning).
/// Added rows-affected check.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn dismiss_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE ban_appeals SET status='dismissed' WHERE id=?1",
            params![appeal_id],
        )
        .context("Failed to dismiss ban appeal")?;
    if n == 0 {
        anyhow::bail!("Ban appeal id {appeal_id} not found");
    }
    Ok(())
}

/// Dismiss appeal AND lift the ban for this `ip_hash`.
///
/// Accepted appeals now set status='accepted' (not 'dismissed')
/// so the moderation history accurately distinguishes denied vs granted appeals.
/// The valid status values for `BanAppeal` are: "open" | "dismissed" | "accepted".
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn accept_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64, ip_hash: &str) -> Result<()> {
    // Both updates must succeed atomically; IMMEDIATE prevents write contention.
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin accept-appeal transaction")?;

    let result: anyhow::Result<()> = (|| {
        let n = conn
            .execute(
                "UPDATE ban_appeals SET status='accepted' WHERE id=?1",
                params![appeal_id],
            )
            .context("Failed to accept ban appeal")?;
        if n == 0 {
            anyhow::bail!("Ban appeal id {appeal_id} not found");
        }
        conn.execute("DELETE FROM bans WHERE ip_hash=?1", params![ip_hash])
            .context("Failed to lift ban during appeal acceptance")?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit accept-appeal transaction")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
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

// ─── IP history ───────────────────────────────────────────────────────────────

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
/// Replaced the three-way join (posts → threads → boards) with a
/// direct two-way join (posts → boards). The posts table already carries `board_id`,
/// making the threads join unnecessary. The old join also silently hid any posts
/// whose thread had been deleted (orphaned posts) because the INNER JOIN on
/// threads would exclude them.
///
/// Switched from bare prepare to `prepare_cached`.
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
                p.edited_at,
                b.short_name
         FROM posts p
         JOIN boards b ON b.id = p.board_id
         WHERE p.ip_hash = ?1
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let rows = stmt.query_map(rusqlite::params![ip_hash, limit, offset], |row| {
        // map_post reads columns 0–22 (the 23 canonical post columns).
        // Column 23 is b.short_name, appended only by this query.
        let post = super::posts::map_post(row)?;
        let board_short: String = row.get(23)?;
        Ok((post, board_short))
    })?;

    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ─── Database maintenance ─────────────────────────────────────────────────────

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

fn integrity_check_status(conn: &rusqlite::Connection) -> (String, bool) {
    let mut stmt = match conn.prepare("PRAGMA integrity_check") {
        Ok(stmt) => stmt,
        Err(error) => return (format!("integrity_check failed: {error}"), false),
    };

    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(error) => return (format!("integrity_check failed: {error}"), false),
    };

    let mut messages = Vec::new();
    for row in rows.take(8) {
        match row {
            Ok(message) => {
                let ok = message.eq_ignore_ascii_case("ok");
                messages.push(message);
                if ok {
                    break;
                }
            }
            Err(error) => {
                messages.push(format!("integrity_check row failed: {error}"));
                break;
            }
        }
    }

    if messages.is_empty() {
        return ("integrity_check returned no rows".to_string(), false);
    }

    let joined = messages.join(" | ");
    let ok = matches!(messages.as_slice(), [message] if message.eq_ignore_ascii_case("ok"));
    (joined, ok)
}

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

pub fn check_db_health(conn: &rusqlite::Connection) -> DbHealthReport {
    let (before_check, before_ok) = integrity_check_status(conn);
    DbHealthReport {
        before_check,
        before_ok,
        repair_attempted: false,
        repair_summary: Vec::new(),
        repair_steps: Vec::new(),
        after_check: None,
        after_ok: None,
    }
}

pub fn attempt_db_repair(conn: &rusqlite::Connection) -> DbHealthReport {
    let (before_check, before_ok) = integrity_check_status(conn);
    let mut repair_summary = Vec::new();
    let mut repair_steps = Vec::new();

    if before_ok {
        repair_summary
            .push("No integrity problems were detected before the repair run.".to_string());
        repair_summary.push(
            "No corruption-specific fixes were required; the system only ran maintenance and index rebuild steps."
                .to_string(),
        );
    } else {
        repair_summary.push(
            "The initial integrity check reported a database problem, so repair steps were attempted."
                .to_string(),
        );
    }

    match conn.execute_batch("REINDEX;") {
        Ok(()) => repair_steps.push("Rebuilt SQLite indexes.".to_string()),
        Err(error) => repair_steps.push(format!("Could not rebuild SQLite indexes: {error}")),
    }

    match rebuild_posts_fts(conn) {
        Ok(()) => repair_steps
            .push("Rebuilt the post search index and recreated its update triggers.".to_string()),
        Err(error) => repair_steps.push(format!(
            "Could not rebuild the post search index and triggers: {error}"
        )),
    }

    match conn.execute_batch("PRAGMA optimize;") {
        Ok(()) => repair_steps.push("Optimized SQLite query-planner statistics.".to_string()),
        Err(error) => repair_steps.push(format!(
            "Could not optimize SQLite query-planner statistics: {error}"
        )),
    }

    let (after_check, after_ok) = integrity_check_status(conn);

    if before_ok && after_ok {
        repair_summary.push(
            "The final integrity check still passed, confirming that no additional repairs were needed."
                .to_string(),
        );
    } else if after_ok {
        repair_summary.push(
            "The final integrity check passed after the repair run, so the detected problem was cleared."
                .to_string(),
        );
    } else {
        repair_summary.push(
            "The repair run finished, but the final integrity check still reports a problem."
                .to_string(),
        );
    }

    DbHealthReport {
        before_check,
        before_ok,
        repair_attempted: true,
        repair_summary,
        repair_steps,
        after_check: Some(after_check),
        after_ok: Some(after_ok),
    }
}

#[cfg(test)]
mod tests {
    use super::{attempt_db_repair, check_db_health, file_ban_appeal, BanAppealSubmission};

    #[test]
    fn ban_appeal_submission_is_deduplicated_within_window() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");
        crate::db::add_ban(&conn, "hash1", "reason", None).expect("add ban");

        let first = file_ban_appeal(&conn, "hash1", "please unban").expect("first appeal");
        let second = file_ban_appeal(&conn, "hash1", "second try").expect("second appeal");

        assert_eq!(first, BanAppealSubmission::Filed);
        assert_eq!(second, BanAppealSubmission::AlreadyFiled);
    }

    #[test]
    fn ban_appeal_submission_requires_active_ban() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");

        let result = file_ban_appeal(&conn, "hash2", "please unban").expect("appeal result");
        assert_eq!(result, BanAppealSubmission::NotBanned);
    }

    #[test]
    fn db_health_check_reports_ok_for_clean_test_db() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");

        let report = check_db_health(&conn);
        assert!(report.before_ok);
        assert_eq!(report.before_check, "ok");
        assert!(!report.repair_attempted);
        assert!(report.repair_summary.is_empty());
    }

    #[test]
    fn db_health_repair_noops_when_db_is_already_clean() {
        let pool = crate::db::init_test_pool().expect("test pool");
        let conn = pool.get().expect("db connection");

        let report = attempt_db_repair(&conn);
        assert!(report.before_ok);
        assert_eq!(report.after_check.as_deref(), Some("ok"));
        assert_eq!(report.after_ok, Some(true));
        assert!(report
            .repair_summary
            .iter()
            .any(|line| line.contains("No corruption-specific fixes were required")));
    }
}
