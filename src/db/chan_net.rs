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
// Schema verification notes (checked against src/db/posts.rs):
//   - Post body column is `body`         (NOT `content`)
//   - Post author column is `name`        (NOT `author`)
//   - `body_html` is NOT NULL — set to plain text content for gateway-inserted posts
//   - `ip_hash` is nullable — NULL for gateway posts (no inbound IP available)
//   - `deletion_token` is NOT NULL — a fresh UUID v4 is generated per insert
//   - `created_at` has a DB-level default of unixepoch() — omitted from INSERT
//   - `is_op` is 0 for all replies

use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
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
///
/// # Errors
///
/// Returns an error if the SELECT or INSERT statement fails (e.g. DB connection
/// lost, schema mismatch).
pub fn insert_board_if_absent(conn: &Connection, short_name: &str, title: &str) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM boards WHERE short_name = ?1",
            rusqlite::params![short_name],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        return Ok(id);
    }

    // Use INSERT … RETURNING id instead of last_insert_rowid().
    // last_insert_rowid() is connection-local; in a multi-connection pool another
    // write on the same connection between the INSERT and this call would return
    // the wrong row ID.
    let id: i64 = conn.query_row(
        "INSERT INTO boards (short_name, name, description, nsfw, max_threads, bump_limit)
         VALUES (?1, ?2, '', 0, 100, 300) RETURNING id",
        rusqlite::params![short_name, title],
        |r| r.get(0),
    )?;
    Ok(id)
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
///
/// # Errors
///
/// Returns an error if the INSERT statement fails (e.g. DB connection lost or
/// a NOT NULL constraint is violated by a malformed `SnapshotPost`).
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
            post.post_id.cast_signed(),
            local_board_id,
            &post.author,
            &post.content,
            post.timestamp.cast_signed(),
        ],
    )?;
    Ok(())
}

/// Load the durable set of imported `ChanNet` transaction IDs.
///
/// # Errors
/// Returns an error if the ledger table cannot be queried.
pub fn load_import_ledger(conn: &Connection) -> Result<Vec<Uuid>> {
    let mut stmt = conn.prepare_cached("SELECT tx_id FROM chan_net_import_ledger")?;
    let tx_ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|raw| Uuid::parse_str(&raw).ok())
        .collect();
    Ok(tx_ids)
}

/// Record a successfully imported `ChanNet` transaction ID durably.
///
/// # Errors
/// Returns an error if the ledger row cannot be inserted.
pub fn record_import_tx_id(conn: &Connection, tx_id: &Uuid) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO chan_net_import_ledger (tx_id) VALUES (?1)",
        rusqlite::params![tx_id.to_string()],
    )?;
    Ok(())
}

// ── insert_reply_into_thread ──────────────────────────────────────────────────

/// Insert a reply from `RustWave` directly into the live `posts` table.
///
/// This is the ONLY write path from the `RustWave` gateway into the live forum
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
/// The `content` parameter is written to `body`, while `body_html` is generated
/// by the standard escaped render pipeline used for local posts.
/// `ip_hash` is NULL — no client IP is available for gateway posts.
/// `deletion_token` is a freshly generated UUID v4 string.
/// `is_op` is 0 — gateway posts are always replies.
/// `created_at` is set by the database default (`unixepoch()`); the `timestamp`
/// parameter from `RustWave` is informational and is not written to the posts table
/// to avoid clock-skew issues between nodes.
///
/// After a successful insert, `bump_thread` is called to increment `reply_count`
/// and advance `bumped_at`. This mirrors the behaviour of the normal post-creation
/// path in `src/db/threads.rs`.
///
/// # Errors
///
/// - Returns an error if the thread does not exist, belongs to a different board,
///   or is archived (precondition failure).
/// - Returns an error if any DB statement fails (connection lost, constraint
///   violation, or `spawn_blocking` panic).
pub fn insert_reply_into_thread(
    conn: &Connection,
    board_short_name: &str,
    thread_id: i64,
    author: &str,
    content: &str,
    _timestamp: i64,
) -> Result<i64> {
    use crate::utils::sanitize::{escape_html, render_post_body};

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
        .optional()?;

    let (_, board_id) = row.ok_or_else(|| {
        anyhow::anyhow!(
            "Thread {thread_id} on board '{board_short_name}' does not exist or is archived"
        )
    })?;

    // ── Insert into the live posts table ──────────────────────────────────────
    //
    // Only the text fields are written. No file paths, MIME types, thumbnail
    // paths, or binary data are accepted from the gateway.
    //
    // `deletion_token` is a fresh UUID v4 so that local admins can delete
    // gateway-inserted posts through the normal deletion interface.
    let deletion_token = Uuid::new_v4().to_string();
    let body_html = render_post_body(&escape_html(content), false);

    let gateway_post = crate::db::NewPost {
        thread_id,
        board_id,
        name: author.to_owned(),
        tripcode: None,
        subject: None,
        body: content.to_owned(),
        body_html,
        ip_hash: None,
        file_path: None,
        file_name: None,
        file_size: None,
        thumb_path: None,
        mime_type: None,
        media_type: None,
        audio_file_path: None,
        audio_file_name: None,
        audio_file_size: None,
        audio_mime_type: None,
        deletion_token,
        is_op: false,
    };

    crate::db::threads::create_reply_with_thread_update(conn, &gateway_post, "", true, None)
}

#[cfg(test)]
mod tests {
    use super::insert_reply_into_thread;

    fn setup_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        super::super::schema::install_or_migrate_schema(&conn).expect("install schema");
        conn.execute(
            "INSERT INTO boards (id, name, short_name, description) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![1_i64, "Test", "test", "board"],
        )
        .expect("insert board");
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived, reply_count) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![1_i64, 1_i64, "thread"],
        )
        .expect("insert thread");
        conn
    }

    #[test]
    fn gateway_replies_escape_html_and_preserve_null_ip_hash() {
        let conn = setup_conn();
        let post_id = insert_reply_into_thread(
            &conn,
            "test",
            1,
            "RustWave",
            "<script>alert(1)</script>\n&gt;quoted",
            0,
        )
        .expect("insert gateway reply");

        let (body, body_html, ip_hash): (String, String, Option<String>) = conn
            .query_row(
                "SELECT body, body_html, ip_hash FROM posts WHERE id = ?1",
                rusqlite::params![post_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("load post");

        assert_eq!(body, "<script>alert(1)</script>\n&gt;quoted");
        assert!(body_html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!body_html.contains("<script>alert(1)</script>"));
        assert_eq!(ip_hash, None);
    }
}
