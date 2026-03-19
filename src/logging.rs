// db/logging.rs — Structured logging initialisation.
//
<<<<<<< Updated upstream
// Sets up two output layers:
//   1. stderr  — human-readable, `info` and above (terminal/systemd journal)
//   2. file    — JSON formatted, `debug` and above, written to
//               `rustchan.log` inside `db_dir`
//
// Respects `RUST_LOG` env var if set.
//
// NOTE: this module lives in `db/` so that log files are written alongside the
// database file rather than next to the binary.  The caller should pass the
// same directory used for `chan.db` (typically the directory containing the
// binary at first run, but overridable via config).
=======
// Two output channels:
//
//   1. Terminal (stdout, TTY-aware)
//      • When stdout is a TTY (interactive terminal):
//          HH:MM:SS [LEVEL] [component] message
//        Coloured level tags; component tag in cyan; fits in 80 columns.
//      • When stdout is not a TTY (piped, systemd, Docker, nohup):
//          YYYY-MM-DDTHH:MM:SSZ [LEVEL] [component] message
//        Zero ANSI codes — clean for log shippers and `grep`.
//
//      All writes go through `CONSOLE_MUTEX` so `console.rs` interactive
//      output never interleaves with log events.
//
//   2. Log file (rustchan.YYYY-MM-DD.log, JSON, always-on, daily rotation)
//      Full structured JSON with timestamps, file, line, and fields.
//      Never interleaves — tracing-appender uses its own internal lock.
//
// `CONSOLE_MUTEX`
// ───────────────
// A `parking_lot::Mutex<()>` used as a coordinated write-lock on stdout.
// Both this module's `ConsoleLock` `MakeWriter` (used by the tracing
// terminal layer) and `console.rs`'s helpers acquire this same lock before
// writing. This eliminates byte-level interleaving between log events and
// stats/prompt output regardless of concurrent async activity.
//
// `IS_TTY`
// ────────
// Set once during `init_logging()` via `std::io::IsTerminal`.
// Read at every log event and every `console.rs` write to decide whether
// to emit ANSI escape codes. Exposed via `is_tty()`.
//
// Respects the RUST_LOG environment variable if set.
>>>>>>> Stashed changes

use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{FmtSpan, FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, MakeWriter};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// ─── Shared console write lock ────────────────────────────────────────────────

/// Global mutex that serialises all stdout writes across the process.
///
<<<<<<< Updated upstream
/// `db_dir` is the directory where `rustchan.log` will be written.
/// This should be the same directory as `chan.db` so that logs and the
/// database stay together and are easy to locate, back up, and rotate as
/// a unit.
///
/// FIX[#33]: The file appender now uses daily rotation
/// (`tracing_appender::rolling::daily`) instead of `rolling::never`.  A single
/// append-only log file grows without bound on a busy instance; if the
/// filesystem fills, `SQLite`'s WAL checkpoint will fail and can corrupt the
/// database.  Daily rotation caps each log file to roughly one day of output
/// and leaves old files on disk with a date suffix so they can be pruned by a
/// standard `logrotate` rule or a cron job.
///
/// Suggested logrotate stanza (`/etc/logrotate.d/rustchan`):
/// ```text
/// /path/to/db/rustchan.log.* {
///     daily
///     rotate 14
///     compress
///     missingok
///     notifempty
///     copytruncate
/// }
/// ```
pub fn init_logging(db_dir: &Path) {
=======
/// Both the tracing terminal layer (via [`ConsoleLock`]) and the `console.rs`
/// interactive helpers acquire this before writing. The result: log events
/// and stats/prompt blocks never interleave, even under high async concurrency.
static CONSOLE_MUTEX: LazyLock<parking_lot::Mutex<()>> =
    LazyLock::new(|| parking_lot::Mutex::new(()));

// ─── TTY detection ────────────────────────────────────────────────────────────

static IS_TTY: AtomicBool = AtomicBool::new(false);

/// Returns `true` when stdout was a real interactive terminal at startup.
///
/// Used by the log formatter and `console.rs` to decide whether to emit ANSI
/// escape codes. Always `false` when output is piped, redirected, or captured
/// by a process supervisor like systemd.
pub fn is_tty() -> bool {
    IS_TTY.load(Ordering::Relaxed)
}

// ─── Component name extraction ────────────────────────────────────────────────

/// Extract the useful short name from a tracing target (module path).
///
/// `rustchan::server::server` → `server`
/// `rustchan::db::mod`        → `db`
/// `rustchan::workers`        → `workers`
/// `tower_http::trace`        → `trace`
fn extract_component(target: &str) -> &str {
    let mut parts = target.rsplit("::");
    let last = parts.next().unwrap_or(target);
    if last == "mod" {
        parts.next().unwrap_or(last)
    } else {
        last
    }
}

// ─── Custom terminal event formatter ─────────────────────────────────────────

/// Writes a compact, structured line per log event.
///
/// TTY mode:
///   `14:22:01 [INFO ] [server  ] Listening on http://0.0.0.0:8080`
///
/// Non-TTY mode (piped / systemd / Docker):
///   `2026-03-18T14:22:01Z [INFO ] [server  ] Listening on http://0.0.0.0:8080`
///
/// The component column is fixed at 8 characters (truncated or space-padded)
/// so message text always starts at the same column.
struct ConsoleFormatter;

impl<S, N> FormatEvent<S, N> for ConsoleFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let tty = IS_TTY.load(Ordering::Relaxed);

        // ── Timestamp ─────────────────────────────────────────────────────────
        if tty {
            let now = chrono::Local::now();
            write!(writer, "{} ", now.format("%H:%M:%S"))?;
        } else {
            let now = chrono::Utc::now();
            write!(writer, "{} ", now.format("%Y-%m-%dT%H:%M:%SZ"))?;
        }

        // ── Level tag (fixed 7 chars: "[LEVEL]") ────────────────────────────
        let level = *event.metadata().level();
        let (tag, open, close) = if tty {
            match level {
                Level::ERROR => ("[ERROR]", "\x1b[1;31m", "\x1b[0m"),
                Level::WARN => ("[WARN ]", "\x1b[33m", "\x1b[0m"),
                Level::INFO => ("[INFO ]", "", ""),
                Level::DEBUG => ("[DEBUG]", "\x1b[2m", "\x1b[0m"),
                Level::TRACE => ("[TRACE]", "\x1b[2m", "\x1b[0m"),
            }
        } else {
            match level {
                Level::ERROR => ("[ERROR]", "", ""),
                Level::WARN => ("[WARN ]", "", ""),
                Level::INFO => ("[INFO ]", "", ""),
                Level::DEBUG => ("[DEBUG]", "", ""),
                Level::TRACE => ("[TRACE]", "", ""),
            }
        };
        write!(writer, "{open}{tag}{close} ")?;

        // ── Component tag (8-char fixed column, cyan in TTY) ─────────────────
        let target = event.metadata().target();
        let component = extract_component(target);
        let component_chars: Vec<char> = component.chars().collect();
        let display_len = component_chars.len().min(8);
        // Use .get() to avoid a panic on the slice if somehow display_len > len.
        let display: String = component_chars
            .get(..display_len)
            .unwrap_or(&component_chars)
            .iter()
            .collect();

        let (ctag_open, ctag_close) = if tty {
            ("\x1b[36m", "\x1b[0m")
        } else {
            ("", "")
        };
        write!(writer, "{ctag_open}[{display:<8}]{ctag_close} ")?;

        // ── Message and structured key=value fields ───────────────────────────
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

// ─── MakeWriter: routes terminal writes through CONSOLE_MUTEX ─────────────────

/// Holds the console lock guard for the duration of one log event write.
///
/// The `_guard` field is held solely for its `Drop` side-effect (releasing
/// `CONSOLE_MUTEX`). It is never accessed directly.
struct LockedWriter {
    _guard: parking_lot::MutexGuard<'static, ()>,
}

impl io::Write for LockedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// `MakeWriter` that returns a [`LockedWriter`] for each log event.
///
/// The tracing layer calls `make_writer()` once per event, writes through
/// the returned writer, then drops it — atomically releasing the stdout lock.
struct ConsoleLock;

impl<'a> MakeWriter<'a> for ConsoleLock {
    type Writer = LockedWriter;

    fn make_writer(&'a self) -> LockedWriter {
        LockedWriter {
            _guard: CONSOLE_MUTEX.lock(),
        }
    }
}

// ─── Initialisation ───────────────────────────────────────────────────────────

/// Initialise the global tracing subscriber. Call exactly once at startup,
/// before any `tracing::info!` or `tracing::warn!` calls are made.
///
/// Detects whether stdout is an interactive terminal and stores the result
/// in `IS_TTY` for the process lifetime. Installs two layers:
///
/// 1. **Terminal layer** — `ConsoleFormatter` writes via `ConsoleLock`
///    (`CONSOLE_MUTEX`). Human-readable, coloured when TTY, plain otherwise.
///    Serialised with `console.rs` output via the shared mutex.
///
/// 2. **File layer** — JSON, daily rotation, `debug`+.
///    Written to `{log_dir}/rustchan.YYYY-MM-DD.log`.
///    Independent of the console lock; uses `tracing-appender`'s own lock.
pub fn init_logging(log_dir: &Path) {
    use std::io::IsTerminal;

    let tty = io::stdout().is_terminal();
    IS_TTY.store(tty, Ordering::Relaxed);

>>>>>>> Stashed changes
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("rustchan=info,tower_http=warn"));

    let terminal_layer = tracing_subscriber::fmt::layer()
        .event_format(ConsoleFormatter)
        .with_writer(ConsoleLock);

<<<<<<< Updated upstream
    // File layer — JSON, includes file/line for structured log analysis.
    // FIX[#33]: daily rotation; files are named `rustchan.log.YYYY-MM-DD`.
    let file_appender = tracing_appender::rolling::daily(db_dir, "rustchan.log");
    let file_layer = fmt::layer()
=======
    let file_appender = tracing_appender::rolling::daily(log_dir, "rustchan.log");
    let file_layer = tracing_subscriber::fmt::layer()
>>>>>>> Stashed changes
        .json()
        .with_writer(file_appender)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::CLOSE);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(terminal_layer)
        .with(file_layer)
        .init();
}

// ─── Console print helpers ────────────────────────────────────────────────────
//
// All helpers acquire `CONSOLE_MUTEX` before writing so they serialise
// correctly with the tracing terminal layer. Use these instead of
// `println!`/`print!` in `console.rs` and `detect.rs`.

/// Print `msg` followed by a newline to stdout, under the console lock.
pub fn console_println(msg: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = writeln!(io::stdout(), "{msg}");
}

/// Write a raw pre-formatted block exactly as provided (no trailing newline added).
///
/// Used for banners, install-hint blocks, and stats sections that are already
/// fully formatted including their own newlines.
pub fn console_print_raw(block: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = write!(io::stdout(), "{block}");
    let _ = io::stdout().flush();
}

/// Write a prompt string (no newline) and flush stdout, under the console lock.
///
/// The lock is released before returning so the subsequent blocking `read_line`
/// call does not prevent log events from being written while waiting for input.
pub fn console_prompt(msg: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    let _ = write!(io::stdout(), "{msg}");
    let _ = io::stdout().flush();
    // _guard dropped here — stdin read happens outside the lock
}
