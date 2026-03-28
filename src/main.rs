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
// Terminal console and startup banner live in server/console/.
// ChanNet / RustWave gateway lives in chan_net/mod.rs (second listener, port 7070).
//
// ─── Architecture notes (tech debt) ─────────────────────────────────────────
//
// CONFIG is currently a process-wide `LazyLock<Config>` singleton.  This makes
// unit testing harder and creates hidden coupling.  A future refactor should
// pass `Arc<Config>` explicitly through the call chain from `main`.
//
// The flat module list (chan_net, config, db, …) should eventually be grouped
// into nested modules by layer (web, data, processing, federation) as the
// codebase grows.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use fs2::FileExt;

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
<<<<<<< HEAD
mod terminal;
=======
pub(crate) mod tls;
>>>>>>> origin/indev-1.1.0-alpha-4
mod utils;
mod workers;

// ─── Graceful shutdown signal ────────────────────────────────────────────────
//
// Listens for Ctrl-C on all platforms, and additionally SIGTERM on Unix.
// The first signal received causes the future to resolve, which in turn
// cancels the server via `tokio::select!`.

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    // FIX 1: Replace .expect() with proper error handling via let-else.
    let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
        tracing::warn!("Failed to register SIGTERM handler; falling back to Ctrl-C only");
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("Received Ctrl-C — initiating graceful shutdown"),
            Err(e) => tracing::error!("Failed to listen for Ctrl-C: {e}"),
        }
        return;
    };

    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            // FIX 2: Replace .expect() with match.
            match result {
                Ok(()) => tracing::info!("Received Ctrl-C — initiating graceful shutdown"),
                Err(e) => tracing::error!("Failed to listen for Ctrl-C: {e}"),
            }
        }
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM — initiating graceful shutdown");
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => tracing::info!("Received Ctrl-C — initiating graceful shutdown"),
        Err(e) => tracing::error!("Failed to listen for Ctrl-C: {e}"),
    }
}

// ─── Data directory resolution ───────────────────────────────────────────────
//
// Precedence:
//   1. `CHAN_DATA_DIR` environment variable     (explicit override)
//   2. `<binary-dir>/rustchan-data`             (binary-relative)
//   3. Hard error — no silent CWD fallback.
//
// After resolution the path is canonicalised (resolving symlinks) and checked
// against a deny-list of dangerous system prefixes.  On Unix the directory
// permissions are also inspected.

fn resolve_data_dir() -> anyhow::Result<PathBuf> {
    // FIX 3: Move static item before any statements to avoid
    //        "adding items after statements" lint.
    static FORBIDDEN_PREFIXES: &[&str] =
        &["/etc", "/proc", "/sys", "/dev", "/boot", "/sbin", "/bin"];

    let (raw, source) = if let Some(dir) = std::env::var_os("CHAN_DATA_DIR") {
        (PathBuf::from(dir), "CHAN_DATA_DIR environment variable")
    } else if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            (parent.join("rustchan-data"), "binary-relative path")
        } else {
            anyhow::bail!(
                "Could not determine parent directory of executable: {}. \
                 Set CHAN_DATA_DIR explicitly.",
                exe.display()
            );
        }
    } else {
        anyhow::bail!(
            "Could not determine executable path and CHAN_DATA_DIR is not set. \
             Please set the CHAN_DATA_DIR environment variable."
        );
    };

    // Create the directory tree if it doesn't exist yet.
    std::fs::create_dir_all(&raw)
        .with_context(|| format!("Failed to create data directory: {}", raw.display()))?;

    // Canonicalise to resolve symlinks and produce an absolute path.
    let canon = raw
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize data directory: {}", raw.display()))?;

    // ── Reject dangerous system paths ────────────────────────────────────
    let canon_str = canon.to_string_lossy();
    for prefix in FORBIDDEN_PREFIXES {
        if canon_str == *prefix || canon_str.starts_with(&format!("{prefix}/")) {
            // FIX 4: Use Display formatting instead of Debug ({:?}) in bail!.
            anyhow::bail!(
                "Refusing to use {} as data directory \
                 (resolves under forbidden prefix {prefix})",
                canon.display()
            );
        }
    }
    // FIX 5: Use Path::new("/") instead of PathBuf::from("/") to avoid
    //        creating an owned instance just for comparison.
    if canon.as_path() == Path::new("/") {
        anyhow::bail!("Refusing to use filesystem root as data directory");
    }

    // ── Warn about loose permissions (Unix only) ─────────────────────────
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(&canon)
            .with_context(|| format!("Failed to stat data directory: {}", canon.display()))?;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "Warning: data directory {} has loose permissions ({:o}). \
                 Consider `chmod 700`.",
                canon.display(),
                mode
            );
        }
    }

    // Log which resolution strategy was used (logging may not be initialised
    // yet, so duplicate to stderr as well).
    eprintln!(
        "Data directory: {} (resolved via {source})",
        canon.display()
    );

    Ok(canon)
}

// ─── Instance lock ───────────────────────────────────────────────────────────
//
// Acquires an exclusive advisory lock on `<data_dir>/rustchan.lock` so that
// two processes cannot run against the same data directory simultaneously,
// which would risk SQLite WAL corruption and file-write races.
//
// The returned `File` handle must be kept alive for the duration of the
// process — dropping it releases the lock.

fn acquire_instance_lock(data_dir: &std::path::Path) -> anyhow::Result<std::fs::File> {
    let lock_path = data_dir.join("rustchan.lock");

    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))?;

    lock_file.try_lock_exclusive().with_context(|| {
        format!(
            "Another rustchan instance is already running against {}. \
             If this is wrong, delete {}.",
            data_dir.display(),
            lock_path.display()
        )
    })?;

    // Write our PID so operators can identify the owning process.
    {
        use std::io::Write;
        let mut f = &lock_file;
        let _ = f.write_all(format!("{}", std::process::id()).as_bytes());
    }

    Ok(lock_file)
}

// ─── Configuration loader ────────────────────────────────────────────────────
//
// Wraps the `CONFIG` LazyLock access and extracts the panic payload into a
// proper `anyhow::Error` so that config-parse errors produce helpful messages
// rather than a generic "check settings.toml" hint.

fn load_blocking_threads() -> anyhow::Result<usize> {
    let result = std::panic::catch_unwind(|| config::CONFIG.blocking_threads);

    match result {
        Ok(bt) => Ok(bt),
        Err(payload) => {
            // FIX 6: Use Option combinators instead of if let/else chain.
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown error (non-string panic payload)".to_owned());

            Err(anyhow::anyhow!(
                "Failed to load configuration: {msg}\n\
                 Hint: check settings.toml for syntax errors."
            ))
        }
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────
//
// We build the Tokio runtime manually (instead of `#[tokio::main]`) so we can
// tune `max_blocking_threads` via `blocking_threads` in settings.toml or the
// `CHAN_BLOCKING_THREADS` environment variable.  The blocking pool is used by
// every `spawn_blocking` call — page renders, DB queries, file I/O.

fn main() -> anyhow::Result<()> {
<<<<<<< HEAD
    // ── Auto-terminal relaunch (double-click / no-TTY support) ───────────
    //
    // Must be the very first thing in main() — before arg parsing, logging,
    // or any other initialisation.  If we are not attached to a TTY and
    // RUSTCHAN_SPAWNED is not set, spawn_in_terminal() opens a terminal
    // emulator, re-runs this binary inside it, and returns true so we exit
    // the current headless process immediately.
    if terminal::relaunch_in_terminal_if_needed()? {
        return Ok(());
=======
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
>>>>>>> origin/indev-1.1.0-alpha-4
    }

    // ── Parse CLI arguments first ────────────────────────────────────────
    //
    // Admin sub-commands are fully synchronous and intentionally run *before*
    // config validation and the Tokio runtime, so that operators can still
    // use admin commands even when settings.toml is broken.
    let cli = server::cli::Cli::parse();

    if let Some(server::cli::Command::Admin { action }) = cli.command {
        return server::cli::run_admin(action);
    }

    // From this point on we are in the "server" path.
    let port = cli.port;
    let chan_net = cli.chan_net;

    // ── Resolve and create data directory ────────────────────────────────
    let data_dir = resolve_data_dir()?;

    // ── Acquire instance lock ────────────────────────────────────────────
    //
    // `_lock_guard` must stay alive until process exit.  Dropping it
    // releases the advisory lock.
    let _lock_guard = acquire_instance_lock(&data_dir)?;

    // ── Load configuration ───────────────────────────────────────────────
    //
    // We validate the config *before* initialising logging so that a bad
    // config file does not cause an unguarded panic inside `init_logging`
    // (which may itself access CONFIG for log-level settings).
    let blocking_threads = load_blocking_threads()?;

    anyhow::ensure!(
        (1..=4096).contains(&blocking_threads),
        "blocking_threads must be between 1 and 4096, got {blocking_threads}",
    );

    if blocking_threads > 512 {
        eprintln!(
            "Warning: blocking_threads is set to {blocking_threads}, which is \
             above Tokio's default of 512. This may cause resource exhaustion \
             on memory-constrained systems."
        );
    }

    // ── Initialise logging ───────────────────────────────────────────────
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
        data_dir = %data_dir.display(),
        blocking_threads,
        "rustchan starting",
    );

    // Log which data directory resolution strategy was used (the earlier
    // eprintln covers the pre-logging window).
    tracing::debug!(
        target: "startup",
        data_dir = %data_dir.display(),
        "Data directory resolved and locked"
    );

    // ── Build the Tokio runtime (server path only) ───────────────────────
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(blocking_threads)
        .build()
        .context("Failed to build Tokio runtime")?;

    // ── Run the server with graceful shutdown ────────────────────────────
    //
    // `tokio::select!` races the server future against the shutdown signal.
    // Whichever resolves first wins; the other branch is cancelled.
    let result = rt.block_on(async move {
        tokio::select! {
            biased; // prefer checking shutdown first when both are ready

<<<<<<< HEAD
            // FIX 7: Use `()` instead of `_` since shutdown_signal() returns `()`.
            () = shutdown_signal() => {
                tracing::info!("Shutdown signal received — draining connections");
=======
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
>>>>>>> origin/indev-1.1.0-alpha-4
                Ok(())
            }
            server_result = server::run_server(port, chan_net) => {
                server_result
            }
        }
    });

    // ── Explicit runtime shutdown with timeout ───────────────────────────
    //
    // Give background tasks (workers, pending DB writes, ChanNet sessions)
    // up to 30 seconds to finish before forcibly dropping them.
    tracing::info!("Waiting up to 30 s for background tasks to drain…");
    rt.shutdown_timeout(Duration::from_secs(30));

    tracing::info!("rustchan shut down cleanly");
    result
}
