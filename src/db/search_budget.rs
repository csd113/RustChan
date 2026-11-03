//! Cooperative SQLite work budgets for anonymous search count/sort operations.

use crate::error::{AppError, Result};
use rusqlite::{Connection, ErrorCode};
use std::time::{Duration, Instant};

/// Progress checks are amortized over this many SQLite VM instructions.
const CHECK_INTERVAL: i32 = 10_000;
/// Approximate combined VM instruction budget for a count and result page.
const MAX_CHECKS: usize = 200;
/// Cooperative wall-clock allowance, checked at each VM progress checkpoint.
const SEARCH_DEADLINE: Duration = Duration::from_millis(500);

/// Removes the callback before a pooled connection can serve other requests.
struct SearchBudget<'a> {
    /// Connection whose callback belongs exclusively to the scoped search.
    conn: &'a Connection,
}

impl Drop for SearchBudget<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.conn.progress_handler(0, None::<fn() -> bool>) {
            tracing::error!(%error, "could not clear SQLite search progress handler");
        }
    }
}

/// Run bounded search work and translate budget exhaustion into a useful client error.
pub(super) fn with_search_budget<T>(
    conn: &Connection,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let started = Instant::now();
    let mut checks = 0usize;
    conn.progress_handler(
        CHECK_INTERVAL,
        Some(move || {
            checks = checks.saturating_add(1);
            checks >= MAX_CHECKS || started.elapsed() >= SEARCH_DEADLINE
        }),
    )?;
    let guard = SearchBudget { conn };
    let result = work();
    drop(guard);
    match result {
        Err(AppError::Internal(error)) if error.chain().any(|cause| matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == ErrorCode::OperationInterrupted
        )) => Err(AppError::BadRequest("This search is too broad. Please use more specific words.".into())),
        other => other,
    }
}
