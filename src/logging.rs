// logging.rs — Structured logging initialisation.
//
// Sets up two output layers:
//   1. stderr  — human-readable, `info` and above (terminal/systemd journal)
//   2. file    — JSON formatted, `debug` and above, written to
//               `rustchan.log` inside `log_dir`
//
// Respects `RUST_LOG` env var if set.

use std::path::Path;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// `log_dir` is the directory where `rustchan.log` will be written.
/// Typically this is the directory that contains the binary itself
/// (`std::env::current_exe()?.parent()`), so log output stays alongside
/// the executable and is easy to find.
///
/// The file appender is non-rotating — a single `rustchan.log` is used for
/// the lifetime of the process.  Rotation can be added later via
/// `tracing_appender::rolling::daily` / `hourly` without changing callers.
pub fn init_logging(log_dir: &Path) {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("rustchan=info,tower_http=warn"));

    // stderr layer — compact human-readable output for the terminal.
    let stderr_layer = fmt::layer().with_target(false).compact();

    // File layer — JSON, includes file/line for structured log analysis.
    // Uses `tracing_appender::rolling::never` so the file is never rotated
    // automatically; restart the process to start a fresh log.
    let file_appender = tracing_appender::rolling::never(log_dir, "rustchan.log");
    let file_layer = fmt::layer()
        .json()
        .with_writer(file_appender)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(fmt::format::FmtSpan::CLOSE);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
}
