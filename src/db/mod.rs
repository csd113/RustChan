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
use std::collections::HashSet;
use std::path::Path;

pub mod admin;
pub mod boards;
pub mod chan_net;
pub mod posts;
pub mod threads;

// Re-export sub-module symbols so callers can use db::foo directly.
pub use admin::*;
pub use boards::*;
pub use posts::*;
pub use threads::*;

// ─── Public pool type ─────────────────────────────────────────────────────────

pub type DbPool = Pool<SqliteConnectionManager>;

// ─── Shared data types ────────────────────────────────────────────────────────

/// Data needed to insert a new post.
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
    pub media_type: Option<String>,
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
/// # Errors
/// Returns an error if the database operation fails.
pub fn init_pool() -> Result<DbPool> {
    let db_path = &CONFIG.database_path;

    if let Some(parent) = Path::new(db_path).parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
        // Per-connection pragmas: WAL mode, normal sync, FK enforcement,
        // page cache, temp store, mmap, and busy timeout.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA cache_size = -32000;
             PRAGMA temp_store = MEMORY;
             PRAGMA mmap_size = 67108864;
             PRAGMA busy_timeout = 10000;",
        )
    });

    // Pool size from config, default 8. busy_timeout is 10s so connection_timeout
    // is set to 15s to avoid pool starvation under write contention.
    let pool_size = CONFIG.db_pool_size;

    let pool = Pool::builder()
        .max_size(pool_size)
        .connection_timeout(std::time::Duration::from_secs(15))
        .build(manager)
        .context("Failed to build database pool")?;

    let conn = pool.get().context("Failed to get DB connection")?;
    create_schema(&conn)?;

    tracing::info!(target: "db", path = db_path, "Database initialised");
    Ok(pool)
}

// ─── First-run check ─────────────────────────────────────────────────────────

/// Check whether this is a first run (no boards and no admins).
///
/// # Errors
/// Returns an error if the database connection cannot be obtained.
pub fn first_run_check(pool: &DbPool) -> anyhow::Result<()> {
    let conn = pool.get()?;
    let board_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
        .context("Failed to count boards during first-run check")?;
    let admin_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM admin_users", [], |r| r.get(0))
        .context("Failed to count admin users during first-run check")?;

    if board_count == 0 {
        tracing::info!(
            target: "startup",
            boards = 0,
            admins = admin_count,
            "No boards found — create boards via admin panel or: rustchan-cli admin create-board"
        );
    }
    Ok(())
}

/// Returns `true` when the database contains no admin accounts.
///
/// Logs a warning and returns `false` on any database error (fail-safe:
/// skip the wizard rather than blocking startup).
#[must_use]
pub fn has_no_admin(pool: &DbPool) -> bool {
    match pool.get() {
        Ok(conn) => match conn.query_row("SELECT COUNT(*) FROM admin_users", [], |r| {
            r.get::<_, i64>(0)
        }) {
            Ok(count) => count == 0,
            Err(e) => {
                tracing::warn!(target: "db", "Failed to count admin users: {e}");
                false
            }
        },
        Err(e) => {
            tracing::warn!(target: "db", "Failed to get DB connection for admin check: {e}");
            false
        }
    }
}

// ─── Schema creation & migrations ────────────────────────────────────────────

/// Derive max migration version from the migrations array at compile time.
macro_rules! max_migration {
    ( $( ($ver:expr, $sql:expr) ),+ $(,)? ) => {{
        let migrations: &[(i64, &str)] = &[ $( ($ver, $sql), )+ ];
        let max = migrations.last().unwrap().0;
        (migrations, max)
    }};
}

#[allow(clippy::too_many_lines)]
fn create_schema(conn: &rusqlite::Connection) -> Result<()> {
    // Base DDL — all tables and indices. Fresh installs get everything here;
    // migrations below only exist for databases created before a given column
    // or table was added.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS boards (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            short_name      TEXT NOT NULL UNIQUE,
            name            TEXT NOT NULL,
            description     TEXT NOT NULL DEFAULT '',
            nsfw            INTEGER NOT NULL DEFAULT 0,
            max_threads     INTEGER NOT NULL DEFAULT 150,
            bump_limit      INTEGER NOT NULL DEFAULT 500,
            allow_video     INTEGER NOT NULL DEFAULT 1,
            allow_tripcodes INTEGER NOT NULL DEFAULT 1,
            allow_images    INTEGER NOT NULL DEFAULT 1,
            allow_audio     INTEGER NOT NULL DEFAULT 0,
            edit_window_secs    INTEGER NOT NULL DEFAULT 0,
            allow_editing       INTEGER NOT NULL DEFAULT 0,
            allow_archive       INTEGER NOT NULL DEFAULT 1,
            allow_video_embeds  INTEGER NOT NULL DEFAULT 0,
            allow_captcha       INTEGER NOT NULL DEFAULT 0,
            post_cooldown_secs  INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL DEFAULT (unixepoch())
        );

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

        CREATE TABLE IF NOT EXISTS posts (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            board_id       INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            name           TEXT NOT NULL DEFAULT 'Anonymous',
            tripcode       TEXT,
            subject        TEXT,
            body           TEXT NOT NULL,
            body_html      TEXT NOT NULL,
            ip_hash        TEXT,
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

        CREATE TABLE IF NOT EXISTS file_hashes (
            sha256     TEXT PRIMARY KEY,
            file_path  TEXT NOT NULL,
            thumb_path TEXT NOT NULL,
            mime_type  TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS admin_users (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            username      TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS admin_sessions (
            id         TEXT PRIMARY KEY,
            admin_id   INTEGER NOT NULL REFERENCES admin_users(id) ON DELETE CASCADE,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            expires_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS bans (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            ip_hash    TEXT NOT NULL,
            reason     TEXT,
            expires_at INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS ban_appeals (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ip_hash     TEXT NOT NULL,
            reason      TEXT NOT NULL DEFAULT '',
            status      TEXT NOT NULL DEFAULT 'open',
            created_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS word_filters (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            pattern     TEXT NOT NULL,
            replacement TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS polls (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            thread_id  INTEGER NOT NULL UNIQUE REFERENCES threads(id) ON DELETE CASCADE,
            question   TEXT NOT NULL,
            expires_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE TABLE IF NOT EXISTS poll_options (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            poll_id  INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
            text     TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS poll_votes (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            poll_id   INTEGER NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
            option_id INTEGER NOT NULL REFERENCES poll_options(id) ON DELETE CASCADE,
            ip_hash   TEXT NOT NULL,
            UNIQUE(poll_id, ip_hash)
        );

        CREATE TABLE IF NOT EXISTS site_settings (
            key        TEXT PRIMARY KEY,
            value      TEXT NOT NULL
        );

        -- reports: thread_id and board_id have FK constraints with SET NULL
        -- so deleted threads/boards don't leave dangling IDs.
        CREATE TABLE IF NOT EXISTS reports (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            post_id        INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            thread_id      INTEGER REFERENCES threads(id) ON DELETE SET NULL,
            board_id       INTEGER REFERENCES boards(id) ON DELETE SET NULL,
            reason         TEXT NOT NULL DEFAULT '',
            reporter_hash  TEXT NOT NULL,
            status         TEXT NOT NULL DEFAULT 'open',
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            resolved_at    INTEGER,
            resolved_by    INTEGER
        );

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

        CREATE TABLE IF NOT EXISTS background_jobs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            job_type    TEXT NOT NULL,
            payload     TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending',
            priority    INTEGER NOT NULL DEFAULT 0,
            attempts    INTEGER NOT NULL DEFAULT 0,
            last_error  TEXT,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- Indices (no redundant index on file_hashes.sha256 — already the PK)
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
        CREATE INDEX IF NOT EXISTS idx_jobs_pending
            ON background_jobs(status, priority DESC, created_at ASC);
        CREATE INDEX IF NOT EXISTS idx_reports_status
            ON reports(status, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_mod_log_created
            ON mod_log(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_posts_thread_id
            ON posts(thread_id);
        CREATE INDEX IF NOT EXISTS idx_posts_ip_hash
            ON posts(ip_hash);
        CREATE INDEX IF NOT EXISTS idx_threads_archived
            ON threads(board_id, archived, bumped_at DESC);
        CREATE INDEX IF NOT EXISTS idx_file_hashes_thumb
            ON file_hashes(thumb_path);

        CREATE TABLE IF NOT EXISTS chan_net_posts (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_post_id  INTEGER NOT NULL,
            board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            author          TEXT    NOT NULL DEFAULT 'anon',
            content         TEXT    NOT NULL DEFAULT '',
            remote_ts       INTEGER NOT NULL,
            imported_at     INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_chan_net_posts_remote
            ON chan_net_posts(remote_post_id, board_id);
        ",
    )
    .context("Schema creation failed")?;

    // ─── Schema versioning ──────────────────────────────────────────────────
    // Holds exactly one row. Fresh installs seed with the max migration version
    // so all migrations are skipped (base DDL already includes everything).
    // Max version is derived from the migrations array via macro.

    let (migrations, current_max_migration) = max_migration![
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
        (25, r"CREATE TABLE IF NOT EXISTS chan_net_posts (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_post_id  INTEGER NOT NULL,
            board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            author          TEXT    NOT NULL DEFAULT 'anon',
            content         TEXT    NOT NULL DEFAULT '',
            remote_ts       INTEGER NOT NULL,
            imported_at     INTEGER NOT NULL DEFAULT (unixepoch())
        )"),
        (26, "CREATE UNIQUE INDEX IF NOT EXISTS idx_chan_net_posts_remote \
              ON chan_net_posts(remote_post_id, board_id)"),
    ];

    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(version)
         );
         INSERT INTO schema_version (version)
         SELECT {current_max_migration}
         WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
    ))
    .context("Failed to create schema_version table")?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .context("Failed to read schema_version")?;

    // Apply pending migrations one at a time, updating schema_version after each.
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
                // Idempotent: column/index already exists from a previous run.
                tracing::warn!("Migration v{version} already applied (idempotent), skipping");
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Migration v{version} failed: {e} — SQL: {sql}"
                ));
            }
        }

        // Update version after each migration so crashes don't re-run completed ones.
        conn.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![version],
        )
        .with_context(|| format!("Failed to update schema_version after migration v{version}"))?;
    }

    // ─── Structural migration: make posts.ip_hash nullable ───────────────────
    // SQLite doesn't support ALTER COLUMN, so we use the copy-rename-drop pattern.
    // Guarded by PRAGMA table_info so it's a no-op if already nullable.
    // FK checks are disabled for the swap and unconditionally re-enabled after,
    // even on failure.
    let ip_hash_notnull: i64 = conn
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('posts') WHERE name = 'ip_hash'",
            [],
            |r| r.get(0),
        )
        .context("Failed to read ip_hash nullability from pragma_table_info")?;

    if ip_hash_notnull == 1 {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .context("Failed to disable foreign_keys for structural migration")?;

        let result = conn.execute_batch(
            "BEGIN;

             CREATE TABLE posts_new (
                 id               INTEGER PRIMARY KEY AUTOINCREMENT,
                 thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                 board_id         INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
                 name             TEXT    NOT NULL DEFAULT 'Anonymous',
                 tripcode         TEXT,
                 subject          TEXT,
                 body             TEXT    NOT NULL,
                 body_html        TEXT    NOT NULL,
                 ip_hash          TEXT,
                 file_path        TEXT,
                 file_name        TEXT,
                 file_size        INTEGER,
                 thumb_path       TEXT,
                 mime_type        TEXT,
                 created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
                 deletion_token   TEXT    NOT NULL,
                 is_op            INTEGER NOT NULL DEFAULT 0,
                 media_type       TEXT,
                 audio_file_path  TEXT,
                 audio_file_name  TEXT,
                 audio_file_size  INTEGER,
                 audio_mime_type  TEXT,
                 edited_at        INTEGER
             );

             INSERT INTO posts_new SELECT * FROM posts;
             DROP TABLE posts;
             ALTER TABLE posts_new RENAME TO posts;

             CREATE INDEX IF NOT EXISTS idx_posts_thread
                 ON posts(thread_id, created_at ASC);
             CREATE INDEX IF NOT EXISTS idx_posts_board
                 ON posts(board_id, created_at DESC);
             CREATE INDEX IF NOT EXISTS idx_posts_thread_id
                 ON posts(thread_id);
             CREATE INDEX IF NOT EXISTS idx_posts_ip_hash
                 ON posts(ip_hash);

             COMMIT;",
        );

        // Always re-enable FK checks regardless of migration success or failure.
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .context("Failed to re-enable foreign_keys after structural migration")?;

        result.context("Structural migration: make posts.ip_hash nullable failed")?;

        tracing::info!(target: "db", "Applied structural migration: posts.ip_hash is now nullable");
    }

    // ─── One-time media_type backfill ────────────────────────────────────────
    // Only runs when there are posts with a file_path but no media_type set.
    // Uses EXISTS to short-circuit instead of a full-scan COUNT.
    let needs_backfill: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM posts WHERE media_type IS NULL AND file_path IS NOT NULL)",
            [],
            |r| r.get(0),
        )
        .context("Failed to check posts needing media_type backfill")?;

    if needs_backfill {
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

    // ─── Recover stuck "running" jobs from a previous crash ──────────────────
    conn.execute(
        "UPDATE background_jobs
         SET status    = 'pending',
             attempts  = attempts + 1,
             last_error = 'Recovered from crash (was stuck in running state)',
             updated_at = unixepoch()
         WHERE status = 'running'",
        [],
    )
    .context("Failed to recover stuck running jobs")?;

    // ─── Prune expired admin sessions ────────────────────────────────────────
    conn.execute(
        "DELETE FROM admin_sessions WHERE expires_at < unixepoch()",
        [],
    )
    .context("Failed to prune expired admin sessions")?;

    Ok(())
}

// ─── Shared file-safety helper ────────────────────────────────────────────────

/// Given candidate file paths from posts about to be deleted, return only
/// those no longer referenced by any remaining post.
///
/// Must be called inside the same transaction as the `DELETE` so no concurrent
/// insert can reference these paths between the delete and the check.
///
/// Also purges corresponding `file_hashes` rows for fully-orphaned files.
///
/// # Errors
/// Returns a [`rusqlite::Error`] if the reference-check query fails.
pub fn paths_safe_to_delete(
    conn: &rusqlite::Connection,
    candidates: Vec<String>,
) -> Result<Vec<String>, rusqlite::Error> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Deduplicate candidates.
    let unique: Vec<String> = candidates
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if unique.is_empty() {
        return Ok(Vec::new());
    }

    // Use a temp table for broad SQLite compatibility.
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS _candidate_paths (p TEXT NOT NULL)")?;
    conn.execute("DELETE FROM _candidate_paths", [])?;

    // Insert all candidate paths into the temp table.
    let mut insert_stmt = conn.prepare_cached("INSERT INTO _candidate_paths (p) VALUES (?1)")?;
    for path in &unique {
        insert_stmt.execute(params![path])?;
    }
    drop(insert_stmt);

    // Find paths with zero remaining references in posts.
    let mut safe_stmt = conn.prepare(
        "SELECT p FROM _candidate_paths
         WHERE NOT EXISTS (
             SELECT 1 FROM posts
             WHERE file_path = p OR thumb_path = p OR audio_file_path = p
         )",
    )?;
    let safe: Vec<String> = safe_stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    drop(safe_stmt);

    // Purge orphaned file_hashes: only delete a row if BOTH its file_path
    // and thumb_path are in the safe set (no remaining references to either).
    if !safe.is_empty() {
        let safe_set: HashSet<&str> = safe.iter().map(String::as_str).collect();

        let mut hash_stmt = conn.prepare(
            "SELECT sha256, file_path, thumb_path FROM file_hashes
             WHERE file_path IN (SELECT p FROM _candidate_paths)
                OR thumb_path IN (SELECT p FROM _candidate_paths)",
        )?;

        let hashes_to_delete: Vec<String> = hash_stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|row| {
                let (sha, fp, tp) = row.ok()?;
                if safe_set.contains(fp.as_str()) && safe_set.contains(tp.as_str()) {
                    Some(sha)
                } else {
                    None
                }
            })
            .collect();
        drop(hash_stmt);

        // Delete orphaned hash entries, propagating errors.
        let mut del_stmt = conn.prepare_cached("DELETE FROM file_hashes WHERE sha256 = ?1")?;
        for sha in &hashes_to_delete {
            del_stmt.execute(params![sha]).map_err(|e| {
                tracing::error!("Failed to delete file_hashes entry {sha}: {e}");
                e
            })?;
        }
    }

    // Clean up temp table.
    conn.execute("DELETE FROM _candidate_paths", [])?;

    Ok(safe)
}
