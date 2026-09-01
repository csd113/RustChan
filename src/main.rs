#![expect(
    unused_crate_dependencies,
    reason = "Cargo exposes the companion chan library and the transitive rustls-webpki security floor to this standalone binary target"
)]
#![cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "private items are documented only when comments add rationale beyond their names and types"
    )
)]

//! Single-binary `RustChan` entry point.

use clap::Parser as _;
use std::io::{self, Write as _};

/// Banner validation, storage, selection, and rendering.
pub mod banner;
/// HTTP cache-control header helpers.
pub mod cache;
/// CAPTCHA challenge generation and validation.
pub mod captcha;
/// `ChanNet` federation and gateway endpoints.
pub mod chan_net;
/// Runtime configuration and persistent settings.
pub mod config;
/// SQLite persistence operations and models.
pub mod db;
/// Optional external-tool and in-process Tor detection.
pub mod detect;
/// Application error types and HTTP error responses.
pub mod error;
/// Favicon validation and storage.
pub mod favicon;
/// Browser-facing and administration HTTP handlers.
pub mod handlers;
/// Structured application, dependency, and console logging.
pub mod logging;
/// Uploaded-media inspection and conversion.
pub mod media;
/// Request middleware and shared application state.
pub mod middleware;
/// Shared application data models.
pub mod models;
/// Durable filesystem-operation journal and recovery.
pub mod pending_fs;
/// HTTP runtime, terminal console, and administration CLI.
pub mod server;
/// Server-rendered HTML templates.
pub mod templates;
#[cfg(test)]
/// Reusable fixtures for unit tests.
pub mod test_fixtures;
#[cfg(test)]
/// Shared state and request builders for crate-local tests.
pub(crate) mod test_support;
/// Built-in theme metadata.
pub mod theme;
/// Custom theme configuration and CSS generation.
pub mod theme_builder;
/// TLS certificate loading and acceptor construction used by the server.
pub mod tls;
/// Cryptographic, filesystem, sanitization, and redirect helpers.
pub mod utils;
/// Background media and maintenance job processing.
pub mod workers;

use config::{generate_settings_file_if_missing, CONFIG};

/// Relaunches a no-argument, non-interactive Linux or Windows invocation in a
/// terminal when a supported terminal host is available.
///
/// # Errors
///
/// Returns an error when the current executable path cannot be resolved.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows")),
    expect(
        clippy::unnecessary_wraps,
        reason = "the shared cross-platform call site is fallible only on Linux and Windows"
    )
)]
fn relaunch_in_terminal_if_needed() -> anyhow::Result<bool> {
    use std::io::IsTerminal as _;

    let launched_without_arguments = std::env::args_os().nth(1).is_none();
    if !launched_without_arguments
        || io::stdout().is_terminal()
        || std::env::var("RUSTCHAN_SPAWNED").is_ok()
    {
        return Ok(false);
    }

    #[cfg(target_os = "linux")]
    {
        let exe = std::env::current_exe()?;
        let exe_str = exe.to_string_lossy();
        // Command::new takes the binary name alone; the executable and child
        // environment must remain separate exec arguments.
        for terminal in [
            "xterm",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "x-terminal-emulator",
        ] {
            if std::process::Command::new(terminal)
                .arg("-e")
                .arg(exe_str.as_ref())
                .env("RUSTCHAN_SPAWNED", "1")
                .spawn()
                .is_ok()
            {
                return Ok(true);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let exe = std::env::current_exe()?;
        // `start` creates a console; the empty argument is its required
        // window-title slot when the executable path is quoted.
        if std::process::Command::new("cmd.exe")
            .args(["/C", "start", ""])
            .arg(exe)
            .env("RUSTCHAN_SPAWNED", "1")
            .spawn()
            .is_ok()
        {
            return Ok(true);
        }
    }

    Ok(false)
}

// Entry point
// `#[tokio::main]` does not expose `max_blocking_threads`, so we build the
// runtime manually.  The blocking thread pool (used by every `spawn_blocking`
// call — page renders, DB queries, file I/O) defaults to logical CPUs × 4 but
// can be tuned via `blocking_threads` in settings.toml or the
// CHAN_BLOCKING_THREADS environment variable.

fn main() -> anyhow::Result<()> {
    // Parse before terminal attachment, filesystem creation, logging, settings
    // generation, or CONFIG access. Clap handles --help and --version here and
    // exits successfully without mutating runtime state.
    let cli = server::cli::Cli::parse();
    config::configure_data_dir(cli.data_dir.as_deref())?;

    // Double-click / no-TTY guard
    // When launched from a file manager (Linux) or Explorer (Windows), stdout
    // is not a TTY. Re-attach to a terminal so the banner, first-run wizard,
    // and keyboard console are visible to the user.
    if relaunch_in_terminal_if_needed()? {
        return Ok(());
    }

    // Create the runtime data directories before init_logging so the rolling
    // file appender can open the directory immediately on startup.
    let data_dir = config::data_dir();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        drop(writeln!(
            io::stderr().lock(),
            "Warning: could not create rustchan-data directory: {e}"
        ));
    }
    if let Err(e) = config::migrate_runtime_layout_if_needed() {
        drop(writeln!(
            io::stderr().lock(),
            "Warning: could not migrate runtime layout: {e}"
        ));
    }
    let log_dir = config::logs_dir();
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        drop(writeln!(
            io::stderr().lock(),
            "Warning: could not create rustchan-data/logs directory: {e}"
        ));
    }
    generate_settings_file_if_missing();

    logging::init_logging(&log_dir);

    // Install a panic hook that restores the terminal before printing the
    // panic message.  Without this, a panic while the TUI is active leaves
    // the terminal in raw/alternate-screen mode and the operator sees nothing.
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // cleanup() is a no-op if the TUI was never started or already cleaned up.
            server::cleanup();
            default_hook(info);
        }));
    }

    let dependency_log_path = logging::dependency_log_path(&log_dir);
    tracing::info!(
        target: "startup",
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        dependency_log = %dependency_log_path.display(),
        "rustchan starting",
    );

    // CONFIG must be initialised before building the runtime so that
    // blocking_threads is available.  This is safe because CONFIG is a
    // LazyLock<Config> that initialises itself on first access.
    let blocking_threads = CONFIG.blocking_threads;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(blocking_threads)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build Tokio runtime: {e}"))?;

    rt.block_on(async move {
        // Install the ring crypto provider once before anything else accesses
        // rustls. ok() = harmless if already installed (tests, re-runs).
        drop(rustls::crypto::ring::default_provider().install_default());

        match cli.command {
            // Default (no subcommand) or explicit `serve`: start the server.
            None | Some(server::cli::Command::Serve) => {
                let result = server::run_server(cli.port, cli.chan_net).await;
                // Restore terminal unconditionally after the server exits
                // (graceful shutdown, SIGTERM, etc.).  cleanup() is idempotent.
                server::cleanup();
                result
            }

            Some(server::cli::Command::Admin { action }) => {
                server::cli::run_admin(action)?;
                Ok(())
            }
        }
    })
}
