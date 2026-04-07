// src/db/schema.rs

use anyhow::{Context, Result};

use super::migrations::{apply_migrations, CURRENT_MAX_MIGRATION};

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
        allow_any_files INTEGER NOT NULL DEFAULT 0,
        edit_window_secs    INTEGER NOT NULL DEFAULT 0,
        allow_editing       INTEGER NOT NULL DEFAULT 0,
        allow_archive       INTEGER NOT NULL DEFAULT 1,
        allow_video_embeds  INTEGER NOT NULL DEFAULT 0,
        allow_captcha       INTEGER NOT NULL DEFAULT 0,
        show_poster_ids     INTEGER NOT NULL DEFAULT 0,
        collapse_greentext  INTEGER NOT NULL DEFAULT 0,
        post_cooldown_secs  INTEGER NOT NULL DEFAULT 0,
        default_theme       TEXT NOT NULL DEFAULT '',
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

pub(super) fn create_schema(conn: &rusqlite::Connection) -> Result<()> {
    create_base_schema(conn)?;

    let _ = CURRENT_MAX_MIGRATION;
    apply_migrations(conn)?;
    create_indexes(conn)?;
    ensure_reports_table_integrity(conn)?;
    ensure_posts_search_index(conn)?;
    ensure_post_invariants(conn)?;
    relax_posts_ip_hash(conn)?;
    backfill_media_type(conn)?;
    Ok(())
}

fn create_base_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(BASE_SCHEMA_SQL)
        .context("Schema table creation failed")
}

fn create_indexes(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(INDEX_SCHEMA_SQL)
        .context("Schema index creation failed")
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

fn read_column_notnull(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool> {
    let query = format!("SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = ?1");
    let notnull: i64 = conn
        .query_row(&query, [column], |row| row.get(0))
        .with_context(|| format!("Failed to read {table}.{column} nullability"))?;
    Ok(notnull == 1)
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
            conn.execute_batch("COMMIT")
                .with_context(|| format!("Commit {failure_context}"))?;
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

fn relax_posts_ip_hash(conn: &rusqlite::Connection) -> Result<()> {
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
                      file_path LIKE '%.webp' THEN 'image'
                 WHEN file_path LIKE '%.mp4'  OR file_path LIKE '%.webm' THEN 'video'
                 WHEN file_path LIKE '%.mp3'  OR file_path LIKE '%.ogg'  OR
                      file_path LIKE '%.flac' OR file_path LIKE '%.wav'  OR
                      file_path LIKE '%.m4a'  OR file_path LIKE '%.aac'  OR
                      file_path LIKE '%.opus' THEN 'audio'
                 ELSE 'other'
             END
             WHERE media_type IS NULL AND file_path IS NOT NULL;",
        )
        .context("Failed to backfill media_type column")?;
    }

    Ok(())
}
