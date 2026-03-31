// src/db/migrations.rs

use anyhow::{Context, Result};

pub(super) const CURRENT_MAX_MIGRATION: i64 = 27;

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
];

pub(super) fn apply_migrations(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(version)
         );
         INSERT INTO schema_version (version)
         SELECT {CURRENT_MAX_MIGRATION}
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

    for &(version, sql) in MIGRATIONS {
        if version <= current_version {
            continue;
        }

        match conn.execute_batch(sql) {
            Ok(()) => {
                tracing::debug!("Applied migration v{version}");
            }
            Err(rusqlite::Error::SqliteFailure(ref error, ref msg))
                if error.code == rusqlite::ErrorCode::Unknown
                    && msg.as_deref().is_some_and(|message| {
                        message.contains("duplicate column name")
                            || message.contains("already exists")
                    }) =>
            {
                tracing::warn!("Migration v{version} already applied (idempotent), skipping");
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "Migration v{version} failed: {error} — SQL: {sql}"
                ));
            }
        }

        conn.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![version],
        )
        .with_context(|| format!("Failed to update schema_version after migration v{version}"))?;
    }

    Ok(())
}
