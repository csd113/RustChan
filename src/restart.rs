//! Manual settings-restart guidance and normal server lifecycle budgets.

use std::sync::LazyLock;

/// Per-process readiness identity, independent of configuration secrets.
pub static INSTANCE: LazyLock<uuid::Uuid> = LazyLock::new(uuid::Uuid::new_v4);
/// Maximum HTTP request drain before forced connection closure.
pub const HTTP_DRAIN: std::time::Duration = std::time::Duration::from_secs(10);
/// Total graceful application shutdown budget.
pub const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug)]
/// Administrator guidance for saved settings requiring an operator restart.
pub struct View {
    /// Safe settings status or manual restart guidance.
    pub message: String,
}
