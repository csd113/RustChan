// main.rs — Single-binary entry point.
//
// Run modes (via subcommands):
//   chan                             → start the web server (default)
//   chan admin create-admin  <u> <p> → create an admin user
//   chan admin reset-password <u> <p>→ reset admin password
//   chan admin list-admins           → list admins
//   chan admin create-board  <s> <n> [desc] [--nsfw]
//   chan admin delete-board  <short>
//   chan admin list-boards
//   chan admin ban    <ip_hash> <reason> [hours]
//   chan admin unban  <ban_id>
//   chan admin list-bans
//
// Data lives in  <exe-dir>/chan-data/   (override with CHAN_DB / CHAN_UPLOADS)
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
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::info;
use tracing_subscriber::{fmt, filter::EnvFilter};

mod config;
mod db;
mod error;
mod handlers;
mod middleware;
mod models;
mod templates;
mod utils;

use config::{CONFIG, generate_settings_file_if_missing};
use middleware::AppState;

// ─── Embedded static assets ───────────────────────────────────────────────────
// The CSS is compiled into the binary at build time — zero external files.
static STYLE_CSS: &str = include_str!("../static/style.css");

// ─── Global request counter (for terminal stats) ──────────────────────────────
pub static REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);

// ─── CLI definition ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name  = "chan",
    about = "Self-contained imageboard server",
    long_about = "Chan Imageboard — single binary, zero dependencies.\n\
                  Data is stored in ./chan-data/ next to the binary.\n\
                  Run without arguments to start the server."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the web server (default when no subcommand given)
    Serve {
        /// Override the port to listen on (e.g. --port 3000). Falls back to
        /// CHAN_BIND env var, then default 8080.
        #[arg(long, short = 'p')]
        port: Option<u16>,
    },

    /// Administration commands
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
enum AdminAction {
    /// Create a new admin user
    CreateAdmin { username: String, password: String },

    /// Reset an existing admin's password
    ResetPassword { username: String, new_password: String },

    /// List all admin accounts
    ListAdmins,

    /// Create a board (e.g. tech "Technology" "Programming talk")
    CreateBoard {
        short: String,
        name: String,
        #[arg(default_value = "")]
        description: String,
        #[arg(long)]
        nsfw: bool,
    },

    /// Delete a board and all its content (asks for confirmation)
    DeleteBoard { short: String },

    /// List all boards
    ListBoards,

    /// Ban an IP hash (find it in the DB). Omit hours for permanent.
    Ban {
        ip_hash: String,
        reason: String,
        hours: Option<i64>,
    },

    /// Lift a ban by its ID (see list-bans)
    Unban { ban_id: i64 },

    /// Show all active bans
    ListBans,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging: compact stdout format; override with RUST_LOG env var
    fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("chan=info,tower_http=warn")),
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
    // Create chan-data/ first — settings.toml lives there
    let early_data_dir = {
        let exe = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        exe.join("chan-data")
    };
    std::fs::create_dir_all(&early_data_dir)?;

    // Generate settings.toml on first run (must happen before CONFIG is accessed)
    generate_settings_file_if_missing();

    // Ensure all data directories exist
    let data_dir = std::path::Path::new(&CONFIG.database_path)
        .parent()
        .unwrap_or(std::path::Path::new("."));

    std::fs::create_dir_all(data_dir)?;
    std::fs::create_dir_all(&CONFIG.upload_dir)?;
    std::fs::create_dir_all(format!("{}/thumbs", CONFIG.upload_dir))?;

    print_banner();

    // Resolve the effective bind address — CLI --port wins over CHAN_BIND
    let bind_addr: String = if let Some(p) = port_override {
        // Replace just the port component while keeping the host
        let host = CONFIG.bind_addr.split(':').next().unwrap_or("0.0.0.0");
        format!("{}:{}", host, p)
    } else {
        CONFIG.bind_addr.clone()
    };

    let pool = db::init_pool()?;
    first_run_check(&pool)?;

    let state = AppState { db: pool.clone() };
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

    // Background: print stats to terminal every 60 s
    {
        let bg = pool.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(60));
            iv.tick().await; // skip the immediate first tick
            loop {
                iv.tick().await;
                print_stats(&bg, start_time);
            }
        });
    }

    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("Listening on  http://{}", bind_addr);
    info!("Admin panel   http://{}/admin", bind_addr);
    info!("Data dir      {}", data_dir.display());
    println!(); // breathing room after startup block

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
        // ── Embedded CSS ──
        .route("/static/style.css", get(serve_css))
        // ── Home ──
        .route("/", get(handlers::board::index))
        // ── Board ──
        .route("/:board/",        get(handlers::board::board_index))
        .route("/:board/",        post(handlers::board::create_thread))
        .route("/:board/catalog", get(handlers::board::catalog))
        .route("/:board/search",  get(handlers::board::search))
        // ── Thread ──
        .route("/:board/thread/:id", get(handlers::thread::view_thread))
        .route("/:board/thread/:id", post(handlers::thread::post_reply))
        // ── User deletion ──
        .route("/delete", post(handlers::board::delete_post))
        // ── Uploads (served from chan-data/uploads/) ──
        .nest_service("/uploads", tower_http::services::ServeDir::new(&CONFIG.upload_dir))
        // ── Admin ──
        .route("/admin",                 get(handlers::admin::admin_index))
        .route("/admin/login",           post(handlers::admin::admin_login))
        .route("/admin/logout",          post(handlers::admin::admin_logout))
        .route("/admin/panel",           get(handlers::admin::admin_panel))
        .route("/admin/board/create",    post(handlers::admin::create_board))
        .route("/admin/board/delete",    post(handlers::admin::delete_board))
        .route("/admin/board/settings",  post(handlers::admin::update_board_settings))
        .route("/admin/thread/action",   post(handlers::admin::thread_action))
        .route("/admin/thread/delete",   post(handlers::admin::admin_delete_thread))
        .route("/admin/post/delete",     post(handlers::admin::admin_delete_post))
        .route("/admin/ban/add",         post(handlers::admin::add_ban))
        .route("/admin/ban/remove",      post(handlers::admin::remove_ban))
        .route("/admin/filter/add",      post(handlers::admin::add_filter))
        .route("/admin/filter/remove",   post(handlers::admin::remove_filter))
        // ── Rate limiting ──
        .layer(axum_middleware::from_fn(middleware::rate_limit_middleware))
        // ── Body size limit — set to max video size ──
        .layer(DefaultBodyLimit::max(CONFIG.max_video_size))
        // ── Request counter ──
        .layer(axum_middleware::from_fn(count_requests))
        .with_state(state)
}

/// Serve the embedded stylesheet — no external file needed.
async fn serve_css() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8"),
         (header::CACHE_CONTROL, "public, max-age=86400")],
        STYLE_CSS,
    )
}

/// Increment the global request counter on every request.
async fn count_requests(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    next.run(req).await
}

// ─── First-run check ─────────────────────────────────────────────────────────

fn first_run_check(pool: &db::DbPool) -> anyhow::Result<()> {
    let conn = pool.get()?;
    let board_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM boards", [], |r| r.get(0)
    ).unwrap_or(0);

    let admin_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM admin_users", [], |r| r.get(0)
    ).unwrap_or(0);

    if board_count == 0 && admin_count == 0 {
        println!();
        println!("╔══════════════════════════════════════════════════╗");
        println!("║           FIRST RUN — SETUP REQUIRED             ║");
        println!("╠══════════════════════════════════════════════════╣");
        println!("║  No boards or admin accounts found.              ║");
        println!("║  Create your first admin and boards:             ║");
        println!("║                                                  ║");
        println!("║  chan admin create-admin admin mypassword        ║");
        println!("║  chan admin create-board b Random \"Anything\"     ║");
        println!("║  chan admin create-board tech Technology \"Dev\"   ║");
        println!("╚══════════════════════════════════════════════════╝");
        println!();
    }

    Ok(())
}

// ─── Terminal stats ───────────────────────────────────────────────────────────

fn print_stats(pool: &db::DbPool, start: Instant) {
    let uptime = start.elapsed();
    let h = uptime.as_secs() / 3600;
    let m = (uptime.as_secs() % 3600) / 60;
    let reqs = REQUEST_COUNT.load(Ordering::Relaxed);

    let (boards, threads, posts, db_kb) = if let Ok(conn) = pool.get() {
        let b: i64  = conn.query_row("SELECT COUNT(*) FROM boards",  [], |r| r.get(0)).unwrap_or(0);
        let th: i64 = conn.query_row("SELECT COUNT(*) FROM threads", [], |r| r.get(0)).unwrap_or(0);
        let p: i64  = conn.query_row("SELECT COUNT(*) FROM posts",   [], |r| r.get(0)).unwrap_or(0);
        let kb: i64 = {
            let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).unwrap_or(0);
            let page_size: i64  = conn.query_row("PRAGMA page_size",  [], |r| r.get(0)).unwrap_or(4096);
            page_count * page_size / 1024
        };
        (b, th, p, kb)
    } else {
        (0, 0, 0, 0)
    };

    let upload_mb = dir_size_mb(&CONFIG.upload_dir);

    println!(
        "── STATS  uptime {h}h{m:02}m  │  requests {reqs}  │  boards {boards}  threads {threads}  posts {posts}  │  db {db_kb} KiB  uploads {upload_mb:.1} MiB ──"
    );
}

fn dir_size_mb(path: &str) -> f64 {
    let bytes = walkdir_size(std::path::Path::new(path));
    bytes as f64 / (1024.0 * 1024.0)
}

fn walkdir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries.flatten().map(|e| {
        let p = e.path();
        if p.is_dir() { walkdir_size(&p) }
        else { e.metadata().map(|m| m.len()).unwrap_or(0) }
    }).sum()
}

// ─── Startup banner ──────────────────────────────────────────────────────────

fn print_banner() {
    println!("┌─────────────────────────────────────────────────────┐");
    println!("│           {} v{}                    │", CONFIG.forum_name, env!("CARGO_PKG_VERSION"));
    println!("├─────────────────────────────────────────────────────┤");
    println!("│  Bind    {}                              │", &CONFIG.bind_addr);
    println!("│  DB      {}  │", &CONFIG.database_path);
    println!("│  Uploads {}  │", &CONFIG.upload_dir);
    println!("│  Images  {} MiB max  │  Videos  {} MiB max  │",
        CONFIG.max_image_size / 1024 / 1024,
        CONFIG.max_video_size / 1024 / 1024);
    println!("└─────────────────────────────────────────────────────┘");
}

// ─── Graceful shutdown ────────────────────────────────────────────────────────

async fn shutdown_signal() {
    use tokio::signal;
    // FIX[MEDIUM-5]: Replaced .expect() with graceful error handling.
    // A panic in the shutdown handler would crash the server without
    // completing in-flight requests. We log the error and continue instead.
    let ctrl_c = async {
        if let Err(e) = signal::ctrl_c().await {
            tracing::error!("Failed to listen for Ctrl+C: {}", e);
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => { sig.recv().await; }
            Err(e) => {
                tracing::error!("Failed to register SIGTERM handler: {}", e);
                // If we can't register SIGTERM, wait forever (Ctrl+C still works)
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c    => info!("Received Ctrl+C"),
        _ = terminate => info!("Received SIGTERM"),
    }
}

// ─── Admin CLI mode ───────────────────────────────────────────────────────────

fn run_admin(action: AdminAction) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};
    use chrono::TimeZone;

    // Ensure the data directory exists before opening the DB
    let db_path = std::path::Path::new(&CONFIG.database_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = db::init_pool()?;
    let conn = pool.get()?;

    match action {
        // ── create-admin ───────────────────────────────────────────────────
        AdminAction::CreateAdmin { username, password } => {
            validate_password(&password)?;
            let hash = crypto::hash_password(&password)?;
            let id = db::create_admin(&conn, &username, &hash)?;
            println!("✓ Admin '{}' created (id={}).", username, id);
        }

        // ── reset-password ─────────────────────────────────────────────────
        AdminAction::ResetPassword { username, new_password } => {
            validate_password(&new_password)?;
            db::get_admin_by_username(&conn, &username)?
                .ok_or_else(|| anyhow::anyhow!("Admin '{}' not found.", username))?;
            let hash = crypto::hash_password(&new_password)?;
            db::update_admin_password(&conn, &username, &hash)?;
            println!("✓ Password updated for '{}'.", username);
        }

        // ── list-admins ────────────────────────────────────────────────────
        AdminAction::ListAdmins => {
            let rows = db::list_admins(&conn)?;
            if rows.is_empty() {
                println!("No admins. Run: chan admin create-admin <user> <pass>");
            } else {
                println!("{:<6} {:<24} Created", "ID", "Username");
                println!("{}", "-".repeat(45));
                for (id, user, ts) in &rows {
                    let date = chrono::Utc.timestamp_opt(*ts, 0)
                        .single()
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_else(|| "?".to_string());
                    println!("{:<6} {:<24} {}", id, user, date);
                }
            }
        }

        // ── create-board ───────────────────────────────────────────────────
        AdminAction::CreateBoard { short, name, description, nsfw } => {
            let short = short.to_lowercase();
            if !short.chars().all(|c| c.is_ascii_alphanumeric()) || short.is_empty() || short.len() > 8 {
                anyhow::bail!("Short name must be 1-8 alphanumeric chars (e.g. 'tech', 'b').");
            }
            let id = db::create_board(&conn, &short, &name, &description, nsfw)?;
            let nsfw_str = if nsfw { " [NSFW]" } else { "" };
            println!("✓ Board /{short}/ — {name}{nsfw_str} created (id={id}).");
        }

        // ── delete-board ───────────────────────────────────────────────────
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

        // ── list-boards ────────────────────────────────────────────────────
        AdminAction::ListBoards => {
            let boards = db::get_all_boards(&conn)?;
            if boards.is_empty() {
                println!("No boards. Run: chan admin create-board <short> <name>");
            } else {
                println!("{:<5} {:<12} {:<22} NSFW", "ID", "Short", "Name");
                println!("{}", "-".repeat(50));
                for b in &boards {
                    println!("{:<5} /{:<11} {:<22} {}",
                        b.id, format!("{}/", b.short_name), b.name,
                        if b.nsfw { "yes" } else { "no" }
                    );
                }
            }
        }

        // ── ban ────────────────────────────────────────────────────────────
        AdminAction::Ban { ip_hash, reason, hours } => {
            let expires = hours.filter(|&h| h > 0)
                .map(|h| chrono::Utc::now().timestamp() + h * 3600);
            let id = db::add_ban(&conn, &ip_hash, &reason, expires)?;
            let exp_str = expires
                .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "permanent".to_string());
            println!("✓ Ban #{id} added (expires: {exp_str}).");
        }

        // ── unban ──────────────────────────────────────────────────────────
        AdminAction::Unban { ban_id } => {
            db::remove_ban(&conn, ban_id)?;
            println!("✓ Ban #{ban_id} lifted.");
        }

        // ── list-bans ──────────────────────────────────────────────────────
        AdminAction::ListBans => {
            let bans = db::list_bans(&conn)?;
            if bans.is_empty() {
                println!("No active bans.");
            } else {
                println!("{:<5} {:<18} {:<28} Expires", "ID", "IP Hash (partial)", "Reason");
                println!("{}", "-".repeat(75));
                for b in &bans {
                    let partial = &b.ip_hash[..b.ip_hash.len().min(16)];
                    let expires = b.expires_at
                        .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "Permanent".to_string());
                    println!("{:<5} {:<18} {:<28} {}",
                        b.id, partial, b.reason.as_deref().unwrap_or(""), expires);
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
