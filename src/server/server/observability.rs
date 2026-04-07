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
use crate::handlers::admin::{full_backup_dir, list_backup_files, BackupListKind};
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
    media_processing_failed: i64,
    maintenance_active: bool,
    maintenance_label: Option<String>,
    latest_full_backup_verified: bool,
    latest_full_backup_age_hours: Option<i64>,
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
    let (
        database_ready,
        media_processing_failed,
        latest_full_backup_verified,
        latest_full_backup_age_hours,
    ) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> (bool, i64, bool, Option<i64>) {
            let full_backups = list_backup_files(&full_backup_dir(), BackupListKind::Full);
            let latest_backup = full_backups.first().cloned();
            let latest_full_backup_verified =
                latest_backup.as_ref().is_some_and(|backup| backup.verified);
            let latest_full_backup_age_hours = latest_backup.and_then(|backup| {
                backup
                    .modified_epoch
                    .map(|ts| chrono::Utc::now().timestamp().saturating_sub(ts).max(0) / 3600)
            });
            match pool.get() {
                Ok(conn) => {
                    let ready = conn
                        .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                        .ok()
                        .is_some_and(|value| value == 1);
                    let failed = crate::db::count_posts_by_media_processing_state(
                        &conn,
                        crate::db::MEDIA_PROCESSING_FAILED,
                    )
                    .unwrap_or(0);
                    (
                        ready,
                        failed,
                        latest_full_backup_verified,
                        latest_full_backup_age_hours,
                    )
                }
                Err(_) => (
                    false,
                    0,
                    latest_full_backup_verified,
                    latest_full_backup_age_hours,
                ),
            }
        }
    })
    .await
    .unwrap_or((false, 0, false, None));

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
        media_processing_failed,
        maintenance_active: state.maintenance_gate.is_active(),
        maintenance_label: state.maintenance_gate.active_label(),
        latest_full_backup_verified,
        latest_full_backup_age_hours,
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
    let (
        media_processing_pending,
        media_processing_failed,
        full_backup_count,
        latest_full_backup_verified,
        latest_full_backup_age_seconds,
    ) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> (i64, i64, i64, bool, i64) {
            let full_backups = list_backup_files(&full_backup_dir(), BackupListKind::Full);
            let full_backup_count = i64::try_from(full_backups.len()).unwrap_or(i64::MAX);
            let latest_full_backup_verified =
                full_backups.first().is_some_and(|backup| backup.verified);
            let latest_full_backup_age_seconds = full_backups
                .first()
                .and_then(|backup| backup.modified_epoch)
                .map(|ts| chrono::Utc::now().timestamp().saturating_sub(ts).max(0))
                .unwrap_or(-1);
            match pool.get() {
                Ok(conn) => (
                    crate::db::count_posts_by_media_processing_state(
                        &conn,
                        crate::db::MEDIA_PROCESSING_PENDING,
                    )
                    .unwrap_or(0),
                    crate::db::count_posts_by_media_processing_state(
                        &conn,
                        crate::db::MEDIA_PROCESSING_FAILED,
                    )
                    .unwrap_or(0),
                    full_backup_count,
                    latest_full_backup_verified,
                    latest_full_backup_age_seconds,
                ),
                Err(_) => (
                    0,
                    0,
                    full_backup_count,
                    latest_full_backup_verified,
                    latest_full_backup_age_seconds,
                ),
            }
        }
    })
    .await
    .unwrap_or((0, 0, 0, false, -1));

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
            "# TYPE rustchan_job_queue_dropped_total counter\n",
            "rustchan_job_queue_dropped_total {}\n",
            "# TYPE rustchan_media_processing_pending gauge\n",
            "rustchan_media_processing_pending {}\n",
            "# TYPE rustchan_media_processing_failed gauge\n",
            "rustchan_media_processing_failed {}\n",
            "# TYPE rustchan_full_backups_saved gauge\n",
            "rustchan_full_backups_saved {}\n",
            "# TYPE rustchan_latest_full_backup_verified gauge\n",
            "rustchan_latest_full_backup_verified {}\n",
            "# TYPE rustchan_latest_full_backup_age_seconds gauge\n",
            "rustchan_latest_full_backup_age_seconds {}\n",
            "# TYPE rustchan_maintenance_active gauge\n",
            "rustchan_maintenance_active {}\n",
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
        state.job_queue.dropped_count(),
        media_processing_pending,
        media_processing_failed,
        full_backup_count,
        u8::from(latest_full_backup_verified),
        latest_full_backup_age_seconds,
        u8::from(state.maintenance_gate.is_active()),
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
