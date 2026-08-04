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

use anyhow::Context as _;
use axum::{
    http::header,
    response::{IntoResponse as _, Redirect},
};
use dashmap::DashMap;
use std::future::pending;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, LazyLock,
};
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
/// HTTP route construction and handler wiring.
mod router;

use super::console::input::KeyEvent;
use super::console::{ConsoleMode, WizardKind};
use lifecycle::shutdown_signal;
use router::build_router;

/// Apply the primary request-boundary checks to a secondary listener request.
pub(super) async fn secondary_listener_request_boundary(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    headers::request_boundary_middleware(request, next).await
}

/// Return whether background maintenance can safely start at the current load.
fn should_run_background_maintenance(state: &AppState, max_in_flight: u64) -> bool {
    !state.maintenance_gate.is_active()
        && ACTIVE_UPLOADS.load(Ordering::Relaxed) == 0
        && IN_FLIGHT.load(Ordering::Relaxed) <= max_in_flight
}

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
/// RAII guard that saturating-decrements an atomic counter when dropped.
pub(super) struct ScopedDecrement<'a>(pub(super) &'a AtomicU64);

impl Drop for ScopedDecrement<'_> {
    fn drop(&mut self) {
        // Saturating decrement: fetch_update retries on spurious failure.
        let _previous_value = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }
}

// ─── Server mode ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Plaintext application listener mode selected from the TLS and Tor settings.
enum PlaintextAppListener {
    /// Expose the application over the configured public plaintext listener.
    Public,
    /// Restrict plaintext application traffic to the local Tor backend.
    TorBackend,
    /// Do not start a plaintext application listener.
    Disabled,
}

/// Select the plaintext listener mode for the current TLS and Tor configuration.
const fn plaintext_app_listener(
    tls_enabled: bool,
    require_https: bool,
    tor_enabled: bool,
) -> PlaintextAppListener {
    match (tls_enabled && require_https, tor_enabled) {
        (false, _) => PlaintextAppListener::Public,
        (true, true) => PlaintextAppListener::TorBackend,
        (true, false) => PlaintextAppListener::Disabled,
    }
}

/// Admit authenticated Tor backend traffic and reject or redirect other plaintext traffic.
async fn tls_plaintext_backend_gate(
    req: axum::extract::Request,
    next: axum::middleware::Next,
    https_port: u16,
    redirect_http: bool,
) -> axum::response::Response {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0);
    if crate::detect::tor_stream_token_identity(peer, true).is_some() {
        return next.run(req).await;
    }

    if redirect_http {
        return build_redirect_response(&req, https_port).unwrap_or_else(|| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                "Refusing HTTP redirect for untrusted host header",
            )
                .into_response()
        });
    }

    let mut response = (
        axum::http::StatusCode::UPGRADE_REQUIRED,
        "HTTPS is required",
    )
        .into_response();
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("close"),
    );
    response
}

/// Wrap the Tor plaintext backend with its admission gate and request boundary.
fn protect_tls_plaintext_backend(
    app: axum::Router,
    https_port: u16,
    redirect_http: bool,
) -> axum::Router {
    app.layer(axum::middleware::from_fn(move |req, next| {
        tls_plaintext_backend_gate(req, next, https_port, redirect_http)
    }))
    // build_router already has this boundary for requests that pass the gate.
    // Keep a second, outer boundary so malformed direct traffic is rejected
    // before redirect or upgrade handling as well.
    .layer(axum::middleware::from_fn(
        headers::request_boundary_middleware,
    ))
}

#[expect(
    clippy::cognitive_complexity,
    reason = "startup, listener selection, and coordinated shutdown form one server lifecycle"
)]
#[expect(
    clippy::too_many_lines,
    reason = "startup ordering and shutdown ownership are one cohesive server lifecycle"
)]
/// Initialize and run the configured HTTP, HTTPS, Tor, and background services.
///
/// # Errors
///
/// Returns an error when configuration validation, filesystem or database
/// initialization, listener startup, or coordinated listener execution fails.
pub async fn run_server(port_override: Option<u16>, chan_net: bool) -> anyhow::Result<()> {
    // rustls 0.23 requires an explicit process-wide crypto provider.
    // install_default() is idempotent — a second call (e.g. in tests) returns
    // Err but never panics, so the let _ discard is intentional.
    drop(rustls::crypto::ring::default_provider().install_default());

    let early_data_dir = data_dir();
    std::fs::create_dir_all(&early_data_dir)?;
    migrate_runtime_layout_if_needed()?;

    generate_settings_file_if_missing();

    // Validate critical configuration values immediately — fail fast with a
    // clear error rather than discovering misconfiguration at runtime (#8).
    CONFIG.validate()?;
    if chan_net {
        CONFIG.validate_chan_net_listener()?;
    }

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
        let reconciliation = tokio::task::spawn_blocking(move || {
            crate::pending_fs::reconcile_pending_fs_ops(&reconcile_pool, &upload_dir)
        })
        .await
        .map_err(|error| anyhow::anyhow!("pending_fs_ops reconciliation task failed: {error}"))?;
        if let Err(error) = reconciliation {
            tracing::warn!(
                target: "pending_fs",
                error = %error,
                "startup filesystem reconciliation left quarantined operations for retry"
            );
        }
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
                drop(crate::db::set_site_setting(
                    &conn,
                    "site_name",
                    &CONFIG.forum_name,
                ));
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
                    "select board to proceed".to_owned()
                } else {
                    CONFIG.initial_site_subtitle.clone()
                };
                drop(crate::db::set_site_setting(&conn, "site_subtitle", &seed));
                seed
            });
            crate::templates::set_live_site_subtitle(&subtitle);

            let legacy_new_activity =
                crate::db::get_site_setting(&conn, "new_activity_notifications_enabled")
                    .ok()
                    .flatten();
            if crate::db::get_site_setting(&conn, "homepage_new_thread_badges_enabled")
                .ok()
                .flatten()
                .is_none()
            {
                let value = legacy_new_activity.as_deref().unwrap_or(
                    if CONFIG.initial_homepage_new_thread_badges_enabled {
                        "1"
                    } else {
                        "0"
                    },
                );
                drop(crate::db::set_site_setting(
                    &conn,
                    "homepage_new_thread_badges_enabled",
                    value,
                ));
            }
            if crate::db::get_site_setting(&conn, "homepage_new_reply_badges_enabled")
                .ok()
                .flatten()
                .is_none()
            {
                let value = legacy_new_activity.as_deref().unwrap_or(
                    if CONFIG.initial_homepage_new_reply_badges_enabled {
                        "1"
                    } else {
                        "0"
                    },
                );
                drop(crate::db::set_site_setting(
                    &conn,
                    "homepage_new_reply_badges_enabled",
                    value,
                ));
            }
            if crate::db::get_site_setting(&conn, "thread_new_reply_badges_enabled")
                .ok()
                .flatten()
                .is_none()
            {
                let value = legacy_new_activity.as_deref().unwrap_or(
                    if CONFIG.initial_thread_new_reply_badges_enabled {
                        "1"
                    } else {
                        "0"
                    },
                );
                drop(crate::db::set_site_setting(
                    &conn,
                    "thread_new_reply_badges_enabled",
                    value,
                ));
            }

            seed_initial_default_theme(&conn, &CONFIG.initial_default_theme);
            if crate::db::get_site_setting(&conn, crate::db::MEDIA_AUTO_PRUNE_ENABLED_KEY)
                .ok()
                .flatten()
                .is_none()
            {
                drop(crate::db::set_site_setting(
                    &conn,
                    crate::db::MEDIA_AUTO_PRUNE_ENABLED_KEY,
                    &CONFIG.initial_media_auto_prune_enabled.to_string(),
                ));
            }
            if crate::db::get_site_setting(
                &conn,
                crate::db::MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES_KEY,
            )
            .ok()
            .flatten()
            .is_none()
            {
                drop(crate::db::set_site_setting(
                    &conn,
                    crate::db::MEDIA_MAX_ACTIVE_CONTENT_SIZE_BYTES_KEY,
                    &CONFIG
                        .initial_media_max_active_content_size_bytes
                        .to_string(),
                ));
            }
            drop(crate::db::sync_live_theme_state(&conn));

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
    if CONFIG.require_ffmpeg && !ffmpeg_available {
        anyhow::bail!(
            "ffmpeg is required but was not detected at '{}'",
            CONFIG.ffmpeg_path
        );
    }
    // ffprobe is used lazily for WebM codec inspection, so probe it at startup
    // to make explicit configured paths authoritative and catch bogus paths early.
    let ffprobe_available = crate::detect::detect_ffprobe();
    // libwebp encoder: needed for image→WebP conversion.  Checked independently
    // so that a stock ffmpeg build (missing libwebp) still enables video/audio
    // features while image conversion degrades gracefully.
    let ffmpeg_webp_available = crate::detect::detect_webp_encoder(ffmpeg_available);
    // libvpx-vp9 + libopus encoders: needed for MP4→WebM transcoding and
    // WebM/AV1→VP9 re-encoding.  Checked independently so that a build missing
    // only these codecs still enables image conversion and thumbnail generation.
    let ffmpeg_webm_status = crate::detect::detect_webm_encoder(ffmpeg_available);
    let ffmpeg_vp9_available = ffmpeg_webm_status.is_available();
    let pdf_thumbnail_renderers = crate::detect::detect_pdf_thumbnail_renderers();

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
    {
        let conn = pool.get()?;
        let recovery = crate::db::recover_interrupted_background_jobs(&conn)?;
        crate::workers::cleanup_recovered_waveform_placeholders(
            &conn,
            std::path::Path::new(&CONFIG.upload_dir),
        )?;
        if recovery.jobs_reset > 0 || recovery.jobs_resolved > 0 {
            tracing::warn!(
                target: "workers",
                jobs_reset = recovery.jobs_reset,
                jobs_resolved = recovery.jobs_resolved,
                media_posts_reset = recovery.media_posts_reset,
                "Recovered interrupted background jobs from previous shutdown"
            );
        }
    }
    if let Err(error) = crate::workers::normalize_legacy_thread_prune_jobs(
        &pool,
        crate::workers::THREAD_PRUNE_RECONCILE_BATCH,
    ) {
        tracing::error!(
            target: "workers",
            error = %error,
            "startup legacy thread-prune normalization failed; periodic recovery remains enabled"
        );
    }
    let startup_prune_reconciliation = match crate::workers::reconcile_thread_prune_intents(
        &pool,
        0,
        crate::workers::THREAD_PRUNE_RECONCILE_BATCH,
    ) {
        Ok(report) => report,
        Err(error) => {
            tracing::error!(
                target: "workers",
                error = %error,
                "startup thread-retention reconciliation failed; periodic recovery remains enabled"
            );
            crate::workers::ThreadPruneReconciliation::default()
        }
    };
    if startup_prune_reconciliation.inserted > 0
        || startup_prune_reconciliation.coalesced > 0
        || startup_prune_reconciliation.failures > 0
    {
        tracing::info!(
            target: "workers",
            boards_scanned = startup_prune_reconciliation.scanned,
            intents_inserted = startup_prune_reconciliation.inserted,
            intents_coalesced = startup_prune_reconciliation.coalesced,
            failures = startup_prune_reconciliation.failures,
            "startup thread-retention reconciliation completed"
        );
    }
    let worker_queue = Arc::new(crate::workers::JobQueue::new(pool.clone()));
    let worker_handles = crate::workers::start_worker_pool(
        &worker_queue,
        ffmpeg_available,
        ffprobe_available,
        ffmpeg_vp9_available,
    );

    let chan_ledger = if chan_net {
        let conn = pool.get()?;
        let ledger = crate::db::chan_net::load_import_ledger(&conn)?
            .into_iter()
            .collect::<crate::chan_net::ledger::TxLedger>();
        Some(Arc::new(parking_lot::Mutex::new(ledger)))
    } else {
        None
    };

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
        ffprobe_available,
        ffmpeg_webp_available,
        ffmpeg_vp9_available,
        ffmpeg_vp9_encoder_available: ffmpeg_webm_status.vp9,
        ffmpeg_opus_available: ffmpeg_webm_status.opus,
        pdf_thumbnail_renderer: pdf_thumbnail_renderers
            .first()
            .map(|renderer| renderer.binary_name()),
        job_queue: worker_queue,
        backup_progress: Arc::new(crate::middleware::BackupProgress::new()),
        auto_full_backup_settings: crate::middleware::AutoFullBackupSettings::new(
            CONFIG.auto_full_backup_interval_hours,
            CONFIG.auto_full_backup_copies_to_keep,
            CONFIG.auto_full_backup_include_tor_hidden_service_keys,
            CONFIG.auto_full_backup_storage_mode.clone(),
            CONFIG.auto_full_backup_split_zip_part_size_bytes,
        ),
        maintenance_gate: crate::middleware::MaintenanceGate::new(),
        db_maintenance_jobs: crate::middleware::DbMaintenanceJobs::new(),
        chan_ledger,
        onion_address: Arc::new(tokio::sync::RwLock::new(None)),
    };

    // worker_cancel is the shutdown token threaded through all background tasks
    // and the Tor task. Declared here so it is available to background tasks
    // spawned below. detect_tor is called later, after the first-run wizard.
    let worker_cancel = state.job_queue.cancel.clone();
    let start_time = Instant::now();

    // Required thread-retention work is repaired independently of new posts.
    // Each pass inspects one bounded board-id page and carries its cursor into
    // the next pass, wrapping only after reaching the end.
    {
        let reconcile_pool = pool.clone();
        let reconcile_queue = Arc::clone(&state.job_queue);
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let mut cursor = startup_prune_reconciliation.next_board_id;
            let mut interval = tokio::time::interval(Duration::from_mins(1));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let pass_pool = reconcile_pool.clone();
                        let pass = tokio::task::spawn_blocking(move || {
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
                        tracing::debug!("Thread-retention reconciliation task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // Background: purge expired sessions hourly
    {
        let bg = pool.clone();
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_hours(1));
            loop {
                tokio::select! {
                    _ = iv.tick() => {
                        let connection = bg.get();
                        if let Ok(conn) = connection {
                            let purge_result = crate::db::purge_expired_sessions(&conn);
                            match purge_result {
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
                        let connection = bg.get();
                        if let Ok(conn) = connection {
                            match crate::db::run_wal_checkpoint(&conn) {
                                Ok((pages, moved, backfill)) => {
                                    tracing::debug!("WAL checkpoint: {pages} pages total, {moved} moved, {backfill} backfilled");
                                }
                                Err(e) => tracing::warn!("WAL checkpoint failed: {e}"),
                            }
                            // Fix #7: reuse `conn` instead of calling bg.get() again.
                            // A second acquire while the first is still alive deadlocks
                            // with a pool size of 1.
                            drop(conn.execute_batch("PRAGMA optimize;"));
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
            let mut iv = tokio::time::interval(Duration::from_mins(5));
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
                        let progress = Arc::clone(&maintenance_state.backup_progress);
                        let attempt_result = tokio::task::spawn_blocking(move || {
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
                () = tokio::time::sleep(Duration::from_mins(10)) => {} // initial delay
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

    // 2.6: Retry durable filesystem operations independently of request traffic.
    // Each pass is bounded by the finite queue snapshot loaded by the reconciler.
    {
        let bg = pool.clone();
        let cancel_clone = worker_cancel.clone();
        tokio::spawn(async move {
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
                        tokio::task::spawn_blocking(move || {
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
                        })
                        .await
                        .ok();
                    }
                    () = cancel_clone.cancelled() => {
                        tracing::debug!("Filesystem reconciliation task shutting down");
                        return;
                    }
                }
            }
        });
    }

    // 2.7: Waveform/thumbnail cache eviction — keep total size of all thumbs
    // directories under CONFIG.waveform_cache_max_bytes by deleting only the
    // oldest files that have no post reference. Uses 1-hour intervals.
    if CONFIG.waveform_cache_max_bytes > 0 {
        let max_bytes = CONFIG.waveform_cache_max_bytes;
        let cancel_clone = worker_cancel.clone();
        let bg = pool.clone();
        tokio::spawn(async move {
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
                        tokio::task::spawn_blocking(move || {
                            let Ok(conn) = eviction_pool.get() else {
                                return;
                            };
                            let eviction_result = crate::workers::evict_thumb_cache(
                                &conn,
                                &upload_dir,
                                max_bytes,
                            );
                            if let Err(error) = eviction_result {
                                tracing::warn!(error = %error, "Thumbnail cache eviction failed");
                            }
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

    let plaintext_server = match plaintext_app_listener(
        CONFIG.tls.enabled,
        CONFIG.tls.require_https,
        CONFIG.enable_tor_support,
    ) {
        PlaintextAppListener::Public => {
            let app = build_router(state.clone(), false);
            let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
            tracing::info!(target: "server", addr = %bind_addr, "HTTP server listening");
            tracing::info!(target: "server", url = %format!("http://{bind_addr}/admin"), "Admin panel");
            Some((listener, app))
        }
        PlaintextAppListener::TorBackend => {
            let backend_addr = CONFIG.loopback_addr_with_port(bind_port);
            let https_port = CONFIG.tls.port;
            let redirect_http = CONFIG.tls.redirect_http;
            let app = protect_tls_plaintext_backend(
                build_router(state.clone(), false),
                https_port,
                redirect_http,
            );
            let listener = tokio::net::TcpListener::bind(&backend_addr)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "Failed to bind internal Tor HTTP backend on {backend_addr}: {error}"
                    )
                })?;
            tracing::info!(
                target: "server",
                addr = %backend_addr,
                "HTTPS-only mode enabled; internal Tor HTTP backend listening"
            );
            Some((listener, app))
        }
        PlaintextAppListener::Disabled => {
            tracing::info!(
                target: "server",
                "HTTPS-only mode enabled; plaintext application listener disabled"
            );
            None
        }
    };
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
    let shared_stats: super::console::SharedStats = Arc::new(tokio::sync::RwLock::new(
        super::console::ChanStats::default(),
    ));
    let shared_mode: super::console::SharedConsoleMode =
        Arc::new(tokio::sync::RwLock::new(ConsoleMode::Dashboard));
    // Stats refresh task — polls DB every 3 s (or immediately on [R]).
    // block_in_place keeps &mut delta locals on the same stack frame so
    // req/s and other deltas are correctly accumulated across calls.
    let force_reload_notify = Arc::new(tokio::sync::Notify::new());
    {
        let pool_stats = pool.clone();
        let worker_queue_stats = Arc::clone(&state.job_queue);
        let stats_w = Arc::clone(&shared_stats);
        let cancel_stats = worker_cancel.clone();
        let onion_addr = Arc::clone(&state.onion_address);
        let force_reload = Arc::clone(&force_reload_notify);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            let mut prev_req = REQUEST_COUNT.load(Ordering::Relaxed);
            let mut prev_tick = Instant::now();
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
        Arc::clone(&state.onion_address),
        worker_cancel.clone(),
    );

    // Event dispatch — translate KeyEvents into mode changes and wizard launches.
    {
        let mode_d = Arc::clone(&shared_mode);
        let pool_d = pool.clone();
        let cancel_d = worker_cancel.clone();
        let shutdown_tx = worker_cancel.clone();
        let force_reload = Arc::clone(&force_reload_notify);
        tokio::spawn(async move {
            loop {
                let next_key = key_rx.recv().await;
                let Some(key) = next_key else {
                    break;
                };
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
                        let mode_w = Arc::clone(&mode_d);
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
                        let mode_w = Arc::clone(&mode_d);
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
                        let mode_w = Arc::clone(&mode_d);
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

    let mut listener_tasks: tokio::task::JoinSet<ListenerTaskResult> = tokio::task::JoinSet::new();
    if chan_net {
        let chan_addr = CONFIG.chan_net_bind.clone();
        let chan_app = crate::chan_net::chan_router(state.clone());
        let chan_listener = tokio::net::TcpListener::bind(&chan_addr).await?;
        let chan_cancel = worker_cancel.clone();
        tracing::info!(target: "chan_net", addr = %chan_addr, "ChanNet API listening");
        // Reuse the bounded HTTP server so this secondary listener gets the
        // same protocol-level header cap and graceful shutdown as the forum.
        listener_tasks.spawn(async move {
            (
                "ChanNet",
                run_plain_http(chan_listener, chan_app, chan_cancel)
                    .await
                    .map_err(anyhow::Error::from),
            )
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

        let https_addr: SocketAddr = CONFIG
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

                listener_tasks.spawn(async move {
                    (
                        "HTTPS",
                        run_https_static(https_tcp, rustls_cfg, app_tls, cancel_tls)
                            .await
                            .map_err(anyhow::Error::from),
                    )
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

                listener_tasks.spawn(async move {
                    (
                        "HTTPS/ACME",
                        run_https_acme(https_tcp, acme_acceptor, server_cfg, app_tls, cancel_tls)
                            .await
                            .map_err(anyhow::Error::from),
                    )
                });
            }

            None => {
                return Err(anyhow::anyhow!(
                    "TLS is enabled but no HTTPS acceptor was constructed"
                ));
            }
        }
    }

    // ── HTTP→HTTPS redirect listener (optional) ───────────────────────────────
    if CONFIG.tls.enabled && CONFIG.tls.redirect_http {
        let http_addr: SocketAddr = CONFIG
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
        listener_tasks.spawn(async move {
            (
                "HTTP redirect",
                run_http_redirect(http_listener, https_port, cancel_redirect)
                    .await
                    .map_err(anyhow::Error::from),
            )
        });
    }

    let wait_shutdown = worker_cancel.clone();
    if let Some((listener, app)) = plaintext_server {
        let serve_cancel = worker_cancel.clone();
        listener_tasks.spawn(async move {
            (
                "HTTP",
                run_plain_http(listener, app, serve_cancel)
                    .await
                    .map_err(anyhow::Error::from),
            )
        });
    }

    let runtime_result = tokio::select! {
        () = shutdown_signal() => {
            worker_cancel.cancel();
            Ok(())
        }
        () = wait_shutdown.cancelled() => Ok(()),
        result = next_listener_exit(&mut listener_tasks, &worker_cancel) => result,
    };
    worker_cancel.cancel();
    let listener_shutdown_result = finish_listener_tasks(&mut listener_tasks).await;

    // timeout, replacing the previous blind 10-second sleep. Each worker is
    // given up to (ffmpeg_timeout + 10)s to finish its in-flight job.
    tracing::info!(target: "server", "Signalling background workers to shut down…");
    worker_cancel.cancel();
    let shutdown_timeout = Duration::from_secs(crate::config::ffmpeg_timeout_secs() + 10);
    for handle in worker_handles {
        drop(tokio::time::timeout(shutdown_timeout, handle).await);
    }
    // CancellationToken, so it will exit its select! loop promptly instead of
    // sleeping through a multi-minute backoff. The 15-second safety-net timeout
    // below is only a last resort for the in-flight copy_bidirectional on any
    // active stream — Arti sends RELAY_END cells synchronously on drop, which
    // completes well within this window under normal conditions.
    if let Some(h) = tor_handle {
        drop(tokio::time::timeout(Duration::from_secs(15), h).await);
    }

    runtime_result?;
    listener_shutdown_result?;
    tracing::info!(target: "server", "Server shut down gracefully.");
    Ok(())
}

/// Seed the database default theme from the initial configuration when eligible.
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
            drop(crate::db::set_site_setting(
                conn,
                "default_theme",
                initial_default_theme,
            ));
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

/// Apply shared HTTP/1 and HTTP/2 request-head limits to a connection builder.
fn configure_http_limits(
    builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
) {
    builder.http1().max_buf_size(headers::HTTP_MAX_HEADER_BYTES);
    builder
        .http2()
        .max_header_list_size(u32::try_from(headers::HTTP_MAX_HEADER_BYTES).unwrap_or(u32::MAX));
}

/// Listener label paired with its task result.
type ListenerTaskResult = (&'static str, anyhow::Result<()>);

/// Wait for the next listener task to exit and classify its result.
async fn next_listener_exit(
    listeners: &mut tokio::task::JoinSet<ListenerTaskResult>,
    cancel: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    match listeners.join_next().await {
        Some(Ok((_, Ok(())))) if cancel.is_cancelled() => Ok(()),
        Some(Ok((listener, Ok(())))) => {
            anyhow::bail!("{listener} listener stopped unexpectedly")
        }
        Some(Ok((listener, Err(error)))) => {
            Err(error).with_context(|| format!("{listener} listener failed"))
        }
        Some(Err(error)) => Err(anyhow::anyhow!("Listener task failed: {error}")),
        None => pending().await,
    }
}

/// Drain listener tasks, forcing shutdown after the graceful timeout.
async fn finish_listener_tasks(
    listeners: &mut tokio::task::JoinSet<ListenerTaskResult>,
) -> anyhow::Result<()> {
    let drain = async {
        let mut first_error = None;
        loop {
            let next_result = listeners.join_next().await;
            let Some(result) = next_result else {
                break;
            };
            let result = match result {
                Ok((_, Ok(()))) => Ok(()),
                Ok((listener, Err(error))) => {
                    Err(error).with_context(|| format!("{listener} listener failed"))
                }
                Err(error) => Err(anyhow::anyhow!("Listener task failed: {error}")),
            };
            if first_error.is_none() {
                first_error = result.err();
            }
        }
        first_error.map_or(Ok(()), Err)
    };

    if let Ok(result) = tokio::time::timeout(Duration::from_secs(3), drain).await {
        return result;
    }
    tracing::warn!(
        target: "server",
        "Forcing disconnect of active listener clients during shutdown"
    );
    listeners.shutdown().await;
    Ok(())
}

/// Serve a pre-bound plaintext listener until cancellation.
async fn run_plain_http(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel.cancelled().await;
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(1)));
    });

    let std_listener = listener.into_std()?;
    let mut server = axum_server::from_tcp(std_listener)?;
    configure_http_limits(server.http_builder());
    server
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
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
/// Serve HTTPS on a pre-bound listener with a static Rustls configuration.
///
/// # Errors
///
/// Returns an I/O error when the listener conversion, server construction, or
/// connection-serving loop fails.
pub async fn run_https_static(
    listener: tokio::net::TcpListener,
    tls_config: axum_server::tls_rustls::RustlsConfig,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    // Wire graceful shutdown to the same CancellationToken that controls
    // background workers and the HTTP listener.
    tokio::spawn(async move {
        cancel.cancelled().await;
        handle_clone.graceful_shutdown(Some(Duration::from_secs(1)));
    });

    // Convert tokio TcpListener → std TcpListener for axum_server::from_tcp_rustls.
    // set_nonblocking(true) is required — axum-server expects a non-blocking socket.
    let std_listener = listener.into_std()?;
    let mut server = axum_server::from_tcp_rustls(std_listener, tls_config)?;
    configure_http_limits(server.http_builder());

    server
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
        .await
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
/// Serve HTTPS with the ACME TLS-ALPN acceptor until cancellation.
///
/// # Errors
///
/// Returns an I/O error when accepting a TCP connection fails.
pub async fn run_https_acme(
    listener: tokio::net::TcpListener,
    acme_acceptor: Arc<rustls_acme::AcmeAcceptor>,
    server_cfg: Arc<rustls::ServerConfig>,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use tower::Service as _;

    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!(target: "server", "HTTPS/ACME listener shutting down");
                break;
            }
            result = listener.accept() => {
                let (tcp, peer_addr) = result?;

                let acme_acceptor = Arc::clone(&acme_acceptor);
                let server_cfg = Arc::clone(&server_cfg);
                let svc           = app.clone();

                connections.spawn(async move {
                    use tokio_util::compat::{TokioAsyncReadCompatExt as _, FuturesAsyncReadCompatExt as _};
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
                                    let mut builder = http1::Builder::new();
                                    builder.max_buf_size(headers::HTTP_MAX_HEADER_BYTES);
                                    let connection_result = builder.serve_connection(io, svc).await;
                                    if let Err(e) = connection_result {
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
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result {
                    tracing::warn!(
                        target: "server",
                        %error,
                        "ACME HTTPS connection task failed"
                    );
                }
            }
        }
    }

    let drain_connections = async {
        loop {
            let next_result = connections.join_next().await;
            let Some(result) = next_result else {
                break;
            };
            if let Err(error) = result {
                tracing::warn!(
                    target: "server",
                    %error,
                    "ACME HTTPS connection task failed during shutdown"
                );
            }
        }
    };
    if tokio::time::timeout(Duration::from_secs(1), drain_connections)
        .await
        .is_err()
    {
        connections.shutdown().await;
    }
    Ok(())
}

// ── HTTP→HTTPS redirect listener ─────────────────────────────────────────────
//
// Issues a 301 permanent redirect to the HTTPS equivalent of every request.
// Only spawned when `tls.enabled = true` and `tls.redirect_http = true`.
/// Serve the HTTP-to-HTTPS redirect listener until cancellation.
///
/// # Errors
///
/// Returns an I/O error when the listener cannot be served.
pub async fn run_http_redirect(
    listener: tokio::net::TcpListener,
    https_port: u16,
    cancel: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    use axum::{extract::Request, http::StatusCode, response::IntoResponse as _, routing::any};

    let redirect_app = axum::Router::new()
        .route(
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
        )
        .layer(axum::middleware::from_fn(
            headers::request_boundary_middleware,
        ));

    run_plain_http(listener, redirect_app, cancel).await
}

/// Build a trusted permanent HTTPS redirect for a request.
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

/// Resolve a request host that is permitted as an HTTPS redirect destination.
fn redirect_host(req: &axum::extract::Request) -> Option<String> {
    let authority = req
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_authority_host);
    let trusted_hosts = redirect_trusted_hosts();

    if let Some(host) = authority.as_deref() {
        if trusted_hosts
            .iter()
            .any(|trusted| trusted.eq_ignore_ascii_case(host))
        {
            return Some(host.to_owned());
        }

        if trusted_hosts.is_empty() && is_local_redirect_host(host) {
            return Some(host.to_owned());
        }
    }

    trusted_hosts.first().cloned()
}

/// Collect normalized redirect hosts from the active configuration.
fn redirect_trusted_hosts() -> Vec<String> {
    redirect_trusted_hosts_with(
        &CONFIG.public_hosts,
        CONFIG.tls.acme.enabled,
        &CONFIG.tls.acme.domains,
        &CONFIG.bind_addr,
    )
}

/// Collect and deduplicate redirect hosts from explicit configuration values.
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

/// Extract the host portion of a configured socket address.
fn parse_bind_host(bind_addr: &str) -> Option<String> {
    if let Some(rest) = bind_addr.strip_prefix('[') {
        let (host, _) = rest.split_once("]:")?;
        return Some(host.to_owned());
    }

    bind_addr
        .rsplit_once(':')
        .map(|(host, _port)| host.to_owned())
}

/// Parse and normalize the host component of an HTTP authority.
fn parse_authority_host(value: &str) -> Option<String> {
    let authority = value.parse::<axum::http::uri::Authority>().ok()?;
    Some(authority.host().to_owned())
}

/// Format a host and HTTPS port as a redirect authority.
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

/// Return whether a redirect host is local and safe without an allowlist.
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
    if !CONFIG.enable_tor_support {
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
        build_redirect_response, format_redirect_authority, next_listener_exit,
        protect_tls_plaintext_backend, redirect_host, redirect_trusted_hosts_with, run_plain_http,
        scheduled_full_backup_failure_retry_delay, seed_initial_default_theme,
        PlaintextAppListener,
    };
    use axum::{
        body::Body,
        extract::{ConnectInfo, Request},
        http::{header, StatusCode},
        middleware::from_fn,
        routing::any,
        Router,
    };
    use std::{net::SocketAddr, sync::Arc, time::Duration};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tower::ServiceExt as _;

    /// Build a test request containing the supplied Host header.
    fn request_with_host(host: &str) -> anyhow::Result<Request> {
        Request::builder()
            .uri("/demo?x=1")
            .header(header::HOST, host)
            .body(Body::empty())
            .map_err(|error| anyhow::anyhow!("build request: {error}"))
    }

    /// Open an isolated test database connection.
    fn test_conn() -> anyhow::Result<r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>> {
        let pool = crate::db::init_test_pool()?;
        pool.get().map_err(anyhow::Error::from)
    }

    /// Build the application used to exercise the plaintext Tor backend gate.
    fn tls_backend_app(redirect_http: bool) -> Router {
        protect_tls_plaintext_backend(
            Router::new().route("/", any(|| async { StatusCode::NO_CONTENT })),
            8443,
            redirect_http,
        )
    }

    /// Build a test request carrying the supplied connection peer.
    fn request_from_peer(peer: SocketAddr) -> anyhow::Result<Request> {
        Request::builder()
            .uri("/")
            .header(header::HOST, "127.0.0.1")
            .extension(ConnectInfo(peer))
            .body(Body::empty())
            .map_err(|error| anyhow::anyhow!("build request: {error}"))
    }

    /// Send one raw HTTP request and collect its response bytes.
    async fn raw_http_request(address: SocketAddr, request: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut stream = tokio::net::TcpStream::connect(address).await?;
        stream.write_all(request).await?;
        let mut response = Vec::new();
        let read_result =
            tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
                .await
                .map_err(|error| anyhow::anyhow!("response timeout: {error}"))?;
        if let Err(error) = read_result {
            anyhow::ensure!(
                error.kind() == std::io::ErrorKind::ConnectionReset && !response.is_empty(),
                "read response: {error}"
            );
        }
        Ok(response)
    }

    /// Verify the status code fragment in a raw HTTP response.
    fn ensure_raw_status(response: &[u8], expected: &[u8]) -> anyhow::Result<()> {
        let status_line = response
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        anyhow::ensure!(
            status_line
                .windows(expected.len())
                .any(|part| part == expected),
            "unexpected response status: {}",
            String::from_utf8_lossy(status_line)
        );
        Ok(())
    }

    #[test]
    fn native_tls_requires_explicit_https_only_mode_to_disable_plaintext() {
        assert_eq!(
            super::plaintext_app_listener(false, false, false),
            PlaintextAppListener::Public
        );
        assert_eq!(
            super::plaintext_app_listener(false, true, false),
            PlaintextAppListener::Public
        );
        assert_eq!(
            super::plaintext_app_listener(true, false, false),
            PlaintextAppListener::Public
        );
        assert_eq!(
            super::plaintext_app_listener(true, false, true),
            PlaintextAppListener::Public
        );
        assert_eq!(
            super::plaintext_app_listener(true, true, false),
            PlaintextAppListener::Disabled
        );
        assert_eq!(
            super::plaintext_app_listener(true, true, true),
            PlaintextAppListener::TorBackend
        );
    }

    #[tokio::test]
    async fn tls_tor_backend_redirects_direct_plaintext_requests() -> anyhow::Result<()> {
        let peer = "127.0.0.1:61001".parse()?;
        let response = tls_backend_app(true)
            .oneshot(request_from_peer(peer)?)
            .await?;

        anyhow::ensure!(response.status() == StatusCode::PERMANENT_REDIRECT);
        anyhow::ensure!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                == Some("https://127.0.0.1:8443/"),
            "redirect should point to the configured HTTPS port"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tls_tor_backend_rejects_direct_plaintext_without_redirect_listener(
    ) -> anyhow::Result<()> {
        let peer = "127.0.0.1:61003".parse()?;
        let response = tls_backend_app(false)
            .oneshot(request_from_peer(peer)?)
            .await?;

        anyhow::ensure!(response.status() == StatusCode::UPGRADE_REQUIRED);
        anyhow::ensure!(
            response.headers().get(header::CONNECTION)
                == Some(&header::HeaderValue::from_static("close")),
            "rejected plaintext connections should be closed"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tls_tor_backend_allows_only_registered_tor_proxy_peers() -> anyhow::Result<()> {
        let peer: SocketAddr = "127.0.0.1:61002".parse()?;
        let same_port_other_loopback: SocketAddr = "127.0.0.2:61002".parse()?;
        crate::detect::TOR_STREAM_TOKENS.insert(peer, "tor:test".into());

        let response = tls_backend_app(true)
            .oneshot(request_from_peer(peer)?)
            .await?;
        let spoofed = tls_backend_app(true)
            .oneshot(request_from_peer(same_port_other_loopback)?)
            .await?;
        crate::detect::TOR_STREAM_TOKENS.remove(&peer);

        anyhow::ensure!(response.status() == StatusCode::NO_CONTENT);
        anyhow::ensure!(spoofed.status() == StatusCode::PERMANENT_REDIRECT);
        Ok(())
    }

    #[tokio::test]
    async fn tls_tor_backend_rejects_ambiguous_framing_before_redirect_gate() -> anyhow::Result<()>
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel.clone();
        let server = tokio::spawn(async move {
            run_plain_http(listener, tls_backend_app(true), server_cancel).await
        });

        let ambiguous = raw_http_request(
            address,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n",
        )
        .await?;
        ensure_raw_status(&ambiguous, b"400")?;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), server).await???;
        Ok(())
    }

    #[tokio::test]
    async fn listener_supervisor_reports_unexpected_listener_failure() -> anyhow::Result<()> {
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut listeners: tokio::task::JoinSet<super::ListenerTaskResult> =
            tokio::task::JoinSet::new();
        listeners.spawn(async { ("HTTPS", Err(anyhow::anyhow!("injected listener failure"))) });

        let Err(error) = next_listener_exit(&mut listeners, &cancel).await else {
            anyhow::bail!("unexpected listener failure must stop the runtime");
        };
        let message = format!("{error:#}");
        anyhow::ensure!(message.contains("HTTPS listener failed"));
        anyhow::ensure!(message.contains("injected listener failure"));
        Ok(())
    }

    #[tokio::test]
    async fn plain_http_boundary_rejects_te_cl_and_large_request_heads() -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let app = Router::new()
            .route("/", any(|| async { StatusCode::NO_CONTENT }))
            .layer(from_fn(super::headers::request_boundary_middleware));
        let server_cancel = cancel.clone();
        let server =
            tokio::spawn(async move { run_plain_http(listener, app, server_cancel).await });

        let valid = raw_http_request(
            address,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest",
        )
        .await?;
        ensure_raw_status(&valid, b"204")?;

        let exact_header_value = format!(
            "GET / HTTP/1.1\r\nHost: localhost\r\nX-Boundary: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(super::headers::HTTP_MAX_HEADER_VALUE_BYTES)
        );
        let exact_header_value = raw_http_request(address, exact_header_value.as_bytes()).await?;
        ensure_raw_status(&exact_header_value, b"204")?;

        let ambiguous = raw_http_request(
            address,
            b"POST / HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\nConnection: close\r\n\r\n4\r\ntest\r\n0\r\n\r\n",
        )
        .await?;
        ensure_raw_status(&ambiguous, b"400")?;

        let oversized = format!(
            "GET / HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(super::headers::HTTP_MAX_HEADER_BYTES)
        );
        let oversized = raw_http_request(address, oversized.as_bytes()).await?;
        ensure_raw_status(&oversized, b"431")?;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), server).await???;
        Ok(())
    }

    #[tokio::test]
    async fn channet_listener_rejects_ambiguous_framing_and_large_request_heads(
    ) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let app = crate::chan_net::chan_router_with_auth(
            crate::test_support::app_state(),
            Arc::from("0123456789abcdef0123456789abcdef"),
            512 * 1024,
            10 * 1024 * 1024,
        );
        let server_cancel = cancel.clone();
        let server =
            tokio::spawn(async move { run_plain_http(listener, app, server_cancel).await });

        let valid = raw_http_request(
            address,
            b"GET /chan/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        ensure_raw_status(&valid, b"200")?;

        let ambiguous = raw_http_request(
            address,
            b"GET /chan/status HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\nConnection: close\r\n\r\n0\r\n\r\n",
        )
        .await?;
        ensure_raw_status(&ambiguous, b"400")?;

        let oversized = format!(
            "GET /chan/status HTTP/1.1\r\nHost: localhost\r\nX-Large: {}\r\nConnection: close\r\n\r\n",
            "a".repeat(super::headers::HTTP_MAX_HEADER_BYTES)
        );
        let oversized = raw_http_request(address, oversized.as_bytes()).await?;
        ensure_raw_status(&oversized, b"431")?;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(3), server).await???;
        Ok(())
    }

    #[test]
    fn initial_default_theme_seeds_enabled_non_hard_default_theme() -> anyhow::Result<()> {
        let conn = test_conn()?;

        seed_initial_default_theme(&conn, "terminal");

        anyhow::ensure!(
            crate::db::get_site_setting(&conn, "default_theme")?.as_deref() == Some("terminal"),
            "configured enabled theme should seed the default"
        );
        Ok(())
    }

    #[test]
    fn initial_default_theme_does_not_overwrite_existing_db_setting() -> anyhow::Result<()> {
        let conn = test_conn()?;
        crate::db::set_site_setting(&conn, "default_theme", "blue-sky")?;

        seed_initial_default_theme(&conn, "terminal");

        anyhow::ensure!(
            crate::db::get_site_setting(&conn, "default_theme")?.as_deref() == Some("blue-sky"),
            "an existing database setting should take precedence"
        );
        Ok(())
    }

    #[test]
    fn initial_default_theme_skips_invalid_theme() -> anyhow::Result<()> {
        let conn = test_conn()?;

        seed_initial_default_theme(&conn, "missing-theme");

        anyhow::ensure!(
            crate::db::get_site_setting(&conn, "default_theme")?.is_none(),
            "an unknown theme should not seed the default"
        );
        Ok(())
    }

    #[test]
    fn initial_default_theme_skips_disabled_theme() -> anyhow::Result<()> {
        let conn = test_conn()?;
        conn.execute("UPDATE themes SET enabled = 0 WHERE slug = 'terminal'", [])
            .map_err(|error| anyhow::anyhow!("disable terminal theme: {error}"))?;

        seed_initial_default_theme(&conn, "terminal");

        anyhow::ensure!(
            crate::db::get_site_setting(&conn, "default_theme")?.is_none(),
            "a disabled theme should not seed the default"
        );
        Ok(())
    }

    #[test]
    fn redirect_rejects_untrusted_hosts_without_allowlist() -> anyhow::Result<()> {
        let request = request_with_host("evil.example")?;
        anyhow::ensure!(redirect_host(&request).is_none());
        anyhow::ensure!(build_redirect_response(&request, 8443).is_none());
        Ok(())
    }

    #[test]
    fn redirect_allows_local_hosts_without_allowlist() -> anyhow::Result<()> {
        let request = request_with_host("127.0.0.1")?;
        anyhow::ensure!(redirect_host(&request).as_deref() == Some("127.0.0.1"));
        Ok(())
    }

    #[test]
    fn redirect_formats_ipv6_authorities_with_brackets() {
        assert_eq!(format_redirect_authority("::1", 8443), "[::1]:8443");
    }

    #[test]
    fn redirect_trusted_hosts_include_configured_public_hosts_for_manual_cert_setups() {
        let hosts = redirect_trusted_hosts_with(
            &["example.com".to_owned(), "www.example.com".to_owned()],
            false,
            &[],
            "0.0.0.0:8080",
        );
        assert!(hosts.iter().any(|host| host == "example.com"));
        assert!(hosts.iter().any(|host| host == "www.example.com"));
    }

    #[test]
    fn scheduled_full_backup_retry_delay_uses_exponential_backoff() {
        let interval = Duration::from_hours(24);
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 1),
            Duration::from_mins(15)
        );
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 2),
            Duration::from_mins(30)
        );
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 3),
            Duration::from_hours(1)
        );
    }

    #[test]
    fn scheduled_full_backup_retry_delay_caps_at_backup_interval() {
        let interval = Duration::from_hours(1);
        assert_eq!(
            scheduled_full_backup_failure_retry_delay(interval, 10),
            interval
        );
    }
}
