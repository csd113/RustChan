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
use sha2::{Digest as _, Sha256};
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

/// Domain separator for deterministic legacy reply replay tokens.
const REPLY_REPLAY_DOMAIN: &[u8] = b"rustchan-channet-reply-v1\0";
/// Prefix distinguishing caller-provided message identifiers.
const REPLY_MESSAGE_ID_PREFIX: &str = "channet-reply-id-v1:";

#[derive(Debug, thiserror::Error)]
#[error("ChanNet reply request was already processed")]
/// Error returned when a federated reply has already been persisted.
pub struct ReplyReplayError;

/// Add one length-delimited field to a reply replay-token hash.
fn hash_reply_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(bytes.len().to_be_bytes());
    hasher.update(bytes);
}

/// Build the replay token for a federated reply request.
fn reply_replay_token(
    board_short_name: &str,
    thread_id: i64,
    author: &str,
    content: &str,
    timestamp: i64,
    message_id: Option<&Uuid>,
) -> String {
    if let Some(message_id) = message_id {
        return format!("{REPLY_MESSAGE_ID_PREFIX}{message_id}");
    }

    let mut hasher = Sha256::new();
    hasher.update(REPLY_REPLAY_DOMAIN);
    hash_reply_field(&mut hasher, board_short_name.as_bytes());
    hasher.update(thread_id.to_be_bytes());
    hash_reply_field(&mut hasher, author.as_bytes());
    hash_reply_field(&mut hasher, content.as_bytes());
    hasher.update(timestamp.to_be_bytes());
    format!("channet-reply-v1:{}", hex::encode(hasher.finalize()))
}

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
/// `message_id`, when supplied, is the stable remote idempotency key. Legacy
/// callers that omit it retain the content-and-timestamp fingerprint so
/// existing integrations continue to work during migration.
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
    timestamp: i64,
    message_id: Option<&Uuid>,
) -> Result<i64> {
    use crate::utils::sanitize::{escape_html, render_post_body};

    let replay_token = reply_replay_token(
        board_short_name,
        thread_id,
        author,
        content,
        timestamp,
        message_id,
    );
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(anyhow::Error::from)?;

    let result: Result<i64> = (|| {
        // Verify the target and its current mutable state while holding the
        // same write transaction used for replay registration and insertion.
        let row: Option<(i64, bool, bool)> = conn
            .query_row(
                "SELECT t.board_id, t.locked, t.archived
                 FROM threads t
                 JOIN boards b ON t.board_id = b.id
                 WHERE t.id = ?1
                   AND b.short_name = ?2
                   AND b.access_mode IN ('public', 'post_password')",
                rusqlite::params![thread_id, board_short_name],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i32>(1)? != 0,
                        row.get::<_, i32>(2)? != 0,
                    ))
                },
            )
            .optional()?;
        let Some((board_id, locked, archived)) = row else {
            anyhow::bail!(
                "Thread {thread_id} on board '{board_short_name}' does not exist or is not exportable"
            );
        };
        if locked {
            anyhow::bail!("This thread is locked.");
        }
        if archived {
            anyhow::bail!("This thread is archived.");
        }

        let registered = conn.execute(
            "INSERT OR IGNORE INTO chan_net_import_ledger (tx_id) VALUES (?1)",
            rusqlite::params![replay_token],
        )?;
        if registered == 0 {
            return Err(ReplyReplayError.into());
        }

        // Only text fields are written. The replay ledger and post mutation
        // commit atomically, so neither a retry nor a crash can duplicate the
        // reply or register a request whose post was not inserted.
        let gateway_post = crate::db::NewPost {
            thread_id,
            board_id,
            name: author.to_owned(),
            tripcode: None,
            subject: None,
            body: content.to_owned(),
            body_html: render_post_body(&escape_html(content), false),
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
            deletion_token: Uuid::new_v4().to_string(),
            is_op: false,
        };
        let post_id = super::posts::create_post_inner(conn, &gateway_post)?;
        let updated = conn.execute(
            "UPDATE threads
             SET bumped_at = unixepoch(), reply_count = reply_count + 1
             WHERE id = ?1 AND board_id = ?2 AND locked = 0 AND archived = 0",
            rusqlite::params![thread_id, board_id],
        )?;
        if updated == 0 {
            anyhow::bail!("Thread id {thread_id} changed state while creating reply");
        }
        Ok(post_id)
    })();

    match result {
        Ok(post_id) => {
            let commit_result = conn.execute_batch("COMMIT");
            match commit_result {
                Ok(()) => Ok(post_id),
                Err(error) => {
                    drop(conn.execute_batch("ROLLBACK"));
                    Err(anyhow::Error::from(error))
                }
            }
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{insert_reply_into_thread, ReplyReplayError};
    use anyhow::{Context as _, Result};

    fn setup_conn() -> Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open_in_memory()?;
        super::super::schema::install_or_migrate_schema(&conn)?;
        conn.execute(
            "INSERT INTO boards (id, name, short_name, description) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![1_i64, "Test", "test", "board"],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived, reply_count) VALUES (?1, ?2, ?3, 0, 0)",
            rusqlite::params![1_i64, 1_i64, "thread"],
        )?;
        Ok(conn)
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn gateway_replies_escape_html_and_preserve_null_ip_hash() -> Result<()> {
        let conn = setup_conn()?;
        let post_id = insert_reply_into_thread(
            &conn,
            "test",
            1,
            "RustWave",
            "<script>alert(1)</script>\n&gt;quoted",
            0,
            None,
        )?;

        let (body, body_html, ip_hash): (String, String, Option<String>) = conn.query_row(
            "SELECT body, body_html, ip_hash FROM posts WHERE id = ?1",
            rusqlite::params![post_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        assert_eq!(
            body, "<script>alert(1)</script>\n&gt;quoted",
            "plain body should be preserved"
        );
        assert!(
            body_html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "rendered body should escape script tags"
        );
        assert!(
            !body_html.contains("<script>alert(1)</script>"),
            "rendered body must not contain executable script markup"
        );
        assert_eq!(
            ip_hash, None,
            "gateway replies should not fabricate a source IP hash"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn replayed_gateway_reply_is_rejected_without_duplicate_insert() -> Result<()> {
        let conn = setup_conn()?;
        insert_reply_into_thread(&conn, "test", 1, "RustWave", "same request", 123, None)?;

        let error =
            insert_reply_into_thread(&conn, "test", 1, "RustWave", "same request", 123, None)
                .err()
                .context("replay must be rejected")?;
        assert!(
            error.downcast_ref::<ReplyReplayError>().is_some(),
            "replay should return the typed replay error"
        );

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = 1 AND body = 'same request'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "replay should not create a duplicate row");

        insert_reply_into_thread(&conn, "test", 1, "RustWave", "same request", 124, None)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = 1 AND body = 'same request'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            count, 2,
            "a nearby request with a different timestamp should be distinct"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn stable_message_ids_distinguish_identical_replies_and_reject_id_reuse() -> Result<()> {
        let conn = setup_conn()?;
        let first_id = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001")?;
        let second_id = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000002")?;

        insert_reply_into_thread(
            &conn,
            "test",
            1,
            "RustWave",
            "identical legitimate reply",
            123,
            Some(&first_id),
        )?;
        insert_reply_into_thread(
            &conn,
            "test",
            1,
            "RustWave",
            "identical legitimate reply",
            123,
            Some(&second_id),
        )?;

        let replay = insert_reply_into_thread(
            &conn,
            "test",
            1,
            "different author",
            "different body",
            999,
            Some(&first_id),
        )
        .err()
        .context("reused stable message id must be rejected")?;
        assert!(
            replay.downcast_ref::<ReplyReplayError>().is_some(),
            "message-ID reuse should return the typed replay error"
        );

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = 1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            count, 2,
            "distinct stable message IDs should preserve identical replies"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn gateway_reply_rejects_non_exportable_board_before_any_write() -> Result<()> {
        let conn = setup_conn()?;
        conn.execute(
            "INSERT INTO boards
             (id, name, short_name, description, access_mode, access_password_hash)
             VALUES (2, 'Protected', 'protected', '', 'view_password', 'hash')",
            [],
        )?;
        conn.execute(
            "INSERT INTO threads (id, board_id, subject, archived, reply_count)
             VALUES (2, 2, 'protected thread', 0, 0)",
            [],
        )?;

        let error =
            insert_reply_into_thread(&conn, "protected", 2, "RustWave", "secret reply", 123, None)
                .err()
                .context("protected board reply must be rejected")?;
        assert!(
            error.to_string().contains("not exportable"),
            "the rejection should identify board exportability"
        );

        let post_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = 2",
            [],
            |row| row.get(0),
        )?;
        let replay_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chan_net_import_ledger
                 WHERE tx_id LIKE 'channet-reply-v1:%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            post_count, 0,
            "protected-board rejection should insert no post"
        );
        assert_eq!(
            replay_count, 0,
            "protected-board rejection should insert no replay token"
        );
        Ok(())
    }
}
