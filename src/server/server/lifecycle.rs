// src/server/server/lifecycle.rs

use std::sync::atomic::Ordering;
use std::time::Instant;
use tracing::Instrument as _;

use super::{ScopedDecrement, ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT};

pub(super) async fn track_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let _in_flight_guard = ScopedDecrement(&IN_FLIGHT);

    let req_id = uuid::Uuid::new_v4();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let span = tracing::info_span!(
        "request",
        req_id = %req_id,
        method = %method,
        path  = %path,
    );

    {
        use sha2::{Digest, Sha256};
        let real_ip = crate::middleware::extract_ip(&req);
        let mut h = Sha256::new();
        h.update(real_ip.as_bytes());
        let ip_hash = hex::encode(h.finalize());
        if ACTIVE_IPS.len() < 10_000 {
            ACTIVE_IPS.insert(ip_hash, Instant::now());
        }
    }

    let is_upload = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("multipart/form-data"));

    let _upload_guard = is_upload.then(|| {
        ACTIVE_UPLOADS.fetch_add(1, Ordering::Relaxed);
        ScopedDecrement(&ACTIVE_UPLOADS)
    });

    next.run(req).instrument(span).await
}

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
                sig.recv().await;
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
