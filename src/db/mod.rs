// db/mod.rs — Database layer.
//
// All SQL lives in the sub-modules. Handlers call these functions via
// spawn_blocking. Schema is created on first run; WAL + NORMAL sync reduces
// disk writes without compromising durability.
//
// Layout
// ──────
//   mod.rs    — pool type, init_pool, create_schema, shared types,
//               paths_safe_to_delete helper, sub-module re-exports
//   boards.rs — site settings, board CRUD, get_site_stats
//   threads.rs — thread queries, archive/prune logic
//   posts.rs  — post CRUD, file dedup, polls, job queue, worker helpers
//   admin.rs  — admin/session, bans, word filters, reports, mod log,
//               ban appeals, IP history, DB maintenance

use crate::config::CONFIG;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::path::Path;
use tracing::info;

pub mod admin;
pub mod boards;
pub mod posts;
pub mod threads;

// Re-export every public symbol so all existing call-sites (db::foo) compile
// without any changes.
pub use admin::*;
pub use boards::*;
pub use posts::*;
pub use threads::*;

// ─── Public pool type ─────────────────────────────────────────────────────────

pub type DbPool = Pool<SqliteConnectionManager>;

// ─── Shared data types ────────────────────────────────────────────────────────

/// Data needed to insert a new post.
/// `Clone` is derived so `create_thread_with_op` can rebind fields without
/// requiring the caller to construct two separate NewPost values.
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

/// A resolved file-deduplication cache entry.
pub struct CachedFile {
    pub file_path: String,
    pub thumb_path: String,
    pub mime_type: String,
}

// ─── Connection pool initialisation ──────────────────────────────────────────

/// Create the connection pool and run schema migrations.
pub fn init_pool() -> Result<DbPool> {
    let db_path = &CONFIG.database_path;

    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        // These pragmas apply to every new connection in the pool.
        // WAL: readers don't block writers; good for concurrent requests.
        // synchronous=NORMAL: safe with WAL, reduces fsync calls.
        // foreign_keys: enforce relational integrity.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -4096;  -- 4 MiB page cache per connection
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 67108864; -- 64 MiB memory-mapped IO
             PRAGMA busy_timeout = 10000; -- 10s: wait instead of instant SQLITE_BUSY",
        )
    });

    let pool = Pool::builder()
        // FIX[LOW-4]: Pool size of 8 gives enough headroom for concurrent
        // requests without exhausting SQLite's WAL-mode write serialisation.
        .max_size(8)
        // FIX[HIGH-2]: Bound how long spawn_blocking threads wait for a
        // connection. Without this, a burst can exhaust the Tokio thread pool.
        .connection_timeout(std::time::Duration::from_secs(5))
        .build(manager)
        .context("Failed to build database pool")?;

    let conn = pool.get().context("Failed to get DB connection")?;
    create_schema(&conn)?;

    info!("Database initialised at {}", db_path);
    Ok(pool)
}

// ─── Schema creation & migrations ────────────────────────────────────────────

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
            media_type       TEXT,
            audio_file_path  TEXT,
            audio_file_name  TEXT,
            audio_file_size  INTEGER,
            audio_mime_type  TEXT,
            edited_at        INTEGER
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
            UNIQUE(poll_id, ip_hash)
        );

        -- Site-wide key/value settings
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

    // ─── Schema versioning ──────────────────────────────────────────────────
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO schema_version (version)
         SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
    )
    .context("Failed to create schema_version table")?;

    let current_version: i64 =
        conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;

    // Each entry is (introduced_at_version, sql).
    // ALTER TABLE … ADD COLUMN returns SQLITE_ERROR (code 1) with the message
    // "duplicate column name: X" when the column already exists — this happens
    // when the binary is restarted against a DB that was already migrated.
    // CREATE INDEX … IF NOT EXISTS is already idempotent and never errors.
    //
    // FIX[MIGRATION]: The previous guard caught ALL ErrorCode::Unknown errors,
    // which maps to the generic SQLITE_ERROR (code 1).  That code is also
    // returned for SQL syntax errors, wrong number of columns, etc.  A typo
    // in migration SQL (e.g. "ADD COULMN") would be silently swallowed,
    // marked as applied in schema_version, and the column would never exist.
    //
    // We now additionally inspect the error message string to confirm the
    // error is specifically "duplicate column name" before treating it as
    // idempotent.  Any other SQLITE_ERROR is propagated so the operator sees
    // it immediately rather than discovering a missing column at runtime.
    let migrations: &[(i64, &str)] = &[
        (1,  "ALTER TABLE boards ADD COLUMN allow_video    INTEGER NOT NULL DEFAULT 1"),
        (2,  "ALTER TABLE boards ADD COLUMN allow_tripcodes INTEGER NOT NULL DEFAULT 1"),
        (3,  "ALTER TABLE boards ADD COLUMN allow_images  INTEGER NOT NULL DEFAULT 1"),
        (4,  "ALTER TABLE boards ADD COLUMN allow_audio   INTEGER NOT NULL DEFAULT 0"),
        (5,  "ALTER TABLE posts ADD COLUMN media_type TEXT"),
        (6,  "ALTER TABLE posts ADD COLUMN audio_file_path TEXT"),
        (7,  "ALTER TABLE posts ADD COLUMN audio_file_name TEXT"),
        (8,  "ALTER TABLE posts ADD COLUMN audio_file_size INTEGER"),
        (9,  "ALTER TABLE posts ADD COLUMN audio_mime_type TEXT"),
        (10, "ALTER TABLE posts ADD COLUMN edited_at INTEGER"),
        (11, "CREATE INDEX IF NOT EXISTS idx_jobs_pending ON background_jobs(status, priority DESC, created_at ASC)"),
        (12, "CREATE INDEX IF NOT EXISTS idx_reports_status ON reports(status, created_at DESC)"),
        (13, "CREATE INDEX IF NOT EXISTS idx_mod_log_created ON mod_log(created_at DESC)"),
        (14, "ALTER TABLE threads ADD COLUMN archived INTEGER NOT NULL DEFAULT 0"),
        (15, "CREATE INDEX IF NOT EXISTS idx_threads_archived ON threads(board_id, archived, bumped_at DESC)"),
        (16, "ALTER TABLE boards ADD COLUMN edit_window_secs INTEGER NOT NULL DEFAULT 0"),
        (17, "ALTER TABLE boards ADD COLUMN allow_editing INTEGER NOT NULL DEFAULT 0"),
        (18, "ALTER TABLE boards ADD COLUMN allow_archive INTEGER NOT NULL DEFAULT 1"),
        (19, "ALTER TABLE boards ADD COLUMN allow_video_embeds INTEGER NOT NULL DEFAULT 0"),
        (20, "ALTER TABLE boards ADD COLUMN allow_captcha INTEGER NOT NULL DEFAULT 0"),
        (21, r"CREATE TABLE IF NOT EXISTS ban_appeals (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ip_hash     TEXT NOT NULL,
            reason      TEXT NOT NULL DEFAULT '',
            status      TEXT NOT NULL DEFAULT 'open',
            created_at  INTEGER NOT NULL DEFAULT (unixepoch())
        )"),
        (22, "ALTER TABLE boards ADD COLUMN post_cooldown_secs INTEGER NOT NULL DEFAULT 0"),
    ];

    let mut highest_applied = current_version;
    for &(version, sql) in migrations {
        if version <= current_version {
            continue;
        }
        match conn.execute(sql, []) {
            Ok(_) => {
                tracing::debug!("Applied migration v{}", version);
                highest_applied = version;
            }
            Err(rusqlite::Error::SqliteFailure(ref e, ref msg))
                if e.code == rusqlite::ErrorCode::Unknown
                    && msg
                        .as_deref()
                        .map(|m| {
                            m.contains("duplicate column name") || m.contains("already exists")
                        })
                        .unwrap_or(false) =>
            {
                // Idempotent: column already added or index already exists.
                // Only reached for ALTER TABLE … ADD COLUMN (duplicate column)
                // and CREATE INDEX (already exists). All other SQLITE_ERROR
                // values (syntax errors, wrong column counts, etc.) are NOT
                // caught here and will propagate as real failures.
                tracing::debug!(
                    "Migration v{} already applied (idempotent), skipping",
                    version
                );
                highest_applied = version;
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Migration v{} failed: {} — SQL: {}",
                    version,
                    e,
                    sql
                ));
            }
        }
    }

    if highest_applied > current_version {
        conn.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![highest_applied],
        )
        .context("Failed to update schema_version")?;
    }

    // Backfill media_type for existing posts that pre-date the column.
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

// ─── Shared file-safety helper ────────────────────────────────────────────────

/// Given a list of candidate file paths collected from posts about to be deleted,
/// return only those paths that are no longer referenced by *any* remaining post.
///
/// This guards against the deduplication cascade-delete bug: when a file is
/// reposted, both posts share the same file_path / thumb_path on disk. Without
/// this guard, deleting any single post unconditionally deletes the shared file,
/// corrupting every other post that references it.
///
/// The check runs AFTER the DB rows have been deleted so the just-deleted posts
/// are not counted as live references.
///
/// Also purges the corresponding file_hashes rows for files that have no
/// remaining references, so the dedup table never points at deleted files.
///
/// `pub(super)` — visible to all four sub-modules but not to external callers.
pub(super) fn paths_safe_to_delete(
    conn: &rusqlite::Connection,
    mut candidates: Vec<String>,
) -> Vec<String> {
    // Deduplicate first: when multiple deleted posts share the same file via
    // the dedup system, the same path can appear in `candidates` more than once.
    // Without this, the returned Vec can contain duplicates — both pass the
    // COUNT check after rows are deleted — and the caller would attempt
    // fs::remove_file on the same path twice, producing a spurious I/O error.
    candidates.sort_unstable();
    candidates.dedup();

    candidates
        .into_iter()
        .filter(|path| {
            let still_used: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM posts
                      WHERE file_path = ?1
                         OR thumb_path = ?1
                         OR audio_file_path = ?1",
                    params![path],
                    |r| r.get(0),
                )
                .unwrap_or(1); // on error, assume still in use — safer to leak than corrupt
            if still_used == 0 {
                // No remaining posts reference this file; remove from dedup table too.
                let _ = conn.execute(
                    "DELETE FROM file_hashes WHERE file_path = ?1 OR thumb_path = ?1",
                    params![path],
                );
                true
            } else {
                false
            }
        })
        .collect()
}
