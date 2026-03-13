// main.rs — Single-binary entry point.
//
// Run modes (via subcommands):
//   rustchan-cli                               → start the web server (default)
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
// Terminal console and startup banner live in server/console.rs.

use clap::Parser;

mod config;
mod db;
mod detect;
mod error;
mod handlers;
mod logging;
mod media;
mod middleware;
mod models;
mod server;
mod templates;
mod utils;
mod workers;

use config::CONFIG;

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
    // Resolve the binary directory so the log file lands alongside the
    // executable (e.g. /opt/rustchan/rustchan.log).  Falls back to "." if the
    // path cannot be determined.
    let binary_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    logging::init_logging(&binary_dir);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %binary_dir.display(),
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
        .expect("Failed to build Tokio runtime");

    let cli = server::cli::Cli::parse();

    rt.block_on(async move {
        match cli.command {
            None | Some(server::cli::Command::Serve { port: None }) => {
                server::run_server(None).await
            }
            Some(server::cli::Command::Serve { port }) => server::run_server(port).await,
            Some(server::cli::Command::Admin { action }) => {
                server::cli::run_admin(action)?;
                Ok(())
            }
        }
    })
}
