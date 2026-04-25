// server/server.rs — HTTP server runtime.
//
// Contains:
//   • Global request-counter atomics (REQUEST_COUNT, IN_FLIGHT, etc.)
//   • ScopedDecrement RAII guard
//   • run_server()            — full server startup sequence
//   • build_router()          — Axum router wiring
//   • spawn background tasks  — session purge, WAL checkpoint, IP prune,
//                               login-fail prune, VACUUM, poll cleanup,
//                               thumb-cache eviction
//   • Static asset handlers   — serve_css, serve_main_js, serve_theme_init_js
//   • track_requests          — per-request counter middleware
//   • hsts_middleware         — HSTS header (HTTPS-only)
//   • shutdown_signal()       — Ctrl-C / SIGTERM waiter

use axum::{
    http::header,
    response::{IntoResponse, Redirect},
};
use dashmap::DashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use crate::config::{
    check_cookie_secret_rotation, data_dir, generate_settings_file_if_missing,
    migrate_runtime_layout_if_needed, CONFIG,
};
use crate::middleware::AppState;

mod assets;
mod headers;
mod lifecycle;
mod observability;
mod router;

use lifecycle::shutdown_signal;
use router::build_router;

fn should_run_background_maintenance(state: &AppState, max_in_flight: u64) -> bool {
    !state.maintenance_gate.is_active()
        && ACTIVE_UPLOADS.load(Ordering::Relaxed) == 0
        && IN_FLIGHT.load(Ordering::Relaxed) <= max_in_flight
}

const SCHEDULED_FULL_BACKUP_RETRY_BASE_SECS: u64 = 15 * 60;
const SCHEDULED_FULL_BACKUP_RETRY_MAX_SECS: u64 = 6 * 60 * 60;

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

// ─── Global terminal state ─────────────────────────────────────────────────────
/// Total HTTP requests handled since startup.
pub static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
/// Requests currently being processed (in-flight).
///
/// Changed from `AtomicI64` to `AtomicU64`.  In-flight request
/// counts are inherently non-negative; using a signed type required defensive
/// `.max(0)` casts at every read site and masked counter underflow bugs.
/// Decrements use `ScopedDecrement` RAII guards (see below) to prevent
/// counter leaks when async futures are cancelled mid-flight.
pub static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
/// Multipart file uploads currently in progress.
///
/// Same signed→unsigned change as `IN_FLIGHT`.
pub static ACTIVE_UPLOADS: AtomicU64 = AtomicU64::new(0);
/// Monotonic tick used to animate the upload spinner.
pub static SPINNER_TICK: AtomicU64 = AtomicU64::new(0);
/// Recently active client IPs (last ~5 min); maps SHA-256(IP) → last-seen Instant.
/// memory (or coredumps). The count is used for the "users online" display.
pub static ACTIVE_IPS: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);

// ─── RAII counter guard ───────────────────────────────────────────────────────
//
// `IN_FLIGHT` and `ACTIVE_UPLOADS` are decremented inside
// `track_requests` *after* `.await`.  If the surrounding future is cancelled
// (e.g. client disconnect, timeout, or panic in a handler), the post-await
// code never runs and the counters permanently over-count.
//
// `ScopedDecrement` ties the decrement to the guard's lifetime so it fires
// unconditionally via `Drop`, even when the future is dropped mid-flight.
// The decrement is saturating to prevent underflow on `AtomicU64`.
pub(super) struct ScopedDecrement<'a>(pub(super) &'a AtomicU64);

impl Drop for ScopedDecrement<'_> {
    fn drop(&mut self) {
        // Saturating decrement: fetch_update retries on spurious failure.
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

// ─── Server mode ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub async fn run_server(port_override: Option<u16>, chan_net: bool) -> anyhow::Result<()> {
    // rustls 0.23 requires an explicit process-wide crypto provider.
    // install_default() is idempotent — a second call (e.g. in tests) returns
    // Err but never panics, so the let _ discard is intentional.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let early_data_dir = data_dir();
    std::fs::create_dir_all(&early_data_dir)?;
    migrate_runtime_layout_if_needed()?;

    generate_settings_file_if_missing();

    // Validate critical configuration values immediately — fail fast with a
    // clear error rather than discovering misconfiguration at runtime (#8).
    CONFIG.validate()?;

    let data_dir = super::parent_dir_or_current(std::path::Path::new(&CONFIG.database_path));

    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&CONFIG.upload_dir)?;

    let bind_addr: String = port_override.map_or_else(
        || CONFIG.bind_addr.clone(),
        |p| CONFIG.bind_addr_with_port(p),
    );

    let pool = crate::db::init_pool()?;
    crate::db::first_run_check(&pool)?;
    {
        let reconcile_pool = pool.clone();
        let upload_dir = CONFIG.upload_dir.clone();
        tokio::task::spawn_blocking(move || {
            crate::pending_fs::reconcile_pending_fs_ops(&reconcile_pool, &upload_dir)
        })
        .await
        .map_err(|error| anyhow::anyhow!("pending_fs_ops reconciliation task failed: {error}"))??;
    }

    // Check whether cookie_secret has changed since the last run (#19).
    // Must run after DB init so the site_settings table exists.
    if let Ok(conn) = pool.get() {
        check_cookie_secret_rotation(&conn);
    }

    // Initialise the live site name and subtitle from DB so they're available before any request.
    {
        if let Ok(conn) = pool.get() {
            // Site name: use DB value if an admin has set one, otherwise seed
            // from CONFIG.forum_name (settings.toml).  Using get_site_setting
            // (not get_site_name) lets us distinguish "never set" from "set to
            // the default", so that editing forum_name in settings.toml and
            // restarting takes effect when no admin override is in the DB.
            let name_in_db = crate::db::get_site_setting(&conn, "site_name")
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty());
            let name = name_in_db.unwrap_or_else(|| {
                // Seed DB from settings.toml so get_site_name is always consistent.
                let _ = crate::db::set_site_setting(&conn, "site_name", &CONFIG.forum_name);
                CONFIG.forum_name.clone()
            });
            crate::templates::set_live_site_name(&name);

            // Seed subtitle from settings.toml if not yet configured in DB.
            // BUG FIX: get_site_subtitle() always returns a non-empty fallback
            // string, so we must query the DB key directly to detect "never set".
            let subtitle_in_db = crate::db::get_site_setting(&conn, "site_subtitle")
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty());
            let subtitle = subtitle_in_db.unwrap_or_else(|| {
                // Nothing in DB — seed from CONFIG (settings.toml).
                let seed = if CONFIG.initial_site_subtitle.is_empty() {
                    "select board to proceed".to_string()
                } else {
                    CONFIG.initial_site_subtitle.clone()
                };
                let _ = crate::db::set_site_setting(&conn, "site_subtitle", &seed);
                seed
            });
            crate::templates::set_live_site_subtitle(&subtitle);

            if crate::db::get_site_setting(&conn, "new_activity_notifications_enabled")
                .ok()
                .flatten()
                .is_none()
            {
                let _ = crate::db::set_site_setting(
                    &conn,
                    "new_activity_notifications_enabled",
                    if CONFIG.initial_new_activity_notifications_enabled {
                        "1"
                    } else {
                        "0"
                    },
                );
            }

            seed_initial_default_theme(&conn, &CONFIG.initial_default_theme);
            let _ = crate::db::sync_live_theme_state(&conn);

            // Seed the live board list used by error pages and ban pages.
            if let Ok(boards) = crate::db::get_all_boards(&conn) {
                crate::templates::set_live_boards(boards);
            }
        }
    }
    // ── External tool detection ────────────────────────────────────────────────
    // ffmpeg: required for video thumbnails (optional — graceful degradation).
    let ffmpeg_status = crate::detect::detect_ffmpeg(CONFIG.require_ffmpeg);
    let ffmpeg_available = ffmpeg_status == crate::detect::ToolStatus::Available;
    // ffprobe is used lazily for WebM codec inspection, so probe it at startup
    // to make explicit configured paths authoritative and catch bogus paths early.
    let _ffprobe_available = crate::detect::detect_ffprobe();
    // libwebp encoder: needed for image→WebP conversion.  Checked independently
    // so that a stock ffmpeg build (missing libwebp) still enables video/audio
    // features while image conversion degrades gracefully.
    let ffmpeg_webp_available = crate::detect::detect_webp_encoder(ffmpeg_available);
    // libvpx-vp9 + libopus encoders: needed for MP4→WebM transcoding and
    // WebM/AV1→VP9 re-encoding.  Checked independently so that a build missing
    // only these codecs still enables image conversion and thumbnail generation.
    let ffmpeg_vp9_available = crate::detect::detect_webm_encoder(ffmpeg_available);

    // Derive bind_port from `bind_addr` (which already incorporates port_override).
    // rsplit_once(':') handles both IPv4 ("0.0.0.0:9000") and IPv6 ("[::1]:9000").
    // F-07: Log a warning if parsing fails so the operator knows Tor proxy is
    // using a fallback port that may not match the actual HTTP listener.
    let bind_port = bind_addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or_else(|| {
            tracing::warn!(
                target: "server",
                bind_addr = %bind_addr,
                fallback = 8080,
                "Could not parse port from bind_addr — Tor proxy will use port 8080"
            );
            8080
        });
    // sequence can await each worker instead of blindly sleeping for 10 s.
    // Previously the return value was silently discarded, making it impossible
    // to know whether in-flight jobs had finished before the process exited.
    let worker_queue = std::sync::Arc::new(crate::workers::JobQueue::new(pool.clone()));
    let worker_handles =
        crate::workers::start_worker_pool(&worker_queue, ffmpeg_available, ffmpeg_vp9_available);

    let chan_ledger = if chan_net {
        let conn = pool.get()?;
        let ledger = crate::db::chan_net::load_import_ledger(&conn)?
            .into_iter()
            .collect::<crate::chan_net::ledger::TxLedger>();
        Some(std::sync::Arc::new(parking_lot::Mutex::new(ledger)))
    } else {
        None
    };

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
        ffmpeg_webp_available,
        job_queue: worker_queue,
        backup_progress: std::sync::Arc::new(crate::middleware::BackupProgress::new()),
        auto_full_backup_settings: crate::middleware::AutoFullBackupSettings::new(
            CONFIG.auto_full_backup_interval_hours,
            CONFIG.auto_full_backup_copies_to_keep,
            CONFIG.auto_full_backup_include_tor_hidden_service_keys,
        ),
        maintenance_gate: crate::middleware::MaintenanceGate::new(),
        db_maintenance_jobs: crate::middleware::DbMaintenanceJobs::new(),
        chan_ledger,
        onion_address: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
    };

    // worker_cancel is the shutdown token threaded through all background tasks
    // and the Tor task. Declared here so it is available to background tasks
    // spawned below. detect_tor is called later, after the first-run wizard.
    let worker_cancel = state.job_queue.cancel.clone();
    let start_time = Instant::now();

    // Background: purge expired sessions hourly
    {
        let bg = pool.clone();
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        if let Ok(conn) = bg.get() {
                            match crate::db::purge_expired_sessions(&conn) {
                                Ok(n) if n > 0 => {
                                    tracing::info!(target: "sessions", purged = n, "Expired sessions purged");
                                }
                                Err(e) => tracing::error!("Session purge error: {e}"),
                                Ok(_) => {}
                            }
                        }
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Session purge task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // Background: WAL checkpoint — prevent WAL files growing unbounded.
    // Runs PRAGMA wal_checkpoint(TRUNCATE) at the configured interval, plus
    // PRAGMA optimize to keep query-planner statistics current (#18).
    if CONFIG.wal_checkpoint_interval > 0 {
        let bg = pool.clone();
        let maintenance_state = state.clone();
        let interval_secs = CONFIG.wal_checkpoint_interval;
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            // Stagger the first run by half the interval so it doesn't fire
            // immediately at startup alongside the session purge.
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(interval_secs / 2 + 1)) => {}
                () = cancel_clone.cancelled() => { return; }
            }
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
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
                        if let Ok(conn) = bg.get() {
                            match crate::db::run_wal_checkpoint(&conn) {
                                Ok((pages, moved, backfill)) => {
                                    tracing::debug!("WAL checkpoint: {pages} pages total, {moved} moved, {backfill} backfilled");
                                }
                                Err(e) => tracing::warn!("WAL checkpoint failed: {e}"),
                            }
                            // Fix #7: reuse `conn` instead of calling bg.get() again.
                            // A second acquire while the first is still alive deadlocks
                            // with a pool size of 1.
                            let _ = conn.execute_batch("PRAGMA optimize;");
                        }
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("WAL checkpoint task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // Background: prune stale IPs from ACTIVE_IPS every 5 min
    {
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        let cutoff = Instant::now()
                            .checked_sub(Duration::from_secs(300))
                            .unwrap_or_else(Instant::now);
                        ACTIVE_IPS.retain(|_, last_seen| *last_seen > cutoff);
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Active IP prune task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // Background: prune expired entries from ADMIN_LOGIN_FAILS every 5 min.
    // Prevents unbounded growth under a sustained brute-force attack that
    // never produces a successful login (which would trigger the existing
    // opportunistic prune path inside clear_login_fails).
    {
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(300));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        crate::handlers::admin::prune_login_fails();
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Login fail prune task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // 1.6: Scheduled database VACUUM — reclaim disk space from deleted posts
    // and threads without requiring manual admin intervention.
    if CONFIG.auto_vacuum_interval_hours > 0 {
        let bg = pool.clone();
        let maintenance_state = state.clone();
        let interval_secs = CONFIG.auto_vacuum_interval_hours * 3600;
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            // Stagger the first run by half the interval to avoid hammering the
            // DB immediately at startup alongside WAL checkpoint and session purge.
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(interval_secs / 2 + 7)) => {}
                () = cancel_clone.cancelled() => { return; }
            }
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
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
                        let Ok(_guard) = maintenance_state.maintenance_gate.try_begin("Scheduled VACUUM") else {
                            tracing::debug!(target: "db", "Skipping scheduled VACUUM because maintenance is already running");
                            continue;
                        };
                        let bg2 = bg.clone();
                        tokio::task::spawn_blocking(move || {
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
                                    Err(e) => tracing::warn!("Scheduled VACUUM failed: {e}"),
                                }
                            }
                        })
                        .await
                        .ok();
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("VACUUM task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // Automatic saved full backups — creates a server-side full backup on the
    // configured cadence and trims older saved full backups to the configured
    // retention limit after each successful save.
    {
        let bg = pool.clone();
        let maintenance_state = state.clone();
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let scheduler_started_at = SystemTime::now();
            let mut failure_streak = 0u32;
            let mut retry_not_before: Option<SystemTime> = None;
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(60)) => {}
                () = cancel_clone.cancelled() => { return; }
            }
            let mut iv = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        let settings = maintenance_state.auto_full_backup_settings.snapshot();
                        if settings.interval_hours == 0 {
                            failure_streak = 0;
                            retry_not_before = None;
                            continue;
                        }

                        let interval = Duration::from_secs(settings.interval_hours.saturating_mul(3600));
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
                        let Ok(_guard) = maintenance_state.maintenance_gate.try_begin("Scheduled full backup") else {
                            tracing::debug!(target: "admin", "Skipping scheduled full backup because maintenance is already running");
                            continue;
                        };

                        let bg2 = bg.clone();
                        let progress = maintenance_state.backup_progress.clone();
                        let attempt_result = tokio::task::spawn_blocking(move || {
                            crate::handlers::admin::create_full_backup_to_server(
                                &bg2,
                                None,
                                &progress,
                                settings.copies_to_keep,
                                settings.include_tor_hidden_service_keys,
                            )
                        })
                        .await;
                        match attempt_result {
                            Ok(Ok(filename)) => {
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
                        tracing::debug!("Scheduled full-backup task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // 1.7: Expired poll vote cleanup — purge per-IP vote rows for polls whose
    // expiry is older than poll_cleanup_interval_hours, preventing the
    // poll_votes table from growing indefinitely.
    if CONFIG.poll_cleanup_interval_hours > 0 {
        let bg = pool.clone();
        let interval_secs = CONFIG.poll_cleanup_interval_hours * 3600;
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(600)) => {} // initial delay
                () = cancel_clone.cancelled() => { return; }
            }
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        let bg2 = bg.clone();
                        let retention_cutoff_secs = interval_secs.cast_signed();
                        tokio::task::spawn_blocking(move || {
                            if let Ok(conn) = bg2.get() {
                                let cutoff = chrono::Utc::now().timestamp() - retention_cutoff_secs;
                                match crate::db::cleanup_expired_poll_votes(&conn, cutoff) {
                                    Ok(n) if n > 0 => {
                                        tracing::info!(target: "polls", removed = n, "Expired poll vote rows purged");
                                    }
                                    Ok(_) => {}
                                    Err(e) => tracing::warn!("Poll vote cleanup failed: {e}"),
                                }
                            }
                        })
                        .await
                        .ok();
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Poll vote cleanup task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // 2.6: Waveform/thumbnail cache eviction — keep total size of all thumbs
    // directories under CONFIG.waveform_cache_max_bytes by deleting the oldest
    // files when the threshold is exceeded.  Waveform PNGs can be regenerated
    // by re-enqueueing the AudioWaveform job; image thumbnails can be
    // regenerated from the originals.  Uses 1-hour intervals.
    if CONFIG.waveform_cache_max_bytes > 0 {
        let max_bytes = CONFIG.waveform_cache_max_bytes;
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(1800)) => {} // initial stagger
                () = cancel_clone.cancelled() => { return; }
            }
            let mut iv = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        let upload_dir = CONFIG.upload_dir.clone();
                        tokio::task::spawn_blocking(move || {
                            crate::workers::evict_thumb_cache(&upload_dir, max_bytes);
                        })
                        .await
                        .ok();
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Waveform cache eviction task shutting down");
                        return;
                    }
                }
            }
        });
    }

    let app = build_router(state.clone(), false);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(target: "server", addr = %bind_addr, "HTTP server listening");
    tracing::info!(target: "server", url = %format!("http://{bind_addr}/admin"), "Admin panel");
    tracing::info!(target: "server", path = %data_dir.display(), "Data directory");

    // First-run admin wizard: if no admin accounts exist and stdout is a TTY,
    // prompt interactively before starting the keyboard handler (which also
    // reads stdin).  In non-TTY mode (daemon/systemd) we log a warning instead
    // so the operator knows to use the CLI.
    if crate::db::has_no_admin(&pool) {
        if crate::logging::is_tty() {
            let stdin = std::io::stdin();
            // Acquire and immediately pass the stdin lock to the wizard.
            // The lock is released when `reader` drops at the end of this block,
            // before spawn_keyboard_handler acquires its own stdin lock below.
            let mut reader = std::io::BufReader::new(stdin.lock());
            super::console::prompt_create_first_admin(&pool, &mut reader);
        } else {
            tracing::warn!(
                target: "startup",
                "No admin accounts exist — run: rustchan-cli admin create-admin <username> <password>"
            );
        }
    }

    // ── Full-screen TUI console ───────────────────────────────────────────────
    // Build shared state for the TUI.
    let shared_stats: super::console::SharedStats = std::sync::Arc::new(tokio::sync::RwLock::new(
        super::console::ChanStats::default(),
    ));
    let shared_mode: super::console::SharedConsoleMode = std::sync::Arc::new(
        tokio::sync::RwLock::new(super::console::ConsoleMode::Dashboard),
    );
    // Stats refresh task — polls DB every 3 s (or immediately on [R]).
    // block_in_place keeps &mut delta locals on the same stack frame so
    // req/s and other deltas are correctly accumulated across calls.
    let force_reload_notify = std::sync::Arc::new(tokio::sync::Notify::new());
    {
        let pool_stats = pool.clone();
        let worker_queue_stats = state.job_queue.clone();
        let stats_w = shared_stats.clone();
        let cancel_stats = worker_cancel.clone();
        let onion_addr = state.onion_address.clone();
        let force_reload = force_reload_notify.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            let mut prev_req = REQUEST_COUNT.load(std::sync::atomic::Ordering::Relaxed);
            let mut prev_tick = std::time::Instant::now();
            let mut prev_threads: i64 = 0;
            let mut prev_posts: i64 = 0;
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    () = force_reload.notified() => {
                        interval.reset();
                    }
                    () = cancel_stats.cancelled() => {
                        tracing::debug!("Stats refresh task shutting down");
                        return;
                    }
                }
                let onion = onion_addr.read().await.clone();
                let snap = tokio::task::block_in_place(|| {
                    super::console::collect_stats(
                        &pool_stats,
                        &worker_queue_stats,
                        start_time,
                        &mut prev_req,
                        &mut prev_tick,
                        &mut prev_threads,
                        &mut prev_posts,
                        onion,
                    )
                });
                *stats_w.write().await = snap;
            }
        });
    }

    // Enter the alternate screen BEFORE spawning Tor so Tor bootstrap log
    // events go to the file log rather than scrolling the normal terminal.
    // detect.rs checks is_tui_active() and skips its onion-address banner
    // box — the dashboard shows the address on its next render tick instead.
    let (mut key_rx, _force_reload_render) = super::console::start(&shared_stats, &shared_mode);

    // Tor: spawned after the TUI is up. F-04: handle awaited on shutdown.
    let tor_handle = crate::detect::detect_tor(
        CONFIG.enable_tor_support,
        bind_port,
        &data_dir,
        state.onion_address.clone(),
        worker_cancel.clone(),
    );

    // Event dispatch — translate KeyEvents into mode changes and wizard launches.
    {
        let mode_d = shared_mode.clone();
        let pool_d = pool.clone();
        let cancel_d = worker_cancel.clone();
        let shutdown_tx = worker_cancel.clone();
        let force_reload = force_reload_notify.clone();
        tokio::spawn(async move {
            while let Some(key) = key_rx.recv().await {
                use super::console::input::KeyEvent;
                use super::console::{ConsoleMode, WizardKind};

                let current = mode_d.read().await.clone();

                match key {
                    KeyEvent::Reload => {
                        force_reload.notify_one();
                    }
                    KeyEvent::ToggleLogs => {
                        let next = if current == ConsoleMode::LogView {
                            ConsoleMode::Dashboard
                        } else {
                            ConsoleMode::LogView
                        };
                        *mode_d.write().await = next;
                    }
                    KeyEvent::BoardList => {
                        let next = if current == ConsoleMode::BoardList {
                            ConsoleMode::Dashboard
                        } else {
                            ConsoleMode::BoardList
                        };
                        *mode_d.write().await = next;
                    }
                    KeyEvent::Help => {
                        let next = if current == ConsoleMode::Help {
                            ConsoleMode::Dashboard
                        } else {
                            ConsoleMode::Help
                        };
                        *mode_d.write().await = next;
                    }
                    KeyEvent::Quit => {
                        *mode_d.write().await = ConsoleMode::ConfirmQuit;
                    }
                    KeyEvent::Cancel => {
                        *mode_d.write().await = ConsoleMode::Dashboard;
                    }
                    KeyEvent::Confirm => {
                        if current == ConsoleMode::ConfirmQuit {
                            tracing::info!(target: "server", "Graceful shutdown initiated from console");
                            super::console::cleanup();
                            shutdown_tx.cancel();
                            return;
                        }
                    }
                    KeyEvent::ForceQuit => {
                        tracing::info!(target: "server", "Force quit from console (Ctrl-C)");
                        super::console::cleanup();
                        shutdown_tx.cancel();
                        return;
                    }
                    KeyEvent::CreateBoard => {
                        *mode_d.write().await = ConsoleMode::Wizard(WizardKind::CreateBoard);
                        let pool_w = pool_d.clone();
                        let mode_w = mode_d.clone();
                        tokio::task::spawn_blocking(move || {
                            super::console::wizard::run_wizard(
                                &WizardKind::CreateBoard,
                                &pool_w,
                                &mode_w,
                            );
                        });
                    }
                    KeyEvent::CreateAdmin => {
                        *mode_d.write().await = ConsoleMode::Wizard(WizardKind::CreateAdmin);
                        let pool_w = pool_d.clone();
                        let mode_w = mode_d.clone();
                        tokio::task::spawn_blocking(move || {
                            super::console::wizard::run_wizard(
                                &WizardKind::CreateAdmin,
                                &pool_w,
                                &mode_w,
                            );
                        });
                    }
                    KeyEvent::DeleteThread => {
                        *mode_d.write().await = ConsoleMode::Wizard(WizardKind::DeleteThread);
                        let pool_w = pool_d.clone();
                        let mode_w = mode_d.clone();
                        tokio::task::spawn_blocking(move || {
                            super::console::wizard::run_wizard(
                                &WizardKind::DeleteThread,
                                &pool_w,
                                &mode_w,
                            );
                        });
                    }
                    KeyEvent::Other => {}
                }

                if cancel_d.is_cancelled() {
                    break;
                }
            }
        });
    }

    if chan_net {
        let chan_addr = crate::config::CONFIG.chan_net_bind.clone();
        let chan_app = crate::chan_net::chan_router(state.clone());
        let chan_listener = tokio::net::TcpListener::bind(&chan_addr).await?;
        tracing::info!(target: "chan_net", addr = %chan_addr, "ChanNet API listening");
        // requests are drained before the runtime is dropped. Without this the
        // ChanNet task was detached and forcibly killed on SIGTERM, potentially
        // corrupting a streaming snapshot response mid-transfer.
        tokio::spawn(async move {
            if let Err(e) = axum::serve(chan_listener, chan_app.into_make_service())
                .with_graceful_shutdown(shutdown_signal())
                .await
            {
                tracing::error!(target: "chan_net", error = %e, "ChanNet server error");
            }
        });
    }

    // ── TLS / HTTPS listener ──────────────────────────────────────────────────
    // Spawned as a background task so the HTTP listener below can start
    // immediately. Both share the same AppState (Arc'd internally).
    // build_acceptor() returns None when tls.enabled = false — existing
    // HTTP-only deployments are completely unaffected.
    //
    // The TCP socket is pre-bound here on the *main* task (before
    // spawning) so that any bind failure (port in use, missing CAP_NET_BIND_SERVICE,
    // etc.) is caught immediately with `?` propagation and a clear error message,
    // rather than silently dying inside a spawned future where the error is easy
    // to miss.  The "HTTPS server listening" log is emitted here — after the
    // successful bind — so it is never printed for a socket that wasn't actually
    // bound.  axum_server::from_tcp_rustls accepts the pre-bound std::TcpListener
    // and does not attempt a second bind.
    //
    // build_acceptor failure is now a hard error (return Err) instead
    // of a silent log-and-continue.  If TLS is enabled in config but the acceptor
    // cannot be constructed (missing cert files, bad PEM, permission denied on
    // runtime/tls/dev/, etc.), the process exits with a clear message rather than running
    // silently as HTTP-only with no indication that HTTPS is absent.
    if CONFIG.tls.enabled {
        let data_dir_tls = data_dir.clone();
        let cancel_tls = worker_cancel.clone();
        let app_tls = build_router(state.clone(), true);

        let https_addr: std::net::SocketAddr = CONFIG
            .bind_addr_with_port(CONFIG.tls.port)
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid HTTPS bind address: {e}"))?;

        // propagate build_acceptor errors as hard failures.
        let acceptor = crate::tls::build_acceptor(&CONFIG.tls, &data_dir_tls)
            .map_err(|e| anyhow::anyhow!("TLS init failed — cannot start HTTPS listener: {e}"))?;

        match acceptor {
            Some(crate::tls::Acceptor::Static(_arc_acceptor, server_cfg)) => {
                // Acceptor::Static now carries Arc<ServerConfig> directly alongside
                // the TlsAcceptor — pass it straight to axum-server.
                let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_config(server_cfg);

                // pre-bind on the main task so failures surface here.
                let https_tcp = tokio::net::TcpListener::bind(https_addr)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to bind HTTPS listener on {https_addr}: {e}")
                    })?;

                tracing::info!(target: "server", addr = %https_addr, "HTTPS server listening");
                tracing::info!(
                    target: "server",
                    url = %format!("https://{https_addr}/admin"),
                    "Admin panel available over HTTPS"
                );

                tokio::spawn(async move {
                    run_https_static(https_tcp, rustls_cfg, app_tls, cancel_tls).await;
                });
            }

            #[cfg(feature = "tls-acme")]
            Some(crate::tls::Acceptor::Acme(acme_acceptor, server_cfg)) => {
                let https_tcp = tokio::net::TcpListener::bind(https_addr)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to bind HTTPS/ACME listener on {https_addr}: {e}")
                    })?;

                tracing::info!(target: "server", addr = %https_addr, "HTTPS/ACME server listening");
                tracing::info!(
                    target: "server",
                    url = %format!("https://{https_addr}/admin"),
                    "Admin panel available over HTTPS (ACME)"
                );

                tokio::spawn(async move {
                    run_https_acme(https_tcp, acme_acceptor, server_cfg, app_tls, cancel_tls).await;
                });
            }

            None => { /* tls.enabled = false — unreachable here but exhaustive */ }

            // Suppress unreachable-pattern warning when tls-acme feature is off.
            #[allow(unreachable_patterns)]
            Some(_) => {
                return Err(anyhow::anyhow!(
                    "ACME acceptor built but tls-acme feature is not enabled — \
                     rebuild with: cargo build --features tls-acme"
                ));
            }
        }
    }

    // ── HTTP→HTTPS redirect listener (optional) ───────────────────────────────
    if CONFIG.tls.enabled && CONFIG.tls.redirect_http {
        let http_addr: std::net::SocketAddr = CONFIG
            .bind_addr_with_port(CONFIG.tls.http_port)
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid HTTP redirect bind address: {e}"))?;
        let https_port = CONFIG.tls.port;
        let cancel_redirect = worker_cancel.clone();
        let http_listener = tokio::net::TcpListener::bind(http_addr)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to bind HTTP→HTTPS redirect listener on {http_addr}: {e}")
            })?;
        tracing::info!(target: "server", addr = %http_addr, "HTTP→HTTPS redirect listening");
        tokio::spawn(async move {
            run_http_redirect(http_listener, https_port, cancel_redirect).await;
        });
    }

    let serve_cancel = worker_cancel.clone();
    let wait_shutdown = worker_cancel.clone();
    let mut http_server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            serve_cancel.cancelled().await;
        })
        .await
    });

    tokio::select! {
        () = shutdown_signal() => {
            worker_cancel.cancel();
        }
        () = wait_shutdown.cancelled() => {}
    }

    let server_result: anyhow::Result<()> =
        match tokio::time::timeout(Duration::from_secs(1), &mut http_server).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e.into()),
            Ok(Err(join_err)) => Err(anyhow::anyhow!("HTTP server task failed: {join_err}")),
            Err(_) => {
                tracing::warn!(
                    target: "server",
                    "Forcing disconnect of active HTTP clients during shutdown"
                );
                http_server.abort();
                let _ = http_server.await;
                Ok(())
            }
        };
    server_result?;
    // timeout, replacing the previous blind 10-second sleep. Each worker is
    // given up to (ffmpeg_timeout + 10)s to finish its in-flight job.
    tracing::info!(target: "server", "Signalling background workers to shut down…");
    worker_cancel.cancel();
    let shutdown_timeout = Duration::from_secs(CONFIG.ffmpeg_timeout_secs + 10);
    for handle in worker_handles {
        let _ = tokio::time::timeout(shutdown_timeout, handle).await;
    }
    // CancellationToken, so it will exit its select! loop promptly instead of
    // sleeping through a multi-minute backoff. The 15-second safety-net timeout
    // below is only a last resort for the in-flight copy_bidirectional on any
    // active stream — Arti sends RELAY_END cells synchronously on drop, which
    // completes well within this window under normal conditions.
    if let Some(h) = tor_handle {
        let _ = tokio::time::timeout(Duration::from_secs(15), h).await;
    }

    tracing::info!(target: "server", "Server shut down gracefully.");
    Ok(())
}

fn seed_initial_default_theme(conn: &rusqlite::Connection, initial_default_theme: &str) {
    let default_theme = crate::db::get_default_user_theme(conn);
    if !default_theme.is_empty()
        || initial_default_theme.is_empty()
        || initial_default_theme == crate::theme::HARD_DEFAULT_THEME
    {
        return;
    }

    match crate::db::get_theme(conn, initial_default_theme) {
        Ok(Some(theme)) if theme.enabled => {
            let _ = crate::db::set_site_setting(conn, "default_theme", initial_default_theme);
        }
        Ok(Some(_)) => {
            tracing::warn!(
                target: "config",
                default_theme = %initial_default_theme,
                "settings.toml default_theme is disabled; falling back to the first enabled theme"
            );
        }
        Ok(None) => {
            tracing::warn!(
                target: "config",
                default_theme = %initial_default_theme,
                "settings.toml default_theme does not match any configured theme; falling back to the first enabled theme"
            );
        }
        Err(error) => {
            tracing::warn!(
                target: "config",
                default_theme = %initial_default_theme,
                %error,
                "Could not validate settings.toml default_theme"
            );
        }
    }
}

// ── HTTPS listener (Static path: self-signed or manual PEM) ──────────────────
//
// Uses axum-server which preserves ConnectInfo<SocketAddr> so the IP-banning
// and rate-limiting middleware in middleware/mod.rs continues to work correctly.
//
// Accepts a pre-bound TcpListener instead of a SocketAddr so the
// actual socket bind (and any OS-level failure) happens on the main task in
// run_server() where errors propagate with `?`.  axum_server::from_tcp_rustls
// takes ownership of the already-bound std::TcpListener and does not re-bind.
// The "HTTPS server listening" log is emitted by run_server() after the pre-bind
// succeeds, so it is never printed for a socket that wasn't actually bound.
pub async fn run_https_static(
    listener: tokio::net::TcpListener,
    tls_config: axum_server::tls_rustls::RustlsConfig,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) {
    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    // Wire graceful shutdown to the same CancellationToken that controls
    // background workers and the HTTP listener.
    tokio::spawn(async move {
        cancel.cancelled().await;
        handle_clone.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
    });

    // Convert tokio TcpListener → std TcpListener for axum_server::from_tcp_rustls.
    // set_nonblocking(true) is required — axum-server expects a non-blocking socket.
    let std_listener = match listener.into_std() {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(target: "server", error = %e, "Failed to convert HTTPS listener");
            return;
        }
    };

    let server = match axum_server::from_tcp_rustls(std_listener, tls_config) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(target: "server", error = %e, "Failed to create HTTPS server");
            return;
        }
    };

    if let Err(e) = server
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
    {
        tracing::error!(target: "server", error = %e, "HTTPS server error");
    }
}

// ── HTTPS listener (ACME / Let's Encrypt path) ────────────────────────────────
//
// ACME requires a manual accept loop because AcmeAcceptor::accept() must
// inspect each connection for TLS-ALPN-01 challenges before the TLS handshake
// completes. axum-server cannot intercept at that level.
//
// ConnectInfo<SocketAddr> is injected manually into each request so that the
// IP-banning and rate-limiting middleware in middleware/mod.rs continues to work.
#[cfg(feature = "tls-acme")]
pub async fn run_https_acme(
    listener: tokio::net::TcpListener,
    acme_acceptor: std::sync::Arc<rustls_acme::AcmeAcceptor>,
    server_cfg: std::sync::Arc<rustls::ServerConfig>,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use tower::Service as _;

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!(target: "server", "HTTPS/ACME listener shutting down");
                break;
            }
            result = listener.accept() => {
                let (tcp, peer_addr) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(target: "server", error = %e, "ACME TCP accept error");
                        continue;
                    }
                };

                let acme_acceptor = acme_acceptor.clone();
                let server_cfg    = server_cfg.clone();
                let svc           = app.clone();

                tokio::spawn(async move {
                    use tokio_util::compat::{TokioAsyncReadCompatExt, FuturesAsyncReadCompatExt};
                    // rustls-acme requires futures::{AsyncRead, AsyncWrite}; wrap
                    // the tokio TcpStream with the tokio-util compat shim.
                    let tcp = tcp.compat();
                    match acme_acceptor.accept(tcp).await {
                        Ok(Some(start)) => {
                            match start.into_stream(server_cfg).await {
                                Ok(tls_stream) => {
                                    // Convert the futures-io TLS stream back to a
                                    // tokio-io stream so TokioIo / hyper can use it.
                                    let io = TokioIo::new(tls_stream.compat());
                                    // hyper::service::service_fn requires Fn (not FnMut), but
                                    // Tower's Service::call takes &mut self. Clone the router
                                    // per-request — axum::Router is Arc-backed so this is cheap.
                                    let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                                        let (mut parts, body) = req.into_parts();
                                        parts.extensions.insert(axum::extract::ConnectInfo(peer_addr));
                                        let req = axum::extract::Request::from_parts(
                                            parts,
                                            axum::body::Body::new(body),
                                        );
                                        svc.clone().call(req)
                                    });
                                    if let Err(e) = http1::Builder::new()
                                        .serve_connection(io, svc)
                                        .await
                                    {
                                        tracing::debug!(
                                            target: "server",
                                            peer = %peer_addr,
                                            error = %e,
                                            "ACME HTTPS connection error"
                                        );
                                    }
                                }
                                Err(e) => tracing::debug!(
                                    target: "server",
                                    peer = %peer_addr,
                                    error = %e,
                                    "TLS handshake failed"
                                ),
                            }
                        }
                        Ok(None) => { /* ACME challenge — handled internally, no action needed */ }
                        Err(e)   => tracing::debug!(
                            target: "server",
                            peer = %peer_addr,
                            error = %e,
                            "ACME acceptor error"
                        ),
                    }
                });
            }
        }
    }
}

// ── HTTP→HTTPS redirect listener ─────────────────────────────────────────────
//
// Issues a 301 permanent redirect to the HTTPS equivalent of every request.
// Only spawned when `tls.enabled = true` and `tls.redirect_http = true`.
pub async fn run_http_redirect(
    listener: tokio::net::TcpListener,
    https_port: u16,
    cancel: tokio_util::sync::CancellationToken,
) {
    use axum::{extract::Request, http::StatusCode, response::IntoResponse, routing::any};

    let redirect_app = axum::Router::new().route(
        "/{*path}",
        any(move |req: Request| async move {
            build_redirect_response(&req, https_port).unwrap_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    "Refusing HTTP redirect for untrusted host header",
                )
                    .into_response()
            })
        }),
    );

    axum::serve(listener, redirect_app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .ok();
}

fn build_redirect_response(
    req: &axum::extract::Request,
    https_port: u16,
) -> Option<axum::response::Response> {
    let path = req
        .uri()
        .path_and_query()
        .map_or("/", axum::http::uri::PathAndQuery::as_str);
    let host = redirect_host(req)?;
    Some(
        Redirect::permanent(&format!(
            "https://{}{path}",
            format_redirect_authority(&host, https_port)
        ))
        .into_response(),
    )
}

fn redirect_host(req: &axum::extract::Request) -> Option<String> {
    let authority = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authority_host);
    let trusted_hosts = redirect_trusted_hosts();

    if let Some(host) = authority.as_deref() {
        if trusted_hosts
            .iter()
            .any(|trusted| trusted.eq_ignore_ascii_case(host))
        {
            return Some(host.to_string());
        }

        if trusted_hosts.is_empty() && is_local_redirect_host(host) {
            return Some(host.to_string());
        }
    }

    trusted_hosts.first().cloned()
}

fn redirect_trusted_hosts() -> Vec<String> {
    redirect_trusted_hosts_with(
        &CONFIG.public_hosts,
        CONFIG.tls.acme.enabled,
        &CONFIG.tls.acme.domains,
        &CONFIG.bind_addr,
    )
}

fn redirect_trusted_hosts_with(
    public_hosts: &[String],
    acme_enabled: bool,
    acme_domains: &[String],
    bind_addr: &str,
) -> Vec<String> {
    let mut hosts = Vec::new();

    hosts.extend(
        public_hosts
            .iter()
            .filter_map(|host| crate::config::normalize_public_host(host)),
    );

    if acme_enabled {
        hosts.extend(
            acme_domains
                .iter()
                .filter_map(|domain| crate::config::normalize_public_host(domain)),
        );
    }

    if let Some(bind_host) =
        parse_bind_host(bind_addr).filter(|host| !matches!(host.as_str(), "0.0.0.0" | "::"))
    {
        hosts.push(bind_host);
    }

    hosts.sort_unstable_by_key(|host| host.to_ascii_lowercase());
    hosts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    hosts
}

fn parse_bind_host(bind_addr: &str) -> Option<String> {
    if let Some(rest) = bind_addr.strip_prefix('[') {
        let (host, _) = rest.split_once("]:")?;
        return Some(host.to_string());
    }

    bind_addr
        .rsplit_once(':')
        .map(|(host, _port)| host.to_string())
}

fn parse_authority_host(value: &str) -> Option<String> {
    let authority = value.parse::<axum::http::uri::Authority>().ok()?;
    Some(authority.host().to_string())
}

fn format_redirect_authority(host: &str, https_port: u16) -> String {
    if host
        .parse::<IpAddr>()
        .is_ok_and(|address| matches!(address, IpAddr::V6(_)))
    {
        format!("[{host}]:{https_port}")
    } else {
        format!("{host}:{https_port}")
    }
}

fn is_local_redirect_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok()
}

/// Inject the `Onion-Location` response header when the Tor hidden service is
/// active and the request arrived over clearnet (not already via .onion).
///
/// Tor Browser reads this header and prompts the user to switch to the .onion
/// address automatically, improving privacy without requiring the user to know
/// the onion address in advance.
///
/// The header is suppressed when:
///   - `enable_tor_support` is false (no onion service running)
///   - The onion address is not yet known (Arti still bootstrapping)
///   - The request already came in via the onion address (no double-redirect)
///
/// Spec: <https://community.torproject.org/onion-services/advanced/onion-location/>
async fn onion_location_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Skip if Tor is not enabled.
    if !crate::config::CONFIG.enable_tor_support {
        return next.run(req).await;
    }

    // Read the onion address under a short-lived read lock before await.
    let maybe_addr = state.onion_address.read().await.clone();
    let request_host = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authority_host);

    let mut resp = next.run(req).await;

    if let Some(addr) = maybe_addr {
        // Only inject on HTML responses — static assets, JSON, and media do
        // not benefit from the header and it adds noise to every response.
        let is_html = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/html"));

        let already_on_onion = request_host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case(&addr));

        if is_html && !already_on_onion {
            if let Ok(val) = header::HeaderValue::from_str(&format!("http://{addr}")) {
                resp.headers_mut()
                    .insert(header::HeaderName::from_static("onion-location"), val);
            }
        }
    }

    resp
}

#[cfg(test)]
mod tests {
    use super::{
        build_redirect_response, format_redirect_authority, redirect_host,
        redirect_trusted_hosts_with, scheduled_full_backup_failure_retry_delay,
        seed_initial_default_theme,
    };
    use axum::{body::Body, extract::Request, http::header};
    use std::time::Duration;

    fn request_with_host(host: &str) -> Request {
        Request::builder()
            .uri("/demo?x=1")
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("request")
    }

    fn test_conn() -> r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager> {
        let pool = crate::db::init_test_pool().expect("test pool");
        pool.get().expect("db connection")
    }

    #[test]
    fn initial_default_theme_seeds_enabled_non_hard_default_theme() {
        let conn = test_conn();

        seed_initial_default_theme(&conn, "terminal");

        assert_eq!(
            crate::db::get_site_setting(&conn, "default_theme")
                .expect("read default theme")
                .as_deref(),
            Some("terminal")
        );
    }

    #[test]
    fn initial_default_theme_does_not_overwrite_existing_db_setting() {
        let conn = test_conn();
        crate::db::set_site_setting(&conn, "default_theme", "blue-sky").expect("set default");

        seed_initial_default_theme(&conn, "terminal");

        assert_eq!(
            crate::db::get_site_setting(&conn, "default_theme")
                .expect("read default theme")
                .as_deref(),
            Some("blue-sky")
        );
    }

    #[test]
    fn initial_default_theme_skips_invalid_theme() {
        let conn = test_conn();

        seed_initial_default_theme(&conn, "missing-theme");

        assert_eq!(
            crate::db::get_site_setting(&conn, "default_theme").expect("read default theme"),
            None
        );
    }

    #[test]
    fn initial_default_theme_skips_disabled_theme() {
        let conn = test_conn();
        conn.execute("UPDATE themes SET enabled = 0 WHERE slug = 'terminal'", [])
            .expect("disable terminal");

        seed_initial_default_theme(&conn, "terminal");

        assert_eq!(
            crate::db::get_site_setting(&conn, "default_theme").expect("read default theme"),
            None
        );
    }

    #[test]
    fn redirect_rejects_untrusted_hosts_without_allowlist() {
        let request = request_with_host("evil.example");
        assert!(redirect_host(&request).is_none());
        assert!(build_redirect_response(&request, 8443).is_none());
    }

    #[test]
    fn redirect_allows_local_hosts_without_allowlist() {
        let request = request_with_host("127.0.0.1");
        assert_eq!(redirect_host(&request).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn redirect_formats_ipv6_authorities_with_brackets() {
        assert_eq!(format_redirect_authority("::1", 8443), "[::1]:8443");
    }

    #[test]
    fn redirect_trusted_hosts_include_configured_public_hosts_for_manual_cert_setups() {
        let hosts = redirect_trusted_hosts_with(
            &["example.com".to_string(), "www.example.com".to_string()],
            false,
            &[],
            "0.0.0.0:8080",
        );
        assert!(hosts.iter().any(|host| host == "example.com"));
        assert!(hosts.iter().any(|host| host == "www.example.com"));
    }

    #[test]
    fn scheduled_full_backup_retry_delay_uses_exponential_backoff() {
        let interval = Duration::from_secs(24 * 3600);
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 1),
            Duration::from_secs(15 * 60)
        );
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 2),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 3),
            Duration::from_secs(60 * 60)
        );
    }

    #[test]
    fn scheduled_full_backup_retry_delay_caps_at_backup_interval() {
        let interval = Duration::from_secs(3600);
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 10),
            interval
        );
    }
}
