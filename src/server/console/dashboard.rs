// The string-pattern check here is intentional; the lint would only add noise to this console test.
#![allow(clippy::single_char_pattern)]

// server/console/dashboard.rs — Pure render functions for each ConsoleMode.
//
// Layout matches the RustHost reference style:
//   Status section   — server running state, HTTPS, Tor (each labelled)
//   Endpoints section — concrete URLs for HTTP / HTTPS / Onion
//   Content section  — boards / threads / posts
//   Storage section  — DB size / upload size / memory
//   Boards section   — per-board breakdown
//   Footer           — key bar
//
// Rule: pad plain text to the desired column width FIRST, then wrap in ANSI
// colour helpers.  Escape bytes are invisible to the terminal's width
// calculation but are counted by Rust's len() / format padding — doing it the
// other way around produces misaligned columns.

use super::ChanStats;
use crate::config::CONFIG;
use std::path::{Path, PathBuf};

// ─── ANSI helpers ─────────────────────────────────────────────────────────────

fn green(s: &str) -> String {
    format!("\x1b[32m{s}\x1b[0m")
}
fn yellow(s: &str) -> String {
    format!("\x1b[33m{s}\x1b[0m")
}
fn red(s: &str) -> String {
    format!("\x1b[31m{s}\x1b[0m")
}
fn cyan(s: &str) -> String {
    format!("\x1b[36m{s}\x1b[0m")
}
fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}
fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}

const RULE: &str = "\x1b[2m\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\
\x1b[0m";

// Left-column label width for aligned rows.
const LW: usize = 14;

/// Print one aligned " Label : value" row.
/// `label` is plain ASCII; `value` is already-coloured.
fn row(out: &mut String, label: &str, value: &str) {
    use std::fmt::Write as _;
    writeln!(out, " {label:<LW$} : {value}").ok();
}

// ─── Formatters ───────────────────────────────────────────────────────────────

#[allow(clippy::cast_precision_loss)]
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

fn fmt_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}h {m:02}m {s:02}s")
}

// ─── TLS cert type label ──────────────────────────────────────────────────────

fn https_cert_label() -> &'static str {
    if CONFIG.tls.acme.enabled {
        "Let's Encrypt"
    } else if CONFIG.tls.manual_cert.is_some() {
        "manual cert"
    } else {
        "self-signed"
    }
}

// ─── render_dashboard() ───────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub fn render_dashboard(stats: &ChanStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2048);

    // ── Header ────────────────────────────────────────────────────────────────
    writeln!(out, "{RULE}").ok();
    writeln!(
        out,
        " {}  v{}",
        bold(&CONFIG.forum_name),
        env!("CARGO_PKG_VERSION")
    )
    .ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();

    // ── Status ────────────────────────────────────────────────────────────────
    writeln!(out, " {}", bold("Status")).ok();

    // Local server
    let local_status = format!("{}  ({})", green("RUNNING"), dim(&CONFIG.bind_addr));
    row(&mut out, "Local Server", &local_status);

    // HTTPS
    if CONFIG.tls.enabled {
        let https_status = format!(
            "{}  (port {}  \u{00b7}  {})",
            green("RUNNING"),
            cyan(&CONFIG.tls.port.to_string()),
            dim(https_cert_label()),
        );
        row(&mut out, "HTTPS", &https_status);
    } else {
        row(&mut out, "HTTPS", &red("DISABLED"));
    }

    // Tor
    if !CONFIG.enable_tor_support {
        row(&mut out, "Tor", &red("DISABLED"));
    } else if CONFIG.tor_only {
        match &stats.onion_address {
            Some(_) => row(
                &mut out,
                "Tor",
                &format!("{}  {}", green("READY"), yellow("tor-only mode")),
            ),
            None => row(
                &mut out,
                "Tor",
                &format!("{}  {}", yellow("BOOTSTRAPPING"), yellow("tor-only mode")),
            ),
        }
    } else {
        match &stats.onion_address {
            Some(_) => row(&mut out, "Tor", &green("READY")),
            None => row(&mut out, "Tor", &yellow("BOOTSTRAPPING\u{2026}")),
        }
    }

    writeln!(out).ok();

    // ── Endpoints ─────────────────────────────────────────────────────────────
    writeln!(out, " {}", bold("Endpoints")).ok();

    // Derive the local URL — use localhost when bound to 0.0.0.0 / ::
    let local_url = {
        let port = CONFIG
            .bind_addr
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .unwrap_or(8080);
        format!("http://localhost:{port}")
    };
    row(&mut out, "Local", &cyan(&local_url));

    if CONFIG.tls.enabled {
        let https_url = format!("https://localhost:{}", CONFIG.tls.port);
        row(&mut out, "HTTPS", &cyan(&https_url));
    }

    match &stats.onion_address {
        Some(addr) => row(&mut out, "Onion", &cyan(&format!("http://{addr}"))),
        None if CONFIG.enable_tor_support => {
            row(&mut out, "Onion", &dim("waiting for Tor\u{2026}"));
        }
        None => {}
    }

    writeln!(out).ok();

    // ── Activity ──────────────────────────────────────────────────────────────
    writeln!(out, " {}", bold("Activity")).ok();
    let rps_str = if stats.rps >= 1.0 {
        green(&format!("{:.1}/s", stats.rps))
    } else {
        dim(&format!("{:.2}/s", stats.rps))
    };
    let inflight_str = if stats.in_flight > 5 {
        yellow(&stats.in_flight.to_string())
    } else {
        dim(&stats.in_flight.to_string())
    };
    row(
        &mut out,
        "Requests",
        &format!(
            "{}   {}   {} in-flight",
            stats.req_count, rps_str, inflight_str
        ),
    );
    row(&mut out, "Online", &stats.online.to_string());
    row(&mut out, "Uptime", &fmt_uptime(stats.uptime_secs));
    row(&mut out, "Memory", &fmt_bytes(stats.mem_bytes));
    let ffmpeg_videos = if stats.active_ffmpeg_videos > 0 {
        yellow(&stats.active_ffmpeg_videos.to_string())
    } else {
        dim(&stats.active_ffmpeg_videos.to_string())
    };
    row(
        &mut out,
        "FFmpeg",
        &format!("{ffmpeg_videos} videos processing"),
    );
    writeln!(out).ok();

    // ── Content ───────────────────────────────────────────────────────────────
    writeln!(out, " {}", bold("Content")).ok();
    row(&mut out, "Boards", &stats.boards.to_string());
    row(&mut out, "Threads", &stats.threads.to_string());
    row(&mut out, "Posts", &stats.posts.to_string());
    writeln!(out).ok();

    // ── Storage ───────────────────────────────────────────────────────────────
    writeln!(out, " {}", bold("Storage")).ok();
    row(&mut out, "Database", &fmt_bytes(stats.db_bytes));
    row(&mut out, "Uploads", &fmt_bytes(stats.upload_bytes));
    writeln!(out).ok();

    // ── Upload spinner ────────────────────────────────────────────────────────
    if stats.active_uploads > 0 {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        // spinner_tick is computed as (fetch_add(1) % 10) as u8 in collect_stats,
        // so it is always in 0..=9 — a direct index is safe and needs no modulo.
        let frame = FRAMES
            .get(usize::from(stats.spinner_tick))
            .copied()
            .unwrap_or("⠋");
        writeln!(out, " {}  {} uploading", cyan(frame), stats.active_uploads).ok();
        writeln!(out).ok();
    }

    // ── Footer ────────────────────────────────────────────────────────────────
    writeln!(out, "{RULE}").ok();
    writeln!(out,
        " {} Help  {} Reload  {} Boards  {} New board  {} New admin  {} Del thread  {} Logs  {} Quit",
        bold("[H]"), bold("[R]"), bold("[B]"), bold("[C]"), bold("[A]"), bold("[D]"), bold("[L]"), bold("[Q]"),
    ).ok();
    writeln!(out, "{RULE}").ok();

    out
}

// ─── render_log_view() ────────────────────────────────────────────────────────

pub fn render_log_view() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(8192);
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Log View", bold("\u{25c8}")).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();

    let logs_dir = crate::config::logs_dir();
    match latest_log_file(&logs_dir) {
        Some(path) => {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("current log");
            writeln!(out, " {} {}", dim("File:"), cyan(filename)).ok();
            writeln!(out).ok();

            match read_log_tail(&path, 24 * 1024) {
                Ok((content, truncated)) => {
                    let available_lines = log_body_height();
                    let lines = content
                        .lines()
                        .map(str::trim_end)
                        .filter(|line| !line.is_empty())
                        .collect::<Vec<_>>();

                    if lines.is_empty() {
                        writeln!(out, " {}", dim("Log file is empty.")).ok();
                    } else {
                        let start = lines.len().saturating_sub(available_lines);
                        if truncated || start > 0 {
                            writeln!(out, " {}", dim("Showing newest log lines...")).ok();
                            writeln!(out).ok();
                        }
                        if let Some(visible_lines) = lines.get(start..) {
                            for line in visible_lines {
                                writeln!(out, " {line}").ok();
                            }
                        }
                    }
                }
                Err(err) => {
                    writeln!(out, " {}", red("Could not read live log.")).ok();
                    writeln!(out, " {}", dim(&err)).ok();
                }
            }
        }
        None => {
            writeln!(out, " {}", dim("No live log file found yet.")).ok();
        }
    }

    writeln!(out).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Back", bold("[L]")).ok();
    writeln!(out, "{RULE}").ok();
    out
}

fn log_body_height() -> usize {
    crossterm::terminal::size().map_or(24, |(_, rows)| usize::from(rows.saturating_sub(8)).max(6))
}

fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let mut files = std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("log"))
        .collect::<Vec<_>>();
    files.sort();
    files.pop()
}

fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| format!("Open log: {e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("Log metadata: {e}"))?
        .len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Seek log: {e}"))?;

    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("Read log: {e}"))?;

    let text = String::from_utf8_lossy(&buf).into_owned();
    let truncated = start > 0;
    let content = if truncated {
        match text.find('\n') {
            Some(pos) if pos + 1 < text.len() => text[pos + 1..].to_string(),
            _ => text,
        }
    } else {
        text
    };
    Ok((content, truncated))
}

// ─── render_help() ────────────────────────────────────────────────────────────

pub fn render_help() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(1024);
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Key Reference", bold("\u{25c8}")).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();
    let keys: &[(&str, &str)] = &[
        ("[H]", "Help"),
        ("[R]", "Force-reload stats"),
        ("[L]", "Log view"),
        ("[B]", "Board list"),
        ("[C]", "Create board wizard"),
        ("[A]", "Create admin wizard"),
        ("[D]", "Delete thread wizard"),
        ("[Q]", "Quit (confirmation)"),
        ("[Y]", "Confirm"),
        ("[N]", "Cancel / back"),
        ("Esc", "Back to dashboard"),
        ("Ctrl-C", "Force quit"),
    ];
    for (key, desc) in keys {
        writeln!(out, " {:<10}  {}", bold(key), desc).ok();
    }
    writeln!(out).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Back", bold("[H]")).ok();
    writeln!(out, "{RULE}").ok();
    out
}

// ─── render_board_list() ─────────────────────────────────────────────────────

pub fn render_board_list(stats: &ChanStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(1024);
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Board List", bold("\u{25c8}")).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();
    if stats.board_rows.is_empty() {
        writeln!(out, " {}", dim("No boards yet. Press [C] to create one.")).ok();
    } else {
        writeln!(
            out,
            " {:<14}   {:>8}   {:>8}",
            dim("Board"),
            dim("Threads"),
            dim("Posts")
        )
        .ok();
        writeln!(out, " {}", dim(&"\u{2500}".repeat(34))).ok();
        for (short, thr, pst) in &stats.board_rows {
            let label = format!("/{short}/");
            writeln!(out, " {:<14}   {:>8}   {:>8}", cyan(&label), thr, pst).ok();
        }
    }
    writeln!(out).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(
        out,
        " {} Back   {} New board   {} Del thread",
        bold("[B]"),
        bold("[C]"),
        bold("[D]")
    )
    .ok();
    writeln!(out, "{RULE}").ok();
    out
}

// ─── render_confirm_quit() ────────────────────────────────────────────────────

pub fn render_confirm_quit() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(256);
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Confirm Quit", bold("\u{25c8}")).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();
    writeln!(
        out,
        " Shut down RustChan? In-flight requests will drain gracefully."
    )
    .ok();
    writeln!(out).ok();
    writeln!(
        out,
        " {}  Yes, quit      {}  Cancel",
        bold("[Y]"),
        bold("[N]")
    )
    .ok();
    writeln!(out).ok();
    writeln!(out, "{RULE}").ok();
    out
}

#[cfg(test)]
mod tests {
    use super::render_dashboard;
    use crate::server::console::ChanStats;

    #[test]
    fn dashboard_shows_active_ffmpeg_video_count() {
        let stats = ChanStats {
            active_ffmpeg_videos: 3,
            ..ChanStats::default()
        };

        let rendered = render_dashboard(&stats);

        assert!(rendered.contains("FFmpeg"));
        assert!(rendered.contains("videos processing"));
        assert!(rendered.contains("3"));
    }
}
