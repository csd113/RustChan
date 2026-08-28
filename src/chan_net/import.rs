//! Federation import handler.
//
// POST /chan/import receives a raw snapshot ZIP body, performs deduplication
// via the in-memory TxLedger, validates the payload schema, writes boards and
// posts to the `chan_net_posts` mirror table, then records the tx_id in the
// ledger.
//
// `do_import()` is also called by `poll.rs` when draining the RustWave
// broadcast queue, so it is visible to the parent module and accepts pre-read
// `bytes::Bytes` plus the authenticated handler's processing guard rather than
// reading from an Axum extractor internally.
//
// Order of operations inside do_import (MUST NOT be changed without updating
// the security hardening checklist in channet_build_plan.md § 6.3):
//
//   1. Unpack and parse the ZIP (rejects unknown filenames — path traversal guard)
//   2. Ed25519 signature check — log-and-skip if signature is present (not yet verified)
//   3. Check TxLedger — reject duplicate tx_ids BEFORE any DB write
//   4. Parse all untrusted identifiers and bounded text into validated values
//   5. Atomically claim tx_id and write boards/posts in one DB transaction
//   6. Record tx_id in the in-memory ledger after the transaction commits

use anyhow::Context as _;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use tokio_util::bytes;

use super::snapshot::{unpack_snapshot, SnapshotBoard, SnapshotMetadata, SnapshotPost};
use crate::{
    error::AppError,
    middleware::{AppState, ChanImportGuard},
};

/// Maximum display-name characters accepted for an imported board.
const SNAPSHOT_BOARD_TITLE_MAX_CHARS: usize = 64;
/// Maximum author characters accepted for an imported post.
const SNAPSHOT_POST_AUTHOR_MAX_CHARS: usize = 255;
/// Maximum content characters accepted for an imported post.
const SNAPSHOT_POST_CONTENT_MAX_CHARS: usize = 32_768;

/// Board data that has passed the local identifier and text policy.
struct ValidatedSnapshotBoard {
    /// Lowercase ASCII board slug safe for database and path-adjacent use.
    id: String,
    /// Bounded human-readable title.
    title: String,
}

/// Post data whose identifiers fit `SQLite`'s signed integer representation.
struct ValidatedSnapshotPost {
    /// Remote post identifier parsed without wrapping.
    remote_post_id: i64,
    /// Validated board slug referenced by this post.
    board: String,
    /// Bounded display name.
    author: String,
    /// Bounded plain-text content.
    content: String,
    /// Remote Unix timestamp parsed without wrapping.
    remote_timestamp: i64,
}

/// Fully validated snapshot ready for one atomic database transaction.
struct ValidatedSnapshot {
    /// Unique boards declared by the snapshot.
    boards: Vec<ValidatedSnapshotBoard>,
    /// Posts whose board references and integer values were checked.
    posts: Vec<ValidatedSnapshotPost>,
}

/// Constructs the fail-closed error used when the import ledger is unavailable.
fn chan_ledger_not_initialised() -> AppError {
    AppError::Internal(anyhow::anyhow!("ChanNet ledger not initialised"))
}

/// Returns whether a snapshot board identifier matches the local slug policy.
fn is_valid_snapshot_board_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 8
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// Parses untrusted snapshot values into the types accepted by persistence.
fn validate_snapshot(
    boards: Vec<SnapshotBoard>,
    posts: Vec<SnapshotPost>,
    metadata: &SnapshotMetadata,
) -> Result<ValidatedSnapshot, AppError> {
    let expected_post_count = u64::try_from(posts.len()).map_err(|error| {
        AppError::BadRequest(format!(
            "Snapshot post count cannot be represented: {error}"
        ))
    })?;
    if metadata.post_count != expected_post_count {
        return Err(AppError::BadRequest(format!(
            "Snapshot metadata declares {} posts, but posts.json contains {expected_post_count}",
            metadata.post_count
        )));
    }

    let mut board_ids = HashSet::with_capacity(boards.len());
    let mut validated_boards = Vec::with_capacity(boards.len());
    for board in boards {
        if !is_valid_snapshot_board_id(&board.id) {
            return Err(AppError::BadRequest(
                "Snapshot board id must be 1-8 lowercase ASCII letters or digits".into(),
            ));
        }
        if board.title.chars().count() > SNAPSHOT_BOARD_TITLE_MAX_CHARS {
            return Err(AppError::BadRequest(format!(
                "Snapshot board {} title exceeds the {SNAPSHOT_BOARD_TITLE_MAX_CHARS}-character limit",
                board.id
            )));
        }
        if !board_ids.insert(board.id.clone()) {
            return Err(AppError::BadRequest(format!(
                "Snapshot declares duplicate board {}",
                board.id
            )));
        }
        validated_boards.push(ValidatedSnapshotBoard {
            id: board.id,
            title: board.title,
        });
    }

    let mut validated_posts = Vec::with_capacity(posts.len());
    for post in posts {
        if !is_valid_snapshot_board_id(&post.board) {
            return Err(AppError::BadRequest(format!(
                "Post {} board must be 1-8 lowercase ASCII letters or digits",
                post.post_id
            )));
        }
        if !board_ids.contains(&post.board) {
            return Err(AppError::BadRequest(format!(
                "Post {} references an undeclared board",
                post.post_id
            )));
        }
        if post.content.chars().count() > SNAPSHOT_POST_CONTENT_MAX_CHARS {
            return Err(AppError::BadRequest(format!(
                "Post {} content exceeds the {SNAPSHOT_POST_CONTENT_MAX_CHARS}-character limit",
                post.post_id
            )));
        }
        if post.author.chars().count() > SNAPSHOT_POST_AUTHOR_MAX_CHARS {
            return Err(AppError::BadRequest(format!(
                "Post {} author exceeds the {SNAPSHOT_POST_AUTHOR_MAX_CHARS}-character limit",
                post.post_id
            )));
        }
        let remote_post_id = i64::try_from(post.post_id).map_err(|error| {
            AppError::BadRequest(format!(
                "Post {} id exceeds SQLite's signed integer range: {error}",
                post.post_id
            ))
        })?;
        let remote_timestamp = i64::try_from(post.timestamp).map_err(|error| {
            AppError::BadRequest(format!(
                "Post {} timestamp exceeds SQLite's signed integer range: {error}",
                post.post_id
            ))
        })?;
        validated_posts.push(ValidatedSnapshotPost {
            remote_post_id,
            board: post.board,
            author: post.author,
            content: post.content,
            remote_timestamp,
        });
    }

    Ok(ValidatedSnapshot {
        boards: validated_boards,
        posts: validated_posts,
    })
}

/// Claims and persists one validated snapshot in an atomic `SQLite` transaction.
fn commit_snapshot(
    conn: &mut rusqlite::Connection,
    snapshot: ValidatedSnapshot,
    tx_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let transaction = conn
        .transaction()
        .context("begin ChanNet snapshot transaction")?;
    crate::db::chan_net::claim_import_tx_id(&transaction, &tx_id)
        .context("claim ChanNet snapshot transaction id")?;

    let mut local_board_ids = HashMap::with_capacity(snapshot.boards.len());
    for board in snapshot.boards {
        let local_id =
            crate::db::chan_net::insert_board_if_absent(&transaction, &board.id, &board.title)
                .with_context(|| format!("persist imported board {}", board.id))?;
        local_board_ids.insert(board.id, local_id);
    }

    for post in snapshot.posts {
        let local_board_id = local_board_ids.get(&post.board).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "validated post {} lost its board mapping",
                post.remote_post_id
            )
        })?;
        crate::db::chan_net::insert_post_if_absent(
            &transaction,
            post.remote_post_id,
            local_board_id,
            &post.author,
            &post.content,
            post.remote_timestamp,
        )
        .with_context(|| format!("persist imported post {}", post.remote_post_id))?;
    }

    transaction
        .commit()
        .context("commit ChanNet snapshot transaction")
}

// ── do_import ─────────────────────────────────────────────────────────────────

/// Core import logic shared by `chan_import` (POST /chan/import) and
/// `chan_poll` (which drains the `RustWave` broadcast queue).
///
/// Returns the number of posts in the snapshot on success.
///
/// # Errors
///
/// - `AppError::BadRequest`  — ZIP is malformed, contains unexpected files,
///   or a post fails schema validation.
/// - `AppError::Conflict`    — the `tx_id` in `metadata` has already been
///   imported (duplicate snapshot).
/// - `AppError::Internal`    — DB connection failure or `spawn_blocking` panic.
pub(super) async fn do_import(
    state: &AppState,
    bytes: bytes::Bytes,
    processing_guard: &ChanImportGuard,
) -> Result<usize, AppError> {
    // ── 1. Unpack ────────────────────────────────────────────────────────────
    let unpack_guard = processing_guard.clone();
    let (boards, posts, metadata) = tokio::task::spawn_blocking(move || {
        let _processing_guard = unpack_guard;
        unpack_snapshot(&bytes)
    })
    .await
    .map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "ChanNet snapshot unpack task failed: {error}"
        ))
    })?
    .map_err(|error| AppError::BadRequest(error.to_string()))?;

    // ── 2. Ed25519 signature check ───────────────────────────────────────────
    // Verification is not yet implemented. Reject any signed snapshot rather
    // than silently accepting unverified data. A signed snapshot without
    // verification offers zero authenticity guarantee and exposes the
    // chan_net_posts table to arbitrary data injection.
    //
    // This guard must be removed only when Phase N (Ed25519 verification) is
    // fully implemented and tested (see channet_build_plan.md § 6.3).
    if metadata.signature.is_some() {
        return Err(AppError::BadRequest(
            "Ed25519 signature verification is not yet implemented.              Signed snapshots are rejected until Phase N is complete.".into(),
        ));
    }

    // ── 3. Ledger check — must happen BEFORE any DB write ───────────────────
    {
        let ledger_arc = state
            .chan_ledger
            .as_ref()
            .ok_or_else(chan_ledger_not_initialised)?;

        // parking_lot::Mutex::lock() never poisons — no unwrap needed.
        let ledger = ledger_arc.lock();
        if ledger.contains(&metadata.tx_id) {
            return Err(AppError::Conflict("Snapshot already imported".into()));
        }
    } // ledger guard released here

    // ── 4. Schema validation — before any DB write ───────────────────────────
    let validated = validate_snapshot(boards, posts, &metadata)?;
    let post_count = validated.posts.len();
    let tx_id = metadata.tx_id;

    // ── 5. Atomic DB transaction — all in one spawn_blocking ────────────────
    let mut conn = state.db.get()?;

    let commit_guard = processing_guard.clone();
    let commit_result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _processing_guard = commit_guard;
        commit_snapshot(&mut conn, validated, tx_id)
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!("ChanNet import task failed: {error}")))?;
    if let Err(error) = commit_result {
        if error
            .downcast_ref::<crate::db::chan_net::SnapshotImportReplayError>()
            .is_some()
        {
            return Err(AppError::Conflict("Snapshot already imported".into()));
        }
        return Err(AppError::from(error));
    }

    // ── 6. Record tx_id in ledger after confirmed successful write ───────────
    {
        let ledger_arc = state
            .chan_ledger
            .as_ref()
            .ok_or_else(chan_ledger_not_initialised)?;

        ledger_arc.lock().insert(tx_id);
    }

    Ok(post_count)
}

// ── chan_import ───────────────────────────────────────────────────────────────

/// POST /chan/import — receives a federation snapshot ZIP as raw bytes.
///
/// Returns `{"imported": N}` on success, where N is the number of posts in
/// the received snapshot (not necessarily the number actually written — posts
/// that already exist in `chan_net_posts` are silently skipped by
/// INSERT OR IGNORE).
///
/// The request body limit is enforced by `DefaultBodyLimit::max(CONFIG.chan_net_max_body)`
/// applied in `chan_router()`. This handler never reads more than that limit.
///
/// # Errors
///
/// Returns a [`super::ChanError`] when the snapshot fails validation, was
/// already imported, or cannot be committed to the database.
pub async fn chan_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, super::ChanError> {
    let processing_guard = state.chan_import_gate.try_begin()?;
    let imported = do_import(&state, body, &processing_guard).await?;
    drop(processing_guard);
    Ok((StatusCode::OK, Json(json!({ "imported": imported }))))
}

#[cfg(test)]
mod tests {
    use super::{commit_snapshot, validate_snapshot};
    use crate::chan_net::snapshot::{SnapshotBoard, SnapshotMetadata, SnapshotPost};
    use anyhow::{Context as _, Result};

    /// Creates deterministic metadata for validation and transaction tests.
    fn metadata(tx_id: uuid::Uuid, post_count: u64) -> SnapshotMetadata {
        SnapshotMetadata {
            generated_at: 1,
            rustchan_version: "test".to_owned(),
            post_count,
            tx_id,
            signature: None,
            since: None,
            is_delta: false,
            includes_archive: false,
        }
    }

    /// Creates one valid board and two posts for transaction tests.
    fn atomic_snapshot_parts() -> (Vec<SnapshotBoard>, Vec<SnapshotPost>) {
        (
            vec![SnapshotBoard {
                id: "atomic".to_owned(),
                title: "Atomic".to_owned(),
            }],
            vec![
                SnapshotPost {
                    post_id: 1,
                    board: "atomic".to_owned(),
                    author: "one".to_owned(),
                    content: "first".to_owned(),
                    timestamp: 1,
                },
                SnapshotPost {
                    post_id: 2,
                    board: "atomic".to_owned(),
                    author: "two".to_owned(),
                    content: "second".to_owned(),
                    timestamp: 2,
                },
            ],
        )
    }

    #[test]
    fn snapshot_validation_rejects_path_hostile_board_and_wrapping_integer() -> Result<()> {
        let invalid_board = vec![SnapshotBoard {
            id: "../admin".to_owned(),
            title: "Invalid".to_owned(),
        }];
        let Err(crate::error::AppError::BadRequest(board_error)) =
            validate_snapshot(invalid_board, Vec::new(), &metadata(uuid::Uuid::nil(), 0))
        else {
            anyhow::bail!("path-hostile snapshot board id was accepted");
        };
        anyhow::ensure!(board_error.contains("lowercase ASCII"));

        let boards = vec![SnapshotBoard {
            id: "safe".to_owned(),
            title: "Safe".to_owned(),
        }];
        let posts = vec![SnapshotPost {
            post_id: u64::MAX,
            board: "safe".to_owned(),
            author: "author".to_owned(),
            content: "content".to_owned(),
            timestamp: 1,
        }];
        let Err(crate::error::AppError::BadRequest(integer_error)) =
            validate_snapshot(boards, posts, &metadata(uuid::Uuid::nil(), 1))
        else {
            anyhow::bail!("wrapping snapshot post id was accepted");
        };
        anyhow::ensure!(integer_error.contains("signed integer range"));
        Ok(())
    }

    #[test]
    fn snapshot_validation_rejects_undeclared_board_reference() -> Result<()> {
        let posts = vec![SnapshotPost {
            post_id: 1,
            board: "missing".to_owned(),
            author: "author".to_owned(),
            content: "content".to_owned(),
            timestamp: 1,
        }];

        let Err(crate::error::AppError::BadRequest(error)) =
            validate_snapshot(Vec::new(), posts, &metadata(uuid::Uuid::nil(), 1))
        else {
            anyhow::bail!("post referencing an undeclared board was accepted");
        };

        anyhow::ensure!(error.contains("undeclared board"));
        Ok(())
    }

    #[test]
    fn snapshot_validation_does_not_reflect_oversized_identifier() -> Result<()> {
        let oversized_identifier = "x".repeat(1024 * 1024);
        let boards = vec![SnapshotBoard {
            id: "safe".to_owned(),
            title: "Safe".to_owned(),
        }];
        let posts = vec![SnapshotPost {
            post_id: 1,
            board: oversized_identifier.clone(),
            author: "author".to_owned(),
            content: "content".to_owned(),
            timestamp: 1,
        }];

        let Err(crate::error::AppError::BadRequest(error)) =
            validate_snapshot(boards, posts, &metadata(uuid::Uuid::nil(), 1))
        else {
            anyhow::bail!("oversized snapshot board identifier was accepted");
        };

        anyhow::ensure!(
            error.len() < 256,
            "validation error reflected oversized input"
        );
        anyhow::ensure!(
            !error.contains(&oversized_identifier),
            "validation error included the untrusted identifier"
        );
        Ok(())
    }

    #[test]
    fn snapshot_commit_rolls_back_content_and_claim_on_mid_import_failure() -> Result<()> {
        let pool = crate::db::init_test_pool().context("create ChanNet transaction test pool")?;
        let mut conn = pool
            .get()
            .context("get ChanNet transaction test connection")?;
        conn.execute_batch(
            "CREATE TRIGGER fail_second_imported_post
             BEFORE INSERT ON chan_net_posts
             WHEN NEW.remote_post_id = 2
             BEGIN
                 SELECT RAISE(ABORT, 'forced ChanNet import failure');
             END;",
        )
        .context("install forced ChanNet import failure trigger")?;
        let tx_id = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000123")?;
        let (boards, posts) = atomic_snapshot_parts();
        let snapshot = validate_snapshot(boards, posts, &metadata(tx_id, 2))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let failed = commit_snapshot(&mut conn, snapshot, tx_id);

        anyhow::ensure!(
            failed.is_err(),
            "forced mid-import failure unexpectedly committed"
        );
        let board_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM boards WHERE short_name = 'atomic'",
            [],
            |row| row.get(0),
        )?;
        let post_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM chan_net_posts", [], |row| row.get(0))?;
        let claim_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM chan_net_import_ledger WHERE tx_id = ?1",
            rusqlite::params![tx_id.to_string()],
            |row| row.get(0),
        )?;
        anyhow::ensure!(board_count == 0, "failed import left a partial board");
        anyhow::ensure!(post_count == 0, "failed import left partial posts");
        anyhow::ensure!(claim_count == 0, "failed import left a replay claim");

        conn.execute_batch("DROP TRIGGER fail_second_imported_post")?;
        let (boards, posts) = atomic_snapshot_parts();
        let retry = validate_snapshot(boards, posts, &metadata(tx_id, 2))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        commit_snapshot(&mut conn, retry, tx_id)?;
        let committed_posts: i64 =
            conn.query_row("SELECT COUNT(*) FROM chan_net_posts", [], |row| row.get(0))?;
        anyhow::ensure!(
            committed_posts == 2,
            "retry did not commit both posts atomically"
        );

        let (boards, posts) = atomic_snapshot_parts();
        let replay = validate_snapshot(boards, posts, &metadata(tx_id, 2))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let Err(replay_error) = commit_snapshot(&mut conn, replay, tx_id) else {
            anyhow::bail!("durably claimed snapshot transaction was accepted twice");
        };
        anyhow::ensure!(
            replay_error
                .downcast_ref::<crate::db::chan_net::SnapshotImportReplayError>()
                .is_some(),
            "duplicate snapshot did not preserve its typed replay error"
        );
        let unchanged_posts: i64 =
            conn.query_row("SELECT COUNT(*) FROM chan_net_posts", [], |row| row.get(0))?;
        anyhow::ensure!(unchanged_posts == 2, "replay changed imported post rows");
        Ok(())
    }
}
