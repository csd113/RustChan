// main.rs — Single-binary entry point.
//
// Run modes (via subcommands):
//   rustchan-cli                               → start the web server (default)
//   rustchan-cli admin create-admin  <u> <p>   → create an admin user
//   rustchan-cli admin reset-password <u> <p>  → reset admin password
//   rustchan-cli admin list-admins             → list admins
//   rustchan-cli admin create-board  <s> <n> [desc] [--nsfw]
//   rustchan-cli admin delete-board  <short>
//   rustchan-cli admin list-boards
//   rustchan-cli admin ban    <ip_hash> <reason> [hours]
//   rustchan-cli admin unban  <ban_id>
//   rustchan-cli admin list-bans
//
// Data lives in  <exe-dir>/rustchan-data/   (override with CHAN_DB / CHAN_UPLOADS)
// Static CSS is compiled into the binary — no external files needed.

use axum::{
    extract::DefaultBodyLimit,
    http::{header, StatusCode},
    middleware as axum_middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use clap::{Parser, Subcommand};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::info;
use tracing_subscriber::{filter::EnvFilter, fmt};

mod config;
mod db;
mod detect;
mod error;
mod handlers;
mod middleware;
mod models;
mod templates;
mod utils;
mod workers;

use config::{check_cookie_secret_rotation, generate_settings_file_if_missing, CONFIG};
use middleware::AppState;

// ─── Embedded static assets ───────────────────────────────────────────────────
static STYLE_CSS: &str = include_str!("../static/style.css");
static MAIN_JS: &str = include_str!("../static/main.js");
static THEME_INIT_JS: &str = include_str!("../static/theme-init.js");

// ─── Global terminal state ─────────────────────────────────────────────────────
/// Total HTTP requests handled since startup.
pub static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
/// Requests currently being processed (in-flight).
static IN_FLIGHT: AtomicI64 = AtomicI64::new(0);
/// Multipart file uploads currently in progress.
static ACTIVE_UPLOADS: AtomicI64 = AtomicI64::new(0);
/// Monotonic tick used to animate the upload spinner.
static SPINNER_TICK: AtomicU64 = AtomicU64::new(0);
/// Recently active client IPs (last ~5 min); maps SHA-256(IP) → last-seen Instant.
/// CRIT-5: Keys are hashed so raw IP addresses are never retained in process
/// memory (or coredumps). The count is used for the "users online" display.
static ACTIVE_IPS: Lazy<DashMap<String, Instant>> = Lazy::new(DashMap::new);

// ─── CLI definition ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rustchan-cli",
    about = "Self-contained imageboard server",
    long_about = "RustChan Imageboard — single binary, zero dependencies.\n\
                  Data is stored in ./rustchan-data/ next to the binary.\n\
                  Run without arguments to start the server."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, short = 'p')]
        port: Option<u16>,
    },
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
enum AdminAction {
    CreateAdmin {
        username: String,
        password: String,
    },
    ResetPassword {
        username: String,
        new_password: String,
    },
    ListAdmins,
    CreateBoard {
        short: String,
        name: String,
        #[arg(default_value = "")]
        description: String,
        #[arg(long)]
        nsfw: bool,
        /// Disable image uploads on this board (default: images allowed)
        #[arg(long = "no-images")]
        no_images: bool,
        /// Disable video uploads on this board (default: video allowed)
        #[arg(long = "no-videos")]
        no_videos: bool,
        /// Disable audio uploads on this board (default: audio allowed)
        #[arg(long = "no-audio")]
        no_audio: bool,
    },
    DeleteBoard {
        short: String,
    },
    ListBoards,
    Ban {
        ip_hash: String,
        reason: String,
        hours: Option<i64>,
    },
    Unban {
        ban_id: i64,
    },
    ListBans,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("rustchan=info,tower_http=warn")),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Serve { port: None }) => run_server(None).await,
        Some(Command::Serve { port }) => run_server(port).await,
        Some(Command::Admin { action }) => run_admin(action),
    }
}

// ─── Server mode ─────────────────────────────────────────────────────────────

async fn run_server(port_override: Option<u16>) -> anyhow::Result<()> {
    let early_data_dir = {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
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

    print_banner();

    let bind_addr: String = if let Some(p) = port_override {
        // rsplit_once splits at the LAST colon only, which correctly handles
        // both IPv4 ("0.0.0.0:8080") and IPv6 ("[::1]:8080") bind addresses.
        // rsplit(':').nth(1) was incorrect for IPv6 — it returned "1]" instead
        // of "[::1]" because rsplit splits on every colon in the address.
        let host = CONFIG
            .bind_addr
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or("0.0.0.0");
        format!("{}:{}", host, p)
    } else {
        CONFIG.bind_addr.clone()
    };

    let pool = db::init_pool()?;
    first_run_check(&pool)?;

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
            let name_in_db = db::get_site_setting(&conn, "site_name")
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty());
            let name = if let Some(db_val) = name_in_db {
                db_val
            } else {
                // Seed DB from settings.toml so get_site_name is always consistent.
                let _ = db::set_site_setting(&conn, "site_name", &CONFIG.forum_name);
                CONFIG.forum_name.clone()
            };
            templates::set_live_site_name(&name);

            // Seed subtitle from settings.toml if not yet configured in DB.
            // BUG FIX: get_site_subtitle() always returns a non-empty fallback
            // string, so we must query the DB key directly to detect "never set".
            let subtitle_in_db = db::get_site_setting(&conn, "site_subtitle")
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty());
            let subtitle = if let Some(db_val) = subtitle_in_db {
                db_val
            } else {
                // Nothing in DB — seed from CONFIG (settings.toml).
                let seed = if !CONFIG.initial_site_subtitle.is_empty() {
                    CONFIG.initial_site_subtitle.clone()
                } else {
                    "select board to proceed".to_string()
                };
                let _ = db::set_site_setting(&conn, "site_subtitle", &seed);
                seed
            };
            templates::set_live_site_subtitle(&subtitle);

            // Seed default_theme from settings.toml if not yet configured in DB.
            let default_theme = db::get_default_user_theme(&conn);
            let default_theme = if default_theme.is_empty()
                && !CONFIG.initial_default_theme.is_empty()
                && CONFIG.initial_default_theme != "terminal"
            {
                let _ = db::set_site_setting(&conn, "default_theme", &CONFIG.initial_default_theme);
                CONFIG.initial_default_theme.clone()
            } else {
                default_theme
            };
            templates::set_live_default_theme(&default_theme);
        }
    }
    // ── External tool detection ────────────────────────────────────────────────
    // ffmpeg: required for video thumbnails (optional — graceful degradation).
    let ffmpeg_status = detect::detect_ffmpeg(CONFIG.require_ffmpeg);
    let ffmpeg_available = ffmpeg_status == detect::ToolStatus::Available;

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
    detect::detect_tor(CONFIG.enable_tor_support, bind_port, &data_dir);
    println!();

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
        job_queue: {
            let q = std::sync::Arc::new(workers::JobQueue::new(pool.clone()));
            workers::start_worker_pool(q.clone(), ffmpeg_available);
            q
        },
        backup_progress: std::sync::Arc::new(middleware::BackupProgress::new()),
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
                    match db::purge_expired_sessions(&conn) {
                        Ok(n) if n > 0 => info!("Purged {} expired sessions", n),
                        Err(e) => tracing::error!("Session purge error: {}", e),
                        _ => {}
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
                    match db::run_wal_checkpoint(&conn) {
                        Ok((pages, moved, backfill)) => {
                            tracing::debug!(
                                "WAL checkpoint: {} pages total, {} moved, {} backfilled",
                                pages,
                                moved,
                                backfill
                            );
                        }
                        Err(e) => tracing::warn!("WAL checkpoint failed: {}", e),
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
            let cutoff = Instant::now() - Duration::from_secs(300);
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

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("Listening on  http://{}", bind_addr);
    info!("Admin panel   http://{}/admin", bind_addr);
    info!("Data dir      {}", data_dir.display());
    println!();

    spawn_keyboard_handler(pool, start_time);

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

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/static/main.js", get(serve_main_js))
        .route("/static/theme-init.js", get(serve_theme_init_js))
        .route("/", get(handlers::board::index))
        .route("/{board}", get(handlers::board::board_index))
        .route(
            "/{board}",
            post(handlers::board::create_thread).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route("/{board}/catalog", get(handlers::board::catalog))
        .route("/{board}/archive", get(handlers::board::board_archive))
        .route("/{board}/search", get(handlers::board::search))
        .route("/{board}/thread/{id}", get(handlers::thread::view_thread))
        .route(
            "/{board}/thread/{id}",
            post(handlers::thread::post_reply).layer(DefaultBodyLimit::max(
                CONFIG.max_video_size.max(CONFIG.max_audio_size),
            )),
        )
        .route(
            "/{board}/post/{id}/edit",
            get(handlers::thread::edit_post_get),
        )
        .route(
            "/{board}/post/{id}/edit",
            post(handlers::thread::edit_post_post),
        )
        .route(
            "/report",
            post(handlers::board::file_report).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/appeal",
            post(handlers::board::submit_appeal).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/vote",
            post(handlers::thread::vote_handler).layer(DefaultBodyLimit::max(65_536)),
        )
        .route(
            "/api/post/{board}/{post_id}",
            get(handlers::board::api_post_preview),
        )
        .route(
            "/{board}/post/{post_id}",
            get(handlers::board::redirect_to_post),
        )
        .route(
            "/admin/post/ban-delete",
            post(handlers::admin::admin_ban_and_delete),
        )
        .route(
            "/admin/appeal/dismiss",
            post(handlers::admin::dismiss_appeal),
        )
        .route("/admin/appeal/accept", post(handlers::admin::accept_appeal))
        .route(
            "/{board}/thread/{id}/updates",
            get(handlers::thread::thread_updates),
        )
        // Wildcard board media route: handles all /boards/** requests.
        // For .mp4 files that have been transcoded away to .webm, issues a
        // permanent redirect. All other paths are served directly from disk
        // via tower-http ServeFile (Range, ETag, Content-Type handled correctly).
        .route(
            "/boards/{*media_path}",
            get(handlers::board::serve_board_media),
        )
        .route("/admin", get(handlers::admin::admin_index))
        .route(
            "/admin/login",
            post(handlers::admin::admin_login).layer(DefaultBodyLimit::max(65_536)),
        )
        .route("/admin/logout", post(handlers::admin::admin_logout))
        .route("/admin/panel", get(handlers::admin::admin_panel))
        .route("/admin/board/create", post(handlers::admin::create_board))
        .route("/admin/board/delete", post(handlers::admin::delete_board))
        .route(
            "/admin/board/settings",
            post(handlers::admin::update_board_settings),
        )
        .route("/admin/thread/action", post(handlers::admin::thread_action))
        .route(
            "/admin/thread/delete",
            post(handlers::admin::admin_delete_thread),
        )
        .route(
            "/admin/post/delete",
            post(handlers::admin::admin_delete_post),
        )
        .route("/admin/ban/add", post(handlers::admin::add_ban))
        .route("/admin/ban/remove", post(handlers::admin::remove_ban))
        .route(
            "/admin/report/resolve",
            post(handlers::admin::resolve_report),
        )
        .route("/admin/mod-log", get(handlers::admin::mod_log_page))
        .route("/admin/filter/add", post(handlers::admin::add_filter))
        .route("/admin/filter/remove", post(handlers::admin::remove_filter))
        .route(
            "/admin/site/settings",
            post(handlers::admin::update_site_settings),
        )
        .route("/admin/vacuum", post(handlers::admin::admin_vacuum))
        .route(
            "/admin/ip/{ip_hash}",
            get(handlers::admin::admin_ip_history),
        )
        .route("/admin/backup", get(handlers::admin::admin_backup))
        // Admin restore routes have no body-size cap — backups can be multi-GB
        // and these endpoints require a valid admin session, so there is no
        // anonymous upload risk.
        .route(
            "/admin/restore",
            post(handlers::admin::admin_restore).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/admin/board/backup/{board}",
            get(handlers::admin::board_backup),
        )
        .route(
            "/admin/board/restore",
            post(handlers::admin::board_restore).layer(DefaultBodyLimit::disable()),
        )
        // ── Disk-based backup management routes ──────────────────────────────
        .route(
            "/admin/backup/create",
            post(handlers::admin::create_full_backup),
        )
        .route(
            "/admin/board/backup/create",
            post(handlers::admin::create_board_backup),
        )
        .route(
            "/admin/backup/download/{kind}/{filename}",
            get(handlers::admin::download_backup),
        )
        .route(
            "/admin/backup/progress",
            get(handlers::admin::backup_progress_json),
        )
        .route("/admin/backup/delete", post(handlers::admin::delete_backup))
        .route(
            "/admin/backup/restore-saved",
            post(handlers::admin::restore_saved_full_backup),
        )
        .route(
            "/admin/board/backup/restore-saved",
            post(handlers::admin::restore_saved_board_backup),
        )
        .layer(axum_middleware::from_fn(middleware::rate_limit_middleware))
        .layer(DefaultBodyLimit::max(CONFIG.max_video_size))
        .layer(axum_middleware::from_fn(track_requests))
        // Normalize trailing slashes before routing: redirect /path/ → /path (301).
        // Applied last (outermost) so it fires before any other middleware sees the URI.
        .layer(axum_middleware::from_fn(
            middleware::normalize_trailing_slash,
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
            .map(|v| v.eq_ignore_ascii_case("https"))
            .unwrap_or(false);

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
        let real_ip = middleware::extract_ip(&req);
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
        .map(|ct| ct.contains("multipart/form-data"))
        .unwrap_or(false);

    if is_upload {
        ACTIVE_UPLOADS.fetch_add(1, Ordering::Relaxed);
    }

    use tracing::Instrument as _;
    let resp = next.run(req).instrument(span).await;

    IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    if is_upload {
        ACTIVE_UPLOADS.fetch_sub(1, Ordering::Relaxed);
    }

    resp
}

// ─── First-run check ─────────────────────────────────────────────────────────

fn first_run_check(pool: &db::DbPool) -> anyhow::Result<()> {
    let conn = pool.get()?;
    let board_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
        .unwrap_or(0);
    let admin_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM admin_users", [], |r| r.get(0))
        .unwrap_or(0);

    if board_count == 0 && admin_count == 0 {
        println!();
        println!("╔══════════════════════════════════════════════════════╗");
        println!("║           FIRST RUN — SETUP REQUIRED                 ║");
        println!("╠══════════════════════════════════════════════════════╣");
        println!("║  No boards or admin accounts found.                  ║");
        // Fix #2: original line was only 40 display-columns wide (missing 16 spaces),
        // breaking the box alignment.  Padded to the correct inner width of 54.
        println!("║  Create your first admin and boards:                 ║");
        println!("║                                                      ║");
        println!("║  rustchan-cli admin create-admin admin mypassword    ║");
        println!("║  rustchan-cli admin create-board b Random \"Anything\" ║");
        println!("║  rustchan-cli admin create-board tech Technology     ║");
        println!("╚══════════════════════════════════════════════════════╝");
        println!();
    }
    Ok(())
}

// ─── Terminal stats ───────────────────────────────────────────────────────────

struct TermStats {
    prev_req_count: u64,
    prev_post_count: i64,
    prev_thread_count: i64,
    last_tick: Instant,
}

fn print_stats(pool: &db::DbPool, start: Instant, ts: &mut TermStats) {
    // Uptime
    let uptime = start.elapsed();
    let h = uptime.as_secs() / 3600;
    let m = (uptime.as_secs() % 3600) / 60;

    // req/s — delta since last tick
    let now = Instant::now();
    let elapsed_secs = now.duration_since(ts.last_tick).as_secs_f64().max(0.001);
    ts.last_tick = now;
    let curr_reqs = REQUEST_COUNT.load(Ordering::Relaxed);
    let req_delta = curr_reqs.saturating_sub(ts.prev_req_count);
    let rps = req_delta as f64 / elapsed_secs;
    ts.prev_req_count = curr_reqs;

    // DB query
    let (boards, threads, posts, db_kb, board_stats) = if let Ok(conn) = pool.get() {
        let b: i64 = conn
            .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
            .unwrap_or(0);
        let th: i64 = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0))
            .unwrap_or(0);
        let p: i64 = conn
            .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
            .unwrap_or(0);
        let kb: i64 = {
            let pc: i64 = conn
                .query_row("PRAGMA page_count", [], |r| r.get(0))
                .unwrap_or(0);
            let ps: i64 = conn
                .query_row("PRAGMA page_size", [], |r| r.get(0))
                .unwrap_or(4096);
            pc * ps / 1024
        };
        let bs = get_per_board_stats(&conn);
        (b, th, p, kb, bs)
    } else {
        (0, 0, 0, 0, vec![])
    };

    let upload_mb = dir_size_mb(&CONFIG.upload_dir);

    // New-event flash: bold+yellow when counts increased since last tick
    let new_threads = (threads - ts.prev_thread_count).max(0);
    let new_posts = (posts - ts.prev_post_count).max(0);
    let thread_str = if new_threads > 0 {
        format!("\x1b[1;33mthreads {} (+{})\x1b[0m", threads, new_threads)
    } else {
        format!("threads {}", threads)
    };
    let post_str = if new_posts > 0 {
        format!("\x1b[1;33mposts {} (+{})\x1b[0m", posts, new_posts)
    } else {
        format!("posts {}", posts)
    };
    ts.prev_thread_count = threads;
    ts.prev_post_count = posts;

    // Active connections / users online
    let in_flight = IN_FLIGHT.load(Ordering::Relaxed).max(0) as u64;
    let online_count = ACTIVE_IPS.len();
    // CRIT-5: Keys are SHA-256 hashes — show 8-char prefixes for diagnostics.
    let ip_list: String = {
        let mut hashes: Vec<String> = ACTIVE_IPS
            .iter()
            .map(|e| e.key()[..8].to_string())
            .collect();
        hashes.sort();
        hashes.truncate(5);
        if hashes.is_empty() {
            "none".into()
        } else {
            hashes.join(", ")
        }
    };

    // Upload progress bar — shown only while uploads are active
    let active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed).max(0) as u64;
    if active_uploads > 0 {
        // Fix #5: SPINNER_TICK was read but never written anywhere, so the
        // spinner was permanently frozen on frame 0 ("⠋").  Increment it here,
        // inside the only branch that actually displays the spinner.
        let tick = SPINNER_TICK.fetch_add(1, Ordering::Relaxed);
        let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = spinners
            .get((tick as usize) % spinners.len())
            .copied()
            .unwrap_or("⠋");
        let fill = ((tick % 20) as usize).min(10);
        let bar = format!("{}{}", "█".repeat(fill), "░".repeat(10 - fill));
        println!(
            "  \x1b[36m{} UPLOAD  [{}]  {} file(s) uploading\x1b[0m",
            spin, bar, active_uploads
        );
    }

    // Main stats line
    println!(
        "── STATS  uptime {h}h{m:02}m  │  requests {}  │  \x1b[32m{:.1} req/s\x1b[0m  │  in-flight {}  │  boards {}  {}  {}  │  db {} KiB  uploads {:.1} MiB ──",
        curr_reqs, rps, in_flight, boards, thread_str, post_str, db_kb, upload_mb
    );

    // Users online line
    println!(
        "   users online: {}  │  IPs: {}  │  mem: {} KiB RSS",
        online_count,
        ip_list,
        process_rss_kb()
    );

    // Per-board breakdown
    if !board_stats.is_empty() {
        let segments: Vec<String> = board_stats
            .iter()
            .map(|(short, t, p)| format!("/{}/  threads:{} posts:{}", short, t, p))
            .collect();
        let mut line = String::from("   ");
        let mut line_len = 0usize;
        for seg in &segments {
            if line_len > 0 && line_len + seg.len() + 5 > 110 {
                println!("{}", line);
                line = String::from("   ");
                line_len = 0;
            }
            if line_len > 0 {
                line.push_str("  │  ");
                line_len += 5;
            }
            line.push_str(seg);
            line_len += seg.len();
        }
        if line_len > 0 {
            println!("{}", line);
        }
    }
}

/// Read the process RSS (resident set size) in KiB.
///
/// * Linux  — parsed from `/proc/self/status` (VmRSS field, already in KiB).
/// * macOS  — Fix #11: spawns `ps -o rss= -p <pid>` (output is KiB on macOS).
///   Previously this returned 0 on macOS, showing a misleading
///   `mem: 0 KiB RSS` in the terminal stats display.
/// * Other  — returns 0 rather than showing a misleading value.
fn process_rss_kb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(val) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = val
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    return kb;
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        // `ps -o rss=` outputs the RSS in KiB on macOS (no header when '=' suffix used).
        let pid = std::process::id().to_string();
        if let Ok(out) = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Ok(kb) = s.trim().parse::<u64>() {
                return kb;
            }
        }
    }
    0
}

fn get_per_board_stats(conn: &rusqlite::Connection) -> Vec<(String, i64, i64)> {
    let mut stmt = match conn.prepare(
        "SELECT b.short_name, \
                (SELECT COUNT(*) FROM threads WHERE board_id = b.id) AS tc, \
                (SELECT COUNT(*) FROM posts p \
                   JOIN threads t ON p.thread_id = t.id \
                  WHERE t.board_id = b.id) AS pc \
         FROM boards b ORDER BY b.short_name",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

fn dir_size_mb(path: &str) -> f64 {
    walkdir_size(std::path::Path::new(path)) as f64 / (1024.0 * 1024.0)
}

fn walkdir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            // Fix #10: use file_type() from the DirEntry (does NOT follow
            // symlinks) instead of Path::is_dir() (which does).  A symlink
            // loop via is_dir() causes unbounded recursion and a stack overflow.
            let is_real_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_real_dir {
                walkdir_size(&e.path())
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

// ─── Startup banner ──────────────────────────────────────────────────────────

fn print_banner() {
    // Fix #3: All dynamic values (forum_name, bind_addr, paths, MiB sizes) are
    // padded/truncated to exactly fill the fixed inner width, so the right-hand
    // │ character is always aligned regardless of the actual value length.
    const INNER: usize = 53;

    // Truncate `s` to `width` chars, then right-pad with spaces to `width`.
    let cell = |s: String, width: usize| -> String {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() >= width {
            chars
                .get(..width)
                .map(|s| s.iter().collect())
                .unwrap_or_else(|| s.clone())
        } else {
            format!("{}{}", s, " ".repeat(width - chars.len()))
        }
    };

    let title = cell(
        format!("{} v{}", CONFIG.forum_name, env!("CARGO_PKG_VERSION")),
        INNER - 2, // 2 leading spaces in "│  <title>│"
    );
    let bind = cell(CONFIG.bind_addr.clone(), INNER - 10); // "│  Bind    <val>│"
    let db = cell(CONFIG.database_path.clone(), INNER - 10); // "│  DB      <val>│"
    let upl = cell(CONFIG.upload_dir.clone(), INNER - 10); // "│  Uploads <val>│"
    let img_mib = CONFIG.max_image_size / 1024 / 1024;
    let vid_mib = CONFIG.max_video_size / 1024 / 1024;
    let limits = cell(
        format!("Images {} MiB max  │  Videos {} MiB max", img_mib, vid_mib),
        INNER - 4, // "│  <val>  │"
    );

    println!("┌─────────────────────────────────────────────────────┐");
    println!("│  {}│", title);
    println!("├─────────────────────────────────────────────────────┤");
    println!("│  Bind    {}│", bind);
    println!("│  DB      {}│", db);
    println!("│  Uploads {}│", upl);
    println!("│  {}  │", limits);
    println!("└─────────────────────────────────────────────────────┘");
}

// ─── Keyboard-driven admin console ───────────────────────────────────────────

fn spawn_keyboard_handler(pool: db::DbPool, start_time: Instant) {
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};

        // Small delay so startup messages settle first
        std::thread::sleep(Duration::from_millis(600));
        print_keyboard_help();

        let stdin = std::io::stdin();
        let handle = stdin.lock();
        let mut reader = BufReader::new(handle);

        // Fix #4: TermStats must persist across keypresses so that
        // prev_req_count/prev_post_count/prev_thread_count reflect the values
        // at the *previous* 's' press, not zero.  Initializing inside the
        // match arm made every post/thread appear as "+N new" and reported the
        // lifetime-average req/s instead of the current rate.
        let mut persistent_stats = TermStats {
            prev_req_count: REQUEST_COUNT.load(Ordering::Relaxed),
            prev_post_count: 0,
            prev_thread_count: 0,
            last_tick: Instant::now(),
        };

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF — stdin closed (daemon mode)
                Ok(_) => {}
                Err(_) => break,
            }
            let cmd = line.trim().to_lowercase();
            match cmd.as_str() {
                "s" => {
                    print_stats(&pool, start_time, &mut persistent_stats);
                }
                "l" => kb_list_boards(&pool),
                "c" => kb_create_board(&pool, &mut reader),
                "d" => kb_delete_thread(&pool, &mut reader),
                "h" => print_keyboard_help(),
                "q" => println!("  \x1b[33m[!]\x1b[0m Use Ctrl+C or SIGTERM to stop the server."),
                "" => {}
                other => println!("  Unknown command '{}'. Press [h] for help.", other),
            }
            let _ = std::io::stdout().flush();
        }
    });
}

fn print_keyboard_help() {
    println!();
    println!("  \x1b[36m╔══ Admin Console ════════════════════════════════╗\x1b[0m");
    println!("  \x1b[36m║\x1b[0m  [s] show stats now    [l] list boards          \x1b[36m║\x1b[0m");
    println!("  \x1b[36m║\x1b[0m  [c] create board      [d] delete thread        \x1b[36m║\x1b[0m");
    println!("  \x1b[36m║\x1b[0m  [h] help              [q] quit hint            \x1b[36m║\x1b[0m");
    println!("  \x1b[36m╚═════════════════════════════════════════════════╝\x1b[0m");
    println!();
}

fn kb_list_boards(pool: &db::DbPool) {
    let Ok(conn) = pool.get() else {
        println!("  \x1b[31m[err]\x1b[0m Could not get DB connection.");
        return;
    };
    let boards = match db::get_all_boards(&conn) {
        Ok(b) => b,
        Err(e) => {
            println!("  \x1b[31m[err]\x1b[0m {}", e);
            return;
        }
    };
    if boards.is_empty() {
        println!("  No boards found.");
        return;
    }
    println!("  {:<5} {:<12} {:<24} NSFW", "ID", "Short", "Name");
    println!("  {}", "─".repeat(48));
    for b in &boards {
        println!(
            "  {:<5} /{:<11} {:<24} {}",
            b.id,
            format!("{}/", b.short_name),
            b.name,
            if b.nsfw { "yes" } else { "no" }
        );
    }
    println!();
}

fn kb_create_board(pool: &db::DbPool, reader: &mut dyn std::io::BufRead) {
    use std::io::Write;
    let mut prompt = |msg: &str| -> String {
        print!("  \x1b[36m{}\x1b[0m ", msg);
        let _ = std::io::stdout().flush();
        let mut s = String::new();
        let _ = reader.read_line(&mut s);
        s.trim().to_string()
    };

    let short = prompt("Short name (e.g. 'tech'):");
    if short.is_empty() {
        println!("  Aborted.");
        return;
    }
    let name = prompt("Display name:");
    if name.is_empty() {
        println!("  Aborted.");
        return;
    }
    let desc = prompt("Description (blank = none):");
    let nsfw_raw = prompt("NSFW board? [y/N]:");
    let nsfw = matches!(nsfw_raw.to_lowercase().as_str(), "y" | "yes");

    // Fix #6: prompt for media flags and call create_board_with_media_flags so
    // boards created from the console have the same capabilities as those
    // created via `rustchan-cli admin create-board`.
    let no_images_raw = prompt("Disable images? [y/N]:");
    let no_videos_raw = prompt("Disable video?  [y/N]:");
    let no_audio_raw = prompt("Disable audio?  [y/N]:");
    let allow_images = !matches!(no_images_raw.to_lowercase().as_str(), "y" | "yes");
    let allow_video = !matches!(no_videos_raw.to_lowercase().as_str(), "y" | "yes");
    let allow_audio = !matches!(no_audio_raw.to_lowercase().as_str(), "y" | "yes");

    let short_lc = short.to_lowercase();
    if !short_lc.chars().all(|c| c.is_ascii_alphanumeric())
        || short_lc.is_empty()
        || short_lc.len() > 8
    {
        println!("  \x1b[31m[err]\x1b[0m Short name must be 1-8 alphanumeric characters.");
        return;
    }

    let Ok(conn) = pool.get() else {
        println!("  \x1b[31m[err]\x1b[0m Could not get DB connection.");
        return;
    };
    match db::create_board_with_media_flags(
        &conn,
        &short_lc,
        &name,
        &desc,
        nsfw,
        allow_images,
        allow_video,
        allow_audio,
    ) {
        Ok(id) => println!(
            "  \x1b[32m✓\x1b[0m Board /{}/  — {}{}  created (id={}).  images:{} video:{} audio:{}",
            short_lc,
            name,
            if nsfw { " [NSFW]" } else { "" },
            id,
            if allow_images { "yes" } else { "no" },
            if allow_video { "yes" } else { "no" },
            if allow_audio { "yes" } else { "no" },
        ),
        Err(e) => println!("  \x1b[31m[err]\x1b[0m {}", e),
    }
    println!();
}

fn kb_delete_thread(pool: &db::DbPool, reader: &mut dyn std::io::BufRead) {
    use std::io::Write;

    print!("  \x1b[36mThread ID to delete:\x1b[0m ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = reader.read_line(&mut s);
    let thread_id: i64 = match s.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            println!(
                "  \x1b[31m[err]\x1b[0m '{}' is not a valid thread ID.",
                s.trim()
            );
            return;
        }
    };

    let Ok(conn) = pool.get() else {
        println!("  \x1b[31m[err]\x1b[0m Could not get DB connection.");
        return;
    };
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            [thread_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        println!("  \x1b[31m[err]\x1b[0m Thread {} not found.", thread_id);
        return;
    }

    print!(
        "  \x1b[33mDelete thread {} and all its posts? [y/N]:\x1b[0m ",
        thread_id
    );
    let _ = std::io::stdout().flush();
    let mut confirm = String::new();
    let _ = reader.read_line(&mut confirm);
    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("  Aborted.");
        return;
    }

    match db::delete_thread(&conn, thread_id) {
        Ok(paths) => {
            for p in &paths {
                crate::utils::files::delete_file(&CONFIG.upload_dir, p);
            }
            println!(
                "  \x1b[32m✓\x1b[0m Thread {} deleted ({} file(s) removed).",
                thread_id,
                paths.len()
            );
        }
        Err(e) => println!("  \x1b[31m[err]\x1b[0m {}", e),
    }
    println!();
}

// ─── Graceful shutdown ────────────────────────────────────────────────────────

async fn shutdown_signal() {
    use tokio::signal;
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!("Failed to listen for Ctrl+C: {}", e);
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!("Failed to register SIGTERM handler: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C"),
        _ = terminate => info!("Received SIGTERM"),
    }
}

// ─── Admin CLI mode ───────────────────────────────────────────────────────────

fn run_admin(action: AdminAction) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};
    use chrono::TimeZone;

    let db_path = std::path::Path::new(&CONFIG.database_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = db::init_pool()?;
    let conn = pool.get()?;

    match action {
        AdminAction::CreateAdmin { username, password } => {
            validate_password(&password)?;
            let hash = crypto::hash_password(&password)?;
            let id = db::create_admin(&conn, &username, &hash)?;
            println!("✓ Admin '{}' created (id={}).", username, id);
        }
        AdminAction::ResetPassword {
            username,
            new_password,
        } => {
            validate_password(&new_password)?;
            db::get_admin_by_username(&conn, &username)?
                .ok_or_else(|| anyhow::anyhow!("Admin '{}' not found.", username))?;
            let hash = crypto::hash_password(&new_password)?;
            db::update_admin_password(&conn, &username, &hash)?;
            println!("✓ Password updated for '{}'.", username);
        }
        AdminAction::ListAdmins => {
            let rows = db::list_admins(&conn)?;
            if rows.is_empty() {
                println!("No admins. Run: rustchan-cli admin create-admin <user> <pass>");
            } else {
                println!("{:<6} {:<24} Created", "ID", "Username");
                println!("{}", "-".repeat(45));
                for (id, user, ts) in &rows {
                    let date = chrono::Utc
                        .timestamp_opt(*ts, 0)
                        .single()
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "?".to_string());
                    println!("{:<6} {:<24} {}", id, user, date);
                }
            }
        }
        AdminAction::CreateBoard {
            short,
            name,
            description,
            nsfw,
            no_images,
            no_videos,
            no_audio,
        } => {
            let short = short.to_lowercase();
            if !short.chars().all(|c| c.is_ascii_alphanumeric())
                || short.is_empty()
                || short.len() > 8
            {
                anyhow::bail!("Short name must be 1-8 alphanumeric chars (e.g. 'tech', 'b').");
            }
            let allow_images = !no_images;
            let allow_video = !no_videos;
            let allow_audio = !no_audio;
            let id = db::create_board_with_media_flags(
                &conn,
                &short,
                &name,
                &description,
                nsfw,
                allow_images,
                allow_video,
                allow_audio,
            )?;
            let nsfw_str = if nsfw { " [NSFW]" } else { "" };
            let media_info = format!(
                "  images:{} video:{} audio:{}",
                if allow_images { "yes" } else { "no" },
                if allow_video { "yes" } else { "no" },
                if allow_audio { "yes" } else { "no" },
            );
            println!("✓ Board /{short}/ — {name}{nsfw_str} created (id={id}).{media_info}");
        }
        AdminAction::DeleteBoard { short } => {
            let board = db::get_board_by_short(&conn, &short)?
                .ok_or_else(|| anyhow::anyhow!("Board /{short}/ not found."))?;
            print!("Delete /{short}/ and ALL its content? Type 'yes' to confirm: ");
            use std::io::Write;
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if input.trim() != "yes" {
                println!("Aborted.");
                return Ok(());
            }
            db::delete_board(&conn, board.id)?;
            println!("✓ Board /{short}/ deleted.");
        }
        AdminAction::ListBoards => {
            let boards = db::get_all_boards(&conn)?;
            if boards.is_empty() {
                println!("No boards. Run: rustchan-cli admin create-board <short> <n>");
            } else {
                println!("{:<5} {:<12} {:<22} NSFW", "ID", "Short", "Name");
                println!("{}", "-".repeat(50));
                for b in &boards {
                    println!(
                        "{:<5} /{:<11} {:<22} {}",
                        b.id,
                        format!("{}/", b.short_name),
                        b.name,
                        if b.nsfw { "yes" } else { "no" }
                    );
                }
            }
        }
        AdminAction::Ban {
            ip_hash,
            reason,
            hours,
        } => {
            let expires = hours
                .filter(|&h| h > 0)
                .map(|h| chrono::Utc::now().timestamp() + h.min(87_600).saturating_mul(3600));
            let id = db::add_ban(&conn, &ip_hash, &reason, expires)?;
            let exp_str = expires
                .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "permanent".to_string());
            println!("✓ Ban #{id} added (expires: {exp_str}).");
        }
        AdminAction::Unban { ban_id } => {
            db::remove_ban(&conn, ban_id)?;
            println!("✓ Ban #{ban_id} lifted.");
        }
        AdminAction::ListBans => {
            let bans = db::list_bans(&conn)?;
            if bans.is_empty() {
                println!("No active bans.");
            } else {
                println!(
                    "{:<5} {:<18} {:<28} Expires",
                    "ID", "IP Hash (partial)", "Reason"
                );
                println!("{}", "-".repeat(75));
                for b in &bans {
                    let partial = &b.ip_hash[..b.ip_hash.len().min(16)];
                    let expires = b
                        .expires_at
                        .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "Permanent".to_string());
                    println!(
                        "{:<5} {:<18} {:<28} {}",
                        b.id,
                        partial,
                        b.reason.as_deref().unwrap_or(""),
                        expires
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_password(p: &str) -> anyhow::Result<()> {
    if p.len() < 8 {
        anyhow::bail!("Password must be at least 8 characters.");
    }
    Ok(())
}
