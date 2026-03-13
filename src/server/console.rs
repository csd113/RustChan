// server/console.rs — Terminal stats display and interactive keyboard console.
//
// Everything in this file is pure terminal I/O — it has no knowledge of HTTP
// routing, middleware, or request handling.
//
// Exported entry points called from server/server.rs:
//   print_banner()                           — startup box printed before bind
//   spawn_keyboard_handler(pool, start_time) — spawns the stdin-reading thread

use crate::config::CONFIG;
use crate::db::DbPool;
use crate::server::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT, SPINNER_TICK};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

// ─── Terminal stats ───────────────────────────────────────────────────────────

pub struct TermStats {
    pub prev_req_count: u64,
    pub prev_post_count: i64,
    pub prev_thread_count: i64,
    pub last_tick: Instant,
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub fn print_stats(pool: &DbPool, start: Instant, ts: &mut TermStats) {
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
    #[allow(clippy::cast_precision_loss)]
    let rps = req_delta as f64 / elapsed_secs;
    ts.prev_req_count = curr_reqs;

    // DB query
    let (boards, threads, posts, db_kb, board_stats) = pool.get().map_or_else(
        |_| (0i64, 0i64, 0i64, 0i64, vec![]),
        |conn| {
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
            let bs = crate::db::get_per_board_stats(&conn);
            (b, th, p, kb, bs)
        },
    );

    let upload_mb = dir_size_mb(&CONFIG.upload_dir);

    // New-event flash: bold+yellow when counts increased since last tick
    let new_threads = (threads - ts.prev_thread_count).max(0);
    let new_posts = (posts - ts.prev_post_count).max(0);
    let thread_str = if new_threads > 0 {
        format!("\x1b[1;33mthreads {threads} (+{new_threads})\x1b[0m")
    } else {
        format!("threads {threads}")
    };
    let post_str = if new_posts > 0 {
        format!("\x1b[1;33mposts {posts} (+{new_posts})\x1b[0m")
    } else {
        format!("posts {posts}")
    };
    ts.prev_thread_count = threads;
    ts.prev_post_count = posts;

    // Active connections / users online
    // FIX[AUDIT-1]: IN_FLIGHT is now AtomicU64 — load directly, no .max(0) cast.
    let in_flight = IN_FLIGHT.load(Ordering::Relaxed);
    let online_count = ACTIVE_IPS.len();

    // CRIT-5: Keys are SHA-256 hashes — show 8-char prefixes for diagnostics.
    // FIX[AUDIT-3]: Use .get(..8) instead of direct [..8] byte-index so this
    // stays safe if a key is ever shorter than 8 chars (defensive programming).
    let ip_list: String = {
        let mut hashes: Vec<String> = ACTIVE_IPS
            .iter()
            .map(|e| {
                let key = e.key();
                key.get(..8).unwrap_or(key.as_str()).to_string()
            })
            .collect();
        hashes.sort_unstable();
        hashes.truncate(5);
        if hashes.is_empty() {
            "none".into()
        } else {
            hashes.join(", ")
        }
    };

    // Upload progress bar — shown only while uploads are active
    // FIX[AUDIT-1]: ACTIVE_UPLOADS is now AtomicU64 — load directly.
    let active_uploads = ACTIVE_UPLOADS.load(Ordering::Relaxed);
    if active_uploads > 0 {
        // Fix #5: SPINNER_TICK was read but never written anywhere, so the
        // spinner was permanently frozen on frame 0 ("⠋").  Increment it here,
        // inside the only branch that actually displays the spinner.
        let tick = SPINNER_TICK.fetch_add(1, Ordering::Relaxed);
        let spinners = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spin = spinners
            .get((usize::try_from(tick).unwrap_or(0)) % spinners.len())
            .copied()
            .unwrap_or("⠋");
        let fill = ((tick % 20) as usize).min(10);
        let bar = format!("{}{}", "█".repeat(fill), "░".repeat(10 - fill));
        println!("  \x1b[36m{spin} UPLOAD  [{bar}]  {active_uploads} file(s) uploading\x1b[0m");
    }

    // Main stats line
    #[allow(clippy::cast_precision_loss)]
    let stats_line = format!(
        "── STATS  uptime {h}h{m:02}m  │  requests {curr_reqs}  │  \x1b[32m{rps:.1} req/s\x1b[0m  │  in-flight {in_flight}  │  boards {boards}  {thread_str}  {post_str}  │  db {db_kb} KiB  uploads {upload_mb:.1} MiB ──"
    );
    println!("{stats_line}");

    // Users online line
    let mem_rss = process_rss_kb();
    println!("   users online: {online_count}  │  IPs: {ip_list}  │  mem: {mem_rss} KiB RSS");

    // Per-board breakdown
    if !board_stats.is_empty() {
        let segments: Vec<String> = board_stats
            .iter()
            .map(|(short, t, p)| format!("/{short}/  threads:{t} posts:{p}"))
            .collect();
        let mut line = String::from("   ");
        let mut line_len = 0usize;
        for seg in &segments {
            if line_len > 0 && line_len + seg.len() + 5 > 110 {
                println!("{line}");
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
            println!("{line}");
        }
    }
}

/// Read the process RSS (resident set size) in KiB.
///
/// * Linux  — parsed from `/proc/self/status` (`VmRSS` field, already in KiB).
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

fn dir_size_mb(path: &str) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let mb = walkdir_size(std::path::Path::new(path)) as f64 / (1024.0 * 1024.0);
    mb
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
    // Fix #3: All dynamic values (forum_name, bind_addr, paths, MiB sizes) are
    // padded/truncated to exactly fill the fixed inner width, so the right-hand
    // │ character is always aligned regardless of the actual value length.
    const INNER: usize = 53;

    // FIX[AUDIT-4]: The original closure collected chars into a `Vec<char>` and
    // had a dead `unwrap_or_else(|| s.clone())` branch — `get(..width)` on a
    // slice of length >= width never returns None.  The rewrite uses
    // `chars().count()` (no heap allocation) and `chars().take(width).collect()`
    // (single pass), eliminating both the intermediate Vec and the dead code.
    let cell = |s: String, width: usize| -> String {
        let char_count = s.chars().count();
        if char_count >= width {
            s.chars().take(width).collect()
        } else {
            format!("{s}{}", " ".repeat(width - char_count))
        }
    };

    let title = cell(
        format!("{} v{}", env!("CARGO_PKG_VERSION"), CONFIG.forum_name),
        INNER - 2, // 2 leading spaces in "│  <title>│"
    );
    let bind = cell(CONFIG.bind_addr.clone(), INNER - 10); // "│  Bind    <val>│"
    let db = cell(CONFIG.database_path.clone(), INNER - 10); // "│  DB      <val>│"
    let upl = cell(CONFIG.upload_dir.clone(), INNER - 10); // "│  Uploads <val>│"
    let img_mib = CONFIG.max_image_size / 1024 / 1024;
    let vid_mib = CONFIG.max_video_size / 1024 / 1024;
    let limits = cell(
        format!("Images {img_mib} MiB max  │  Videos {vid_mib} MiB max"),
        INNER - 4, // "│  <val>  │"
    );

    println!("┌─────────────────────────────────────────────────────┐");
    println!("│  {title}│");
    println!("├─────────────────────────────────────────────────────┤");
    println!("│  Bind    {bind}│");
    println!("│  DB      {db}│");
    println!("│  Uploads {upl}│");
    println!("│  {limits}  │");
    println!("└─────────────────────────────────────────────────────┘");
}

// ─── Keyboard-driven admin console ───────────────────────────────────────────

// `reader` (BufReader<StdinLock>) must persist for the entire loop because it
// is passed by &mut into sub-commands; the lint's inline suggestion is invalid.
#[allow(clippy::significant_drop_tightening)]
pub fn spawn_keyboard_handler(pool: DbPool, start_time: Instant) {
    std::thread::spawn(move || {
        // Small delay so startup messages settle first
        std::thread::sleep(Duration::from_millis(600));
        print_keyboard_help();

        let stdin = std::io::stdin();

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

        // Acquire stdin lock only after all pre-loop setup is complete so the
        // StdinLock (significant Drop) is held for the shortest possible scope.
        // `reader` is passed into kb_create_board / kb_delete_thread inside the
        // loop, so it must live for the full loop scope and cannot be inlined.
        let mut reader = BufReader::new(stdin.lock());

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break, // EOF — stdin closed (daemon mode)
                Ok(_) => {}
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
                other => println!("  Unknown command '{other}'. Press [h] for help."),
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

fn kb_list_boards(pool: &DbPool) {
    let Ok(conn) = pool.get() else {
        println!("  \x1b[31m[err]\x1b[0m Could not get DB connection.");
        return;
    };
    let boards = match crate::db::get_all_boards(&conn) {
        Ok(b) => b,
        Err(e) => {
            println!("  \x1b[31m[err]\x1b[0m {e}");
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

fn kb_create_board(pool: &DbPool, reader: &mut dyn std::io::BufRead) {
    let mut prompt = |msg: &str| -> String {
        print!("  \x1b[36m{msg}\x1b[0m ");
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

    // FIX[AUDIT-5]: Validate short name immediately after reading it, before
    // prompting for the remaining fields.  Previously validation happened at
    // the bottom of the function, so the user would fill in all prompts before
    // learning the short name was invalid.
    let short_lc = short.to_lowercase();
    if short_lc.is_empty()
        || short_lc.len() > 8
        || !short_lc.chars().all(|c| c.is_ascii_alphanumeric())
    {
        println!("  \x1b[31m[err]\x1b[0m Short name must be 1-8 alphanumeric characters.");
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

    let Ok(conn) = pool.get() else {
        println!("  \x1b[31m[err]\x1b[0m Could not get DB connection.");
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
        Err(e) => println!("  \x1b[31m[err]\x1b[0m {e}"),
    }
    println!();
}

fn kb_delete_thread(pool: &DbPool, reader: &mut dyn std::io::BufRead) {
    print!("  \x1b[36mThread ID to delete:\x1b[0m ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = reader.read_line(&mut s);
    let thread_id: i64 = if let Ok(n) = s.trim().parse() {
        n
    } else {
        println!(
            "  \x1b[31m[err]\x1b[0m '{}' is not a valid thread ID.",
            s.trim()
        );
        return;
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
        println!("  \x1b[31m[err]\x1b[0m Thread {thread_id} not found.");
        return;
    }

    print!("  \x1b[33mDelete thread {thread_id} and all its posts? [y/N]:\x1b[0m ");
    let _ = std::io::stdout().flush();
    let mut confirm = String::new();
    let _ = reader.read_line(&mut confirm);
    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("  Aborted.");
        return;
    }

    match crate::db::delete_thread(&conn, thread_id) {
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
        Err(e) => println!("  \x1b[31m[err]\x1b[0m {e}"),
    }
    println!();
}
