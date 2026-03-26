// server/console.rs — Terminal stats display and interactive keyboard console.
//
// All output goes through crate::logging helpers (console_println,
// console_print_raw, console_prompt) which acquire CONSOLE_MUTEX before
// writing.  This ensures log events from the tracing layer and console
// output from this module never interleave on stdout.
//
// All ANSI escape codes are guarded by cached TTY detection so that
// piped / systemd / Docker deployments receive plain text with zero escape
// pollution.
//
// Exported entry points called from server/server.rs:
//   print_banner()                                  — startup box before bind
//   prompt_create_first_admin(pool, reader)         — first-run admin wizard
//   spawn_keyboard_handler(pool, start_time)        — spawns the stdin thread

use crate::config::CONFIG;
use crate::db::DbPool;
use crate::server::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT, SPINNER_TICK};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// ─── Cached TTY detection ─────────────────────────────────────────────────────

/// Cached result of `crate::logging::is_tty()`.  Evaluated once, reused
/// everywhere so we never call `isatty(2)` more than once.
static IS_TTY: std::sync::LazyLock<bool> = std::sync::LazyLock::new(crate::logging::is_tty);

#[inline]
fn is_tty() -> bool {
    *IS_TTY
}

// ─── ANSI helpers ─────────────────────────────────────────────────────────────

/// Return `code` when in TTY mode, empty string otherwise.
#[inline]
fn c(code: &'static str) -> &'static str {
    if is_tty() {
        code
    } else {
        ""
    }
}

const RST: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GRN: &str = "\x1b[32m";
const YLW: &str = "\x1b[33m";
const CYN: &str = "\x1b[36m";
const BLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const BLD_GRN: &str = "\x1b[1;32m";
const BLD_YLW: &str = "\x1b[1;33m";

// ─── Constants ────────────────────────────────────────────────────────────────

/// Minimum interval between successive stat prints (rate-limit).
const STATS_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum board description length (bytes).
const MAX_BOARD_DESC_LEN: usize = 500;

/// Maximum board display-name length (bytes).
const MAX_BOARD_NAME_LEN: usize = 64;

/// Maximum recursion depth for `walkdir_size`.
const MAX_DIR_DEPTH: u32 = 64;

// ─── Byte-size thresholds (used by `fmt_bytes`) ──────────────────────────────

const KIB: i64 = 1024;
const MIB: i64 = 1024 * 1024;
const GIB: i64 = 1024 * 1024 * 1024;

// ─── Unicode width helpers ────────────────────────────────────────────────────

/// Return `true` for characters that typically occupy two terminal columns
/// (CJK Unified Ideographs, fullwidth forms, etc.).
fn is_wide_char(ch: char) -> bool {
    let cp = ch as u32;
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFF01..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x20000..=0x2FA1F).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
        || (0x2E80..=0x2FDF).contains(&cp)
        || (0x3000..=0x303F).contains(&cp)
        || (0x3040..=0x30FF).contains(&cp)
        || (0x3200..=0x33FF).contains(&cp)
}

/// Estimate the display-column width of a single character.
fn char_display_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else if is_wide_char(ch) {
        2
    } else {
        1
    }
}

/// Estimate the display-column width of a string.
fn display_width(s: &str) -> usize {
    s.chars().map(char_display_width).sum()
}

// ─── Zeroize helper ───────────────────────────────────────────────────────────

/// Securely overwrite a `String`'s backing buffer with zeros, then truncate.
/// Uses `write_volatile` to prevent the compiler from eliding the stores.
/// This is a best-effort measure — the allocator may retain copies elsewhere.
fn zeroize_string(s: &mut String) {
    // SAFETY: we overwrite valid UTF-8 bytes with 0x00 (valid UTF-8 for NUL),
    // then immediately clear the length so the String is empty.
    unsafe {
        let bytes = s.as_bytes_mut();
        for b in bytes.iter_mut() {
            std::ptr::write_volatile(b, 0);
        }
    }
    s.clear();
    s.shrink_to_fit();
}

// ─── Numeric helpers ──────────────────────────────────────────────────────────

/// Format a byte count as a human-readable string (B / KiB / MiB / GiB).
/// Negative inputs are clamped to zero.
#[allow(clippy::cast_precision_loss)]
fn fmt_bytes(b: i64) -> String {
    let b = b.max(0);
    if b < KIB {
        format!("{b} B")
    } else if b < MIB {
        format!("{:.1} KiB", b as f64 / KIB as f64)
    } else if b < GIB {
        format!("{:.1} MiB", b as f64 / MIB as f64)
    } else {
        format!("{:.2} GiB", b as f64 / GIB as f64)
    }
}

/// Saturating cast from `u64` to `i64`, clamping at `i64::MAX`.
#[inline]
fn saturating_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

// ─── Terminal stats ───────────────────────────────────────────────────────────

pub struct TermStats {
    pub prev_req_count: u64,
    pub prev_post_count: i64,
    pub prev_thread_count: i64,
    pub last_tick: Instant,
    pub last_stats_time: Option<Instant>,
}

#[allow(clippy::too_many_lines, clippy::arithmetic_side_effects)]
pub fn print_stats(pool: &DbPool, start: Instant, ts: &mut TermStats) {
    use std::fmt::Write as _;

    // ── Rate-limit ────────────────────────────────────────────────────────────
    let now = Instant::now();
    if let Some(last) = ts.last_stats_time {
        if now.duration_since(last) < STATS_MIN_INTERVAL {
            crate::logging::console_println(
                "  Stats rate-limited — please wait at least 1 second.",
            );
            return;
        }
    }
    ts.last_stats_time = Some(now);

    // ── Uptime ────────────────────────────────────────────────────────────────
    let uptime = start.elapsed();
    let up_h = uptime.as_secs() / 3600;
    let up_m = (uptime.as_secs() % 3600) / 60;
    let up_s = uptime.as_secs() % 60;

    // ── req/s delta since last tick ───────────────────────────────────────────
    let now_instant = Instant::now();
    let elapsed_secs = now_instant
        .duration_since(ts.last_tick)
        .as_secs_f64()
        .max(0.001);
    ts.last_tick = now_instant;
    let curr_reqs = REQUEST_COUNT.load(Ordering::Relaxed);
    let req_delta = curr_reqs.saturating_sub(ts.prev_req_count);
    #[allow(clippy::cast_precision_loss)]
    let rps = req_delta as f64 / elapsed_secs;
    ts.prev_req_count = curr_reqs;

    // ── DB snapshot ───────────────────────────────────────────────────────────
    let (boards, threads, posts, db_bytes, board_stats) = pool.get().map_or_else(
        |_| (0_i64, 0_i64, 0_i64, 0_i64, vec![]),
        |conn| {
            let boards_n: i64 = conn
                .query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))
                .unwrap_or(0);
            let threads_n: i64 = conn
                .query_row("SELECT COUNT(*) FROM threads WHERE archived = 0", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            let posts_n: i64 = conn
                .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
                .unwrap_or(0);
            let db_n: i64 = {
                let pc: i64 = conn
                    .query_row("PRAGMA page_count", [], |r| r.get(0))
                    .unwrap_or(0);
                let ps: i64 = conn
                    .query_row("PRAGMA page_size", [], |r| r.get(0))
                    .unwrap_or(4096);
                pc.saturating_mul(ps)
            };
            let stats = crate::db::get_per_board_stats(&conn);
            (boards_n, threads_n, posts_n, db_n, stats)
        },
    );

    let upload_bytes = dir_size_bytes(&CONFIG.upload_dir);
    let in_flight = IN_FLIGHT.load(Ordering::Relaxed);
    let online = ACTIVE_IPS.len();
    let mem_bytes = saturating_i64(process_rss_kb()).saturating_mul(1024);

    // ── Delta highlights ──────────────────────────────────────────────────────
    let new_threads = threads.saturating_sub(ts.prev_thread_count).max(0);
    let new_posts = posts.saturating_sub(ts.prev_post_count).max(0);
    ts.prev_thread_count = threads;
    ts.prev_post_count = posts;

    // ── Active uploads spinner ────────────────────────────────────────────────
    let active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed);
    let upload_line = if active_uploads > 0 {
        let tick = SPINNER_TICK.fetch_add(1, Ordering::Relaxed);
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = frames
            .get(usize::try_from(tick).unwrap_or(0) % frames.len())
            .copied()
            .unwrap_or("⠋");
        format!(
            "  {}{frame}{}  {active_uploads} file(s) uploading\n",
            c(CYN),
            c(RST)
        )
    } else {
        String::new()
    };

    // ── Value strings ─────────────────────────────────────────────────────────
    let ts_str = chrono::Local::now().format("%H:%M:%S").to_string();

    let thread_val = if new_threads > 0 {
        format!("{}{threads}(+{new_threads}){}", c(BLD_YLW), c(RST))
    } else {
        threads.to_string()
    };
    let post_val = if new_posts > 0 {
        format!("{}{posts}(+{new_posts}){}", c(BLD_YLW), c(RST))
    } else {
        posts.to_string()
    };
    let rps_val = if rps >= 1.0 {
        format!("{}{rps:.1}/s{}", c(BLD_GRN), c(RST))
    } else {
        format!("{rps:.2}/s")
    };

    // ── Assemble the output block ─────────────────────────────────────────────
    let mut block = String::new();

    writeln!(
        block,
        "\n{dim}── stats {ts_str}{rst}",
        dim = c(DIM),
        rst = c(RST)
    )
    .ok();

    writeln!(
        block,
        "  uptime  {up_h}h {up_m:02}m {up_s:02}s    requests  {curr_reqs}    \
         {rps_val}    in-flight {in_flight}"
    )
    .ok();

    writeln!(
        block,
        "  boards  {boards:<5}  threads  {thread_val:<20}  posts  {post_val}"
    )
    .ok();

    writeln!(
        block,
        "  db      {db:<12}  uploads  {upl:<12}  online  {online}    rss  {rss}",
        db = fmt_bytes(db_bytes),
        upl = fmt_bytes(upload_bytes),
        rss = fmt_bytes(mem_bytes)
    )
    .ok();

    // Board breakdown (two boards per line)
    if !board_stats.is_empty() {
        writeln!(block, "  {dim}", dim = c(DIM)).ok();
        let mut col = 0_usize;
        for (short, thr, pst) in &board_stats {
            let seg = format!("/{short}/ {thr}t {pst}p");
            if col == 0 {
                write!(block, "  {seg:<36}").ok();
                col = 1;
            } else {
                writeln!(block, "  {seg}").ok();
                col = 0;
            }
        }
        if col == 1 {
            block.push('\n');
        }
        block.push_str(c(RST));
    }

    block.push_str(&upload_line);
    block.push('\n');

    crate::logging::console_print_raw(&block);
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tracing::debug!(target: "console", "RSS measurement not available on this platform");
    }
    0
}

fn dir_size_bytes(path: &str) -> i64 {
    saturating_i64(walkdir_size(std::path::Path::new(path), 0))
}

fn walkdir_size(path: &std::path::Path, depth: u32) -> u64 {
    if depth > MAX_DIR_DEPTH {
        tracing::warn!(
            target: "console",
            path = %path.display(),
            "Directory traversal exceeded max depth ({MAX_DIR_DEPTH}), skipping"
        );
        return 0;
    }

    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let Ok(ft) = e.file_type() else {
                return 0;
            };
            if ft.is_symlink() {
                // Do not follow symlinks — avoids inflated sizes and loops.
                0
            } else if ft.is_dir() {
                walkdir_size(&e.path(), depth.saturating_add(1))
            } else {
                // Use symlink_metadata to avoid following symlinks for size.
                std::fs::symlink_metadata(e.path())
                    .map(|m| m.len())
                    .unwrap_or(0)
            }
        })
        .sum()
}

// ─── Startup banner ──────────────────────────────────────────────────────────

/// Pad or truncate `s` to exactly `width` **display columns**.
fn banner_cell(s: &str, width: usize) -> String {
    let dw = display_width(s);
    if dw >= width {
        let mut out = String::new();
        let mut w = 0_usize;
        for ch in s.chars() {
            let cw = char_display_width(ch);
            if w.saturating_add(cw) > width {
                break;
            }
            out.push(ch);
            w = w.saturating_add(cw);
        }
        while w < width {
            out.push(' ');
            w = w.saturating_add(1);
        }
        out
    } else {
        format!("{s}{}", " ".repeat(width.saturating_sub(dw)))
    }
}

#[allow(clippy::cast_precision_loss)]
pub fn print_banner() {
    const INNER: usize = 53;

    let title_raw = format!("{} v{}", CONFIG.forum_name, env!("CARGO_PKG_VERSION"));
    let title = banner_cell(&title_raw, INNER.saturating_sub(2));
    let bind = banner_cell(&CONFIG.bind_addr, INNER.saturating_sub(10));
    let db = banner_cell(&CONFIG.database_path, INNER.saturating_sub(10));
    let upl = banner_cell(&CONFIG.upload_dir, INNER.saturating_sub(10));

    let img_mib = CONFIG.max_image_size as f64 / (1024.0 * 1024.0);
    let vid_mib = CONFIG.max_video_size as f64 / (1024.0 * 1024.0);
    let aud_mib = CONFIG.max_audio_size as f64 / (1024.0 * 1024.0);
    let limits_raw = format!("img {img_mib:.1} MiB  vid {vid_mib:.1} MiB  audio {aud_mib:.1} MiB");
    let limits = banner_cell(&limits_raw, INNER.saturating_sub(4));

    let block = format!(
        "{cyan}┌─────────────────────────────────────────────────────┐\n\
         │  {title}│\n\
         ├─────────────────────────────────────────────────────┤\n\
         │  Bind    {bind}│\n\
         │  DB      {db}│\n\
         │  Uploads {upl}│\n\
         │  {limits}  │\n\
         └─────────────────────────────────────────────────────┘{rst}\n",
        cyan = c(CYN),
        rst = c(RST),
    );
    crate::logging::console_print_raw(&block);
}

// ─── Shared prompt helpers ────────────────────────────────────────────────────

/// Helper to print a prompt and read one line from `reader`.
/// Returns `None` on EOF or I/O error (prints a message in both cases).
fn console_prompt_line(msg: &str, reader: &mut dyn BufRead) -> Option<String> {
    crate::logging::console_prompt(msg);
    let mut s = String::new();
    match reader.read_line(&mut s) {
        Ok(0) => {
            crate::logging::console_println("  Input cancelled (EOF).");
            None
        }
        Err(e) => {
            crate::logging::console_println(&format!("  Input error: {e}"));
            None
        }
        Ok(_) => Some(s.trim().to_string()),
    }
}

/// Strip ASCII control characters (except space) from `s`.
fn strip_control_chars(s: &str) -> String {
    s.chars()
        .filter(|ch| !ch.is_control() || *ch == ' ')
        .collect()
}

/// Read and validate a username from `reader`. Returns `None` on EOF/Ctrl-C.
fn prompt_username(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt(&format!("  {}Username:{} ", c(CYN), c(RST)));
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

/// Read and confirm a password from `reader`. Returns `None` on EOF/Ctrl-C.
/// The returned `String` should be zeroed after use via `zeroize_string`.
fn prompt_password(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt(&format!("  {}Password (min 8 chars):{} ", c(CYN), c(RST)));
        let mut p1_raw = String::new();
        match reader.read_line(&mut p1_raw) {
            Ok(0) | Err(_) => {
                zeroize_string(&mut p1_raw);
                crate::logging::console_println("\n  Skipped.");
                return None;
            }
            Ok(_) => {}
        }
        let trimmed = p1_raw.trim().to_string();
        zeroize_string(&mut p1_raw);
        let mut p1 = trimmed;

        if let Err(e) = crate::utils::crypto::validate_password(&p1) {
            zeroize_string(&mut p1);
            crate::logging::console_println(&format!("  {}✗{} {e}", c(RED), c(RST)));
            continue;
        }

        crate::logging::console_prompt(&format!("  {}Confirm password:{}   ", c(CYN), c(RST)));
        let mut p2_raw = String::new();
        if reader.read_line(&mut p2_raw).is_err() {
            zeroize_string(&mut p1);
            zeroize_string(&mut p2_raw);
            crate::logging::console_println("\n  Skipped.");
            return None;
        }
        let trimmed2 = p2_raw.trim().to_string();
        zeroize_string(&mut p2_raw);
        let mut p2 = trimmed2;

        if p1 != p2 {
            zeroize_string(&mut p1);
            zeroize_string(&mut p2);
            crate::logging::console_println(&format!(
                "  {}✗{} Passwords do not match. Try again.",
                c(RED),
                c(RST)
            ));
            continue;
        }
        zeroize_string(&mut p2);
        return Some(p1);
    }
}

// ─── First-run admin wizard ───────────────────────────────────────────────────

/// Interactive wizard that creates the first admin account and optionally a
/// first board. Called from `server.rs` on first run (no admin accounts in DB)
/// when stdout is a TTY, before `spawn_keyboard_handler` so stdin is uncontested.
///
/// Reads from `reader` for input. Each prompt releases the console lock before
/// blocking on `read_line` so log events are not stalled while waiting for input.
#[allow(clippy::too_many_lines)]
pub fn prompt_create_first_admin(pool: &DbPool, reader: &mut dyn BufRead) {
    let header = format!(
        "\n\
        {cyan}╔══════════════════════════════════════════════════════╗\n\
        ║         FIRST RUN — CREATE ADMIN ACCOUNT             ║\n\
        ╠══════════════════════════════════════════════════════╣\n\
        ║  No admin accounts found.                            ║\n\
        ║  Create one now to access the admin panel at /admin  ║\n\
        ║  after the server starts. (Ctrl+C to skip for now.)  ║\n\
        ╚══════════════════════════════════════════════════════╝{rst}\n\n",
        cyan = c(CYN),
        rst = c(RST),
    );
    crate::logging::console_print_raw(&header);

    let Some(username) = prompt_username(reader) else {
        return;
    };

    if is_tty() {
        crate::logging::console_println(&format!(
            "  {}Note: password input is visible — this is a one-time setup.{}",
            c(YLW),
            c(RST)
        ));
    }

    let Some(mut password) = prompt_password(reader) else {
        return;
    };

    let hash_result = crate::utils::crypto::hash_password(&password);
    zeroize_string(&mut password);

    let Ok(hash) = hash_result else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Failed to hash password.",
            c(RED),
            c(RST)
        ));
        return;
    };

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} DB connection failed.",
            c(RED),
            c(RST)
        ));
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
                "\n  {}✓{} Admin '{}{username}{}' created (id={id}).\n\
                   {}→{} Log in at /admin once the server is running.\n\n",
                c(GRN),
                c(RST),
                c(BLD),
                c(RST),
                c(CYN),
                c(RST),
            ));
        }
        Err(e) => {
            crate::logging::console_println(&format!(
                "  {}[err]{} Failed to create admin: {e}",
                c(RED),
                c(RST)
            ));
            return;
        }
    }

    crate::logging::console_prompt(&format!(
        "  {}Create a board now?{} [y/N]: ",
        c(CYN),
        c(RST)
    ));
    let mut ans = String::new();
    if reader.read_line(&mut ans).is_ok()
        && matches!(ans.trim().to_lowercase().as_str(), "y" | "yes")
    {
        crate::logging::console_println("");
        kb_create_board(pool, reader);
    }

    crate::logging::console_println("");
}

// ─── Keyboard-driven admin console ───────────────────────────────────────────

/// Global shutdown flag for the keyboard handler thread.
/// Checked in the input loop; set via `signal_keyboard_shutdown()`.
static KEYBOARD_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Signal the keyboard handler thread to stop.
/// Called from the shutdown path in `server.rs` (e.g. on `SIGTERM` / `Ctrl+C`).
#[allow(dead_code)]
pub fn signal_keyboard_shutdown() {
    KEYBOARD_SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Spawn the interactive keyboard console thread.
///
/// The thread sleeps briefly to let server bind/startup log messages flush,
/// then prints the help text and enters the input loop.
///
/// **Invariant:** Only this thread reads from stdin for the lifetime of the
/// process.  No other code path should call `stdin().lock()` or
/// `stdin().read_line()`.
#[allow(clippy::significant_drop_tightening)]
pub fn spawn_keyboard_handler(pool: DbPool, start_time: Instant) {
    let result = std::thread::Builder::new()
        .name("console-kbd".into())
        .spawn(move || {
            // Brief sleep to let server bind/startup log messages flush first.
            std::thread::sleep(Duration::from_millis(600));

            print_keyboard_help();

            // Initialise counters from the DB so the first [s] doesn't show
            // the entire database as "new".
            let (init_posts, init_threads) = pool.get().map_or((0_i64, 0_i64), |conn| {
                let posts: i64 = conn
                    .query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
                    .unwrap_or(0);
                let threads: i64 = conn
                    .query_row("SELECT COUNT(*) FROM threads WHERE archived = 0", [], |r| {
                        r.get(0)
                    })
                    .unwrap_or(0);
                (posts, threads)
            });

            let mut persistent_stats = TermStats {
                prev_req_count: REQUEST_COUNT.load(Ordering::Relaxed),
                prev_post_count: init_posts,
                prev_thread_count: init_threads,
                last_tick: Instant::now(),
                last_stats_time: None,
            };

            let stdin = std::io::stdin();

            // Note: the stdin lock is held for the lifetime of this thread.
            // No other code path in the application should read from stdin.
            let mut reader = BufReader::new(stdin.lock());

            loop {
                if KEYBOARD_SHUTDOWN.load(Ordering::Relaxed) {
                    break;
                }

                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }

                if KEYBOARD_SHUTDOWN.load(Ordering::Relaxed) {
                    break;
                }

                let cmd = line.trim().to_lowercase();
                match cmd.as_str() {
                    "s" => print_stats(&pool, start_time, &mut persistent_stats),
                    "l" => kb_list_boards(&pool),
                    "c" => kb_create_board(&pool, &mut reader),
                    "a" => kb_create_admin(&pool, &mut reader),
                    "d" => kb_delete_thread(&pool, &mut reader),
                    "h" => print_keyboard_help(),
                    "q" => crate::logging::console_println(&format!(
                        "  {}[!]{} Use Ctrl+C or SIGTERM to stop the server.",
                        c(YLW),
                        c(RST)
                    )),
                    "" => {}
                    other => crate::logging::console_println(&format!(
                        "  Unknown command '{other}'. Press [h] for help."
                    )),
                }
            }
        });

    if let Err(e) = result {
        tracing::error!(target: "console", "Failed to spawn console-kbd thread: {e}");
    }
}

fn print_keyboard_help() {
    let block = format!(
        "\n\
        {cyan}╔══ Admin Console ══════════════════════════════════╗{rst}\n\
        {cyan}║{rst}  [s] show stats     [l] list boards             {cyan}║{rst}\n\
        {cyan}║{rst}  [c] create board   [a] create admin account    {cyan}║{rst}\n\
        {cyan}║{rst}  [d] delete thread  [h] help    [q] quit hint   {cyan}║{rst}\n\
        {cyan}╚══════════════════════════════════════════════════╝{rst}\n\n",
        cyan = c(CYN),
        rst = c(RST),
    );
    crate::logging::console_print_raw(&block);
}

// ─── kb_list_boards ───────────────────────────────────────────────────────────

fn kb_list_boards(pool: &DbPool) {
    use std::fmt::Write as _;

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };
    let boards = match crate::db::get_all_boards(&conn) {
        Ok(b) => b,
        Err(e) => {
            crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST)));
            return;
        }
    };
    if boards.is_empty() {
        crate::logging::console_println("  No boards found.");
        return;
    }

    let mut block = format!(
        "  {d}{:<5} {:<12} {:<24} NSFW{r}\n  {}\n",
        "ID",
        "Short",
        "Name",
        "─".repeat(48),
        d = c(DIM),
        r = c(RST),
    );
    for b in &boards {
        writeln!(
            block,
            "  {:<5} /{:<11} {:<24} {}",
            b.id,
            format!("{}/", b.short_name),
            b.name,
            if b.nsfw { "yes" } else { "no" },
        )
        .ok();
    }
    block.push('\n');
    crate::logging::console_print_raw(&block);
}

// ─── kb_create_board ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub fn kb_create_board(pool: &DbPool, reader: &mut dyn BufRead) {
    let short = match console_prompt_line(
        &format!("  {}Short name (e.g. 'tech'):{} ", c(CYN), c(RST)),
        reader,
    ) {
        Some(v) if !v.is_empty() => v,
        _ => {
            crate::logging::console_println("  Aborted.");
            return;
        }
    };

    let short_lc = short.to_lowercase();
    if short_lc.is_empty()
        || short_lc.len() > 8
        || !short_lc.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        crate::logging::console_println(&format!(
            "  {}[err]{} Short name must be 1-8 alphanumeric characters.",
            c(RED),
            c(RST)
        ));
        return;
    }

    let name = match console_prompt_line(&format!("  {}Display name:{} ", c(CYN), c(RST)), reader) {
        Some(v) if !v.is_empty() => v,
        _ => {
            crate::logging::console_println("  Aborted.");
            return;
        }
    };

    // Validate display name: strip control chars, enforce length.
    let name = strip_control_chars(&name);
    if name.is_empty() {
        crate::logging::console_println(&format!(
            "  {}[err]{} Display name cannot be empty after stripping control characters.",
            c(RED),
            c(RST)
        ));
        return;
    }
    if name.len() > MAX_BOARD_NAME_LEN {
        crate::logging::console_println(&format!(
            "  {}[err]{} Display name must be {MAX_BOARD_NAME_LEN} characters or fewer.",
            c(RED),
            c(RST)
        ));
        return;
    }

    let desc_raw = console_prompt_line(
        &format!("  {}Description (blank = none):{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();

    // Validate and sanitise description.
    let desc = strip_control_chars(&desc_raw);
    if desc.len() > MAX_BOARD_DESC_LEN {
        crate::logging::console_println(&format!(
            "  {}[err]{} Description must be {MAX_BOARD_DESC_LEN} characters or fewer.",
            c(RED),
            c(RST)
        ));
        return;
    }

    let nsfw_raw = console_prompt_line(&format!("  {}NSFW? [y/N]:{} ", c(CYN), c(RST)), reader)
        .unwrap_or_default();
    let nsfw = matches!(nsfw_raw.to_lowercase().as_str(), "y" | "yes");

    let no_img = console_prompt_line(
        &format!("  {}Disable images? [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_vid = console_prompt_line(
        &format!("  {}Disable video?  [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_aud = console_prompt_line(
        &format!("  {}Disable audio?  [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();

    let allow_images = !matches!(no_img.to_lowercase().as_str(), "y" | "yes");
    let allow_video = !matches!(no_vid.to_lowercase().as_str(), "y" | "yes");
    let allow_audio = !matches!(no_aud.to_lowercase().as_str(), "y" | "yes");

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    match crate::db::create_board_with_media_flags(
        &conn,
        &short_lc,
        &name,
        &desc,
        nsfw,
        allow_images,
        allow_video,
        allow_audio,
    ) {
        Ok(id) => {
            tracing::info!(
                target: "console",
                board = %short_lc,
                name  = %name,
                id    = id,
                "Board created via console"
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Board /{short_lc}/  — {name}{}  created (id={id}).",
                c(GRN),
                c(RST),
                if nsfw { " [NSFW]" } else { "" },
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}

// ─── kb_create_admin ─────────────────────────────────────────────────────────

/// Create an additional admin account from the interactive console.
#[allow(clippy::too_many_lines)]
fn kb_create_admin(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_print_raw(&format!(
        "\n  {}── Create Admin Account ─────────────────────────────────{}\n\n",
        c(CYN),
        c(RST),
    ));

    if is_tty() {
        crate::logging::console_println(&format!(
            "  {}Note: password input is visible in terminal.{}",
            c(YLW),
            c(RST)
        ));
    }

    let Some(username) = prompt_username(reader) else {
        return;
    };
    let Some(mut password) = prompt_password(reader) else {
        return;
    };

    let hash_result = crate::utils::crypto::hash_password(&password);
    zeroize_string(&mut password);

    let Ok(hash) = hash_result else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Failed to hash password.",
            c(RED),
            c(RST)
        ));
        return;
    };

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    match crate::db::create_admin(&conn, &username, &hash) {
        Ok(id) => {
            tracing::info!(
                target: "console",
                username = %username,
                id = id,
                "Admin account created via console"
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Admin '{username}' created (id={id}).",
                c(GRN),
                c(RST),
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}

// ─── kb_delete_thread ────────────────────────────────────────────────────────

fn kb_delete_thread(pool: &DbPool, reader: &mut dyn BufRead) {
    let Some(id_str) = console_prompt_line(
        &format!("  {}Thread ID to delete:{} ", c(CYN), c(RST)),
        reader,
    ) else {
        return;
    };

    // Parse as u64 — thread IDs are always positive.
    let Ok(thread_id_u) = id_str.parse::<u64>() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} '{}' is not a valid thread ID (must be a positive integer).",
            c(RED),
            c(RST),
            id_str
        ));
        return;
    };
    if thread_id_u == 0 {
        crate::logging::console_println(&format!(
            "  {}[err]{} Thread ID must be greater than zero.",
            c(RED),
            c(RST)
        ));
        return;
    }
    let thread_id = saturating_i64(thread_id_u);

    let Some(confirm_str) = console_prompt_line(
        &format!(
            "  {}Delete thread {thread_id} and all its posts? [y/N]:{} ",
            c(YLW),
            c(RST)
        ),
        reader,
    ) else {
        return;
    };
    if !matches!(confirm_str.to_lowercase().as_str(), "y" | "yes") {
        crate::logging::console_println("  Aborted.");
        return;
    }

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    // Perform existence check + deletion atomically to avoid TOCTOU.
    let result: Result<Vec<String>, String> = (|| {
        conn.execute("BEGIN IMMEDIATE", [])
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM threads WHERE id = ?1",
                [thread_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if exists == 0 {
            conn.execute("ROLLBACK", []).ok();
            return Err(format!("Thread {thread_id} not found."));
        }

        match crate::db::delete_thread(&conn, thread_id) {
            Ok(paths) => {
                conn.execute("COMMIT", [])
                    .map_err(|e| format!("Failed to commit: {e}"))?;
                Ok(paths)
            }
            Err(e) => {
                conn.execute("ROLLBACK", []).ok();
                Err(format!("{e}"))
            }
        }
    })();

    match result {
        Ok(paths) => {
            let n = paths.len();
            for p in &paths {
                crate::utils::files::delete_file(&CONFIG.upload_dir, p);
            }
            tracing::info!(
                target: "console",
                thread_id = thread_id,
                files_removed = n,
                "Thread deleted via console"
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Thread {thread_id} deleted ({n} file(s) removed).",
                c(GRN),
                c(RST)
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}
