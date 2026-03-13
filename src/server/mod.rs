// server/mod.rs — Server subsystem module.
//
// Sub-modules:
//   cli.rs     — Cli, Command, AdminAction clap types + run_admin
//   console.rs — TermStats, print_stats, spawn_keyboard_handler, print_banner,
//                kb_* interactive console functions
//   server.rs  — run_server, build_router, background tasks,
//                static asset handlers, hsts_middleware, track_requests,
//                ScopedDecrement, global request-counter atomics

pub mod cli;
pub mod console;
#[allow(clippy::module_inception)]
pub mod server;

pub use server::run_server;

// Re-export the global atomics so console.rs (and any future module) can
// reference them as `crate::server::REQUEST_COUNT` etc. rather than the
// longer `crate::server::server::REQUEST_COUNT`.
pub use server::{ACTIVE_IPS, ACTIVE_UPLOADS, IN_FLIGHT, REQUEST_COUNT, SPINNER_TICK};
