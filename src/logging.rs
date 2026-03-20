// logging.rs — Structured logging initialisation.
//
// Two output channels:
//
//   1. Terminal (stdout, TTY-aware)
//      • When stdout is a TTY (interactive terminal):
//          HH:MM:SS.mmm [LEVEL] [component] message  key=val …
//        Coloured level tags; component tag in cyan; fits in 80 columns.
//      • When stdout is not a TTY (piped, systemd, Docker, nohup):
//          YYYY-MM-DD HH:MM:SS.mmm [LEVEL] [component] message  key=val …
//        Zero ANSI codes — clean for log shippers and `grep`.
//
//      All writes go through `CONSOLE_MUTEX` so `console.rs` interactive
//      output never interleaves with log events.
//
//   2. Log file (rustchan.YYYY-MM-DD.log, human-readable text, daily rotation)
//      Same fixed-column format as the non-TTY terminal output, with two extras:
//        • Millisecond precision on every timestamp.
//        • WARN and ERROR lines append  (src/file.rs:line)  at the end so you
//          can jump straight to the source without grepping the codebase.
//      One event per line — easy to tail, grep, and read in any text editor.
//      If you need machine-parseable output for a log shipper (Loki, Datadog,
//      etc.) swap the FileFormatter layer for .json() — see the comment in
//      init_logging().
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

use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, MakeWriter};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// ─── Shared console write lock ────────────────────────────────────────────────

/// Global mutex that serialises all stdout writes across the process.
///
/// Both the tracing terminal layer (via [`ConsoleLock`]) and the `console.rs`
/// interactive helpers acquire this before writing. The result: log events
/// and stats/prompt blocks never interleave, even under high async concurrency.
static CONSOLE_MUTEX: LazyLock<parking_lot::Mutex<()>> =
    LazyLock::new(|| parking_lot::Mutex::new(()));

// ─── Non-blocking file writer guard ─────────────────────────────────────────────
//
// `tracing_appender::non_blocking()` spawns a background thread that drains a
// channel and flushes writes to disk after every batch.  The returned
// `WorkerGuard` MUST stay alive for the entire process lifetime — dropping it
// stops the background thread, which silently discards any buffered log lines
// that have not yet been flushed, producing blank or truncated log files.
//
// Using a module-level OnceLock means the guard is stored here without
// requiring callers to thread it through their own state, and without changing
// the public `init_logging(&Path)` signature.
static FILE_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

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

// ─── Shared formatting helpers ────────────────────────────────────────────────

/// Write the fixed-width level tag.  Returns the ANSI open/close codes for
/// the tag (empty strings when `ansi` is false).
fn write_level_tag(writer: &mut Writer<'_>, level: Level, ansi: bool) -> fmt::Result {
    let (tag, open, close) = if ansi {
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
    write!(writer, "{open}{tag}{close} ")
}

/// Write the fixed-width (8-char) component tag, cyan when `ansi` is true.
fn write_component_tag(writer: &mut Writer<'_>, target: &str, ansi: bool) -> fmt::Result {
    let component = extract_component(target);
    let chars: Vec<char> = component.chars().collect();
    let len = chars.len().min(8);
    let display: String = chars.get(..len).unwrap_or(&chars).iter().collect();

    let (open, close) = if ansi {
        ("\x1b[36m", "\x1b[0m")
    } else {
        ("", "")
    };
    write!(writer, "{open}[{display:<8}]{close} ")
}

// ─── Terminal formatter ───────────────────────────────────────────────────────

/// Writes one compact line per log event to the terminal.
///
/// TTY mode (local dev):
///   `14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///
/// Non-TTY mode (piped / systemd / Docker):
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///
/// Columns are fixed-width so the message text always starts at the same
/// horizontal position, making it easy to scan down a busy log stream.
struct TerminalFormatter;

impl<S, N> FormatEvent<S, N> for TerminalFormatter
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

        // ── Timestamp (with milliseconds) ─────────────────────────────────────
        if tty {
            let now = chrono::Local::now();
            write!(writer, "{} ", now.format("%H:%M:%S%.3f"))?;
        } else {
            let now = chrono::Utc::now();
            write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;
        }

        // ── Level + component columns ─────────────────────────────────────────
        let level = *event.metadata().level();
        write_level_tag(&mut writer, level, tty)?;
        write_component_tag(&mut writer, event.metadata().target(), tty)?;

        // ── Message and structured fields ─────────────────────────────────────
        // tracing_subscriber writes the `message` field first, then all other
        // key=value fields separated by spaces — e.g.:
        //   "Request received  method=GET path=/b/ latency_ms=4"
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

// ─── File formatter ───────────────────────────────────────────────────────────

/// Writes one human-readable line per log event to the log file.
///
/// Format:
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///   `2026-03-18 14:22:01.456 [ERROR] [error   ] DB query failed  err=no such table  (src/db/posts.rs:79)`
///
/// Differences from the terminal format:
///   • Always UTC with the full date — the file is an archive, not a live view.
///   • No ANSI colour codes — clean for `grep`, `less`, and text editors.
///   • WARN and ERROR lines append `(file:line)` at the end so you can jump
///     directly to the call site without having to search the source tree.
struct FileFormatter;

impl<S, N> FormatEvent<S, N> for FileFormatter
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
        // ── Timestamp — UTC, full date, millisecond precision ─────────────────
        let now = chrono::Utc::now();
        write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;

        // ── Level + component columns (no colour) ─────────────────────────────
        let level = *event.metadata().level();
        let meta = event.metadata();
        write_level_tag(&mut writer, level, false)?;
        write_component_tag(&mut writer, meta.target(), false)?;

        // ── Message and structured key=value fields ───────────────────────────
        ctx.format_fields(writer.by_ref(), event)?;

        // ── Source location suffix for WARN and ERROR ─────────────────────────
        // Only attached at these levels because:
        //   • INFO/DEBUG events fire thousands of times per minute on a busy
        //     board; the call site is rarely the interesting part.
        //   • WARN/ERROR events are rare and almost always need follow-up —
        //     having the exact file:line avoids a grep → blame cycle.
        if matches!(level, Level::ERROR | Level::WARN) {
            if let (Some(file), Some(line)) = (meta.file(), meta.line()) {
                // Trim the leading "src/" that Rust adds to all file paths so
                // the suffix stays compact: "(db/posts.rs:79)" not
                // "(src/db/posts.rs:79)".
                let trimmed = file.strip_prefix("src/").unwrap_or(file);
                write!(writer, "  ({trimmed}:{line})")?;
            }
        }

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
/// 1. **Terminal layer** — `TerminalFormatter` writes via `ConsoleLock`
///    (`CONSOLE_MUTEX`). Human-readable, coloured when TTY, plain otherwise.
///    Serialised with `console.rs` output via the shared mutex.
///
/// 2. **File layer** — `FileFormatter`, daily rotation, `debug`+.
///    Written to `{log_dir}/rustchan.YYYY-MM-DD.log`.
///    One human-readable line per event; WARN/ERROR include source location.
///    Independent of the console lock; uses `tracing-appender`'s own lock.
///
/// To switch the file layer to JSON for a log aggregator (Loki, Datadog, …):
/// replace `FileFormatter` with `.json()` and add `.with_file(true)
/// .with_line_number(true)` to the layer builder.
pub fn init_logging(log_dir: &Path) {
    use std::io::IsTerminal;

    let tty = io::stdout().is_terminal();
    IS_TTY.store(tty, Ordering::Relaxed);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("rustchan=info,tower_http=warn"));

    let terminal_layer = tracing_subscriber::fmt::layer()
        .event_format(TerminalFormatter)
        .with_writer(ConsoleLock);

    // Build the rolling file appender.
    // FIX (filename): tracing_appender::rolling::daily(dir, "rustchan.log")
    // appends the date *after* the full string → "rustchan.log.2024-01-15"
    // (no .log extension on rotated files).  The builder API separates
    // prefix from suffix → "rustchan.2024-01-15.log".
    let rolling = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("rustchan")
        .filename_suffix("log")
        .build(log_dir)
        .unwrap_or_else(|e| {
            eprintln!("Warning: could not create log file appender: {e}");
            tracing_appender::rolling::never(log_dir, "rustchan.log")
        });

    // FIX (blank files): wrap the appender in a non-blocking writer.
    // RollingFileAppender uses an internal BufWriter that only flushes when
    // the buffer fills to 8 KB or the appender is dropped.  On a quiet server
    // you never hit 8 KB between restarts, so the file stays empty.
    // non_blocking() moves writes to a background thread that flushes after
    // every batch, guaranteeing data reaches disk promptly.
    // The WorkerGuard is stored in FILE_GUARD so it lives for the entire
    // process — see the comment on that static for why this matters.
    let (non_blocking_writer, guard) = tracing_appender::non_blocking(rolling);
    let _ = FILE_GUARD.set(guard);

    let file_layer = tracing_subscriber::fmt::layer()
        .event_format(FileFormatter)
        .with_writer(non_blocking_writer);

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
