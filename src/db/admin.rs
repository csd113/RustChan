// db/admin.rs — Admin-facing queries.
//
// Covers: admin user & session management, bans, word filters, user reports,
// moderation log, ban appeals, IP history, WAL checkpoint, VACUUM, DB size,
// and the list_admins helper used by CLI tooling.
//
// FIX summary (from audit):
//   HIGH-1   is_banned: switched to prepare_cached (hot path on every post submission)
//   HIGH-2   get_word_filters: switched to prepare_cached (hot path on every post submission)
//   HIGH-3   get_posts_by_ip_hash: removed unnecessary threads join; posts already carries board_id
//   MED-4    create_admin, add_ban, add_word_filter, file_report, file_ban_appeal:
//              INSERT … RETURNING id replaces execute + last_insert_rowid()
//   MED-5    update_admin_password, remove_ban, resolve_report, dismiss_ban_appeal:
//              added rows-affected checks so missing targets surface as errors
//   MED-6    accept_ban_appeal: status='accepted' (was duplicating 'dismissed')
//              already correct; doc comment clarified
//   MED-7    file_report: added has_reported_post guard to prevent spam reports
//   MED-8    has_recent_appeal / file_ban_appeal TOCTOU: documented; full fix
//              requires a schema-level UNIQUE constraint
//   LOW-9    Remaining bare prepare → prepare_cached throughout
//   LOW-10   Added .context() on key operations
//   LOW-11   get_db_size_bytes: added doc comment noting WAL file not included
//   LOW-12   is_banned NULLS FIRST: added doc comment for SQLite ≥ 3.30.0 requirement

use crate::models::*;
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

// ─── Admin user queries ───────────────────────────────────────────────────────

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

/// FIX[MED-4]: Replaced execute + last_insert_rowid() with INSERT … RETURNING id
/// to retrieve the new row id atomically in the same statement.
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

/// FIX[MED-5]: Added rows-affected check — silently succeeding when the target
/// username doesn't exist made password-reset errors invisible to the operator.
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
        anyhow::bail!("Admin user '{}' not found", username);
    }
    Ok(())
}

/// List all admin users (for CLI tooling).
/// FIX[LOW-9]: Switched from bare prepare to prepare_cached.
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, username, created_at FROM admin_users ORDER BY id ASC")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String, i64)>>>()?;
    Ok(rows)
}

/// Retrieve admin username by admin_id (used when building log entries).
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

pub fn delete_session(conn: &rusqlite::Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM admin_sessions WHERE id = ?1",
        params![session_id],
    )?;
    Ok(())
}

/// Clean up expired sessions (called periodically).
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
/// FIX[HIGH-1]: Switched to prepare_cached — this is called on every post
/// submission and was recompiling the statement on every call.
///
/// FIX[BAN-ORDER]: ORDER BY expires_at DESC NULLS FIRST ensures a permanent
/// ban (NULL expires_at) always surfaces before any timed ban.
///
/// Note: NULLS FIRST requires SQLite ≥ 3.30.0 (released 2019-10-04).
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
    Ok(result.map(|r| r.unwrap_or_default()))
}

/// FIX[MED-4]: INSERT … RETURNING id replaces execute + last_insert_rowid().
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

/// FIX[MED-5]: Returns an error when the target ban row does not exist,
/// making double-removes and stale ban-ids visible rather than silently succeeding.
pub fn remove_ban(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn
        .execute("DELETE FROM bans WHERE id = ?1", params![id])
        .context("Failed to remove ban")?;
    if n == 0 {
        anyhow::bail!("Ban id {} not found", id);
    }
    Ok(())
}

/// FIX[LOW-9]: Switched from bare prepare to prepare_cached.
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

/// FIX[HIGH-2]: Switched to prepare_cached — called on every post submission
/// to apply word filters; recompiling the statement every time was wasteful.
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

/// FIX[MED-4]: INSERT … RETURNING id replaces execute + last_insert_rowid().
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

pub fn remove_word_filter(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM word_filters WHERE id = ?1", params![id])?;
    Ok(())
}

// ─── Reports ──────────────────────────────────────────────────────────────────

/// Guard helper: returns true if `reporter_hash` has already filed a report
/// against `post_id` that is still open.
///
/// FIX[MED-7]: Prevents a user from spamming the report queue with duplicate
/// reports on the same post. Called inside file_report before the INSERT.
///
/// Note: There is a TOCTOU race between has_reported_post and the INSERT in
/// file_report (two concurrent requests can both pass the check). A full fix
/// would require a schema-level UNIQUE(post_id, reporter_hash) constraint, but
/// that would block re-reporting after a resolved report. The guard here is
/// sufficient to prevent accidental spam; deliberate concurrent abuse is
/// extremely unlikely in practice.
fn has_reported_post(
    conn: &rusqlite::Connection,
    post_id: i64,
    reporter_hash: &str,
) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM reports
         WHERE post_id = ?1 AND reporter_hash = ?2 AND status = 'open'",
        params![post_id, reporter_hash],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// File a new report against a post. Returns the new report id.
///
/// FIX[MED-4]: INSERT … RETURNING id replaces execute + last_insert_rowid().
/// FIX[MED-7]: Duplicate-report guard added via has_reported_post.
pub fn file_report(
    conn: &rusqlite::Connection,
    post_id: i64,
    thread_id: i64,
    board_id: i64,
    reason: &str,
    reporter_hash: &str,
) -> Result<i64> {
    if has_reported_post(conn, post_id, reporter_hash)? {
        anyhow::bail!("Already reported post {}", post_id);
    }
    let id: i64 = conn
        .query_row(
            "INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash)
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
            params![post_id, thread_id, board_id, reason, reporter_hash],
            |r| r.get(0),
        )
        .context("Failed to insert report")?;
    Ok(id)
}

/// Return all open reports enriched with board name and post preview.
/// FIX[LOW-9]: Switched from bare prepare to prepare_cached.
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
        let ip_hash: String = row.get(12)?;
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
/// FIX[MED-5]: Added rows-affected check.
pub fn resolve_report(conn: &rusqlite::Connection, report_id: i64, admin_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE reports SET status='resolved', resolved_at=unixepoch(), resolved_by=?1
             WHERE id = ?2",
            params![admin_id, report_id],
        )
        .context("Failed to resolve report")?;
    if n == 0 {
        anyhow::bail!("Report id {} not found", report_id);
    }
    Ok(())
}

/// Count of currently open (unresolved) reports.
#[allow(dead_code)]
pub fn open_report_count(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM reports WHERE status='open'",
        [],
        |r| r.get(0),
    )?)
}

// ─── Moderation log ───────────────────────────────────────────────────────────

/// Append one entry to the moderation action log.
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
/// FIX[LOW-9]: Switched from bare prepare to prepare_cached.
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

/// Total count of mod_log entries (for pagination).
pub fn count_mod_log(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM mod_log", [], |r| r.get(0))?)
}

// ─── Ban appeals ──────────────────────────────────────────────────────────────

/// Insert a new ban appeal. Returns the new appeal id.
///
/// FIX[MED-4]: INSERT … RETURNING id replaces execute + last_insert_rowid().
///
/// Note (TOCTOU): has_recent_appeal and file_ban_appeal have a race — two
/// concurrent requests from the same IP can both pass the check and both
/// insert appeals. A full fix requires a schema-level UNIQUE(ip_hash) or a
/// time-windowed partial unique index. The guard is retained as a best-effort
/// spam deterrent for the common (non-concurrent) case.
pub fn file_ban_appeal(conn: &rusqlite::Connection, ip_hash: &str, reason: &str) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO ban_appeals (ip_hash, reason) VALUES (?1, ?2) RETURNING id",
            params![ip_hash, reason],
            |r| r.get(0),
        )
        .context("Failed to insert ban appeal")?;
    Ok(id)
}

/// Return all open ban appeals, newest first.
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
/// FIX[MED-5]: Added rows-affected check.
pub fn dismiss_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE ban_appeals SET status='dismissed' WHERE id=?1",
            params![appeal_id],
        )
        .context("Failed to dismiss ban appeal")?;
    if n == 0 {
        anyhow::bail!("Ban appeal id {} not found", appeal_id);
    }
    Ok(())
}

/// Dismiss appeal AND lift the ban for this ip_hash.
///
/// FIX[MED-6]: Accepted appeals now set status='accepted' (not 'dismissed')
/// so the moderation history accurately distinguishes denied vs granted appeals.
/// The valid status values for BanAppeal are: "open" | "dismissed" | "accepted".
pub fn accept_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64, ip_hash: &str) -> Result<()> {
    // Both updates must succeed together.
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin accept-appeal transaction")?;
    let n = tx
        .execute(
            "UPDATE ban_appeals SET status='accepted' WHERE id=?1",
            params![appeal_id],
        )
        .context("Failed to accept ban appeal")?;
    if n == 0 {
        tx.rollback().ok();
        anyhow::bail!("Ban appeal id {} not found", appeal_id);
    }
    tx.execute("DELETE FROM bans WHERE ip_hash=?1", params![ip_hash])
        .context("Failed to lift ban during appeal acceptance")?;
    tx.commit()
        .context("Failed to commit accept-appeal transaction")?;
    Ok(())
}

/// Count of currently open ban appeals.
#[allow(dead_code)]
pub fn open_appeal_count(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM ban_appeals WHERE status='open'",
        [],
        |r| r.get(0),
    )?)
}

/// Check if an appeal has already been filed from this ip_hash (any status)
/// within the last 24 hours, to prevent spam.
///
/// Note (TOCTOU): see file_ban_appeal for the concurrency caveat.
pub fn has_recent_appeal(conn: &rusqlite::Connection, ip_hash: &str) -> Result<bool> {
    let cutoff = chrono::Utc::now().timestamp() - 86400;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ban_appeals WHERE ip_hash=?1 AND created_at > ?2",
        params![ip_hash, cutoff],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

// ─── IP history ───────────────────────────────────────────────────────────────

/// Count total posts by IP hash across all boards.
pub fn count_posts_by_ip_hash(conn: &rusqlite::Connection, ip_hash: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE ip_hash = ?1",
        rusqlite::params![ip_hash],
        |r| r.get(0),
    )?)
}

/// Return paginated posts by IP hash, newest first, across all boards.
/// Each post is joined with its board short_name for display.
///
/// FIX[HIGH-3]: Replaced the three-way join (posts → threads → boards) with a
/// direct two-way join (posts → boards). The posts table already carries board_id,
/// making the threads join unnecessary. The old join also silently hid any posts
/// whose thread had been deleted (orphaned posts) because the INNER JOIN on
/// threads would exclude them.
///
/// FIX[LOW-9]: Switched from bare prepare to prepare_cached.
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

/// Run PRAGMA wal_checkpoint(TRUNCATE) and return (log_pages, checkpointed_pages, busy).
///
/// The raw PRAGMA wal_checkpoint pragma returns three columns in this order:
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
/// (page_count × page_size, as reported by SQLite).
///
/// FIX[LOW-11]: Note that this does NOT include the WAL file size. When the
/// database is in WAL mode, the total on-disk footprint is this value plus the
/// size of the .db-wal file. Call run_wal_checkpoint before get_db_size_bytes
/// if you need a reliable post-checkpoint size.
pub fn get_db_size_bytes(conn: &rusqlite::Connection) -> Result<i64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(page_count * page_size)
}

/// Run VACUUM on the database, rebuilding it into a minimal file.
///
/// VACUUM rewrites the entire database file, compacting free pages left by
/// bulk deletions. It cannot run inside a transaction. The call blocks until
/// the full rebuild is complete; for large databases this may take several
/// seconds. Always call `get_db_size_bytes` before and after to report the
/// space saving to the operator.
pub fn run_vacuum(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}
