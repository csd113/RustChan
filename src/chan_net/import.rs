// chan_net/import.rs — Federation import handler.
//
// POST /chan/import receives a raw snapshot ZIP body, performs deduplication
// via the in-memory TxLedger, validates the payload schema, writes boards and
// posts to the `chan_net_posts` mirror table, then records the tx_id in the
// ledger.
//
// `do_import()` is also called by `poll.rs` when draining the RustWave
// broadcast queue, so it is `pub` and accepts pre-read `bytes::Bytes` rather
// than reading from an Axum extractor internally.
//
// Order of operations inside do_import (MUST NOT be changed without updating
// the security hardening checklist in channet_build_plan.md § 6.3):
//
//   1. Unpack and parse the ZIP (rejects unknown filenames — path traversal guard)
//   2. Ed25519 signature check — log-and-skip if signature is present (not yet verified)
//   3. Check TxLedger — reject duplicate tx_ids BEFORE any DB write
//   4. Schema validation — all posts must have a non-empty board field and
//      content within the 32 768-character limit
//   5. DB writes (single spawn_blocking: boards then posts)
//   6. Record tx_id in ledger after confirmed successful write

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;
use tokio_util::bytes;

use super::snapshot::unpack_snapshot;
use crate::{error::AppError, middleware::AppState};

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
pub async fn do_import(state: &AppState, bytes: bytes::Bytes) -> Result<usize, AppError> {
    // ── 1. Unpack ────────────────────────────────────────────────────────────
    let (boards, posts, metadata) =
        unpack_snapshot(&bytes).map_err(|e| AppError::BadRequest(e.to_string()))?;

    // ── 2. Ed25519 signature check ───────────────────────────────────────────
    // Verification is not yet implemented. If a signature is present we log a
    // warning and continue rather than silently ignoring it.  A future phase
    // will verify the signature and reject snapshots that fail verification.
    //
    // SECURITY NOTE: Accepting signed snapshots without verification means
    // signature presence currently offers no authenticity guarantee.  Do NOT
    // promote this instance to production without completing Ed25519
    // verification (see channet_build_plan.md § 6.3).
    if let Some(ref sig) = metadata.signature {
        tracing::warn!(
            tx_id = %metadata.tx_id,
            signature = %sig,
            "Snapshot carries an Ed25519 signature — verification not yet \
             implemented; signature will not be checked until Phase N. \
             Proceeding without verification."
        );
    }

    // ── 3. Ledger check — must happen BEFORE any DB write ───────────────────
    {
        let ledger_arc = state
            .chan_ledger
            .as_ref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("ChanNet ledger not initialised")))?;

        // parking_lot::Mutex::lock() never poisons — no unwrap needed.
        let ledger = ledger_arc.lock();
        if ledger.contains(&metadata.tx_id) {
            return Err(AppError::Conflict("Snapshot already imported".into()));
        }
    } // ledger guard released here

    // ── 4. Schema validation — before any DB write ───────────────────────────
    for post in &posts {
        if post.board.trim().is_empty() {
            return Err(AppError::BadRequest(format!(
                "Post {} has an empty board field",
                post.post_id
            )));
        }
        if post.content.len() > 32_768 {
            return Err(AppError::BadRequest(format!(
                "Post {} content exceeds the 32 768-character limit",
                post.post_id
            )));
        }
    }

    let post_count = posts.len();
    let tx_id = metadata.tx_id;

    // ── 5. DB writes — all in one spawn_blocking ────────────────────────────
    let conn = state.db.get()?;

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        for board in &boards {
            // board.id is the short_name string (e.g. "tech", "b")
            let board_id =
                crate::db::chan_net::insert_board_if_absent(&conn, &board.id, &board.title)?;

            for post in posts.iter().filter(|p| p.board == board.id) {
                crate::db::chan_net::insert_post_if_absent(&conn, post, board_id)?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("spawn_blocking panic: {e}")))?
    .map_err(AppError::Internal)?;

    // ── 6. Record tx_id in ledger after confirmed successful write ───────────
    {
        let ledger_arc = state
            .chan_ledger
            .as_ref()
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("ChanNet ledger not initialised")))?;

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
pub async fn chan_import(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, super::ChanError> {
    let imported = do_import(&state, body)
        .await
        .map_err(super::ChanError::from)?;
    Ok((StatusCode::OK, Json(json!({ "imported": imported }))))
}
