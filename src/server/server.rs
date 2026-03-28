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
    extract::DefaultBodyLimit,
    http::{header, StatusCode},
    middleware as axum_middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tracing::Instrument as _;

use crate::config::{check_cookie_secret_rotation, generate_settings_file_if_missing, CONFIG};
use crate::middleware::AppState;

// ─── Embedded static assets ───────────────────────────────────────────────────
static STYLE_CSS: &str = include_str!("../../static/style.css");
static MAIN_JS: &str = include_str!("../../static/main.js");
static THEME_INIT_JS: &str = include_str!("../../static/theme-init.js");

// ─── Global terminal state ─────────────────────────────────────────────────────
/// Total HTTP requests handled since startup.
pub static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
/// Requests currently being processed (in-flight).
///
/// FIX[AUDIT-1]: Changed from `AtomicI64` to `AtomicU64`.  In-flight request
/// counts are inherently non-negative; using a signed type required defensive
/// `.max(0)` casts at every read site and masked counter underflow bugs.
/// Decrements use `ScopedDecrement` RAII guards (see below) to prevent
/// counter leaks when async futures are cancelled mid-flight.
pub static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
/// Multipart file uploads currently in progress.
///
/// FIX[AUDIT-1]: Same signed→unsigned change as `IN_FLIGHT`.
pub static ACTIVE_UPLOADS: AtomicU64 = AtomicU64::new(0);
/// Monotonic tick used to animate the upload spinner.
pub static SPINNER_TICK: AtomicU64 = AtomicU64::new(0);
/// Recently active client IPs (last ~5 min); maps SHA-256(IP) → last-seen Instant.
/// CRIT-5: Keys are hashed so raw IP addresses are never retained in process
/// memory (or coredumps). The count is used for the "users online" display.
pub static ACTIVE_IPS: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);

// ─── RAII counter guard ───────────────────────────────────────────────────────
//
// FIX[AUDIT-2]: `IN_FLIGHT` and `ACTIVE_UPLOADS` are decremented inside
// `track_requests` *after* `.await`.  If the surrounding future is cancelled
// (e.g. client disconnect, timeout, or panic in a handler), the post-await
// code never runs and the counters permanently over-count.
//
// `ScopedDecrement` ties the decrement to the guard's lifetime so it fires
// unconditionally via `Drop`, even when the future is dropped mid-flight.
// The decrement is saturating to prevent underflow on `AtomicU64`.
struct ScopedDecrement<'a>(&'a AtomicU64);

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
#[allow(clippy::arithmetic_side_effects)]
pub async fn run_server(port_override: Option<u16>, chan_net: bool) -> anyhow::Result<()> {
    // rustls 0.23 requires an explicit process-wide crypto provider.
    // install_default() is idempotent — a second call (e.g. in tests) returns
    // Err but never panics, so the let _ discard is intentional.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let early_data_dir = {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p: &std::path::Path| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        exe.join("rustchan-data")
    };
    std::fs::create_dir_all(&early_data_dir)?;

    generate_settings_file_if_missing();

    // Validate critical configuration values immediately — fail fast with a
    // clear error rather than discovering misconfiguration at runtime (#8).
    CONFIG.validate()?;

    // Fix #9: Path::parent() on a bare filename (e.g. "rustchan.db") returns
    // Some("") rather than None, so the old `unwrap_or(".")` never fired and
    // `create_dir_all("")` would fail with NotFound.  Treat an empty-string
    // parent the same as a missing one.
    let data_dir: std::path::PathBuf = {
        let p = std::path::Path::new(&CONFIG.database_path);
        match p.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        }
    };

    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(&CONFIG.upload_dir)?;

    super::console::print_banner();

    let bind_addr: String = port_override.map_or_else(
        || CONFIG.bind_addr.clone(),
        |p| {
            // rsplit_once splits at the LAST colon only, which correctly handles
            // both IPv4 ("0.0.0.0:8080") and IPv6 ("[::1]:8080") bind addresses.
            // rsplit(':').nth(1) was incorrect for IPv6 — it returned "1]" instead
            // of "[::1]" because rsplit splits on every colon in the address.
            let host = CONFIG
                .bind_addr
                .rsplit_once(':')
                .map_or("0.0.0.0", |(h, _)| h);
            format!("{host}:{p}")
        },
    );

    let pool = crate::db::init_pool()?;
    crate::db::first_run_check(&pool)?;

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

            // Seed default_theme from settings.toml if not yet configured in DB.
            let default_theme = crate::db::get_default_user_theme(&conn);
            let default_theme = if default_theme.is_empty()
                && !CONFIG.initial_default_theme.is_empty()
                && CONFIG.initial_default_theme != "terminal"
            {
                let _ = crate::db::set_site_setting(
                    &conn,
                    "default_theme",
                    &CONFIG.initial_default_theme,
                );
                CONFIG.initial_default_theme.clone()
            } else {
                default_theme
            };
            crate::templates::set_live_default_theme(&default_theme);

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

    // CRIT-1 FIX: Capture JoinHandles from start_worker_pool so the shutdown
    // sequence can await each worker instead of blindly sleeping for 10 s.
    // Previously the return value was silently discarded, making it impossible
    // to know whether in-flight jobs had finished before the process exited.
    let worker_queue = std::sync::Arc::new(crate::workers::JobQueue::new(pool.clone()));
    let worker_handles =
        crate::workers::start_worker_pool(&worker_queue, ffmpeg_available, ffmpeg_vp9_available);

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
        ffmpeg_webp_available,
        job_queue: worker_queue,
        backup_progress: std::sync::Arc::new(crate::middleware::BackupProgress::new()),
        chan_ledger: if chan_net {
            Some(std::sync::Arc::new(parking_lot::Mutex::new(
                std::collections::HashSet::<uuid::Uuid>::new(),
            )))
        } else {
            None
        },
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

    let app = build_router(state.clone());
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
        // CRIT-2 FIX: Wire the same shutdown signal so in-flight federation
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
    // FIX[TLS-1]: The TCP socket is pre-bound here on the *main* task (before
    // spawning) so that any bind failure (port in use, missing CAP_NET_BIND_SERVICE,
    // etc.) is caught immediately with `?` propagation and a clear error message,
    // rather than silently dying inside a spawned future where the error is easy
    // to miss.  The "HTTPS server listening" log is emitted here — after the
    // successful bind — so it is never printed for a socket that wasn't actually
    // bound.  axum_server::from_tcp_rustls accepts the pre-bound std::TcpListener
    // and does not attempt a second bind.
    //
    // FIX[TLS-2]: build_acceptor failure is now a hard error (return Err) instead
    // of a silent log-and-continue.  If TLS is enabled in config but the acceptor
    // cannot be constructed (missing cert files, bad PEM, permission denied on
    // tls/dev/, etc.), the process exits with a clear message rather than running
    // silently as HTTP-only with no indication that HTTPS is absent.
    if CONFIG.tls.enabled {
        let data_dir_tls = data_dir.clone();
        let cancel_tls = worker_cancel.clone();
        let app_tls = build_router(state.clone());

        let https_addr: std::net::SocketAddr = format!("0.0.0.0:{}", CONFIG.tls.port)
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid HTTPS bind address: {e}"))?;

        // FIX[TLS-2]: propagate build_acceptor errors as hard failures.
        let acceptor = crate::tls::build_acceptor(&CONFIG.tls, &data_dir_tls)
            .map_err(|e| anyhow::anyhow!("TLS init failed — cannot start HTTPS listener: {e}"))?;

        match acceptor {
            Some(crate::tls::Acceptor::Static(_arc_acceptor, server_cfg)) => {
                // Acceptor::Static now carries Arc<ServerConfig> directly alongside
                // the TlsAcceptor — pass it straight to axum-server.
                let rustls_cfg = axum_server::tls_rustls::RustlsConfig::from_config(server_cfg);

                // FIX[TLS-1]: pre-bind on the main task so failures surface here.
                let https_tcp = tokio::net::TcpListener::bind(https_addr)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to bind HTTPS listener on {https_addr}: {e}")
                    })?;

                tracing::info!(target: "server", addr = %https_addr, "HTTPS server listening");
                tracing::info!(
                    target: "server",
                    url = %format!("https://{https_addr}/admin"),
                    "Admin panel (HTTPS)"
                );

                tokio::spawn(async move {
                    run_https_static(https_tcp, rustls_cfg, app_tls, cancel_tls).await;
                });
            }

            #[cfg(feature = "tls-acme")]
            Some(crate::tls::Acceptor::Acme(acme_acceptor, server_cfg)) => {
                tokio::spawn(async move {
                    run_https_acme(https_addr, acme_acceptor, server_cfg, app_tls, cancel_tls)
                        .await;
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
        let http_addr: std::net::SocketAddr =
            format!("0.0.0.0:{}", CONFIG.tls.http_port)
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid HTTP redirect bind address: {e}"))?;
        let https_port = CONFIG.tls.port;
        let cancel_redirect = worker_cancel.clone();
        tokio::spawn(async move {
            run_http_redirect(http_addr, https_port, cancel_redirect).await;
        });
    }

    let serve_cancel = worker_cancel.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            () = shutdown_signal() => {}
            () = serve_cancel.cancelled() => {}
        }
    })
    .await?;

    // CRIT-1 FIX: Signal workers and then await each handle with a per-worker
    // timeout, replacing the previous blind 10-second sleep. Each worker is
    // given up to (ffmpeg_timeout + 10)s to finish its in-flight job.
    tracing::info!(target: "server", "Signalling background workers to shut down…");
    worker_cancel.cancel();
    let shutdown_timeout = Duration::from_secs(CONFIG.ffmpeg_timeout_secs + 10);
    for handle in worker_handles {
        let _ = tokio::time::timeout(shutdown_timeout, handle).await;
    }

    // CRIT-3 FIX: worker_cancel.cancel() above already signals the Tor task's
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

// ── HTTPS listener (Static path: self-signed or manual PEM) ──────────────────
//
// Uses axum-server which preserves ConnectInfo<SocketAddr> so the IP-banning
// and rate-limiting middleware in middleware/mod.rs continues to work correctly.
//
// FIX[TLS-1]: Accepts a pre-bound TcpListener instead of a SocketAddr so the
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
        handle_clone.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
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

    if let Err(e) = axum_server::from_tcp_rustls(std_listener, tls_config)
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
    https_addr: std::net::SocketAddr,
    acme_acceptor: std::sync::Arc<rustls_acme::AcmeAcceptor>,
    server_cfg: std::sync::Arc<rustls::ServerConfig>,
    app: axum::Router,
    cancel: tokio_util::sync::CancellationToken,
) {
    use hyper::server::conn::http1;
    use hyper_util::rt::TokioIo;
    use tower::Service as _;

    let listener = match tokio::net::TcpListener::bind(https_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(target: "server", error = %e, "Failed to bind HTTPS/ACME listener");
            return;
        }
    };
    tracing::info!(target: "server", addr = %https_addr, "HTTPS/ACME server listening");
    tracing::info!(
        target: "server",
        url = %format!("https://{https_addr}/admin"),
        "Admin panel (HTTPS/ACME)"
    );

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
    http_addr: std::net::SocketAddr,
    https_port: u16,
    cancel: tokio_util::sync::CancellationToken,
) {
    use axum::{extract::Request, response::Redirect, routing::any};

    let redirect_app = axum::Router::new().route(
        "/{*path}",
        any(move |req: Request| async move {
            let path = req
                .uri()
                .path_and_query()
                .map_or("/", axum::http::uri::PathAndQuery::as_str);
            let host = req
                .headers()
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
                .and_then(|h| h.split(':').next())
                .unwrap_or("localhost");
            Redirect::permanent(&format!("https://{host}:{https_port}{path}"))
        }),
    );

    let listener = match tokio::net::TcpListener::bind(http_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                target: "server",
                error = %e,
                "Failed to bind HTTP→HTTPS redirect listener"
            );
            return;
        }
    };
    tracing::info!(target: "server", addr = %http_addr, "HTTP→HTTPS redirect listening");

    axum::serve(listener, redirect_app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .ok();
}

#[allow(clippy::too_many_lines)]
fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .route("/", get(crate::handlers::board::index))
        .route("/{board}", get(crate::handlers::board::board_index))
        .route(
            "/{board}",
            post(crate::handlers::board::create_thread).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route("/{board}/catalog", get(crate::handlers::board::catalog))
        .route(
            "/{board}/archive",
            get(crate::handlers::board::board_archive),
        )
        .route("/{board}/search", get(crate::handlers::board::search))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .route(
            "/{board}/thread/{id}",
            post(crate::handlers::thread::post_reply).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route(
            "/{board}/post/{id}/edit",
            get(crate::handlers::thread::edit_post_get),
        )
        .route(
            "/{board}/post/{id}/edit",
            post(crate::handlers::thread::edit_post_post),
        )
        .route(
            "/report",
            post(crate::handlers::board::file_report).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/appeal",
            post(crate::handlers::board::submit_appeal).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/vote",
            post(crate::handlers::thread::vote_handler).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/api/post/{board}/{post_id}",
            get(crate::handlers::board::api_post_preview),
        )
        .route(
            "/{board}/post/{post_id}",
            get(crate::handlers::board::redirect_to_post),
        )
        .route(
            "/admin/post/ban-delete",
            post(crate::handlers::admin::admin_ban_and_delete),
        )
        .route(
            "/admin/appeal/dismiss",
            post(crate::handlers::admin::dismiss_appeal),
        )
        .route(
            "/admin/appeal/accept",
            post(crate::handlers::admin::accept_appeal),
        )
        .route(
            "/{board}/thread/{id}/updates",
            get(crate::handlers::thread::thread_updates),
        )
        // Wildcard board media route: handles all /boards/** requests.
        // For .mp4 files that have been transcoded away to .webm, issues a
        // permanent redirect. All other paths are served directly from disk
        // via tower-http ServeFile (Range, ETag, Content-Type handled correctly).
        .route(
            "/boards/{*media_path}",
            get(crate::handlers::board::serve_board_media),
        )
        .route("/admin", get(crate::handlers::admin::admin_index))
        .route(
            "/admin/login",
            post(crate::handlers::admin::admin_login).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/admin/logout", post(crate::handlers::admin::admin_logout))
        .route("/admin/panel", get(crate::handlers::admin::admin_panel))
        .route(
            "/admin/board/create",
            post(crate::handlers::admin::create_board),
        )
        .route(
            "/admin/board/delete",
            post(crate::handlers::admin::delete_board),
        )
        .route(
            "/admin/board/settings",
            post(crate::handlers::admin::update_board_settings),
        )
        .route(
            "/admin/thread/action",
            post(crate::handlers::admin::thread_action),
        )
        .route(
            "/admin/thread/delete",
            post(crate::handlers::admin::admin_delete_thread),
        )
        .route(
            "/admin/post/delete",
            post(crate::handlers::admin::admin_delete_post),
        )
        .route("/admin/ban/add", post(crate::handlers::admin::add_ban))
        .route(
            "/admin/ban/remove",
            post(crate::handlers::admin::remove_ban),
        )
        .route(
            "/admin/report/resolve",
            post(crate::handlers::admin::resolve_report),
        )
        .route("/admin/mod-log", get(crate::handlers::admin::mod_log_page))
        .route(
            "/admin/filter/add",
            post(crate::handlers::admin::add_filter),
        )
        .route(
            "/admin/filter/remove",
            post(crate::handlers::admin::remove_filter),
        )
        .route(
            "/admin/site/settings",
            post(crate::handlers::admin::update_site_settings),
        )
        .route("/admin/vacuum", post(crate::handlers::admin::admin_vacuum))
        .route(
            "/admin/ip/{ip_hash}",
            get(crate::handlers::admin::admin_ip_history),
        )
        .route("/admin/backup", get(crate::handlers::admin::admin_backup))
        // Admin restore routes have no body-size cap — backups can be multi-GB
        // and these endpoints require a valid admin session, so there is no
        // anonymous upload risk.
        .route(
            "/admin/restore",
            post(crate::handlers::admin::admin_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)), // 20 GiB — large but bounded
        )
        .route(
            "/admin/board/backup/{board}",
            get(crate::handlers::admin::board_backup),
        )
        .route(
            "/admin/board/restore",
            post(crate::handlers::admin::board_restore)
                .layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)), // 20 GiB — large but bounded
        )
        // ── Disk-based backup management routes ──────────────────────────────
        .route(
            "/admin/backup/create",
            post(crate::handlers::admin::create_full_backup),
        )
        .route(
            "/admin/board/backup/create",
            post(crate::handlers::admin::create_board_backup),
        )
        .route(
            "/admin/backup/download/{kind}/{filename}",
            get(crate::handlers::admin::download_backup),
        )
        .route(
            "/admin/backup/progress",
            get(crate::handlers::admin::backup_progress_json),
        )
        .route(
            "/admin/backup/delete",
            post(crate::handlers::admin::delete_backup),
        )
        .route(
            "/admin/backup/restore-saved",
            post(crate::handlers::admin::restore_saved_full_backup),
        )
        .route(
            "/admin/board/backup/restore-saved",
            post(crate::handlers::admin::restore_saved_board_backup),
        )
        .layer(axum_middleware::from_fn(
            crate::middleware::rate_limit_middleware,
        ))
        .layer(DefaultBodyLimit::max(CONFIG.max_video_size))
        .layer(axum_middleware::from_fn(track_requests))
        // 3.3: Gzip/Brotli/Zstd response compression.  HTML pages compress 5–10×
        // with gzip and even better with Brotli.  tower-http respects the client's
        // Accept-Encoding header and negotiates the best supported algorithm.
        // Applied before the trailing-slash normaliser so compressed responses
        // are served correctly for all paths including redirects.
        .layer(tower_http::compression::CompressionLayer::new())
        // Normalize trailing slashes before routing: redirect /path/ → /path (301).
        // Applied last (outermost) so it fires before any other middleware sees the URI.
        .layer(axum_middleware::from_fn(
            crate::middleware::normalize_trailing_slash,
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-content-type-options"),
            header::HeaderValue::from_static("nosniff"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-frame-options"),
            header::HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("referrer-policy"),
            header::HeaderValue::from_static("same-origin"),
        ))
        // FIX[NEW-H1]: 'unsafe-inline' removed from script-src.  All JavaScript
        // has been moved to /static/main.js (loaded with 'self') and
        // /static/theme-init.js.  Inline event handlers (onclick= etc.) have
        // been replaced with data-* attributes handled by main.js event
        // delegation, so no inline script execution is required.
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("content-security-policy"),
            header::HeaderValue::from_static(
                "default-src 'self'; \
                 script-src 'self'; \
                 style-src 'self' 'unsafe-inline'; \
                 img-src 'self' data: blob: https://img.youtube.com; \
                 media-src 'self' blob:; \
                 font-src 'self'; \
                 connect-src 'self'; \
                 frame-src https://www.youtube-nocookie.com https://streamable.com; \
                 frame-ancestors 'none'; \
                 object-src 'none'; \
                 base-uri 'self'",
            ),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("permissions-policy"),
            header::HeaderValue::from_static(
                "geolocation=(), camera=(), microphone=(), payment=()",
            ),
        ))
        // Fix #8: HSTS (RFC 6797 §7.2) MUST only be sent over HTTPS.
        // Sending it over plain HTTP (localhost dev, Tor .onion) is incorrect
        // and can cause Tor-aware clients to misbehave.  The middleware below
        // checks both the request scheme and the X-Forwarded-Proto header
        // (set by TLS-terminating proxies) before adding the header.
        .layer(axum_middleware::from_fn(hsts_middleware))
        // MED-4 FIX: Hard per-request timeout to prevent slow-loris style
        // attacks from holding async tasks indefinitely. Clients that open a
        // connection but never finish sending the request body will be cut off
        // after 30 seconds. This covers all routes including file upload
        // endpoints where the multipart streaming loop could block forever.
        //
        // TimeoutLayer injects Box<dyn Error> into the service error type when
        // a timeout fires. Axum's Router::layer requires all errors to be
        // Into<Infallible>, so HandleErrorLayer must wrap TimeoutLayer and both
        // must be bundled inside a ServiceBuilder — applying them as separate
        // .layer() calls leaves the intermediate error type unresolved and
        // causes E0277. ServiceBuilder fuses them into a single layer whose
        // output error type is Infallible.
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(
                    |_err: tower::BoxError| async {
                        (axum::http::StatusCode::REQUEST_TIMEOUT, "Request timed out")
                    },
                ))
                .layer(tower::timeout::TimeoutLayer::new(Duration::from_secs(30))),
        )
        // HTTP tracing: silent for normal responses, loud for failures.
        //
        // on_response is intentionally omitted — logging every 200/304 at INFO
        // floods the terminal with one line per user action and buries real events.
        // Operators who want per-request access logs can set RUST_LOG=tower_http=debug.
        //
        // on_failure fires for 5xx responses and transport errors at ERROR level.
        // on_eos fires when a streaming response body closes unexpectedly.
        // make_span_with creates a DEBUG span so req_id / method / uri are available
        // in the file log for correlation without appearing on the terminal at INFO.
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::debug_span!(
                        "http",
                        method = %request.method(),
                        uri    = %request.uri(),
                    )
                })
                // No on_response — 200/304/etc. are completely silent at INFO level.
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::TRACE),
                )
                .on_failure(
                    |error: tower_http::classify::ServerErrorsFailureClass,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::error!(
                            target: "server",
                            %error,
                            latency_ms = latency.as_millis(),
                            "request failed",
                        );
                    },
                ),
        )
        // HIGH-9: Inject `Onion-Location` response header when the onion service
        // is active. Tor Browser reads this header on clearnet responses and
        // prompts the user to switch to the .onion address automatically.
        // Only injected when enable_tor_support=true and the address is known.
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            onion_location_middleware,
        ))
        .with_state(state)
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

    let mut resp = next.run(req).await;

    if let Some(addr) = maybe_addr {
        // Only inject on HTML responses — static assets, JSON, and media do
        // not benefit from the header and it adds noise to every response.
        let is_html = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("text/html"));

        if is_html {
            if let Ok(val) = header::HeaderValue::from_str(&format!("http://{addr}")) {
                resp.headers_mut()
                    .insert(header::HeaderName::from_static("onion-location"), val);
            }
        }
    }

    resp
}

/// Middleware that adds `Strict-Transport-Security` only when the connection
/// is confirmed to be HTTPS (RFC 6797 §7.2).  Checks both the URI scheme
/// (set by some reverse proxies) and the `X-Forwarded-Proto` header.
async fn hsts_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_https = req.uri().scheme_str() == Some("https")
        || req
            .headers()
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"));

    let mut resp = next.run(req).await;
    if is_https {
        resp.headers_mut().insert(
            header::HeaderName::from_static("strict-transport-security"),
            header::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    resp
}

async fn serve_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        STYLE_CSS,
    )
}

async fn serve_main_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        MAIN_JS,
    )
}

async fn serve_theme_init_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        THEME_INIT_JS,
    )
}

// ─── Request tracking middleware ──────────────────────────────────────────────

async fn track_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);

    // FIX[AUDIT-2]: Bind the in-flight decrement to a RAII guard so it fires
    // even if this future is cancelled (e.g. client disconnect, handler panic).
    let _in_flight_guard = ScopedDecrement(&IN_FLIGHT);

    // Attach a per-request UUID to every tracing span so correlated log lines
    // can be grouped by request even under concurrent load (#12).
    let req_id = uuid::Uuid::new_v4();
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let span = tracing::info_span!(
        "request",
        req_id = %req_id,
        method = %method,
        path  = %path,
    );

    // Record the client IP for the "users online" display.
    // CRIT-5: Store a SHA-256 hash of the IP (not the raw address) to avoid
    // retaining PII in process memory and coredumps.
    // CRIT-2: Use extract_ip() so proxy-forwarded real IPs are used instead
    // of the raw socket address (which would always be the proxy's IP).
    // Cap at 10,000 entries to prevent unbounded memory growth under a
    // Sybil/bot attack rotating IPs (#11). The count is cosmetic so
    // dropping inserts beyond the cap has no functional impact.
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

    // Detect file uploads by Content-Type
    let is_upload = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("multipart/form-data"));

    // FIX[AUDIT-2]: Bind upload decrement to a RAII guard for the same reason.
    // Option<ScopedDecrement> is None when is_upload is false — zero-cost branch.
    let _upload_guard = is_upload.then(|| {
        ACTIVE_UPLOADS.fetch_add(1, Ordering::Relaxed);
        ScopedDecrement(&ACTIVE_UPLOADS)
    });

    next.run(req).instrument(span).await
}

// ─── Graceful shutdown ────────────────────────────────────────────────────────

async fn shutdown_signal() {
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
        () = ctrl_c =>   tracing::info!(target: "server", signal = "SIGINT",  "Shutdown signal received"),
        () = terminate => tracing::info!(target: "server", signal = "SIGTERM", "Shutdown signal received"),
    }
}
