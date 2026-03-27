// db/admin.rs — Admin-facing queries.
//
// Covers: admin user & session management, bans, word filters, user reports,
// moderation log, ban appeals, IP history, WAL checkpoint, VACUUM, DB size,
// and the list_admins helper used by CLI tooling.
//
use crate::models::{AdminSession, AdminUser, Ban, WordFilter};
use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

/// Maximum page size for admin list-style endpoints.
const MAX_PAGE_SIZE: i64 = 500;

/// Default page size for open moderation queues.
const DEFAULT_OPEN_QUEUE_LIMIT: i64 = 200;

// ─── Small helpers ────────────────────────────────────────────────────────────

fn normalize_paging(limit: i64, offset: i64) -> Result<(i64, i64)> {
    if limit < 0 {
        anyhow::bail!("limit must be >= 0");
    }
    if offset < 0 {
        anyhow::bail!("offset must be >= 0");
    }
    Ok((limit.min(MAX_PAGE_SIZE), offset))
}

// ─── Admin user queries ───────────────────────────────────────────────────────

/// Retrieve an admin user by exact username.
///
/// Username comparison semantics depend on the `SQLite` collation configured on
/// `admin_users.username`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_admin_by_username(
    conn: &rusqlite::Connection,
    username: &str,
) -> Result<Option<AdminUser>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, username, password_hash, created_at
             FROM admin_users
             WHERE username = ?1",
        )
        .context("Failed to prepare `get_admin_by_username`")?;

    stmt.query_row(params![username], |r| {
        Ok(AdminUser {
            id: r.get(0)?,
            username: r.get(1)?,
            password_hash: r.get(2)?,
            created_at: r.get(3)?,
        })
    })
    .optional()
    .context("Failed to fetch admin by username")
}

/// Create a new admin user and return its row id.
///
/// # Errors
/// Returns an error if the insert fails.
pub fn create_admin(conn: &rusqlite::Connection, username: &str, hash: &str) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO admin_users (username, password_hash)
             VALUES (?1, ?2)
             RETURNING id",
            params![username.trim(), hash],
            |r| r.get(0),
        )
        .context("Failed to create admin user")?;
    Ok(id)
}

/// Update the password hash for an existing admin.
///
/// # Errors
/// Returns an error if the update fails or the user does not exist.
pub fn update_admin_password(
    conn: &rusqlite::Connection,
    username: &str,
    hash: &str,
) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE admin_users
             SET password_hash = ?1
             WHERE username = ?2",
            params![hash, username.trim()],
        )
        .context("Failed to update admin password")?;
    if n == 0 {
        anyhow::bail!("Admin user '{username}' not found");
    }
    Ok(())
}

/// List all admin users (for CLI tooling).
///
/// # Errors
/// Returns an error if the query fails.
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, username, created_at
             FROM admin_users
             ORDER BY username COLLATE NOCASE ASC, id ASC",
        )
        .context("Failed to prepare `list_admins`")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .context("Failed to query admin list")?
        .collect::<rusqlite::Result<Vec<(i64, String, i64)>>>()
        .context("Failed to collect admin list rows")?;
    Ok(rows)
}

/// Retrieve admin username by `admin_id`.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_admin_name_by_id(conn: &rusqlite::Connection, admin_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT username
         FROM admin_users
         WHERE id = ?1",
        params![admin_id],
        |r| r.get(0),
    )
    .optional()
    .context("Failed to fetch admin name by id")
}

// ─── Session queries ──────────────────────────────────────────────────────────

/// Create a new admin session.
///
/// This function permits multiple concurrent sessions per admin.
///
/// # Errors
/// Returns an error if the insert fails.
pub fn create_session(
    conn: &rusqlite::Connection,
    session_id: &str,
    admin_id: i64,
    expires_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO admin_sessions (id, admin_id, expires_at)
         VALUES (?1, ?2, ?3)",
        params![session_id, admin_id, expires_at],
    )
    .context("Failed to create admin session")?;
    Ok(())
}

/// Fetch an unexpired session.
///
/// Expired sessions are not returned. This function also verifies that the
/// referenced admin user still exists.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_session(conn: &rusqlite::Connection, session_id: &str) -> Result<Option<AdminSession>> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn
        .prepare_cached(
            "SELECT s.id, s.admin_id, s.created_at, s.expires_at
             FROM admin_sessions s
             JOIN admin_users u ON u.id = s.admin_id
             WHERE s.id = ?1
               AND s.expires_at > ?2",
        )
        .context("Failed to prepare `get_session`")?;

    stmt.query_row(params![session_id, now], |r| {
        Ok(AdminSession {
            id: r.get(0)?,
            admin_id: r.get(1)?,
            created_at: r.get(2)?,
            expires_at: r.get(3)?,
        })
    })
    .optional()
    .context("Failed to fetch admin session")
}

/// Delete an admin session.
///
/// # Errors
/// Returns an error if the session does not exist or the delete fails.
pub fn delete_session(conn: &rusqlite::Connection, session_id: &str) -> Result<()> {
    let n = conn
        .execute(
            "DELETE FROM admin_sessions
             WHERE id = ?1",
            params![session_id],
        )
        .context("Failed to delete admin session")?;
    if n == 0 {
        anyhow::bail!("Admin session '{session_id}' not found");
    }
    Ok(())
}

/// Clean up expired sessions.
///
/// # Errors
/// Returns an error if the delete fails.
pub fn purge_expired_sessions(conn: &rusqlite::Connection) -> Result<usize> {
    let now = chrono::Utc::now().timestamp();
    let n = conn
        .execute(
            "DELETE FROM admin_sessions
             WHERE expires_at <= ?1",
            params![now],
        )
        .context("Failed to purge expired sessions")?;
    Ok(n)
}

// ─── Ban queries ──────────────────────────────────────────────────────────────

/// Check whether `ip_hash` is currently banned.
///
/// Returns:
/// - `Ok(None)` if not banned
/// - `Ok(Some(reason))` if banned, where `reason` may be the empty string if no
///   reason was stored
///
/// Permanent bans (`expires_at IS NULL`) sort before temporary bans. The
/// `NULLS FIRST` syntax requires `SQLite` ≥ 3.30.0.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn is_banned(conn: &rusqlite::Connection, ip_hash: &str) -> Result<Option<String>> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn
        .prepare_cached(
            "SELECT COALESCE(reason, '')
             FROM bans
             WHERE ip_hash = ?1
               AND (expires_at IS NULL OR expires_at > ?2)
             ORDER BY expires_at DESC NULLS FIRST,
                      created_at DESC,
                      id DESC
             LIMIT 1",
        )
        .context("Failed to prepare `is_banned`")?;

    stmt.query_row(params![ip_hash, now], |r| r.get(0))
        .optional()
        .context("Failed to query ban status")
}

/// Add a ban and return the new ban id.
///
/// # Errors
/// Returns an error if the insert fails.
pub fn add_ban(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    reason: &str,
    expires_at: Option<i64>,
) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO bans (ip_hash, reason, expires_at)
             VALUES (?1, ?2, ?3)
             RETURNING id",
            params![ip_hash, reason, expires_at],
            |r| r.get(0),
        )
        .context("Failed to insert ban")?;
    Ok(id)
}

/// Remove a ban by id.
///
/// # Errors
/// Returns an error if the ban does not exist or the delete fails.
pub fn remove_ban(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn
        .execute("DELETE FROM bans WHERE id = ?1", params![id])
        .context("Failed to remove ban")?;
    if n == 0 {
        anyhow::bail!("Ban id {id} not found");
    }
    Ok(())
}

/// List all bans, newest first.
///
/// # Errors
/// Returns an error if the query fails.
pub fn list_bans(conn: &rusqlite::Connection) -> Result<Vec<Ban>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, ip_hash, reason, expires_at, created_at
             FROM bans
             ORDER BY created_at DESC, id DESC",
        )
        .context("Failed to prepare `list_bans`")?;
    let bans = stmt
        .query_map([], |r| {
            Ok(Ban {
                id: r.get(0)?,
                ip_hash: r.get(1)?,
                reason: r.get(2)?,
                expires_at: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .context("Failed to query ban list")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect ban rows")?;
    Ok(bans)
}

// ─── Word filter queries ──────────────────────────────────────────────────────

/// Return all word filters in deterministic order.
///
/// # Errors
/// Returns an error if the database operation fails.
pub fn get_word_filters(conn: &rusqlite::Connection) -> Result<Vec<WordFilter>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, pattern, replacement
             FROM word_filters
             ORDER BY id ASC",
        )
        .context("Failed to prepare `get_word_filters`")?;
    let filters = stmt
        .query_map([], |r| {
            Ok(WordFilter {
                id: r.get(0)?,
                pattern: r.get(1)?,
                replacement: r.get(2)?,
            })
        })
        .context("Failed to query word filters")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect word filters")?;
    Ok(filters)
}

/// Add a word filter and return the new id.
///
/// # Errors
/// Returns an error if the insert fails.
pub fn add_word_filter(
    conn: &rusqlite::Connection,
    pattern: &str,
    replacement: &str,
) -> Result<i64> {
    let id: i64 = conn
        .query_row(
            "INSERT INTO word_filters (pattern, replacement)
             VALUES (?1, ?2)
             RETURNING id",
            params![pattern, replacement],
            |r| r.get(0),
        )
        .context("Failed to insert word filter")?;
    Ok(id)
}

/// Remove a word filter by id.
///
/// # Errors
/// Returns an error if the filter does not exist or the delete fails.
pub fn remove_word_filter(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    let n = conn
        .execute("DELETE FROM word_filters WHERE id = ?1", params![id])
        .context("Failed to remove word filter")?;
    if n == 0 {
        anyhow::bail!("Word filter id {id} not found");
    }
    Ok(())
}

// ─── Reports ──────────────────────────────────────────────────────────────────

/// Returns `true` if `reporter_hash` already has an open report for `post_id`.
///
/// This is a best-effort guard. A complete concurrency-safe solution requires a
/// schema-level constraint or a transaction strategy around a canonical key.
///
/// # Errors
/// Returns an error if the query fails.
fn has_reported_post(
    conn: &rusqlite::Connection,
    post_id: i64,
    reporter_hash: &str,
) -> Result<bool> {
    let exists: i64 = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM reports
                 WHERE post_id = ?1
                   AND reporter_hash = ?2
                   AND status = 'open'
             )",
            params![post_id, reporter_hash],
            |r| r.get(0),
        )
        .context("Failed to check for duplicate open report")?;
    Ok(exists != 0)
}

/// File a new report against a post and return its id.
///
/// `thread_id` and `board_id` are derived from the target post to avoid
/// caller-supplied inconsistencies.
///
/// # Errors
/// Returns an error if the post does not exist, if the same reporter already
/// has an open report for the post, or if a database operation fails.
pub fn file_report(
    conn: &rusqlite::Connection,
    post_id: i64,
    _thread_id: i64,
    _board_id: i64,
    reason: &str,
    reporter_hash: &str,
) -> Result<i64> {
    if has_reported_post(conn, post_id, reporter_hash)? {
        anyhow::bail!("Already reported post {post_id}");
    }

    let (thread_id, board_id): (i64, i64) = conn
        .query_row(
            "SELECT thread_id, board_id
             FROM posts
             WHERE id = ?1",
            params![post_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("Failed to resolve report context from post")?;

    let id: i64 = conn
        .query_row(
            "INSERT INTO reports (post_id, thread_id, board_id, reason, reporter_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)
             RETURNING id",
            params![post_id, thread_id, board_id, reason, reporter_hash],
            |r| r.get(0),
        )
        .context("Failed to insert report")?;
    Ok(id)
}

/// Return a default page of open reports enriched with board name and post
/// preview.
///
/// Missing board/post context is tolerated via `LEFT JOIN` so reports do not
/// disappear from moderation views if related rows are removed.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_open_reports(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::ReportWithContext>> {
    get_open_reports_page(conn, DEFAULT_OPEN_QUEUE_LIMIT, 0)
}

/// Return a page of open reports enriched with board name and post preview.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_open_reports_page(
    conn: &rusqlite::Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::ReportWithContext>> {
    let (limit, offset) = normalize_paging(limit, offset)?;
    let mut stmt = conn
        .prepare_cached(
            "SELECT r.id, r.post_id, r.thread_id, r.board_id, r.reason,
                    r.reporter_hash, r.status, r.created_at, r.resolved_at, r.resolved_by,
                    COALESCE(b.short_name, '[missing-board]'),
                    COALESCE(substr(p.body, 1, 120), '[missing-post]'),
                    p.ip_hash
             FROM reports r
             LEFT JOIN boards b ON b.id = r.board_id
             LEFT JOIN posts  p ON p.id = r.post_id
             WHERE r.status = 'open'
             ORDER BY r.created_at DESC, r.id DESC
             LIMIT ?1 OFFSET ?2",
        )
        .context("Failed to prepare `get_open_reports_page`")?;

    let rows = stmt
        .query_map(params![limit, offset], |row| {
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
            let preview: String = row.get(11)?;
            let ip_hash: Option<String> = row.get(12)?;
            Ok(crate::models::ReportWithContext {
                report,
                board_short,
                post_preview: preview,
                post_ip_hash: ip_hash,
            })
        })
        .context("Failed to query open reports")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect open reports")
}

/// Resolve an open report.
///
/// The state transition is only valid from `open` to `resolved`.
///
/// # Errors
/// Returns an error if the report does not exist, is not open, or the update
/// fails.
pub fn resolve_report(conn: &rusqlite::Connection, report_id: i64, admin_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE reports
             SET status = 'resolved',
                 resolved_at = unixepoch(),
                 resolved_by = ?1
             WHERE id = ?2
               AND status = 'open'",
            params![admin_id, report_id],
        )
        .context("Failed to resolve report")?;
    if n == 0 {
        let exists: Option<String> = conn
            .query_row(
                "SELECT status FROM reports WHERE id = ?1",
                params![report_id],
                |r| r.get(0),
            )
            .optional()
            .context("Failed to inspect report state after failed resolve")?;
        match exists {
            None => anyhow::bail!("Report id {report_id} not found"),
            Some(status) => anyhow::bail!("Report id {report_id} is not open (status={status})"),
        }
    }
    Ok(())
}

/// Count currently open reports.
///
/// # Errors
/// Returns an error if the query fails.
#[allow(dead_code)]
pub fn open_report_count(conn: &rusqlite::Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM reports
         WHERE status = 'open'",
        [],
        |r| r.get(0),
    )
    .context("Failed to count open reports")
}

// ─── Moderation log ───────────────────────────────────────────────────────────

/// Append one entry to the moderation action log.
///
/// This call is not automatically coupled to the action it records; callers
/// should wrap mutation + log insertion in the same transaction when atomic
/// auditability matters.
///
/// # Errors
/// Returns an error if the insert fails.
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
    )
    .context("Failed to insert moderation log entry")?;
    Ok(())
}

/// Retrieve a page of moderation log entries, newest first.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_mod_log(
    conn: &rusqlite::Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::ModLogEntry>> {
    let (limit, offset) = normalize_paging(limit, offset)?;
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, admin_id, admin_name, action, target_type, target_id,
                    board_short, detail, created_at
             FROM mod_log
             ORDER BY created_at DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )
        .context("Failed to prepare `get_mod_log`")?;
    let rows = stmt
        .query_map(params![limit, offset], |row| {
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
        })
        .context("Failed to query moderation log")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect moderation log rows")
}

/// Total moderation log entry count.
///
/// # Errors
/// Returns an error if the query fails.
pub fn count_mod_log(conn: &rusqlite::Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM mod_log", [], |r| r.get(0))
        .context("Failed to count moderation log entries")
}

// ─── Ban appeals ──────────────────────────────────────────────────────────────

/// Insert a new ban appeal and return its id.
///
/// This function rate-limits repeat submissions using `has_recent_appeal` and
/// also requires the IP to be currently banned.
///
/// Note: the anti-spam check is still best-effort under concurrency unless the
/// schema enforces a uniqueness policy.
///
/// # Errors
/// Returns an error if the IP is not currently banned, if a recent appeal
/// exists, or if the insert fails.
pub fn file_ban_appeal(conn: &rusqlite::Connection, ip_hash: &str, reason: &str) -> Result<i64> {
    if is_banned(conn, ip_hash)?.is_none() {
        anyhow::bail!("Cannot appeal: no active ban exists for this IP");
    }
    if has_recent_appeal(conn, ip_hash)? {
        anyhow::bail!("A recent ban appeal has already been filed for this IP");
    }

    let id: i64 = conn
        .query_row(
            "INSERT INTO ban_appeals (ip_hash, reason)
             VALUES (?1, ?2)
             RETURNING id",
            params![ip_hash, reason],
            |r| r.get(0),
        )
        .context("Failed to insert ban appeal")?;
    Ok(id)
}

/// Return a default page of open ban appeals, newest first.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_open_ban_appeals(conn: &rusqlite::Connection) -> Result<Vec<crate::models::BanAppeal>> {
    get_open_ban_appeals_page(conn, DEFAULT_OPEN_QUEUE_LIMIT, 0)
}

/// Return a page of open ban appeals, newest first.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_open_ban_appeals_page(
    conn: &rusqlite::Connection,
    limit: i64,
    offset: i64,
) -> Result<Vec<crate::models::BanAppeal>> {
    let (limit, offset) = normalize_paging(limit, offset)?;
    let mut stmt = conn
        .prepare_cached(
            "SELECT id, ip_hash, reason, status, created_at
             FROM ban_appeals
             WHERE status = 'open'
             ORDER BY created_at DESC, id DESC
             LIMIT ?1 OFFSET ?2",
        )
        .context("Failed to prepare `get_open_ban_appeals_page`")?;
    let rows = stmt
        .query_map(params![limit, offset], |r| {
            Ok(crate::models::BanAppeal {
                id: r.get(0)?,
                ip_hash: r.get(1)?,
                reason: r.get(2)?,
                status: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .context("Failed to query open ban appeals")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect open ban appeals")
}

/// Dismiss an open ban appeal.
///
/// The state transition is only valid from `open` to `dismissed`.
///
/// # Errors
/// Returns an error if the appeal does not exist, is not open, or the update
/// fails.
pub fn dismiss_ban_appeal(conn: &rusqlite::Connection, appeal_id: i64) -> Result<()> {
    let n = conn
        .execute(
            "UPDATE ban_appeals
             SET status = 'dismissed'
             WHERE id = ?1
               AND status = 'open'",
            params![appeal_id],
        )
        .context("Failed to dismiss ban appeal")?;
    if n == 0 {
        let exists: Option<String> = conn
            .query_row(
                "SELECT status FROM ban_appeals WHERE id = ?1",
                params![appeal_id],
                |r| r.get(0),
            )
            .optional()
            .context("Failed to inspect appeal state after failed dismiss")?;
        match exists {
            None => anyhow::bail!("Ban appeal id {appeal_id} not found"),
            Some(status) => {
                anyhow::bail!("Ban appeal id {appeal_id} is not open (status={status})")
            }
        }
    }
    Ok(())
}

/// Accept an open appeal and lift the active ban(s) for the appealed IP.
///
/// The `ip_hash` parameter is accepted for backward compatibility with existing
/// call sites but is intentionally ignored. The appealed IP is loaded from the
/// appeal row inside the transaction rather than trusting caller input.
///
/// The state transition is only valid from `open` to `accepted`.
///
/// # Errors
/// Returns an error if the appeal does not exist, is not open, if no active ban
/// exists for the appealed IP, or if any database operation fails.
pub fn accept_ban_appeal(
    conn: &rusqlite::Connection,
    appeal_id: i64,
    _ip_hash: &str,
) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Failed to begin accept-appeal transaction")?;

    let result = (|| -> Result<()> {
        let appeal_ip_hash: String = conn
            .query_row(
                "SELECT ip_hash
                 FROM ban_appeals
                 WHERE id = ?1
                   AND status = 'open'",
                params![appeal_id],
                |r| r.get(0),
            )
            .optional()
            .context("Failed to load open ban appeal")?
            .ok_or_else(|| anyhow::anyhow!("Ban appeal id {appeal_id} not found or not open"))?;

        let now = chrono::Utc::now().timestamp();
        let active_ban_exists: i64 = conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1
                     FROM bans
                     WHERE ip_hash = ?1
                       AND (expires_at IS NULL OR expires_at > ?2)
                 )",
                params![appeal_ip_hash, now],
                |r| r.get(0),
            )
            .context("Failed to verify active ban before appeal acceptance")?;
        if active_ban_exists == 0 {
            anyhow::bail!(
                "Cannot accept ban appeal id {appeal_id}: no active ban exists for the appealed IP"
            );
        }

        let delete_count = conn
            .execute(
                "DELETE FROM bans
                 WHERE ip_hash = ?1
                   AND (expires_at IS NULL OR expires_at > ?2)",
                params![appeal_ip_hash, now],
            )
            .context("Failed to lift ban during appeal acceptance")?;
        if delete_count == 0 {
            anyhow::bail!(
                "Cannot accept ban appeal id {appeal_id}: no active ban rows were removed"
            );
        }

        let update_count = conn
            .execute(
                "UPDATE ban_appeals
                 SET status = 'accepted'
                 WHERE id = ?1
                   AND status = 'open'",
                params![appeal_id],
            )
            .context("Failed to accept ban appeal")?;
        if update_count == 0 {
            anyhow::bail!("Ban appeal id {appeal_id} not found or not open");
        }

        Ok(())
    })();

    match result {
        Ok(()) => {
            if let Err(commit_err) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                Err(commit_err).context("Failed to commit accept-appeal transaction")
            } else {
                Ok(())
            }
        }
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(err)
        }
    }
}

/// Count currently open ban appeals.
///
/// # Errors
/// Returns an error if the query fails.
#[allow(dead_code)]
pub fn open_appeal_count(conn: &rusqlite::Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM ban_appeals
         WHERE status = 'open'",
        [],
        |r| r.get(0),
    )
    .context("Failed to count open ban appeals")
}

/// Check if an appeal has already been filed from this `ip_hash` within the
/// last `24` hours.
///
/// This checks all statuses, not only `open`, as a simple spam throttle.
///
/// # Errors
/// Returns an error if the query fails.
pub fn has_recent_appeal(conn: &rusqlite::Connection, ip_hash: &str) -> Result<bool> {
    let cutoff = chrono::Utc::now().timestamp().saturating_sub(86_400);
    let exists: i64 = conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM ban_appeals
                 WHERE ip_hash = ?1
                   AND created_at >= ?2
             )",
            params![ip_hash, cutoff],
            |r| r.get(0),
        )
        .context("Failed to check for recent ban appeal")?;
    Ok(exists != 0)
}

// ─── IP history ───────────────────────────────────────────────────────────────

/// Count total posts by IP hash across all boards.
///
/// # Errors
/// Returns an error if the query fails.
pub fn count_posts_by_ip_hash(conn: &rusqlite::Connection, ip_hash: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM posts
         WHERE ip_hash = ?1",
        params![ip_hash],
        |r| r.get(0),
    )
    .context("Failed to count posts by IP hash")
}

/// Return paginated posts by IP hash, newest first, across all boards.
///
/// Each post is joined with its board `short_name`.
///
/// # Errors
/// Returns an error if the query fails or paging inputs are invalid.
pub fn get_posts_by_ip_hash(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<(crate::models::Post, String)>> {
    let (limit, offset) = normalize_paging(limit, offset)?;
    let mut stmt = conn
        .prepare_cached(
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
        )
        .context("Failed to prepare `get_posts_by_ip_hash`")?;

    let rows = stmt
        .query_map(params![ip_hash, limit, offset], |row| {
            let post = super::posts::map_post(row)?;
            let board_short: String = row.get(23)?;
            Ok((post, board_short))
        })
        .context("Failed to query posts by IP hash")?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("Failed to collect posts by IP hash")
}

// ─── Database maintenance ─────────────────────────────────────────────────────

/// Run `PRAGMA wal_checkpoint(TRUNCATE)`.
///
/// `SQLite` returns columns in raw order:
/// - col `0`: `busy`
/// - col `1`: `log`
/// - col `2`: `checkpointed`
///
/// This function intentionally returns `(log_pages, checkpointed_pages, busy)`.
///
/// # Errors
/// Returns an error if the `PRAGMA` fails.
pub fn run_wal_checkpoint(conn: &rusqlite::Connection) -> Result<(i64, i64, i64)> {
    let (busy, log_pages, checkpointed) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .context("Failed to run WAL checkpoint")?;
    Ok((log_pages, checkpointed, busy))
}

/// Return the logical main-database size in bytes (`page_count * page_size`).
///
/// This does not include the WAL file.
///
/// # Errors
/// Returns an error if the `PRAGMA`s fail.
pub fn get_db_size_bytes(conn: &rusqlite::Connection) -> Result<i64> {
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .context("Failed to read `PRAGMA page_count`")?;
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .context("Failed to read `PRAGMA page_size`")?;
    Ok(page_count.saturating_mul(page_size))
}

/// Run `VACUUM`.
///
/// `VACUUM` cannot run inside an active transaction and may block for a while on
/// larger databases.
///
/// # Errors
/// Returns an error if the command fails.
pub fn run_vacuum(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("VACUUM")
        .context("Failed to run `VACUUM`")?;
    Ok(())
}
