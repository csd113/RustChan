// server/console/mod.rs — Full-screen TUI console for RustChan.
//
// Architecture:
//   • RAW_MODE_ACTIVE atomic    — safe cleanup from panic hooks / signal handlers
//   • ConsoleMode enum          — drives which screen the render task draws
//   • SharedStats / ChanStats   — snapshot written by the stats refresh task
//   • start()                   — enters alternate screen, spawns render + input tasks
//   • cleanup()                 — restores terminal; safe to call multiple times
//   • render()                  — async, interval-driven, skips identical frames
//   • collect_stats()           — pure DB+atomics snapshot, called from server.rs
//   • prompt_create_first_admin() — first-run wizard (pre-TUI, normal terminal mode)
//
// Wizard flows (CreateBoard / CreateAdmin / DeleteThread) exit raw mode, run the
// blocking interactive prompts from wizard.rs, then re-enter raw mode.
// ConsoleMode::Wizard(_) causes render() to return immediately so no partial
// frames race with the wizard's own output.

pub mod dashboard;
pub mod input;
pub mod wizard;

use crossterm::{cursor, execute, terminal};
use std::io::stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify, RwLock};

// ─── Raw-mode safety flag ─────────────────────────────────────────────────────

/// True once raw mode is active. `cleanup()` CAS-es it to false so a second call
/// is a guaranteed no-op even under concurrent access.
static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

// ─── Console mode ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleMode {
    Dashboard,
    LogView,
    Help,
    BoardList,
    ConfirmQuit,
    Wizard(WizardKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WizardKind {
    CreateBoard,
    CreateAdmin,
    DeleteThread,
}

pub type SharedConsoleMode = Arc<RwLock<ConsoleMode>>;

// ─── Force-reload notifier ────────────────────────────────────────────────────

/// Shared between the key-dispatch task and the stats-refresh task.
/// Sending a notification causes the refresh task to skip its next sleep
/// and collect stats immediately.
pub type ForceReload = Arc<Notify>;

// ─── Shared stats ─────────────────────────────────────────────────────────────

pub type SharedStats = Arc<RwLock<ChanStats>>;

pub struct ChanStats {
    pub uptime_secs: u64,
    pub req_count: u64,
    pub rps: f64,
    pub in_flight: u64,
    pub online: usize,
    pub boards: i64,
    pub threads: i64,
    pub posts: i64,
    pub db_bytes: i64,
    pub upload_bytes: i64,
    pub mem_bytes: i64,
    pub board_rows: Vec<(String, i64, i64)>, // (short, threads, posts)
    pub active_uploads: u64,
    pub spinner_tick: u8,
    /// Live onion address once Tor has bootstrapped, None while bootstrapping.
    pub onion_address: Option<String>,
}

impl Default for ChanStats {
    fn default() -> Self {
        Self {
            uptime_secs: 0,
            req_count: 0,
            rps: 0.0,
            in_flight: 0,
            online: 0,
            boards: 0,
            threads: 0,
            posts: 0,
            db_bytes: 0,
            upload_bytes: 0,
            mem_bytes: 0,
            board_rows: vec![],
            active_uploads: 0,
            spinner_tick: 0,
            onion_address: None,
        }
    }
}

// ─── cleanup() ───────────────────────────────────────────────────────────────

/// Restore the terminal unconditionally. Safe to call from panic hooks, signal
/// handlers, and normal shutdown paths. Uses CAS so a second call is a no-op.
pub fn cleanup() {
    if RAW_MODE_ACTIVE
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        crate::logging::set_tui_active(false);
        let _ = terminal::disable_raw_mode();
        let _ = execute!(stdout(), terminal::LeaveAlternateScreen, cursor::Show);
    }
}

// ─── render() ────────────────────────────────────────────────────────────────

/// Render one frame. Returns immediately when mode is Wizard so wizard I/O
/// is uncontested. Uses last-rendered diffing to skip identical frames.
async fn render(mode: &SharedConsoleMode, stats: &SharedStats, last_rendered: &mut String) {
    use std::io::Write as _;

    let current_mode = mode.read().await.clone();

    let frame = {
        let snap = stats.read().await;
        match current_mode {
            ConsoleMode::Wizard(_) => return,
            ConsoleMode::Dashboard => dashboard::render_dashboard(&snap),
            ConsoleMode::LogView => dashboard::render_log_view(),
            ConsoleMode::Help => dashboard::render_help(),
            ConsoleMode::BoardList => dashboard::render_board_list(&snap),
            ConsoleMode::ConfirmQuit => dashboard::render_confirm_quit(),
        }
    };

    if frame == *last_rendered {
        return;
    }
    last_rendered.clone_from(&frame);

    // In raw mode \n moves the cursor down but does NOT return to column 0.
    // Every bare \n must become \r\n so lines start at the left edge.
    let frame_crlf = normalise_newlines(&frame);

    let _ = execute!(
        stdout(),
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All),
    );
    let _ = stdout().write_all(frame_crlf.as_bytes());
    let _ = stdout().flush();
}

/// Replace every bare `\n` (not already preceded by `\r`) with `\r\n`.
/// Called once per frame so the cost is negligible.
fn normalise_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(64));
    let mut prev = '\0';
    for ch in s.chars() {
        if ch == '\n' && prev != '\r' {
            out.push('\r');
        }
        out.push(ch);
        prev = ch;
    }
    out
}

// ─── start() ─────────────────────────────────────────────────────────────────

/// Minimum terminal dimensions for the dashboard to render without wrapping.
const MIN_COLS: u16 = 90;
const MIN_ROWS: u16 = 36;

/// Enter the full-screen TUI. Spawns:
///   1. Input task  — reads crossterm events, sends `KeyEvent` over the returned channel.
///   2. Render task — redraws every 500 ms.
///
/// Returns `(key_rx, force_reload)` so `server.rs` can drive mode transitions and
/// trigger immediate stats refreshes on [R].
pub fn start(
    stats: &SharedStats,
    mode: &SharedConsoleMode,
) -> (mpsc::UnboundedReceiver<input::KeyEvent>, ForceReload) {
    let _ = terminal::enable_raw_mode();
    RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);
    let _ = execute!(stdout(), terminal::EnterAlternateScreen, cursor::Hide);

    // Ensure the window is wide and tall enough to display the dashboard
    // without wrapping or truncation.  Only resize if the current dimensions
    // are smaller than the minimum — never shrink a larger window.
    if let Ok((cols, rows)) = terminal::size() {
        let new_cols = cols.max(MIN_COLS);
        let new_rows = rows.max(MIN_ROWS);
        if new_cols != cols || new_rows != rows {
            let _ = execute!(stdout(), terminal::SetSize(new_cols, new_rows));
        }
    }
    // Signal to the rest of the codebase that the TUI owns the screen.
    // Any code that would print banners or boxes (e.g. detect.rs Tor box)
    // must check is_tui_active() and skip its output.
    crate::logging::set_tui_active(true);

    let (key_tx, key_rx) = mpsc::unbounded_channel::<input::KeyEvent>();
    if let Err(e) = input::spawn(key_tx) {
        tracing::error!(target: "console", error = %e, "Failed to spawn console-input thread");
    }

    let stats_r = stats.clone();
    let mode_r = mode.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut last_rendered = String::new();
        loop {
            interval.tick().await;
            render(&mode_r, &stats_r, &mut last_rendered).await;
        }
    });

    let force_reload = Arc::new(Notify::new());
    (key_rx, force_reload)
}

// ─── collect_stats() ─────────────────────────────────────────────────────────

/// Collect a fresh `ChanStats` snapshot from the DB and global atomics.
/// Mutates the delta-tracking locals in place so req/s and other deltas
/// are accurate across calls. Runs on the calling thread — use
/// `tokio::task::block_in_place` at the call site when inside an async context.
#[allow(clippy::cast_precision_loss)]
#[allow(clippy::arithmetic_side_effects)]
#[allow(clippy::too_many_arguments)]
pub fn collect_stats(
    pool: &crate::db::DbPool,
    start: Instant,
    prev_req: &mut u64,
    prev_tick: &mut Instant,
    prev_threads: &mut i64,
    prev_posts: &mut i64,
    onion_address: Option<String>,
) -> ChanStats {
    use std::sync::atomic::Ordering;

    let uptime = start.elapsed().as_secs();

    // req/s delta since previous call
    let now_instant = Instant::now();
    let elapsed_secs = now_instant
        .duration_since(*prev_tick)
        .as_secs_f64()
        .max(0.001);
    let curr_reqs = crate::server::REQUEST_COUNT.load(Ordering::Relaxed);
    let req_delta = curr_reqs.saturating_sub(*prev_req);
    let rps = req_delta as f64 / elapsed_secs;
    *prev_req = curr_reqs;
    *prev_tick = now_instant;

    let in_flight = crate::server::IN_FLIGHT.load(Ordering::Relaxed);
    let active_uploads = crate::server::ACTIVE_UPLOADS.load(Ordering::Relaxed);
    let online = crate::server::ACTIVE_IPS.len();
    let spinner_tick = (crate::server::SPINNER_TICK.fetch_add(1, Ordering::Relaxed) % 10) as u8;

    let (boards, threads, posts, db_bytes, board_rows) = pool.get().map_or_else(
        |_| (0i64, 0i64, 0i64, 0i64, vec![]),
        |conn| {
            let b: i64 = conn
                .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
                .unwrap_or(0);
            let t: i64 = conn
                .query_row("SELECT COUNT(*) FROM threads WHERE archived = 0", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            let p: i64 = conn
                .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
                .unwrap_or(0);
            let db: i64 = {
                let pc: i64 = conn
                    .query_row("PRAGMA page_count", [], |r| r.get(0))
                    .unwrap_or(0);
                let ps: i64 = conn
                    .query_row("PRAGMA page_size", [], |r| r.get(0))
                    .unwrap_or(4096);
                pc * ps
            };
            let rows = crate::db::get_per_board_stats(&conn);
            (b, t, p, db, rows)
        },
    );

    *prev_threads = threads;
    *prev_posts = posts;

    let upload_bytes = dir_size_bytes(&crate::config::CONFIG.upload_dir);
    let mem_bytes = process_rss_kb().cast_signed().saturating_mul(1024);

    ChanStats {
        uptime_secs: uptime,
        req_count: curr_reqs,
        rps,
        in_flight,
        online,
        boards,
        threads,
        posts,
        db_bytes,
        upload_bytes,
        mem_bytes,
        board_rows,
        active_uploads,
        spinner_tick,
        onion_address,
    }
}

// ─── Utility helpers ──────────────────────────────────────────────────────────

fn dir_size_bytes(path: &str) -> i64 {
    walkdir_size(std::path::Path::new(path)).cast_signed()
}

fn walkdir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            if e.file_type().is_ok_and(|ft| ft.is_dir()) {
                walkdir_size(&e.path())
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn process_rss_kb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(val) = line.strip_prefix("VmRSS:") {
                    return val
                        .split_whitespace()
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
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

// ─── prompt_create_first_admin() helpers ─────────────────────────────────────

// These are module-level private functions (not inner functions) so that
// the `clippy::items_after_statements` lint is satisfied — inner `fn` items
// defined after the first statement in a function body trigger that lint.

fn first_admin_prompt_u(reader: &mut dyn std::io::BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt("  \x1b[36mUsername:\x1b[0m ");
        let mut s = String::new();
        match reader.read_line(&mut s) {
            Ok(0) | Err(_) => {
                crate::logging::console_println(
                    "\n  Skipped — run: rustchan-cli admin create-admin <user> <pass>",
                );
                return None;
            }
            Ok(_) => {}
        }
        let u = s.trim().to_string();
        if u.is_empty() {
            crate::logging::console_println("  Username cannot be empty.");
            continue;
        }
        if u.len() > 32 {
            crate::logging::console_println("  Username must be 32 characters or fewer.");
            continue;
        }
        if !u
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            crate::logging::console_println(
                "  Username must be alphanumeric (underscores and hyphens allowed).",
            );
            continue;
        }
        return Some(u);
    }
}

fn first_admin_prompt_p(reader: &mut dyn std::io::BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt("  \x1b[36mPassword (min 8 chars):\x1b[0m ");
        let mut p1 = String::new();
        match reader.read_line(&mut p1) {
            Ok(0) | Err(_) => {
                crate::logging::console_println("\n  Skipped.");
                return None;
            }
            Ok(_) => {}
        }
        let p1 = p1.trim().to_string();
        if let Err(e) = crate::utils::crypto::validate_password(&p1) {
            crate::logging::console_println(&format!("  \x1b[31m✗\x1b[0m {e}"));
            continue;
        }
        crate::logging::console_prompt("  \x1b[36mConfirm password:\x1b[0m   ");
        let mut p2 = String::new();
        if reader.read_line(&mut p2).is_err() {
            crate::logging::console_println("\n  Skipped.");
            return None;
        }
        let p2 = p2.trim().to_string();
        if p1 != p2 {
            crate::logging::console_println(
                "  \x1b[31m✗\x1b[0m Passwords do not match. Try again.",
            );
            continue;
        }
        return Some(p1);
    }
}

// ─── prompt_create_first_admin() ─────────────────────────────────────────────

/// First-run wizard. Called before the TUI starts, so stdout is in normal
/// terminal mode — no raw mode toggling needed here.
#[allow(clippy::too_many_lines)]
pub fn prompt_create_first_admin(pool: &crate::db::DbPool, reader: &mut dyn std::io::BufRead) {
    crate::logging::console_print_raw(
        "\n\
        \x1b[36m╔══════════════════════════════════════════════════════╗\n\
        ║         FIRST RUN — CREATE ADMIN ACCOUNT             ║\n\
        ╠══════════════════════════════════════════════════════╣\n\
        ║  No admin accounts found.                            ║\n\
        ║  Create one now to access the admin panel at /admin  ║\n\
        ║  after the server starts. (Ctrl+C to skip for now.)  ║\n\
        ╚══════════════════════════════════════════════════════╝\x1b[0m\n\n",
    );

    if crate::logging::is_tty() {
        crate::logging::console_println(
            "  \x1b[33mNote: password input is visible — this is a one-time setup.\x1b[0m",
        );
    }

    let Some(username) = first_admin_prompt_u(reader) else {
        return;
    };
    let Some(password) = first_admin_prompt_p(reader) else {
        return;
    };

    let Ok(hash) = crate::utils::crypto::hash_password(&password) else {
        crate::logging::console_println("  \x1b[31m[err]\x1b[0m Failed to hash password.");
        return;
    };
    let Ok(conn) = pool.get() else {
        crate::logging::console_println("  \x1b[31m[err]\x1b[0m DB connection failed.");
        return;
    };

    match crate::db::create_admin(&conn, &username, &hash) {
        Ok(id) => {
            tracing::info!(
                target: "startup",
                username = %username,
                id = id,
                "First admin account created via setup wizard"
            );
            crate::logging::console_print_raw(&format!(
                "\n  \x1b[32m✓\x1b[0m Admin '\x1b[1m{username}\x1b[0m' created (id={id}).\n\
                   \x1b[36m→\x1b[0m Log in at /admin once the server is running.\n\n",
            ));
        }
        Err(e) => {
            crate::logging::console_println(&format!(
                "  \x1b[31m[err]\x1b[0m Failed to create admin: {e}"
            ));
            return;
        }
    }

    crate::logging::console_prompt("  \x1b[36mCreate a board now?\x1b[0m [y/N]: ");
    let mut ans = String::new();
    if reader.read_line(&mut ans).is_ok()
        && matches!(ans.trim().to_lowercase().as_str(), "y" | "yes")
    {
        crate::logging::console_println("");
        wizard::kb_create_board(pool, reader);
    }
    crate::logging::console_println("");
}
