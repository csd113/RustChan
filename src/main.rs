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
// Data lives in  <exe-dir>/rustchan-data/   (override with CHAN_DATA_DIR env var)
// Static CSS is compiled into the binary — no external files needed.
//
// All HTTP server logic lives in server/server.rs.
// CLI types and admin commands live in server/cli.rs.
// Terminal console and startup banner live in server/console.rs.
// ChanNet / RustWave gateway lives in chan_net/mod.rs (second listener, port 7070).

use anyhow::Context;
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
mod server;
mod templates;
mod utils;
mod workers;

use config::CONFIG;

// ─── Data directory resolution ───────────────────────────────────────────────
//
// Precedence:
//   1. `CHAN_DATA_DIR` environment variable  (explicit override)
//   2. `<binary-dir>/rustchan-data`          (binary-relative)
//   3. `./rustchan-data`                     (fallback — CWD-relative)

fn resolve_data_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("CHAN_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("rustchan-data");
        }
    }

    // Logging is not initialised yet — stderr is all we have.
    eprintln!(
        "Warning: could not determine binary directory; \
         falling back to ./rustchan-data"
    );
    std::path::PathBuf::from("./rustchan-data")
}

// ─── Entry point ─────────────────────────────────────────────────────────────
//
// We build the Tokio runtime manually (instead of `#[tokio::main]`) so we can
// tune `max_blocking_threads` via `blocking_threads` in settings.toml or the
// `CHAN_BLOCKING_THREADS` environment variable.  The blocking pool is used by
// every `spawn_blocking` call — page renders, DB queries, file I/O.

fn main() -> anyhow::Result<()> {
    // ── Resolve and create data directory ────────────────────────────────
    let data_dir = resolve_data_dir();

    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;

    // ── Initialise logging ───────────────────────────────────────────────
    logging::init_logging(&data_dir);

    tracing::info!(
        target: "startup",
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %data_dir.display(),
        "rustchan starting",
    );

    // ── Load configuration ───────────────────────────────────────────────
    //
    // CONFIG is a LazyLock<Config> that initialises on first access.  Wrap
    // the access in catch_unwind so a config-parse panic becomes a clean
    // error rather than an opaque abort.
    let blocking_threads = std::panic::catch_unwind(|| CONFIG.blocking_threads).map_err(|_| {
        anyhow::anyhow!("Failed to load configuration — check settings.toml for syntax errors")
    })?;

    anyhow::ensure!(
        (1..=4096).contains(&blocking_threads),
        "blocking_threads must be between 1 and 4096, got {blocking_threads}",
    );

    // ── Parse CLI arguments ──────────────────────────────────────────────
    let cli = server::cli::Cli::parse();
    let port = cli.port;
    let chan_net = cli.chan_net;

    // ── Admin commands are synchronous — no async runtime needed ─────────
    if let Some(server::cli::Command::Admin { action }) = cli.command {
        return server::cli::run_admin(action);
    }

    // ── Build the Tokio runtime (server path only) ───────────────────────
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(blocking_threads)
        .build()
        .context("Failed to build Tokio runtime")?;

    rt.block_on(async move { server::run_server(port, chan_net).await })
}
