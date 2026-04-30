// src/db/schema.rs

use anyhow::{Context, Result};

use super::migrations::{read_schema_version, stamp_schema_version, POST_SQUASH_SCHEMA_VERSION};

const BASE_SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS boards (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        display_order   INTEGER NOT NULL DEFAULT 0,
        short_name      TEXT NOT NULL UNIQUE,
        name            TEXT NOT NULL,
        description     TEXT NOT NULL DEFAULT '',
        nsfw            INTEGER NOT NULL DEFAULT 0,
        max_threads     INTEGER NOT NULL DEFAULT 150,
        max_archived_threads INTEGER NOT NULL DEFAULT 150,
        bump_limit      INTEGER NOT NULL DEFAULT 500,
        allow_video     INTEGER NOT NULL DEFAULT 1,
        allow_tripcodes INTEGER NOT NULL DEFAULT 1,
        allow_images    INTEGER NOT NULL DEFAULT 1,
        allow_audio     INTEGER NOT NULL DEFAULT 0,
        max_image_size  INTEGER NOT NULL DEFAULT 8388608,
        max_video_size  INTEGER NOT NULL DEFAULT 52428800,
        max_audio_size  INTEGER NOT NULL DEFAULT 157286400,
        allow_pdf       INTEGER NOT NULL DEFAULT 0,
        allow_any_files INTEGER NOT NULL DEFAULT 0,
        edit_window_secs    INTEGER NOT NULL DEFAULT 0,
        allow_editing       INTEGER NOT NULL DEFAULT 1,
        allow_self_delete   INTEGER NOT NULL DEFAULT 1,
        allow_archive       INTEGER NOT NULL DEFAULT 1,
        allow_video_embeds  INTEGER NOT NULL DEFAULT 1,
        allow_captcha       INTEGER NOT NULL DEFAULT 0,
        show_poster_ids     INTEGER NOT NULL DEFAULT 1,
        collapse_greentext  INTEGER NOT NULL DEFAULT 0,
        post_cooldown_secs  INTEGER NOT NULL DEFAULT 0,
        default_theme       TEXT NOT NULL DEFAULT '',
        banner_mode         TEXT NOT NULL DEFAULT 'inherit'
                                CHECK (banner_mode IN ('inherit', 'none', 'override')),
        access_mode         TEXT NOT NULL DEFAULT 'public'
                                CHECK (access_mode IN ('public', 'view_password', 'post_password')),
        access_password_hash TEXT NOT NULL DEFAULT '',
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
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        board_id         INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        name             TEXT NOT NULL DEFAULT 'Anonymous',
        tripcode         TEXT,
        subject          TEXT,
        body             TEXT NOT NULL,
        body_html        TEXT NOT NULL,
        ip_hash          TEXT,
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
        edited_at        INTEGER,
        media_processing_state TEXT NOT NULL DEFAULT '',
        media_processing_error TEXT
    );

    CREATE TABLE IF NOT EXISTS file_hashes (
        sha256     TEXT PRIMARY KEY,
        file_path  TEXT NOT NULL,
        thumb_path TEXT NOT NULL,
        mime_type  TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    );

    CREATE TABLE IF NOT EXISTS pending_fs_ops (
        id           TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        created_at   INTEGER NOT NULL DEFAULT (unixepoch())
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

    CREATE TABLE IF NOT EXISTS banner_assets (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        scope_type      TEXT NOT NULL,
        board_id        INTEGER REFERENCES boards(id) ON DELETE CASCADE,
        storage_key     TEXT NOT NULL UNIQUE,
        width           INTEGER NOT NULL,
        height          INTEGER NOT NULL,
        file_size       INTEGER NOT NULL,
        enabled         INTEGER NOT NULL DEFAULT 1,
        sort_order      INTEGER NOT NULL DEFAULT 0,
        target_type     TEXT NOT NULL DEFAULT 'none',
        target_value    TEXT NOT NULL DEFAULT '',
        show_on_index   INTEGER NOT NULL DEFAULT 1,
        show_on_catalog INTEGER NOT NULL DEFAULT 1,
        created_at      INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE INDEX IF NOT EXISTS idx_banner_assets_scope_sort
        ON banner_assets(scope_type, board_id, sort_order, id);

    CREATE TABLE IF NOT EXISTS themes (
        slug         TEXT PRIMARY KEY,
        display_name TEXT NOT NULL,
        description  TEXT NOT NULL DEFAULT '',
        swatch_hex   TEXT NOT NULL DEFAULT '#888888',
        enabled      INTEGER NOT NULL DEFAULT 1,
        sort_order   INTEGER NOT NULL DEFAULT 0,
        is_builtin   INTEGER NOT NULL DEFAULT 0,
        custom_css   TEXT NOT NULL DEFAULT ''
    );

    CREATE TABLE IF NOT EXISTS reports (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        post_id        INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
        thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        board_id       INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        reason         TEXT NOT NULL DEFAULT '',
        reporter_hash  TEXT NOT NULL,
        status         TEXT NOT NULL DEFAULT 'open',
        created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
        resolved_at    INTEGER,
        resolved_by    INTEGER REFERENCES admin_users(id) ON DELETE SET NULL
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

    CREATE TABLE IF NOT EXISTS chan_net_posts (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        remote_post_id  INTEGER NOT NULL,
        board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        author          TEXT    NOT NULL DEFAULT 'anon',
        content         TEXT    NOT NULL DEFAULT '',
        remote_ts       INTEGER NOT NULL,
        imported_at     INTEGER NOT NULL DEFAULT (unixepoch())
    );

    CREATE TABLE IF NOT EXISTS chan_net_import_ledger (
        tx_id        TEXT PRIMARY KEY,
        imported_at  INTEGER NOT NULL DEFAULT (unixepoch())
    );

    CREATE TABLE IF NOT EXISTS user_thread_preferences (
        user_hash   TEXT NOT NULL,
        thread_id    INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        pinned      INTEGER NOT NULL DEFAULT 0,
        hidden      INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        updated_at  INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY(user_hash, thread_id)
    );

    CREATE TABLE IF NOT EXISTS post_submissions (
        submission_token TEXT PRIMARY KEY,
        ip_hash          TEXT NOT NULL,
        board_id         INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
        thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        post_id          INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
        is_thread        INTEGER NOT NULL DEFAULT 0,
        created_at       INTEGER NOT NULL DEFAULT (unixepoch())
    );
";

const INDEX_SCHEMA_SQL: &str = "
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
    CREATE INDEX IF NOT EXISTS idx_pending_fs_ops_created
        ON pending_fs_ops(created_at ASC);
    CREATE INDEX IF NOT EXISTS idx_jobs_pending
        ON background_jobs(status, priority DESC, created_at ASC);
    CREATE INDEX IF NOT EXISTS idx_reports_status
        ON reports(status, created_at DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_reports_open_unique
        ON reports(post_id, reporter_hash)
        WHERE status = 'open';
    CREATE INDEX IF NOT EXISTS idx_mod_log_created
        ON mod_log(created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_posts_thread_id
        ON posts(thread_id);
    CREATE INDEX IF NOT EXISTS idx_posts_media_processing_state
        ON posts(media_processing_state);
    CREATE INDEX IF NOT EXISTS idx_posts_ip_hash
        ON posts(ip_hash);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_posts_one_op_per_thread
        ON posts(thread_id)
        WHERE is_op = 1;
    CREATE INDEX IF NOT EXISTS idx_threads_archived
        ON threads(board_id, archived, bumped_at DESC);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_chan_net_posts_remote
        ON chan_net_posts(remote_post_id, board_id);
    CREATE INDEX IF NOT EXISTS idx_user_thread_preferences_user_hidden
        ON user_thread_preferences(user_hash, hidden);
    CREATE INDEX IF NOT EXISTS idx_user_thread_preferences_thread
        ON user_thread_preferences(thread_id);
    CREATE INDEX IF NOT EXISTS idx_post_submissions_created_at
        ON post_submissions(created_at ASC);
";

const LEGACY_BASELINE_COLUMN_ADDITIONS: [(&str, &str, &str); 33] = [
    (
        "boards",
        "display_order",
        "ALTER TABLE boards ADD COLUMN display_order INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "max_archived_threads",
        "ALTER TABLE boards ADD COLUMN max_archived_threads INTEGER NOT NULL DEFAULT 150",
    ),
    (
        "boards",
        "allow_video",
        "ALTER TABLE boards ADD COLUMN allow_video INTEGER NOT NULL DEFAULT 1",
    ),
    (
        "boards",
        "allow_tripcodes",
        "ALTER TABLE boards ADD COLUMN allow_tripcodes INTEGER NOT NULL DEFAULT 1",
    ),
    (
        "boards",
        "allow_images",
        "ALTER TABLE boards ADD COLUMN allow_images INTEGER NOT NULL DEFAULT 1",
    ),
    (
        "boards",
        "allow_audio",
        "ALTER TABLE boards ADD COLUMN allow_audio INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "max_image_size",
        "ALTER TABLE boards ADD COLUMN max_image_size INTEGER NOT NULL DEFAULT 8388608",
    ),
    (
        "boards",
        "max_video_size",
        "ALTER TABLE boards ADD COLUMN max_video_size INTEGER NOT NULL DEFAULT 52428800",
    ),
    (
        "boards",
        "max_audio_size",
        "ALTER TABLE boards ADD COLUMN max_audio_size INTEGER NOT NULL DEFAULT 157286400",
    ),
    (
        "boards",
        "allow_pdf",
        "ALTER TABLE boards ADD COLUMN allow_pdf INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "allow_any_files",
        "ALTER TABLE boards ADD COLUMN allow_any_files INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "edit_window_secs",
        "ALTER TABLE boards ADD COLUMN edit_window_secs INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "allow_editing",
        "ALTER TABLE boards ADD COLUMN allow_editing INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "allow_self_delete",
        "ALTER TABLE boards ADD COLUMN allow_self_delete INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "allow_archive",
        "ALTER TABLE boards ADD COLUMN allow_archive INTEGER NOT NULL DEFAULT 1",
    ),
    (
        "boards",
        "allow_video_embeds",
        "ALTER TABLE boards ADD COLUMN allow_video_embeds INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "allow_captcha",
        "ALTER TABLE boards ADD COLUMN allow_captcha INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "show_poster_ids",
        "ALTER TABLE boards ADD COLUMN show_poster_ids INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "collapse_greentext",
        "ALTER TABLE boards ADD COLUMN collapse_greentext INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "post_cooldown_secs",
        "ALTER TABLE boards ADD COLUMN post_cooldown_secs INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "boards",
        "default_theme",
        "ALTER TABLE boards ADD COLUMN default_theme TEXT NOT NULL DEFAULT ''",
    ),
    (
        "boards",
        "banner_mode",
        "ALTER TABLE boards ADD COLUMN banner_mode TEXT NOT NULL DEFAULT 'inherit'
         CHECK (banner_mode IN ('inherit', 'none', 'override'))",
    ),
    (
        "boards",
        "access_mode",
        "ALTER TABLE boards ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'public'
         CHECK (access_mode IN ('public', 'view_password', 'post_password'))",
    ),
    (
        "boards",
        "access_password_hash",
        "ALTER TABLE boards ADD COLUMN access_password_hash TEXT NOT NULL DEFAULT ''",
    ),
    (
        "threads",
        "archived",
        "ALTER TABLE threads ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "posts",
        "media_type",
        "ALTER TABLE posts ADD COLUMN media_type TEXT",
    ),
    (
        "posts",
        "audio_file_path",
        "ALTER TABLE posts ADD COLUMN audio_file_path TEXT",
    ),
    (
        "posts",
        "audio_file_name",
        "ALTER TABLE posts ADD COLUMN audio_file_name TEXT",
    ),
    (
        "posts",
        "audio_file_size",
        "ALTER TABLE posts ADD COLUMN audio_file_size INTEGER",
    ),
    (
        "posts",
        "audio_mime_type",
        "ALTER TABLE posts ADD COLUMN audio_mime_type TEXT",
    ),
    (
        "posts",
        "edited_at",
        "ALTER TABLE posts ADD COLUMN edited_at INTEGER",
    ),
    (
        "posts",
        "media_processing_state",
        "ALTER TABLE posts ADD COLUMN media_processing_state TEXT NOT NULL DEFAULT ''",
    ),
    (
        "posts",
        "media_processing_error",
        "ALTER TABLE posts ADD COLUMN media_processing_error TEXT",
    ),
];

pub(super) fn install_or_migrate_schema(conn: &rusqlite::Connection) -> Result<()> {
    let fresh_database = is_fresh_database(conn)?;
    if fresh_database {
        install_post_squash_baseline(conn)?;
    } else {
        let schema_version = read_schema_version(conn)?;
        if schema_version == POST_SQUASH_SCHEMA_VERSION && has_post_squash_baseline_markers(conn)? {
            finish_baseline_schema(conn)?;
        } else {
            upgrade_pre_squash_database_to_v1(conn)?;
        }
    }

    Ok(())
}

fn install_post_squash_baseline(conn: &rusqlite::Connection) -> Result<()> {
    finish_baseline_schema(conn)?;
    stamp_schema_version(conn, POST_SQUASH_SCHEMA_VERSION)
}

fn finish_baseline_schema(conn: &rusqlite::Connection) -> Result<()> {
    create_base_tables(conn)?;
    // Post-squash databases can already be stamped at v1 while still missing
    // additive baseline columns introduced later. Keep the baseline repair
    // idempotent so existing installs receive new board/post/thread columns.
    ensure_legacy_columns_for_baseline(conn)?;
    create_indexes(conn)?;
    ensure_reports_table_integrity(conn)?;
    ensure_posts_ip_hash_nullable(conn)?;
    backfill_media_type(conn)?;
    // The posts table may be rebuilt by compatibility repairs above, so create
    // FTS and post triggers only after those table-level repairs are complete.
    ensure_posts_search_index(conn)?;
    ensure_post_invariants(conn)?;
    ensure_board_access_invariants(conn)?;
    Ok(())
}

fn upgrade_pre_squash_database_to_v1(conn: &rusqlite::Connection) -> Result<()> {
    // Legacy databases may have any old historical schema_version up through
    // the removed early-development ladder. Bring them to the same canonical
    // post-squash baseline as a fresh install, then stamp v1 only after every
    // compatibility step succeeds.
    create_base_tables(conn)?;
    ensure_legacy_columns_for_baseline(conn)?;
    create_indexes(conn)?;
    ensure_reports_table_integrity(conn)?;
    ensure_posts_ip_hash_nullable(conn)?;
    backfill_media_type(conn)?;
    ensure_posts_search_index(conn)?;
    ensure_post_invariants(conn)?;
    ensure_board_access_invariants(conn)?;
    stamp_schema_version(conn, POST_SQUASH_SCHEMA_VERSION)
}

fn create_base_tables(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(BASE_SCHEMA_SQL)
        .context("Schema table creation failed")
}

fn is_fresh_database(conn: &rusqlite::Connection) -> Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) = 0
         FROM sqlite_master
         WHERE type IN ('table', 'view', 'index', 'trigger')
           AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )
    .context("Failed to detect whether database is fresh")
}

fn has_post_squash_baseline_markers(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(object_exists(conn, "table", "banner_assets")?
        && column_exists(conn, "boards", "banner_mode")?
        && column_exists(conn, "boards", "access_mode")?
        && column_exists(conn, "threads", "archived")?
        && column_exists(conn, "posts", "media_processing_state")?)
}

fn object_exists(conn: &rusqlite::Connection, kind: &str, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM sqlite_master
            WHERE type = ?1 AND name = ?2
        )",
        rusqlite::params![kind, name],
        |row| row.get(0),
    )
    .with_context(|| format!("Failed to inspect schema object {kind}:{name}"))
}

fn create_indexes(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(INDEX_SCHEMA_SQL)
        .context("Schema index creation failed")
}

fn ensure_legacy_columns_for_baseline(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin legacy baseline column bridge failed")?;
    let result = (|| {
        for (table, column, sql) in LEGACY_BASELINE_COLUMN_ADDITIONS {
            ensure_column(conn, table, column, sql)?;
        }
        conn.execute_batch(
            "UPDATE boards
             SET display_order = id
             WHERE display_order = 0;

             UPDATE boards
             SET collapse_greentext = CASE
                 WHEN EXISTS (
                     SELECT 1
                     FROM site_settings
                     WHERE key = 'collapse_greentext'
                       AND (value = '1' OR lower(value) = 'true')
                 ) THEN 1
                 ELSE 0
             END
             WHERE collapse_greentext = 0;",
        )
        .context("Backfill legacy board baseline columns failed")?;
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .context("Commit legacy baseline column bridge failed"),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ensure_posts_search_index(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE VIRTUAL TABLE IF NOT EXISTS posts_fts
        USING fts5(body, content='posts', content_rowid='id', tokenize='unicode61');

        CREATE TRIGGER IF NOT EXISTS posts_ai AFTER INSERT ON posts BEGIN
            INSERT INTO posts_fts(rowid, body) VALUES (new.id, new.body);
        END;

        CREATE TRIGGER IF NOT EXISTS posts_ad AFTER DELETE ON posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, body) VALUES('delete', old.id, old.body);
        END;

        CREATE TRIGGER IF NOT EXISTS posts_au AFTER UPDATE OF body ON posts BEGIN
            INSERT INTO posts_fts(posts_fts, rowid, body) VALUES('delete', old.id, old.body);
            INSERT INTO posts_fts(rowid, body) VALUES (new.id, new.body);
        END;
        ",
    )
    .context("Search index creation failed")?;

    let post_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))
        .context("Failed to count posts for FTS validation")?;
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM posts_fts", [], |row| row.get(0))
        .context("Failed to count posts_fts rows")?;
    if post_count != fts_count {
        conn.execute_batch("INSERT INTO posts_fts(posts_fts) VALUES('rebuild');")
            .context("Failed to rebuild posts_fts index")?;
    }
    Ok(())
}

fn ensure_post_invariants(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE UNIQUE INDEX IF NOT EXISTS idx_posts_one_op_per_thread
            ON posts(thread_id)
            WHERE is_op = 1;

        CREATE TRIGGER IF NOT EXISTS posts_board_match_insert
        BEFORE INSERT ON posts
        FOR EACH ROW
        WHEN NEW.board_id != (SELECT board_id FROM threads WHERE id = NEW.thread_id)
        BEGIN
            SELECT RAISE(ABORT, 'posts.board_id must match thread board_id');
        END;

        CREATE TRIGGER IF NOT EXISTS posts_board_match_update
        BEFORE UPDATE OF thread_id, board_id ON posts
        FOR EACH ROW
        WHEN NEW.board_id != (SELECT board_id FROM threads WHERE id = NEW.thread_id)
        BEGIN
            SELECT RAISE(ABORT, 'posts.board_id must match thread board_id');
        END;
        ",
    )
    .context("Post invariant creation failed")
}

fn ensure_board_access_invariants(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        r"
        UPDATE boards
        SET access_mode = 'view_password'
        WHERE access_mode NOT IN ('public', 'view_password', 'post_password');

        CREATE TRIGGER IF NOT EXISTS boards_access_mode_insert
        BEFORE INSERT ON boards
        FOR EACH ROW
        WHEN NEW.access_mode NOT IN ('public', 'view_password', 'post_password')
        BEGIN
            SELECT RAISE(ABORT, 'boards.access_mode must be public, view_password, or post_password');
        END;

        CREATE TRIGGER IF NOT EXISTS boards_access_mode_update
        BEFORE UPDATE OF access_mode ON boards
        FOR EACH ROW
        WHEN NEW.access_mode NOT IN ('public', 'view_password', 'post_password')
        BEGIN
            SELECT RAISE(ABORT, 'boards.access_mode must be public, view_password, or post_password');
        END;

        CREATE TRIGGER IF NOT EXISTS boards_access_password_insert
        BEFORE INSERT ON boards
        FOR EACH ROW
        WHEN NEW.access_mode IN ('view_password', 'post_password') AND NEW.access_password_hash = ''
        BEGIN
            SELECT RAISE(ABORT, 'protected boards require access_password_hash');
        END;

        CREATE TRIGGER IF NOT EXISTS boards_access_password_update
        BEFORE UPDATE OF access_mode, access_password_hash ON boards
        FOR EACH ROW
        WHEN NEW.access_mode IN ('view_password', 'post_password') AND NEW.access_password_hash = ''
        BEGIN
            SELECT RAISE(ABORT, 'protected boards require access_password_hash');
        END;
        ",
    )
    .context("Board access invariant creation failed")
}

fn read_column_notnull(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool> {
    let query = format!("SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = ?1");
    let notnull: i64 = conn
        .query_row(&query, [column], |row| row.get(0))
        .with_context(|| format!("Failed to read {table}.{column} nullability"))?;
    Ok(notnull == 1)
}

fn ensure_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    add_column_sql: &str,
) -> Result<()> {
    if column_exists(conn, table, column)? {
        return Ok(());
    }

    conn.execute_batch(add_column_sql)
        .with_context(|| format!("Add legacy baseline column {table}.{column} failed"))
}

fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (
            SELECT 1
            FROM pragma_table_info(?1)
            WHERE name = ?2
        )",
        rusqlite::params![table, column],
        |row| row.get(0),
    )
    .with_context(|| format!("Failed to inspect schema column {table}.{column}"))
}

fn run_structural_migration(
    conn: &rusqlite::Connection,
    sql: &str,
    failure_context: &str,
    success_log: &str,
) -> Result<()> {
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .with_context(|| format!("Disable foreign keys for {failure_context}"))?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .with_context(|| format!("Begin transaction for {failure_context}"))?;

    match conn.execute_batch(sql) {
        Ok(()) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
                return Err(error).with_context(|| format!("Commit {failure_context}"));
            }
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .with_context(|| format!("Re-enable foreign keys after {failure_context}"))?;
            tracing::info!(target: "db", "{success_log}");
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            let _ = conn.execute_batch("PRAGMA foreign_keys = ON;");
            Err(error).context(failure_context.to_owned())
        }
    }
}

fn reports_has_full_foreign_keys(conn: &rusqlite::Connection) -> Result<bool> {
    let mut stmt = conn
        .prepare("SELECT \"from\", \"table\", on_delete FROM pragma_foreign_key_list('reports')")
        .context("Prepare reports foreign-key inspection failed")?;
    let foreign_keys = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("Query reports foreign keys failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("Read reports foreign keys failed")?;

    Ok(foreign_keys.iter().any(|(from, table, on_delete)| {
        from == "post_id" && table == "posts" && on_delete.eq_ignore_ascii_case("CASCADE")
    }) && foreign_keys.iter().any(|(from, table, on_delete)| {
        from == "thread_id" && table == "threads" && on_delete.eq_ignore_ascii_case("CASCADE")
    }) && foreign_keys.iter().any(|(from, table, on_delete)| {
        from == "board_id" && table == "boards" && on_delete.eq_ignore_ascii_case("CASCADE")
    }) && foreign_keys.iter().any(|(from, table, on_delete)| {
        from == "resolved_by"
            && table == "admin_users"
            && on_delete.eq_ignore_ascii_case("SET NULL")
    }))
}

fn ensure_reports_table_integrity(conn: &rusqlite::Connection) -> Result<()> {
    if reports_has_full_foreign_keys(conn)? {
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_reports_open_unique
             ON reports(post_id, reporter_hash)
             WHERE status = 'open';",
        )
        .context("Reports unique-index creation failed")?;
        return Ok(());
    }

    run_structural_migration(
        conn,
        r"
        CREATE TABLE reports_new (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            post_id        INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            thread_id      INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            board_id       INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            reason         TEXT NOT NULL DEFAULT '',
            reporter_hash  TEXT NOT NULL,
            status         TEXT NOT NULL DEFAULT 'open',
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            resolved_at    INTEGER,
            resolved_by    INTEGER REFERENCES admin_users(id) ON DELETE SET NULL
        );

        INSERT INTO reports_new
            (id, post_id, thread_id, board_id, reason, reporter_hash,
             status, created_at, resolved_at, resolved_by)
        SELECT r.id,
               r.post_id,
               p.thread_id,
               p.board_id,
               r.reason,
               r.reporter_hash,
               r.status,
               r.created_at,
               r.resolved_at,
               CASE
                   WHEN r.resolved_by IS NULL THEN NULL
                   WHEN EXISTS (
                       SELECT 1 FROM admin_users au
                       WHERE au.id = r.resolved_by
                   ) THEN r.resolved_by
                   ELSE NULL
               END
        FROM reports r
        JOIN posts p ON p.id = r.post_id;

        DROP TABLE reports;
        ALTER TABLE reports_new RENAME TO reports;

        CREATE INDEX idx_reports_status
            ON reports(status, created_at DESC);
        CREATE UNIQUE INDEX idx_reports_open_unique
            ON reports(post_id, reporter_hash)
            WHERE status = 'open';
        ",
        "Structural migration: rebuild reports table with full foreign keys failed",
        "Applied structural migration: reports table integrity hardened",
    )
}

fn ensure_posts_ip_hash_nullable(conn: &rusqlite::Connection) -> Result<()> {
    if read_column_notnull(conn, "posts", "ip_hash")? {
        run_structural_migration(
            conn,
            "CREATE TABLE posts_new (
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
                 edited_at        INTEGER,
                 media_processing_state TEXT NOT NULL DEFAULT '',
                 media_processing_error TEXT
             );

             INSERT INTO posts_new (
                 id, thread_id, board_id, name, tripcode, subject, body, body_html,
                 ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                 created_at, deletion_token, is_op, media_type, audio_file_path,
                 audio_file_name, audio_file_size, audio_mime_type, edited_at,
                 media_processing_state, media_processing_error
             )
             SELECT
                 id, thread_id, board_id, name, tripcode, subject, body, body_html,
                 ip_hash, file_path, file_name, file_size, thumb_path, mime_type,
                 created_at, deletion_token, is_op, media_type, audio_file_path,
                 audio_file_name, audio_file_size, audio_mime_type, edited_at,
                 '' AS media_processing_state,
                 NULL AS media_processing_error
             FROM posts;
             DROP TABLE posts;
             ALTER TABLE posts_new RENAME TO posts;

             CREATE INDEX IF NOT EXISTS idx_posts_thread
                 ON posts(thread_id, created_at ASC);
             CREATE INDEX IF NOT EXISTS idx_posts_board
                 ON posts(board_id, created_at DESC);
             CREATE INDEX IF NOT EXISTS idx_posts_thread_id
                 ON posts(thread_id);
             CREATE INDEX IF NOT EXISTS idx_posts_media_processing_state
                 ON posts(media_processing_state);
             CREATE INDEX IF NOT EXISTS idx_posts_ip_hash
                 ON posts(ip_hash);",
            "Structural migration: make posts.ip_hash nullable failed",
            "Applied structural migration: posts.ip_hash is now nullable",
        )?;
    }

    Ok(())
}

fn backfill_media_type(conn: &rusqlite::Connection) -> Result<()> {
    let needs_backfill: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM posts WHERE media_type IS NULL AND file_path IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .context("Failed to count posts needing media_type backfill")?;

    if needs_backfill > 0 {
        conn.execute_batch(
            "UPDATE posts
             SET media_type = CASE
                 WHEN file_path LIKE '%.jpg'  OR file_path LIKE '%.jpeg' OR
                      file_path LIKE '%.png'  OR file_path LIKE '%.gif'  OR
                      file_path LIKE '%.webp' OR file_path LIKE '%.heic' OR
                      file_path LIKE '%.heif' THEN 'image'
                 WHEN file_path LIKE '%.mp4'  OR file_path LIKE '%.webm' THEN 'video'
                 WHEN file_path LIKE '%.mp3'  OR file_path LIKE '%.ogg'  OR
                      file_path LIKE '%.flac' OR file_path LIKE '%.wav'  OR
                      file_path LIKE '%.m4a'  OR file_path LIKE '%.aac'  OR
                      file_path LIKE '%.opus' THEN 'audio'
                 WHEN file_path LIKE '%.pdf' THEN 'pdf'
                 ELSE 'other'
             END
             WHERE media_type IS NULL AND file_path IS NOT NULL;",
        )
        .context("Failed to backfill media_type column")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::install_or_migrate_schema;
    use crate::db::migrations::POST_SQUASH_SCHEMA_VERSION;

    fn schema_version(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .expect("read schema_version")
    }

    fn object_exists(conn: &rusqlite::Connection, kind: &str, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM sqlite_master
                WHERE type = ?1 AND name = ?2
            )",
            rusqlite::params![kind, name],
            |row| row.get(0),
        )
        .expect("inspect sqlite object")
    }

    fn table_has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM pragma_table_info(?1)
                WHERE name = ?2
            )",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .expect("inspect table column")
    }

    fn create_representative_legacy_schema(conn: &rusqlite::Connection, version: i64) {
        conn.execute_batch(
            r"
            CREATE TABLE boards (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                short_name  TEXT NOT NULL UNIQUE,
                name        TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                nsfw        INTEGER NOT NULL DEFAULT 0,
                max_threads INTEGER NOT NULL DEFAULT 150,
                bump_limit  INTEGER NOT NULL DEFAULT 500,
                created_at  INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE TABLE threads (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                board_id    INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
                subject     TEXT,
                created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
                bumped_at   INTEGER NOT NULL DEFAULT (unixepoch()),
                locked      INTEGER NOT NULL DEFAULT 0,
                sticky      INTEGER NOT NULL DEFAULT 0,
                reply_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE posts (
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
            CREATE TABLE site_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );",
        )
        .expect("create representative legacy schema");

        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )
        .expect("insert legacy schema version");
    }

    fn insert_legacy_thread_with_post(conn: &rusqlite::Connection) {
        conn.execute_batch(
            r"
            INSERT INTO site_settings (key, value) VALUES ('collapse_greentext', 'true');
            INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
            INSERT INTO threads (id, board_id, subject) VALUES (10, 1, 'legacy subject');
            INSERT INTO posts (
                id, thread_id, board_id, body, body_html, ip_hash,
                file_path, file_name, mime_type, deletion_token, is_op
            )
            VALUES (
                100, 10, 1, 'legacy searchable body', '<p>legacy searchable body</p>',
                'old-ip', 'uploads/a.png', 'a.png', 'image/png', 'tok', 1
            );",
        )
        .expect("insert representative legacy data");
    }

    fn insert_legacy_duplicate_ops(conn: &rusqlite::Connection) {
        conn.execute_batch(
            r"
            INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
            INSERT INTO threads (id, board_id, subject) VALUES (10, 1, 'legacy subject');
            INSERT INTO posts (id, thread_id, board_id, body, body_html, ip_hash, deletion_token, is_op)
            VALUES (100, 10, 1, 'first op', 'first op', 'ip-a', 'tok-a', 1);
            INSERT INTO posts (id, thread_id, board_id, body, body_html, ip_hash, deletion_token, is_op)
            VALUES (101, 10, 1, 'duplicate op', 'duplicate op', 'ip-b', 'tok-b', 1);",
        )
        .expect("insert invalid legacy data");
    }

    fn create_partial_post_squash_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            r"
            CREATE TABLE boards (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                display_order        INTEGER NOT NULL DEFAULT 0,
                short_name           TEXT NOT NULL UNIQUE,
                name                 TEXT NOT NULL,
                description          TEXT NOT NULL DEFAULT '',
                nsfw                 INTEGER NOT NULL DEFAULT 0,
                max_threads          INTEGER NOT NULL DEFAULT 150,
                max_archived_threads INTEGER NOT NULL DEFAULT 150,
                bump_limit           INTEGER NOT NULL DEFAULT 500,
                allow_video          INTEGER NOT NULL DEFAULT 1,
                allow_tripcodes      INTEGER NOT NULL DEFAULT 1,
                allow_images         INTEGER NOT NULL DEFAULT 1,
                allow_audio          INTEGER NOT NULL DEFAULT 0,
                allow_any_files      INTEGER NOT NULL DEFAULT 0,
                edit_window_secs     INTEGER NOT NULL DEFAULT 0,
                allow_editing        INTEGER NOT NULL DEFAULT 1,
                allow_video_embeds   INTEGER NOT NULL DEFAULT 1,
                allow_captcha        INTEGER NOT NULL DEFAULT 0,
                show_poster_ids      INTEGER NOT NULL DEFAULT 1,
                collapse_greentext   INTEGER NOT NULL DEFAULT 0,
                post_cooldown_secs   INTEGER NOT NULL DEFAULT 0,
                default_theme        TEXT NOT NULL DEFAULT '',
                banner_mode          TEXT NOT NULL DEFAULT 'inherit'
                                         CHECK (banner_mode IN ('inherit', 'none', 'override')),
                access_mode          TEXT NOT NULL DEFAULT 'public'
                                         CHECK (access_mode IN ('public', 'view_password', 'post_password')),
                access_password_hash TEXT NOT NULL DEFAULT '',
                created_at           INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE TABLE threads (
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
            CREATE TABLE posts (
                id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id              INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                board_id               INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
                name                   TEXT NOT NULL DEFAULT 'Anonymous',
                tripcode               TEXT,
                subject                TEXT,
                body                   TEXT NOT NULL,
                body_html              TEXT NOT NULL,
                ip_hash                TEXT,
                file_path              TEXT,
                file_name              TEXT,
                file_size              INTEGER,
                thumb_path             TEXT,
                mime_type              TEXT,
                created_at             INTEGER NOT NULL DEFAULT (unixepoch()),
                deletion_token         TEXT NOT NULL,
                is_op                  INTEGER NOT NULL DEFAULT 0,
                media_type             TEXT,
                audio_file_path        TEXT,
                audio_file_name        TEXT,
                audio_file_size        INTEGER,
                audio_mime_type        TEXT,
                edited_at              INTEGER,
                media_processing_state TEXT NOT NULL DEFAULT '',
                media_processing_error TEXT
            );
            CREATE TABLE site_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE banner_assets (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                scope_type      TEXT NOT NULL,
                board_id        INTEGER REFERENCES boards(id) ON DELETE CASCADE,
                storage_key     TEXT NOT NULL UNIQUE,
                width           INTEGER NOT NULL,
                height          INTEGER NOT NULL,
                file_size       INTEGER NOT NULL,
                enabled         INTEGER NOT NULL DEFAULT 1,
                sort_order      INTEGER NOT NULL DEFAULT 0,
                target_type     TEXT NOT NULL DEFAULT 'none',
                target_value    TEXT NOT NULL DEFAULT '',
                show_on_index   INTEGER NOT NULL DEFAULT 1,
                show_on_catalog INTEGER NOT NULL DEFAULT 1,
                created_at      INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (1);
            INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
            ",
        )
        .expect("create partial post-squash schema");
    }

    #[test]
    fn fresh_database_installs_canonical_post_squash_baseline() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        install_or_migrate_schema(&conn).expect("install schema");

        assert_eq!(schema_version(&conn), POST_SQUASH_SCHEMA_VERSION);
        assert!(object_exists(&conn, "table", "boards"));
        assert!(object_exists(&conn, "table", "posts"));
        assert!(object_exists(&conn, "table", "posts_fts"));
        assert!(object_exists(&conn, "index", "idx_posts_one_op_per_thread"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_banner_assets_scope_sort"
        ));
        assert!(object_exists(&conn, "trigger", "posts_ai"));
        assert!(object_exists(&conn, "trigger", "posts_board_match_insert"));
        assert!(object_exists(&conn, "trigger", "boards_access_mode_insert"));
    }

    #[test]
    fn legacy_posts_ip_hash_rebuild_keeps_posts_triggers_and_indexes() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            r"
            CREATE TABLE posts (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                board_id         INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
                name             TEXT NOT NULL DEFAULT 'Anonymous',
                tripcode         TEXT,
                subject          TEXT,
                body             TEXT NOT NULL,
                body_html        TEXT NOT NULL,
                ip_hash          TEXT NOT NULL,
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
                edited_at        INTEGER,
                media_processing_state TEXT NOT NULL DEFAULT '',
                media_processing_error TEXT
            );
            CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);
            ",
        )
        .expect("create legacy posts table");

        install_or_migrate_schema(&conn).expect("install schema");

        let posts_ai_exists: bool = conn
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'trigger' AND name = 'posts_ai'
                )",
                [],
                |row| row.get(0),
            )
            .expect("check posts_ai trigger");
        let board_match_trigger_exists: bool = conn
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'trigger' AND name = 'posts_board_match_insert'
                )",
                [],
                |row| row.get(0),
            )
            .expect("check board-match trigger");
        let one_op_index_exists: bool = conn
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'index' AND name = 'idx_posts_one_op_per_thread'
                )",
                [],
                |row| row.get(0),
            )
            .expect("check one-op index");

        assert!(posts_ai_exists);
        assert!(board_match_trigger_exists);
        assert!(one_op_index_exists);

        conn.execute(
            "INSERT INTO boards (short_name, name) VALUES ('test', 'Test')",
            [],
        )
        .expect("insert board");
        let board_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'subject')",
            [board_id],
        )
        .expect("insert thread");
        let thread_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO posts (thread_id, board_id, body, body_html, deletion_token, is_op)
             VALUES (?1, ?2, 'searchable body', 'searchable body', 'token', 1)",
            (thread_id, board_id),
        )
        .expect("insert post");

        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts_fts", [], |row| row.get(0))
            .expect("read posts_fts count");
        assert_eq!(fts_count, 1);
    }

    #[test]
    fn legacy_database_upgrades_to_post_squash_baseline_and_preserves_data() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_representative_legacy_schema(&conn, 4);
        insert_legacy_thread_with_post(&conn);

        install_or_migrate_schema(&conn).expect("upgrade legacy schema");

        assert_eq!(schema_version(&conn), POST_SQUASH_SCHEMA_VERSION);
        assert!(table_has_column(&conn, "boards", "banner_mode"));
        assert!(table_has_column(&conn, "boards", "access_password_hash"));
        assert!(table_has_column(&conn, "threads", "archived"));
        assert!(table_has_column(&conn, "posts", "media_processing_state"));
        assert!(object_exists(&conn, "table", "banner_assets"));
        assert!(object_exists(&conn, "table", "posts_fts"));
        assert!(object_exists(
            &conn,
            "index",
            "idx_posts_media_processing_state"
        ));
        assert!(object_exists(&conn, "trigger", "posts_ai"));
        assert!(object_exists(&conn, "trigger", "posts_board_match_insert"));

        let post: (String, Option<String>, String) = conn
            .query_row(
                "SELECT body, media_type, ip_hash FROM posts WHERE id = 100",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read preserved post");
        assert_eq!(
            post,
            (
                "legacy searchable body".to_string(),
                Some("image".to_string()),
                "old-ip".to_string()
            )
        );

        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts_fts", [], |row| row.get(0))
            .expect("read posts_fts count");
        assert_eq!(fts_count, 1);

        let collapse_greentext: i64 = conn
            .query_row(
                "SELECT collapse_greentext FROM boards WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("read collapse_greentext");
        assert_eq!(collapse_greentext, 1);
    }

    #[test]
    fn historical_v1_database_is_still_detected_as_legacy() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_representative_legacy_schema(&conn, POST_SQUASH_SCHEMA_VERSION);
        insert_legacy_thread_with_post(&conn);

        install_or_migrate_schema(&conn).expect("upgrade historical v1 schema");

        assert_eq!(schema_version(&conn), POST_SQUASH_SCHEMA_VERSION);
        assert!(table_has_column(&conn, "boards", "banner_mode"));
        assert!(table_has_column(&conn, "posts", "media_processing_state"));
        assert!(object_exists(&conn, "table", "banner_assets"));
        assert!(object_exists(&conn, "trigger", "posts_ai"));
    }

    #[test]
    fn post_squash_database_repairs_missing_additive_board_columns() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_partial_post_squash_schema(&conn);

        install_or_migrate_schema(&conn).expect("repair partial post-squash schema");

        assert_eq!(schema_version(&conn), POST_SQUASH_SCHEMA_VERSION);
        assert!(table_has_column(&conn, "boards", "allow_self_delete"));
        assert!(table_has_column(&conn, "boards", "allow_archive"));
        assert!(table_has_column(&conn, "boards", "allow_pdf"));

        let flags: (i64, i64, i64) = conn
            .query_row(
                "SELECT allow_self_delete, allow_archive, allow_pdf FROM boards WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read repaired board flags");
        assert_eq!(flags, (0, 1, 0));
    }

    #[test]
    fn fresh_schema_uses_new_board_feature_defaults() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");

        install_or_migrate_schema(&conn).expect("install schema");
        conn.execute(
            "INSERT INTO boards (short_name, name) VALUES ('fresh', 'Fresh Board')",
            [],
        )
        .expect("insert board with schema defaults");

        let flags: (i64, i64, i64, i64, i64) = conn
            .query_row(
                "SELECT allow_audio, allow_video_embeds, show_poster_ids, allow_editing, allow_self_delete
                 FROM boards WHERE short_name = 'fresh'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read fresh-schema board defaults");
        assert_eq!(
            flags,
            (
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_AUDIO),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_SHOW_POSTER_IDS),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_EDITING),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE),
            )
        );
    }

    #[test]
    fn failed_legacy_upgrade_does_not_stamp_post_squash_version() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_representative_legacy_schema(&conn, 36);
        insert_legacy_duplicate_ops(&conn);

        let error = install_or_migrate_schema(&conn).expect_err("legacy upgrade should fail");
        assert!(
            error.to_string().contains("Schema index creation failed")
                || error.to_string().contains("Post invariant creation failed"),
            "unexpected error: {error:#}"
        );
        assert_eq!(schema_version(&conn), 36);
    }
}
