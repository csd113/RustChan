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
use tracing::info;
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

    // Tor: create hidden-service directory + torrc, launch tor as a background
    // process, and poll for the hostname file (all non-blocking).
    // Fix #1: derive bind_port from `bind_addr` (which already incorporates
    // port_override) rather than CONFIG.bind_addr.  Previously, starting with
    // `--port 9000` would still pass 8080 to Tor's HiddenServicePort.
    // rsplit_once(':') handles both IPv4 ("0.0.0.0:9000") and IPv6 ("[::1]:9000").
    let bind_port = bind_addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(8080);
    crate::detect::detect_tor(CONFIG.enable_tor_support, bind_port, &data_dir);
    println!();

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
        ffmpeg_webp_available,
        job_queue: {
            let q = std::sync::Arc::new(crate::workers::JobQueue::new(pool.clone()));
            crate::workers::start_worker_pool(&q, ffmpeg_available, ffmpeg_vp9_available);
            q
        },
        backup_progress: std::sync::Arc::new(crate::middleware::BackupProgress::new()),
        chan_ledger: if chan_net {
            Some(std::sync::Arc::new(parking_lot::Mutex::new(
                std::collections::HashSet::<uuid::Uuid>::new(),
            )))
        } else {
            None
        },
    };
    // Keep a reference to the job queue cancel token for graceful shutdown (#7).
    let worker_cancel = state.job_queue.cancel.clone();
    let start_time = Instant::now();

    // Background: purge expired sessions hourly
    {
        let bg = pool.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(3600));
            loop {
                iv.tick().await;
                if let Ok(conn) = bg.get() {
                    match crate::db::purge_expired_sessions(&conn) {
                        Ok(n) if n > 0 => info!("Purged {n} expired sessions"),
                        Err(e) => tracing::error!("Session purge error: {e}"),
                        Ok(_) => {}
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
        tokio::spawn(async move {
            // Stagger the first run by half the interval so it doesn't fire
            // immediately at startup alongside the session purge.
            tokio::time::sleep(Duration::from_secs(interval_secs / 2 + 1)).await;
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                iv.tick().await;
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
        });
    }

    // Background: prune stale IPs from ACTIVE_IPS every 5 min
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(300));
        loop {
            iv.tick().await;
            let cutoff = Instant::now()
                .checked_sub(Duration::from_secs(300))
                .unwrap_or_else(Instant::now);
            ACTIVE_IPS.retain(|_, last_seen| *last_seen > cutoff);
        }
    });

    // Background: prune expired entries from ADMIN_LOGIN_FAILS every 5 min.
    // Prevents unbounded growth under a sustained brute-force attack that
    // never produces a successful login (which would trigger the existing
    // opportunistic prune path inside clear_login_fails).
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(300));
        loop {
            iv.tick().await;
            crate::handlers::admin::prune_login_fails();
        }
    });

    // 1.6: Scheduled database VACUUM — reclaim disk space from deleted posts
    // and threads without requiring manual admin intervention.
    if CONFIG.auto_vacuum_interval_hours > 0 {
        let bg = pool.clone();
        let interval_secs = CONFIG.auto_vacuum_interval_hours * 3600;
        tokio::spawn(async move {
            // Stagger the first run by half the interval to avoid hammering the
            // DB immediately at startup alongside WAL checkpoint and session purge.
            tokio::time::sleep(Duration::from_secs(interval_secs / 2 + 7)).await;
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                iv.tick().await;
                let bg2 = bg.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = bg2.get() {
                        let before = crate::db::get_db_size_bytes(&conn).unwrap_or(0);
                        match crate::db::run_vacuum(&conn) {
                            Ok(()) => {
                                let after = crate::db::get_db_size_bytes(&conn).unwrap_or(0);
                                let saved = before.saturating_sub(after);
                                info!(
                                    "Scheduled VACUUM complete: {} → {} bytes ({} reclaimed)",
                                    before, after, saved
                                );
                            }
                            Err(e) => tracing::warn!("Scheduled VACUUM failed: {e}"),
                        }
                    }
                })
                .await
                .ok();
            }
        });
    }

    // 1.7: Expired poll vote cleanup — purge per-IP vote rows for polls whose
    // expiry is older than poll_cleanup_interval_hours, preventing the
    // poll_votes table from growing indefinitely.
    if CONFIG.poll_cleanup_interval_hours > 0 {
        let bg = pool.clone();
        let interval_secs = CONFIG.poll_cleanup_interval_hours * 3600;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(600)).await; // initial delay
            let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                iv.tick().await;
                let bg2 = bg.clone();
                let retention_cutoff_secs = interval_secs.cast_signed();
                tokio::task::spawn_blocking(move || {
                    if let Ok(conn) = bg2.get() {
                        let cutoff = chrono::Utc::now().timestamp() - retention_cutoff_secs;
                        match crate::db::cleanup_expired_poll_votes(&conn, cutoff) {
                            Ok(n) if n > 0 => {
                                info!("Poll vote cleanup: removed {} expired vote row(s)", n);
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!("Poll vote cleanup failed: {e}"),
                        }
                    }
                })
                .await
                .ok();
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
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1800)).await; // initial stagger
            let mut iv = tokio::time::interval(Duration::from_secs(3600));
            loop {
                iv.tick().await;
                let upload_dir = CONFIG.upload_dir.clone();
                tokio::task::spawn_blocking(move || {
                    crate::workers::evict_thumb_cache(&upload_dir, max_bytes);
                })
                .await
                .ok();
            }
        });
    }

    let app = build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("Listening on  http://{bind_addr}");
    info!("Admin panel   http://{bind_addr}/admin");
    info!("Data dir      {}", data_dir.display());
    println!();

    super::console::spawn_keyboard_handler(pool, start_time);

    if chan_net {
        let chan_addr = crate::config::CONFIG.chan_net_bind.clone();
        let chan_app = crate::chan_net::chan_router(state.clone());
        let chan_listener = tokio::net::TcpListener::bind(&chan_addr).await?;
        tracing::info!("ChanNet API listening on http://{chan_addr}/chan/status");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(chan_listener, chan_app.into_make_service()).await {
                tracing::error!("ChanNet server error: {e}");
            }
        });
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // Signal background workers to drain and exit (#7).
    info!("Signalling background workers to shut down…");
    worker_cancel.cancel();
    // Give workers up to 10 seconds to finish in-flight jobs.
    tokio::time::sleep(Duration::from_secs(10)).await;

    info!("Server shut down gracefully.");
    Ok(())
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
            post(crate::handlers::admin::admin_restore).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/admin/board/backup/{board}",
            get(crate::handlers::admin::board_backup),
        )
        .route(
            "/admin/board/restore",
            post(crate::handlers::admin::board_restore).layer(DefaultBodyLimit::disable()),
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
        // Structured per-request tracing: logs method, URI, status, and
        // latency for every HTTP request.  Spans are emitted at `info` level;
        // failures at `error`.  tower_http filter in RUST_LOG controls noise.
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        uri    = %request.uri(),
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "response sent",
                        );
                    },
                )
                .on_failure(
                    |error: tower_http::classify::ServerErrorsFailureClass,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::error!(
                            %error,
                            latency_ms = latency.as_millis(),
                            "request failed",
                        );
                    },
                ),
        )
        .with_state(state)
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
        () = ctrl_c => info!("Received Ctrl+C"),
        () = terminate => info!("Received SIGTERM"),
    }
}
