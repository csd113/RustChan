//! Pure render functions for each console mode.
//!
//! Plain text is padded before ANSI styling because escape bytes are invisible
//! to the terminal but count toward Rust string widths.

use super::ChanStats;
use crate::config::CONFIG;
use std::path::{Path, PathBuf};

// ANSI helpers
/// Style text green when ANSI output is enabled.
fn green(s: &str) -> String {
    colour("32", s)
}
/// Style text yellow when ANSI output is enabled.
fn yellow(s: &str) -> String {
    colour("33", s)
}
/// Style text red when ANSI output is enabled.
fn red(s: &str) -> String {
    colour("31", s)
}
/// Style text cyan when ANSI output is enabled.
fn cyan(s: &str) -> String {
    colour("36", s)
}
/// Dim text when ANSI output is enabled.
fn dim(s: &str) -> String {
    colour("2", s)
}
/// Bold text when ANSI output is enabled.
fn bold(s: &str) -> String {
    colour("1", s)
}

/// Apply an ANSI Select Graphic Rendition code when supported.
fn colour(code: &str, s: &str) -> String {
    if crate::logging::ansi_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

/// Render one dashboard separator.
fn rule() -> String {
    dim(&rule_char().repeat(60))
}

/// Return the platform-appropriate separator character.
#[cfg(windows)]
const fn rule_char() -> &'static str {
    "-"
}

/// Return the platform-appropriate separator character.
#[cfg(not(windows))]
const fn rule_char() -> &'static str {
    "\u{2500}"
}

/// Return the platform-appropriate heading marker.
#[cfg(windows)]
const fn marker() -> &'static str {
    "*"
}

/// Return the platform-appropriate heading marker.
#[cfg(not(windows))]
const fn marker() -> &'static str {
    "\u{25c8}"
}

/// Return the platform-appropriate ellipsis.
#[cfg(windows)]
const fn ellipsis() -> &'static str {
    "..."
}

/// Return the platform-appropriate ellipsis.
#[cfg(not(windows))]
const fn ellipsis() -> &'static str {
    "\u{2026}"
}

/// Return the platform-appropriate inline bullet.
#[cfg(windows)]
const fn bullet() -> &'static str {
    "-"
}

/// Return the platform-appropriate inline bullet.
#[cfg(not(windows))]
const fn bullet() -> &'static str {
    "\u{00b7}"
}

/// Return one platform-appropriate spinner frame.
#[cfg(windows)]
fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
    FRAMES[usize::from(tick) % FRAMES.len()]
}

/// Return one platform-appropriate spinner frame.
#[cfg(not(windows))]
fn spinner_frame(tick: u8) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES.get(usize::from(tick)).copied().unwrap_or("⠋")
}

/// Left-column label width for aligned rows.
const LW: usize = 14;

/// Print one aligned " Label : value" row.
/// `label` is plain ASCII; `value` is already-coloured.
fn row(out: &mut String, label: &str, value: &str) {
    use std::fmt::Write as _;
    writeln!(out, " {label:<LW$} : {value}").ok();
}

// Formatters
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "human-readable byte sizes intentionally trade integer precision for compact display"
)]
/// Format a byte count using binary units.
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

/// Format uptime as hours, minutes, and seconds.
fn fmt_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h}h {m:02}m {s:02}s")
}

// TLS cert type label
/// Return the active HTTPS certificate-source label.
fn https_cert_label() -> &'static str {
    if CONFIG.tls.acme.enabled {
        "Let's Encrypt"
    } else if CONFIG.tls.manual_cert.is_some() {
        "manual cert"
    } else {
        "self-signed"
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the contiguous dashboard renderer keeps terminal row ordering auditable"
)]
/// Render the main server dashboard.
pub fn render_dashboard(stats: &ChanStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2048);

    // Header
    writeln!(out, "{}", rule()).ok();
    writeln!(
        out,
        " {}  v{}",
        bold(&CONFIG.forum_name),
        env!("CARGO_PKG_VERSION")
    )
    .ok();
    writeln!(out, "{}", rule()).ok();
    writeln!(out).ok();

    // Status
    writeln!(out, " {}", bold("Status")).ok();

    if CONFIG.tls.enabled {
        if CONFIG.enable_tor_support {
            let backend_addr = CONFIG.loopback_addr_with_port(
                CONFIG
                    .bind_addr
                    .rsplit_once(':')
                    .and_then(|(_, port)| port.parse::<u16>().ok())
                    .unwrap_or(8080),
            );
            let backend_status = format!(
                "{}  ({}  {}  {})",
                green("RUNNING"),
                dim(&backend_addr),
                bullet(),
                dim("Tor-internal"),
            );
            row(&mut out, "HTTP Backend", &backend_status);
        } else {
            row(&mut out, "HTTP App", &red("DISABLED"));
        }
        if CONFIG.tls.redirect_http {
            let redirect_status = format!(
                "{}  (port {})",
                green("RUNNING"),
                cyan(&CONFIG.tls.http_port.to_string()),
            );
            row(&mut out, "HTTP Redirect", &redirect_status);
        }
    } else {
        let local_status = format!("{}  ({})", green("RUNNING"), dim(&CONFIG.bind_addr));
        row(&mut out, "Local Server", &local_status);
    }

    // HTTPS
    if CONFIG.tls.enabled {
        let https_status = format!(
            "{}  (port {}  {}  {})",
            green("RUNNING"),
            cyan(&CONFIG.tls.port.to_string()),
            bullet(),
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
            None => row(
                &mut out,
                "Tor",
                &yellow(&format!("BOOTSTRAPPING{}", ellipsis())),
            ),
        }
    }

    writeln!(out).ok();

    // Endpoints
    writeln!(out, " {}", bold("Endpoints")).ok();

    if CONFIG.tls.enabled {
        if CONFIG.tls.redirect_http {
            let redirect_url = format!("http://localhost:{}", CONFIG.tls.http_port);
            row(&mut out, "HTTP Redirect", &cyan(&redirect_url));
        }
    } else {
        // Derive the local URL — use localhost when bound to 0.0.0.0 / ::.
        let local_url = {
            let port = CONFIG
                .bind_addr
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .unwrap_or(8080);
            format!("http://localhost:{port}")
        };
        row(&mut out, "Local", &cyan(&local_url));
    }

    if CONFIG.tls.enabled {
        let https_url = format!("https://localhost:{}", CONFIG.tls.port);
        row(&mut out, "HTTPS", &cyan(&https_url));
    }

    match &stats.onion_address {
        Some(addr) => row(&mut out, "Onion", &cyan(&format!("http://{addr}"))),
        None if CONFIG.enable_tor_support => {
            row(
                &mut out,
                "Onion",
                &dim(&format!("waiting for Tor{}", ellipsis())),
            );
        }
        None => {}
    }

    writeln!(out).ok();

    // Activity
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

    // Content
    writeln!(out, " {}", bold("Content")).ok();
    row(&mut out, "Boards", &stats.boards.to_string());
    row(&mut out, "Threads", &stats.threads.to_string());
    row(&mut out, "Posts", &stats.posts.to_string());
    writeln!(out).ok();

    // Storage
    writeln!(out, " {}", bold("Storage")).ok();
    row(&mut out, "Database", &fmt_bytes(stats.db_bytes));
    row(&mut out, "Uploads", &fmt_bytes(stats.upload_bytes));
    writeln!(out).ok();

    // Upload spinner
    if stats.active_uploads > 0 {
        let frame = spinner_frame(stats.spinner_tick);
        writeln!(out, " {}  {} uploading", cyan(frame), stats.active_uploads).ok();
        writeln!(out).ok();
    }

    // Footer
    writeln!(out, "{}", rule()).ok();
    writeln!(out,
        " {} Help  {} Reload  {} Boards  {} New board  {} New admin  {} Del thread  {} Logs  {} Quit",
        bold("[H]"), bold("[R]"), bold("[B]"), bold("[C]"), bold("[A]"), bold("[D]"), bold("[L]"), bold("[Q]"),
    ).ok();
    writeln!(out, "{}", rule()).ok();

    out
}

/// Render the live-log view.
pub fn render_log_view() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(8192);
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Log View", bold(marker())).ok();
    writeln!(out, "{}", rule()).ok();
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
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Back", bold("[L]")).ok();
    writeln!(out, "{}", rule()).ok();
    out
}

/// Return the number of terminal rows available for log content.
fn log_body_height() -> usize {
    crossterm::terminal::size().map_or(24, |(_, rows)| usize::from(rows.saturating_sub(8)).max(6))
}

/// Find the newest main-process log file.
fn latest_log_file(logs_dir: &Path) -> Option<PathBuf> {
    let mut files = std::fs::read_dir(logs_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| crate::logging::is_main_log_file(path))
        .collect::<Vec<_>>();
    files.sort();
    files.pop()
}

/// Read at most the newest `max_bytes` of a log file.
fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), String> {
    use std::io::{Seek as _, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| format!("Open log: {e}"))?;
    let len = file
        .metadata()
        .map_err(|e| format!("Log metadata: {e}"))?
        .len();
    let start = len.saturating_sub(u64::try_from(max_bytes).unwrap_or(u64::MAX));
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("Seek log: {e}"))?;

    let buf = std::fs::read(path).map_err(|e| format!("Read log: {e}"))?;
    let start = usize::try_from(start).unwrap_or(usize::MAX);
    let text = String::from_utf8_lossy(buf.get(start..).unwrap_or_default()).into_owned();
    let truncated = start > 0;
    let content = if truncated {
        match text.find('\n') {
            Some(pos) if pos + 1 < text.len() => text.get(pos + 1..).unwrap_or_default().to_owned(),
            _ => text,
        }
    } else {
        text
    };
    Ok((content, truncated))
}

/// Render the console key reference.
#[must_use]
pub fn render_help() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(1024);
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Key Reference", bold(marker())).ok();
    writeln!(out, "{}", rule()).ok();
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
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Back", bold("[H]")).ok();
    writeln!(out, "{}", rule()).ok();
    out
}

/// Render the per-board content summary.
#[must_use]
pub fn render_board_list(stats: &ChanStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(1024);
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Board List", bold(marker())).ok();
    writeln!(out, "{}", rule()).ok();
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
        writeln!(out, " {}", dim(&rule_char().repeat(34))).ok();
        for (short, thr, pst) in &stats.board_rows {
            let label = format!("/{short}/");
            writeln!(out, " {:<14}   {:>8}   {:>8}", cyan(&label), thr, pst).ok();
        }
    }
    writeln!(out).ok();
    writeln!(out, "{}", rule()).ok();
    writeln!(
        out,
        " {} Back   {} New board   {} Del thread",
        bold("[B]"),
        bold("[C]"),
        bold("[D]")
    )
    .ok();
    writeln!(out, "{}", rule()).ok();
    out
}

/// Render the graceful-shutdown confirmation prompt.
#[must_use]
pub fn render_confirm_quit() -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(256);
    writeln!(out, "{}", rule()).ok();
    writeln!(out, " {} Confirm Quit", bold(marker())).ok();
    writeln!(out, "{}", rule()).ok();
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
    writeln!(out, "{}", rule()).ok();
    out
}

#[cfg(test)]
/// Dashboard and log-selection tests.
mod tests {
    use super::{latest_log_file, render_dashboard};
    use crate::server::console::ChanStats;

    #[test]
    /// Displays the active `FFmpeg` video count.
    fn dashboard_shows_active_ffmpeg_video_count() {
        let stats = ChanStats {
            active_ffmpeg_videos: 3,
            ..ChanStats::default()
        };

        let rendered = render_dashboard(&stats);

        assert!(
            rendered.contains("FFmpeg"),
            "dashboard should contain the FFmpeg row"
        );
        assert!(
            rendered.contains("videos processing"),
            "dashboard should label active video processing"
        );
        assert!(
            rendered.contains('3'),
            "dashboard should display the active-video count"
        );
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertion failures are the intended failure mechanism for this test"
    )]
    /// Selects the main log instead of the dependency log.
    fn log_view_selects_main_log_not_dependency_log() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("rustchan.2026-04-01.log"), "main")?;
        std::fs::write(
            dir.path().join(crate::logging::DEPENDENCY_LOG_FILE_NAME),
            "deps",
        )?;

        let latest =
            latest_log_file(dir.path()).ok_or_else(|| anyhow::anyhow!("main log not found"))?;

        assert_eq!(
            latest.file_name().and_then(|name| name.to_str()),
            Some("rustchan.2026-04-01.log"),
            "main-process logs should win over dependency logs"
        );
        Ok(())
    }
}
