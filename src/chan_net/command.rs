//! `RustWave` gateway handler.
//
// POST /chan/command accepts a raw JSON body (Content-Type: application/json),
// deserialises it into the `Command` enum (dispatched via
// #[serde(tag = "type", rename_all = "snake_case")]), and returns a ZIP data
// package with a timestamped Content-Disposition filename.
//
// Commands:
//   full_export     — all boards, active threads, posts (optional `since` delta)
//   board_export    — one board, active threads, posts (optional `since` delta)
//   thread_export   — one thread, all its posts       (optional `since` delta)
//   archive_export  — archived threads for one board  (no `since` support)
//   force_refresh   — everything including archives   (no `since`, logs warn!)
//   reply_push      — insert a reply into the live posts table
//
// Text content only — no media fields ever cross this interface.
//
// Security hardening:
//   ✔ DefaultBodyLimit::max(CONFIG.chan_net_command_max_body) applied in mod.rs
//   ✔ reply_push: content validated at ≤ 32,768 chars before any DB write
//   ✔ reply_push: author validated at ≤ 255 chars before any DB write
//   ✔ insert_reply_into_thread verifies thread exists, board matches, not archived
//   ✔ No board or thread is created as a side effect — unknown targets return 400
//   ✔ `scope` field in GwMetadata set correctly by each builder
//   ✔ force_refresh emits tracing::warn! at call site (selective_snapshot.rs)
//   ✔ Content-Disposition filename follows the route naming convention

use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use super::selective_snapshot::{
    build_archive_snapshot, build_board_snapshot, build_force_refresh_snapshot,
    build_full_snapshot, build_thread_snapshot,
};
use crate::{error::AppError, middleware::AppState};

// ── ChanJson extractor ────────────────────────────────────────────────────────
//
// axum's built-in `Json` extractor maps all deserialization failures
// (missing required fields, wrong types, syntax errors) to HTTP 422.
// `/chan/command` must return 400 for those cases.
//
// `ChanJson` wraps `Json`, intercepts `JsonRejection`, and converts it to a
// `ChanError(AppError::BadRequest(…))` so that the existing `ChanError`
// `IntoResponse` impl renders the correct status code and JSON shape:
//   { "error": "<message>" }

/// JSON extractor that maps Axum rejections into the `ChanNet` error contract.
#[derive(Debug)]
pub struct ChanJson<T>(T);

impl<T, S> FromRequest<S> for ChanJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = super::ChanError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(val)) => Ok(Self(val)),
            Err(rejection) => {
                let msg = match &rejection {
                    JsonRejection::JsonDataError(_) => {
                        // Covers missing required fields and wrong field types.
                        format!("Invalid request body: {rejection}")
                    }
                    JsonRejection::JsonSyntaxError(_) => {
                        format!("Malformed JSON: {rejection}")
                    }
                    JsonRejection::MissingJsonContentType(_) => {
                        "Content-Type must be application/json".to_owned()
                    }
                    _ => rejection.to_string(),
                };
                if rejection.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                    Err(AppError::UploadTooLarge("Request body too large".to_owned()).into())
                } else {
                    Err(AppError::BadRequest(msg).into())
                }
            }
        }
    }
}

// ── Command enum ──────────────────────────────────────────────────────────────

/// Maximum number of Unicode scalar values accepted in a gateway reply body.
const MAX_REPLY_CONTENT_CHARS: usize = 32_768;
/// Maximum number of Unicode scalar values accepted in a gateway author name.
const MAX_REPLY_AUTHOR_CHARS: usize = 255;

/// Validates the semantic text limits for an incoming gateway reply.
fn validate_reply_text(author: &str, content: &str) -> anyhow::Result<()> {
    if content.chars().count() > MAX_REPLY_CONTENT_CHARS {
        anyhow::bail!("Reply content exceeds maximum length of 32,768 characters");
    }
    if author.chars().count() > MAX_REPLY_AUTHOR_CHARS {
        anyhow::bail!("Author name exceeds maximum length of 255 characters");
    }
    Ok(())
}

/// Maps internal command errors onto the stable HTTP command contract.
fn map_command_error(error: anyhow::Error) -> AppError {
    if error
        .downcast_ref::<crate::db::chan_net::ReplyReplayError>()
        .is_some()
    {
        return AppError::Conflict(error.to_string());
    }

    match AppError::from(error) {
        // Snapshot selection and command precondition errors historically use
        // the command API's 400 contract. Preserve that contract while letting
        // typed temporary database contention remain a retryable 503.
        AppError::Internal(error) => AppError::BadRequest(error.to_string()),
        other => other,
    }
}

/// All commands accepted by `POST /chan/command`.
///
/// Serialised as a JSON object with a `"type"` discriminant field.
/// Field names use `snake_case`. See the `RustWave` API reference section of
/// `channet_build_plan.md` for full field documentation and example payloads.
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Return all boards, all active (non-archived) threads, and all their
    /// posts. If `since` is provided, only posts newer than that Unix timestamp
    /// are included (delta mode). Thread metadata is always emitted in full.
    FullExport {
        /// Optional exclusive lower bound for post creation timestamps.
        since: Option<u64>,
    },

    /// Return all active threads and posts for a single board.
    /// If `since` is provided, only newer posts are included.
    BoardExport {
        /// URL-facing name of the board to export.
        board: String,
        /// Optional exclusive lower bound for post creation timestamps.
        since: Option<u64>,
    },

    /// Return all posts for a single thread.
    /// If `since` is provided, only newer posts are included.
    ThreadExport {
        /// Local identifier of the thread to export.
        thread_id: i64,
        /// Optional exclusive lower bound for post creation timestamps.
        since: Option<u64>,
    },

    /// Return all archived threads and their posts for a single board.
    /// `since` is not accepted — archives are static.
    ArchiveExport {
        /// URL-facing name of the board whose archive is exported.
        board: String,
    },

    /// Return everything: all boards, all active threads, all archived threads,
    /// all posts. No timestamp filtering. Intended for initial sync and
    /// disaster recovery. Use sparingly — this is the heaviest possible response.
    ForceRefresh,

    /// Insert a reply into the live `posts` table.
    ///
    /// This is the only command that writes to the database. The reply is
    /// immediately visible to web users browsing the board.
    ///
    /// Preconditions (enforced by `insert_reply_into_thread`):
    /// - Thread `thread_id` must exist.
    /// - Thread must belong to board `board`.
    /// - Thread must not be archived.
    ///
    /// `content` must be ≤ 32,768 characters.
    /// `author`  must be ≤ 255 characters.
    /// These are validated before any DB write; violations return 400.
    ///
    /// New integrations should always provide a stable, globally unique
    /// `message_id`. It is the durable idempotency key, so retrying the same
    /// message is safe even if other fields change. The field remains optional
    /// for compatibility with older gateways; legacy requests use the original
    /// content-and-timestamp fingerprint and therefore cannot distinguish two
    /// identical replies created within the same timestamp second.
    ReplyPush {
        /// URL-facing name of the destination board.
        board: String,
        /// Local identifier of the destination thread.
        thread_id: i64,
        /// Displayed author name.
        author: String,
        /// Plain reply content.
        content: String,
        /// Source Unix timestamp used by legacy idempotency.
        timestamp: u64,
        /// Stable idempotency key supplied by modern gateways.
        message_id: Option<uuid::Uuid>,
    },
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// `POST /chan/command`
///
/// Accepts a JSON body (Content-Type: application/json, body ≤
/// `CONFIG.chan_net_command_max_body` bytes — enforced by `DefaultBodyLimit`
/// applied in `chan_router()`).
///
/// Returns a ZIP package (Content-Type: application/zip) with a
/// `Content-Disposition: attachment; filename="<timestamped-name>.zip"` header.
///
/// Filename conventions (Unix timestamp = seconds since epoch at dispatch time):
///   `full_export`     → `rustchan_full_<ts>.zip`
///   `board_export`    → `rustchan_board_<board>_<ts>.zip`
///   `thread_export`   → `rustchan_thread_<thread_id>_<ts>.zip`
///   `archive_export`  → `rustchan_archive_<board>_<ts>.zip`
///   `force_refresh`   → `rustchan_force_refresh_<ts>.zip`
///   `reply_push`      → `rustchan_thread_<thread_id>_reply_confirmed_<ts>.zip`
///
/// Error mapping:
///   Snapshot builder / DB errors → 400 Bad Request (anyhow errors from the
///   blocking task are mapped by `.map_err(|e| AppError::BadRequest(e.to_string()))`)
///   Tokio join errors            → 500 Internal Server Error
///
/// # Errors
///
/// Returns a [`super::ChanError`] when authentication-adjacent extraction,
/// validation, database access, snapshot construction, or task execution fails.
pub async fn chan_command(
    State(state): State<AppState>,
    ChanJson(cmd): ChanJson<Command>,
) -> Result<impl IntoResponse, super::ChanError> {
    // Use ? directly — AppError implements From<r2d2::Error>.
    let conn = state.db.get()?;

    let (zip_bytes, filename) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, String)> {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            match cmd {
                Command::FullExport { since } => {
                    let (zip, _) = build_full_snapshot(&conn, since)?;
                    Ok((zip, format!("rustchan_full_{now}.zip")))
                }

                Command::BoardExport { board, since } => {
                    let (zip, _) = build_board_snapshot(&conn, &board, since)?;
                    Ok((zip, format!("rustchan_board_{board}_{now}.zip")))
                }

                Command::ThreadExport { thread_id, since } => {
                    let (zip, _) = build_thread_snapshot(&conn, thread_id, since)?;
                    Ok((zip, format!("rustchan_thread_{thread_id}_{now}.zip")))
                }

                Command::ArchiveExport { board } => {
                    let (zip, _) = build_archive_snapshot(&conn, &board)?;
                    Ok((zip, format!("rustchan_archive_{board}_{now}.zip")))
                }

                Command::ForceRefresh => {
                    let (zip, _) = build_force_refresh_snapshot(&conn)?;
                    Ok((zip, format!("rustchan_force_refresh_{now}.zip")))
                }

                Command::ReplyPush {
                    board,
                    thread_id,
                    author,
                    content,
                    timestamp,
                    message_id,
                } => {
                    // ── Input validation — must happen before any DB write ──
                    validate_reply_text(&author, &content)?;

                    // ── DB write ───────────────────────────────────────────
                    // insert_reply_into_thread validates thread existence,
                    // board membership, and archive status internally.
                    crate::db::chan_net::insert_reply_into_thread(
                        &conn,
                        &board,
                        thread_id,
                        &author,
                        &content,
                        timestamp.cast_signed(),
                        message_id.as_ref(),
                    )?;

                    // Return the updated thread as the confirmation payload.
                    // `since = None` so that the full thread (including the new
                    // post) is always included in the response ZIP.
                    let (zip, _) = build_thread_snapshot(&conn, thread_id, None)?;
                    Ok((
                        zip,
                        format!("rustchan_thread_{thread_id}_reply_confirmed_{now}.zip"),
                    ))
                }
            }
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))? // JoinError → 500
        .map_err(map_command_error)?;

    let disposition = format!("attachment; filename=\"{filename}\"");

    Ok((
        axum::http::StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        zip_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::validate_reply_text;

    #[test]
    fn reply_limits_count_unicode_scalar_values() {
        assert!(validate_reply_text(&"🦀".repeat(255), &"🦀".repeat(32_768)).is_ok());
        assert!(validate_reply_text(&"🦀".repeat(256), "valid").is_err());
        assert!(validate_reply_text("valid", &"🦀".repeat(32_769)).is_err());
    }
}
