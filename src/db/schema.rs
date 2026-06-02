// src/db/schema.rs

use anyhow::{bail, Context as _, Result};
use std::collections::{BTreeMap, BTreeSet};

use super::migrations::{read_schema_version, stamp_schema_version, BASELINE_SCHEMA_VERSION};

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
        max_pdf_size    INTEGER NOT NULL DEFAULT 157286400,
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

const LEGACY_THEME_SORT_INDEX: &str = "idx_themes_enabled_sort";
const LEGACY_BOARD_ZERO_DEFAULT_COLUMNS: [&str; 4] = [
    "allow_editing",
    "allow_self_delete",
    "allow_video_embeds",
    "show_poster_ids",
];

pub(super) fn install_or_migrate_schema(conn: &rusqlite::Connection) -> Result<()> {
    if is_fresh_database(conn)? {
        install_baseline_schema(conn)?;
        return Ok(());
    }

    normalize_database_schema_version(conn)
}

pub(super) const fn baseline_schema_version() -> &'static str {
    BASELINE_SCHEMA_VERSION
}

pub(super) fn verify_database_schema(conn: &rusqlite::Connection) -> Result<()> {
    verify_database_schema_structure(conn)?;
    match read_schema_version(conn)? {
        Some(version) if version == BASELINE_SCHEMA_VERSION => Ok(()),
        Some(version) => {
            bail!("database schema version is {version}, expected {BASELINE_SCHEMA_VERSION}")
        }
        None => bail!("database schema version is missing, expected {BASELINE_SCHEMA_VERSION}"),
    }
}

pub(super) fn normalize_database_schema_version(conn: &rusqlite::Connection) -> Result<()> {
    repair_known_legacy_baseline_drift(conn)?;
    verify_database_schema_structure(conn)?;
    if read_schema_version(conn)?.as_deref() != Some(BASELINE_SCHEMA_VERSION) {
        stamp_schema_version(conn)?;
    }
    verify_database_schema(conn)
}

pub(super) fn database_schema_status_label(conn: &rusqlite::Connection) -> String {
    match verify_database_schema(conn) {
        Ok(()) => format!("{BASELINE_SCHEMA_VERSION} baseline verified"),
        Err(error) => format!("{BASELINE_SCHEMA_VERSION} baseline mismatch: {error}"),
    }
}

fn install_baseline_schema(conn: &rusqlite::Connection) -> Result<()> {
    create_baseline_schema_objects(conn)?;
    stamp_schema_version(conn)?;
    verify_database_schema(conn)
}

fn create_baseline_schema_objects(conn: &rusqlite::Connection) -> Result<()> {
    create_base_tables(conn)?;
    create_indexes(conn)?;
    ensure_posts_search_index(conn)?;
    ensure_post_invariants(conn)?;
    ensure_board_access_invariants(conn)
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

fn repair_known_legacy_baseline_drift(conn: &rusqlite::Connection) -> Result<()> {
    if !is_known_legacy_schema_version(read_schema_version(conn)?.as_deref()) {
        return Ok(());
    }

    let expected = expected_schema_shape()?;
    let actual = schema_shape(conn)?;
    if !can_repair_known_legacy_baseline_drift(&expected, &actual) {
        return Ok(());
    }

    if actual.objects.contains_key(LEGACY_THEME_SORT_INDEX) {
        conn.execute_batch("DROP INDEX IF EXISTS idx_themes_enabled_sort")
            .context("Drop legacy themes sort index failed")?;
    }

    if boards_table_requires_baseline_rebuild(&expected, &actual) {
        rebuild_boards_table_for_baseline(conn)?;
        ensure_board_access_invariants(conn)?;
    }

    Ok(())
}

fn is_known_legacy_schema_version(version: Option<&str>) -> bool {
    match version.and_then(|value| value.parse::<i64>().ok()) {
        Some(1..=41) => true,
        Some(_) | None => false,
    }
}

fn can_repair_known_legacy_baseline_drift(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    schema_objects_are_legacy_repairable(expected, actual)
        && tables_are_legacy_repairable(expected, actual)
}

fn schema_objects_are_legacy_repairable(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    for (name, expected_object) in &expected.objects {
        let Some(actual_object) = actual.objects.get(name) else {
            return false;
        };
        if actual_object.kind != expected_object.kind {
            return false;
        }
        if matches!(expected_object.kind.as_str(), "index" | "trigger")
            && actual_object.sql != expected_object.sql
        {
            return false;
        }
    }

    for (name, actual_object) in &actual.objects {
        if expected.objects.contains_key(name) {
            continue;
        }
        if name == LEGACY_THEME_SORT_INDEX
            && actual_object.kind == "index"
            && actual_object
                .sql
                .as_deref()
                .is_some_and(is_legacy_theme_sort_index)
        {
            continue;
        }
        return false;
    }

    true
}

fn is_legacy_theme_sort_index(sql: &str) -> bool {
    sql == normalize_schema_sql(
        "CREATE INDEX idx_themes_enabled_sort ON themes(enabled, sort_order, slug)",
    )
}

fn tables_are_legacy_repairable(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    for (table, expected_table) in &expected.tables {
        let Some(actual_table) = actual.tables.get(table) else {
            return false;
        };
        let repairable = match table.as_str() {
            "boards" => boards_table_is_legacy_repairable(expected_table, actual_table),
            "themes" => themes_table_is_legacy_repairable(expected_table, actual_table),
            _ => actual_table == expected_table,
        };
        if !repairable {
            return false;
        }
    }

    true
}

fn themes_table_is_legacy_repairable(expected: &TableShape, actual: &TableShape) -> bool {
    if actual.columns != expected.columns || actual.foreign_keys != expected.foreign_keys {
        return false;
    }

    let mut actual_indexes = actual.indexes.clone();
    actual_indexes.remove(LEGACY_THEME_SORT_INDEX);
    actual_indexes == expected.indexes
}

fn boards_table_is_legacy_repairable(expected: &TableShape, actual: &TableShape) -> bool {
    actual.foreign_keys == expected.foreign_keys
        && legacy_board_columns_match(expected, actual)
        && legacy_board_indexes_are_repairable(actual)
}

fn legacy_board_columns_match(expected: &TableShape, actual: &TableShape) -> bool {
    if actual.columns.len() != expected.columns.len() {
        return false;
    }

    for (column, expected_shape) in &expected.columns {
        let Some(actual_shape) = actual.columns.get(column) else {
            return false;
        };
        if !legacy_board_column_matches(column, expected_shape, actual_shape) {
            return false;
        }
    }

    true
}

fn legacy_board_column_matches(column: &str, expected: &ColumnShape, actual: &ColumnShape) -> bool {
    if actual == expected {
        return true;
    }
    if !LEGACY_BOARD_ZERO_DEFAULT_COLUMNS.contains(&column) {
        return false;
    }

    let mut legacy_expected = expected.clone();
    legacy_expected.default_value = Some("0".to_owned());
    actual == &legacy_expected
}

fn legacy_board_indexes_are_repairable(actual: &TableShape) -> bool {
    actual.indexes.len() == 1
        && actual.indexes.values().any(|index| {
            index.unique
                && index.origin == "u"
                && !index.partial
                && index.where_clause.is_none()
                && index.columns.len() == 1
                && index
                    .columns
                    .first()
                    .and_then(|column| column.name.as_deref())
                    == Some("short_name")
        })
}

fn boards_table_requires_baseline_rebuild(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    actual.tables.get("boards") != expected.tables.get("boards")
        || required_fragments_missing_for_table(actual, "boards")
}

fn required_fragments_missing_for_table(shape: &SchemaShape, table: &str) -> bool {
    let Some(object) = shape.objects.get(table) else {
        return true;
    };
    let Some(sql) = object.sql.as_deref() else {
        return true;
    };

    REQUIRED_TABLE_SQL_FRAGMENTS
        .iter()
        .filter(|(fragment_table, _)| *fragment_table == table)
        .any(|(_, fragment)| !sql.contains(&normalize_schema_sql(fragment)))
}

fn rebuild_boards_table_for_baseline(conn: &rusqlite::Connection) -> Result<()> {
    run_structural_schema_repair(
        conn,
        r"
        CREATE TABLE boards_new (
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
            max_pdf_size    INTEGER NOT NULL DEFAULT 157286400,
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

        INSERT INTO boards_new (
            id, display_order, short_name, name, description, nsfw, max_threads,
            max_archived_threads, bump_limit, allow_video, allow_tripcodes,
            allow_images, allow_audio, max_image_size, max_video_size,
            max_audio_size, max_pdf_size, allow_pdf, allow_any_files,
            edit_window_secs, allow_editing, allow_self_delete, allow_archive,
            allow_video_embeds, allow_captcha, show_poster_ids,
            collapse_greentext, post_cooldown_secs, default_theme,
            banner_mode, access_mode, access_password_hash, created_at
        )
        SELECT
            id, display_order, short_name, name, description, nsfw, max_threads,
            max_archived_threads, bump_limit, allow_video, allow_tripcodes,
            allow_images, allow_audio, max_image_size, max_video_size,
            max_audio_size, max_pdf_size, allow_pdf, allow_any_files,
            edit_window_secs, allow_editing, allow_self_delete, allow_archive,
            allow_video_embeds, allow_captcha, show_poster_ids,
            collapse_greentext, post_cooldown_secs, default_theme,
            CASE
                WHEN banner_mode IN ('inherit', 'none', 'override') THEN banner_mode
                ELSE 'inherit'
            END,
            CASE
                WHEN access_mode IN ('public', 'view_password', 'post_password') THEN access_mode
                ELSE 'view_password'
            END,
            access_password_hash, created_at
        FROM boards;

        DROP TABLE boards;
        ALTER TABLE boards_new RENAME TO boards;
        ",
        "Structural migration: rebuild boards table for 1.3.0 baseline failed",
        "Applied structural migration: boards table matches 1.3.0 baseline",
    )
}

fn run_structural_schema_repair(
    conn: &rusqlite::Connection,
    sql: &str,
    failure_context: &str,
    success_log: &str,
) -> Result<()> {
    let foreign_keys_enabled = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .context("Read foreign key setting before schema repair failed")?
        != 0;
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .with_context(|| format!("Disable foreign keys for {failure_context}"))?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .with_context(|| format!("Begin transaction for {failure_context}"))?;

    match conn.execute_batch(sql) {
        Ok(()) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                restore_foreign_keys(conn, foreign_keys_enabled);
                return Err(error).with_context(|| format!("Commit {failure_context}"));
            }
            restore_foreign_keys(conn, foreign_keys_enabled);
            tracing::info!(target: "db", "{success_log}");
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            restore_foreign_keys(conn, foreign_keys_enabled);
            Err(error).context(failure_context.to_owned())
        }
    }
}

fn restore_foreign_keys(conn: &rusqlite::Connection, enabled: bool) {
    let statement = if enabled {
        "PRAGMA foreign_keys = ON;"
    } else {
        "PRAGMA foreign_keys = OFF;"
    };
    let _ = conn.execute_batch(statement);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaShape {
    objects: BTreeMap<String, SchemaObject>,
    tables: BTreeMap<String, TableShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaObject {
    kind: String,
    sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableShape {
    columns: BTreeMap<String, ColumnShape>,
    foreign_keys: BTreeSet<ForeignKeyShape>,
    indexes: BTreeMap<String, IndexShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnShape {
    decl_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_position: i64,
    hidden: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ForeignKeyShape {
    table_name: String,
    from_column: String,
    to_column: Option<String>,
    on_update: String,
    on_delete: String,
    match_rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexShape {
    unique: bool,
    origin: String,
    partial: bool,
    columns: Vec<IndexColumnShape>,
    where_clause: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexColumnShape {
    seqno: i64,
    cid: i64,
    name: Option<String>,
    descending: bool,
    collation: Option<String>,
}

fn verify_database_schema_structure(conn: &rusqlite::Connection) -> Result<()> {
    let expected = expected_schema_shape()?;
    let actual = schema_shape(conn)?;
    let mut issues = Vec::new();

    compare_schema_objects(&expected, &actual, &mut issues);
    compare_tables(&expected, &actual, &mut issues);
    verify_required_sql_fragments(&actual, &mut issues);
    verify_sqlite_health(conn, &mut issues);

    if issues.is_empty() {
        Ok(())
    } else {
        bail!(
            "database does not match RustChan {BASELINE_SCHEMA_VERSION} baseline: {}",
            issues.join("; ")
        )
    }
}

fn expected_schema_shape() -> Result<SchemaShape> {
    let expected = rusqlite::Connection::open_in_memory()
        .context("Open in-memory baseline schema connection failed")?;
    create_baseline_schema_objects(&expected)?;
    schema_shape(&expected)
}

fn schema_shape(conn: &rusqlite::Connection) -> Result<SchemaShape> {
    let objects = schema_objects(conn)?;
    let mut tables = BTreeMap::new();
    for (name, object) in &objects {
        if object.kind == "table" {
            tables.insert(name.clone(), table_shape(conn, name)?);
        }
    }
    Ok(SchemaShape { objects, tables })
}

fn schema_objects(conn: &rusqlite::Connection) -> Result<BTreeMap<String, SchemaObject>> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql
             FROM sqlite_schema
             WHERE type IN ('table', 'index', 'trigger', 'view')
               AND name NOT LIKE 'sqlite_%'
               AND name <> 'schema_version'
             ORDER BY type, name",
        )
        .context("Prepare schema object inspection failed")?;
    let rows = stmt
        .query_map([], |row| {
            let kind: String = row.get(0)?;
            let name: String = row.get(1)?;
            let sql: Option<String> = row.get(2)?;
            Ok((
                name,
                SchemaObject {
                    kind,
                    sql: sql.map(|sql| normalize_schema_sql(&sql)),
                },
            ))
        })
        .context("Query schema objects failed")?;

    let mut objects = BTreeMap::new();
    for row in rows {
        let (name, object) = row.context("Read schema object failed")?;
        objects.insert(name, object);
    }
    Ok(objects)
}

fn table_shape(conn: &rusqlite::Connection, table: &str) -> Result<TableShape> {
    Ok(TableShape {
        columns: table_columns(conn, table)?,
        foreign_keys: table_foreign_keys(conn, table)?,
        indexes: table_indexes(conn, table)?,
    })
}

fn table_columns(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<BTreeMap<String, ColumnShape>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden
             FROM pragma_table_xinfo(?1)
             ORDER BY cid",
        )
        .with_context(|| format!("Prepare column inspection for {table} failed"))?;
    let rows = stmt
        .query_map([table], |row| {
            let name: String = row.get(0)?;
            Ok((
                name,
                ColumnShape {
                    decl_type: row.get(1)?,
                    not_null: row.get::<_, i64>(2)? == 1,
                    default_value: row.get(3)?,
                    primary_key_position: row.get(4)?,
                    hidden: row.get(5)?,
                },
            ))
        })
        .with_context(|| format!("Query columns for {table} failed"))?;

    let mut columns = BTreeMap::new();
    for row in rows {
        let (name, column) = row.with_context(|| format!("Read column for {table} failed"))?;
        columns.insert(name, column);
    }
    Ok(columns)
}

fn table_foreign_keys(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<BTreeSet<ForeignKeyShape>> {
    let mut stmt = conn
        .prepare(
            "SELECT \"table\", \"from\", \"to\", on_update, on_delete, match
             FROM pragma_foreign_key_list(?1)
             ORDER BY id, seq",
        )
        .with_context(|| format!("Prepare foreign-key inspection for {table} failed"))?;
    let rows = stmt
        .query_map([table], |row| {
            Ok(ForeignKeyShape {
                table_name: row.get(0)?,
                from_column: row.get(1)?,
                to_column: row.get(2)?,
                on_update: row.get(3)?,
                on_delete: row.get(4)?,
                match_rule: row.get(5)?,
            })
        })
        .with_context(|| format!("Query foreign keys for {table} failed"))?;

    let mut foreign_keys = BTreeSet::new();
    for row in rows {
        foreign_keys.insert(row.with_context(|| format!("Read foreign key for {table} failed"))?);
    }
    Ok(foreign_keys)
}

fn table_indexes(conn: &rusqlite::Connection, table: &str) -> Result<BTreeMap<String, IndexShape>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, \"unique\", origin, partial
             FROM pragma_index_list(?1)
             ORDER BY name",
        )
        .with_context(|| format!("Prepare index inspection for {table} failed"))?;
    let rows = stmt
        .query_map([table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? == 1,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? == 1,
            ))
        })
        .with_context(|| format!("Query indexes for {table} failed"))?;

    let mut indexes = BTreeMap::new();
    for row in rows {
        let (name, unique, origin, partial) =
            row.with_context(|| format!("Read index for {table} failed"))?;
        let sql = if name.starts_with("sqlite_") {
            None
        } else {
            schema_sql_for_object(conn, "index", &name)?
        };
        indexes.insert(
            name.clone(),
            IndexShape {
                unique,
                origin,
                partial,
                columns: index_columns(conn, &name)?,
                where_clause: sql.as_deref().and_then(index_where_clause),
            },
        );
    }
    Ok(indexes)
}

fn index_columns(conn: &rusqlite::Connection, index: &str) -> Result<Vec<IndexColumnShape>> {
    let mut stmt = conn
        .prepare(
            "SELECT seqno, cid, name, \"desc\", coll, key
             FROM pragma_index_xinfo(?1)
             ORDER BY seqno",
        )
        .with_context(|| format!("Prepare index-column inspection for {index} failed"))?;
    let rows = stmt
        .query_map([index], |row| {
            Ok((
                row.get::<_, i64>(5)? == 1,
                IndexColumnShape {
                    seqno: row.get(0)?,
                    cid: row.get(1)?,
                    name: row.get(2)?,
                    descending: row.get::<_, i64>(3)? == 1,
                    collation: row.get(4)?,
                },
            ))
        })
        .with_context(|| format!("Query index columns for {index} failed"))?;

    let mut columns = Vec::new();
    for row in rows {
        let (is_key_column, column) =
            row.with_context(|| format!("Read index column for {index} failed"))?;
        if is_key_column {
            columns.push(column);
        }
    }
    Ok(columns)
}

fn schema_sql_for_object(
    conn: &rusqlite::Connection,
    kind: &str,
    name: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = ?1 AND name = ?2",
        rusqlite::params![kind, name],
        |row| row.get::<_, Option<String>>(0),
    )
    .with_context(|| format!("Inspect SQL for schema object {kind}:{name}"))
}

fn compare_schema_objects(expected: &SchemaShape, actual: &SchemaShape, issues: &mut Vec<String>) {
    for (name, expected_object) in &expected.objects {
        match actual.objects.get(name) {
            Some(actual_object) if actual_object.kind == expected_object.kind => {
                if matches!(expected_object.kind.as_str(), "index" | "trigger")
                    && actual_object.sql != expected_object.sql
                {
                    issues.push(format!("{name} definition differs from baseline"));
                }
            }
            Some(actual_object) => issues.push(format!(
                "{name} is a {}, expected {}",
                actual_object.kind, expected_object.kind
            )),
            None => issues.push(format!("missing {} {name}", expected_object.kind)),
        }
    }

    for (name, actual_object) in &actual.objects {
        if !expected.objects.contains_key(name) {
            issues.push(format!("unexpected {} {name}", actual_object.kind));
        }
    }
}

fn compare_tables(expected: &SchemaShape, actual: &SchemaShape, issues: &mut Vec<String>) {
    for (table, expected_table) in &expected.tables {
        let Some(actual_table) = actual.tables.get(table) else {
            continue;
        };
        compare_columns(
            table,
            &expected_table.columns,
            &actual_table.columns,
            issues,
        );
        if actual_table.foreign_keys != expected_table.foreign_keys {
            issues.push(format!("{table} foreign keys differ from baseline"));
        }
        if actual_table.indexes != expected_table.indexes {
            issues.push(format!("{table} indexes differ from baseline"));
        }
    }
}

fn compare_columns(
    table: &str,
    expected: &BTreeMap<String, ColumnShape>,
    actual: &BTreeMap<String, ColumnShape>,
    issues: &mut Vec<String>,
) {
    for (column, expected_shape) in expected {
        match actual.get(column) {
            Some(actual_shape) if actual_shape == expected_shape => {}
            Some(_) => issues.push(format!("{table}.{column} definition differs from baseline")),
            None => issues.push(format!("missing column {table}.{column}")),
        }
    }

    for column in actual.keys() {
        if !expected.contains_key(column) {
            issues.push(format!("unexpected column {table}.{column}"));
        }
    }
}

const REQUIRED_TABLE_SQL_FRAGMENTS: [(&str, &str); 2] = [
    (
        "boards",
        "CHECK (banner_mode IN ('inherit', 'none', 'override'))",
    ),
    (
        "boards",
        "CHECK (access_mode IN ('public', 'view_password', 'post_password'))",
    ),
];

fn verify_required_sql_fragments(shape: &SchemaShape, issues: &mut Vec<String>) {
    for (table, fragment) in REQUIRED_TABLE_SQL_FRAGMENTS {
        let Some(object) = shape.objects.get(table) else {
            continue;
        };
        let Some(sql) = object.sql.as_deref() else {
            issues.push(format!("{table} SQL definition is missing"));
            continue;
        };
        let normalized_fragment = normalize_schema_sql(fragment);
        if !sql.contains(&normalized_fragment) {
            issues.push(format!("{table} is missing required constraint {fragment}"));
        }
    }
}

fn verify_sqlite_health(conn: &rusqlite::Connection, issues: &mut Vec<String>) {
    match conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) if result.eq_ignore_ascii_case("ok") => {}
        Ok(result) => issues.push(format!("quick_check reported {result}")),
        Err(error) => issues.push(format!("quick_check failed: {error}")),
    }

    let mut stmt = match conn.prepare("PRAGMA foreign_key_check") {
        Ok(stmt) => stmt,
        Err(error) => {
            issues.push(format!("foreign_key_check failed: {error}"));
            return;
        }
    };
    let rows = match stmt.query_map([], |row| {
        let table: String = row.get(0)?;
        let rowid: Option<i64> = row.get(1)?;
        let parent: String = row.get(2)?;
        Ok(format!(
            "{table} rowid={} parent={parent}",
            rowid.map_or_else(|| "unknown".to_owned(), |rowid| rowid.to_string())
        ))
    }) {
        Ok(rows) => rows,
        Err(error) => {
            issues.push(format!("foreign_key_check failed: {error}"));
            return;
        }
    };

    let mut violations = Vec::new();
    for row in rows {
        match row {
            Ok(message) => violations.push(message),
            Err(error) => {
                issues.push(format!("foreign_key_check row failed: {error}"));
                return;
            }
        }
    }
    if !violations.is_empty() {
        issues.push(format!(
            "foreign_key_check reported {} violation(s): {}",
            violations.len(),
            violations.join(", ")
        ));
    }
}

fn index_where_clause(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let where_index = lower.find(" where ")?;
    sql.get(where_index..).map(str::trim).map(str::to_owned)
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('"', "")
}

#[cfg(test)]
mod tests {
    use super::{
        baseline_schema_version, create_baseline_schema_objects, ensure_board_access_invariants,
        install_or_migrate_schema, normalize_database_schema_version, read_schema_version,
        verify_database_schema,
    };

    fn schema_version(conn: &rusqlite::Connection) -> String {
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

    fn column_default(conn: &rusqlite::Connection, table: &str, column: &str) -> Option<String> {
        conn.query_row(
            "SELECT dflt_value FROM pragma_table_info(?1) WHERE name = ?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )
        .expect("read column default")
    }

    fn rebuild_boards_with_historical_v41_shape(conn: &rusqlite::Connection) {
        conn.execute_batch(
            r"
            PRAGMA foreign_keys = OFF;
            BEGIN IMMEDIATE;

            CREATE TABLE boards_legacy (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                short_name      TEXT NOT NULL UNIQUE,
                name            TEXT NOT NULL,
                description     TEXT NOT NULL DEFAULT '',
                nsfw            INTEGER NOT NULL DEFAULT 0,
                max_threads     INTEGER NOT NULL DEFAULT 150,
                bump_limit      INTEGER NOT NULL DEFAULT 500,
                created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
                display_order   INTEGER NOT NULL DEFAULT 0,
                max_archived_threads INTEGER NOT NULL DEFAULT 150,
                allow_video     INTEGER NOT NULL DEFAULT 1,
                allow_tripcodes INTEGER NOT NULL DEFAULT 1,
                allow_images    INTEGER NOT NULL DEFAULT 1,
                allow_audio     INTEGER NOT NULL DEFAULT 0,
                max_image_size  INTEGER NOT NULL DEFAULT 8388608,
                max_video_size  INTEGER NOT NULL DEFAULT 52428800,
                max_audio_size  INTEGER NOT NULL DEFAULT 157286400,
                max_pdf_size    INTEGER NOT NULL DEFAULT 157286400,
                allow_pdf       INTEGER NOT NULL DEFAULT 0,
                allow_any_files INTEGER NOT NULL DEFAULT 0,
                edit_window_secs    INTEGER NOT NULL DEFAULT 0,
                allow_editing       INTEGER NOT NULL DEFAULT 0,
                allow_self_delete   INTEGER NOT NULL DEFAULT 0,
                allow_archive       INTEGER NOT NULL DEFAULT 1,
                allow_video_embeds  INTEGER NOT NULL DEFAULT 0,
                allow_captcha       INTEGER NOT NULL DEFAULT 0,
                show_poster_ids     INTEGER NOT NULL DEFAULT 0,
                collapse_greentext  INTEGER NOT NULL DEFAULT 0,
                post_cooldown_secs  INTEGER NOT NULL DEFAULT 0,
                default_theme       TEXT NOT NULL DEFAULT '',
                access_mode         TEXT NOT NULL DEFAULT 'public',
                access_password_hash TEXT NOT NULL DEFAULT '',
                banner_mode         TEXT NOT NULL DEFAULT 'inherit'
            );

            INSERT INTO boards_legacy (
                id, short_name, name, description, nsfw, max_threads,
                bump_limit, created_at, display_order, max_archived_threads,
                allow_video, allow_tripcodes, allow_images, allow_audio,
                max_image_size, max_video_size, max_audio_size, max_pdf_size,
                allow_pdf, allow_any_files, edit_window_secs, allow_editing,
                allow_self_delete, allow_archive, allow_video_embeds,
                allow_captcha, show_poster_ids, collapse_greentext,
                post_cooldown_secs, default_theme, access_mode,
                access_password_hash, banner_mode
            )
            SELECT
                id, short_name, name, description, nsfw, max_threads,
                bump_limit, created_at, display_order, max_archived_threads,
                allow_video, allow_tripcodes, allow_images, allow_audio,
                max_image_size, max_video_size, max_audio_size, max_pdf_size,
                allow_pdf, allow_any_files, edit_window_secs, allow_editing,
                allow_self_delete, allow_archive, allow_video_embeds,
                allow_captcha, show_poster_ids, collapse_greentext,
                post_cooldown_secs, default_theme, access_mode,
                access_password_hash, banner_mode
            FROM boards;

            DROP TABLE boards;
            ALTER TABLE boards_legacy RENAME TO boards;

            COMMIT;
            PRAGMA foreign_keys = ON;
            ",
        )
        .expect("rebuild boards into historical v41 shape");
        ensure_board_access_invariants(conn).expect("restore board access triggers");
    }

    #[test]
    fn fresh_database_installs_130_baseline_directly() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        install_or_migrate_schema(&conn).expect("install schema");

        assert_eq!(schema_version(&conn), "1.3.0");
        assert_eq!(baseline_schema_version(), "1.3.0");
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
        verify_database_schema(&conn).expect("fresh schema verifies");
    }

    #[test]
    fn existing_current_schema_is_adopted_as_130_without_data_loss() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_baseline_schema_objects(&conn).expect("create baseline objects");
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);
            INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
            INSERT INTO threads (id, board_id, subject) VALUES (10, 1, 'subject');
            INSERT INTO posts (id, thread_id, board_id, body, body_html, deletion_token, is_op)
            VALUES (100, 10, 1, 'preserved body', '<p>preserved body</p>', 'tok', 1);",
        )
        .expect("seed adoptable current schema");

        install_or_migrate_schema(&conn).expect("adopt current schema");

        assert_eq!(schema_version(&conn), "1.3.0");
        let body: String = conn
            .query_row("SELECT body FROM posts WHERE id = 100", [], |row| {
                row.get(0)
            })
            .expect("read preserved post");
        assert_eq!(body, "preserved body");
    }

    #[test]
    fn historical_v41_schema_drift_repairs_to_130_baseline_without_data_loss() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_baseline_schema_objects(&conn).expect("create baseline objects");
        conn.execute(
            "INSERT INTO boards (
                id, short_name, name, allow_editing, allow_self_delete,
                allow_video_embeds, show_poster_ids, banner_mode, access_mode
             )
             VALUES (1, 'b', 'Random', 0, 1, 0, 1, 'override', 'public')",
            [],
        )
        .expect("insert board before legacy reshape");
        rebuild_boards_with_historical_v41_shape(&conn);
        conn.execute_batch(
            "CREATE INDEX idx_themes_enabled_sort
                 ON themes(enabled, sort_order, slug);
             CREATE TABLE schema_version (
                 version INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(version)
             );
             INSERT INTO schema_version (version) VALUES (41);",
        )
        .expect("create historical v41 drift markers");

        let error =
            verify_database_schema(&conn).expect_err("legacy drift should fail strict check");
        let message = error.to_string();
        assert!(
            message.contains("idx_themes_enabled_sort")
                && message.contains("boards.allow_editing definition differs")
                && message.contains("missing required constraint CHECK (access_mode"),
            "unexpected strict-check error: {error:#}"
        );

        install_or_migrate_schema(&conn).expect("repair historical v41 schema drift");

        assert_eq!(schema_version(&conn), "1.3.0");
        assert!(!object_exists(&conn, "index", "idx_themes_enabled_sort"));
        verify_database_schema(&conn).expect("repaired schema verifies");
        assert_eq!(
            column_default(&conn, "boards", "allow_editing").as_deref(),
            Some("1")
        );
        assert_eq!(
            column_default(&conn, "boards", "allow_self_delete").as_deref(),
            Some("1")
        );

        let board_flags: (i64, i64, i64, i64, String, String) = conn
            .query_row(
                "SELECT allow_editing, allow_self_delete, allow_video_embeds,
                        show_poster_ids, banner_mode, access_mode
                 FROM boards WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("read preserved board flags");
        assert_eq!(
            board_flags,
            (0, 1, 0, 1, "override".to_owned(), "public".to_owned())
        );
    }

    #[test]
    fn partial_schema_fails_closed_without_creating_missing_tables() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE boards (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                short_name TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL
            );
            CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (36);
            INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');",
        )
        .expect("create partial schema");

        let error = install_or_migrate_schema(&conn).expect_err("partial schema should fail");
        assert!(
            error
                .to_string()
                .contains("does not match RustChan 1.3.0 baseline"),
            "unexpected error: {error:#}"
        );

        assert!(!object_exists(&conn, "table", "posts"));
        assert_eq!(
            read_schema_version(&conn).expect("read version"),
            Some("36".to_owned())
        );
    }

    #[test]
    fn unknown_schema_object_fails_closed() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        install_or_migrate_schema(&conn).expect("install schema");
        conn.execute("CREATE TABLE operator_notes (body TEXT)", [])
            .expect("create unexpected table");

        let error = normalize_database_schema_version(&conn).expect_err("unknown table fails");
        assert!(
            error
                .to_string()
                .contains("unexpected table operator_notes"),
            "unexpected error: {error:#}"
        );
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
    fn invalid_structural_change_does_not_stamp_130() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        create_baseline_schema_objects(&conn).expect("create baseline objects");
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);
            DROP TRIGGER posts_board_match_insert;",
        )
        .expect("make schema invalid");

        let error = install_or_migrate_schema(&conn).expect_err("invalid schema should fail");
        assert!(
            error
                .to_string()
                .contains("missing trigger posts_board_match_insert"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            read_schema_version(&conn).expect("read version"),
            Some("41".to_owned())
        );
    }
}
