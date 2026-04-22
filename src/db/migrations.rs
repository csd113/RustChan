// src/db/migrations.rs

use anyhow::{Context, Result};

pub(super) const CURRENT_MAX_MIGRATION: i64 = 41;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, "ALTER TABLE boards ADD COLUMN allow_video    INTEGER NOT NULL DEFAULT 1"),
    (
        2,
        "ALTER TABLE boards ADD COLUMN allow_tripcodes INTEGER NOT NULL DEFAULT 1",
    ),
    (3, "ALTER TABLE boards ADD COLUMN allow_images  INTEGER NOT NULL DEFAULT 1"),
    (4, "ALTER TABLE boards ADD COLUMN allow_audio   INTEGER NOT NULL DEFAULT 0"),
    (5, "ALTER TABLE posts ADD COLUMN media_type TEXT"),
    (6, "ALTER TABLE posts ADD COLUMN audio_file_path TEXT"),
    (7, "ALTER TABLE posts ADD COLUMN audio_file_name TEXT"),
    (8, "ALTER TABLE posts ADD COLUMN audio_file_size INTEGER"),
    (9, "ALTER TABLE posts ADD COLUMN audio_mime_type TEXT"),
    (10, "ALTER TABLE posts ADD COLUMN edited_at INTEGER"),
    (
        11,
        "CREATE INDEX IF NOT EXISTS idx_jobs_pending ON background_jobs(status, priority DESC, created_at ASC)",
    ),
    (
        12,
        "CREATE INDEX IF NOT EXISTS idx_reports_status ON reports(status, created_at DESC)",
    ),
    (
        13,
        "CREATE INDEX IF NOT EXISTS idx_mod_log_created ON mod_log(created_at DESC)",
    ),
    (14, "ALTER TABLE threads ADD COLUMN archived INTEGER NOT NULL DEFAULT 0"),
    (
        15,
        "CREATE INDEX IF NOT EXISTS idx_threads_archived ON threads(board_id, archived, bumped_at DESC)",
    ),
    (
        16,
        "ALTER TABLE boards ADD COLUMN edit_window_secs INTEGER NOT NULL DEFAULT 0",
    ),
    (17, "ALTER TABLE boards ADD COLUMN allow_editing INTEGER NOT NULL DEFAULT 0"),
    (18, "ALTER TABLE boards ADD COLUMN allow_archive INTEGER NOT NULL DEFAULT 1"),
    (
        19,
        "ALTER TABLE boards ADD COLUMN allow_video_embeds INTEGER NOT NULL DEFAULT 0",
    ),
    (20, "ALTER TABLE boards ADD COLUMN allow_captcha INTEGER NOT NULL DEFAULT 0"),
    (
        21,
        r"CREATE TABLE IF NOT EXISTS ban_appeals (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ip_hash     TEXT NOT NULL,
            reason      TEXT NOT NULL DEFAULT '',
            status      TEXT NOT NULL DEFAULT 'open',
            created_at  INTEGER NOT NULL DEFAULT (unixepoch())
        )",
    ),
    (
        22,
        "ALTER TABLE boards ADD COLUMN post_cooldown_secs INTEGER NOT NULL DEFAULT 0",
    ),
    (23, "CREATE INDEX IF NOT EXISTS idx_posts_thread_id ON posts(thread_id)"),
    (24, "CREATE INDEX IF NOT EXISTS idx_posts_ip_hash ON posts(ip_hash)"),
    (
        25,
        r"CREATE TABLE IF NOT EXISTS chan_net_posts (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_post_id  INTEGER NOT NULL,
            board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            author          TEXT    NOT NULL DEFAULT 'anon',
            content         TEXT    NOT NULL DEFAULT '',
            remote_ts       INTEGER NOT NULL,
            imported_at     INTEGER NOT NULL DEFAULT (unixepoch())
        )",
    ),
    (
        26,
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_chan_net_posts_remote \
         ON chan_net_posts(remote_post_id, board_id)",
    ),
    (
        27,
        "ALTER TABLE boards ADD COLUMN allow_any_files INTEGER NOT NULL DEFAULT 0",
    ),
    (
        28,
        r"CREATE TABLE IF NOT EXISTS pending_fs_ops (
            id           TEXT PRIMARY KEY,
            kind         TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at   INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE INDEX IF NOT EXISTS idx_pending_fs_ops_created
            ON pending_fs_ops(created_at ASC)",
    ),
    (
        29,
        "ALTER TABLE boards ADD COLUMN show_poster_ids INTEGER NOT NULL DEFAULT 0",
    ),
    (
        30,
        r"CREATE TABLE IF NOT EXISTS user_thread_preferences (
            user_hash   TEXT NOT NULL,
            thread_id    INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            pinned      INTEGER NOT NULL DEFAULT 0,
            hidden      INTEGER NOT NULL DEFAULT 0,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(user_hash, thread_id)
        );
        CREATE INDEX IF NOT EXISTS idx_user_thread_preferences_user_hidden
            ON user_thread_preferences(user_hash, hidden);
        CREATE INDEX IF NOT EXISTS idx_user_thread_preferences_thread
            ON user_thread_preferences(thread_id)",
    ),
    (
        31,
        "ALTER TABLE boards ADD COLUMN max_archived_threads INTEGER NOT NULL DEFAULT 150",
    ),
    (
        32,
        r"ALTER TABLE boards ADD COLUMN display_order INTEGER NOT NULL DEFAULT 0;
        UPDATE boards
        SET display_order = id
        WHERE display_order = 0",
    ),
    (
        33,
        r"ALTER TABLE boards ADD COLUMN collapse_greentext INTEGER NOT NULL DEFAULT 0;
        UPDATE boards
        SET collapse_greentext = CASE
            WHEN EXISTS (
                SELECT 1
                FROM site_settings
                WHERE key = 'collapse_greentext'
                  AND (value = '1' OR lower(value) = 'true')
            ) THEN 1
            ELSE 0
        END",
    ),
    (
        34,
        r"ALTER TABLE boards ADD COLUMN default_theme TEXT NOT NULL DEFAULT '';
        CREATE TABLE IF NOT EXISTS themes (
            slug         TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            description  TEXT NOT NULL DEFAULT '',
            swatch_hex   TEXT NOT NULL DEFAULT '#888888',
            enabled      INTEGER NOT NULL DEFAULT 1,
            sort_order   INTEGER NOT NULL DEFAULT 0,
            is_builtin   INTEGER NOT NULL DEFAULT 0,
            custom_css   TEXT NOT NULL DEFAULT ''
        )",
    ),
    (
        35,
        "CREATE INDEX IF NOT EXISTS idx_themes_enabled_sort ON themes(enabled, sort_order, slug)",
    ),
    (
        36,
        r"CREATE TABLE IF NOT EXISTS post_submissions (
            submission_token TEXT PRIMARY KEY,
            ip_hash          TEXT NOT NULL,
            board_id         INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
            thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
            post_id          INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            is_thread        INTEGER NOT NULL DEFAULT 0,
            created_at       INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE INDEX IF NOT EXISTS idx_post_submissions_created_at
            ON post_submissions(created_at ASC)",
    ),
    (
        37,
        r"ALTER TABLE posts ADD COLUMN media_processing_state TEXT NOT NULL DEFAULT '';
        ALTER TABLE posts ADD COLUMN media_processing_error TEXT;
        CREATE INDEX IF NOT EXISTS idx_posts_media_processing_state
            ON posts(media_processing_state)",
    ),
    (
        38,
        "ALTER TABLE boards ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'public'",
    ),
    (
        39,
        "ALTER TABLE boards ADD COLUMN access_password_hash TEXT NOT NULL DEFAULT ''",
    ),
    (
        40,
        "ALTER TABLE boards ADD COLUMN banner_mode TEXT NOT NULL DEFAULT 'inherit'",
    ),
    (
        41,
        r"CREATE TABLE IF NOT EXISTS banner_assets (
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
            ON banner_assets(scope_type, board_id, sort_order, id)",
    ),
];

pub(super) fn apply_pending_migrations(conn: &rusqlite::Connection) -> Result<()> {
    ensure_schema_version_table_has_row(conn)?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .context("Failed to read schema_version")?;

    for &(version, sql) in MIGRATIONS {
        if version <= current_version {
            continue;
        }

        apply_one_migration(conn, version, sql)?;
    }

    Ok(())
}

pub(super) fn stamp_schema_version(conn: &rusqlite::Connection, version: i64) -> Result<()> {
    ensure_schema_version_table_has_row(conn)?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .with_context(|| format!("Failed to begin schema_version stamp to v{version}"))?;

    let result = (|| {
        conn.execute("DELETE FROM schema_version", [])
            .context("Failed to clear schema_version table")?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![version],
        )
        .with_context(|| format!("Failed to set schema_version to v{version}"))?;
        Ok(())
    })();

    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .with_context(|| format!("Failed to commit schema_version stamp to v{version}")),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ensure_schema_version_table_has_row(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(version)
         );
         INSERT INTO schema_version (version)
         SELECT 0
         WHERE NOT EXISTS (SELECT 1 FROM schema_version);",
    )
    .context("Failed to create schema_version table")
}

fn apply_one_migration(conn: &rusqlite::Connection, version: i64, sql: &str) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .with_context(|| format!("Failed to begin migration v{version}"))?;

    let result = run_migration_sql_verify_and_stamp(conn, version, sql);
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")
                .with_context(|| format!("Failed to commit migration v{version}"))?;
            tracing::debug!("Applied migration v{version}");
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn run_migration_sql_verify_and_stamp(
    conn: &rusqlite::Connection,
    version: i64,
    sql: &str,
) -> Result<()> {
    // Keep the historical SQL, object verification, and schema_version update
    // in the caller's transaction so a crash cannot record half a migration.
    match conn.execute_batch(sql) {
        Ok(()) => {}
        Err(error) if is_replay_tolerance_error(&error) => {
            complete_replayed_migration(conn, version).with_context(|| {
                format!(
                    "Migration v{version} replay failed verification after duplicate-object error"
                )
            })?;
            tracing::warn!(
                "Migration v{version} was already partially applied; completed and verified replay"
            );
        }
        Err(error) => Err(anyhow::anyhow!(
            "Migration v{version} failed: {error} — SQL: {sql}"
        ))?,
    }

    verify_migration(conn, version)
        .with_context(|| format!("Migration v{version} did not leave expected schema objects"))?;
    conn.execute(
        "UPDATE schema_version SET version = ?1",
        rusqlite::params![version],
    )
    .with_context(|| format!("Failed to update schema_version after migration v{version}"))?;
    Ok(())
}

fn is_replay_tolerance_error(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(inner, message) => {
            inner.code == rusqlite::ErrorCode::Unknown
                && message.as_deref().is_some_and(|message| {
                    message.contains("duplicate column name") || message.contains("already exists")
                })
        }
        _ => false,
    }
}

fn complete_replayed_migration(conn: &rusqlite::Connection, version: i64) -> Result<()> {
    match version {
        32 => {
            ensure_column(
                conn,
                "boards",
                "display_order",
                "ALTER TABLE boards ADD COLUMN display_order INTEGER NOT NULL DEFAULT 0",
            )?;
            conn.execute_batch(
                "UPDATE boards
                 SET display_order = id
                 WHERE display_order = 0",
            )
            .context("Complete migration v32 display_order backfill failed")?;
        }
        33 => {
            ensure_column(
                conn,
                "boards",
                "collapse_greentext",
                "ALTER TABLE boards ADD COLUMN collapse_greentext INTEGER NOT NULL DEFAULT 0",
            )?;
            conn.execute_batch(
                "UPDATE boards
                 SET collapse_greentext = CASE
                     WHEN EXISTS (
                         SELECT 1
                         FROM site_settings
                         WHERE key = 'collapse_greentext'
                           AND (value = '1' OR lower(value) = 'true')
                     ) THEN 1
                     ELSE 0
                 END",
            )
            .context("Complete migration v33 collapse_greentext backfill failed")?;
        }
        34 => {
            ensure_column(
                conn,
                "boards",
                "default_theme",
                "ALTER TABLE boards ADD COLUMN default_theme TEXT NOT NULL DEFAULT ''",
            )?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS themes (
                    slug         TEXT PRIMARY KEY,
                    display_name TEXT NOT NULL,
                    description  TEXT NOT NULL DEFAULT '',
                    swatch_hex   TEXT NOT NULL DEFAULT '#888888',
                    enabled      INTEGER NOT NULL DEFAULT 1,
                    sort_order   INTEGER NOT NULL DEFAULT 0,
                    is_builtin   INTEGER NOT NULL DEFAULT 0,
                    custom_css   TEXT NOT NULL DEFAULT ''
                )",
            )
            .context("Complete migration v34 themes table creation failed")?;
        }
        37 => {
            ensure_column(
                conn,
                "posts",
                "media_processing_state",
                "ALTER TABLE posts ADD COLUMN media_processing_state TEXT NOT NULL DEFAULT ''",
            )?;
            ensure_column(
                conn,
                "posts",
                "media_processing_error",
                "ALTER TABLE posts ADD COLUMN media_processing_error TEXT",
            )?;
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_posts_media_processing_state
                 ON posts(media_processing_state)",
            )
            .context("Complete migration v37 media-processing index creation failed")?;
        }
        _ => {}
    }
    verify_migration(conn, version)
}

fn verify_migration(conn: &rusqlite::Connection, version: i64) -> Result<()> {
    match version {
        1 => ensure_column_exists(conn, "boards", "allow_video"),
        2 => ensure_column_exists(conn, "boards", "allow_tripcodes"),
        3 => ensure_column_exists(conn, "boards", "allow_images"),
        4 => ensure_column_exists(conn, "boards", "allow_audio"),
        5 => ensure_column_exists(conn, "posts", "media_type"),
        6 => ensure_column_exists(conn, "posts", "audio_file_path"),
        7 => ensure_column_exists(conn, "posts", "audio_file_name"),
        8 => ensure_column_exists(conn, "posts", "audio_file_size"),
        9 => ensure_column_exists(conn, "posts", "audio_mime_type"),
        10 => ensure_column_exists(conn, "posts", "edited_at"),
        11 => ensure_index_exists(conn, "idx_jobs_pending"),
        12 => ensure_index_exists(conn, "idx_reports_status"),
        13 => ensure_index_exists(conn, "idx_mod_log_created"),
        14 => ensure_column_exists(conn, "threads", "archived"),
        15 => ensure_index_exists(conn, "idx_threads_archived"),
        16 => ensure_column_exists(conn, "boards", "edit_window_secs"),
        17 => ensure_column_exists(conn, "boards", "allow_editing"),
        18 => ensure_column_exists(conn, "boards", "allow_archive"),
        19 => ensure_column_exists(conn, "boards", "allow_video_embeds"),
        20 => ensure_column_exists(conn, "boards", "allow_captcha"),
        21 => ensure_table_exists(conn, "ban_appeals"),
        22 => ensure_column_exists(conn, "boards", "post_cooldown_secs"),
        23 => ensure_index_exists(conn, "idx_posts_thread_id"),
        24 => ensure_index_exists(conn, "idx_posts_ip_hash"),
        25 => ensure_table_exists(conn, "chan_net_posts"),
        26 => ensure_index_exists(conn, "idx_chan_net_posts_remote"),
        27 => ensure_column_exists(conn, "boards", "allow_any_files"),
        28 => {
            ensure_table_exists(conn, "pending_fs_ops")?;
            ensure_index_exists(conn, "idx_pending_fs_ops_created")
        }
        29 => ensure_column_exists(conn, "boards", "show_poster_ids"),
        30 => {
            ensure_table_exists(conn, "user_thread_preferences")?;
            ensure_index_exists(conn, "idx_user_thread_preferences_user_hidden")?;
            ensure_index_exists(conn, "idx_user_thread_preferences_thread")
        }
        31 => ensure_column_exists(conn, "boards", "max_archived_threads"),
        32 => ensure_column_exists(conn, "boards", "display_order"),
        33 => ensure_column_exists(conn, "boards", "collapse_greentext"),
        34 => {
            ensure_column_exists(conn, "boards", "default_theme")?;
            ensure_table_exists(conn, "themes")
        }
        35 => ensure_index_exists(conn, "idx_themes_enabled_sort"),
        36 => {
            ensure_table_exists(conn, "post_submissions")?;
            ensure_index_exists(conn, "idx_post_submissions_created_at")
        }
        37 => {
            ensure_column_exists(conn, "posts", "media_processing_state")?;
            ensure_column_exists(conn, "posts", "media_processing_error")?;
            ensure_index_exists(conn, "idx_posts_media_processing_state")
        }
        38 => ensure_column_exists(conn, "boards", "access_mode"),
        39 => ensure_column_exists(conn, "boards", "access_password_hash"),
        40 => ensure_column_exists(conn, "boards", "banner_mode"),
        41 => {
            ensure_table_exists(conn, "banner_assets")?;
            ensure_index_exists(conn, "idx_banner_assets_scope_sort")
        }
        _ => anyhow::bail!("No verification defined for migration v{version}"),
    }
}

fn ensure_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    add_column_sql: &str,
) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(add_column_sql)
            .with_context(|| format!("Add missing migration column {table}.{column} failed"))?;
    }
    Ok(())
}

fn ensure_column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<()> {
    if column_exists(conn, table, column)? {
        Ok(())
    } else {
        anyhow::bail!("Expected migration column {table}.{column} does not exist")
    }
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
    .with_context(|| format!("Failed to inspect migration column {table}.{column}"))
}

fn ensure_table_exists(conn: &rusqlite::Connection, table: &str) -> Result<()> {
    if object_exists(conn, "table", table)? {
        Ok(())
    } else {
        anyhow::bail!("Expected migration table {table} does not exist")
    }
}

fn ensure_index_exists(conn: &rusqlite::Connection, index: &str) -> Result<()> {
    if object_exists(conn, "index", index)? {
        Ok(())
    } else {
        anyhow::bail!("Expected migration index {index} does not exist")
    }
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
    .with_context(|| format!("Failed to inspect migration object {kind}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::{apply_one_migration, ensure_schema_version_table_has_row, MIGRATIONS};

    fn version(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .expect("read schema version")
    }

    fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
        super::column_exists(conn, table, column).expect("inspect column")
    }

    #[test]
    fn failed_migration_rolls_back_schema_changes_and_version_update() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch("CREATE TABLE boards (id INTEGER PRIMARY KEY);")
            .expect("create boards");
        ensure_schema_version_table_has_row(&conn).expect("create schema_version");

        let error = apply_one_migration(
            &conn,
            1,
            "ALTER TABLE boards ADD COLUMN allow_video INTEGER NOT NULL DEFAULT 1;
             SELECT * FROM missing_table;",
        )
        .expect_err("migration should fail");

        assert!(
            error.to_string().contains("Migration v1 failed"),
            "unexpected error: {error:#}"
        );
        assert_eq!(version(&conn), 0);
        assert!(!column_exists(&conn, "boards", "allow_video"));
    }

    #[test]
    fn replayed_multi_step_column_migration_completes_missing_backfill() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE boards (
                id INTEGER PRIMARY KEY,
                display_order INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO boards (id, display_order) VALUES (7, 0);",
        )
        .expect("create partially migrated boards");
        ensure_schema_version_table_has_row(&conn).expect("create schema_version");
        conn.execute("UPDATE schema_version SET version = 31", [])
            .expect("set schema version");

        let (_, sql) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == 32)
            .expect("migration v32 exists");
        apply_one_migration(&conn, 32, sql).expect("replay migration v32");

        let display_order: i64 = conn
            .query_row("SELECT display_order FROM boards WHERE id = 7", [], |row| {
                row.get(0)
            })
            .expect("read display_order");
        assert_eq!(display_order, 7);
        assert_eq!(version(&conn), 32);
    }

    #[test]
    fn replayed_multi_step_post_migration_completes_missing_column_and_index() {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE posts (
                id INTEGER PRIMARY KEY,
                media_processing_state TEXT NOT NULL DEFAULT ''
            );",
        )
        .expect("create partially migrated posts");
        ensure_schema_version_table_has_row(&conn).expect("create schema_version");
        conn.execute("UPDATE schema_version SET version = 36", [])
            .expect("set schema version");

        let (_, sql) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == 37)
            .expect("migration v37 exists");
        apply_one_migration(&conn, 37, sql).expect("replay migration v37");

        assert!(column_exists(&conn, "posts", "media_processing_error"));
        assert!(
            super::object_exists(&conn, "index", "idx_posts_media_processing_state")
                .expect("inspect index")
        );
        assert_eq!(version(&conn), 37);
    }
}
