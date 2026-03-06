// db.rs — Database layer.
//
// All SQL lives here. Handlers call these functions via spawn_blocking.
// Schema is created on first run. WAL mode + NORMAL sync reduces disk writes.
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
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        // These pragmas apply to every new connection in the pool.
        // WAL: readers don't block writers; good for concurrent requests.
        // synchronous=NORMAL: safe with WAL, reduces fsync calls.
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
        // FIX[LOW-4]: Removed hardware-specific comment. Pool size of 8 gives
        // enough headroom for concurrent requests without exhausting SQLite's
        // WAL-mode write serialisation.
        .max_size(8)
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
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            short_name    TEXT NOT NULL UNIQUE,
            name          TEXT NOT NULL,
            description   TEXT NOT NULL DEFAULT '',
            nsfw          INTEGER NOT NULL DEFAULT 0,
            max_threads   INTEGER NOT NULL DEFAULT 150,
            bump_limit    INTEGER NOT NULL DEFAULT 500,
            allow_video   INTEGER NOT NULL DEFAULT 1,
            allow_tripcodes INTEGER NOT NULL DEFAULT 1,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
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
            archived    INTEGER NOT NULL DEFAULT 0,
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
            file_path        TEXT,
            file_name        TEXT,
            file_size        INTEGER,
            thumb_path       TEXT,
            mime_type        TEXT,
            created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
            deletion_token   TEXT NOT NULL,
            is_op            INTEGER NOT NULL DEFAULT 0,
            audio_file_path  TEXT,
            audio_file_name  TEXT,
            audio_file_size  INTEGER,
            audio_mime_type  TEXT
        );

        -- File deduplication table (SHA-256 hash → existing file paths)
        CREATE TABLE IF NOT EXISTS file_hashes (
            sha256     TEXT PRIMARY KEY,
            file_path  TEXT NOT NULL,
            thumb_path TEXT NOT NULL,
            mime_type  TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
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

        -- Polls (one per thread, OP only)
        CREATE TABLE IF NOT EXISTS polls (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id  INTEGER NOT NULL UNIQUE REFERENCES threads(id) ON DELETE CASCADE,
            question   TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Poll options
        CREATE TABLE IF NOT EXISTS poll_options (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            poll_id  INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
            text     TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0
        );

        -- Poll votes — one per (poll, ip_hash) pair
        CREATE TABLE IF NOT EXISTS poll_votes (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            poll_id   INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
            option_id INTEGER NOT NULL REFERENCES poll_options(id) ON DELETE CASCADE,
            ip_hash   TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(poll_id, ip_hash)
        );

        -- Site-wide key/value settings (admin-configurable at runtime)
        CREATE TABLE IF NOT EXISTS site_settings (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL
        );

        -- User-filed reports
        CREATE TABLE IF NOT EXISTS reports (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            post_id        INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            thread_id      INTEGER NOT NULL,
            board_id       INTEGER NOT NULL,
            reason         TEXT NOT NULL DEFAULT '',
            reporter_hash  TEXT NOT NULL,
            status         TEXT NOT NULL DEFAULT 'open',
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            resolved_at    INTEGER,
            resolved_by    INTEGER
        );

        -- Moderation action log
        CREATE TABLE IF NOT EXISTS mod_log (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            admin_id     INTEGER NOT NULL,
            admin_name   TEXT NOT NULL,
            action       TEXT NOT NULL,
            target_type  TEXT NOT NULL DEFAULT '',
            target_id    INTEGER,
            board_short  TEXT NOT NULL DEFAULT '',
            detail       TEXT NOT NULL DEFAULT '',
            created_at   INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Background job queue (persistent across restarts)
        CREATE TABLE IF NOT EXISTS background_jobs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_type    TEXT NOT NULL,
            payload     TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending',  -- pending|running|done|failed
            priority    INTEGER NOT NULL DEFAULT 0,
            attempts    INTEGER NOT NULL DEFAULT 0,
            last_error  TEXT,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
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
        CREATE INDEX IF NOT EXISTS idx_file_hashes
            ON file_hashes(sha256);
        CREATE INDEX IF NOT EXISTS idx_jobs_pending
            ON background_jobs(status, priority DESC, created_at ASC);
        CREATE INDEX IF NOT EXISTS idx_reports_status
            ON reports(status, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_mod_log_created
            ON mod_log(created_at DESC);
        ",
    )
    .context("Schema creation failed")?;

    // Additive migrations for existing databases that pre-date new columns.
    // SQLite returns an error on duplicate column — we just ignore it.
    let migrations: &[&str] = &[
        "ALTER TABLE boards ADD COLUMN allow_video    INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE boards ADD COLUMN allow_tripcodes INTEGER NOT NULL DEFAULT 1",
        // Per-board image and audio toggles (Part 4)
        "ALTER TABLE boards ADD COLUMN allow_images  INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE boards ADD COLUMN allow_audio   INTEGER NOT NULL DEFAULT 0",
        // MediaType column on posts for explicit classification (Part 3)
        "ALTER TABLE posts ADD COLUMN media_type TEXT",
        "ALTER TABLE posts ADD COLUMN audio_file_path TEXT",
        "ALTER TABLE posts ADD COLUMN audio_file_name TEXT",
        "ALTER TABLE posts ADD COLUMN audio_file_size INTEGER",
        "ALTER TABLE posts ADD COLUMN audio_mime_type TEXT",
        // Poll tables added later — CREATE TABLE IF NOT EXISTS handles this gracefully
        // Post editing support: timestamp of last edit (NULL = never edited)
        "ALTER TABLE posts ADD COLUMN edited_at INTEGER",
        // Background job queue — added to CREATE TABLE IF NOT EXISTS above,
        // but the index must also exist on databases created before this version.
        "CREATE INDEX IF NOT EXISTS idx_jobs_pending ON background_jobs(status, priority DESC, created_at ASC)",
        // Reports and mod_log — CREATE TABLE IF NOT EXISTS handles new installs;
        // these indexes ensure they exist on pre-existing databases too.
        "CREATE INDEX IF NOT EXISTS idx_reports_status ON reports(status, created_at DESC)",
        "CREATE INDEX IF NOT EXISTS idx_mod_log_created ON mod_log(created_at DESC)",
        // v1.0.8: archive column on threads (non-destructive prune)
        "ALTER TABLE threads ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
        "CREATE INDEX IF NOT EXISTS idx_threads_archived ON threads(board_id, archived, bumped_at DESC)",
        // v1.0.9: per-board edit window; 0 = use 300s when editing enabled
        "ALTER TABLE boards ADD COLUMN edit_window_secs INTEGER NOT NULL DEFAULT 0",
        // v1.0.9: per-board editing toggle (off by default)
        "ALTER TABLE boards ADD COLUMN allow_editing INTEGER NOT NULL DEFAULT 0",
        // v1.0.9: per-board archive toggle (on by default for existing boards)
        "ALTER TABLE boards ADD COLUMN allow_archive INTEGER NOT NULL DEFAULT 1",
    ];
    for sql in migrations {
        let _ = conn.execute(sql, []); // ignore "duplicate column" errors
    }

    // Backfill media_type for existing posts that pre-date the column.
    // We infer the type from the file extension embedded in file_path.
    // This is non-destructive: posts without a file are left NULL.
    let _ = conn.execute_batch(
        "UPDATE posts
         SET media_type = CASE
             WHEN file_path LIKE '%.jpg'  OR file_path LIKE '%.jpeg' OR
                  file_path LIKE '%.png'  OR file_path LIKE '%.gif'  OR
                  file_path LIKE '%.webp' THEN 'image'
             WHEN file_path LIKE '%.mp4'  OR file_path LIKE '%.webm' THEN 'video'
             WHEN file_path LIKE '%.mp3'  OR file_path LIKE '%.ogg'  OR
                  file_path LIKE '%.flac' OR file_path LIKE '%.wav'  OR
                  file_path LIKE '%.m4a'  OR file_path LIKE '%.aac'  OR
                  file_path LIKE '%.opus' THEN 'audio'
             ELSE NULL
         END
         WHERE media_type IS NULL AND file_path IS NOT NULL;",
    );

    Ok(())
}

// ─── Site settings ───────────────────────────────────────────────────────────

/// Read a site-wide setting by key. Returns None if the key has never been set.
pub fn get_site_setting(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT value FROM site_settings WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(result)
}

/// Write (upsert) a site-wide setting.
pub fn set_site_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO site_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Convenience: read the collapsible-greentext toggle (default: false).
/// Returns the admin-configured site name, or falls back to CONFIG.forum_name.
pub fn get_site_name(conn: &rusqlite::Connection) -> String {
    get_site_setting(conn, "site_name")
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| crate::config::CONFIG.forum_name.clone())
}

pub fn get_collapse_greentext(conn: &rusqlite::Connection) -> bool {
    get_site_setting(conn, "collapse_greentext")
        .ok()
        .flatten()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ─── Board queries ────────────────────────────────────────────────────────────

pub fn get_all_boards(conn: &rusqlite::Connection) -> Result<Vec<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                allow_editing, allow_archive, created_at
         FROM boards ORDER BY id ASC",
    )?;
    let boards = stmt
        .query_map([], map_board)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(boards)
}

/// Like get_all_boards but also returns live thread count for each board.
pub fn get_all_boards_with_stats(
    conn: &rusqlite::Connection,
) -> Result<Vec<crate::models::BoardStats>> {
    let boards = get_all_boards(conn)?;
    let mut out = Vec::with_capacity(boards.len());
    for board in boards {
        let thread_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
            params![board.id],
            |r| r.get(0),
        )?;
        out.push(crate::models::BoardStats {
            board,
            thread_count,
        });
    }
    Ok(out)
}

pub fn get_board_by_short(conn: &rusqlite::Connection, short: &str) -> Result<Option<Board>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, short_name, name, description, nsfw, max_threads, bump_limit,
                allow_images, allow_video, allow_audio, allow_tripcodes, edit_window_secs,
                allow_editing, allow_archive, created_at
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
    // New boards default to images and video enabled; audio off by default.
    conn.execute(
        "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
         VALUES (?1, ?2, ?3, ?4, 1, 1, 0)",
        params![short, name, description, nsfw as i32],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Create a board with explicit per-media-type toggles.
/// Used by the CLI `--no-images / --no-videos / --no-audio` flags.
#[allow(clippy::too_many_arguments)]
pub fn create_board_with_media_flags(
    conn: &rusqlite::Connection,
    short: &str,
    name: &str,
    description: &str,
    nsfw: bool,
    allow_images: bool,
    allow_video: bool,
    allow_audio: bool,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO boards (short_name, name, description, nsfw, allow_images, allow_video, allow_audio)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            short, name, description, nsfw as i32,
            allow_images as i32, allow_video as i32, allow_audio as i32,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(dead_code)]
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

/// Update all per-board settings from the admin panel.
#[allow(clippy::too_many_arguments)]
pub fn update_board_settings(
    conn: &rusqlite::Connection,
    id: i64,
    name: &str,
    description: &str,
    nsfw: bool,
    bump_limit: i64,
    max_threads: i64,
    allow_images: bool,
    allow_video: bool,
    allow_audio: bool,
    allow_tripcodes: bool,
    edit_window_secs: i64,
    allow_editing: bool,
    allow_archive: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE boards SET name=?1, description=?2, nsfw=?3,
         bump_limit=?4, max_threads=?5,
         allow_images=?6, allow_video=?7, allow_audio=?8, allow_tripcodes=?9,
         edit_window_secs=?10, allow_editing=?11, allow_archive=?12
         WHERE id=?13",
        params![
            name,
            description,
            nsfw as i32,
            bump_limit,
            max_threads,
            allow_images as i32,
            allow_video as i32,
            allow_audio as i32,
            allow_tripcodes as i32,
            edit_window_secs,
            allow_editing as i32,
            allow_archive as i32,
            id,
        ],
    )?;
    Ok(())
}

pub fn delete_board(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    // Collect every file path that belongs to this board before deletion.
    // The CASCADE on boards→threads→posts handles DB row removal, but the
    // on-disk files must be cleaned up by the caller.
    let mut stmt = conn.prepare(
        "SELECT p.file_path, p.thumb_path
         FROM posts p
         JOIN threads t ON p.thread_id = t.id
         WHERE t.board_id = ?1",
    )?;
    let pairs: Vec<(Option<String>, Option<String>)> = stmt
        .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut paths = Vec::new();
    for (f, t) in pairs {
        if let Some(p) = f {
            paths.push(p);
        }
        if let Some(p) = t {
            paths.push(p);
        }
    }

    // Cascade deletes threads, posts, polls, etc.
    conn.execute("DELETE FROM boards WHERE id = ?1", params![id])?;
    Ok(paths)
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
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 AND (p.media_type = 'image'
                      OR (p.media_type IS NULL AND (
                          p.file_path LIKE '%.jpg' OR p.file_path LIKE '%.jpeg' OR
                          p.file_path LIKE '%.png' OR p.file_path LIKE '%.gif' OR
                          p.file_path LIKE '%.webp'
                      ))
                 )) AS image_count
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.board_id = ?1 AND t.archived = 0
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
                archived: row.get::<_, i32>(14)? != 0,
                image_count: row.get(15)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

pub fn count_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 0",
        params![board_id],
        |r| r.get(0),
    )?)
}

pub fn get_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Option<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 AND (p.media_type = 'image'
                      OR (p.media_type IS NULL AND (
                          p.file_path LIKE '%.jpg' OR p.file_path LIKE '%.jpeg' OR
                          p.file_path LIKE '%.png' OR p.file_path LIKE '%.gif' OR
                          p.file_path LIKE '%.webp'
                      ))
                 )) AS image_count
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
                archived: row.get::<_, i32>(14)? != 0,
                image_count: row.get(15)?,
            })
        })
        .optional()?)
}

/// Create a thread AND its OP post atomically in a single transaction.
///
/// FIX[MEDIUM-3]: The previous design had two separate DB calls — create_thread
/// followed by create_post — with no transaction. A crash between the two calls
/// left an orphaned thread with no OP post, causing all board-listing queries
/// (which JOIN on is_op=1) to silently skip the thread forever.
///
/// This function is the single entry point for thread creation and wraps both
/// operations in a transaction, guaranteeing the invariant that every thread
/// row has exactly one corresponding post with is_op=1.
///
/// Returns (thread_id, post_id).
pub fn create_thread_with_op(
    conn: &rusqlite::Connection,
    board_id: i64,
    subject: Option<&str>,
    post: &NewPost,
) -> Result<(i64, i64)> {
    // Begin an exclusive transaction so no other write can interleave.
    conn.execute("BEGIN IMMEDIATE", [])?;

    let result = (|| -> Result<(i64, i64)> {
        conn.execute(
            "INSERT INTO threads (board_id, subject) VALUES (?1, ?2)",
            params![board_id, subject],
        )?;
        let thread_id = conn.last_insert_rowid();

        // Bind the OP post to the newly-created thread
        let post_with_thread = NewPost {
            thread_id,
            is_op: true,
            ..post.clone()
        };
        let post_id = create_post_inner(conn, &post_with_thread)?;

        Ok((thread_id, post_id))
    })();

    match result {
        Ok(ids) => {
            conn.execute("COMMIT", [])?;
            Ok(ids)
        }
        Err(e) => {
            let _ = conn.execute("ROLLBACK", []);
            Err(e)
        }
    }
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
        .prepare("SELECT file_path, thumb_path, audio_file_path FROM posts WHERE thread_id = ?1")?;
    let paths: Vec<(Option<String>, Option<String>, Option<String>)> = stmt
        .query_map(params![thread_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut all_paths = Vec::new();
    for (f, t, a) in paths {
        if let Some(p) = f {
            all_paths.push(p);
        }
        if let Some(p) = t {
            all_paths.push(p);
        }
        if let Some(p) = a {
            all_paths.push(p);
        }
    }

    conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
    Ok(all_paths)
}

/// Archive oldest non-sticky threads that exceed the board's max_threads limit.
/// Archived threads are locked and marked read-only instead of deleted, so
/// their content remains accessible via /{board}/archive.
/// Returns the count of threads archived (no file deletion occurs).
pub fn archive_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let ids = stmt
            .query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let count = ids.len();
    for id in ids {
        conn.execute(
            "UPDATE threads SET archived = 1, locked = 1 WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(count)
}

/// Hard-delete oldest non-sticky, non-archived threads that exceed max_threads.
/// Used when a board has archiving disabled — threads are permanently removed.
/// Returns the count of threads deleted.
pub fn prune_old_threads(conn: &rusqlite::Connection, board_id: i64, max: i64) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM threads
             WHERE board_id = ?1 AND sticky = 0 AND archived = 0
             ORDER BY bumped_at DESC LIMIT -1 OFFSET ?2",
        )?;
        let ids = stmt
            .query_map(params![board_id, max], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let count = ids.len();
    for id in ids {
        conn.execute("DELETE FROM threads WHERE id = ?1", params![id])?;
    }
    Ok(count)
}
pub fn get_archived_threads_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Thread>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.board_id, t.subject, t.created_at, t.bumped_at,
                t.locked, t.sticky, t.reply_count,
                op.body, op.file_path, op.thumb_path, op.name, op.tripcode, op.id,
                t.archived,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id
                 AND p.file_path IS NOT NULL
                 AND (p.media_type = 'image'
                      OR (p.media_type IS NULL AND (
                          p.file_path LIKE '%.jpg' OR p.file_path LIKE '%.jpeg' OR
                          p.file_path LIKE '%.png' OR p.file_path LIKE '%.gif' OR
                          p.file_path LIKE '%.webp'
                      ))
                 )) AS image_count
         FROM threads t
         JOIN posts op ON op.thread_id = t.id AND op.is_op = 1
         WHERE t.board_id = ?1 AND t.archived = 1
         ORDER BY t.bumped_at DESC
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
                archived: row.get::<_, i32>(14)? != 0,
                image_count: row.get(15)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(threads)
}

/// Count archived threads for a board (used for archive pagination).
pub fn count_archived_threads_for_board(conn: &rusqlite::Connection, board_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE board_id = ?1 AND archived = 1",
        params![board_id],
        |r| r.get(0),
    )?)
}

// ─── Post queries ─────────────────────────────────────────────────────────────

pub fn get_posts_for_thread(conn: &rusqlite::Connection, thread_id: i64) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE thread_id = ?1 ORDER BY created_at ASC, id ASC",
    )?;
    let posts = stmt
        .query_map(params![thread_id], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Fetch posts in `thread_id` whose id is strictly greater than `since_id`.
/// Returns them oldest-first. Used by the thread auto-update polling endpoint.
pub fn get_new_posts_since(
    conn: &rusqlite::Connection,
    thread_id: i64,
    since_id: i64,
) -> Result<Vec<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE thread_id = ?1 AND id > ?2
         ORDER BY id ASC
         LIMIT 100",
    )?;
    let posts = stmt
        .query_map(params![thread_id, since_id], map_post)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(posts)
}

/// Get last N posts for a thread (for board index preview)
pub fn get_preview_posts(conn: &rusqlite::Connection, thread_id: i64, n: i64) -> Result<Vec<Post>> {
    // Subquery gets the last N, outer query re-orders ascending for display
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
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

/// Internal post insertion — used by create_thread_with_op and create_reply.
fn create_post_inner(conn: &rusqlite::Connection, p: &NewPost) -> Result<i64> {
    conn.execute(
        "INSERT INTO posts
         (thread_id, board_id, name, tripcode, subject, body, body_html,
          ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
          deletion_token, is_op, media_type,
          audio_file_path, audio_file_name, audio_file_size, audio_mime_type)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
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
            p.media_type,
            p.audio_file_path,
            p.audio_file_name,
            p.audio_file_size,
            p.audio_mime_type,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn create_post(conn: &rusqlite::Connection, p: &NewPost) -> Result<i64> {
    create_post_inner(conn, p)
}

pub fn get_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Option<Post>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, thread_id, board_id, name, tripcode, subject, body, body_html,
                ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
         FROM posts WHERE id = ?1",
    )?;
    Ok(stmt.query_row(params![post_id], map_post).optional()?)
}

/// Delete a post by id; returns file paths for cleanup.
pub fn delete_post(conn: &rusqlite::Connection, post_id: i64) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    if let Some(post) = get_post(conn, post_id)? {
        if let Some(p) = post.file_path {
            paths.push(p);
        }
        if let Some(p) = post.thumb_path {
            paths.push(p);
        }
        if let Some(p) = post.audio_file_path {
            paths.push(p);
        }
    }
    conn.execute("DELETE FROM posts WHERE id = ?1", params![post_id])?;
    Ok(paths)
}

/// FIX[LOW-3]: Use constant-time byte comparison to prevent timing attacks on
/// deletion token verification. Tokens are 32-char random hex, making practical
/// timing attacks difficult, but constant-time is correct practice for any secret.
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

    Ok(stored
        .map(|s| constant_time_eq(s.as_bytes(), token.as_bytes()))
        .unwrap_or(false))
}

/// Edit a post's body, verified against the deletion token and a per-board edit window.
///
/// `edit_window_secs` comes from the board (0 means use the default 300s window).
/// The caller is responsible for checking `board.allow_editing` before calling this.
/// Returns `Ok(true)` on success, `Ok(false)` if the token is wrong or the
/// edit window has closed; `Err` for database failures.
pub fn edit_post(
    conn: &rusqlite::Connection,
    post_id: i64,
    token: &str,
    new_body: &str,
    new_body_html: &str,
    edit_window_secs: i64,
) -> Result<bool> {
    // 0 means "use the default window of 300 seconds"
    let window = if edit_window_secs <= 0 {
        300
    } else {
        edit_window_secs
    };

    // Verify token first (constant-time)
    if !verify_deletion_token(conn, post_id, token)? {
        return Ok(false);
    }

    // Check edit window
    let created_at: Option<i64> = conn
        .query_row(
            "SELECT created_at FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get(0),
        )
        .optional()?;

    let created_at = match created_at {
        Some(t) => t,
        None => return Ok(false),
    };

    let now = chrono::Utc::now().timestamp();
    if now - created_at > window {
        return Ok(false);
    }

    conn.execute(
        "UPDATE posts SET body = ?1, body_html = ?2, edited_at = ?3 WHERE id = ?4",
        params![new_body, new_body_html, now, post_id],
    )?;

    Ok(true)
}

/// Constant-time byte slice comparison to prevent timing side-channel attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // XOR all bytes; any difference leaves a non-zero accumulator.
    let diff = a
        .iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
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
                created_at, deletion_token, is_op, media_type,
                audio_file_path, audio_file_name, audio_file_size, audio_mime_type,
                edited_at
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

// ─── File deduplication ───────────────────────────────────────────────────────

pub struct CachedFile {
    pub file_path: String,
    pub thumb_path: String,
    pub mime_type: String,
}

/// Look up an existing upload by its SHA-256 hash.
pub fn find_file_by_hash(conn: &rusqlite::Connection, sha256: &str) -> Result<Option<CachedFile>> {
    let mut stmt = conn.prepare_cached(
        "SELECT file_path, thumb_path, mime_type FROM file_hashes WHERE sha256 = ?1",
    )?;
    Ok(stmt
        .query_row(params![sha256], |r| {
            Ok(CachedFile {
                file_path: r.get(0)?,
                thumb_path: r.get(1)?,
                mime_type: r.get(2)?,
            })
        })
        .optional()?)
}

/// Record a newly saved upload in the deduplication table.
pub fn record_file_hash(
    conn: &rusqlite::Connection,
    sha256: &str,
    file_path: &str,
    thumb_path: &str,
    mime_type: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO file_hashes (sha256, file_path, thumb_path, mime_type)
         VALUES (?1, ?2, ?3, ?4)",
        params![sha256, file_path, thumb_path, mime_type],
    )?;
    Ok(())
}

// ─── Poll queries ─────────────────────────────────────────────────────────────

/// Create a poll with its options atomically.
pub fn create_poll(
    conn: &rusqlite::Connection,
    thread_id: i64,
    question: &str,
    options: &[String],
    expires_at: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO polls (thread_id, question, expires_at) VALUES (?1, ?2, ?3)",
        params![thread_id, question, expires_at],
    )?;
    let poll_id = conn.last_insert_rowid();
    for (i, text) in options.iter().enumerate() {
        conn.execute(
            "INSERT INTO poll_options (poll_id, text, position) VALUES (?1, ?2, ?3)",
            params![poll_id, text, i as i64],
        )?;
    }
    Ok(poll_id)
}

/// Fetch the full poll for a thread including vote counts and the user's choice.
pub fn get_poll_for_thread(
    conn: &rusqlite::Connection,
    thread_id: i64,
    ip_hash: &str,
) -> Result<Option<crate::models::PollData>> {
    let now = chrono::Utc::now().timestamp();

    let poll_row = conn
        .query_row(
            "SELECT id, thread_id, question, expires_at, created_at FROM polls WHERE thread_id = ?1",
            params![thread_id],
            |r| {
                Ok(crate::models::Poll {
                    id: r.get(0)?,
                    thread_id: r.get(1)?,
                    question: r.get(2)?,
                    expires_at: r.get(3)?,
                    created_at: r.get(4)?,
                })
            },
        )
        .optional()?;

    let poll = match poll_row {
        Some(p) => p,
        None => return Ok(None),
    };

    // Options with live vote counts
    let mut stmt = conn.prepare_cached(
        "SELECT po.id, po.poll_id, po.text, po.position,
                COUNT(pv.id) as vote_count
         FROM poll_options po
         LEFT JOIN poll_votes pv ON pv.option_id = po.id
         WHERE po.poll_id = ?1
         GROUP BY po.id
         ORDER BY po.position ASC",
    )?;
    let options: Vec<crate::models::PollOption> = stmt
        .query_map(params![poll.id], |r| {
            Ok(crate::models::PollOption {
                id: r.get(0)?,
                poll_id: r.get(1)?,
                text: r.get(2)?,
                position: r.get(3)?,
                vote_count: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let total_votes: i64 = options.iter().map(|o| o.vote_count).sum();

    // Did this user vote, and for which option?
    let user_voted_option: Option<i64> = conn
        .query_row(
            "SELECT option_id FROM poll_votes WHERE poll_id = ?1 AND ip_hash = ?2",
            params![poll.id, ip_hash],
            |r| r.get(0),
        )
        .optional()?;

    let is_expired = poll.expires_at <= now;

    Ok(Some(crate::models::PollData {
        poll,
        options,
        total_votes,
        user_voted_option,
        is_expired,
    }))
}

/// Cast a vote. Returns true if vote was recorded, false if already voted.
pub fn cast_vote(
    conn: &rusqlite::Connection,
    poll_id: i64,
    option_id: i64,
    ip_hash: &str,
) -> Result<bool> {
    let result = conn.execute(
        "INSERT OR IGNORE INTO poll_votes (poll_id, option_id, ip_hash)
         VALUES (?1, ?2, ?3)",
        params![poll_id, option_id, ip_hash],
    )?;
    Ok(result > 0)
}

/// Resolve (poll_id, thread_id, board_short) from an option_id.
pub fn get_poll_context(
    conn: &rusqlite::Connection,
    option_id: i64,
) -> Result<Option<(i64, i64, String)>> {
    Ok(conn
        .query_row(
            "SELECT p.id, p.thread_id, b.short_name
         FROM poll_options po
         JOIN polls p ON p.id = po.poll_id
         JOIN threads t ON t.id = p.thread_id
         JOIN boards b ON b.id = t.board_id
         WHERE po.id = ?1",
            params![option_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
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
        allow_images: row.get::<_, i32>(7)? != 0,
        allow_video: row.get::<_, i32>(8)? != 0,
        allow_audio: row.get::<_, i32>(9)? != 0,
        allow_tripcodes: row.get::<_, i32>(10)? != 0,
        edit_window_secs: row.get(11)?,
        allow_editing: row.get::<_, i32>(12)? != 0,
        allow_archive: row.get::<_, i32>(13)? != 0,
        created_at: row.get(14)?,
    })
}

fn map_post(row: &rusqlite::Row<'_>) -> rusqlite::Result<Post> {
    // media_type is stored as TEXT; map NULL or unknown values to None.
    let media_type_str: Option<String> = row.get(17)?;
    let media_type = media_type_str
        .as_deref()
        .and_then(crate::models::MediaType::from_db_str);

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
        media_type,
        audio_file_path: row.get(18)?,
        audio_file_name: row.get(19)?,
        audio_file_size: row.get(20)?,
        audio_mime_type: row.get(21)?,
        edited_at: row.get(22)?,
    })
}

/// Data needed to insert a new post.
/// FIX[MEDIUM-3]: Derives Clone so create_thread_with_op can rebind fields.
#[derive(Clone)]
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
    /// Explicit media classification derived from MIME type at upload time.
    pub media_type: Option<String>,
    /// Secondary audio file for image+audio combo posts.
    pub audio_file_path: Option<String>,
    pub audio_file_name: Option<String>,
    pub audio_file_size: Option<i64>,
    pub audio_mime_type: Option<String>,
    pub deletion_token: String,
    pub is_op: bool,
}

/// List all admin users (for CLI tooling)
pub fn list_admins(conn: &rusqlite::Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT id, username, created_at FROM admin_users ORDER BY id ASC")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(i64, String, i64)>>>()?;
    Ok(rows)
}

/// Gather aggregate site-wide statistics for the home page.
///
/// Uses a single pass over the posts table to count totals by media_type,
/// plus a SUM of file_size for posts that still have a file on disk.
pub fn get_site_stats(conn: &rusqlite::Connection) -> Result<crate::models::SiteStats> {
    // Total post count (all posts ever inserted; not decremented on delete
    // because SQLite sequences don't roll back, but COUNT(*) gives live count).
    let total_posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;

    // Per-type counts and active byte total in one query.
    let total_images: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'image'",
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'video'",
        [],
        |r| r.get(0),
    )?;
    let total_audio: i64 = conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE media_type = 'audio'",
        [],
        |r| r.get(0),
    )?;
    // Active bytes: sum file_size for posts that still have a file_path recorded.
    let active_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(file_size), 0) FROM posts WHERE file_path IS NOT NULL AND file_size IS NOT NULL",
        [], |r| r.get(0),
    )?;

    Ok(crate::models::SiteStats {
        total_posts,
        total_images,
        total_videos,
        total_audio,
        active_bytes,
    })
}

// ─── WAL checkpoint ───────────────────────────────────────────────────────────

/// Run PRAGMA wal_checkpoint(TRUNCATE) and return (log_pages, moved_pages, busy).
///
/// SQLite's wal_checkpoint pragma returns three columns:
///   col 0 — busy:         1 if a checkpoint could not complete due to an active reader/writer
///   col 1 — log:          total pages in the WAL file
///   col 2 — checkpointed: pages that were actually written back to the database
///
/// TRUNCATE mode: after a complete checkpoint, the WAL file is truncated to
/// zero bytes, reclaiming disk space immediately.  It is safe to call at any
/// time; if a reader or writer is active the checkpoint proceeds partially and
/// the next run will complete it.
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

// ─── Database size ────────────────────────────────────────────────────────────

/// Return the current on-disk size of the database in bytes
/// (page_count × page_size, as reported by SQLite).
///
/// This reflects the main database file only; the WAL file is separate and
/// typically small after a checkpoint.
pub fn get_db_size_bytes(conn: &rusqlite::Connection) -> Result<i64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(page_count * page_size)
}

/// Run VACUUM on the database, rebuilding it into a minimal file.
///
/// VACUUM rewrites the entire database file, compacting free pages left by
/// bulk deletions.  It cannot run inside a transaction.  The call blocks until
/// the full rebuild is complete; for large databases this may take several
/// seconds.  Always call `get_db_size_bytes` before and after to report the
/// space saving to the operator.
pub fn run_vacuum(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
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

// ─── Background job queue ─────────────────────────────────────────────────────
//
// Jobs flow through: pending → running → done | failed
// claim_next_job uses UPDATE … RETURNING for atomic claim with no TOCTOU race.

/// Persist a new job in the pending state. Returns the new row id.
pub fn enqueue_job(conn: &rusqlite::Connection, job_type: &str, payload: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO background_jobs (job_type, payload, status, updated_at)
         VALUES (?1, ?2, 'pending', unixepoch())",
        params![job_type, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Atomically claim the highest-priority pending job that has not exhausted
/// its retry budget. Returns (job_id, payload) or None when the queue is empty.
///
/// The UPDATE … RETURNING subquery is a single atomic operation in SQLite's
/// WAL mode, so no two workers can claim the same job.
pub fn claim_next_job(conn: &rusqlite::Connection) -> Result<Option<(i64, String)>> {
    let mut stmt = conn.prepare_cached(
        "UPDATE background_jobs
         SET status = 'running',
             attempts  = attempts + 1,
             updated_at = unixepoch()
         WHERE id = (
             SELECT id FROM background_jobs
             WHERE status = 'pending' AND attempts < 3
             ORDER BY priority DESC, created_at ASC
             LIMIT 1
         )
         RETURNING id, payload",
    )?;
    let result = stmt
        .query_row([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .optional()?;
    Ok(result)
}

/// Mark a job as successfully completed.
pub fn complete_job(conn: &rusqlite::Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE background_jobs SET status = 'done', updated_at = unixepoch()
         WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Record a job failure. After MAX_ATTEMPTS the job stays "failed" permanently.
pub fn fail_job(conn: &rusqlite::Connection, id: i64, error: &str) -> Result<()> {
    // Truncate error to 512 chars to prevent runaway row sizes.
    let err_trunc: String = error.chars().take(512).collect();
    conn.execute(
        "UPDATE background_jobs
         SET status = CASE WHEN attempts >= 3 THEN 'failed' ELSE 'pending' END,
             last_error  = ?2,
             updated_at  = unixepoch()
         WHERE id = ?1",
        params![id, err_trunc],
    )?;
    Ok(())
}

/// Count jobs currently in the 'pending' state (used for monitoring).
#[allow(dead_code)]
pub fn pending_job_count(conn: &rusqlite::Connection) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM background_jobs WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

// ─── Post update helpers (used by background workers) ────────────────────────

/// Update a post's file_path and mime_type after background transcoding.
pub fn update_post_file_info(
    conn: &rusqlite::Connection,
    post_id: i64,
    file_path: &str,
    mime_type: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE posts SET file_path = ?1, mime_type = ?2 WHERE id = ?3",
        params![file_path, mime_type, post_id],
    )?;
    Ok(())
}

/// Update a post's thumb_path after background waveform / thumbnail generation.
pub fn update_post_thumb_path(
    conn: &rusqlite::Connection,
    post_id: i64,
    thumb_path: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE posts SET thumb_path = ?1 WHERE id = ?2",
        params![thumb_path, post_id],
    )?;
    Ok(())
}

/// Retrieve just the thumb_path for a post (used by VideoTranscode worker to
/// preserve the existing thumbnail when refreshing the file-hash record).
pub fn get_post_thumb_path(conn: &rusqlite::Connection, post_id: i64) -> Result<Option<String>> {
    let result = conn
        .query_row(
            "SELECT thumb_path FROM posts WHERE id = ?1",
            params![post_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(result)
}

/// Delete a file-hash record by its stored file_path (used when the worker
/// replaces an MP4 with the transcoded WebM and needs to refresh the index).
pub fn delete_file_hash_by_path(conn: &rusqlite::Connection, file_path: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM file_hashes WHERE file_path = ?1",
        params![file_path],
    )?;
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
