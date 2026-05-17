// server/mod.rs — Server subsystem module.
//
// Sub-modules:
//   cli.rs          — Cli, Command, AdminAction clap types + run_admin
//   console/        — Full-screen TUI: mod, input, dashboard, wizard
//     mod.rs        — RAW_MODE_ACTIVE, ConsoleMode, SharedStats/ChanStats,
//                     start(), cleanup(), render(), collect_stats()
//     input.rs      — crossterm key reader, KeyEvent enum, spawn()
//     dashboard.rs  — pure render functions for all ConsoleMode variants
//     wizard.rs     — multi-step admin wizards (create board/admin, delete thread)
//   server.rs       — run_server, build_router, background tasks,
//                     static asset handlers, hsts_middleware, track_requests,
//                     ScopedDecrement, global request-counter atomics

pub mod cli;
pub mod console;
// The nested module name matches the server layer layout and keeps the public path stable.
#[expect(clippy::module_inception)]
pub mod server;

use std::path::{Path, PathBuf};

pub use server::run_server;

// Re-export the global atomics so console/ (and any future module) can
// reference them as `crate::server::REQUEST_COUNT` etc. rather than the
// longer `crate::server::server::REQUEST_COUNT`.
pub use server::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT, SPINNER_TICK};

// Re-export cleanup so main.rs panic hook can call it without a long path.
pub use console::cleanup;

pub fn parent_dir_or_current(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}
