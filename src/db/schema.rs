use anyhow::{bail, Context as _, Result};
use rusqlite::OptionalExtension as _;
use std::collections::{BTreeMap, BTreeSet};

use super::migrations::{
    read_schema_version, stamp_schema_version, stamp_schema_version_in_transaction,
    BASELINE_SCHEMA_VERSION,
};

/// Complete baseline table definitions for a fresh database.
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

/// Complete baseline secondary-index definitions.
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
    CREATE INDEX IF NOT EXISTS idx_posts_file_path
        ON posts(file_path) WHERE file_path IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_posts_thumb_path
        ON posts(thumb_path) WHERE thumb_path IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_posts_audio_file_path
        ON posts(audio_file_path) WHERE audio_file_path IS NOT NULL;
    CREATE INDEX IF NOT EXISTS idx_file_hashes_file_path
        ON file_hashes(file_path);
    CREATE INDEX IF NOT EXISTS idx_file_hashes_thumb_path
        ON file_hashes(thumb_path);
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
    CREATE INDEX IF NOT EXISTS idx_poll_options_poll_position
        ON poll_options(poll_id, position, id);
    CREATE INDEX IF NOT EXISTS idx_poll_votes_option_poll
        ON poll_votes(option_id, poll_id);
    CREATE INDEX IF NOT EXISTS idx_ban_appeals_status_created
        ON ban_appeals(status, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_ban_appeals_ip_created
        ON ban_appeals(ip_hash, created_at DESC);
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

/// Obsolete theme index accepted only during the known legacy repair path.
const LEGACY_THEME_SORT_INDEX: &str = "idx_themes_enabled_sort";
/// Additive indexes introduced after the first package-version baseline.
const ADDITIVE_BASELINE_INDEXES: [&str; 9] = [
    "idx_posts_file_path",
    "idx_posts_thumb_path",
    "idx_posts_audio_file_path",
    "idx_file_hashes_file_path",
    "idx_file_hashes_thumb_path",
    "idx_poll_options_poll_position",
    "idx_poll_votes_option_poll",
    "idx_ban_appeals_status_created",
    "idx_ban_appeals_ip_created",
];
/// Redundant indexes removed when the additive index set is installed.
const REDUNDANT_LEGACY_INDEXES: [&str; 2] = ["idx_file_hashes", "idx_posts_thread_id"];
/// Board columns whose historical default was zero instead of the baseline value.
const LEGACY_BOARD_ZERO_DEFAULT_COLUMNS: [&str; 4] = [
    "allow_editing",
    "allow_self_delete",
    "allow_video_embeds",
    "show_poster_ids",
];

#[derive(Debug, Clone, Copy)]
/// One closed persisted domain enforced for new writes and audited for legacy rows.
struct DomainInvariant {
    /// Table containing the constrained values.
    table: &'static str,
    /// Columns whose updates must re-evaluate the invariant.
    update_columns: &'static str,
    /// Predicate identifying an invalid row; `ROW.` is replaced by the relevant alias.
    invalid_predicate: &'static str,
    /// Stable constraint error returned by `SQLite`.
    error: &'static str,
}

/// Closed domains whose historical writers emitted only the listed values.
const DOMAIN_INVARIANTS: &[DomainInvariant] = &[
    DomainInvariant {
        table: "boards",
        update_columns: "display_order, short_name, nsfw, max_threads, max_archived_threads, bump_limit, allow_video, allow_tripcodes, allow_images, allow_audio, max_image_size, max_video_size, max_audio_size, max_pdf_size, allow_pdf, allow_any_files, edit_window_secs, allow_editing, allow_self_delete, allow_archive, allow_video_embeds, allow_captcha, show_poster_ids, collapse_greentext, post_cooldown_secs, access_mode, access_password_hash",
        invalid_predicate: r"
            typeof(ROW.display_order) <> 'integer' OR ROW.display_order < 0
            OR length(ROW.short_name) NOT BETWEEN 1 AND 8
            OR ROW.short_name GLOB '*[^A-Za-z0-9]*'
            OR typeof(ROW.nsfw) <> 'integer' OR ROW.nsfw NOT IN (0, 1)
            OR typeof(ROW.max_threads) <> 'integer' OR ROW.max_threads < 1
            OR typeof(ROW.max_archived_threads) <> 'integer' OR ROW.max_archived_threads < 1
            OR typeof(ROW.bump_limit) <> 'integer' OR ROW.bump_limit < 1
            OR typeof(ROW.allow_video) <> 'integer' OR ROW.allow_video NOT IN (0, 1)
            OR typeof(ROW.allow_tripcodes) <> 'integer' OR ROW.allow_tripcodes NOT IN (0, 1)
            OR typeof(ROW.allow_images) <> 'integer' OR ROW.allow_images NOT IN (0, 1)
            OR typeof(ROW.allow_audio) <> 'integer' OR ROW.allow_audio NOT IN (0, 1)
            OR typeof(ROW.max_image_size) <> 'integer' OR ROW.max_image_size < 0
            OR typeof(ROW.max_video_size) <> 'integer' OR ROW.max_video_size < 0
            OR typeof(ROW.max_audio_size) <> 'integer' OR ROW.max_audio_size < 0
            OR typeof(ROW.max_pdf_size) <> 'integer' OR ROW.max_pdf_size < 0
            OR typeof(ROW.allow_pdf) <> 'integer' OR ROW.allow_pdf NOT IN (0, 1)
            OR typeof(ROW.allow_any_files) <> 'integer' OR ROW.allow_any_files NOT IN (0, 1)
            OR typeof(ROW.edit_window_secs) <> 'integer' OR ROW.edit_window_secs < 0
            OR typeof(ROW.allow_editing) <> 'integer' OR ROW.allow_editing NOT IN (0, 1)
            OR typeof(ROW.allow_self_delete) <> 'integer' OR ROW.allow_self_delete NOT IN (0, 1)
            OR typeof(ROW.allow_archive) <> 'integer' OR ROW.allow_archive NOT IN (0, 1)
            OR typeof(ROW.allow_video_embeds) <> 'integer' OR ROW.allow_video_embeds NOT IN (0, 1)
            OR typeof(ROW.allow_captcha) <> 'integer' OR ROW.allow_captcha NOT IN (0, 1)
            OR typeof(ROW.show_poster_ids) <> 'integer' OR ROW.show_poster_ids NOT IN (0, 1)
            OR typeof(ROW.collapse_greentext) <> 'integer' OR ROW.collapse_greentext NOT IN (0, 1)
            OR typeof(ROW.post_cooldown_secs) <> 'integer' OR ROW.post_cooldown_secs < 0
            OR (ROW.access_mode IN ('view_password', 'post_password')
                AND ROW.access_password_hash = '')
        ",
        error: "boards row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "threads",
        update_columns: "board_id, locked, sticky, archived, reply_count",
        invalid_predicate: r"
            typeof(ROW.locked) <> 'integer' OR ROW.locked NOT IN (0, 1)
            OR typeof(ROW.sticky) <> 'integer' OR ROW.sticky NOT IN (0, 1)
            OR typeof(ROW.archived) <> 'integer' OR ROW.archived NOT IN (0, 1)
            OR typeof(ROW.reply_count) <> 'integer' OR ROW.reply_count < 0
            OR EXISTS (
                SELECT 1 FROM posts AS child
                WHERE child.thread_id = ROW.id AND child.board_id <> ROW.board_id
            )
        ",
        error: "threads row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "posts",
        update_columns: "thread_id, board_id, file_size, is_op, media_type, audio_file_size, media_processing_state",
        invalid_predicate: r"
            typeof(ROW.is_op) <> 'integer' OR ROW.is_op NOT IN (0, 1)
            OR (ROW.file_size IS NOT NULL
                AND (typeof(ROW.file_size) <> 'integer' OR ROW.file_size < 0))
            OR (ROW.audio_file_size IS NOT NULL
                AND (typeof(ROW.audio_file_size) <> 'integer' OR ROW.audio_file_size < 0))
            OR (ROW.media_type IS NOT NULL
                AND ROW.media_type NOT IN ('image', 'video', 'audio', 'pdf', 'other'))
            OR ROW.media_processing_state NOT IN
                ('', 'pending', 'running', 'failed', 'prune_pending', 'pruned')
            OR NOT EXISTS (
                SELECT 1 FROM threads AS parent
                WHERE parent.id = ROW.thread_id AND parent.board_id = ROW.board_id
            )
            OR EXISTS (
                SELECT 1 FROM reports AS report
                WHERE report.post_id = ROW.id
                  AND (report.thread_id <> ROW.thread_id OR report.board_id <> ROW.board_id)
            )
            OR EXISTS (
                SELECT 1 FROM post_submissions AS submission
                WHERE submission.post_id = ROW.id
                  AND (submission.thread_id <> ROW.thread_id
                       OR submission.board_id <> ROW.board_id
                       OR submission.is_thread <> ROW.is_op)
            )
        ",
        error: "posts row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "pending_fs_ops",
        update_columns: "kind",
        invalid_predicate: r"
            ROW.kind NOT IN (
                'upload_finalize', 'delete_files', 'delete_banner_assets',
                'full_restore_swap', 'board_restore_swap', 'original_prune'
            )
        ",
        error: "pending_fs_ops.kind is not supported",
    },
    DomainInvariant {
        table: "ban_appeals",
        update_columns: "status",
        invalid_predicate: "ROW.status NOT IN ('open', 'dismissed', 'accepted')",
        error: "ban_appeals.status is not supported",
    },
    DomainInvariant {
        table: "poll_options",
        update_columns: "poll_id, position",
        invalid_predicate: r"
            typeof(ROW.position) <> 'integer' OR ROW.position < 0
            OR EXISTS (
                SELECT 1 FROM poll_votes AS vote
                WHERE vote.option_id = ROW.id AND vote.poll_id <> ROW.poll_id
            )
        ",
        error: "poll_options row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "poll_votes",
        update_columns: "poll_id, option_id",
        invalid_predicate: r"
            NOT EXISTS (
                SELECT 1 FROM poll_options AS option
                WHERE option.id = ROW.option_id AND option.poll_id = ROW.poll_id
            )
        ",
        error: "poll_votes.option_id must belong to poll_id",
    },
    DomainInvariant {
        table: "banner_assets",
        update_columns: "scope_type, board_id, storage_key, width, height, file_size, enabled, sort_order, target_type, target_value, show_on_index, show_on_catalog",
        invalid_predicate: r"
            ROW.scope_type NOT IN ('global', 'board', 'home')
            OR (ROW.scope_type = 'board') <> (ROW.board_id IS NOT NULL)
            OR length(ROW.storage_key) <> 32
            OR ROW.storage_key <> lower(ROW.storage_key)
            OR ROW.storage_key GLOB '*[^0-9a-f]*'
            OR typeof(ROW.width) <> 'integer' OR ROW.width <= 0
            OR typeof(ROW.height) <> 'integer' OR ROW.height <= 0
            OR typeof(ROW.file_size) <> 'integer' OR ROW.file_size <= 0
            OR typeof(ROW.enabled) <> 'integer' OR ROW.enabled NOT IN (0, 1)
            OR typeof(ROW.sort_order) <> 'integer' OR ROW.sort_order < 0
            OR ROW.target_type NOT IN
                ('none', 'internal_board', 'internal_path', 'external_url')
            OR (ROW.target_type = 'none' AND ROW.target_value <> '')
            OR (ROW.target_type <> 'none' AND ROW.target_value = '')
            OR typeof(ROW.show_on_index) <> 'integer' OR ROW.show_on_index NOT IN (0, 1)
            OR typeof(ROW.show_on_catalog) <> 'integer' OR ROW.show_on_catalog NOT IN (0, 1)
        ",
        error: "banner_assets row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "themes",
        update_columns: "slug, enabled, is_builtin",
        invalid_predicate: r"
            length(ROW.slug) NOT BETWEEN 1 AND 32
            OR ROW.slug <> lower(ROW.slug)
            OR ROW.slug GLOB '*[^a-z0-9_-]*'
            OR typeof(ROW.enabled) <> 'integer' OR ROW.enabled NOT IN (0, 1)
            OR typeof(ROW.is_builtin) <> 'integer' OR ROW.is_builtin NOT IN (0, 1)
        ",
        error: "themes row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "reports",
        update_columns: "post_id, thread_id, board_id, status, resolved_at, resolved_by",
        invalid_predicate: r"
            ROW.status NOT IN ('open', 'resolved')
            OR (ROW.status = 'open'
                AND (ROW.resolved_at IS NOT NULL OR ROW.resolved_by IS NOT NULL))
            OR (ROW.status = 'resolved' AND ROW.resolved_at IS NULL)
            OR NOT EXISTS (
                SELECT 1 FROM posts AS reported
                WHERE reported.id = ROW.post_id
                  AND reported.thread_id = ROW.thread_id
                  AND reported.board_id = ROW.board_id
            )
        ",
        error: "reports row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "background_jobs",
        update_columns: "job_type, status, attempts",
        invalid_predicate: r"
            ROW.job_type NOT IN
                ('video_transcode', 'audio_waveform', 'thread_prune', 'spam_check')
            OR ROW.status NOT IN ('pending', 'running', 'done', 'failed')
            OR typeof(ROW.attempts) <> 'integer' OR ROW.attempts < 0
        ",
        error: "background_jobs row violates a persisted domain invariant",
    },
    DomainInvariant {
        table: "user_thread_preferences",
        update_columns: "pinned, hidden",
        invalid_predicate: r"
            typeof(ROW.pinned) <> 'integer' OR ROW.pinned NOT IN (0, 1)
            OR typeof(ROW.hidden) <> 'integer' OR ROW.hidden NOT IN (0, 1)
        ",
        error: "user_thread_preferences flags must be zero or one",
    },
    DomainInvariant {
        table: "post_submissions",
        update_columns: "board_id, thread_id, post_id, is_thread",
        invalid_predicate: r"
            typeof(ROW.is_thread) <> 'integer' OR ROW.is_thread NOT IN (0, 1)
            OR NOT EXISTS (
                SELECT 1 FROM posts AS submitted
                WHERE submitted.id = ROW.post_id
                  AND submitted.thread_id = ROW.thread_id
                  AND submitted.board_id = ROW.board_id
                  AND submitted.is_op = ROW.is_thread
            )
        ",
        error: "post_submissions row does not match its post",
    },
];

/// Install the baseline into a fresh database or normalize an existing database.
pub(super) fn install_or_migrate_schema(conn: &rusqlite::Connection) -> Result<()> {
    if is_fresh_database(conn)? {
        conn.execute_batch("BEGIN IMMEDIATE")
            .context("Begin fresh schema installation transaction failed")?;
        let result = (|| {
            if !is_fresh_database(conn)? {
                return Ok(false);
            }
            install_baseline_schema_in_transaction(conn)?;
            Ok(true)
        })();

        match result {
            Ok(installed) => {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    drop(conn.execute_batch("ROLLBACK"));
                    return Err(error).context("Commit fresh schema installation failed");
                }
                if installed {
                    return verify_database_schema(conn);
                }
            }
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error);
            }
        }
    }

    normalize_database_schema_version(conn)
}

/// Return the release's canonical database schema version.
pub(super) const fn baseline_schema_version() -> &'static str {
    BASELINE_SCHEMA_VERSION
}

/// Verify both the database structure and recorded schema version.
pub(super) fn verify_database_schema(conn: &rusqlite::Connection) -> Result<()> {
    verify_database_schema_structure(conn)?;
    verify_thread_reply_counts(conn)?;
    let metadata_issues = schema_version_metadata_issues(conn)?;
    if metadata_issues.is_empty() {
        Ok(())
    } else {
        bail!(
            "database schema-version metadata is invalid: {}",
            metadata_issues.join("; ")
        )
    }
}

/// Repair recognized legacy drift and stamp a structurally valid baseline.
pub(super) fn normalize_database_schema_version(conn: &rusqlite::Connection) -> Result<()> {
    let version_object_type = schema_version_object_type(conn)?;
    if let Some(object_type) = version_object_type.as_deref() {
        if object_type != "table" {
            bail!("schema_version is a {object_type}, expected a table");
        }
    }
    repair_known_legacy_baseline_drift(conn)?;
    verify_database_schema_structure(conn)?;
    reconcile_thread_reply_counts(conn)?;
    let metadata_issues = schema_version_metadata_issues(conn)?;
    if !metadata_issues.is_empty() {
        stamp_schema_version(conn)?;
    }
    verify_database_schema(conn)
}

/// Return an operator-facing schema verification label.
pub(super) fn database_schema_status_label(conn: &rusqlite::Connection) -> String {
    match verify_database_schema(conn) {
        Ok(()) => format!("{BASELINE_SCHEMA_VERSION} baseline verified"),
        Err(error) => format!("{BASELINE_SCHEMA_VERSION} baseline mismatch: {error}"),
    }
}

/// Install and stamp the baseline while the caller holds an immediate transaction.
fn install_baseline_schema_in_transaction(conn: &rusqlite::Connection) -> Result<()> {
    create_baseline_schema_objects(conn)?;
    stamp_schema_version_in_transaction(conn)?;
    verify_database_schema(conn)
}

/// Create every table, index, search object, and invariant in the baseline.
fn create_baseline_schema_objects(conn: &rusqlite::Connection) -> Result<()> {
    create_base_tables(conn)?;
    create_indexes(conn)?;
    ensure_posts_search_index(conn)?;
    ensure_post_invariants(conn)?;
    ensure_board_access_invariants(conn)?;
    ensure_domain_invariants(conn)
}

/// Create all baseline tables.
fn create_base_tables(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(BASE_SCHEMA_SQL)
        .context("Schema table creation failed")
}

/// Return whether the database contains no application schema objects.
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

/// Create all baseline secondary indexes.
fn create_indexes(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(INDEX_SCHEMA_SQL)
        .context("Schema index creation failed")
}

/// Create and synchronize the posts full-text-search index.
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

/// Install database constraints coupling posts to their parent threads.
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

/// Constrain board access modes and password requirements.
fn ensure_board_access_invariants(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        r"
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

/// Install the write-time trigger pair for every closed persisted domain.
fn ensure_domain_invariants(conn: &rusqlite::Connection) -> Result<()> {
    for invariant in DOMAIN_INVARIANTS {
        ensure_domain_invariant(conn, invariant)?;
    }
    Ok(())
}

/// Reinstall the pending-operation domain after restore hardening rebuilds its table.
pub(super) fn ensure_pending_fs_op_invariants(conn: &rusqlite::Connection) -> Result<()> {
    let invariant = DOMAIN_INVARIANTS
        .iter()
        .find(|invariant| invariant.table == "pending_fs_ops")
        .context("pending_fs_ops domain invariant is not defined")?;
    ensure_domain_invariant(conn, invariant)
}

/// Install one insert/update trigger pair from its shared validation predicate.
fn ensure_domain_invariant(conn: &rusqlite::Connection, invariant: &DomainInvariant) -> Result<()> {
    let insert_trigger = domain_trigger_name(invariant.table, "insert");
    let update_trigger = domain_trigger_name(invariant.table, "update");
    let predicate = invariant.invalid_predicate.replace("ROW.", "NEW.");
    let sql = format!(
        "CREATE TRIGGER IF NOT EXISTS {insert_trigger}
         BEFORE INSERT ON {table}
         FOR EACH ROW
         WHEN {predicate}
         BEGIN
             SELECT RAISE(ABORT, '{error}');
         END;

         CREATE TRIGGER IF NOT EXISTS {update_trigger}
         BEFORE UPDATE OF {update_columns} ON {table}
         FOR EACH ROW
         WHEN {predicate}
         BEGIN
             SELECT RAISE(ABORT, '{error}');
         END;",
        table = invariant.table,
        update_columns = invariant.update_columns,
        error = invariant.error,
    );
    conn.execute_batch(&sql)
        .with_context(|| format!("Install {} domain invariants failed", invariant.table))
}

/// Return the canonical name for one generated domain trigger.
fn domain_trigger_name(table: &str, operation: &str) -> String {
    format!("{table}_domain_{operation}")
}

/// Return whether a trigger belongs to the additive domain-hardening migration.
fn is_additive_domain_trigger(name: &str) -> bool {
    DOMAIN_INVARIANTS.iter().any(|invariant| {
        ["insert", "update"]
            .iter()
            .any(|operation| name == domain_trigger_name(invariant.table, operation))
    })
}

/// Apply narrowly recognized repairs for legacy baseline drift.
fn repair_known_legacy_baseline_drift(conn: &rusqlite::Connection) -> Result<()> {
    if !is_known_legacy_schema_version(read_schema_version(conn)?.as_deref()) {
        return Ok(());
    }

    let expected = expected_schema_shape()?;
    let actual = schema_shape(conn)?;
    if actual == expected {
        return Ok(());
    }
    if !can_repair_known_legacy_baseline_drift(&expected, &actual) {
        return Ok(());
    }

    if boards_table_requires_baseline_rebuild(&expected, &actual) {
        rebuild_boards_table_for_baseline(conn)
    } else {
        apply_additive_schema_repairs(conn)
    }
}

/// Return whether a recorded version belongs to a recognized repairable baseline.
fn is_known_legacy_schema_version(version: Option<&str>) -> bool {
    if matches!(version, None | Some("1.3.0" | "1.4.0")) || version == Some(BASELINE_SCHEMA_VERSION)
    {
        return true;
    }
    match version.and_then(|value| value.parse::<i64>().ok()) {
        Some(1..=41) => true,
        Some(_) | None => false,
    }
}

/// Return whether all observed drift matches recognized legacy shapes.
fn can_repair_known_legacy_baseline_drift(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    schema_objects_are_legacy_repairable(expected, actual)
        && tables_are_legacy_repairable(expected, actual)
}

/// Compare schema objects while accepting only recognized additive-object drift.
fn schema_objects_are_legacy_repairable(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    for (name, expected_object) in &expected.objects {
        let Some(actual_object) = actual.objects.get(name) else {
            if ADDITIVE_BASELINE_INDEXES.contains(&name.as_str())
                || (expected_object.kind == "trigger" && is_additive_domain_trigger(name))
            {
                continue;
            }
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
        if REDUNDANT_LEGACY_INDEXES.contains(&name.as_str())
            && actual_object.kind == "index"
            && actual_object
                .sql
                .as_deref()
                .is_some_and(|sql| is_redundant_legacy_index(name, sql))
        {
            continue;
        }
        return false;
    }

    true
}

/// Return whether SQL exactly describes a removed redundant index.
fn is_redundant_legacy_index(name: &str, sql: &str) -> bool {
    let expected = match name {
        "idx_file_hashes" => "CREATE INDEX idx_file_hashes ON file_hashes(sha256)",
        "idx_posts_thread_id" => "CREATE INDEX idx_posts_thread_id ON posts(thread_id)",
        _ => return false,
    };
    sql == normalize_schema_sql(expected)
}

/// Return whether SQL exactly describes the obsolete theme sort index.
fn is_legacy_theme_sort_index(sql: &str) -> bool {
    sql == normalize_schema_sql(
        "CREATE INDEX idx_themes_enabled_sort ON themes(enabled, sort_order, slug)",
    )
}

/// Compare table shapes under the known legacy exceptions.
fn tables_are_legacy_repairable(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    for (table, expected_table) in &expected.tables {
        let Some(actual_table) = actual.tables.get(table) else {
            return false;
        };
        let repairable = match table.as_str() {
            "boards" => boards_table_is_legacy_repairable(expected_table, actual_table),
            "themes" => themes_table_is_legacy_repairable(expected_table, actual_table),
            _ => table_is_additive_index_repairable(expected_table, actual_table),
        };
        if !repairable {
            return false;
        }
    }

    true
}

/// Compare a table while accepting missing additive and present redundant indexes.
fn table_is_additive_index_repairable(expected: &TableShape, actual: &TableShape) -> bool {
    if !table_definition_semantics_match(expected, actual) {
        return false;
    }

    for (name, expected_index) in &expected.indexes {
        match actual.indexes.get(name) {
            Some(actual_index) if actual_index == expected_index => {}
            None if ADDITIVE_BASELINE_INDEXES.contains(&name.as_str()) => {}
            Some(_) | None => return false,
        }
    }

    actual.indexes.keys().all(|name| {
        expected.indexes.contains_key(name) || REDUNDANT_LEGACY_INDEXES.contains(&name.as_str())
    })
}

/// Return whether the themes table differs only by its obsolete sort index.
fn themes_table_is_legacy_repairable(expected: &TableShape, actual: &TableShape) -> bool {
    if !table_definition_semantics_match(expected, actual) {
        return false;
    }

    let mut actual_indexes = actual.indexes.clone();
    actual_indexes.remove(LEGACY_THEME_SORT_INDEX);
    actual_indexes == expected.indexes
}

/// Compare all table-definition semantics except separately repairable indexes.
fn table_definition_semantics_match(expected: &TableShape, actual: &TableShape) -> bool {
    actual.columns == expected.columns
        && actual.foreign_keys == expected.foreign_keys
        && actual.check_constraints == expected.check_constraints
        && actual.autoincrement == expected.autoincrement
        && actual.without_rowid == expected.without_rowid
        && actual.strict == expected.strict
}

/// Atomically install every additive object and verify the repaired baseline.
fn apply_additive_schema_repairs(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin additive schema migration failed")?;
    let result = (|| {
        apply_additive_schema_repairs_in_transaction(conn)?;
        verify_database_schema_structure(conn)
    })();

    match result {
        Ok(()) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(()),
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                Err(error).context("Commit additive schema migration failed")
            }
        },
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Install additive indexes and domain triggers while the caller owns a transaction.
fn apply_additive_schema_repairs_in_transaction(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_themes_enabled_sort;
         DROP INDEX IF EXISTS idx_file_hashes;
         DROP INDEX IF EXISTS idx_posts_thread_id;",
    )
    .context("Remove obsolete indexes failed")?;
    create_indexes(conn).context("Install additive indexes failed")?;
    ensure_domain_invariants(conn).context("Install additive domain invariants")
}

/// Return whether a legacy boards table can be safely rebuilt.
fn boards_table_is_legacy_repairable(expected: &TableShape, actual: &TableShape) -> bool {
    actual.foreign_keys == expected.foreign_keys
        && actual.autoincrement == expected.autoincrement
        && actual.without_rowid == expected.without_rowid
        && actual.strict == expected.strict
        && (actual.check_constraints.is_empty()
            || actual.check_constraints == expected.check_constraints)
        && legacy_board_columns_match(expected, actual)
        && legacy_board_indexes_are_repairable(actual)
}

/// Compare every board column while accepting known historical defaults.
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

/// Compare one board column under the historical-default exception.
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

/// Return whether the legacy boards table has only its expected unique index.
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

/// Return whether the boards table must be rebuilt to match the baseline.
fn boards_table_requires_baseline_rebuild(expected: &SchemaShape, actual: &SchemaShape) -> bool {
    actual.tables.get("boards") != expected.tables.get("boards")
        || required_fragments_missing_for_table(actual, "boards")
}

/// Return whether a table definition lacks a required normalized SQL fragment.
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

/// Rebuild the boards table into the release baseline shape.
fn rebuild_boards_table_for_baseline(conn: &rusqlite::Connection) -> Result<()> {
    let failure_context = format!(
        "Structural migration: rebuild boards table for {} baseline failed",
        baseline_schema_version()
    );
    let success_log = format!(
        "Applied structural migration: boards table matches {} baseline",
        baseline_schema_version()
    );
    let prior_sequence = autoincrement_sequence(conn, "boards")?;
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
            banner_mode, access_mode,
            access_password_hash, created_at
        FROM boards;

        DROP TABLE boards;
        ALTER TABLE boards_new RENAME TO boards;
        ",
        &failure_context,
        &success_log,
        |conn| {
            restore_autoincrement_sequence(conn, "boards", prior_sequence)?;
            ensure_board_access_invariants(conn)?;
            apply_additive_schema_repairs_in_transaction(conn)?;
            verify_database_schema_structure(conn)
        },
    )
}

/// Run one structural repair with foreign keys temporarily disabled.
fn run_structural_schema_repair<F>(
    conn: &rusqlite::Connection,
    sql: &str,
    failure_context: &str,
    success_log: &str,
    finish_in_transaction: F,
) -> Result<()>
where
    F: FnOnce(&rusqlite::Connection) -> Result<()>,
{
    let foreign_keys_enabled = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .context("Read foreign key setting before schema repair failed")?
        != 0;
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .with_context(|| format!("Disable foreign keys for {failure_context}"))?;

    let repair_result = (|| {
        conn.execute_batch("BEGIN IMMEDIATE")
            .with_context(|| format!("Begin transaction for {failure_context}"))?;
        let result = conn
            .execute_batch(sql)
            .context(failure_context.to_owned())
            .and_then(|()| finish_in_transaction(conn));
        match result {
            Ok(()) => {
                if let Err(error) = conn.execute_batch("COMMIT") {
                    drop(conn.execute_batch("ROLLBACK"));
                    return Err(error).with_context(|| format!("Commit {failure_context}"));
                }
                Ok(())
            }
            Err(error) => {
                drop(conn.execute_batch("ROLLBACK"));
                Err(error)
            }
        }
    })();

    let restore_result = restore_foreign_keys(conn, foreign_keys_enabled);
    match (repair_result, restore_result) {
        (Ok(()), Ok(())) => {
            tracing::info!(target: "db", "{success_log}");
            Ok(())
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => Err(error.context(format!(
            "additionally failed to restore foreign-key enforcement: {restore_error}"
        ))),
    }
}

/// Restore the requested `SQLite` foreign-key enforcement state.
fn restore_foreign_keys(conn: &rusqlite::Connection, enabled: bool) -> Result<()> {
    let statement = if enabled {
        "PRAGMA foreign_keys = ON;"
    } else {
        "PRAGMA foreign_keys = OFF;"
    };
    conn.execute_batch(statement)
        .context("Restore foreign-key enforcement after schema repair")
}

/// Read the current `AUTOINCREMENT` high-water mark for one table.
fn autoincrement_sequence(conn: &rusqlite::Connection, table: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name = ?1",
        [table],
        |row| row.get(0),
    )
    .optional()
    .with_context(|| format!("Read {table} AUTOINCREMENT sequence failed"))
}

/// Preserve an `AUTOINCREMENT` high-water mark across a table rebuild.
fn restore_autoincrement_sequence(
    conn: &rusqlite::Connection,
    table: &str,
    prior_sequence: Option<i64>,
) -> Result<()> {
    let Some(prior_sequence) = prior_sequence else {
        return Ok(());
    };
    let current_max = conn
        .query_row(
            &format!("SELECT COALESCE(MAX(id), 0) FROM {table}"),
            [],
            |row| row.get::<_, i64>(0),
        )
        .with_context(|| format!("Read rebuilt {table} maximum id failed"))?;
    let sequence = prior_sequence.max(current_max);
    conn.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table])
        .with_context(|| format!("Clear rebuilt {table} AUTOINCREMENT sequence failed"))?;
    conn.execute(
        "INSERT INTO sqlite_sequence (name, seq) VALUES (?1, ?2)",
        rusqlite::params![table, sequence],
    )
    .with_context(|| format!("Restore rebuilt {table} AUTOINCREMENT sequence failed"))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Normalized structure of all application schema objects and tables.
struct SchemaShape {
    /// Schema objects keyed by name.
    objects: BTreeMap<String, SchemaObject>,
    /// Detailed table shapes keyed by table name.
    tables: BTreeMap<String, TableShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Normalized `SQLite` schema object.
struct SchemaObject {
    /// `SQLite` object type.
    kind: String,
    /// Normalized creation SQL, when `SQLite` records it.
    sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Columns, foreign keys, and indexes comprising a table.
struct TableShape {
    /// Column definitions keyed by name.
    columns: BTreeMap<String, ColumnShape>,
    /// Foreign-key definitions in stable order.
    foreign_keys: BTreeSet<ForeignKeyShape>,
    /// Index definitions keyed by name.
    indexes: BTreeMap<String, IndexShape>,
    /// Normalized `CHECK` expressions, independent of harmless SQL formatting.
    check_constraints: BTreeSet<String>,
    /// Whether row identifiers use the never-reuse `AUTOINCREMENT` algorithm.
    autoincrement: bool,
    /// Whether the table deliberately omits its implicit row identifier.
    without_rowid: bool,
    /// Whether the table uses `SQLite` strict typing.
    strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Normalized `SQLite` column definition.
struct ColumnShape {
    /// Declared `SQLite` type.
    decl_type: String,
    /// Whether the column has a `NOT NULL` constraint.
    not_null: bool,
    /// Normalized default expression.
    default_value: Option<String>,
    /// One-based primary-key position, or zero when not in the key.
    primary_key_position: i64,
    /// `SQLite` hidden-column classification.
    hidden: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
/// Normalized `SQLite` foreign-key definition.
struct ForeignKeyShape {
    /// Referenced table.
    table_name: String,
    /// Local source column.
    from_column: String,
    /// Referenced destination column.
    to_column: Option<String>,
    /// Update action.
    on_update: String,
    /// Delete action.
    on_delete: String,
    /// `SQLite` match rule.
    match_rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Normalized `SQLite` index definition.
struct IndexShape {
    /// Whether index keys must be unique.
    unique: bool,
    /// `SQLite` index-origin code.
    origin: String,
    /// Whether the index is partial.
    partial: bool,
    /// Ordered key columns.
    columns: Vec<IndexColumnShape>,
    /// Normalized partial-index predicate.
    where_clause: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One normalized `SQLite` index key column.
struct IndexColumnShape {
    /// Sequence position in the index.
    seqno: i64,
    /// Source-table column identifier.
    cid: i64,
    /// Source column name, absent for expressions or row identifiers.
    name: Option<String>,
    /// Whether the key is sorted descending.
    descending: bool,
    /// Applied collation name.
    collation: Option<String>,
}

/// Verify exact structural equality with the generated baseline.
fn verify_database_schema_structure(conn: &rusqlite::Connection) -> Result<()> {
    let expected = expected_schema_shape()?;
    let actual = schema_shape(conn)?;
    let mut issues = Vec::new();

    compare_schema_objects(&expected, &actual, &mut issues);
    compare_tables(&expected, &actual, &mut issues);
    verify_required_sql_fragments(&actual, &mut issues);
    verify_persisted_domain_invariants(conn, &mut issues);
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

/// Record any legacy or externally modified rows outside the closed persisted domains.
fn verify_persisted_domain_invariants(conn: &rusqlite::Connection, issues: &mut Vec<String>) {
    for invariant in DOMAIN_INVARIANTS {
        let predicate = invariant.invalid_predicate.replace("ROW.", "candidate.");
        let sql = format!(
            "SELECT COUNT(*) FROM {table} AS candidate WHERE {predicate}",
            table = invariant.table,
        );
        match conn.query_row(&sql, [], |row| row.get::<_, i64>(0)) {
            Ok(0) => {}
            Ok(count) => issues.push(format!(
                "{} has {count} row(s) violating its persisted domain",
                invariant.table
            )),
            Err(error) => issues.push(format!(
                "{} persisted-domain check failed: {error}",
                invariant.table
            )),
        }
    }
}

/// Reject a stored reply counter that no longer matches its authoritative posts.
fn verify_thread_reply_counts(conn: &rusqlite::Connection) -> Result<()> {
    let mismatches = thread_reply_count_mismatches(conn)?;
    if mismatches == 0 {
        Ok(())
    } else {
        bail!("threads.reply_count differs from its authoritative posts for {mismatches} thread(s)")
    }
}

/// Count cached reply counters that differ from the non-OP post count.
fn thread_reply_count_mismatches(conn: &rusqlite::Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*)
         FROM threads AS thread
         WHERE thread.reply_count != (
             SELECT COUNT(*) FROM posts
             WHERE posts.thread_id = thread.id AND posts.is_op = 0
         )",
        [],
        |row| row.get(0),
    )
    .context("Audit threads.reply_count against posts failed")
}

/// Recompute the safe derived reply-count cache after the schema is proven compatible.
fn reconcile_thread_reply_counts(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("Begin thread reply-count reconciliation failed")?;
    let result = conn
        .execute(
            "UPDATE threads
             SET reply_count = (
                 SELECT COUNT(*) FROM posts
                 WHERE posts.thread_id = threads.id AND posts.is_op = 0
             )
             WHERE reply_count != (
                 SELECT COUNT(*) FROM posts
                 WHERE posts.thread_id = threads.id AND posts.is_op = 0
             )",
            [],
        )
        .context("Reconcile threads.reply_count from posts failed");

    match result {
        Ok(repaired) => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                drop(conn.execute_batch("ROLLBACK"));
                return Err(error).context("Commit thread reply-count reconciliation failed");
            }
            if repaired > 0 {
                tracing::warn!(
                    target: "db",
                    repaired,
                    "Recomputed divergent thread reply counters from posts"
                );
            }
            Ok(())
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

/// Return the type of the schema-version object, when one exists.
fn schema_version_object_type(conn: &rusqlite::Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT type FROM sqlite_schema WHERE name = 'schema_version'",
        [],
        |row| row.get(0),
    )
    .optional()
    .context("Inspect schema_version object type failed")
}

/// Inspect the version table's shape, cardinality, and sole value.
fn schema_version_metadata_issues(conn: &rusqlite::Connection) -> Result<Vec<String>> {
    let mut issues = Vec::new();
    match schema_version_object_type(conn)? {
        None => {
            issues.push(format!(
                "schema_version table is missing; expected {BASELINE_SCHEMA_VERSION}"
            ));
            return Ok(issues);
        }
        Some(object_type) if object_type != "table" => {
            issues.push(format!(
                "schema_version is a {object_type}, expected a table"
            ));
            return Ok(issues);
        }
        Some(_) => {}
    }

    let expected = expected_schema_version_shape()?;
    let actual = table_shape(conn, "schema_version")?;
    if actual != expected {
        issues.push("schema_version table definition differs from baseline".to_owned());
    }

    let mut stmt = match conn.prepare("SELECT CAST(version AS TEXT) FROM schema_version") {
        Ok(stmt) => stmt,
        Err(error) => {
            issues.push(format!("schema_version values cannot be read: {error}"));
            return Ok(issues);
        }
    };
    let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(error) => {
            issues.push(format!("schema_version values cannot be queried: {error}"));
            return Ok(issues);
        }
    };
    let mut versions = Vec::new();
    for row in rows {
        match row {
            Ok(version) => versions.push(version),
            Err(error) => {
                issues.push(format!("schema_version value cannot be read: {error}"));
                return Ok(issues);
            }
        }
    }
    if versions.len() != 1 {
        issues.push(format!(
            "schema_version must contain exactly one row, found {}",
            versions.len()
        ));
    } else if versions.first().map(String::as_str) != Some(BASELINE_SCHEMA_VERSION) {
        issues.push(format!(
            "database schema version is {}, expected {BASELINE_SCHEMA_VERSION}",
            versions.first().map_or("missing", String::as_str)
        ));
    }
    Ok(issues)
}

/// Build the canonical schema-version table shape in isolation.
fn expected_schema_version_shape() -> Result<TableShape> {
    let expected = rusqlite::Connection::open_in_memory()
        .context("Open in-memory schema-version connection failed")?;
    expected
        .execute_batch(
            "CREATE TABLE schema_version (
                 version TEXT NOT NULL PRIMARY KEY
             );",
        )
        .context("Create in-memory schema-version baseline failed")?;
    table_shape(&expected, "schema_version")
}

/// Build the expected schema shape in an isolated in-memory database.
fn expected_schema_shape() -> Result<SchemaShape> {
    let expected = rusqlite::Connection::open_in_memory()
        .context("Open in-memory baseline schema connection failed")?;
    create_baseline_schema_objects(&expected)?;
    schema_shape(&expected)
}

/// Inspect and normalize the complete application schema.
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

/// Inspect all non-internal schema objects except the version table.
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

/// Inspect the complete normalized shape of one table.
fn table_shape(conn: &rusqlite::Connection, table: &str) -> Result<TableShape> {
    let definition = schema_sql_for_object(conn, "table", table)?
        .with_context(|| format!("Table {table} has no recorded SQL definition"))?;
    let tokens = schema_sql_tokens(&definition);
    Ok(TableShape {
        columns: table_columns(conn, table)?,
        foreign_keys: table_foreign_keys(conn, table)?,
        indexes: table_indexes(conn, table)?,
        check_constraints: check_constraints_from_tokens(&tokens),
        autoincrement: tokens.iter().any(|token| token == "autoincrement"),
        without_rowid: tokens
            .windows(2)
            .any(|tokens| tokens == ["without", "rowid"]),
        strict: tokens.iter().any(|token| token == "strict"),
    })
}

/// Inspect normalized columns for one table.
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

/// Inspect normalized foreign keys for one table.
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

/// Inspect normalized indexes for one table.
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

/// Inspect ordered key columns for one index.
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

/// Read creation SQL for a named schema object.
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

/// Record missing, extra, or mismatched schema objects.
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

/// Record table-level column, foreign-key, and index mismatches.
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
        if actual_table.check_constraints != expected_table.check_constraints {
            issues.push(format!("{table} CHECK constraints differ from baseline"));
        }
        if actual_table.autoincrement != expected_table.autoincrement {
            issues.push(format!(
                "{table} AUTOINCREMENT semantics differ from baseline"
            ));
        }
        if actual_table.without_rowid != expected_table.without_rowid {
            issues.push(format!("{table} ROWID semantics differ from baseline"));
        }
        if actual_table.strict != expected_table.strict {
            issues.push(format!(
                "{table} strict-typing semantics differ from baseline"
            ));
        }
    }
}

/// Record missing, extra, or mismatched table columns.
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

/// SQL constraints that must appear verbatim after normalization.
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

/// Record table definitions missing required SQL constraints.
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

/// Run `SQLite` quick and foreign-key health checks.
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

/// Extract and normalize a partial index's `WHERE` clause.
fn index_where_clause(sql: &str) -> Option<String> {
    let lower = sql.to_ascii_lowercase();
    let where_index = lower.find(" where ")?;
    sql.get(where_index..).map(str::trim).map(str::to_owned)
}

/// Normalize schema SQL for stable structural comparison.
fn schema_sql_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();

    let flush_current = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            tokens.push(std::mem::take(current));
        }
    };

    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek() == Some(&'-') {
            flush_current(&mut current, &mut tokens);
            let _second_dash = chars.next();
            for comment_ch in chars.by_ref() {
                if comment_ch == '\n' {
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            flush_current(&mut current, &mut tokens);
            let _asterisk = chars.next();
            let mut previous = '\0';
            for comment_ch in chars.by_ref() {
                if previous == '*' && comment_ch == '/' {
                    break;
                }
                previous = comment_ch;
            }
            continue;
        }
        if ch.is_whitespace() {
            flush_current(&mut current, &mut tokens);
            continue;
        }
        if ch == '\'' {
            flush_current(&mut current, &mut tokens);
            let mut literal = String::from("'");
            while let Some(literal_ch) = chars.next() {
                literal.push(literal_ch);
                if literal_ch == '\'' {
                    if chars.peek() == Some(&'\'') {
                        literal.push(chars.next().unwrap_or('\''));
                    } else {
                        break;
                    }
                }
            }
            tokens.push(literal);
            continue;
        }
        if matches!(ch, '"' | '`' | '[') {
            flush_current(&mut current, &mut tokens);
            let terminator = if ch == '[' { ']' } else { ch };
            let mut identifier = String::new();
            for identifier_ch in chars.by_ref() {
                if identifier_ch == terminator {
                    break;
                }
                identifier.extend(identifier_ch.to_lowercase());
            }
            tokens.push(identifier);
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else {
            flush_current(&mut current, &mut tokens);
            tokens.push(ch.to_string());
        }
    }
    flush_current(&mut current, &mut tokens);
    tokens
}

/// Extract normalized, balanced `CHECK` expressions from tokenized table SQL.
fn check_constraints_from_tokens(tokens: &[String]) -> BTreeSet<String> {
    let mut constraints = BTreeSet::new();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        let next_index = index.saturating_add(1);
        if token != "check" || tokens.get(next_index).map(String::as_str) != Some("(") {
            index += 1;
            continue;
        }

        let start = next_index;
        let mut depth = 0_i64;
        let mut end = start;
        while let Some(expression_token) = tokens.get(end) {
            match expression_token.as_str() {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(expression) = tokens.get(start..=end) {
                            constraints.insert(expression.join(" "));
                        }
                        break;
                    }
                }
                _ => {}
            }
            end += 1;
        }
        index = end.saturating_add(1);
    }
    constraints
}

/// Normalize schema SQL for stable structural comparison.
fn normalize_schema_sql(sql: &str) -> String {
    schema_sql_tokens(sql).join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        baseline_schema_version, create_base_tables, create_baseline_schema_objects,
        create_indexes, domain_trigger_name, ensure_board_access_invariants,
        ensure_post_invariants, ensure_posts_search_index, install_or_migrate_schema,
        normalize_database_schema_version, normalize_schema_sql, read_schema_version,
        run_structural_schema_repair, schema_shape, verify_database_schema,
    };
    use anyhow::{bail, ensure, Context as _, Result};

    fn schema_version(conn: &rusqlite::Connection) -> Result<String> {
        Ok(conn.query_row("SELECT version FROM schema_version", [], |row| row.get(0))?)
    }

    fn object_exists(conn: &rusqlite::Connection, kind: &str, name: &str) -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM sqlite_master
                WHERE type = ?1 AND name = ?2
            )",
            rusqlite::params![kind, name],
            |row| row.get(0),
        )?)
    }

    fn column_default(
        conn: &rusqlite::Connection,
        table: &str,
        column: &str,
    ) -> Result<Option<String>> {
        Ok(conn.query_row(
            "SELECT dflt_value FROM pragma_table_info(?1) WHERE name = ?2",
            rusqlite::params![table, column],
            |row| row.get(0),
        )?)
    }

    fn query_plan_details(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(3))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn create_pre_hardening_schema(conn: &rusqlite::Connection, version: &str) -> Result<()> {
        create_base_tables(conn)?;
        create_indexes(conn)?;
        ensure_posts_search_index(conn)?;
        ensure_post_invariants(conn)?;
        ensure_board_access_invariants(conn)?;
        conn.execute_batch(
            "CREATE TABLE schema_version (
                 version TEXT NOT NULL PRIMARY KEY
             );",
        )?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )?;
        Ok(())
    }

    fn assert_sql_rejected(
        conn: &rusqlite::Connection,
        sql: &str,
        expected_message: &str,
    ) -> Result<()> {
        let error = conn
            .execute_batch(sql)
            .err()
            .with_context(|| format!("SQL unexpectedly accepted: {sql}"))?;
        if !error.to_string().contains(expected_message) {
            bail!("unexpected rejection for {sql}: {error}");
        }
        Ok(())
    }

    fn rebuild_boards_with_historical_v41_shape(conn: &rusqlite::Connection) -> Result<()> {
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
        )?;
        ensure_board_access_invariants(conn)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn fresh_database_installs_140_baseline_directly() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;

        assert_eq!(
            schema_version(&conn)?,
            baseline_schema_version(),
            "fresh schema should record the release baseline"
        );
        assert_eq!(
            baseline_schema_version(),
            env!("CARGO_PKG_VERSION"),
            "compiled baseline should match the release"
        );
        assert!(
            object_exists(&conn, "table", "boards")?,
            "boards table should be installed"
        );
        assert!(
            object_exists(&conn, "table", "posts")?,
            "posts table should be installed"
        );
        assert!(
            object_exists(&conn, "table", "posts_fts")?,
            "full-text table should be installed"
        );
        assert!(
            object_exists(&conn, "index", "idx_posts_one_op_per_thread")?,
            "one-OP invariant index should be installed"
        );
        for index in super::ADDITIVE_BASELINE_INDEXES {
            assert!(
                object_exists(&conn, "index", index)?,
                "additive baseline index {index} should be installed"
            );
        }
        assert!(
            object_exists(&conn, "index", "idx_banner_assets_scope_sort")?,
            "banner scope index should be installed"
        );
        assert!(
            object_exists(&conn, "trigger", "posts_ai")?,
            "full-text insert trigger should be installed"
        );
        assert!(
            object_exists(&conn, "trigger", "posts_board_match_insert")?,
            "post-board invariant trigger should be installed"
        );
        assert!(
            object_exists(&conn, "trigger", "boards_access_mode_insert")?,
            "board access-mode trigger should be installed"
        );
        verify_database_schema(&conn)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn concurrent_fresh_installers_converge_on_one_complete_schema() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let database_path = temp_dir.path().join("concurrent-install.sqlite3");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let results = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..2 {
                let path = database_path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                handles.push(scope.spawn(move || -> Result<()> {
                    let conn = rusqlite::Connection::open(path)?;
                    conn.busy_timeout(std::time::Duration::from_secs(5))?;
                    barrier.wait();
                    install_or_migrate_schema(&conn)
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("schema installer thread panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        assert_eq!(results.len(), 2, "both schema installers should finish");

        let conn = rusqlite::Connection::open(database_path)?;
        verify_database_schema(&conn)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn recognized_baseline_index_drift_is_repaired_without_data_loss() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_baseline_schema_objects(&conn)?;
        for index in super::ADDITIVE_BASELINE_INDEXES {
            conn.execute_batch(&format!("DROP INDEX {index}"))?;
        }
        conn.execute_batch(
            "CREATE INDEX idx_file_hashes ON file_hashes(sha256);
             CREATE INDEX idx_posts_thread_id ON posts(thread_id);
             CREATE TABLE schema_version (version TEXT NOT NULL PRIMARY KEY);",
        )?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [baseline_schema_version()],
        )?;
        conn.execute(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random')",
            [],
        )?;

        normalize_database_schema_version(&conn)?;

        assert_eq!(
            conn.query_row("SELECT name FROM boards WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?,
            "Random",
            "additive index repair must preserve application data"
        );
        for index in super::ADDITIVE_BASELINE_INDEXES {
            assert!(
                object_exists(&conn, "index", index)?,
                "missing additive index {index} should be repaired"
            );
        }
        for index in super::REDUNDANT_LEGACY_INDEXES {
            assert!(
                !object_exists(&conn, "index", index)?,
                "redundant legacy index {index} should be removed"
            );
        }
        verify_database_schema(&conn)?;
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn representative_query_plans_use_the_audited_indexes() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;

        let media_plan = query_plan_details(
            &conn,
            "EXPLAIN QUERY PLAN
             SELECT 1 FROM posts
             WHERE file_path = 'b/file.webp'
                OR thumb_path = 'b/file.webp'
                OR audio_file_path = 'b/file.webp'
             LIMIT 1",
        )?;
        for index in [
            "idx_posts_file_path",
            "idx_posts_thumb_path",
            "idx_posts_audio_file_path",
        ] {
            assert!(
                media_plan.iter().any(|detail| detail.contains(index)),
                "media reference plan should use {index}: {media_plan:?}"
            );
        }

        let poll_plan = query_plan_details(
            &conn,
            "EXPLAIN QUERY PLAN
             SELECT po.id, po.poll_id, po.text, po.position,
                    (SELECT COUNT(*) FROM poll_votes AS pv
                     WHERE pv.option_id = po.id AND pv.poll_id = po.poll_id)
             FROM poll_options AS po
             WHERE po.poll_id = 1
             ORDER BY po.position ASC",
        )?;
        for index in [
            "idx_poll_options_poll_position",
            "idx_poll_votes_option_poll",
        ] {
            assert!(
                poll_plan.iter().any(|detail| detail.contains(index)),
                "poll plan should use {index}: {poll_plan:?}"
            );
        }
        assert!(
            poll_plan
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE")),
            "poll plan should not require temporary grouping or sorting: {poll_plan:?}"
        );

        let appeal_status_plan = query_plan_details(
            &conn,
            "EXPLAIN QUERY PLAN
             SELECT id FROM ban_appeals
             WHERE status = 'open' ORDER BY created_at DESC LIMIT 200",
        )?;
        assert!(
            appeal_status_plan
                .iter()
                .any(|detail| detail.contains("idx_ban_appeals_status_created")),
            "open-appeal plan should use the status/time index: {appeal_status_plan:?}"
        );
        let appeal_ip_plan = query_plan_details(
            &conn,
            "EXPLAIN QUERY PLAN
             SELECT COUNT(*) FROM ban_appeals
             WHERE ip_hash = 'address-hash' AND created_at > 1",
        )?;
        assert!(
            appeal_ip_plan
                .iter()
                .any(|detail| detail.contains("idx_ban_appeals_ip_created")),
            "recent-appeal plan should use the address/time index: {appeal_ip_plan:?}"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn existing_current_schema_is_adopted_as_140_without_data_loss() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_baseline_schema_objects(&conn)?;
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
        )?;

        install_or_migrate_schema(&conn)?;

        assert_eq!(
            schema_version(&conn)?,
            baseline_schema_version(),
            "adopted schema should receive the release version"
        );
        let body: String = conn.query_row("SELECT body FROM posts WHERE id = 100", [], |row| {
            row.get(0)
        })?;
        assert_eq!(
            body, "preserved body",
            "schema adoption should preserve post data"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn historical_v41_schema_drift_repairs_to_140_baseline_without_data_loss() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_baseline_schema_objects(&conn)?;
        conn.execute(
            "INSERT INTO boards (
                id, short_name, name, allow_editing, allow_self_delete,
                allow_video_embeds, show_poster_ids, banner_mode, access_mode
             )
             VALUES (1, 'b', 'Random', 0, 1, 0, 1, 'override', 'public')",
            [],
        )?;
        rebuild_boards_with_historical_v41_shape(&conn)?;
        conn.execute_batch(
            "CREATE INDEX idx_themes_enabled_sort
                 ON themes(enabled, sort_order, slug);
             CREATE TABLE schema_version (
                 version INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(version)
             );
             INSERT INTO schema_version (version) VALUES (41);",
        )?;

        let error = verify_database_schema(&conn)
            .err()
            .context("legacy drift should fail strict check")?;
        let message = error.to_string();
        assert!(
            message.contains("idx_themes_enabled_sort")
                && message.contains("boards.allow_editing definition differs")
                && message.contains("missing required constraint CHECK (access_mode"),
            "unexpected strict-check error: {error:#}"
        );

        install_or_migrate_schema(&conn)?;

        assert_eq!(
            schema_version(&conn)?,
            baseline_schema_version(),
            "repaired schema should be stamped with the release version"
        );
        assert!(
            !object_exists(&conn, "index", "idx_themes_enabled_sort")?,
            "obsolete theme index should be removed"
        );
        verify_database_schema(&conn)?;
        assert_eq!(
            column_default(&conn, "boards", "allow_editing")?.as_deref(),
            Some("1"),
            "editing default should be repaired"
        );
        assert_eq!(
            column_default(&conn, "boards", "allow_self_delete")?.as_deref(),
            Some("1"),
            "self-delete default should be repaired"
        );

        let board_flags: (i64, i64, i64, i64, String, String) = conn.query_row(
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
        )?;
        assert_eq!(
            board_flags,
            (0, 1, 0, 1, "override".to_owned(), "public".to_owned()),
            "explicit board values should survive the table rebuild"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn partial_schema_fails_closed_without_creating_missing_tables() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
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
        )?;

        let error = install_or_migrate_schema(&conn)
            .err()
            .context("partial schema should fail")?;
        let expected = format!(
            "does not match RustChan {} baseline",
            baseline_schema_version()
        );
        assert!(
            error.to_string().contains(&expected),
            "unexpected error: {error:#}"
        );

        assert!(
            !object_exists(&conn, "table", "posts")?,
            "failed normalization must not create missing tables"
        );
        assert_eq!(
            read_schema_version(&conn)?,
            Some("36".to_owned()),
            "failed normalization must preserve the recorded version"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn unknown_schema_object_fails_closed() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        conn.execute("CREATE TABLE operator_notes (body TEXT)", [])?;

        let error = normalize_database_schema_version(&conn)
            .err()
            .context("unknown table should fail normalization")?;
        assert!(
            error
                .to_string()
                .contains("unexpected table operator_notes"),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn fresh_schema_uses_new_board_feature_defaults() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;

        install_or_migrate_schema(&conn)?;
        conn.execute(
            "INSERT INTO boards (short_name, name) VALUES ('fresh', 'Fresh Board')",
            [],
        )?;

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
            )?;
        assert_eq!(
            flags,
            (
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_AUDIO),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_SHOW_POSTER_IDS),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_EDITING),
                i64::from(crate::test_fixtures::DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE),
            ),
            "fresh schema should use the standardized board defaults"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn invalid_structural_change_does_not_stamp_140() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_baseline_schema_objects(&conn)?;
        conn.execute_batch(
            "CREATE TABLE schema_version (
                version INTEGER NOT NULL DEFAULT 0,
                UNIQUE(version)
            );
            INSERT INTO schema_version (version) VALUES (41);
            DROP TRIGGER posts_board_match_insert;",
        )?;

        let error = install_or_migrate_schema(&conn)
            .err()
            .context("invalid schema should fail")?;
        assert!(
            error
                .to_string()
                .contains("missing trigger posts_board_match_insert"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            read_schema_version(&conn)?,
            Some("41".to_owned()),
            "failed normalization must preserve the legacy version"
        );
        Ok(())
    }

    #[test]
    fn closed_scalar_domains_are_enforced_by_sqlite() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        conn.execute(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random')",
            [],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject) VALUES (10, 1, 'thread')",
            [],
        )?;
        conn.execute(
            "INSERT INTO posts
             (id, thread_id, board_id, body, body_html, deletion_token, is_op)
             VALUES (100, 10, 1, 'body', 'body', 'token', 1)",
            [],
        )?;

        for sql in [
            "UPDATE boards SET nsfw = 2 WHERE id = 1",
            "UPDATE boards SET max_threads = 0 WHERE id = 1",
            "UPDATE boards SET short_name = '../bad' WHERE id = 1",
        ] {
            assert_sql_rejected(
                &conn,
                sql,
                "boards row violates a persisted domain invariant",
            )?;
        }
        assert_sql_rejected(
            &conn,
            "UPDATE threads SET sticky = -1 WHERE id = 10",
            "threads row violates a persisted domain invariant",
        )?;
        for sql in [
            "UPDATE posts SET is_op = 7 WHERE id = 100",
            "UPDATE posts SET media_type = 'executable' WHERE id = 100",
            "UPDATE posts SET media_processing_state = 'lost' WHERE id = 100",
            "UPDATE posts SET file_size = -1 WHERE id = 100",
        ] {
            assert_sql_rejected(
                &conn,
                sql,
                "posts row violates a persisted domain invariant",
            )?;
        }
        assert_sql_rejected(
            &conn,
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('bad', 'shell_command', '{}')",
            "pending_fs_ops.kind is not supported",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO ban_appeals (ip_hash, status) VALUES ('hash', 'approved')",
            "ban_appeals.status is not supported",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO background_jobs (job_type, payload)
             VALUES ('arbitrary_code', '{}')",
            "background_jobs row violates a persisted domain invariant",
        )?;
        conn.execute(
            "INSERT INTO background_jobs (id, job_type, payload)
             VALUES (1, 'spam_check', '{}')",
            [],
        )?;
        assert_sql_rejected(
            &conn,
            "UPDATE background_jobs SET status = 'stuck' WHERE id = 1",
            "background_jobs row violates a persisted domain invariant",
        )?;
        Ok(())
    }

    #[test]
    fn cross_table_and_compound_domains_are_enforced_by_sqlite() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        install_or_migrate_schema(&conn)?;
        conn.execute_batch(
            "INSERT INTO boards (id, short_name, name) VALUES
                 (1, 'a', 'A'), (2, 'b', 'B');
             INSERT INTO threads (id, board_id, subject) VALUES
                 (10, 1, 'A thread'), (20, 2, 'B thread');
             INSERT INTO posts
                 (id, thread_id, board_id, body, body_html, deletion_token, is_op)
             VALUES
                 (100, 10, 1, 'A post', 'A post', 'token-a', 1),
                 (200, 20, 2, 'B post', 'B post', 'token-b', 1);
             INSERT INTO polls (id, thread_id, question, expires_at)
             VALUES (1000, 10, 'A poll', 2000000000),
                    (2000, 20, 'B poll', 2000000000);
             INSERT INTO poll_options (id, poll_id, text, position)
             VALUES (1001, 1000, 'A option', 0),
                    (2001, 2000, 'B option', 0);",
        )?;

        assert_sql_rejected(
            &conn,
            "INSERT INTO poll_votes (poll_id, option_id, ip_hash)
             VALUES (1000, 2001, 'voter')",
            "poll_votes.option_id must belong to poll_id",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO reports (post_id, thread_id, board_id, reporter_hash)
             VALUES (100, 20, 2, 'reporter')",
            "reports row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO post_submissions
             (submission_token, ip_hash, board_id, thread_id, post_id, is_thread)
             VALUES ('submission', 'poster', 1, 10, 100, 0)",
            "post_submissions row does not match its post",
        )?;
        conn.execute_batch(
            "INSERT INTO poll_votes (poll_id, option_id, ip_hash)
                 VALUES (1000, 1001, 'valid-voter');
             INSERT INTO reports (post_id, thread_id, board_id, reporter_hash)
                 VALUES (100, 10, 1, 'valid-reporter');
             INSERT INTO post_submissions
                 (submission_token, ip_hash, board_id, thread_id, post_id, is_thread)
                 VALUES ('valid-submission', 'poster', 1, 10, 100, 1);",
        )?;
        assert_sql_rejected(
            &conn,
            "UPDATE threads SET board_id = 2 WHERE id = 10",
            "threads row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "UPDATE poll_options SET poll_id = 2000 WHERE id = 1001",
            "poll_options row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "UPDATE posts SET is_op = 0 WHERE id = 100",
            "posts row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO banner_assets
             (scope_type, board_id, storage_key, width, height, file_size)
             VALUES ('global', 1, '0123456789abcdef0123456789abcdef', 585, 75, 1024)",
            "banner_assets row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO banner_assets
             (scope_type, storage_key, width, height, file_size, target_type, target_value)
             VALUES ('global', '0123456789abcdef0123456789abcdef', 585, 75, 1024,
                     'external_url', '')",
            "banner_assets row violates a persisted domain invariant",
        )?;
        assert_sql_rejected(
            &conn,
            "INSERT INTO themes (slug, display_name) VALUES ('Unsafe Theme', 'Unsafe')",
            "themes row violates a persisted domain invariant",
        )?;
        Ok(())
    }

    #[test]
    fn legacy_domain_migration_repairs_counters_and_matches_fresh_schema() -> Result<()> {
        let migrated = rusqlite::Connection::open_in_memory()?;
        create_pre_hardening_schema(&migrated, "1.4.0")?;
        migrated.execute_batch(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
             INSERT INTO threads (id, board_id, subject, reply_count)
             VALUES (10, 1, 'thread', 99);
             INSERT INTO posts
                 (id, thread_id, board_id, body, body_html, deletion_token, is_op)
             VALUES
                 (100, 10, 1, 'op', 'op', 'op-token', 1),
                 (101, 10, 1, 'reply', 'reply', 'reply-token', 0);",
        )?;

        normalize_database_schema_version(&migrated)?;
        let first_shape = schema_shape(&migrated)?;
        normalize_database_schema_version(&migrated)?;
        ensure!(
            schema_shape(&migrated)? == first_shape,
            "retrying the migration should be idempotent"
        );
        let reply_count =
            migrated.query_row("SELECT reply_count FROM threads WHERE id = 10", [], |row| {
                row.get::<_, i64>(0)
            })?;
        ensure!(
            reply_count == 1,
            "the authoritative non-OP post count should repair the cache"
        );

        let fresh = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&fresh)?;
        ensure!(
            schema_shape(&migrated)? == schema_shape(&fresh)?,
            "fresh and migrated application schemas should be equivalent"
        );
        verify_database_schema(&migrated)?;
        Ok(())
    }

    #[test]
    fn malformed_legacy_domain_rolls_back_without_stamping() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_pre_hardening_schema(&conn, "41")?;
        conn.execute(
            "INSERT INTO boards (id, short_name, name, max_threads)
             VALUES (1, 'b', 'Random', 0)",
            [],
        )?;

        let error = normalize_database_schema_version(&conn)
            .err()
            .context("malformed legacy domain should fail migration")?;
        ensure!(
            error.to_string().contains("boards has 1 row(s)"),
            "unexpected error: {error:#}"
        );
        ensure!(
            read_schema_version(&conn)?.as_deref() == Some("41"),
            "failed domain migration must not stamp success"
        );
        let max_threads =
            conn.query_row("SELECT max_threads FROM boards WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })?;
        ensure!(
            max_threads == 0,
            "failed migration must not rewrite ambiguous historical data"
        );
        ensure!(
            !object_exists(&conn, "trigger", "boards_domain_insert")?,
            "failed migration must roll back newly created triggers"
        );
        Ok(())
    }

    #[test]
    fn ambiguous_legacy_access_mode_fails_without_coercion_or_stamping() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        create_baseline_schema_objects(&conn)?;
        rebuild_boards_with_historical_v41_shape(&conn)?;
        conn.execute(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random')",
            [],
        )?;
        let access_trigger: String = conn.query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'boards_access_mode_update'",
            [],
            |row| row.get(0),
        )?;
        conn.execute_batch("DROP TRIGGER boards_access_mode_update")?;
        conn.execute(
            "UPDATE boards SET access_mode = 'unknown_mode' WHERE id = 1",
            [],
        )?;
        conn.execute_batch(&access_trigger)?;
        conn.execute_batch(
            "CREATE TABLE schema_version (version TEXT NOT NULL PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES ('41');",
        )?;

        let error = normalize_database_schema_version(&conn)
            .err()
            .context("ambiguous legacy access mode should fail migration")?;
        ensure!(
            format!("{error:#}").contains("CHECK constraint failed"),
            "unexpected error: {error:#}"
        );
        ensure!(read_schema_version(&conn)?.as_deref() == Some("41"));
        let access_mode =
            conn.query_row("SELECT access_mode FROM boards WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?;
        ensure!(
            access_mode == "unknown_mode",
            "failed migration must not silently reinterpret an unknown access mode"
        );
        Ok(())
    }

    #[test]
    fn boards_rebuild_preserves_foreign_keys_and_autoincrement_high_water_mark() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        create_baseline_schema_objects(&conn)?;
        rebuild_boards_with_historical_v41_shape(&conn)?;
        conn.execute_batch(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
             INSERT INTO threads (id, board_id, subject) VALUES (10, 1, 'child');
             INSERT INTO boards (id, short_name, name) VALUES (500, 'z', 'Deleted');
             DELETE FROM boards WHERE id = 500;
             CREATE TABLE schema_version (version TEXT NOT NULL PRIMARY KEY);
             INSERT INTO schema_version (version) VALUES ('41');",
        )?;

        install_or_migrate_schema(&conn)?;
        let foreign_keys = conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?;
        ensure!(
            foreign_keys == 1,
            "foreign-key enforcement must be restored after rebuild"
        );
        let foreign_key_issues =
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })?;
        ensure!(
            foreign_key_issues == 0,
            "rebuilt references should remain valid"
        );
        let next_id: i64 = conn.query_row(
            "INSERT INTO boards (short_name, name) VALUES ('n', 'Next') RETURNING id",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            next_id == 501,
            "a rebuild must not reuse an AUTOINCREMENT id issued before deletion"
        );
        conn.execute("DELETE FROM boards WHERE id = 1", [])?;
        let child_count =
            conn.query_row("SELECT COUNT(*) FROM threads WHERE id = 10", [], |row| {
                row.get::<_, i64>(0)
            })?;
        ensure!(
            child_count == 0,
            "ON DELETE CASCADE should survive the parent-table rebuild"
        );
        Ok(())
    }

    #[test]
    fn structural_repair_failure_rolls_back_ddl_and_restores_foreign_keys() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE original (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO original (id, value) VALUES (1, 'preserved');",
        )?;

        let result = run_structural_schema_repair(
            &conn,
            "CREATE TABLE replacement (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO replacement SELECT id, value FROM original;
             DROP TABLE original;
             ALTER TABLE replacement RENAME TO original;",
            "injected structural repair",
            "unused success log",
            |_conn| bail!("injected post-rebuild failure"),
        );
        ensure!(result.is_err(), "the injected repair should fail");
        ensure!(object_exists(&conn, "table", "original")?);
        ensure!(!object_exists(&conn, "table", "replacement")?);
        let original_value =
            conn.query_row("SELECT value FROM original WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })?;
        ensure!(original_value == "preserved");
        let foreign_keys = conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?;
        ensure!(foreign_keys == 1);
        Ok(())
    }

    #[test]
    fn malformed_current_version_metadata_is_detected_and_repaired() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        conn.execute_batch(
            "DROP TABLE schema_version;
             CREATE TABLE schema_version (version TEXT NOT NULL);
             INSERT INTO schema_version (version) VALUES ('1.4.1'), ('duplicate');",
        )?;

        let error = verify_database_schema(&conn)
            .err()
            .context("malformed version metadata should fail strict verification")?;
        ensure!(
            error
                .to_string()
                .contains("schema-version metadata is invalid"),
            "unexpected error: {error:#}"
        );
        normalize_database_schema_version(&conn)?;
        verify_database_schema(&conn)?;
        let version_rows = conn.query_row("SELECT COUNT(*) FROM schema_version", [], |row| {
            row.get::<_, i64>(0)
        })?;
        ensure!(version_rows == 1);
        Ok(())
    }

    #[test]
    fn non_table_version_object_fails_before_data_repair() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        conn.execute_batch(
            "INSERT INTO boards (id, short_name, name) VALUES (1, 'b', 'Random');
             INSERT INTO threads (id, board_id, subject, reply_count)
                 VALUES (10, 1, 'thread', 99);
             INSERT INTO posts
                 (id, thread_id, board_id, body, body_html, deletion_token, is_op)
                 VALUES (100, 10, 1, 'op', 'op', 'token', 1);
             DROP TABLE schema_version;
             CREATE VIEW schema_version AS SELECT '1.4.1' AS version;",
        )?;

        let error = normalize_database_schema_version(&conn)
            .err()
            .context("non-table version metadata should fail normalization")?;
        ensure!(
            error
                .to_string()
                .contains("schema_version is a view, expected a table"),
            "unexpected error: {error:#}"
        );
        let reply_count =
            conn.query_row("SELECT reply_count FROM threads WHERE id = 10", [], |row| {
                row.get::<_, i64>(0)
            })?;
        ensure!(
            reply_count == 99,
            "incompatible metadata must be rejected before derived-state repair"
        );
        Ok(())
    }

    #[test]
    fn altered_autoincrement_semantics_are_detected() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        conn.execute_batch(
            "CREATE TABLE bans_new (
                 id INTEGER PRIMARY KEY,
                 ip_hash TEXT NOT NULL,
                 reason TEXT,
                 expires_at INTEGER,
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             INSERT INTO bans_new SELECT * FROM bans;
             DROP TABLE bans;
             ALTER TABLE bans_new RENAME TO bans;
             CREATE INDEX idx_bans_ip ON bans(ip_hash);",
        )?;

        let error = verify_database_schema(&conn)
            .err()
            .context("altered AUTOINCREMENT semantics should fail verification")?;
        ensure!(
            error
                .to_string()
                .contains("bans AUTOINCREMENT semantics differ"),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    #[test]
    fn altered_domain_trigger_is_detected_but_sql_formatting_is_ignored() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        install_or_migrate_schema(&conn)?;
        let trigger_name = domain_trigger_name("ban_appeals", "insert");
        let canonical_sql: String = conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
            [&trigger_name],
            |row| row.get(0),
        )?;
        conn.execute_batch(&format!("DROP TRIGGER {trigger_name}"))?;
        conn.execute_batch(&canonical_sql.to_ascii_lowercase())?;
        verify_database_schema(&conn)?;

        conn.execute_batch(&format!(
            "DROP TRIGGER {trigger_name};
             CREATE TRIGGER {trigger_name}
             BEFORE INSERT ON ban_appeals BEGIN SELECT 1; END;"
        ))?;
        let error = verify_database_schema(&conn)
            .err()
            .context("altered trigger should fail strict verification")?;
        ensure!(
            error
                .to_string()
                .contains("ban_appeals_domain_insert definition differs"),
            "unexpected error: {error:#}"
        );
        ensure!(
            normalize_schema_sql(
                "CREATE /* formatting-only comment */ INDEX Example\n\
                 -- another comment\nON Things(\"Value\")"
            ) == normalize_schema_sql("create index example on things ( value )")
        );
        Ok(())
    }
}
