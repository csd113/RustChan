//! Health, readiness, and Prometheus-compatible metrics endpoints.

use std::sync::atomic::Ordering;

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

#[derive(Serialize)]
/// Minimal liveness response.
struct HealthPayload {
    /// Liveness status label.
    status: &'static str,
}

#[derive(Serialize)]
// This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the detailed readiness schema reports independent subsystem states"
)]
/// Detailed readiness response.
struct ReadyPayload {
    /// Aggregate readiness label.
    status: &'static str,
    /// Whether the database accepts queries and has the expected schema.
    database_ready: bool,
    /// Expected schema version.
    database_schema_version: &'static str,
    /// Whether the schema matches the expected release baseline.
    database_schema_valid: bool,
    /// Whether Tor support is configured.
    tor_enabled: bool,
    /// Whether the onion service has published an address.
    tor_onion_ready: bool,
    /// Pending worker-queue job count.
    worker_queue_pending: i64,
    /// Failed media-processing job count.
    media_processing_failed: i64,
    /// Whether a maintenance operation is active.
    maintenance_active: bool,
    /// Active maintenance operation label.
    maintenance_label: Option<String>,
    /// Whether the newest full backup passed verification.
    latest_full_backup_verified: bool,
    /// Age of the newest full backup in hours.
    latest_full_backup_age_hours: Option<i64>,
}

#[derive(Serialize)]
/// Public readiness response without operational internals.
struct PublicReadyPayload {
    /// Aggregate readiness label.
    status: &'static str,
}

/// Return process liveness.
pub(super) async fn healthz() -> impl IntoResponse {
    Json(HealthPayload { status: "ok" })
}

/// Return configured public or detailed readiness.
pub(super) async fn readyz(State(state): State<AppState>) -> Response {
    readyz_response(state, CONFIG.public_readiness_details).await
}

/// Build a readiness response with optional operational details.
#[expect(
    clippy::too_many_lines,
    reason = "readiness computes one coherent subsystem snapshot before serializing its response"
)]
async fn readyz_response(state: AppState, include_details: bool) -> Response {
    if !include_details {
        let database_ready = tokio::task::spawn_blocking({
            let pool = state.db.clone();
            move || match pool.get() {
                Ok(conn) => {
                    let ready = conn
                        .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                        .ok()
                        .is_some_and(|value| value == 1);
                    ready && crate::db::verify_database_schema(&conn).is_ok()
                }
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false);

        let status_label = if database_ready { "ready" } else { "degraded" };
        let status = if database_ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        return (
            status,
            Json(PublicReadyPayload {
                status: status_label,
            }),
        )
            .into_response();
    }

    let (
        database_ready,
        database_schema_valid,
        media_processing_failed,
        latest_full_backup_verified,
        latest_full_backup_age_hours,
    ) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> (bool, bool, i64, bool, Option<i64>) {
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
                    let schema_valid = crate::db::verify_database_schema(&conn).is_ok();
                    let failed = crate::db::count_posts_by_media_processing_state(
                        &conn,
                        crate::db::MEDIA_PROCESSING_FAILED,
                    )
                    .unwrap_or(0);
                    (
                        ready && schema_valid,
                        schema_valid,
                        failed,
                        latest_full_backup_verified,
                        latest_full_backup_age_hours,
                    )
                }
                Err(_) => (
                    false,
                    false,
                    0,
                    latest_full_backup_verified,
                    latest_full_backup_age_hours,
                ),
            }
        }
    })
    .await
    .unwrap_or((false, false, 0, false, None));

    let tor_onion_ready = if CONFIG.enable_tor_support {
        state.onion_address.read().await.is_some()
    } else {
        false
    };
    let status_label = if database_ready { "ready" } else { "degraded" };
    let status = if database_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let payload = ReadyPayload {
        status: status_label,
        database_ready,
        database_schema_version: crate::db::baseline_schema_version(),
        database_schema_valid,
        tor_enabled: CONFIG.enable_tor_support,
        tor_onion_ready,
        worker_queue_pending: state.job_queue.pending_count(),
        media_processing_failed,
        maintenance_active: state.maintenance_gate.is_active(),
        maintenance_label: state.maintenance_gate.active_label(),
        latest_full_backup_verified,
        latest_full_backup_age_hours,
    };
    (status, Json(payload)).into_response()
}

/// Return public metrics when explicitly enabled.
pub(super) async fn metrics(State(state): State<AppState>) -> Response {
    if !CONFIG.public_metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    metrics_response(state).await
}

/// Build the Prometheus text exposition.
#[expect(
    clippy::too_many_lines,
    reason = "keeping metric declarations and values together prevents exposition-order drift"
)]
async fn metrics_response(state: AppState) -> Response {
    let backup = &state.backup_progress;
    let media_reconcile = crate::media::reconcile::metrics_snapshot();
    let tor_onion_ready = if CONFIG.enable_tor_support {
        state.onion_address.read().await.is_some()
    } else {
        false
    };
    let (
        media_processing_pending,
        media_processing_failed,
        database_schema_valid,
        full_backup_count,
        latest_full_backup_verified,
        latest_full_backup_age_seconds,
    ) = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> (i64, i64, bool, i64, bool, i64) {
            let full_backups = list_backup_files(&full_backup_dir(), BackupListKind::Full);
            let full_backup_count = i64::try_from(full_backups.len()).unwrap_or(i64::MAX);
            let latest_full_backup_verified =
                full_backups.first().is_some_and(|backup| backup.verified);
            let latest_full_backup_age_seconds = full_backups
                .first()
                .and_then(|backup| backup.modified_epoch)
                .map_or(-1, |ts| {
                    chrono::Utc::now().timestamp().saturating_sub(ts).max(0)
                });
            match pool.get() {
                Ok(conn) => {
                    let database_schema_valid = crate::db::verify_database_schema(&conn).is_ok();
                    (
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
                        database_schema_valid,
                        full_backup_count,
                        latest_full_backup_verified,
                        latest_full_backup_age_seconds,
                    )
                }
                Err(_) => (
                    0,
                    0,
                    false,
                    full_backup_count,
                    latest_full_backup_verified,
                    latest_full_backup_age_seconds,
                ),
            }
        }
    })
    .await
    .unwrap_or((0, 0, false, 0, false, -1));

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
            "# TYPE rustchan_media_reconcile_files_scanned_total counter\n",
            "rustchan_media_reconcile_files_scanned_total {}\n",
            "# TYPE rustchan_media_reconcile_references_scanned_total counter\n",
            "rustchan_media_reconcile_references_scanned_total {}\n",
            "# TYPE rustchan_media_reconcile_missing_references_total counter\n",
            "rustchan_media_reconcile_missing_references_total {}\n",
            "# TYPE rustchan_media_reconcile_safe_orphan_bytes_total counter\n",
            "rustchan_media_reconcile_safe_orphan_bytes_total {}\n",
            "# TYPE rustchan_media_reconcile_ambiguous_files_total counter\n",
            "rustchan_media_reconcile_ambiguous_files_total {}\n",
            "# TYPE rustchan_media_reconcile_repairs_total counter\n",
            "rustchan_media_reconcile_repairs_total {}\n",
            "# TYPE rustchan_media_reconcile_repair_conflicts_total counter\n",
            "rustchan_media_reconcile_repair_conflicts_total {}\n",
            "# TYPE rustchan_media_reconcile_scan_incomplete_total counter\n",
            "rustchan_media_reconcile_scan_incomplete_total {}\n",
            "# TYPE rustchan_database_schema_valid gauge\n",
            "rustchan_database_schema_valid{{version=\"{}\"}} {}\n",
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
        media_reconcile.files_scanned_total,
        media_reconcile.references_scanned_total,
        media_reconcile.missing_references_total,
        media_reconcile.safe_orphan_bytes_total,
        media_reconcile.ambiguous_files_total,
        media_reconcile.repairs_total,
        media_reconcile.repair_conflicts_total,
        media_reconcile.incomplete_scans_total,
        crate::db::baseline_schema_version(),
        u8::from(database_schema_valid),
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

#[cfg(test)]
/// Readiness and metrics response-contract tests.
mod tests {
    use super::{metrics_response, readyz_response};
    use axum::{body::to_bytes, http::StatusCode};

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Hides operational readiness fields in the public response.
    async fn public_readyz_response_hides_operational_details() -> anyhow::Result<()> {
        let response = readyz_response(crate::test_support::app_state(), false).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "healthy test state should report ready"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(
            body.get("status").and_then(serde_json::Value::as_str),
            Some("ready"),
            "public readiness should include the aggregate status"
        );
        for field in [
            "database_schema_version",
            "database_schema_valid",
            "worker_queue_pending",
            "media_processing_failed",
            "latest_full_backup_verified",
            "tor_enabled",
        ] {
            assert!(
                body.get(field).is_none(),
                "public readiness should hide {field}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Includes operational readiness fields when details are enabled.
    async fn detailed_readyz_response_remains_available_when_enabled() -> anyhow::Result<()> {
        let response = readyz_response(crate::test_support::app_state(), true).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "healthy detailed state should report ready"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(
            body.get("status").and_then(serde_json::Value::as_str),
            Some("ready"),
            "detailed readiness should include the aggregate status"
        );
        assert_eq!(
            body.get("database_schema_version")
                .and_then(serde_json::Value::as_str),
            Some(crate::db::baseline_schema_version()),
            "detailed readiness should include the expected schema version"
        );
        assert_eq!(
            body.get("database_schema_valid")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "test schema should be valid"
        );
        for field in [
            "worker_queue_pending",
            "latest_full_backup_verified",
            "tor_enabled",
        ] {
            assert!(
                body.get(field).is_some(),
                "detailed readiness should include {field}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Emits the core metric families for enabled scrapers.
    async fn metrics_response_remains_available_for_enabled_scrapers() -> anyhow::Result<()> {
        let response = metrics_response(crate::test_support::app_state()).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "metrics response should succeed"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body = String::from_utf8(body.to_vec())?;
        let schema_metric = format!(
            "rustchan_database_schema_valid{{version=\"{}\"}} 1",
            crate::db::baseline_schema_version()
        );

        for metric in [
            "rustchan_requests_total",
            "rustchan_job_queue_pending",
            "rustchan_media_reconcile_files_scanned_total",
            "rustchan_media_reconcile_repair_conflicts_total",
            schema_metric.as_str(),
        ] {
            assert!(
                body.contains(metric),
                "metrics response should contain {metric}"
            );
        }
        Ok(())
    }
}
