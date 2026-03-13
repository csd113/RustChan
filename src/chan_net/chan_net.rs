// db/chan_net.rs — Database helpers for the ChanNet federation and RustWave gateway layers.
//
// Three functions live here:
//
//   insert_board_if_absent    — idempotent board upsert used during federation import.
//   insert_post_if_absent     — INSERT OR IGNORE into the chan_net_posts mirror table.
//   insert_reply_into_thread  — write path from the RustWave gateway into the live posts
//                               table. Validates thread existence, board membership, and
//                               archive status before inserting. Bumps thread reply_count
//                               and bumped_at on success.
//
// Imports from crate::models::SnapshotPost. SnapshotPost lives in src/models.rs
// so that this db-layer file can import it without a layering inversion.
// chan_net::snapshot re-exports the type so all other call-sites compile unchanged.
//
// Schema verification notes (checked against src/db/posts.rs):
//   - Post body column is `body`         (NOT `content`)
//   - Post author column is `name`        (NOT `author`)
//   - `body_html` is NOT NULL — set to plain text content for gateway-inserted posts
//   - `ip_hash` is nullable — NULL for gateway posts (no inbound IP available)
//   - `deletion_token` is NOT NULL — a fresh UUID v4 is generated per insert
//   - `created_at` has a DB-level default of unixepoch() — omitted from INSERT
//   - `is_op` is 0 for all replies
//
// Phase 7 changes: insert_reply_into_thread stub replaced with full implementation.

use anyhow::Result;
use rusqlite::Connection;
use uuid::Uuid;

// SnapshotPost is defined in src/models.rs (not chan_net::snapshot) so that
// this file, which lives in the db layer, can import it without creating a
// layering inversion. chan_net::snapshot re-exports the type so that all
// other call-sites continue to compile unchanged.
use crate::models::SnapshotPost;

// ── insert_board_if_absent ────────────────────────────────────────────────────

/// Ensure a board with the given `short_name` exists in the `boards` table.
///
/// If a board with that short name already exists, returns its `id` without
/// modifying any data. If it does not exist, inserts a new board with safe
/// default values and returns the new `id`.
///
/// This is called during a federation import for every board in the incoming
/// snapshot. The "absent" check is a SELECT before INSERT so that existing
/// board metadata (name, NSFW flag, thread limits, etc.) set by the local admin
/// is never overwritten by federation data.
pub fn insert_board_if_absent(conn: &Connection, short_name: &str, title: &str) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM boards WHERE short_name = ?1",
            rusqlite::params![short_name],
            |row| row.get(0),
        )
        .ok();

    if let Some(id) = existing {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO boards (short_name, title, description, nsfw, max_threads, bump_limit)
         VALUES (?1, ?2, '', 0, 100, 300)",
        rusqlite::params![short_name, title],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── insert_post_if_absent ─────────────────────────────────────────────────────

/// Insert a remote post into the `chan_net_posts` federation mirror table.
///
/// Uses `INSERT OR IGNORE` so duplicate imports (same `remote_post_id` /
/// `board_id` pair) are silently discarded. The unique index
/// `idx_chan_net_posts_remote` provides the DB-level deduplication guarantee
/// even after a ledger reset (server restart). Posts imported here are NOT
/// inserted into the live `posts` table — they are held in the mirror table
/// and are not visible to web users browsing boards.
///
/// SECURITY: Only the five text fields defined in `SnapshotPost` are written.
/// No file paths, MIME types, thumbnail paths, or binary data are accepted.
pub fn insert_post_if_absent(
    conn: &Connection,
    post: &SnapshotPost,
    local_board_id: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO chan_net_posts
             (remote_post_id, board_id, author, content, remote_ts)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            post.post_id as i64,
            local_board_id,
            &post.author,
            &post.content,
            post.timestamp as i64,
        ],
    )?;
    Ok(())
}

// ── insert_reply_into_thread ──────────────────────────────────────────────────

/// Insert a reply from RustWave directly into the live `posts` table.
///
/// This is the ONLY write path from the RustWave gateway into the live forum
/// data. The reply becomes immediately visible to web users browsing the board.
///
/// # Preconditions (enforced inside this function)
///
/// - The thread identified by `thread_id` must exist.
/// - The thread must belong to the board identified by `board_short_name`.
/// - The thread must not be archived (`archived = 0`).
///
/// Returns the new post's row id on success, or an error if any precondition
/// is violated. No insert is attempted when a precondition fails.
///
/// # Column mapping (verified against src/db/posts.rs)
///
/// The `author` parameter is written to the `name` column.
/// The `content` parameter is written to both the `body` and `body_html` columns.
/// `body_html` is set to the plain-text content — the forum render pipeline is
/// not invoked for gateway-inserted posts, so storing plain text here is safe
/// and avoids introducing an HTML-injection risk.
/// `ip_hash` is NULL — no client IP is available for gateway posts.
/// `deletion_token` is a freshly generated UUID v4 string.
/// `is_op` is 0 — gateway posts are always replies.
/// `created_at` is set by the database default (`unixepoch()`); the `timestamp`
/// parameter from RustWave is informational and is not written to the posts table
/// to avoid clock-skew issues between nodes.
///
/// After a successful insert, `bump_thread` is called to increment `reply_count`
/// and advance `bumped_at`. This mirrors the behaviour of the normal post-creation
/// path in `src/db/threads.rs`.
pub fn insert_reply_into_thread(
    conn: &Connection,
    board_short_name: &str,
    thread_id: i64,
    author: &str,
    content: &str,
    _timestamp: i64,
) -> Result<i64> {
    // ── Precondition check ────────────────────────────────────────────────────
    //
    // Verify the thread exists, belongs to the correct board, and is not
    // archived before attempting any write. The JOIN on boards ensures that a
    // valid thread_id on the wrong board is rejected, not silently accepted.
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT t.id, t.board_id
             FROM threads t
             JOIN boards b ON t.board_id = b.id
             WHERE t.id        = ?1
               AND b.short_name = ?2
               AND t.archived   = 0",
            rusqlite::params![thread_id, board_short_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    let (_, board_id) = row.ok_or_else(|| {
        anyhow::anyhow!(
            "Thread {} on board '{}' does not exist or is archived",
            thread_id,
            board_short_name
        )
    })?;

    // ── Insert into the live posts table ──────────────────────────────────────
    //
    // Only the text fields are written. No file paths, MIME types, thumbnail
    // paths, or binary data are accepted from the gateway.
    //
    // `body_html` is set to the same value as `body`. Gateway posts bypass the
    // normal Markdown/BBCode render pipeline; storing plain text in body_html
    // is intentional and safe — the web layer will display it verbatim inside
    // the pre-escaped template helper.
    //
    // `deletion_token` is a fresh UUID v4 so that local admins can delete
    // gateway-inserted posts through the normal deletion interface.
    let deletion_token = Uuid::new_v4().to_string();

    let post_id: i64 = conn.query_row(
        "INSERT INTO posts
             (thread_id, board_id, name, body, body_html,
              ip_hash, deletion_token, is_op)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, 0)
         RETURNING id",
        rusqlite::params![
            thread_id,
            board_id,
            author,
            content,
            content, // body_html = plain text content
            deletion_token,
        ],
        |row| row.get(0),
    )?;

    // ── Bump the thread ───────────────────────────────────────────────────────
    //
    // Mirror the normal post-creation path: advance bumped_at and increment
    // reply_count. This call is not co-transactional with the INSERT above
    // (same documented limitation as the main post-creation path in threads.rs
    // MED-6). A crash between the two statements leaves reply_count one behind,
    // which is an advisory counter — not a data integrity failure.
    conn.execute(
        "UPDATE threads
         SET bumped_at    = unixepoch(),
             reply_count  = reply_count + 1
         WHERE id = ?1",
        rusqlite::params![thread_id],
    )?;

    Ok(post_id)
}
