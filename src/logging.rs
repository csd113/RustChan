// db/logging.rs — Structured logging initialisation.
//
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

use std::path::Path;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialise the global tracing subscriber.
///
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
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("rustchan=info,tower_http=warn"));

    // stderr layer — compact human-readable output for the terminal.
    let stderr_layer = fmt::layer().with_target(false).compact();

    // File layer — JSON, includes file/line for structured log analysis.
    // FIX[#33]: daily rotation; files are named `rustchan.log.YYYY-MM-DD`.
    let file_appender = tracing_appender::rolling::daily(db_dir, "rustchan.log");
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
