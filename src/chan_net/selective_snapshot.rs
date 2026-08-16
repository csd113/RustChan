//! `RustWave` gateway snapshot builders.
//
// Five scoped ZIP builders for the RustWave gateway layer.
// These builders are entirely separate from snapshot.rs (federation layer)
// so that their contracts remain independently evolvable.
//
// Builders:
//   build_full_snapshot(conn, since)           — all boards, active threads only
//   build_board_snapshot(conn, board, since)   — one board, active threads only
//   build_thread_snapshot(conn, thread_id, since) — one thread
//   build_archive_snapshot(conn, board)        — archived threads, no since support
//   build_force_refresh_snapshot(conn)         — everything including archives,
//                                                no timestamp filtering,
//                                                emits tracing::warn!
//
// All builders return (Vec<u8>, Uuid) — the raw ZIP bytes and the transaction ID
// embedded in metadata.json.
//
// SECURITY: GwPost / GwThread / GwBoard carry text fields only — no media columns,
// no file paths, no MIME types, no thumbnail paths. This boundary is enforced at
// the query level: only the columns listed in fetch_posts() are ever selected.
//
// Column verification (checked against src/db/posts.rs and src/db/threads.rs):
//   - Post body column:   `p.body`   (NOT `p.content`)
//   - Post author column: `p.name`   (NOT `p.author`)
//   - Board name column:  `b.name`   (NOT `b.title`)
//   - Thread subject:     `t.subject` — nullable, COALESCE to ''
//   - Thread archive:     `t.archived` (INTEGER 0/1)

use std::io::{Cursor, Write as _};

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, ZipWriter};

// ── Public structs ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
/// Public-board metadata included in a gateway snapshot.
pub struct GwBoard {
    /// Stable URL-facing board name.
    pub short_name: String,
    /// Human-readable board title.
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// Thread metadata included in a gateway snapshot.
pub struct GwThread {
    /// Local thread identifier.
    pub thread_id: i64,
    /// URL-facing name of the owning board.
    pub board: String,
    /// Thread subject, or an empty string when absent.
    pub subject: String,
    /// Unix timestamp at which the thread was created.
    pub created_at: u64,
    /// Number of posts included for the thread.
    pub post_count: u64,
    /// Whether the thread is archived.
    pub archived: bool,
}

/// SECURITY: No media fields. Text content only.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwPost {
    /// Local post identifier.
    pub post_id: i64,
    /// Local identifier of the owning thread.
    pub thread_id: i64,
    /// URL-facing name of the owning board.
    pub board: String,
    /// Displayed author name.
    pub author: String,
    /// Plain post body.
    pub content: String,
    /// Unix timestamp at which the post was created.
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
/// Metadata describing the contents and scope of a gateway snapshot.
pub struct GwMetadata {
    /// Unix timestamp at which the snapshot was generated.
    pub generated_at: u64,
    /// `RustChan` version that generated the snapshot.
    pub rustchan_version: String,
    /// Number of posts included in the snapshot.
    pub post_count: u64,
    /// Unique identifier for this snapshot transaction.
    pub tx_id: Uuid,
    /// Lower timestamp bound used for a delta snapshot, when present.
    pub since: Option<u64>,
    /// Whether timestamp filtering produced a delta snapshot.
    pub is_delta: bool,
    /// Whether archived threads are included.
    pub includes_archive: bool,
    /// One of: `"full"` | `"board"` | `"thread"` | `"archive"` | `"force_refresh"`
    pub scope: String,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Returns the current Unix timestamp in whole seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Finalizes an in-memory ZIP writer and returns its bytes.
fn finish_zip(zip: ZipWriter<Cursor<Vec<u8>>>) -> Result<Vec<u8>> {
    Ok(zip.finish()?.into_inner())
}

// ── Public snapshot builders ──────────────────────────────────────────────────

/// All boards, all active (non-archived) threads, and all their posts.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned
/// (delta mode). Thread metadata is always emitted in full regardless of `since`
/// so that `RustWave` can maintain a complete thread index.
///
/// # Errors
///
/// Returns an error when database reads, serialization, or ZIP construction fail.
pub fn build_full_snapshot(conn: &Connection, since: Option<u64>) -> Result<(Vec<u8>, Uuid)> {
    let boards = fetch_all_boards(conn)?;
    let threads = fetch_threads(conn, None, false)?;
    let posts = fetch_posts(conn, None, None, since, false)?;

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "full".to_owned(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All active (non-archived) threads and posts for a single board.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned.
/// Returns an error if `board_short_name` does not identify a known board.
///
/// # Errors
///
/// Returns an error for an unknown board or failed database/ZIP operation.
pub fn build_board_snapshot(
    conn: &Connection,
    board_short_name: &str,
    since: Option<u64>,
) -> Result<(Vec<u8>, Uuid)> {
    let board_id = board_id_by_short_name(conn, board_short_name)?;
    let boards = fetch_boards_by_id(conn, board_id)?;
    let threads = fetch_threads(conn, Some(board_id), false)?;
    let posts = fetch_posts(conn, Some(board_id), None, since, false)?;

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "board".to_owned(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All posts for a single thread.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned.
/// Returns an error if `thread_id` does not identify a known thread.
///
/// # Errors
///
/// Returns an error for an unknown thread or failed database/ZIP operation.
pub fn build_thread_snapshot(
    conn: &Connection,
    thread_id: i64,
    since: Option<u64>,
) -> Result<(Vec<u8>, Uuid)> {
    let threads = fetch_thread_by_id(conn, thread_id)?;
    let board_short = threads
        .first()
        .map(|t| t.board.clone())
        .ok_or_else(|| anyhow::anyhow!("Thread {thread_id} not found"))?;

    let boards = fetch_boards_by_short_name(conn, &board_short)?;
    let posts = fetch_posts(conn, None, Some(thread_id), since, false)?;

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "thread".to_owned(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All archived threads and their posts for a single board.
///
/// `since` is not supported for archive exports — archives are static by
/// definition once a thread is archived. Always returns the full archive.
/// Returns an error if `board_short_name` does not identify a known board.
///
/// # Errors
///
/// Returns an error for an unknown board or failed database/ZIP operation.
pub fn build_archive_snapshot(
    conn: &Connection,
    board_short_name: &str,
) -> Result<(Vec<u8>, Uuid)> {
    let board_id = board_id_by_short_name(conn, board_short_name)?;
    let boards = fetch_boards_by_id(conn, board_id)?;
    let threads = fetch_threads(conn, Some(board_id), true)?;
    let posts = fetch_posts(conn, Some(board_id), None, None, true)?;

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        since: None,
        is_delta: false,
        includes_archive: true,
        scope: "archive".to_owned(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// Everything exportable: all public boards, their active and archived
/// threads, and their posts.
///
/// Ignores all timestamps. Intended for initial sync and disaster recovery.
///
/// Emits a `tracing::warn!` to make force-refresh calls visible in the operator
/// log — a full database dump over the gateway is a heavyweight operation.
///
/// # Errors
///
/// Returns an error when database reads, serialization, or ZIP construction fail.
pub fn build_force_refresh_snapshot(conn: &Connection) -> Result<(Vec<u8>, Uuid)> {
    tracing::warn!(
        "Force refresh snapshot requested — returning all exportable data including archives"
    );

    let boards = fetch_all_boards(conn)?;

    let mut threads = fetch_threads(conn, None, false)?;
    let mut archived = fetch_threads(conn, None, true)?;
    threads.append(&mut archived);

    let mut posts = fetch_posts(conn, None, None, None, false)?;
    let mut archive_posts = fetch_posts(conn, None, None, None, true)?;
    posts.append(&mut archive_posts);

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_owned(),
        post_count: u64::try_from(posts.len())?,
        tx_id,
        since: None,
        is_delta: false,
        includes_archive: true,
        scope: "force_refresh".to_owned(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

// ── Private DB helpers ────────────────────────────────────────────────────────

/// Resolves an exportable board's local identifier.
fn board_id_by_short_name(conn: &Connection, short_name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id
         FROM boards
         WHERE short_name = ?1
           AND access_mode IN ('public', 'post_password')",
        rusqlite::params![short_name],
        |r| r.get(0),
    )
    .map_err(|_error| anyhow::anyhow!("Board '{short_name}' not found or not exportable"))
}

/// Load all boards for gateway snapshots.
///
/// `GwBoard.title` maps to the `boards.name` display-name column.
fn fetch_all_boards(conn: &Connection) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare(
        "SELECT short_name, name
         FROM boards
         WHERE access_mode IN ('public', 'post_password')
         ORDER BY nsfw ASC, display_order ASC, id ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(GwBoard {
                short_name: r.get(0)?,
                title: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Load one board by id for gateway snapshots.
fn fetch_boards_by_id(conn: &Connection, board_id: i64) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare(
        "SELECT short_name, name
         FROM boards
         WHERE id = ?1
           AND access_mode IN ('public', 'post_password')",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![board_id], |r| {
            Ok(GwBoard {
                short_name: r.get(0)?,
                title: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Load one board by short name for gateway snapshots.
fn fetch_boards_by_short_name(conn: &Connection, short_name: &str) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare(
        "SELECT short_name, name
         FROM boards
         WHERE short_name = ?1
           AND access_mode IN ('public', 'post_password')",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![short_name], |r| {
            Ok(GwBoard {
                short_name: r.get(0)?,
                title: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Fetch threads filtered by board and archive status.
///
/// If `board_id` is `Some`, only threads belonging to that board are returned.
/// If `archived_only` is `true`, only archived threads are returned; otherwise
/// only active threads are returned.
///
/// Column verification (checked against src/db/threads.rs):
///   `t.id`, `b.short_name`, `t.subject` (nullable → COALESCE), `t.created_at` (INTEGER),
///   post count (correlated subquery), `t.archived` (INTEGER 0/1).
fn fetch_threads(
    conn: &Connection,
    board_id: Option<i64>,
    archived_only: bool,
) -> Result<Vec<GwThread>> {
    let archived_flag: i64 = i64::from(archived_only);

    let sql = match board_id {
        Some(_) => {
            "SELECT t.id, b.short_name, COALESCE(t.subject, ''), t.created_at,
                    (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id), t.archived
             FROM threads t JOIN boards b ON t.board_id = b.id
             WHERE t.board_id = ?1
               AND b.access_mode IN ('public', 'post_password')
               AND t.archived = ?2
             ORDER BY t.id"
        }
        None => {
            "SELECT t.id, b.short_name, COALESCE(t.subject, ''), t.created_at,
                    (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id), t.archived
             FROM threads t JOIN boards b ON t.board_id = b.id
             WHERE b.access_mode IN ('public', 'post_password')
               AND t.archived = ?1
             ORDER BY t.id"
        }
    };

    let mut stmt = conn.prepare(sql)?;

    let map_row = |r: &rusqlite::Row<'_>| {
        Ok(GwThread {
            thread_id: r.get(0)?,
            board: r.get(1)?,
            subject: r.get(2)?,
            created_at: r.get::<_, i64>(3)?.cast_unsigned(),
            post_count: r.get::<_, i64>(4)?.cast_unsigned(),
            archived: r.get::<_, i64>(5)? != 0,
        })
    };

    let rows: Vec<GwThread> = match board_id {
        Some(bid) => stmt
            .query_map(rusqlite::params![bid, archived_flag], map_row)?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt
            .query_map(rusqlite::params![archived_flag], map_row)?
            .collect::<rusqlite::Result<_>>()?,
    };

    Ok(rows)
}

/// Fetches one exportable thread by local identifier.
fn fetch_thread_by_id(conn: &Connection, thread_id: i64) -> Result<Vec<GwThread>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, b.short_name, COALESCE(t.subject, ''), t.created_at,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id), t.archived
         FROM threads t JOIN boards b ON t.board_id = b.id
         WHERE t.id = ?1
           AND b.access_mode IN ('public', 'post_password')",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![thread_id], |r| {
            Ok(GwThread {
                thread_id: r.get(0)?,
                board: r.get(1)?,
                subject: r.get(2)?,
                created_at: r.get::<_, i64>(3)?.cast_unsigned(),
                post_count: r.get::<_, i64>(4)?.cast_unsigned(),
                archived: r.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Fetch posts with optional board, thread, timestamp, and archive filters.
///
/// Parameters:
/// - `board_id`:      if `Some`, restrict to posts on that board
/// - `thread_id`:     if `Some`, restrict to posts in that thread
/// - `since`:         if `Some(ts)`, restrict to posts where `created_at > ts`
/// - `archived_only`: if `true`, only posts in archived threads; if `false`,
///   only posts in active threads
///
/// The query is built dynamically. The `?1` / `?2` slots are always
/// `archived_flag` and `since_val`. Board and thread filters consume `?3` and
/// `?4` respectively when present.
///
/// Column verification (checked against src/db/posts.rs):
///   `p.id`, `p.thread_id`, `b.short_name`, `p.name` (author), `p.body` (content),
///   `p.created_at`. No media columns are selected.
fn fetch_posts(
    conn: &Connection,
    board_id: Option<i64>,
    thread_id: Option<i64>,
    since: Option<u64>,
    archived_only: bool,
) -> Result<Vec<GwPost>> {
    let archived_flag: i64 = i64::from(archived_only);
    let since_val = since.unwrap_or(0).cast_signed();

    // Fixed parameters: ?1 = archived_flag, ?2 = since_val.
    // Optional parameters appended in order: board_id (?3), thread_id (?3 or ?4).
    let mut sql = String::from(
        "SELECT p.id, p.thread_id, b.short_name,
                COALESCE(p.name, 'anon'), COALESCE(p.body, ''), p.created_at
         FROM posts p
         JOIN threads t ON p.thread_id = t.id
         JOIN boards  b ON t.board_id  = b.id
         WHERE t.archived = ?1
           AND b.access_mode IN ('public', 'post_password')
           AND p.created_at > ?2",
    );

    if board_id.is_some() {
        sql.push_str(" AND b.id = ?3");
    }
    if thread_id.is_some() {
        let param_n = if board_id.is_some() { "?4" } else { "?3" };
        sql.push_str(" AND p.thread_id = ");
        sql.push_str(param_n);
    }
    sql.push_str(" ORDER BY p.id");

    let mut stmt = conn.prepare(&sql)?;

    let map_row = |r: &rusqlite::Row<'_>| {
        Ok(GwPost {
            post_id: r.get(0)?,
            thread_id: r.get(1)?,
            board: r.get(2)?,
            author: r.get(3)?,
            content: r.get(4)?,
            timestamp: r.get::<_, i64>(5)?.cast_unsigned(),
        })
    };

    let rows: Vec<GwPost> = match (board_id, thread_id) {
        (None, None) => stmt
            .query_map(rusqlite::params![archived_flag, since_val], map_row)?
            .collect::<rusqlite::Result<_>>()?,
        (Some(b), None) => stmt
            .query_map(rusqlite::params![archived_flag, since_val, b], map_row)?
            .collect::<rusqlite::Result<_>>()?,
        (None, Some(t)) => stmt
            .query_map(rusqlite::params![archived_flag, since_val, t], map_row)?
            .collect::<rusqlite::Result<_>>()?,
        (Some(b), Some(t)) => stmt
            .query_map(rusqlite::params![archived_flag, since_val, b, t], map_row)?
            .collect::<rusqlite::Result<_>>()?,
    };

    Ok(rows)
}

// ── ZIP packing ───────────────────────────────────────────────────────────────

/// Produce a ZIP archive containing four JSON files:
///   boards.json   — `[GwBoard]`
///   threads.json  — `[GwThread]`
///   posts.json    — `[GwPost]`
///   metadata.json — `GwMetadata`
fn pack_zip(
    boards: &[GwBoard],
    threads: &[GwThread],
    posts: &[GwPost],
    metadata: &GwMetadata,
) -> Result<Vec<u8>> {
    let buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let opts = SimpleFileOptions::default();

    zip.start_file("boards.json", opts)?;
    zip.write_all(&serde_json::to_vec(boards)?)?;

    zip.start_file("threads.json", opts)?;
    zip.write_all(&serde_json::to_vec(threads)?)?;

    zip.start_file("posts.json", opts)?;
    zip.write_all(&serde_json::to_vec(posts)?)?;

    zip.start_file("metadata.json", opts)?;
    zip.write_all(&serde_json::to_vec(metadata)?)?;

    finish_zip(zip)
}

#[cfg(test)]
/// Security-boundary tests for gateway and federation snapshot exports.
mod tests {
    use super::{
        build_archive_snapshot, build_board_snapshot, build_force_refresh_snapshot,
        build_full_snapshot, build_thread_snapshot, GwBoard, GwPost, GwThread,
    };
    use anyhow::{Context as _, Result};
    use std::io::{Cursor, Read as _};

    /// Builds a database containing public and protected board variants.
    fn setup_pool() -> Result<crate::db::DbPool> {
        let pool = crate::db::init_test_pool().context("initialize test database")?;
        let conn = pool.get().context("get database connection")?;
        conn.execute(
            "INSERT INTO boards
             (id, short_name, name, description, access_mode, access_password_hash)
             VALUES (1, 'public', 'Public', '', 'public', ''),
                    (2, 'secret', 'Secret', '', 'view_password', 'protected'),
                    (3, 'posting', 'Posting', '', 'post_password', 'protected')",
            [],
        )
        .context("insert boards")?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived)
             VALUES (11, 1, 'public active', 0),
                    (12, 1, 'public archive', 1),
                    (21, 2, 'secret active', 0),
                    (22, 2, 'secret archive', 1),
                    (31, 3, 'posting active', 0),
                    (32, 3, 'posting archive', 1)",
            [],
        )
        .context("insert threads")?;
        conn.execute(
            "INSERT INTO posts
             (id, thread_id, board_id, name, body, body_html, deletion_token, is_op)
             VALUES (101, 11, 1, 'public', 'public active', 'public active', 'a', 1),
                    (102, 12, 1, 'public', 'public archive', 'public archive', 'b', 1),
                    (201, 21, 2, 'secret', 'secret active', 'secret active', 'c', 1),
                    (202, 22, 2, 'secret', 'secret archive', 'secret archive', 'd', 1),
                    (301, 31, 3, 'posting', 'posting active', 'posting active', 'e', 1),
                    (302, 32, 3, 'posting', 'posting archive', 'posting archive', 'f', 1)",
            [],
        )
        .context("insert posts")?;
        drop(conn);
        Ok(pool)
    }

    /// Reads and deserializes one JSON entry from a snapshot ZIP.
    fn zip_json<T: serde::de::DeserializeOwned>(bytes: &[u8], name: &str) -> Result<T> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("open ZIP")?;
        let mut entry = archive.by_name(name).context("open ZIP entry")?;
        let mut json = Vec::new();
        entry.read_to_end(&mut json).context("read ZIP entry")?;
        serde_json::from_slice(&json).context("parse ZIP entry")
    }

    /// Excludes view-password boards from every gateway export scope.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally enforce the protected-board export boundary"
    )]
    fn protected_boards_are_excluded_from_every_gateway_export() -> Result<()> {
        let pool = setup_pool()?;
        let conn = pool.get().context("get database connection")?;

        let (full, _) = build_full_snapshot(&conn, None).context("build full snapshot")?;
        let boards: Vec<GwBoard> = zip_json(&full, "boards.json")?;
        let threads: Vec<GwThread> = zip_json(&full, "threads.json")?;
        let posts: Vec<GwPost> = zip_json(&full, "posts.json")?;
        assert_eq!(
            boards
                .iter()
                .map(|board| board.short_name.as_str())
                .collect::<Vec<_>>(),
            ["public", "posting"],
            "full export must contain only exportable boards"
        );
        assert!(
            threads
                .iter()
                .all(|thread| matches!(thread.board.as_str(), "public" | "posting")),
            "full export must contain only threads from exportable boards"
        );
        assert!(
            posts
                .iter()
                .all(|post| matches!(post.board.as_str(), "public" | "posting")),
            "full export must contain only posts from exportable boards"
        );

        let (force, _) =
            build_force_refresh_snapshot(&conn).context("build force-refresh snapshot")?;
        let force_threads: Vec<GwThread> = zip_json(&force, "threads.json")?;
        let force_posts: Vec<GwPost> = zip_json(&force, "posts.json")?;
        assert_eq!(force_threads.len(), 4, "force export thread count");
        assert_eq!(force_posts.len(), 4, "force export post count");
        assert!(
            force_threads
                .iter()
                .all(|thread| matches!(thread.board.as_str(), "public" | "posting")),
            "force export must exclude protected-board threads"
        );
        assert!(
            force_posts
                .iter()
                .all(|post| matches!(post.board.as_str(), "public" | "posting")),
            "force export must exclude protected-board posts"
        );

        assert!(
            build_board_snapshot(&conn, "secret", None).is_err(),
            "protected board snapshot must fail"
        );
        assert!(
            build_thread_snapshot(&conn, 21, None).is_err(),
            "protected-board thread snapshot must fail"
        );
        assert!(
            build_archive_snapshot(&conn, "secret").is_err(),
            "protected board archive snapshot must fail"
        );

        assert!(
            build_board_snapshot(&conn, "public", None).is_ok(),
            "public board snapshot must succeed"
        );
        assert!(
            build_thread_snapshot(&conn, 11, None).is_ok(),
            "public thread snapshot must succeed"
        );
        assert!(
            build_archive_snapshot(&conn, "public").is_ok(),
            "public archive snapshot must succeed"
        );
        assert!(
            build_board_snapshot(&conn, "posting", None).is_ok(),
            "post-password board snapshot must succeed"
        );
        assert!(
            build_thread_snapshot(&conn, 31, None).is_ok(),
            "post-password thread snapshot must succeed"
        );
        assert!(
            build_archive_snapshot(&conn, "posting").is_ok(),
            "post-password archive snapshot must succeed"
        );
        Ok(())
    }

    /// Excludes view-password boards from the federation export.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally enforce the protected-board federation boundary"
    )]
    fn protected_boards_are_excluded_from_federation_export() -> Result<()> {
        let pool = setup_pool()?;
        let conn = pool.get().context("get database connection")?;
        let (snapshot, _) =
            crate::chan_net::snapshot::build_snapshot(&conn).context("build snapshot")?;
        let (boards, posts, _) =
            crate::chan_net::snapshot::unpack_snapshot(&snapshot).context("unpack snapshot")?;

        assert_eq!(
            boards
                .iter()
                .map(|board| board.id.as_str())
                .collect::<Vec<_>>(),
            ["public", "posting"],
            "federation export must contain only exportable boards"
        );
        assert_eq!(posts.len(), 2, "federation export post count");
        assert!(
            posts
                .iter()
                .all(|post| matches!(post.board.as_str(), "public" | "posting")),
            "federation export must exclude protected-board posts"
        );
        Ok(())
    }
}
