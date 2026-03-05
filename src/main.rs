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
    extract::{ConnectInfo, DefaultBodyLimit},
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

use config::{generate_settings_file_if_missing, CONFIG};
use middleware::AppState;

// ─── Embedded static assets ───────────────────────────────────────────────────
static STYLE_CSS: &str = include_str!("../static/style.css");

// ─── Global terminal state ─────────────────────────────────────────────────────
/// Total HTTP requests handled since startup.
pub static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
/// Requests currently being processed (in-flight).
static IN_FLIGHT: AtomicI64 = AtomicI64::new(0);
/// Multipart file uploads currently in progress.
static ACTIVE_UPLOADS: AtomicI64 = AtomicI64::new(0);
/// Monotonic tick used to animate the upload spinner.
static SPINNER_TICK: AtomicU64 = AtomicU64::new(0);
/// Recently active client IPs (last ~5 min); maps IP-string → last-seen Instant.
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

    let data_dir = std::path::Path::new(&CONFIG.database_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));

    std::fs::create_dir_all(data_dir)?;
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

    // ── External tool detection ────────────────────────────────────────────────
    // ffmpeg: required for video thumbnails (optional — graceful degradation).
    let ffmpeg_status = detect::detect_ffmpeg(CONFIG.require_ffmpeg);
    let ffmpeg_available = ffmpeg_status == detect::ToolStatus::Available;

    // Tor: create hidden-service directory + torrc, launch tor as a background
    // process, and poll for the hostname file (all non-blocking).
    // rsplit(':').next() finds the last colon-delimited segment, which is always
    // the port regardless of whether the host part is IPv4 ("0.0.0.0:8080") or
    // IPv6 ("[::1]:8080").
    let bind_port = CONFIG
        .bind_addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    detect::detect_tor(CONFIG.enable_tor_support, bind_port, data_dir);
    println!();

    let state = AppState {
        db: pool.clone(),
        ffmpeg_available,
    };
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

    // Background: prune stale IPs from ACTIVE_IPS every 5 min
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(300));
        loop {
            iv.tick().await;
            let cutoff = Instant::now() - Duration::from_secs(300);
            ACTIVE_IPS.retain(|_, last_seen| *last_seen > cutoff);
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

    info!("Server shut down gracefully.");
    Ok(())
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/static/style.css", get(serve_css))
        .route("/", get(handlers::board::index))
        .route("/{board}/", get(handlers::board::board_index))
        .route("/{board}/", post(handlers::board::create_thread))
        .route("/{board}/catalog", get(handlers::board::catalog))
        .route("/{board}/search", get(handlers::board::search))
        .route("/{board}/thread/{id}", get(handlers::thread::view_thread))
        .route("/{board}/thread/{id}", post(handlers::thread::post_reply))
        .route("/delete", post(handlers::board::delete_post))
        .route("/vote", post(handlers::thread::vote_handler))
        .nest_service(
            "/boards",
            tower_http::services::ServeDir::new(&CONFIG.upload_dir),
        )
        .route("/admin", get(handlers::admin::admin_index))
        .route("/admin/login", post(handlers::admin::admin_login))
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
        .route("/admin/filter/add", post(handlers::admin::add_filter))
        .route("/admin/filter/remove", post(handlers::admin::remove_filter))
        .route(
            "/admin/site/settings",
            post(handlers::admin::update_site_settings),
        )
        .route("/admin/backup", get(handlers::admin::admin_backup))
        // Disable the global body-size limit for the restore endpoint so that
        // large backup zips are accepted.  In Axum, layers added at the Router
        // level wrap all routes, so a route-level DefaultBodyLimit::max() does
        // NOT override the outer one — it just adds a second (inner) check.
        // DefaultBodyLimit::disable() removes the limit entirely for this route,
        // which is safe here because only authenticated admins reach it.
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
        .with_state(state)
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

// ─── Request tracking middleware ──────────────────────────────────────────────

async fn track_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);

    // Record the client IP for the "users online" display
    if let Some(ci) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        ACTIVE_IPS.insert(ci.0.ip().to_string(), Instant::now());
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

    let resp = next.run(req).await;

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
        println!("║  Create your first admin and boards: ║");
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
    let ip_list: String = {
        let mut ips: Vec<String> = ACTIVE_IPS.iter().map(|e| e.key().clone()).collect();
        ips.sort();
        ips.truncate(5);
        if ips.is_empty() {
            "none".into()
        } else {
            ips.join(", ")
        }
    };

    // Upload progress bar — shown only while uploads are active
    let active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed).max(0) as u64;
    if active_uploads > 0 {
        let tick = SPINNER_TICK.load(Ordering::Relaxed);
        let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = spinners[(tick as usize) % spinners.len()];
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
    println!("   users online: {}  │  IPs: {}", online_count, ip_list);

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
            let p = e.path();
            if p.is_dir() {
                walkdir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

// ─── Startup banner ──────────────────────────────────────────────────────────

fn print_banner() {
    println!("┌─────────────────────────────────────────────────────┐");
    println!(
        "│           {} v{}                    │",
        CONFIG.forum_name,
        env!("CARGO_PKG_VERSION")
    );
    println!("├─────────────────────────────────────────────────────┤");
    println!(
        "│  Bind    {}                              │",
        &CONFIG.bind_addr
    );
    println!("│  DB      {}  │", &CONFIG.database_path);
    println!("│  Uploads {}  │", &CONFIG.upload_dir);
    println!(
        "│  Images  {} MiB max  │  Videos  {} MiB max  │",
        CONFIG.max_image_size / 1024 / 1024,
        CONFIG.max_video_size / 1024 / 1024
    );
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
                    // Snapshot stats without advancing the background state
                    let mut snap = TermStats {
                        prev_req_count: 0,
                        prev_post_count: 0,
                        prev_thread_count: 0,
                        last_tick: start_time,
                    };
                    print_stats(&pool, start_time, &mut snap);
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
    println!(
        "  \x1b[36m║\x1b[0m  [c] create board      [d] delete thread         \x1b[36m║\x1b[0m"
    );
    println!(
        "  \x1b[36m║\x1b[0m  [h] help              [q] quit hint             \x1b[36m║\x1b[0m"
    );
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
    match db::create_board(&conn, &short_lc, &name, &desc, nsfw) {
        Ok(id) => println!(
            "  \x1b[32m✓\x1b[0m Board /{}/  — {}{}  created (id={}).",
            short_lc,
            name,
            if nsfw { " [NSFW]" } else { "" },
            id
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
