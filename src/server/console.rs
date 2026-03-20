// server/console.rs — Terminal stats display and interactive keyboard console.
//
// All output goes through crate::logging helpers (console_println,
// console_print_raw, console_prompt) which acquire CONSOLE_MUTEX before
// writing.  This ensures log events from the tracing layer and console
// output from this module never interleave on stdout.
//
// All ANSI escape codes are guarded by crate::logging::is_tty() so that
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
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

// ─── ANSI helpers ─────────────────────────────────────────────────────────────

/// Return `code` when in TTY mode, empty string otherwise.
fn c(code: &'static str) -> &'static str {
    if crate::logging::is_tty() {
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

// ─── Terminal stats ───────────────────────────────────────────────────────────

pub struct TermStats {
    pub prev_req_count: u64,
    pub prev_post_count: i64,
    pub prev_thread_count: i64,
    pub last_tick: Instant,
}

/// Format a byte count as a human-readable string (B / KiB / MiB / GiB).
#[allow(clippy::cast_precision_loss)] // file-size display; f64 precision is adequate
fn fmt_bytes(b: i64) -> String {
    const KIB: i64 = 1024;
    const MIB: i64 = 1024 * 1024;
    const GIB: i64 = 1024 * 1024 * 1024;
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

#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
#[allow(clippy::many_single_char_names)]
pub fn print_stats(pool: &DbPool, start: Instant, ts: &mut TermStats) {
    use std::fmt::Write as _;

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
        |_| (0i64, 0i64, 0i64, 0i64, vec![]),
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
                pc * ps
            };
            let stats = crate::db::get_per_board_stats(&conn);
            (boards_n, threads_n, posts_n, db_n, stats)
        },
    );

    let upload_bytes = dir_size_bytes(&CONFIG.upload_dir);
    let in_flight = IN_FLIGHT.load(Ordering::Relaxed);
    let online = ACTIVE_IPS.len();
    let mem_bytes = process_rss_kb().cast_signed().saturating_mul(1024);

    // ── Delta highlights ──────────────────────────────────────────────────────
    let new_threads = (threads - ts.prev_thread_count).max(0);
    let new_posts = (posts - ts.prev_post_count).max(0);
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

    writeln!(block,
        "  uptime  {up_h}h {up_m:02}m {up_s:02}s    requests  {curr_reqs}    {rps_val}    in-flight {in_flight}").ok();

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
        let mut col = 0usize;
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
    writeln!(block, "{dim}", dim = c(DIM)).ok();
    block.push('\n');
    block.push_str(c(RST));

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
    0
}

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
            let is_real_dir = e.file_type().is_ok_and(|ft| ft.is_dir());
            if is_real_dir {
                walkdir_size(&e.path())
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

// ─── Startup banner ──────────────────────────────────────────────────────────

#[allow(clippy::arithmetic_side_effects)]
pub fn print_banner() {
    const INNER: usize = 53;
    let cell = |s: String, width: usize| -> String {
        let char_count = s.chars().count();
        if char_count >= width {
            s.chars().take(width).collect()
        } else {
            format!("{s}{}", " ".repeat(width - char_count))
        }
    };

    let title = cell(
        format!("{} v{}", CONFIG.forum_name, env!("CARGO_PKG_VERSION")),
        INNER - 2,
    );
    let bind = cell(CONFIG.bind_addr.clone(), INNER - 10);
    let db = cell(CONFIG.database_path.clone(), INNER - 10);
    let upl = cell(CONFIG.upload_dir.clone(), INNER - 10);
    let img_mib = CONFIG.max_image_size / 1024 / 1024;
    let vid_mib = CONFIG.max_video_size / 1024 / 1024;
    let aud_mib = CONFIG.max_audio_size / 1024 / 1024;
    let limits = cell(
        format!("img {img_mib} MiB  vid {vid_mib} MiB  audio {aud_mib} MiB"),
        INNER - 4,
    );

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

    let username = prompt_username(reader);
    let Some(username) = username else {
        return;
    };

    if crate::logging::is_tty() {
        crate::logging::console_println(&format!(
            "  {}Note: password input is visible — this is a one-time setup.{}",
            c(YLW),
            c(RST)
        ));
    }

    let password = prompt_password(reader);
    let Some(password) = password else {
        return;
    };

    let Ok(hash) = crate::utils::crypto::hash_password(&password) else {
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

// ─── Shared prompt helpers ────────────────────────────────────────────────────

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
fn prompt_password(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt(&format!("  {}Password (min 8 chars):{} ", c(CYN), c(RST)));
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
            crate::logging::console_println(&format!("  {}✗{} {e}", c(RED), c(RST)));
            continue;
        }

        crate::logging::console_prompt(&format!("  {}Confirm password:{}   ", c(CYN), c(RST)));
        let mut p2 = String::new();
        if reader.read_line(&mut p2).is_err() {
            crate::logging::console_println("\n  Skipped.");
            return None;
        }
        let p2 = p2.trim().to_string();
        if p1 != p2 {
            crate::logging::console_println(&format!(
                "  {}✗{} Passwords do not match. Try again.",
                c(RED),
                c(RST)
            ));
            continue;
        }
        return Some(p1);
    }
}

// ─── Keyboard-driven admin console ───────────────────────────────────────────

#[allow(clippy::significant_drop_tightening)]
pub fn spawn_keyboard_handler(pool: DbPool, start_time: Instant) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(600));
        print_keyboard_help();

        let stdin = std::io::stdin();

        let mut persistent_stats = TermStats {
            prev_req_count: REQUEST_COUNT.load(Ordering::Relaxed),
            prev_post_count: 0,
            prev_thread_count: 0,
            last_tick: Instant::now(),
        };

        let mut reader = BufReader::new(stdin.lock());

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
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
    let prompt = |msg: &str, reader: &mut dyn BufRead| -> Option<String> {
        crate::logging::console_prompt(msg);
        let mut s = String::new();
        match reader.read_line(&mut s) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(s.trim().to_string()),
        }
    };

    let short = match prompt(
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

    let name = match prompt(&format!("  {}Display name:{} ", c(CYN), c(RST)), reader) {
        Some(v) if !v.is_empty() => v,
        _ => {
            crate::logging::console_println("  Aborted.");
            return;
        }
    };

    let desc = prompt(
        &format!("  {}Description (blank = none):{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let nsfw_raw =
        prompt(&format!("  {}NSFW? [y/N]:{} ", c(CYN), c(RST)), reader).unwrap_or_default();
    let nsfw = matches!(nsfw_raw.to_lowercase().as_str(), "y" | "yes");

    let no_img = prompt(
        &format!("  {}Disable images? [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_vid = prompt(
        &format!("  {}Disable video?  [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_aud = prompt(
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
/// Also called by `prompt_create_first_admin` for the first-run wizard.
#[allow(clippy::too_many_lines)]
fn kb_create_admin(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_print_raw(&format!(
        "\n  {}── Create Admin Account ─────────────────────────────────{}\n\n",
        c(CYN),
        c(RST),
    ));

    if crate::logging::is_tty() {
        crate::logging::console_println(&format!(
            "  {}Note: password input is visible in terminal.{}",
            c(YLW),
            c(RST)
        ));
    }

    let Some(username) = prompt_username(reader) else {
        return;
    };
    let Some(password) = prompt_password(reader) else {
        return;
    };

    let Ok(hash) = crate::utils::crypto::hash_password(&password) else {
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
    crate::logging::console_prompt(&format!("  {}Thread ID to delete:{} ", c(CYN), c(RST)));
    let mut s = String::new();
    if reader.read_line(&mut s).is_err() {
        return;
    }
    let Ok(thread_id) = s.trim().parse::<i64>() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} '{}' is not a valid thread ID.",
            c(RED),
            c(RST),
            s.trim()
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

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            [thread_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        crate::logging::console_println(&format!(
            "  {}[err]{} Thread {thread_id} not found.",
            c(RED),
            c(RST)
        ));
        return;
    }

    crate::logging::console_prompt(&format!(
        "  {}Delete thread {thread_id} and all its posts? [y/N]:{} ",
        c(YLW),
        c(RST)
    ));
    let mut confirm = String::new();
    if reader.read_line(&mut confirm).is_err() {
        return;
    }
    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
        crate::logging::console_println("  Aborted.");
        return;
    }

    match crate::db::delete_thread(&conn, thread_id) {
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
