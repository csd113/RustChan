#![expect(
    unused_crate_dependencies,
    reason = "Cargo exposes binary-only dependencies to the library target, including the intentional rustls-webpki security floor for RUSTSEC-2026-0049"
)]
//! Shared `RustChan` domain and rendering modules.
//!
//! The standalone CLI builds the same source modules in its binary crate while
//! this library exposes the reusable model, persistence, media, and UI layers.

/// Banner validation, storage, selection, and rendering.
pub mod banner;
/// HTTP cache-control header helpers.
pub mod cache;
/// CAPTCHA challenge generation and validation.
pub mod captcha;
#[cfg(test)]
/// `ChanNet` federation and gateway endpoints used by crate-level tests.
pub mod chan_net;
/// Runtime configuration and persistent settings.
pub mod config;
/// SQLite persistence operations and models.
pub mod db;
#[cfg(test)]
/// External-tool and in-process Tor detection used by crate-level tests.
pub mod detect;
/// Application error types and HTTP error responses.
pub mod error;
/// Favicon validation and storage.
pub mod favicon;
#[cfg(test)]
/// Browser-facing and administration handlers used by crate-level tests.
pub mod handlers;
#[cfg(test)]
/// Structured application and console logging used by crate-level tests.
pub mod logging;
/// Uploaded-media inspection and conversion.
pub mod media;
#[cfg(test)]
/// Request middleware and shared state used by crate-level tests.
pub mod middleware;
/// Shared application data models.
pub mod models;
/// Durable filesystem-operation journal and recovery.
pub mod pending_fs;
#[cfg(test)]
/// HTTP runtime and administration CLI used by crate-level tests.
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
#[cfg(test)]
/// TLS certificate handling used by crate-level tests.
pub mod tls;
/// Cryptographic, filesystem, sanitization, and redirect helpers.
pub mod utils;
#[cfg(test)]
/// Background job processing used by crate-level tests.
pub mod workers;
