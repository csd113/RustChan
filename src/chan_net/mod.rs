// chan_net/mod.rs — ChanNet API module root.
//
// Runs on a second TCP listener (default 127.0.0.1:7070), separate from the
// main forum port. Activated with the --chan-net CLI flag.
//
// Two independent layers:
//   Layer 1 — Federation sync: node-to-node ZIP exchange
//   Layer 2 — RustWave gateway: JSON command in, ZIP package out
//
// Rate-limit middleware is intentionally excluded — all traffic on this
// listener is machine-to-machine.

pub mod command;
pub mod export;
pub mod import;
pub mod ledger;
pub mod poll;
pub mod refresh;
pub mod selective_snapshot;
pub mod snapshot;
pub mod status;

use crate::config::CONFIG;
use crate::error::AppError;
use crate::middleware::AppState;
use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;

// ── ChanError ─────────────────────────────────────────────────────────────────
//
// All `/chan/*` routes are machine-to-machine. They must never return the HTML
// error pages that `AppError::into_response` renders for browser-facing routes.
// `ChanError` wraps `AppError` and overrides `IntoResponse` to emit JSON:
//
//   { "error": "<message>" }
//
// with the same HTTP status code that `AppError` would have produced.

/// JSON-rendering error type for all `/chan/*` handlers.
pub struct ChanError(pub AppError);

impl From<AppError> for ChanError {
    fn from(e: AppError) -> Self {
        Self(e)
    }
}

// Forward the common conversions that handler code uses with `?`.
impl From<r2d2::Error> for ChanError {
    fn from(e: r2d2::Error) -> Self {
        Self(AppError::from(e))
    }
}

impl From<rusqlite::Error> for ChanError {
    fn from(e: rusqlite::Error) -> Self {
        Self(AppError::from(e))
    }
}

impl From<anyhow::Error> for ChanError {
    fn from(e: anyhow::Error) -> Self {
        Self(AppError::from(e))
    }
}

impl IntoResponse for ChanError {
    fn into_response(self) -> Response {
        let (status, message, retry_after) = match self.0 {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg, None),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg, None),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg, None),
            AppError::BannedUser { reason, .. } => (StatusCode::FORBIDDEN, reason, None),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg, None),
            AppError::UploadTooLarge(msg) => (StatusCode::PAYLOAD_TOO_LARGE, msg, None),
            AppError::InvalidMediaType(msg) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, msg, None),
            AppError::DbBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Database busy — retry shortly.".to_owned(),
                Some("1"),
            ),
            AppError::Internal(e) => {
                tracing::error!("ChanNet internal error: {:?}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "An internal error occurred.".to_owned(),
                    None,
                )
            }
            AppError::Tls(msg) => {
                tracing::error!("ChanNet TLS error: {msg}");
                (StatusCode::INTERNAL_SERVER_ERROR, msg, None)
            }
        };

        let mut response = (status, Json(json!({ "error": message }))).into_response();
        if let Some(seconds) = retry_after {
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static(seconds),
            );
        }
        response
    }
}

// ── Body-limit JSON middleware ─────────────────────────────────────────────────
//
// `DefaultBodyLimit` rejects oversized bodies before the handler runs, and its
// built-in rejection renders plain text (StatusCode 413, body:
// "Failed to buffer request body: …"). That bypasses our `ChanError` JSON
// rendering. This middleware sits *outside* the body-limit layer and
// intercepts any 413 response, replacing it with a proper JSON error body.

async fn json_body_limit_error(req: axum::http::Request<axum::body::Body>, next: Next) -> Response {
    let response = next.run(req).await;
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "error": "Request body too large" })),
        )
            .into_response();
    }
    response
}

// ─── ChanNet API key middleware ───────────────────────────────────────────────

/// Middleware that enforces the pre-shared `X-ChanNet-Key` header on sensitive
/// `ChanNet` endpoints.
///
/// Any process that can reach the `ChanNet` bind address can otherwise trigger
/// snapshot export/import or gateway commands with no credentials. The API key
/// is configured via `CHAN_NET_API_KEY` / settings.toml.
///
/// If `chan_net_api_key` is empty the request is rejected with 403 Forbidden
/// (the feature is intentionally disabled rather than wide open).
async fn verify_chan_api_key(
    axum::extract::State(expected): axum::extract::State<Arc<str>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    use subtle::ConstantTimeEq as _;
    if expected.is_empty() {
        // API key not configured — refuse the request to prevent accidental
        // exposure when an operator forgets to set the key.
        return StatusCode::FORBIDDEN.into_response();
    }
    let provided = req
        .headers()
        .get("X-ChanNet-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Constant-time comparison to prevent timing side-channels.
    if provided.as_bytes().ct_eq(expected.as_bytes()).into() {
        next.run(req).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Build the `ChanNet` router.
///
/// All `/chan/*` routes are wired here. `DefaultBodyLimit` is applied
/// per-route so that the `/chan/command` JSON limit does not accidentally
/// apply to the ZIP import route and vice-versa.
///
/// `/chan/command` body limit (`CONFIG.chan_net_command_max_body`) must be
/// large enough that a `reply_push` carrying the maximum 32,768 Unicode scalar
/// values plus JSON escaping and envelope overhead reaches semantic validation
/// rather than being rejected as 413 first. The config default is `512 * 1024`.
pub fn chan_router(state: AppState) -> Router {
    chan_router_with_auth(
        state,
        Arc::from(CONFIG.chan_net_api_key.as_str()),
        CONFIG.chan_net_command_max_body,
        CONFIG.chan_net_max_body,
    )
}

pub fn chan_router_with_auth(
    state: AppState,
    api_key: Arc<str>,
    command_max_body: usize,
    import_max_body: usize,
) -> Router {
    let protected_routes = Router::new()
        // ── RustWave gateway — raw JSON in, ZIP data package out ─────────────
        //
        // The json_body_limit_error middleware is applied *outside* the
        // DefaultBodyLimit layer so that 413 rejections are rendered as JSON
        // instead of the default plain-text "Failed to buffer request body".
        .route(
            "/chan/command",
            post(command::chan_command)
                .layer(DefaultBodyLimit::max(command_max_body))
                .layer(middleware::from_fn(json_body_limit_error)),
        )
        // ── Federation sync — ZIP in, ZIP out ────────────────────────────────
        .route("/chan/export", post(export::chan_export))
        .route(
            "/chan/import",
            post(import::chan_import)
                .layer(DefaultBodyLimit::max(import_max_body))
                .layer(middleware::from_fn(json_body_limit_error)),
        )
        .route("/chan/refresh", post(refresh::chan_refresh))
        .route("/chan/poll", post(poll::chan_poll))
        .route_layer(middleware::from_fn_with_state(api_key, verify_chan_api_key));

    Router::new()
        // ── Status ──────────────────────────────────────────────────────────
        .route("/chan/status", get(status::chan_status))
        .merge(protected_routes)
        .with_state(state)
        .layer(middleware::from_fn(
            crate::server::request_boundary_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use super::chan_router_with_auth;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tower::ServiceExt as _;

    const TEST_KEY: &str = "0123456789abcdef0123456789abcdef";

    fn chan_test_state() -> crate::middleware::AppState {
        let mut state = crate::test_support::app_state();
        state.chan_ledger = Some(Arc::new(parking_lot::Mutex::new(
            crate::chan_net::ledger::TxLedger::default(),
        )));
        state
    }

    fn chan_test_router(state: crate::middleware::AppState) -> axum::Router {
        chan_router_with_auth(state, Arc::from(TEST_KEY), 512 * 1024, 10 * 1024 * 1024)
    }

    fn with_single_connection_pool(
        mut state: crate::middleware::AppState,
        connection_timeout: Duration,
    ) -> crate::middleware::AppState {
        let database_path: String = {
            let conn = state.db.get().expect("initial connection");
            conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get(0),
            )
            .expect("test database path")
        };
        let manager = r2d2_sqlite::SqliteConnectionManager::file(database_path).with_init(|conn| {
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                     PRAGMA busy_timeout = 25;",
            )
        });
        state.db = r2d2::Pool::builder()
            .max_size(1)
            .connection_timeout(connection_timeout)
            .build(manager)
            .expect("single-connection pool");
        state
    }

    fn seed_command_thread(state: &crate::middleware::AppState) -> i64 {
        let conn = state.db.get().expect("db connection");
        conn.execute(
            "INSERT INTO boards (short_name, name, description)
             VALUES ('gateway', 'Gateway', '')",
            [],
        )
        .expect("insert board");
        let board_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO threads (board_id, subject) VALUES (?1, 'gateway thread')",
            rusqlite::params![board_id],
        )
        .expect("insert thread");
        conn.last_insert_rowid()
    }

    async fn command_request(
        router: &axum::Router,
        payload: serde_json::Value,
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&payload).expect("serialise command"),
                    ))
                    .expect("command request"),
            )
            .await
            .expect("command response")
    }

    async fn assert_json_payload_too_large(response: axum::response::Response) {
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("read error response");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON error");
        assert_eq!(
            json.get("error").and_then(serde_json::Value::as_str),
            Some("Request body too large")
        );
    }

    #[tokio::test]
    async fn protected_chan_routes_reject_missing_and_wrong_key() {
        let router = chan_test_router(chan_test_state());
        let cases = [
            ("/chan/command", Body::from(r#"{"type":"full_export"}"#)),
            ("/chan/export", Body::empty()),
            ("/chan/import", Body::from(Vec::<u8>::new())),
        ];

        for (path, body) in cases {
            let missing = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(body)
                        .expect("missing-key request"),
                )
                .await
                .expect("missing-key response");
            assert_eq!(missing.status(), StatusCode::UNAUTHORIZED, "{path}");

            let wrong = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("X-ChanNet-Key", "wrong-key")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(r#"{"type":"full_export"}"#))
                        .expect("wrong-key request"),
                )
                .await
                .expect("wrong-key response");
            assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[tokio::test]
    async fn protected_chan_routes_accept_correct_key() {
        let state = chan_test_state();
        let snapshot = {
            let conn = state.db.get().expect("db connection");
            crate::chan_net::snapshot::build_snapshot(&conn)
                .expect("snapshot")
                .0
        };
        let router = chan_test_router(state);

        let command = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"type":"full_export"}"#))
                    .expect("command request"),
            )
            .await
            .expect("command response");
        assert_eq!(command.status(), StatusCode::OK);

        let export = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/export")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .body(Body::empty())
                    .expect("export request"),
            )
            .await
            .expect("export response");
        assert_eq!(export.status(), StatusCode::OK);

        let import = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/import")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/zip")
                    .body(Body::from(snapshot))
                    .expect("import request"),
            )
            .await
            .expect("import response");
        assert_eq!(import.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn command_accepts_legal_ascii_and_unicode_limits_and_rejects_replays() {
        let state = chan_test_state();
        let thread_id = seed_command_thread(&state);
        let router = chan_test_router(state.clone());

        let unicode = serde_json::json!({
            "type": "reply_push",
            "board": "gateway",
            "thread_id": thread_id,
            "author": "🦀".repeat(100),
            "content": "🦀".repeat(32_768),
            "timestamp": 100_u64,
        });
        let first = command_request(&router, unicode.clone()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let replay = command_request(&router, unicode).await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);

        let exact = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "gateway",
                "content": "a".repeat(32_768),
                "timestamp": 101_u64,
            }),
        )
        .await;
        assert_eq!(exact.status(), StatusCode::OK);

        let over = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "gateway",
                "content": "a".repeat(32_769),
                "timestamp": 102_u64,
            }),
        )
        .await;
        assert_eq!(over.status(), StatusCode::BAD_REQUEST);

        let exact_author = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "界".repeat(255),
                "content": "valid author boundary",
                "timestamp": 103_u64,
            }),
        )
        .await;
        assert_eq!(exact_author.status(), StatusCode::OK);

        let over_author = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "界".repeat(256),
                "content": "invalid author boundary",
                "timestamp": 104_u64,
            }),
        )
        .await;
        assert_eq!(over_author.status(), StatusCode::BAD_REQUEST);

        let conn = state.db.get().expect("db connection");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn command_uses_stable_message_ids_without_merging_identical_replies() {
        let state = chan_test_state();
        let thread_id = seed_command_thread(&state);
        let router = chan_test_router(state.clone());
        let first_id = "00000000-0000-4000-8000-000000000001";
        let second_id = "00000000-0000-4000-8000-000000000002";

        let request = |message_id: &str, author: &str, content: &str, timestamp: u64| {
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": author,
                "content": content,
                "timestamp": timestamp,
                "message_id": message_id,
            })
        };

        let first = command_request(
            &router,
            request(first_id, "gateway", "same legitimate reply", 100),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let distinct = command_request(
            &router,
            request(second_id, "gateway", "same legitimate reply", 100),
        )
        .await;
        assert_eq!(distinct.status(), StatusCode::OK);

        let replay = command_request(
            &router,
            request(first_id, "changed author", "changed body", 999),
        )
        .await;
        assert_eq!(replay.status(), StatusCode::CONFLICT);

        let conn = state.db.get().expect("db connection");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn command_accepts_json_escaped_unicode_at_legal_limit() {
        let state = chan_test_state();
        let thread_id = seed_command_thread(&state);
        let router = chan_test_router(state.clone());

        // Some JSON clients encode a four-byte scalar value as a UTF-16
        // surrogate pair. That representation is twelve wire bytes per
        // character and must still fit beneath the default transport cap.
        let escaped_content = "\\ud83e\\udd80".repeat(32_768);
        let escaped_exact = format!(
            r#"{{"type":"reply_push","board":"gateway","thread_id":{thread_id},"author":"gateway","content":"{escaped_content}","timestamp":105}}"#
        );
        let escaped_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(escaped_exact))
                    .expect("escaped command request"),
            )
            .await
            .expect("escaped command response");
        assert_eq!(escaped_response.status(), StatusCode::OK);

        let escaped_over_content = format!("{escaped_content}\\ud83e\\udd80");
        let escaped_over = format!(
            r#"{{"type":"reply_push","board":"gateway","thread_id":{thread_id},"author":"gateway","content":"{escaped_over_content}","timestamp":106}}"#
        );
        let escaped_over_response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(escaped_over))
                    .expect("escaped over-limit command request"),
            )
            .await
            .expect("escaped over-limit command response");
        assert_eq!(escaped_over_response.status(), StatusCode::BAD_REQUEST);

        let conn = state.db.get().expect("db connection");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn command_write_contention_is_bounded_and_returns_retry_contract() {
        let state = chan_test_state();
        let thread_id = seed_command_thread(&state);
        let router = chan_test_router(state.clone());
        let lock = state.db.get().expect("write-lock connection");
        lock.execute_batch("BEGIN IMMEDIATE")
            .expect("hold database write lock");

        let started = Instant::now();
        let response = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "gateway",
                "content": "contended reply",
                "timestamp": 200_u64,
            }),
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "busy response took {elapsed:?}"
        );
        lock.execute_batch("ROLLBACK").expect("release write lock");

        let conn = state.db.get().expect("verification connection");
        let post_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("database integrity");
        assert_eq!(post_count, 0);
        assert_eq!(integrity, "ok");
    }

    #[tokio::test]
    async fn command_pool_exhaustion_is_bounded_and_returns_retry_contract() {
        let state = with_single_connection_pool(chan_test_state(), Duration::from_millis(25));
        let thread_id = seed_command_thread(&state);
        let router = chan_test_router(state.clone());
        let held_connection = state.db.get().expect("exhaust the only pool slot");

        let started = Instant::now();
        let response = command_request(
            &router,
            serde_json::json!({
                "type": "reply_push",
                "board": "gateway",
                "thread_id": thread_id,
                "author": "gateway",
                "content": "pool-exhausted reply",
                "timestamp": 201_u64,
            }),
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "pool exhaustion response took {elapsed:?}"
        );

        drop(held_connection);
        let conn = state.db.get().expect("verification connection");
        let post_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
                rusqlite::params![thread_id],
                |row| row.get(0),
            )
            .expect("reply count");
        assert_eq!(post_count, 0);
    }

    #[tokio::test]
    async fn command_and_import_bodies_at_exact_limit_reach_semantic_validation() {
        const BODY_LIMIT: usize = 128;

        let state = chan_test_state();
        let router = chan_router_with_auth(state, Arc::from(TEST_KEY), BODY_LIMIT, BODY_LIMIT);
        let prefix = br#"{"type":"unsupported_command","padding":""#;
        let suffix = br#""}"#;
        let padding_len = BODY_LIMIT
            .checked_sub(prefix.len() + suffix.len())
            .expect("test command envelope fits body limit");
        let mut body = Vec::with_capacity(BODY_LIMIT);
        body.extend_from_slice(prefix);
        body.extend(std::iter::repeat_n(b'x', padding_len));
        body.extend_from_slice(suffix);
        assert_eq!(body.len(), BODY_LIMIT);

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("exact-limit command request"),
            )
            .await
            .expect("exact-limit command response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = to_bytes(response.into_body(), 2048)
            .await
            .expect("read semantic error response");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON error");
        assert!(json
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| message.contains("unsupported_command")));

        let import = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/import")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/zip")
                    .body(Body::from(vec![0_u8; BODY_LIMIT]))
                    .expect("exact-limit import request"),
            )
            .await
            .expect("exact-limit import response");

        assert_eq!(import.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            import
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let body = to_bytes(import.into_body(), 2048)
            .await
            .expect("read invalid ZIP error response");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("JSON error");
        assert!(json
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| !message.is_empty() && message != "Request body too large"));
    }

    #[tokio::test]
    async fn command_and_import_body_limits_return_json_413() {
        let state = chan_test_state();
        let router = chan_router_with_auth(state, Arc::from(TEST_KEY), 128, 128);

        let command = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/command")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "type": "full_export",
                            "padding": "x".repeat(256),
                        }))
                        .expect("serialise oversized command"),
                    ))
                    .expect("oversized command request"),
            )
            .await
            .expect("oversized command response");
        assert_json_payload_too_large(command).await;

        let import = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chan/import")
                    .header("X-ChanNet-Key", TEST_KEY)
                    .header(header::CONTENT_TYPE, "application/zip")
                    .body(Body::from(vec![0_u8; 129]))
                    .expect("oversized import request"),
            )
            .await
            .expect("oversized import response");
        assert_json_payload_too_large(import).await;
    }
}
