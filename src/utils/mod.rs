//! Shared utility modules.

/// Random token generation and hashing helpers.
pub mod crypto;
/// Uploaded-file validation and storage helpers.
pub mod files;
/// Filesystem path and archive-entry safety checks.
pub mod fs_security;
/// Internal redirect and query-component helpers.
pub mod redirect;
/// Post-body sanitization and formatting.
pub mod sanitize;
/// Imageboard tripcode generation.
pub mod tripcode;
