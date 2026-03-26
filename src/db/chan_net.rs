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
//   - Post author column is `name`       (NOT `author`)
//   - `body_html` is NOT NULL
//   - `ip_hash` is nullable — NULL for gateway posts (no inbound IP available)
//   - `deletion_token` is NOT NULL — a fresh UUID v4 is generated per insert
//   - `created_at` has a DB-level default of unixepoch() — omitted from INSERT
//   - `is_op` is 0 for all replies

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, Error as SqlError, OptionalExtension};
use uuid::Uuid;

// SnapshotPost is defined in src/models.rs (not chan_net::snapshot) so that
// this file, which lives in the db layer, can import it without creating a
// layering inversion. chan_net::snapshot re-exports the type so that all
// other call-sites continue to compile unchanged.
use crate::models::SnapshotPost;

// Conservative payload limits for gateway/federation text fields.
// These avoid accidental DB bloat and keep this path aligned with normal posting
// expectations without changing core functionality.
const MAX_BOARD_SHORT_NAME_LEN: usize = 64;
const MAX_BOARD_TITLE_LEN: usize = 256;
const MAX_AUTHOR_LEN: usize = 128;
const MAX_CONTENT_LEN: usize = 64 * 1024;

fn validate_non_empty_trimmed(value: &str, field_name: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field_name} must not be empty");
    }
    Ok(())
}

fn validate_max_len(value: &str, max_len: usize, field_name: &str) -> Result<()> {
    if value.chars().count() > max_len {
        bail!("{field_name} exceeds maximum length of {max_len} characters");
    }
    Ok(())
}

fn validate_board_inputs(short_name: &str, title: &str) -> Result<()> {
    validate_non_empty_trimmed(short_name, "short_name")?;
    validate_non_empty_trimmed(title, "title")?;
    validate_max_len(short_name, MAX_BOARD_SHORT_NAME_LEN, "short_name")?;
    validate_max_len(title, MAX_BOARD_TITLE_LEN, "title")?;
    Ok(())
}

fn validate_reply_inputs(author: &str, content: &str) -> Result<()> {
    validate_non_empty_trimmed(author, "author")?;
    validate_non_empty_trimmed(content, "content")?;
    validate_max_len(author, MAX_AUTHOR_LEN, "author")?;
    validate_max_len(content, MAX_CONTENT_LEN, "content")?;
    Ok(())
}

fn validate_snapshot_post(post: &SnapshotPost) -> Result<()> {
    validate_non_empty_trimmed(&post.author, "post.author")?;
    validate_non_empty_trimmed(&post.content, "post.content")?;
    validate_max_len(&post.author, MAX_AUTHOR_LEN, "post.author")?;
    validate_max_len(&post.content, MAX_CONTENT_LEN, "post.content")?;
    Ok(())
}

fn to_i64_checked<T>(value: T, field_name: &str) -> Result<i64>
where
    i64: TryFrom<T>,
    <i64 as TryFrom<T>>::Error: std::fmt::Display,
{
    i64::try_from(value).map_err(|err| anyhow!("{field_name} is out of range for i64: {err}"))
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());

    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#x27;"),
            _ => escaped.push(ch),
        }
    }

    escaped
}

// ── insert_board_if_absent ────────────────────────────────────────────────────

/// Ensure a board with the given `short_name` exists in the `boards` table.
///
/// If a board with that short name already exists, returns its `id` without
/// modifying any data. If it does not exist, inserts a new board with safe
/// default values and returns the new `id`.
///
/// This is called during a federation import for every board in the incoming
/// snapshot. Existing board metadata (name, NSFW flag, thread limits, etc.)
/// set by the local admin is never overwritten by federation data.
///
/// # Errors
///
/// Returns an error if validation fails or if any SQL statement fails
/// (e.g. DB connection lost, schema mismatch).
pub fn insert_board_if_absent(conn: &Connection, short_name: &str, title: &str) -> Result<i64> {
    validate_board_inputs(short_name, title)?;

    conn.execute(
        "INSERT OR IGNORE INTO boards
             (short_name, title, description, nsfw, max_threads, bump_limit)
         VALUES (?1, ?2, '', 0, 100, 300)",
        params![short_name, title],
    )?;

    let id = conn
        .query_row(
            "SELECT id FROM boards WHERE short_name = ?1",
            params![short_name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| {
            anyhow!("board lookup failed after insert/select for short_name '{short_name}'")
        })?;

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
/// Returns an error if validation fails, integer conversion fails, or if the
/// INSERT statement fails (e.g. DB connection lost or a NOT NULL constraint is
/// violated by a malformed `SnapshotPost`).
pub fn insert_post_if_absent(
    conn: &Connection,
    post: &SnapshotPost,
    local_board_id: i64,
) -> Result<()> {
    validate_snapshot_post(post)?;

    let remote_post_id = to_i64_checked(post.post_id, "post.post_id")?;
    let remote_ts = to_i64_checked(post.timestamp, "post.timestamp")?;

    let _rows_affected = conn.execute(
        "INSERT OR IGNORE INTO chan_net_posts
             (remote_post_id, board_id, author, content, remote_ts)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            remote_post_id,
            local_board_id,
            &post.author,
            &post.content,
            remote_ts,
        ],
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
/// The `content` parameter is written to both the `body` and `body_html` columns.
/// `body_html` is stored as escaped plain text content. Gateway posts bypass the
/// normal render pipeline; pre-escaping avoids treating this field as trusted
/// HTML if future readers ever render it directly.
/// `ip_hash` is NULL — no client IP is available for gateway posts.
/// `deletion_token` is a freshly generated UUID v4 string.
/// `is_op` is 0 — gateway posts are always replies.
/// `created_at` is set by the database default (`unixepoch()`); the `timestamp`
/// parameter from `RustWave` is informational and is not written to the posts table
/// to avoid clock-skew issues between nodes.
///
/// After a successful insert, the thread row is updated to increment `reply_count`
/// and advance `bumped_at`.
///
/// # Errors
///
/// - Returns an error if the thread does not exist, belongs to a different board,
///   or is archived.
/// - Returns an error if validation fails.
/// - Returns an error if any DB statement fails.
pub fn insert_reply_into_thread(
    conn: &Connection,
    board_short_name: &str,
    thread_id: i64,
    author: &str,
    content: &str,
    _timestamp: i64,
) -> Result<i64> {
    validate_non_empty_trimmed(board_short_name, "board_short_name")?;
    validate_max_len(
        board_short_name,
        MAX_BOARD_SHORT_NAME_LEN,
        "board_short_name",
    )?;
    validate_reply_inputs(author, content)?;

    let tx = conn.unchecked_transaction()?;

    // Store escaped plain text in body_html to avoid future accidental raw-HTML
    // rendering. This preserves the existing "plain text, no render pipeline"
    // behavior while hardening the storage invariant.
    let body_html = escape_html(content);
    let deletion_token = Uuid::new_v4().to_string();

    let post_id = match tx.query_row(
        "INSERT INTO posts
             (thread_id, board_id, name, body, body_html,
              ip_hash, deletion_token, is_op)
         SELECT
             t.id,
             t.board_id,
             ?3,
             ?4,
             ?5,
             NULL,
             ?6,
             0
         FROM threads t
         JOIN boards b ON t.board_id = b.id
         WHERE t.id = ?1
           AND b.short_name = ?2
           AND t.archived = 0
         RETURNING id",
        params![
            thread_id,
            board_short_name,
            author,
            content,
            body_html,
            deletion_token,
        ],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(id) => id,
        Err(SqlError::QueryReturnedNoRows) => {
            let thread_state = tx
                .query_row(
                    "SELECT t.archived, b.short_name
                     FROM threads t
                     JOIN boards b ON t.board_id = b.id
                     WHERE t.id = ?1",
                    params![thread_id],
                    |row| {
                        let archived = row.get::<_, i64>(0)?;
                        let actual_board = row.get::<_, String>(1)?;
                        Ok((archived, actual_board))
                    },
                )
                .optional()?;

            match thread_state {
                None => bail!("thread {thread_id} does not exist"),
                Some((_, actual_board)) if actual_board != board_short_name => {
                    bail!(
                        "thread {thread_id} belongs to board '{actual_board}', not '{board_short_name}'"
                    )
                }
                Some((archived, _)) if archived != 0 => {
                    bail!("thread {thread_id} is archived")
                }
                Some((_archived, _actual_board)) => {
                    bail!("reply insert precondition failed for thread {thread_id}")
                }
            }
        }
        Err(err) => return Err(err.into()),
    };

    let rows_updated = tx.execute(
        "UPDATE threads
         SET bumped_at   = unixepoch(),
             reply_count = reply_count + 1
         WHERE id = ?1
           AND archived = 0",
        params![thread_id],
    )?;

    if rows_updated != 1 {
        bail!("thread {thread_id} could not be bumped after reply insert");
    }

    tx.commit()?;
    Ok(post_id)
}
