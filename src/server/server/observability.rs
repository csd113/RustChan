use std::sync::atomic::Ordering;
use std::sync::LazyLock;
use std::time::Instant;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::config::CONFIG;
use crate::middleware::AppState;

use super::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT};

static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

#[derive(Serialize)]
struct HealthPayload {
    status: &'static str,
    uptime_seconds: u64,
    request_count: u64,
    in_flight_requests: u64,
}

#[derive(Serialize)]
struct ReadyPayload {
    status: &'static str,
    database_ready: bool,
    tor_enabled: bool,
    tor_onion_ready: bool,
    worker_queue_pending: i64,
}

pub(super) async fn healthz() -> impl IntoResponse {
    Json(HealthPayload {
        status: "ok",
        uptime_seconds: START_TIME.elapsed().as_secs(),
        request_count: REQUEST_COUNT.load(Ordering::Relaxed),
        in_flight_requests: IN_FLIGHT.load(Ordering::Relaxed),
    })
}

pub(super) async fn readyz(State(state): State<AppState>) -> Response {
    let database_ready = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> bool {
            pool.get().ok().is_some_and(|conn| {
                conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                    .ok()
                    .is_some_and(|value| value == 1)
            })
        }
    })
    .await
    .unwrap_or(false);

    let tor_onion_ready = if CONFIG.enable_tor_support {
        state.onion_address.read().await.is_some()
    } else {
        false
    };
    let payload = ReadyPayload {
        status: if database_ready { "ready" } else { "degraded" },
        database_ready,
        tor_enabled: CONFIG.enable_tor_support,
        tor_onion_ready,
        worker_queue_pending: state.job_queue.pending_count(),
    };
    let status = if database_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(payload)).into_response()
}

pub(super) async fn metrics(State(state): State<AppState>) -> Response {
    let backup = &state.backup_progress;
    let tor_onion_ready = if CONFIG.enable_tor_support {
        state.onion_address.read().await.is_some()
    } else {
        false
    };

    let body = format!(
        concat!(
            "# TYPE rustchan_requests_total counter\n",
            "rustchan_requests_total {}\n",
            "# TYPE rustchan_requests_in_flight gauge\n",
            "rustchan_requests_in_flight {}\n",
            "# TYPE rustchan_active_uploads gauge\n",
            "rustchan_active_uploads {}\n",
            "# TYPE rustchan_active_clients gauge\n",
            "rustchan_active_clients {}\n",
            "# TYPE rustchan_job_queue_pending gauge\n",
            "rustchan_job_queue_pending {}\n",
            "# TYPE rustchan_backup_phase gauge\n",
            "rustchan_backup_phase {}\n",
            "# TYPE rustchan_backup_files_done gauge\n",
            "rustchan_backup_files_done {}\n",
            "# TYPE rustchan_backup_files_total gauge\n",
            "rustchan_backup_files_total {}\n",
            "# TYPE rustchan_backup_bytes_done gauge\n",
            "rustchan_backup_bytes_done {}\n",
            "# TYPE rustchan_backup_bytes_total gauge\n",
            "rustchan_backup_bytes_total {}\n",
            "# TYPE rustchan_tor_enabled gauge\n",
            "rustchan_tor_enabled {}\n",
            "# TYPE rustchan_tor_onion_ready gauge\n",
            "rustchan_tor_onion_ready {}\n"
        ),
        REQUEST_COUNT.load(Ordering::Relaxed),
        IN_FLIGHT.load(Ordering::Relaxed),
        ACTIVE_UPLOADS.load(Ordering::Relaxed),
        ACTIVE_IPS.len(),
        state.job_queue.pending_count(),
        backup.phase.load(Ordering::Relaxed),
        backup.files_done.load(Ordering::Relaxed),
        backup.files_total.load(Ordering::Relaxed),
        backup.bytes_done.load(Ordering::Relaxed),
        backup.bytes_total.load(Ordering::Relaxed),
        u8::from(CONFIG.enable_tor_support),
        u8::from(tor_onion_ready),
    );

    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}
