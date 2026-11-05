// Terminal logs use ANSI and short timestamps only on a TTY. File logs use
// daily rotation and route dependency targets separately. All terminal writers
// share `CONSOLE_MUTEX` to prevent log events from interleaving with the TUI.
//
// `IS_TTY`
// Set once during `init_logging()` via `std::io::IsTerminal`.
// Read at every log event and every `console.rs` write to decide whether
// to emit ANSI escape codes. Exposed via `is_tty()`.
//
// Respects the RUST_LOG environment variable if set.

mod events;

use events::{
    is_external_source, is_tor_target, prepare_event_fields, write_event_fields, LogEventFields,
    TorDescriptorTimeoutLimiter, TOR_DESCRIPTOR_TIMEOUT_SUPPRESSION_WINDOW,
};

use std::fmt;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{
    layer::{Context, Layer, SubscriberExt as _},
    util::SubscriberInitExt as _,
    EnvFilter,
};

// Shared console write lock
/// Global mutex that serialises all stdout writes across the process.
///
/// Both the tracing terminal layer (via [`ConsoleLock`]) and the `console.rs`
/// interactive helpers acquire this before writing. The result: log events
/// and stats/prompt blocks never interleave, even under high async concurrency.
static CONSOLE_MUTEX: LazyLock<parking_lot::Mutex<()>> =
    LazyLock::new(|| parking_lot::Mutex::new(()));

// Non-blocking file writer guard
// `tracing_appender::non_blocking()` spawns a background thread that drains a
// channel and flushes writes to disk after every batch.  The returned
// `WorkerGuard` must stay alive while logging is active. `shutdown()` takes and
// drops both guards to drain queued output at the end of the executable lifecycle.
//
// Module-level mutex-held Options own the guards without requiring callers
// to thread them through their own state, and without changing
// the public `init_logging(&Path)` signature.
/// Keeps the main log writer's background worker alive for the process lifetime.
static MAIN_FILE_GUARD: parking_lot::Mutex<Option<tracing_appender::non_blocking::WorkerGuard>> =
    parking_lot::Mutex::new(None);
/// Keeps the dependency log writer's background worker alive for the process lifetime.
static DEPENDENCY_FILE_GUARD: parking_lot::Mutex<
    Option<tracing_appender::non_blocking::WorkerGuard>,
> = parking_lot::Mutex::new(None);

/// Fallback main-log filename used when daily rotation cannot be initialized.
pub const MAIN_LOG_FALLBACK_FILE_NAME: &str = "rustchan.log";
/// Filename used for third-party dependency logs.
pub const DEPENDENCY_LOG_FILE_NAME: &str = "dep_log.log";

// TTY detection
/// Whether standard output was an interactive terminal at startup.
static IS_TTY: AtomicBool = AtomicBool::new(false);
/// Whether terminal output may contain ANSI escape sequences.
static ANSI_ENABLED: AtomicBool = AtomicBool::new(false);

/// Returns `true` when stdout was a real interactive terminal at startup.
///
/// Used by the log formatter and `console.rs` to decide whether to emit ANSI
/// escape codes. Always `false` when output is piped, redirected, or captured
/// by a process supervisor like systemd.
pub fn is_tty() -> bool {
    IS_TTY.load(Ordering::Relaxed)
}

/// Returns `true` when stdout is an interactive terminal that can consume ANSI
/// escape sequences.
///
/// On Windows this also probes/enables virtual terminal
/// processing so raw colour sequences are not printed literally.
pub fn ansi_enabled() -> bool {
    ANSI_ENABLED.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn detect_ansi_enabled(tty: bool) -> bool {
    if !tty {
        return false;
    }

    crossterm::ansi_support::supports_ansi()
}

#[cfg(not(windows))]
/// Returns terminal capability directly on platforms with native ANSI support.
const fn detect_ansi_enabled(tty: bool) -> bool {
    tty
}

/// Set to `true` once the full-screen TUI alternate screen is active.
/// Any code that would print banners or boxes to stdout (e.g. the Tor onion
/// address box in detect.rs) must check this and skip its output — the TUI
/// dashboard owns the screen and will display the information itself.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Records whether the full-screen terminal interface currently owns stdout.
pub fn set_tui_active(active: bool) {
    // Let an in-flight log writer finish before the console takes the screen.
    // Cleanup must remain lock-free here: a panic hook may run from a writer
    // that already holds CONSOLE_MUTEX.
    let _guard = active.then(|| CONSOLE_MUTEX.lock());
    TUI_ACTIVE.store(active, Ordering::SeqCst);
}

/// Returns whether the full-screen terminal interface currently owns stdout.
pub fn is_tui_active() -> bool {
    TUI_ACTIVE.load(Ordering::Relaxed)
}

// Component name extraction
/// Namespace prefixes routed to `RustChan`'s main log.
const APP_LOG_TARGETS: &[&str] = &[
    "admin",
    "board",
    "chan",
    "config",
    "console",
    "db",
    "polls",
    "rustchan",
    "rustchan_cli",
    "server",
    "sessions",
    "startup",
    "tls",
    "workers",
];

/// Exact non-namespaced targets routed to `RustChan`'s main log.
const APP_LOG_EXACT_TARGETS: &[&str] = &["convert", "media_prune"];

/// Returns the configured dependency-log path beneath a log directory.
#[must_use]
pub fn dependency_log_path(log_dir: &Path) -> PathBuf {
    log_dir.join(DEPENDENCY_LOG_FILE_NAME)
}

/// Returns whether a path names a current or rotated `RustChan` main log.
#[must_use]
pub fn is_main_log_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    file_name == MAIN_LOG_FALLBACK_FILE_NAME
        || (file_name.starts_with("rustchan.")
            && path.extension().and_then(|ext| ext.to_str()) == Some("log"))
}

/// Read at most the newest `max_bytes` of a log file without loading its prefix.
///
/// Returns the tail and whether the file was truncated. When the read starts
/// mid-line the partial first line is dropped; when it starts exactly on a line
/// boundary the first complete line is kept.
///
/// # Errors
/// Returns a message naming the failed filesystem step.
pub fn read_log_tail(path: &Path, max_bytes: usize) -> Result<(String, bool), String> {
    let mut file = std::fs::File::open(path).map_err(|error| format!("Open log: {error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("Log metadata: {error}"))?
        .len();
    let start = length.saturating_sub(u64::try_from(max_bytes).unwrap_or(u64::MAX));
    // Inspect the preceding byte so an exact line boundary keeps its first
    // complete line instead of dropping it as an assumed partial prefix.
    let read_start = start.saturating_sub(1);
    file.seek(SeekFrom::Start(read_start))
        .map_err(|error| format!("Seek log: {error}"))
        .map(|_completed_value| ())?;
    let mut bytes =
        Vec::with_capacity(usize::try_from(length.saturating_sub(read_start)).unwrap_or(max_bytes));
    io::copy(
        &mut file.take(length.saturating_sub(read_start)),
        &mut bytes,
    )
    .map_err(|error| format!("Read log: {error}"))
    .map(|_completed_value| ())?;
    let truncated = start > 0;
    let content = if truncated {
        let prefix = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index.saturating_add(1));
        bytes.get(prefix..).unwrap_or_default()
    } else {
        bytes.as_slice()
    };
    Ok((String::from_utf8_lossy(content).into_owned(), truncated))
}

/// Returns whether a tracing target belongs to `RustChan` application code.
fn is_app_log_target(target: &str) -> bool {
    APP_LOG_EXACT_TARGETS.contains(&target)
        || APP_LOG_TARGETS.iter().any(|app_target| {
            target == *app_target
                || target
                    .strip_prefix(*app_target)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
}

/// Returns whether a tracing target belongs to `FFmpeg` integration code.
fn is_ffmpeg_log_target(target: &str) -> bool {
    target == "ffmpeg"
        || target.starts_with("ffmpeg::")
        || target == "ffmpeg_next"
        || target.starts_with("ffmpeg_next::")
        || target.ends_with("::ffmpeg")
        || target.contains("::ffmpeg::")
}

/// Returns whether a tracing target is routed to the main log.
fn is_main_log_target(target: &str) -> bool {
    is_app_log_target(target) || is_ffmpeg_log_target(target)
}

/// Extract the useful short name from a tracing target (module path).
///
/// `rustchan::server::server` → `server`
/// `rustchan::db::mod`        → `db`
/// `rustchan::workers`        → `workers`
/// `tower_http::trace`        → `trace`.
fn extract_component(target: &str) -> &str {
    let mut parts = target.rsplit("::");
    let last = parts.next().unwrap_or(target);
    if last == "mod" {
        parts.next().unwrap_or(last)
    } else {
        last
    }
}

/// Maps a tracing target to its compact display component.
fn display_component(target: &str) -> String {
    if is_tor_target(target) {
        match extract_component(target) {
            "bootstrap" | "dirmgr" | "state" | "sqlite" => "tor-dir".to_owned(),
            "guard" | "guardmgr" => "tor-net".to_owned(),
            "chanmgr" => "tor-chan".to_owned(),
            "circmgr" | "mgr" | "reactor" => "tor-circ".to_owned(),
            "hspool" | "publish" | "descriptor" => "onion".to_owned(),
            "config" => "tor-cfg".to_owned(),
            other => other.to_owned(),
        }
    } else {
        extract_component(target).to_owned()
    }
}

// Shared formatting helpers
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
    let component = display_component(target);
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

/// Builds the default INFO-level filter used when `RUST_LOG` is absent.
fn default_env_filter() -> EnvFilter {
    EnvFilter::new(
        // Default to INFO for all enabled targets so third-party dependency
        // events are not silently dropped; route filters below keep them out
        // of the terminal and main RustChan log.
        "info,\
         rustchan=info,\
         rustchan_cli=info,\
         chan=info,\
         admin=info,\
         board=info,\
         workers=info,\
         server=info,\
         db=info,\
         startup=info,\
         sessions=info,\
         polls=info,\
         console=info,\
         tls=info,\
         config=info",
    )
}

/// Parses `RUST_LOG`, falling back to `RustChan`'s default filter.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .ok()
        .or_else(|| {
            crate::config::saved_log_filter().and_then(|filter| EnvFilter::try_new(filter).ok())
        })
        .unwrap_or_else(default_env_filter)
}

// Terminal formatter
/// Writes one compact line per log event to the terminal.
///
/// TTY mode (local dev):
///   `14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`.
///
/// Non-TTY mode (piped / systemd / Docker):
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`.
///
/// Columns are fixed-width so the message text always starts at the same
/// horizontal position, making it easy to scan down a busy log stream.
fn format_terminal_event(
    mut writer: Writer<'_>,
    event: &Event<'_>,
    fields: &LogEventFields,
) -> fmt::Result {
    let tty = IS_TTY.load(Ordering::Relaxed);
    let ansi = ANSI_ENABLED.load(Ordering::Relaxed);
    let meta = event.metadata();

    let now = chrono::Local::now();
    if tty {
        write!(writer, "{} ", now.format("%H:%M:%S%.3f"))?;
    } else {
        write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;
    }

    let level = *meta.level();
    write_level_tag(&mut writer, level, ansi)?;
    write_component_tag(&mut writer, meta.target(), ansi)?;

    // Message and structured fields
    // tracing_subscriber writes the `message` field first, then all other
    // key=value fields separated by spaces — e.g.:
    //   "Request received  method=GET path=/b/ latency_ms=4"
    write_event_fields(&mut writer, fields)?;
    writeln!(writer)
}

// File formatter
/// Writes one human-readable line per log event to the log file.
///
/// Format:
///   `2026-03-18 14:22:01.123 [INFO ] [server  ] HTTP server listening  addr=0.0.0.0:8080`
///   `2026-03-18 14:22:01.456 [ERROR] [error   ] DB query failed  err=no such table  (src/db/posts.rs:79)`.
///
/// Differences from the terminal format:
///   • Local time with the full date; the terminal shows local time without the date.
///   • No ANSI colour codes — clean for `grep`, `less`, and text editors.
///   • WARN and ERROR lines append `(file:line)` at the end so you can jump
///     directly to the call site without having to search the source tree.
fn format_file_event(
    mut writer: Writer<'_>,
    event: &Event<'_>,
    fields: &LogEventFields,
) -> fmt::Result {
    let meta = event.metadata();

    let now = chrono::Local::now();
    write!(writer, "{} ", now.format("%Y-%m-%d %H:%M:%S%.3f"))?;

    let level = *event.metadata().level();
    write_level_tag(&mut writer, level, false)?;
    write_component_tag(&mut writer, meta.target(), false)?;

    write_event_fields(&mut writer, fields)?;

    // Source location suffix for WARN and ERROR
    // Only attached at these levels because:
    //   • INFO/DEBUG events fire thousands of times per minute on a busy
    //     board; the call site is rarely the interesting part.
    // • WARN/ERROR events are rare and almost always need follow-up
    //     having the exact file:line avoids a grep → blame cycle.
    if matches!(level, Level::ERROR | Level::WARN) {
        if let (Some(file), Some(line)) = (meta.file(), meta.line()) {
            if is_external_source(file) {
                writeln!(writer)?;
                return Ok(());
            }
            // Trim the leading "src/" that Rust adds to all file paths so
            // the suffix stays compact: "(db/posts.rs:79)" not
            // "(src/db/posts.rs:79)".
            let trimmed = file.strip_prefix("src/").unwrap_or(file);
            write!(writer, "  ({trimmed}:{line})")?;
        }
    }

    writeln!(writer)
}

// MakeWriter: routes terminal writes through CONSOLE_MUTEX
/// Holds the console lock guard for the duration of one log event write.
///
/// The `_guard` field is held solely for its `Drop` side-effect (releasing
/// `CONSOLE_MUTEX`). It is never accessed directly.
struct LockedWriter {
    /// Guard that keeps the shared console mutex locked during the event write.
    _guard: parking_lot::MutexGuard<'static, ()>,
    /// Whether output is discarded while the full-screen interface is active.
    suppress: bool,
}

impl io::Write for LockedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.suppress {
            return Ok(buf.len());
        }
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.suppress {
            return Ok(());
        }
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
            suppress: is_tui_active(),
        }
    }
}

/// Routes one prepared event to its enabled terminal and file outputs.
struct OutputLayer<T, M, D> {
    /// Terminal writer, serialized with the interactive console.
    terminal: T,
    /// Writer for application and `FFmpeg` events.
    main_file: M,
    /// Writer for third-party events.
    dependency_file: D,
    /// One constant-space suppression state shared by all outputs.
    descriptor_timeouts: std::sync::Arc<parking_lot::Mutex<TorDescriptorTimeoutLimiter>>,
}

impl<T, M, D> OutputLayer<T, M, D> {
    /// Creates the process's routing layer with an idle warning limiter.
    fn new(terminal: T, main_file: M, dependency_file: D) -> Self {
        Self {
            terminal,
            main_file,
            dependency_file,
            descriptor_timeouts: std::sync::Arc::new(parking_lot::Mutex::new(
                TorDescriptorTimeoutLimiter::new(TOR_DESCRIPTOR_TIMEOUT_SUPPRESSION_WINDOW),
            )),
        }
    }
}

impl<S, T, M, D> Layer<S> for OutputLayer<T, M, D>
where
    S: Subscriber,
    T: for<'a> MakeWriter<'a> + 'static,
    M: for<'a> MakeWriter<'a> + 'static,
    D: for<'a> MakeWriter<'a> + 'static,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Decide once before writer fanout. The limiter never holds a lock
        // while formatting or writing, and no decision survives its event.
        let Some(fields) =
            prepare_event_fields(meta.target(), meta.file(), event, &self.descriptor_timeouts)
        else {
            return;
        };
        if is_main_log_target(meta.target()) {
            write_output(&self.terminal, meta, |writer| {
                format_terminal_event(writer, event, &fields)
            });
            write_output(&self.main_file, meta, |writer| {
                format_file_event(writer, event, &fields)
            });
        } else {
            write_output(&self.dependency_file, meta, |writer| {
                format_file_event(writer, event, &fields)
            });
        }
    }
}

/// Formats one line before acquiring its output writer, matching formatter I/O.
fn write_output<'a, W: MakeWriter<'a>>(
    make_writer: &'a W,
    metadata: &Metadata<'_>,
    format_event: impl FnOnce(Writer<'_>) -> fmt::Result,
) {
    let mut buffer = String::new();
    if format_event(Writer::new(&mut buffer)).is_ok() {
        let mut writer = make_writer.make_writer_for(metadata);
        drop(writer.write_all(buffer.as_bytes()));
    }
}

// Initialisation
/// Initialise the global tracing subscriber. Call exactly once at startup,
/// before any `tracing::info!` or `tracing::warn!` calls are made.
///
/// Detects whether stdout is an interactive terminal and stores the result
/// in `IS_TTY` for the process lifetime. Installs one routing layer with three outputs:
///
/// 1. **Terminal output** — writes via `ConsoleLock`
///    (`CONSOLE_MUTEX`). Human-readable, coloured when TTY, plain otherwise.
///    Serialised with `console.rs` output via the shared mutex. `RustChan` and
///    `FFMPEG` targets only.
///
/// 2. **Main file output** — daily rotation, filtered by
///    `RUST_LOG` or the default INFO-level filter.
///    Written to `{log_dir}/rustchan.YYYY-MM-DD.log`.
///    One human-readable line per event; WARN/ERROR include source location.
///    `RustChan` and `FFMPEG` targets only.
///
/// 3. **Dependency file output** — written to
///    `{log_dir}/dep_log.log`. Receives third-party dependency targets that
///    are hidden from the terminal/main log.
///    Independent of the console lock; uses `tracing-appender`'s own lock.
///
pub fn init_logging(log_dir: &Path) {
    use std::io::IsTerminal as _;

    let tty = io::stdout().is_terminal();
    IS_TTY.store(tty, Ordering::Relaxed);
    ANSI_ENABLED.store(
        detect_ansi_enabled(tty)
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").is_ok_and(|term| term != "dumb")
            && !crate::server::console::capabilities::plain(),
        Ordering::Relaxed,
    );

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
            drop(writeln!(
                io::stderr().lock(),
                "Warning: could not create log file appender: {e}"
            ));
            tracing_appender::rolling::never(log_dir, "rustchan.log")
        });

    // FIX (blank files): wrap the appender in a non-blocking writer.
    // RollingFileAppender uses an internal BufWriter that only flushes when
    // the buffer fills to 8 KB or the appender is dropped.  On a quiet server
    // you never hit 8 KB between restarts, so the file stays empty.
    // non_blocking() moves writes to a background thread that flushes after
    // every batch, guaranteeing data reaches disk promptly.
    // The WorkerGuard is stored in MAIN_FILE_GUARD so it lives for the entire
    // process — see the comment on that static for why this matters.
    let (main_file_writer, main_guard) = tracing_appender::non_blocking(rolling);
    *MAIN_FILE_GUARD.lock() = Some(main_guard);

    let dependency_log = tracing_appender::rolling::never(log_dir, DEPENDENCY_LOG_FILE_NAME);
    let (dependency_file_writer, dependency_guard) = tracing_appender::non_blocking(dependency_log);
    *DEPENDENCY_FILE_GUARD.lock() = Some(dependency_guard);

    tracing_subscriber::registry()
        .with(OutputLayer::new(
            ConsoleLock,
            main_file_writer,
            dependency_file_writer,
        ))
        .with(env_filter())
        .init();
}

// Console print helpers
// All helpers acquire `CONSOLE_MUTEX` before writing so they serialise
// correctly with the tracing terminal layer. Use these instead of
// `println!`/`print!` in `console.rs` and `detect.rs`.

/// Write a raw pre-formatted block exactly as provided (no trailing newline added).
///
/// Used for banners, install-hint blocks, and stats sections that are already
/// fully formatted including their own newlines.
pub fn console_print_raw(block: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    drop(write!(io::stdout(), "{block}"));
    drop(io::stdout().flush());
}

/// Write a prompt string (no newline) and flush stdout, under the console lock.
///
/// The lock is released before returning so the subsequent blocking `read_line`
/// call does not prevent log events from being written while waiting for input.
pub fn console_prompt(msg: &str) {
    let _guard = CONSOLE_MUTEX.lock();
    drop(write!(io::stdout(), "{msg}"));
    drop(io::stdout().flush());
    // _guard dropped here — stdin read happens outside the lock
}

/// Drain queued file logging at the end of the executable lifecycle.
pub fn shutdown() {
    drop(MAIN_FILE_GUARD.lock().take());
    drop(DEPENDENCY_FILE_GUARD.lock().take());
}

#[cfg(test)]
#[path = "logging/tests/regressions.rs"]
mod tests;
