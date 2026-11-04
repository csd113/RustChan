//! Concrete background tasks owned and drained by the parent server lifecycle.

use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

use crate::config::CONFIG;
use crate::db::DbPool;
use crate::media::reconcile::ReconcileCursor;
use crate::middleware::{data_access::DataAccess, AppState, MaintenanceGate};
use crate::workers::JobQueue;

use super::{
    configured_timer_interval, should_run_background_maintenance, ACTIVE_IPS, ACTIVE_UPLOADS,
    IN_FLIGHT,
};

/// Initial retry delay for a failed scheduled full backup.
const SCHEDULED_FULL_BACKUP_RETRY_BASE_SECS: u64 = 15 * 60;
/// Maximum retry delay for a failed scheduled full backup.
const SCHEDULED_FULL_BACKUP_RETRY_MAX_SECS: u64 = 6 * 60 * 60;

/// Calculate the capped exponential retry delay after a scheduled backup failure.
fn scheduled_full_backup_failure_retry_delay(
    backup_interval: Duration,
    failure_streak: u32,
) -> Duration {
    let attempt_shift = failure_streak.saturating_sub(1).min(16);
    let multiplier = 1u64.checked_shl(attempt_shift).unwrap_or(u64::MAX);
    let attempt_secs = SCHEDULED_FULL_BACKUP_RETRY_BASE_SECS.saturating_mul(multiplier);
    let capped_secs = backup_interval.as_secs().clamp(
        SCHEDULED_FULL_BACKUP_RETRY_BASE_SECS,
        SCHEDULED_FULL_BACKUP_RETRY_MAX_SECS,
    );
    Duration::from_secs(attempt_secs.min(capped_secs))
}

/// Run automatic saved full backups with live settings and capped failure retries.
#[expect(
    clippy::too_many_lines,
    reason = "keep scheduled backup timing, admission, and retry state together in one owned task"
)]
pub(super) async fn run_scheduled_full_backups(
    bg: DbPool,
    maintenance_state: AppState,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
) {
    let scheduler_started_at = SystemTime::now();
    let mut failure_streak = 0u32;
    let mut retry_not_before: Option<SystemTime> = None;
    tokio::select! {
        () = tokio::time::sleep(Duration::from_mins(1)) => {}
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(Duration::from_mins(1));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let settings = maintenance_state.auto_full_backup_settings.snapshot();
                if settings.interval_hours == 0 {
                    failure_streak = 0;
                    retry_not_before = None;
                    continue;
                }

                let interval = match configured_timer_interval(settings.interval_hours, 3600, "automatic full backup") {
                    Ok(interval) => interval,
                    Err(error) => {
                        tracing::error!(target: "admin", %error, "Scheduled full backup interval is invalid");
                        continue;
                    }
                };
                let last_saved_at = crate::handlers::admin::latest_verified_full_backup_modified_time()
                    .unwrap_or(scheduler_started_at);
                let due = SystemTime::now()
                    .duration_since(last_saved_at)
                    .is_ok_and(|elapsed| elapsed >= interval);
                if !due {
                    failure_streak = 0;
                    retry_not_before = None;
                    continue;
                }
                if retry_not_before.is_some_and(|not_before| SystemTime::now() < not_before) {
                    continue;
                }
                if !should_run_background_maintenance(&maintenance_state, 0) {
                    tracing::info!(
                        target: "admin",
                        in_flight = IN_FLIGHT.load(Ordering::Relaxed),
                        active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed),
                        maintenance_active = maintenance_state.maintenance_gate.is_active(),
                        "Skipping scheduled full backup because the server is busy"
                    );
                    continue;
                }
                let Ok(maintenance_guard) = maintenance_state.maintenance_gate.try_begin("Scheduled full backup") else {
                    tracing::debug!(target: "admin", "Skipping scheduled full backup because maintenance is already running");
                    continue;
                };

                let bg2 = bg.clone();
                let progress = Arc::clone(&maintenance_state.backup_progress);
                let Some(exclusive_access) = data_access.exclusive().await else { continue; };
                let attempt_result = tokio::task::spawn_blocking(move || {
                    let _exclusive_access = exclusive_access;
                    let _maintenance_guard = maintenance_guard;
                    let storage_mode = crate::handlers::admin::backup::parse_backup_storage_mode_value(
                        Some(&settings.storage_mode),
                    )?;
                    crate::handlers::admin::create_full_backup_to_server(
                        &bg2,
                        None,
                        &progress,
                        settings.copies_to_keep,
                        settings.include_tor_hidden_service_keys,
                        storage_mode,
                        settings.split_zip_part_size,
                    )
                })
                .await;
                match attempt_result {
                    Ok(Ok(filename)) => {
                        *maintenance_state.backup_progress.automatic_outcome.lock() = Some((true, Instant::now()));
                        failure_streak = 0;
                        retry_not_before = None;
                        tracing::info!(
                            target: "admin",
                            filename = %filename,
                            interval_hours = settings.interval_hours,
                            copies_to_keep = settings.copies_to_keep,
                            "Scheduled full backup completed"
                        );
                    }
                    Ok(Err(error)) => {
                        *maintenance_state.backup_progress.automatic_outcome.lock() = Some((false, Instant::now()));
                        failure_streak = failure_streak.saturating_add(1);
                        let retry_delay =
                            scheduled_full_backup_failure_retry_delay(interval, failure_streak);
                        retry_not_before = SystemTime::now().checked_add(retry_delay);
                        tracing::warn!(
                            target: "admin",
                            error = %error,
                            interval_hours = settings.interval_hours,
                            copies_to_keep = settings.copies_to_keep,
                            failure_streak,
                            retry_delay_secs = retry_delay.as_secs(),
                            "Scheduled full backup failed"
                        );
                    }
                    Err(error) => {
                        *maintenance_state.backup_progress.automatic_outcome.lock() = Some((false, Instant::now()));
                        failure_streak = failure_streak.saturating_add(1);
                        let retry_delay =
                            scheduled_full_backup_failure_retry_delay(interval, failure_streak);
                        retry_not_before = SystemTime::now().checked_add(retry_delay);
                        tracing::warn!(
                            target: "admin",
                            error = %error,
                            failure_streak,
                            retry_delay_secs = retry_delay.as_secs(),
                            "Scheduled full backup task join failed"
                        );
                    }
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Scheduled full-backup task shutting down");
                return;
            }
        }
    }
}

/// Purge expired sessions hourly while retaining shared admission in blocking work.
pub(super) async fn run_session_purge(
    bg: DbPool,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
) {
    let mut iv = tokio::time::interval(Duration::from_hours(1));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let purge_pool = bg.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let result = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
                    let _access = access;
                    let conn = purge_pool.get()?;
                    crate::db::purge_expired_sessions(&conn)
                }).await;
                match result {
                    Ok(Ok(n)) if n > 0 => tracing::info!(target: "sessions", purged = n, "Expired sessions purged"),
                    Ok(Ok(_)) => {},
                    Ok(Err(error)) => tracing::error!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), error = %error, "Session purge error"),
                    Err(error) => tracing::error!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), error = %error, "Session purge blocking task failed"),
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Session purge task shutting down");
                return;
            }
        }
    }
}

/// Prune stale entries from the parent server's shared active-IP map.
pub(super) async fn run_active_ip_prune(cancel_clone: CancellationToken) {
    let mut iv = tokio::time::interval(Duration::from_mins(5));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let cutoff = Instant::now()
                    .checked_sub(Duration::from_mins(5))
                    .unwrap_or_else(Instant::now);
                ACTIVE_IPS.retain(|_, last_seen| *last_seen > cutoff);
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Active IP prune task shutting down");
                return;
            }
        }
    }
}

/// Run the existing administrator login-failure pruning policy every five minutes.
pub(super) async fn run_login_fail_prune(cancel_clone: CancellationToken) {
    let mut iv = tokio::time::interval(Duration::from_mins(5));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                crate::handlers::admin::prune_login_fails();
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Login fail prune task shutting down");
                return;
            }
        }
    }
}

/// Run staggered WAL checkpoint and planner maintenance on one pooled connection.
pub(super) async fn run_wal_maintenance(
    bg: DbPool,
    maintenance_state: AppState,
    wal_interval: Duration,
    interval_secs: u64,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
) {
    // Stagger the first run by half the interval so it doesn't fire
    // immediately at startup alongside the session purge.
    tokio::select! {
        () = tokio::time::sleep(Duration::from_secs((interval_secs / 2).saturating_add(1))) => {}
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(wal_interval);
    loop {
        tokio::select! {
            _ = iv.tick() => {
                if !should_run_background_maintenance(&maintenance_state, 3) {
                    tracing::debug!(
                        target: "db",
                        in_flight = IN_FLIGHT.load(Ordering::Relaxed),
                        active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed),
                        maintenance_active = maintenance_state.maintenance_gate.is_active(),
                        "Skipping WAL checkpoint while server is busy"
                    );
                    continue;
                }
                let checkpoint_pool = bg.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let _access = access;
                    let conn = checkpoint_pool.get()?;
                    let (log_pages, checkpointed_pages, busy) = crate::db::run_wal_checkpoint(&conn)?;
                    tracing::debug!(target: "db", log_pages, checkpointed_pages, busy,
                        "WAL checkpoint completed");
                    // Reuse the connection so a one-connection pool cannot
                    // deadlock on a nested checkout. Both operations can
                    // wait for SQLite and must stay off async workers.
                    conn.execute_batch("PRAGMA optimize;")?;
                    Ok(())
                }).await;
                match result {
                    Ok(Ok(())) => {},
                    Ok(Err(error)) => tracing::warn!(target: "db", error = %error, "WAL checkpoint or planner maintenance failed"),
                    Err(error) => tracing::warn!(target: "db", error = %error, "WAL maintenance blocking task failed"),
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "WAL checkpoint task shutting down");
                return;
            }
        }
    }
}

/// Run staggered scheduled VACUUM while retaining its separate maintenance guard.
pub(super) async fn run_scheduled_vacuum(
    bg: DbPool,
    maintenance_state: AppState,
    vacuum_interval: Duration,
    interval_secs: u64,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
) {
    // Stagger the first run by half the interval to avoid hammering the
    // DB immediately at startup alongside WAL checkpoint and session purge.
    tokio::select! {
        () = tokio::time::sleep(Duration::from_secs((interval_secs / 2).saturating_add(7))) => {}
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(vacuum_interval);
    loop {
        tokio::select! {
            _ = iv.tick() => {
                if !should_run_background_maintenance(&maintenance_state, 0) {
                    tracing::info!(
                        target: "db",
                        in_flight = IN_FLIGHT.load(Ordering::Relaxed),
                        active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed),
                        maintenance_active = maintenance_state.maintenance_gate.is_active(),
                        "Skipping scheduled VACUUM because the server is busy"
                    );
                    continue;
                }
                let Ok(maintenance_guard) = maintenance_state.maintenance_gate.try_begin("Scheduled VACUUM") else {
                    tracing::debug!(target: "db", "Skipping scheduled VACUUM because maintenance is already running");
                    continue;
                };
                let bg2 = bg.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let task_result = tokio::task::spawn_blocking(move || {
                    let access_guard = access;
                    let _maintenance_guard = maintenance_guard;
                    if let Ok(conn) = bg2.get() {
                        let before = crate::db::get_db_size_bytes(&conn).unwrap_or(0);
                        match crate::db::run_vacuum(&conn) {
                            Ok(()) => {
                                let after = crate::db::get_db_size_bytes(&conn).unwrap_or(0);
                                let saved = before.saturating_sub(after);
                                tracing::info!(
                                    target: "db",
                                    before_bytes = before,
                                    after_bytes  = after,
                                    saved_bytes  = saved,
                                    "VACUUM complete"
                                );
                            }
                            Err(e) => tracing::warn!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Scheduled VACUUM failed: {e}"),
                        }
                    }
                    drop(access_guard);
                })
                .await;
                if let Err(error) = task_result {
                    tracing::warn!(
                        target: "db",
                        error = %error,
                        "Scheduled VACUUM blocking task failed"
                    );
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "VACUUM task shutting down");
                return;
            }
        }
    }
}

/// Reconcile bounded media job and post pages from the startup cursors.
pub(super) async fn run_worker_media_reconciliation(
    reconcile_pool: DbPool,
    reconcile_queue: Arc<JobQueue>,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
    next_job_id: i64,
    next_post_id: i64,
) {
    let mut job_cursor = next_job_id;
    let mut post_cursor = next_post_id;
    let mut interval = tokio::time::interval(Duration::from_mins(1));
    let _completed_value = interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let pass_pool = reconcile_pool.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let pass = tokio::task::spawn_blocking(move || {
                    let _access = access;
                    crate::workers::reconcile_media_job_states(
                        &pass_pool,
                        job_cursor,
                        post_cursor,
                        crate::workers::MEDIA_RECONCILE_BATCH,
                    )
                }).await;
                match pass {
                    Ok(Ok(report)) => {
                        job_cursor = report.next_job_id;
                        post_cursor = report.next_post_id;
                        if report.jobs_resolved > 0 || report.posts_repaired > 0 {
                            tracing::info!(
                                target: "workers",
                                jobs_scanned = report.jobs_scanned,
                                posts_scanned = report.posts_scanned,
                                jobs_resolved = report.jobs_resolved,
                                posts_repaired = report.posts_repaired,
                                malformed_jobs = report.malformed_jobs,
                                "periodic media reconciliation repaired legacy state"
                            );
                            reconcile_queue.notify.notify_waiters();
                        }
                    }
                    Ok(Err(error)) => tracing::error!(
                        target: "workers",
                        error = %error,
                        "periodic media reconciliation failed"
                    ),
                    Err(error) => tracing::error!(
                        target: "workers",
                        error = %error,
                        "periodic media reconciliation task failed"
                    ),
                }
            }
            () = cancel_clone.cancelled() => break,
        }
    }
}

/// Audit one bounded managed-media page per configured interval.
pub(super) async fn run_managed_media_audit(
    reconcile_pool: DbPool,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
    media_reconcile_interval: Duration,
    startup_managed_media_cursor: ReconcileCursor,
) {
    let mut cursor = startup_managed_media_cursor;
    let mut interval = tokio::time::interval(media_reconcile_interval);
    let _completed_value = interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let pass_pool = reconcile_pool.clone();
                let upload_dir = CONFIG.upload_dir.clone();
                let pass_cursor = cursor.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let pass = tokio::task::spawn_blocking(move || {
                    let _access = access;
                    crate::media::reconcile::reconcile_managed_media(
                        &pass_pool,
                        &upload_dir,
                        crate::media::reconcile::configured_mode(),
                        &pass_cursor,
                        crate::media::reconcile::configured_limits(),
                    )
                }).await;
                match pass {
                    Ok(Ok(report)) => cursor = report.next_cursor,
                    Ok(Err(error)) => tracing::warn!(
                        target: "media_reconcile",
                        error = %error,
                        "periodic managed-media audit failed"
                    ),
                    Err(error) => tracing::warn!(
                        target: "media_reconcile",
                        error = %error,
                        "periodic managed-media audit task failed"
                    ),
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(
                    target: "media_reconcile",
                    "managed-media audit task shutting down"
                );
                return;
            }
        }
    }
}

/// Reconcile bounded thread-retention pages from the startup board cursor.
pub(super) async fn run_thread_retention_reconciliation(
    reconcile_pool: DbPool,
    reconcile_queue: Arc<JobQueue>,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
    next_board_id: i64,
) {
    let mut cursor = next_board_id;
    let mut interval = tokio::time::interval(Duration::from_mins(1));
    let _completed_value = interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let pass_pool = reconcile_pool.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let pass = tokio::task::spawn_blocking(move || {
                    let _access = access;
                    crate::workers::reconcile_thread_prune_intents(
                        &pass_pool,
                        cursor,
                        crate::workers::THREAD_PRUNE_RECONCILE_BATCH,
                    )
                }).await;
                match pass {
                    Ok(Ok(report)) => {
                        cursor = report.next_board_id;
                        if report.inserted > 0 {
                            reconcile_queue.notify.notify_waiters();
                        }
                        if report.failures > 0 {
                            tracing::warn!(
                                target: "workers",
                                boards_scanned = report.scanned,
                                failures = report.failures,
                                "periodic thread-retention reconciliation had board failures"
                            );
                        }
                    }
                    Ok(Err(error)) => tracing::error!(
                        target: "workers",
                        error = %error,
                        "periodic thread-retention reconciliation failed"
                    ),
                    Err(error) => tracing::error!(
                        target: "workers",
                        error = %error,
                        "periodic thread-retention reconciliation task failed"
                    ),
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Thread-retention reconciliation task shutting down");
                return;
            }
        }
    }
}

/// Purge expired poll votes after the initial delay with shared data admission.
pub(super) async fn run_poll_vote_cleanup(
    bg: DbPool,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
    poll_cleanup_interval: Duration,
    poll_retention_cutoff_secs: i64,
) {
    tokio::select! {
        () = tokio::time::sleep(Duration::from_mins(10)) => {} // initial delay
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(poll_cleanup_interval);
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let bg2 = bg.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let task_result = tokio::task::spawn_blocking(move || {
                    let access_guard = access;
                    if let Ok(conn) = bg2.get() {
                        let cutoff = chrono::Utc::now().timestamp().saturating_sub(poll_retention_cutoff_secs);
                        match crate::db::cleanup_expired_poll_votes(&conn, cutoff) {
                            Ok(n) if n > 0 => {
                                tracing::info!(target: "polls", removed = n, "Expired poll vote rows purged");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Poll vote cleanup failed: {e}"),
                        }
                    }
                    drop(access_guard);
                })
                .await;
                if let Err(error) = task_result {
                    tracing::warn!(
                        target: "polls",
                        error = %error,
                        "Poll vote cleanup blocking task failed"
                    );
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Poll vote cleanup task shutting down");
                return;
            }
        }
    }
}

/// Replay the finite pending-filesystem queue under maintenance and exclusive access.
pub(super) async fn run_pending_fs_replay(
    bg: DbPool,
    cancel_clone: CancellationToken,
    data_access: DataAccess,
    maintenance_gate: MaintenanceGate,
) {
    tokio::select! {
        () = tokio::time::sleep(Duration::from_mins(5)) => {}
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(Duration::from_mins(15));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let retry_pool = bg.clone();
                let upload_dir = CONFIG.upload_dir.clone();
                let Ok(maintenance) = maintenance_gate.try_begin("Filesystem reconciliation") else { continue; };
                let Some(access) = data_access.exclusive().await else { continue; };
                let task_result = tokio::task::spawn_blocking(move || {
                    let access_guard = access;
                    let _maintenance = maintenance;
                    if let Err(error) = crate::pending_fs::reconcile_pending_fs_ops(
                        &retry_pool,
                        &upload_dir,
                    ) {
                        tracing::warn!(
                            target: "pending_fs",
                            error = %error,
                            "periodic filesystem reconciliation left quarantined operations"
                        );
                    }
                    drop(access_guard);
                })
                .await;
                if let Err(error) = task_result {
                    tracing::warn!(
                        target: "pending_fs",
                        error = %error,
                        "Periodic filesystem reconciliation blocking task failed"
                    );
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Filesystem reconciliation task shutting down");
                return;
            }
        }
    }
}

/// Evict unreferenced thumbnail and waveform cache files with shared data admission.
pub(super) async fn run_thumb_cache_eviction(
    max_bytes: u64,
    cancel_clone: CancellationToken,
    bg: DbPool,
    data_access: DataAccess,
) {
    tokio::select! {
        () = tokio::time::sleep(Duration::from_mins(30)) => {} // initial stagger
        () = cancel_clone.cancelled() => { return; }
    }
    let mut iv = tokio::time::interval(Duration::from_hours(1));
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let upload_dir = CONFIG.upload_dir.clone();
                let eviction_pool = bg.clone();
                let Some(access) = data_access.try_read() else { continue; };
                let task_result = tokio::task::spawn_blocking(move || {
                    let _access = access;
                    let Ok(conn) = eviction_pool.get() else {
                        return;
                    };
                    let eviction_result = crate::workers::evict_thumb_cache(
                        &conn,
                        &upload_dir,
                        max_bytes,
                    );
                    if let Err(error) = eviction_result {
                        tracing::warn!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), error = %error, "Thumbnail cache eviction failed");
                    }
                })
                .await;
                if let Err(error) = task_result {
                    tracing::warn!(
                        target: "workers",
                        error = %error,
                        "Thumbnail cache eviction blocking task failed"
                    );
                }
            }
            () = cancel_clone.cancelled() => {
                tracing::debug!(target: concat!(env!("CARGO_CRATE_NAME"), "::server::server"), "Waveform cache eviction task shutting down");
                return;
            }
        }
    }
}
