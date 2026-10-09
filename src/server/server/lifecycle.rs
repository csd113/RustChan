//! Request lifecycle accounting and shutdown-signal handling.

use futures::StreamExt as _;

use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::Instrument as _;

use super::{ScopedDecrement, ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT};

/// Response header containing the generated request identifier.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Track request counts, active clients, uploads, tracing, and request IDs.
pub(super) async fn track_requests(
    axum::extract::State(state): axum::extract::State<crate::middleware::AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if state.job_queue.cancel.is_cancelled()
        || (!state.runtime_ready.load(Ordering::Acquire) && req.uri().path() != "/readyz")
    {
        return recovery_unavailable();
    }
    if req.uri().path() != "/readyz"
        && crate::updates::managed()
        && !crate::updates::requests_allowed().await
    {
        return recovery_unavailable();
    }
    let permit = match state.request_work_gate.try_begin() {
        Ok(permit) => permit,
        Err(error) => return axum::response::IntoResponse::into_response(error),
    };
    let _previous_request_count = REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    let _previous_in_flight_count = IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let _in_flight_guard = ScopedDecrement(&IN_FLIGHT);

    let req_id = uuid::Uuid::new_v4().to_string();
    let method = req.method().clone();
    let path = req.uri().path().chars().take(256).collect::<String>();
    let mut req = req;
    let _previous_request_id = req.extensions_mut().insert(req_id.clone());
    let span = tracing::info_span!(
        "request",
        req_id = %req_id,
        method = %method,
        path  = %path,
    );

    {
        use sha2::{Digest as _, Sha256};
        let real_ip = crate::middleware::extract_ip(&req);
        let mut h = Sha256::new();
        h.update(real_ip.as_bytes());
        let ip_hash = hex::encode(h.finalize());
        if ACTIVE_IPS.len() < 10_000 {
            let _previous_ip_activity = ACTIVE_IPS.insert(ip_hash, Instant::now());
        }
    }

    let is_upload = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("multipart/form-data"));

    let _upload_guard = is_upload.then(|| {
        let _previous_upload_count = ACTIVE_UPLOADS.fetch_add(1, Ordering::Relaxed);
        ScopedDecrement(&ACTIVE_UPLOADS)
    });

    let mut response = next.run(req).instrument(span).await;
    if let Ok(value) = axum::http::HeaderValue::from_str(&req_id) {
        let _previous_request_header = response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    // Retain admission through streamed media responses, including clients
    // that stop reading after the response headers have been sent.
    response.map(move |body| {
        axum::body::Body::from_stream(body.into_data_stream().map(move |chunk| {
            let _request_permit = &permit;
            chunk
        }))
    })
}

/// Wait for a platform shutdown signal.
pub(super) async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!("Failed to listen for Ctrl+C: {e}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                let _previous_value = sig.recv().await;
            }
            Err(e) => {
                tracing::error!("Failed to register SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!(target: "server", signal = "SIGINT", "Shutdown signal received"),
        () = terminate => tracing::info!(target: "server", signal = "SIGTERM", "Shutdown signal received"),
    }
}

/// Public write admission reports availability without exposing installation/recovery state.
fn recovery_unavailable() -> axum::response::Response {
    use axum::response::IntoResponse as _;
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "3")],
        "Service temporarily unavailable. Retry shortly.",
    )
        .into_response()
}

#[cfg(test)]
/// Public admission privacy contract.
mod tests {
    use super::*;

    /// Anonymous/API callers must not receive update phase, version or recovery details.
    #[tokio::test]
    async fn public_admission_error_keeps_updates_private() -> anyhow::Result<()> {
        let response = recovery_unavailable();
        anyhow::ensure!(
            response.status() == axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unsafe writes must fail closed"
        );
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        let text = std::str::from_utf8(&body)?;
        anyhow::ensure!(
            !text.contains("update")
                && !text.contains("recovery")
                && !text.contains(crate::updates::VERSION),
            "public availability errors must not expose update state"
        );
        Ok(())
    }
}

#[cfg(test)]
/// Shutdown request admission using the production middleware and cancellation token.
mod shutdown_tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use tower::ServiceExt as _;

    /// Stop new work while allowing already accepted work to complete through listener drain.
    #[tokio::test]
    async fn cancelled_runtime_rejects_new_http_work() -> anyhow::Result<()> {
        let state = crate::test_support::app_state();
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn_with_state(state.clone(), track_requests));
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty())?)
            .await?;
        anyhow::ensure!(
            response.status() == StatusCode::NO_CONTENT,
            "running instance should admit HTTP work"
        );
        state.job_queue.cancel.cancel();
        let unavailable_response = app
            .oneshot(Request::builder().uri("/").body(Body::empty())?)
            .await?;
        anyhow::ensure!(
            unavailable_response.status() == StatusCode::SERVICE_UNAVAILABLE,
            "shutdown must stop new work"
        );
        Ok(())
    }
}
