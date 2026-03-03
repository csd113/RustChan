// db.rs — Database layer.
//
// All SQL lives here. Handlers call these functions via spawn_blocking.
// Schema is created on first run. WAL mode + NORMAL sync reduces SD card writes.
//
// Design: one function per logical operation. No macros, no ORM, plain rusqlite.

use crate::config::CONFIG;
use crate::models::*;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, OptionalExtension};
use std::path::Path;
use tracing::info;

pub type DbPool = Pool<SqliteConnectionManager>;

/// Create the connection pool and run schema migrations.
pub fn init_pool() -> Result<DbPool> {
    let db_path = &CONFIG.database_path;

    // Ensure parent directory exists
    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent)
            .context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        // These pragmas apply to every new connection in the pool.
        // WAL: readers don't block writers; good for concurrent requests.
        // synchronous=NORMAL: safe with WAL, reduces fsync calls → less SD wear.
        // foreign_keys: enforce relational integrity.
        // journal_mode WAL must be set before anything else.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -4096;  -- 4 MiB page cache per connection
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 67108864; -- 64 MiB memory-mapped IO",
        )
    });

    let pool = Pool::builder()
        .max_size(8) // 8 connections; Pi has 4 cores, headroom for bursts
        .build(manager)
        .context("Failed to build database pool")?;

    // Run schema creation on a single connection
    let conn = pool.get().context("Failed to get DB connection")?;
    create_schema(&conn)?;

    info!("Database initialised at {}", db_path);
    Ok(pool)
}

fn create_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Boards table
        CREATE TABLE IF NOT EXISTS boards (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            short_name  TEXT NOT NULL UNIQUE,
            name        TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            nsfw        INTEGER NOT NULL DEFAULT 0,
            max_threads INTEGER NOT NULL DEFAULT 150,
            bump_limit  INTEGER NOT NULL DEFAULT 500,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Threads table (metadata only; OP content is in posts)
        CREATE TABLE IF NOT EXISTS threads (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            board_id    INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            subject     TEXT,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            bumped_at   INTEGER NOT NULL DEFAULT (unixepoch()),
            locked      INTEGER NOT NULL DEFAULT 0,
            sticky      INTEGER NOT NULL DEFAULT 0,
            reply_count INTEGER NOT NULL DEFAULT 0
        );

        -- Posts table (OP and replies)
        CREATE TABLE IF NOT EXISTS posts (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            board_id       INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            name           TEXT NOT NULL DEFAULT 'Anonymous',
            tripcode       TEXT,
            subject        TEXT,
            body           TEXT NOT NULL,
            body_html      TEXT NOT NULL,
            ip_hash        TEXT NOT NULL,
            file_path      TEXT,
            file_name      TEXT,
            file_size      INTEGER,
            thumb_path     TEXT,
            mime_type      TEXT,
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            deletion_token TEXT NOT NULL,
            is_op          INTEGER NOT NULL DEFAULT 0
        );

        -- Admin users
        CREATE TABLE IF NOT EXISTS admin_users (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            username      TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Admin sessions (cookie-based)
        CREATE TABLE IF NOT EXISTS admin_sessions (
            id         TEXT PRIMARY KEY,
            admin_id   INTEGER NOT NULL REFERENCES admin_users(id) ON DELETE CASCADE,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            expires_at INTEGER NOT NULL
        );

        -- IP bans (stored as SHA-256 hashes, never raw IPs)
        CREATE TABLE IF NOT EXISTS bans (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            ip_hash    TEXT NOT NULL,
            reason     TEXT,
            expires_at INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Word filters
        CREATE TABLE IF NOT EXISTS word_filters (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            pattern     TEXT NOT NULL,
            replacement TEXT NOT NULL
        );

        -- Indices for common query patterns
        CREATE INDEX IF NOT EXISTS idx_threads_board_sticky_bumped
            ON threads(board_id, sticky DESC, bumped_at DESC);
        CREATE INDEX IF NOT EXISTS idx_posts_thread
            ON posts(thread_id, created_at ASC);
        CREATE INDEX IF NOT EXISTS idx_posts_board
            ON posts(board_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_bans_ip
            ON bans(ip_hash);
        CREATE INDEX IF NOT EXISTS idx_sessions_expires
            ON admin_sessions(expires_at);
        ",
    )
    .context("Schema creation failed")?;
    Ok(())
}

// ─── Board queries ────────────────────────────────────────────────────────────

pub fn get_all_boards(conn: &rusqlite::Connection) -> Result<Vec<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit, created_at
         FROM boards ORDER BY id ASC",
    )?;
    let boards = stmt
        .query_map([], map_board)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(boards)
}

/// Like get_all_boards but also returns live thread count for each board.
pub fn get_all_boards_with_stats(conn: &rusqlite::Connection) -> Result<Vec<crate::models::BoardStats>> {
    let boards = get_all_boards(conn)?;
    let mut out = Vec::with_capacity(boards.len());
    for board in boards {
        let thread_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
            params![board.id],
            |r| r.get(0),
        )?;
        out.push(crate::models::BoardStats { board, thread_count });
    }
    Ok(out)
}

pub fn get_board_by_short(conn: &rusqlite::Connection, short: &str) -> Result<Option<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit, created_at
         FROM boards WHERE short_name = ?1",
    )?;
    Ok(stmt.query_row(params![short], map_board).optional()?)
}

pub fn create_board(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO boards (short_name, name, description, nsfw) VALUES (?1, ?2, ?3, ?4)",
        params![short, name, description, nsfw as i32],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_board(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE boards SET name=?1, description=?2, nsfw=?3 WHERE id=?4",
        params![name, description, nsfw as i32, id],
    )?;
    Ok(())
}

pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM boards WHERE id=?1", params![id])?;
    Ok(())
}

// ─── Thread queries ───────────────────────────────────────────────────────────

/// Get paginated threads for a board with OP preview data.
pub fn get_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    // Sticky threads float to top, then sorted by most recent bump.
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.board_id = ?1
         ORDER BY t.sticky DESC, t.bumped_at DESC
         LIMIT ?2 OFFSET ?3",
    )?;

    let threads = stmt
        .query_map(params![board_id, limit, offset], |row| {
            Ok(Thread {
                id: row.get(0)?,
                board_id: row.get(1)?,
                subject: row.get(2)?,
                created_at: row.get(3)?,
                bumped_at: row.get(4)?,
                locked: row.get::<_, i32>(5)? != 0,
                sticky: row.get::<_, i32>(6)? != 0,
                reply_count: row.get(7)?,
                op_body: row.get(8)?,
                op_file: row.get(9)?,
                op_thumb: row.get(10)?,
                op_name: row.get(11)?,
                op_tripcode: row.get(12)?,
                op_id: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

pub fn count_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
        params![board_id],
        |r| r.get(0),
    )?)
}

pub fn get_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Option<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.id = ?1",
    )?;
    Ok(stmt
        .query_row(params![thread_id], |row| {
            Ok(Thread {
                id: row.get(0)?,
                board_id: row.get(1)?,
                subject: row.get(2)?,
                created_at: row.get(3)?,
                bumped_at: row.get(4)?,
                locked: row.get::<_, i32>(5)? != 0,
                sticky: row.get::<_, i32>(6)? != 0,
                reply_count: row.get(7)?,
                op_body: row.get(8)?,
                op_file: row.get(9)?,
                op_thumb: row.get(10)?,
                op_name: row.get(11)?,
                op_tripcode: row.get(12)?,
                op_id: row.get(13)?,
            })
        })
        .optional()?)
}

/// Create a thread record, returns new thread_id
pub fn create_thread(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO threads (board_id, subject) VALUES (?1, ?2)",
        params![board_id, subject],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn bump_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE threads SET bumped_at = unixepoch(), reply_count = reply_count + 1
         WHERE id = ?1",
        params![thread_id],
    )?;
    Ok(())
}

pub fn set_thread_sticky(conn: &rusqlite::Connection, thread_id: i64, sticky: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET sticky = ?1 WHERE id = ?2",
        params![sticky as i32, thread_id],
    )?;
    Ok(())
}

pub fn set_thread_locked(conn: &rusqlite::Connection, thread_id: i64, locked: bool) -> Result<()> {
    conn.execute(
        "UPDATE threads SET locked = ?1 WHERE id = ?2",
        params![locked as i32, thread_id],
    )?;
    Ok(())
}

pub fn delete_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<String>> {
    // Collect file paths before deletion (for filesystem cleanup)
    let mut stmt = conn
        .prepare("SELECT file_path, thumb_path FROM posts WHERE thread_id = ?1")?;
    let paths: Vec<(Option<String>, Option<String>)> = stmt
        .query_map(params![thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut all_paths = Vec::new();
    for (f, t) in paths {
        if let Some(p) = f { all_paths.push(p); }
        if let Some(p) = t { all_paths.push(p); }
    }

    conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
    Ok(all_paths)
}

/// Prune oldest non-sticky threads beyond the board limit.
pub fn prune_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<Vec<String>> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM threads WHERE board_id = ?1 AND sticky = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let x = stmt.query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?; x
    };

    let mut all_paths = Vec::new();
    for id in ids {
        let mut paths = delete_thread(conn, id)?;
        all_paths.append(&mut paths);
    }
    Ok(all_paths)
}

// ─── Post queries ─────────────────────────────────────────────────────────────

pub fn get_posts_for_thread(
    conn: &rusqlite::Connection,
    thread_id: i64,
) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op
         FROM posts WHERE thread_id = ?1 ORDER BY created_at ASC, id ASC",
    )?;
    let posts = stmt
        .query_map(params![thread_id], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Get last N posts for a thread (for board index preview)
pub fn get_preview_posts(
    conn: &rusqlite::Connection,
    thread_id: i64,
    n: i64,
) -> Result<Vec<Post>> {
    // Subquery gets the last N, outer query re-orders ascending for display
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op
         FROM (
             SELECT * FROM posts WHERE thread_id = ?1 AND is_op = 0
             ORDER BY created_at DESC, id DESC LIMIT ?2
         ) ORDER BY created_at ASC, id ASC",
    )?;
    let posts = stmt
        .query_map(params![thread_id, n], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

pub fn create_post(conn: &rusqlite::Connection, p: &NewPost) -> Result<i64> {
    conn.execute(
        "INSERT INTO posts
         (thread_id, board_id, name, tripcode, subject, body, body_html,
          ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
          deletion_token, is_op)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            p.thread_id,
            p.board_id,
            p.name,
            p.tripcode,
            p.subject,
            p.body,
            p.body_html,
            p.ip_hash,
            p.file_path,
            p.file_name,
            p.file_size,
            p.thumb_path,
            p.mime_type,
            p.deletion_token,
            p.is_op as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Option<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op
         FROM posts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![post_id], map_post).optional()?)
}

/// Delete a post by id; returns file paths for cleanup.
pub fn delete_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    if let Some(post) = get_post(conn, post_id)? {
        if let Some(p) = post.file_path { paths.push(p); }
        if let Some(p) = post.thumb_path { paths.push(p); }
    }
    conn.execute("DELETE FROM posts WHERE id = ?1", params![post_id])?;
    Ok(paths)
}

pub fn verify_deletion_token(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
) -> Result<bool> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT deletion_token FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(stored.map(|s| s == token).unwrap_or(false))
}

/// Full-text search across post bodies
pub fn search_posts(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Post>> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn.prepare(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op
         FROM posts WHERE board_id = ?1 AND body LIKE ?2 ESCAPE '\\'
         ORDER BY created_at DESC LIMIT ?3 OFFSET ?4",
    )?;
    let posts = stmt
        .query_map(params![board_id, pattern, limit, offset], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

pub fn count_search_results(
    conn: &rusqlite::Connection,
    board_id: i64,
    query: &str,
) -> Result<i64> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE board_id = ?1 AND body LIKE ?2 ESCAPE '\\'",
        params![board_id, pattern],
        |r| r.get(0),
    )?)
}

// ─── Admin / session queries ──────────────────────────────────────────────────

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

pub fn update_admin_password(conn: &rusqlite::Connection, username: &str, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE admin_users SET password_hash = ?1 WHERE username = ?2",
        params![hash, username],
    )?;
    Ok(())
}

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

pub fn get_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Option<AdminSession>> {
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

/// Clean up expired sessions (called periodically)
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
    // reason column is nullable TEXT, so r.get(0) returns Option<String>.
    // .optional() wraps the whole row result: None = no ban row found.
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

// ─── Row mapping helpers ──────────────────────────────────────────────────────

fn map_board(row: &rusqlite::Row<'_>) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get(0)?,
        short_name: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        nsfw: row.get::<_, i32>(4)? != 0,
        max_threads: row.get(5)?,
        bump_limit: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn map_post(row: &rusqlite::Row<'_>) -> rusqlite::Result<Post> {
    Ok(Post {
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
    })
}

/// Data needed to insert a new post
pub struct NewPost {
    pub thread_id: i64,
    pub board_id: i64,
    pub name: String,
    pub tripcode: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub body_html: String,
    pub ip_hash: String,
    pub file_path: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<i64>,
    pub thumb_path: Option<String>,
    pub mime_type: Option<String>,
    pub deletion_token: String,
    pub is_op: bool,
}

/// List all admin users (for CLI tooling)
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, created_at FROM admin_users ORDER BY id ASC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String, i64)>>>()?;
    Ok(rows)
}
