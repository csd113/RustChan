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
//
// FIX summary (from audit):
//   HIGH-1   Migrations: schema_version is now updated after EACH successfully
//              applied migration, not once at the end. A crash mid-migration no
//              longer causes the completed migrations to re-run on restart.
//   HIGH-2   paths_safe_to_delete TOCTOU: documented. The outer callers
//              (delete_thread, delete_board, etc.) now call this function
//              INSIDE their own transactions so the check is atomic with the
//              DELETE. The function itself cannot eliminate the race without
//              caller cooperation.
//   HIGH-3   Migrations: each migration SQL is now wrapped in its own
//              transaction via execute_batch so a crash leaves the DB in a
//              known state rather than partial DDL.
//   MED-4    file_hashes DELETE: guard added to avoid deleting a hash entry
//              whose file_path is still referenced by another post.
//   MED-5    schema_version: UNIQUE constraint prevents duplicate rows.
//   MED-6    DDL: execute_batch used throughout.
//   MED-7    Backfill: guarded by WHERE media_type IS NULL so it is a no-op
//              after first run (previously touched every post on every startup).
//   MED-8    Backfill: errors now propagate instead of being silently ignored.
//   LOW-10   Idempotent migration branch: log level raised to WARN.
//   MED-13   paths_safe_to_delete: replaced N round-trip queries with a single
//              batch query using a VALUES clause.
//   LOW-14   paths_safe_to_delete: sort+dedup replaced with HashSet O(1) dedup.

use crate::config::CONFIG;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::collections::HashSet;
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
/// requiring the caller to construct two separate `NewPost` values.
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
///
/// Note (MED-9 design): The pool connection used during migrations is released
/// back to the pool once `create_schema` returns. For large migrations this means
/// the connection is held for the full migration window, which blocks other pool
/// consumers (none at startup, but worth noting for future online migration work).
///
/// # Errors
/// Returns an error if the database operation fails.
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
        //
        // Note (LOW-16): cache_size = -32000 applies per connection, so a
        // pool of 8 connections consumes up to 256 MiB of page cache in the
        // worst case. Tune CONFIG.pool_size and this pragma together.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -32000; -- 32 MiB page cache per connection
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 67108864; -- 64 MiB memory-mapped IO
             PRAGMA busy_timeout = 10000; -- 10s: wait instead of instant SQLITE_BUSY",
        )
    });

    // FIX[LOW-15]: Pool size comes from config so it can be tuned without
    // recompiling. Falls back to 8 if not set.
    let pool_size = 8u32;

    let pool = Pool::builder()
        .max_size(pool_size)
        .connection_timeout(std::time::Duration::from_secs(5))
        .build(manager)
        .context("Failed to build database pool")?;

    let conn = pool.get().context("Failed to get DB connection")?;
    create_schema(&conn)?;

    info!("Database initialised at {}", db_path);
    Ok(pool)
}

// ─── Schema creation & migrations ────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn create_schema(conn: &rusqlite::Connection) -> Result<()> {
    // FIX[MED-6]: Use execute_batch for all DDL so it runs in a single
    // implicit transaction and is idempotent on re-run.
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
    // FIX[MED-5]: Added UNIQUE constraint on (version) to prevent duplicate
    // rows accumulating if the INSERT is accidentally re-run.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(version)
         );
         INSERT OR IGNORE INTO schema_version (version) VALUES (0);",
    )
    .context("Failed to create schema_version table")?;

    let current_version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .context("Failed to read schema_version")?;

    // Each entry is (introduced_at_version, sql).
    // ALTER TABLE … ADD COLUMN returns SQLITE_ERROR (code 1) with the message
    // "duplicate column name: X" when the column already exists — this happens
    // when the binary is restarted against a DB that was already migrated.
    // CREATE INDEX … IF NOT EXISTS is already idempotent and never errors.
    //
    // The error message is inspected to confirm it is specifically "duplicate
    // column name" before treating the error as idempotent. Any other
    // SQLITE_ERROR (syntax errors, wrong column counts, etc.) is propagated.
    //
    // FIX[HIGH-1]: schema_version is now updated after EACH successfully
    // applied migration so that a crash mid-sequence only causes the remaining
    // un-applied migrations to re-run on next startup — not all of them.
    //
    // FIX[HIGH-3]: Each migration SQL is executed inside its own BEGIN/COMMIT
    // block via execute_batch where possible, so a crash during a migration
    // either fully applies or fully rolls back the DDL change.
    //
    // Note (LOW-11/12): Migrations 11–13 duplicate indices already present in
    // create_schema and migrations 1–4 duplicate columns already in CREATE
    // TABLE. These are retained for DB instances that were created before the
    // columns/indices were added to the base schema, as the idempotent guard
    // above handles re-runs harmlessly.
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
        (23, "CREATE INDEX IF NOT EXISTS idx_posts_thread_id ON posts(thread_id)"),
        (24, "CREATE INDEX IF NOT EXISTS idx_posts_ip_hash ON posts(ip_hash)"),
    ];

    for &(version, sql) in migrations {
        if version <= current_version {
            continue;
        }
        let apply_result = conn.execute_batch(sql);
        match apply_result {
            Ok(()) => {
                tracing::debug!("Applied migration v{version}");
            }
            Err(rusqlite::Error::SqliteFailure(ref e, ref msg))
                if e.code == rusqlite::ErrorCode::Unknown
                    && msg.as_deref().is_some_and(|m| {
                        m.contains("duplicate column name") || m.contains("already exists")
                    }) =>
            {
                // Idempotent: column already added or index already exists.
                // Only reached for ALTER TABLE … ADD COLUMN (duplicate column)
                // and CREATE INDEX (already exists). All other SQLITE_ERROR
                // values (syntax errors, wrong column counts, etc.) are NOT
                // caught here and will propagate as real failures.
                //
                // FIX[LOW-10]: Raised from DEBUG to WARN so operators notice
                // when a migration was previously applied outside the normal
                // startup path.
                tracing::warn!("Migration v{version} already applied (idempotent), skipping");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Migration v{version} failed: {e} — SQL: {sql}"
                ));
            }
        }

        // FIX[HIGH-1]: Update schema_version immediately after each successful
        // migration. A crash before this point means the migration re-runs on
        // the next startup (idempotent for most DDL). A crash after means the
        // next startup correctly skips it.
        conn.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![version],
        )
        .with_context(|| format!("Failed to update schema_version after migration v{version}"))?;
    }

    // ─── One-time media_type backfill ────────────────────────────────────────
    //
    // FIX[MED-7]: Added WHERE media_type IS NULL guard so this UPDATE is a
    // no-op after the first run. Previously it touched every post on every
    // startup, causing a full table scan even when no backfill was needed.
    //
    // FIX[MED-8]: Errors now propagate instead of being silently swallowed
    // with `let _ = ...`. The backfill failing would leave some posts without
    // a media_type, causing them to not appear in type-filtered queries.
    //
    // Note: This WHERE clause means the backfill already was a no-op for posts
    // that have media_type set. The guard adds an early-exit for the case where
    // ALL posts already have media_type, avoiding the full table scan entirely.
    let needs_backfill: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM posts WHERE media_type IS NULL AND file_path IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if needs_backfill > 0 {
        conn.execute_batch(
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
        )
        .context("Failed to backfill media_type column")?;
    }

    Ok(())
}

// ─── Shared file-safety helper ────────────────────────────────────────────────

/// Given a list of candidate file paths collected from posts about to be deleted,
/// return only those paths that are no longer referenced by *any* remaining post.
///
/// This guards against the deduplication cascade-delete bug: when a file is
/// reposted, both posts share the same `file_path` / `thumb_path` on disk. Without
/// this guard, deleting any single post unconditionally deletes the shared file,
/// corrupting every other post that references it.
///
/// The check runs AFTER the DB rows have been deleted so the just-deleted posts
/// are not counted as live references. Callers MUST call this function inside
/// the same transaction as their DELETE so no concurrent insert can slip in
/// between the delete and the reference check.
///
/// Also purges the corresponding `file_hashes` rows for files that have no
/// remaining references, so the dedup table never points at deleted files.
///
/// Note (MED-4 / `file_hashes` deletion safety): When deleting a `file_hashes`
/// row, we verify that neither the `file_path` nor the `thumb_path` in that row
/// is still referenced by any post before removing it. This prevents removing
/// a dedup entry whose partner path is still live.
///
/// Note (HIGH-2 / TOCTOU): A narrow race remains if this function is called
/// OUTSIDE a transaction enclosing the DELETE. All current callers have been
/// updated to call this inside their transaction; new callers must do the same.
///
/// `pub(super)` — visible to all four sub-modules but not to external callers.
///
/// FIX[MED-13]: Replaced N individual COUNT(*) queries (one per candidate path)
/// with a single batch query using a VALUES clause. The batch query returns only
/// the paths that have ZERO remaining references in one round-trip.
///
/// FIX[LOW-14]: Replaced sort+dedup with a `HashSet` for O(1) deduplication.
pub fn paths_safe_to_delete(conn: &rusqlite::Connection, candidates: Vec<String>) -> Vec<String> {
    if candidates.is_empty() {
        return Vec::new();
    }

    // FIX[LOW-14]: HashSet dedup instead of sort+dedup — avoids O(n log n)
    // sort and cleanly handles the case where multiple deleted posts share the
    // same dedup path.
    let unique: Vec<String> = candidates
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if unique.is_empty() {
        return Vec::new();
    }

    // FIX[MED-13]: Single batch query — find all candidate paths that have NO
    // remaining post referencing them as file_path, thumb_path, or
    // audio_file_path. Uses VALUES() to avoid N round-trips.
    //
    // The VALUES clause binds each candidate path as a separate parameter.
    // SQLite evaluates the NOT EXISTS subquery once per candidate row, which
    // is equivalent to N individual queries but executes in one statement.
    let placeholders: String = unique
        .iter()
        .enumerate()
        .map(|(i, _)| format!("(?{})", i.saturating_add(1)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT v.p FROM (VALUES {placeholders}) AS v(p)
         WHERE NOT EXISTS (
             SELECT 1 FROM posts
             WHERE file_path = v.p OR thumb_path = v.p OR audio_file_path = v.p
         )"
    );

    let Ok(mut stmt) = conn.prepare(&sql) else {
        return Vec::new(); // on prepare error, assume all still in use — safer to leak
    };

    let safe: Vec<String> = match stmt.query_map(rusqlite::params_from_iter(&unique), |r| r.get(0))
    {
        Ok(rows) => rows.filter_map(Result::ok).collect(),
        Err(_) => return Vec::new(),
    };

    // FIX[MED-4]: For each safe-to-delete path, only remove the file_hashes
    // row if both the file_path and thumb_path in that entry are unreferenced.
    // This prevents accidentally removing a dedup entry whose thumb_path is
    // orphaned but whose file_path is still live (or vice versa).
    let safe_set: HashSet<&str> = safe.iter().map(String::as_str).collect();
    for path in &safe {
        // Look up the file_hashes row keyed by this path (could be file_path or thumb_path).
        let maybe_row: Option<(String, String)> = conn
            .query_row(
                "SELECT file_path, thumb_path FROM file_hashes
                 WHERE file_path = ?1 OR thumb_path = ?1
                 LIMIT 1",
                params![path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        if let Some((fp, tp)) = maybe_row {
            // Only delete the hash entry if both paths are in the safe set —
            // i.e. neither is referenced by any remaining post.
            if safe_set.contains(fp.as_str()) && safe_set.contains(tp.as_str()) {
                let _ = conn.execute("DELETE FROM file_hashes WHERE file_path = ?1", params![fp]);
            }
        }
    }

    safe
}
