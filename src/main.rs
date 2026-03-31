// main.rs — Single-binary entry point.
//
// Run modes (via subcommands):
//   rustchan-cli                               → start the web server (default)
//   rustchan-cli serve --chan-net              → start server + ChanNet API listener
//   rustchan-cli admin create-admin  <u> <p>   → create an admin user
//   rustchan-cli admin reset-password <u> <p>  → reset admin password
//   rustchan-cli admin list-admins             → list admins
//   rustchan-cli admin create-board  <s> <n> [desc] [--nsfw]
//   rustchan-cli admin delete-board  <short>
//   rustchan-cli admin list-boards
//   rustchan-cli admin ban    <ip_hash> <reason> [hours]
//   rustchan-cli admin unban  <ban_id>
//   rustchan-cli admin list-bans
//
// Data lives in  <exe-dir>/rustchan-data/   (override with CHAN_DB / CHAN_UPLOADS)
// Static CSS is compiled into the binary — no external files needed.
//
// All HTTP server logic lives in server/server.rs.
// CLI types and admin commands live in server/cli.rs.
// Terminal console and startup banner live in server/console/.
// ChanNet / RustWave gateway lives in chan_net/mod.rs (second listener, port 7070).

use clap::Parser;

mod chan_net;
mod config;
mod db;
mod detect;
mod error;
mod handlers;
mod logging;
mod media;
mod middleware;
mod models;
mod pending_fs;
mod server;
mod templates;
pub(crate) mod tls;
mod utils;
mod workers;

use config::{generate_settings_file_if_missing, CONFIG};

// ─── Entry point ─────────────────────────────────────────────────────────────
//
// `#[tokio::main]` does not expose `max_blocking_threads`, so we build the
// runtime manually.  The blocking thread pool (used by every `spawn_blocking`
// call — page renders, DB queries, file I/O) defaults to logical CPUs × 4 but
// can be tuned via `blocking_threads` in settings.toml or the
// CHAN_BLOCKING_THREADS environment variable.

#[allow(clippy::arithmetic_side_effects)]
#[allow(clippy::expect_used)]
fn main() -> anyhow::Result<()> {
    // ── Double-click / no-TTY guard ───────────────────────────────────────────
    // When launched from a file manager (Linux) or Explorer (Windows), stdout
    // is not a TTY. Re-attach to a terminal so the banner, first-run wizard,
    // and keyboard console are visible to the user.
    //
    // RUSTCHAN_SPAWNED prevents the child from looping back here.
    {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() && std::env::var("RUSTCHAN_SPAWNED").is_err() {
            #[cfg(target_os = "linux")]
            {
                let exe = std::env::current_exe()?;
                let exe_str = exe.to_string_lossy();
                // Try terminal emulators in order of likelihood.
                // CRITICAL: Command::new takes the *binary name only*.
                // Passing "env RUSTCHAN_SPAWNED=1 /path/to/bin" as one string
                // to Command::new is the execve bug — the Linux kernel does not
                // tokenise it; it looks for a file literally named that string.
                for term in [
                    "xterm",
                    "gnome-terminal",
                    "konsole",
                    "xfce4-terminal",
                    "x-terminal-emulator",
                ] {
                    if std::process::Command::new(term) // ← binary name only
                        .arg("-e")
                        .arg(exe_str.as_ref()) // ← separate arg
                        .env("RUSTCHAN_SPAWNED", "1") // ← env set on child, not in arg string
                        .spawn()
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                // No terminal emulator found — fall through and run headless.
            }
            #[cfg(target_os = "windows")]
            {
                // On Windows, AllocConsole() attaches a new console window
                // in-process. No re-exec needed; execution continues below
                // with stdout now connected to the new window.
                unsafe {
                    windows_sys::Win32::System::Console::AllocConsole();
                }
            }
        }
    }

    // Resolve the binary directory, then derive rustchan-data/ so the log
    // file lands in <exe-dir>/rustchan-data/ alongside the database and
    // uploads.  Falls back to "./rustchan-data" if the exe path cannot be
    // determined.
    let binary_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    // Create rustchan-data/ before init_logging so the rolling file appender
    // can open the directory immediately on startup.  run_server() also calls
    // create_dir_all on this path; calling it twice is safe.
    let data_dir = binary_dir.join("rustchan-data");
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("Warning: could not create rustchan-data directory: {e}");
    }
    generate_settings_file_if_missing();

    logging::init_logging(&data_dir);

    // Install a panic hook that restores the terminal before printing the
    // panic message.  Without this, a panic while the TUI is active leaves
    // the terminal in raw/alternate-screen mode and the operator sees nothing.
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // cleanup() is a no-op if the TUI was never started or already cleaned up.
            crate::server::cleanup();
            default_hook(info);
        }));
    }

    tracing::info!(
        target: "startup",
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %data_dir.display(),
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

    let cli = server::cli::Cli::parse();

    rt.block_on(async move {
        // Install the ring crypto provider once before anything else accesses
        // rustls. ok() = harmless if already installed (tests, re-runs).
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();

        match cli.command {
            // Default (no subcommand) or explicit `serve`: start the server.
            None | Some(server::cli::Command::Serve) => {
                let result = server::run_server(cli.port, cli.chan_net).await;
                // Restore terminal unconditionally after the server exits
                // (graceful shutdown, SIGTERM, etc.).  cleanup() is idempotent.
                crate::server::cleanup();
                result
            }

            Some(server::cli::Command::Admin { action }) => {
                server::cli::run_admin(action)?;
                Ok(())
            }
        }
    })
}
