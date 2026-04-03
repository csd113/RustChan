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
    let local_status = format!("{}  ({})", green("RUNNING"), dim(&CONFIG.bind_addr),);
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

    let ngrok_status = match &stats.ngrok {
        crate::server::ngrok::NgrokState::Disabled => red("DISABLED"),
        crate::server::ngrok::NgrokState::Starting => yellow("STARTING"),
        crate::server::ngrok::NgrokState::Ready { .. } => green("READY"),
        crate::server::ngrok::NgrokState::NotInstalled => red("NOT INSTALLED"),
        crate::server::ngrok::NgrokState::NotConfigured => yellow("NOT CONFIGURED"),
        crate::server::ngrok::NgrokState::Error { .. } => red("ERROR"),
    };
    row(&mut out, "Ngrok", &ngrok_status);

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

    match &stats.ngrok {
        crate::server::ngrok::NgrokState::Ready { url } => {
            row(&mut out, "Public", &cyan(url));
        }
        crate::server::ngrok::NgrokState::Starting => {
            row(&mut out, "Public", &dim("waiting for ngrok\u{2026}"));
        }
        crate::server::ngrok::NgrokState::NotInstalled
        | crate::server::ngrok::NgrokState::NotConfigured => {
            row(&mut out, "Public", &dim("ngrok setup required"));
        }
        crate::server::ngrok::NgrokState::Error { message } => {
            row(&mut out, "Public", &dim(message));
        }
        crate::server::ngrok::NgrokState::Disabled => {}
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
        " {} Help  {} Reload  {} Ngrok  {} Boards  {} New board  {} New admin  {} Del thread  {} Logs  {} Quit",
        bold("[H]"), bold("[R]"), bold("[T]"), bold("[B]"), bold("[C]"), bold("[A]"), bold("[D]"), bold("[L]"), bold("[Q]"),
    ).ok();
    writeln!(out, "{RULE}").ok();

    out
}

// ─── render_log_view() ────────────────────────────────────────────────────────

pub fn render_log_view() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(512);
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Log View", bold("\u{25c8}")).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out).ok();
    writeln!(
        out,
        " {}",
        dim("Logs are written to rustchan-data/logs/rustchan.YYYY-MM-DD.log")
    )
    .ok();
    writeln!(
        out,
        " {}",
        dim("tail -f rustchan-data/logs/rustchan.$(date +%F).log")
    )
    .ok();
    writeln!(out).ok();
    writeln!(out, "{RULE}").ok();
    writeln!(out, " {} Back", bold("[L]")).ok();
    writeln!(out, "{RULE}").ok();
    out
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
        ("[T]", "Toggle ngrok public URL"),
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
