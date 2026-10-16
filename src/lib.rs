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
/// Runtime configuration and persistent settings.
pub mod config;
/// SQLite persistence operations and models.
pub mod db;
/// Application error types and HTTP error responses.
pub mod error;
/// Favicon validation and storage.
pub mod favicon;
/// Uploaded-media inspection and conversion.
pub mod media;
/// Shared application data models.
pub mod models;
/// Durable filesystem-operation journal and recovery.
pub mod pending_fs;
/// Settings restart coordination and supervisor recovery.
pub mod restart;
/// Server-rendered HTML templates.
pub mod templates;
/// Built-in theme metadata.
pub mod theme;
/// Custom theme configuration and CSS generation.
pub mod theme_builder;
/// Signed release discovery and isolated native update transactions.
pub mod updates;
/// Cryptographic, filesystem, sanitization, and redirect helpers.
pub mod utils;
