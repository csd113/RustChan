// db/admin.rs — Admin-facing queries.
//
// Covers: admin user & session management, bans, word filters, user reports,
// moderation log, ban appeals, IP history, WAL checkpoint, VACUUM, DB size,
// and the list_admins helper used by CLI tooling.

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

pub fn create_admin(conn: &rusqlite::Connection, username: &str, hash: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO admin_users (username, password_hash) VALUES (?1, ?2)",
        params![username, hash],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_admin_password(
    conn: &rusqlite::Connection,
    username: &str,
    hash: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE admin_users SET password_hash = ?1 WHERE username = ?2",
        params![hash, username],
    )?;
    Ok(())
}

/// List all admin users (for CLI tooling).
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT id, username, created_at FROM admin_users ORDER BY id ASC")?;
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
    )?;
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

pub fn is_banned(conn: &rusqlite::Connection, ip_hash: &str) -> Result<Option<String>> {
    let now = chrono::Utc::now().timestamp();
    // A ban with NULL expires_at is permanent.
    let result: Option<Option<String>> = conn
        .query_row(
            "SELECT reason FROM bans WHERE ip_hash = ?1
             AND (expires_at IS NULL OR expires_at > ?2)
             LIMIT 1",
            params![ip_hash, now],
            |r| r.get(0),
        )
        .optional()?;
    // Flatten: None = not banned; Some(r) = banned (r may be empty if no reason set)
    Ok(result.map(|r| r.unwrap_or_default()))
}

pub fn add_ban(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    reason: &str,
    expires_at: Option<i64>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO bans (ip_hash, reason, expires_at) VALUES (?1, ?2, ?3)",
        params![ip_hash, reason, expires_at],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_ban(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM bans WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn list_bans(conn: &rusqlite::Connection) -> Result<Vec<Ban>> {
    let mut stmt = conn.prepare(
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

pub fn get_word_filters(conn: &rusqlite::Connection) -> Result<Vec<WordFilter>> {
    let mut stmt = conn.prepare("SELECT id, pattern, replacement FROM word_filters")?;
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

pub fn add_word_filter(
    conn: &rusqlite::Connection,
    pattern: &str,
    replacement: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO word_filters (pattern, replacement) VALUES (?1, ?2)",
        params![pattern, replacement],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_word_filter(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM word_filters WHERE id = ?1", params![id])?;
    Ok(())
}

// ─── Reports ──────────────────────────────────────────────────────────────────

/// File a new report against a post. Returns the new report id.
pub fn file_report(
    conn: &rusqlite::Connection,
    post_id: i64,
    thread_id: i64,
    board_id: i64,
    reason: &str,
    reporter_hash: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![post_id, thread_id, board_id, reason, reporter_hash],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Return all open reports enriched with board name and post preview.
pub fn get_open_reports(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::ReportWithContext>> {
    let mut stmt = conn.prepare(
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
pub fn resolve_report(conn: &rusqlite::Connection, report_id: i64, admin_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE reports SET status='resolved', resolved_at=unixepoch(), resolved_by=?1
         WHERE id = ?2",
        params![admin_id, report_id],
    )?;
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
pub fn get_mod_log(
    conn: &rusqlite::Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::ModLogEntry>> {
    let mut stmt = conn.prepare(
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
pub fn file_ban_appeal(conn: &rusqlite::Connection, ip_hash: &str, reason: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO ban_appeals (ip_hash, reason) VALUES (?1, ?2)",
        params![ip_hash, reason],
    )?;
    Ok(conn.last_insert_rowid())
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
pub fn dismiss_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE ban_appeals SET status='dismissed' WHERE id=?1",
        params![appeal_id],
    )?;
    Ok(())
}

/// Dismiss appeal AND lift the ban for this ip_hash.
pub fn accept_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64, ip_hash: &str) -> Result<()> {
    // Both updates must succeed together.
    let tx = conn
        .unchecked_transaction()
        .context("Failed to begin accept-appeal transaction")?;
    tx.execute(
        "UPDATE ban_appeals SET status='dismissed' WHERE id=?1",
        params![appeal_id],
    )?;
    tx.execute("DELETE FROM bans WHERE ip_hash=?1", params![ip_hash])?;
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
pub fn get_posts_by_ip_hash(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<(crate::models::Post, String)>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.thread_id, p.board_id, p.name, p.tripcode, p.subject,
                p.body, p.body_html, p.ip_hash, p.file_path, p.file_name,
                p.file_size, p.thumb_path, p.mime_type, p.created_at,
                p.deletion_token, p.is_op, p.media_type,
                p.audio_file_path, p.audio_file_name, p.audio_file_size, p.audio_mime_type,
                p.edited_at,
                b.short_name
         FROM posts p
         JOIN threads t ON p.thread_id = t.id
         JOIN boards  b ON t.board_id  = b.id
         WHERE p.ip_hash = ?1
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let rows = stmt.query_map(rusqlite::params![ip_hash, limit, offset], |row| {
        let media_type_str: Option<String> = row.get(17)?;
        let media_type = media_type_str
            .as_deref()
            .and_then(crate::models::MediaType::from_db_str);
        let post = crate::models::Post {
            id: row.get(0)?,
            thread_id: row.get(1)?,
            board_id: row.get(2)?,
            name: row.get(3)?,
            tripcode: row.get(4)?,
            subject: row.get(5)?,
            body: row.get(6)?,
            body_html: row.get(7)?,
            ip_hash: row.get(8)?,
            file_path: row.get(9)?,
            file_name: row.get(10)?,
            file_size: row.get(11)?,
            thumb_path: row.get(12)?,
            mime_type: row.get(13)?,
            created_at: row.get(14)?,
            deletion_token: row.get(15)?,
            is_op: row.get::<_, i32>(16)? != 0,
            media_type,
            audio_file_path: row.get(18)?,
            audio_file_name: row.get(19)?,
            audio_file_size: row.get(20)?,
            audio_mime_type: row.get(21)?,
            edited_at: row.get(22)?,
        };
        let board_short: String = row.get(23)?;
        Ok((post, board_short))
    })?;

    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ─── Database maintenance ─────────────────────────────────────────────────────

/// Run PRAGMA wal_checkpoint(TRUNCATE) and return (log_pages, moved_pages, busy).
///
/// SQLite's wal_checkpoint pragma returns three columns:
///   col 0 — busy:         1 if a checkpoint could not complete due to an active reader/writer
///   col 1 — log:          total pages in the WAL file
///   col 2 — checkpointed: pages actually written back to the database
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
