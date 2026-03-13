// chan_net/selective_snapshot.rs — RustWave gateway snapshot builders.
//
// Five scoped ZIP builders for the RustWave gateway layer (Phase 7).
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
//   - Board name column:  `b.name`   (NOT `b.title`) ← Phase 8 fix
//   - Thread subject:     `t.subject` — nullable, COALESCE to ''
//   - Thread archive:     `t.archived` (INTEGER 0/1)
//
// Phase 8 fix: the boards table column is `name`, not `title`. All three
// board-fetch helpers have been corrected from `SELECT short_name, title`
// to `SELECT short_name, name`.

use std::io::{Cursor, Write};

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zip::{write::SimpleFileOptions, ZipWriter};

// ── Public structs ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwBoard {
    pub short_name: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwThread {
    pub thread_id: i64,
    pub board: String,
    pub subject: String,
    pub created_at: u64,
    pub post_count: u64,
    pub archived: bool,
}

/// SECURITY: No media fields. Text content only.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwPost {
    pub post_id: i64,
    pub thread_id: i64,
    pub board: String,
    pub author: String,
    pub content: String,
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GwMetadata {
    pub generated_at: u64,
    pub rustchan_version: String,
    pub post_count: u64,
    pub tx_id: Uuid,
    pub since: Option<u64>,
    pub is_delta: bool,
    pub includes_archive: bool,
    /// One of: `"full"` | `"board"` | `"thread"` | `"archive"` | `"force_refresh"`
    pub scope: String,
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn finish_zip(zip: ZipWriter<Cursor<Vec<u8>>>) -> Result<Vec<u8>> {
    Ok(zip.finish()?.into_inner())
}

// ── Public snapshot builders ──────────────────────────────────────────────────

/// All boards, all active (non-archived) threads, and all their posts.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned
/// (delta mode). Thread metadata is always emitted in full regardless of `since`
/// so that `RustWave` can maintain a complete thread index.
pub fn build_full_snapshot(conn: &Connection, since: Option<u64>) -> Result<(Vec<u8>, Uuid)> {
    let boards = fetch_all_boards(conn)?;
    let threads = fetch_threads(conn, None, false)?;
    let posts = fetch_posts(conn, None, None, since, false)?;

    let tx_id = Uuid::new_v4();
    let metadata = GwMetadata {
        generated_at: now_secs(),
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "full".to_string(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All active (non-archived) threads and posts for a single board.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned.
/// Returns an error if `board_short_name` does not identify a known board.
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
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "board".to_string(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All posts for a single thread.
///
/// If `since` is `Some(ts)`, only posts with `created_at > ts` are returned.
/// Returns an error if `thread_id` does not identify a known thread.
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
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        since,
        is_delta: since.is_some(),
        includes_archive: false,
        scope: "thread".to_string(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// All archived threads and their posts for a single board.
///
/// `since` is not supported for archive exports — archives are static by
/// definition once a thread is archived. Always returns the full archive.
/// Returns an error if `board_short_name` does not identify a known board.
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
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        since: None,
        is_delta: false,
        includes_archive: true,
        scope: "archive".to_string(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

/// Everything: all boards, all active threads, all archived threads, all posts.
///
/// Ignores all timestamps. Intended for initial sync and disaster recovery.
///
/// Emits a `tracing::warn!` to make force-refresh calls visible in the operator
/// log — a full database dump over the gateway is a heavyweight operation.
pub fn build_force_refresh_snapshot(conn: &Connection) -> Result<(Vec<u8>, Uuid)> {
    tracing::warn!(
        "Force refresh snapshot requested — returning full database dump including archives"
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
        rustchan_version: env!("CARGO_PKG_VERSION").to_string(),
        post_count: posts.len() as u64,
        tx_id,
        since: None,
        is_delta: false,
        includes_archive: true,
        scope: "force_refresh".to_string(),
    };

    let zip = pack_zip(&boards, &threads, &posts, &metadata)?;
    Ok((zip, tx_id))
}

// ── Private DB helpers ────────────────────────────────────────────────────────

fn board_id_by_short_name(conn: &Connection, short_name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM boards WHERE short_name = ?1",
        rusqlite::params![short_name],
        |r| r.get(0),
    )
    .map_err(|_| anyhow::anyhow!("Board '{short_name}' not found"))
}

/// Phase 8 fix: `SELECT short_name, name` — the boards table column is `name`,
/// not `title`. `GwBoard.title` is the Rust field name; it maps to the `name` SQL
/// column via positional row.get(1).
fn fetch_all_boards(conn: &Connection) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare("SELECT short_name, name FROM boards ORDER BY id")?;
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

/// Phase 8 fix: `SELECT short_name, name` — see `fetch_all_boards`.
fn fetch_boards_by_id(conn: &Connection, board_id: i64) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare("SELECT short_name, name FROM boards WHERE id = ?1")?;
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

/// Phase 8 fix: `SELECT short_name, name` — see `fetch_all_boards`.
fn fetch_boards_by_short_name(conn: &Connection, short_name: &str) -> Result<Vec<GwBoard>> {
    let mut stmt = conn.prepare("SELECT short_name, name FROM boards WHERE short_name = ?1")?;
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
             WHERE t.board_id = ?1 AND t.archived = ?2
             ORDER BY t.id"
        }
        None => {
            "SELECT t.id, b.short_name, COALESCE(t.subject, ''), t.created_at,
                    (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id), t.archived
             FROM threads t JOIN boards b ON t.board_id = b.id
             WHERE t.archived = ?1
             ORDER BY t.id"
        }
    };

    let mut stmt = conn.prepare(sql)?;

    let map_row = |r: &rusqlite::Row| {
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

fn fetch_thread_by_id(conn: &Connection, thread_id: i64) -> Result<Vec<GwThread>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, b.short_name, COALESCE(t.subject, ''), t.created_at,
                (SELECT COUNT(*) FROM posts p WHERE p.thread_id = t.id), t.archived
         FROM threads t JOIN boards b ON t.board_id = b.id
         WHERE t.id = ?1",
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
    let since_val: i64 = since.unwrap_or(0).cast_signed();

    // Fixed parameters: ?1 = archived_flag, ?2 = since_val.
    // Optional parameters appended in order: board_id (?3), thread_id (?3 or ?4).
    let mut sql = String::from(
        "SELECT p.id, p.thread_id, b.short_name,
                COALESCE(p.name, 'anon'), COALESCE(p.body, ''), p.created_at
         FROM posts p
         JOIN threads t ON p.thread_id = t.id
         JOIN boards  b ON t.board_id  = b.id
         WHERE t.archived = ?1
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

    let map_row = |r: &rusqlite::Row| {
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
