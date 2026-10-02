//! Timing-only diagnostics; never record SQL parameters or private content.

use anyhow::{Context as _, Result};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Slow operation threshold; normal checkouts and short writes stay quiet.
const SLOW: Duration = Duration::from_millis(250);
/// Minimum interval between warnings in each pool-event category.
const WARNING_INTERVAL_MS: u64 = 30_000;

/// Pool-wide events avoid changing the existing pool type or checkout API.
#[derive(Debug)]
pub(super) struct PoolEvents {
    /// Monotonic reference time for rate limiting.
    started: Instant,
    /// Last slow checkout warning timestamp.
    checkout_warning: AtomicU64,
    /// Last long connection lifetime warning timestamp.
    hold_warning: AtomicU64,
    /// Last pool exhaustion warning timestamp.
    timeout_warning: AtomicU64,
}

impl PoolEvents {
    /// Initialize independent per-pool warning budgets.
    pub(super) fn new() -> Self {
        Self {
            started: Instant::now(),
            checkout_warning: AtomicU64::new(0),
            hold_warning: AtomicU64::new(0),
            timeout_warning: AtomicU64::new(0),
        }
    }

    /// Atomically grant at most one warning per interval and category.
    fn should_warn(&self, last: &AtomicU64) -> bool {
        let now = u64::try_from(self.started.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let previous = last.load(Ordering::Relaxed);
        (previous == 0 || now.saturating_sub(previous) >= WARNING_INTERVAL_MS)
            && last
                .compare_exchange(previous, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }
}

impl r2d2::HandleEvent for PoolEvents {
    fn handle_checkout(&self, event: r2d2::event::CheckoutEvent) {
        if event.duration() >= SLOW && self.should_warn(&self.checkout_warning) {
            tracing::warn!(target: "db", event = "slow_checkout",
                connection_id = event.connection_id(), duration_ms = %event.duration().as_millis(),
                "database pool checkout waited longer than 250 ms");
        }
    }

    fn handle_checkin(&self, event: r2d2::event::CheckinEvent) {
        if event.duration() >= SLOW && self.should_warn(&self.hold_warning) {
            tracing::warn!(target: "db", event = "long_connection_hold",
                connection_id = event.connection_id(), duration_ms = %event.duration().as_millis(),
                "database connection held longer than 250 ms");
        }
    }

    fn handle_timeout(&self, event: r2d2::event::TimeoutEvent) {
        if self.should_warn(&self.timeout_warning) {
            tracing::warn!(target: "db", event = "pool_timeout", timeout_ms = %event.timeout().as_millis(),
                "database pool checkout timed out");
        }
    }
}

/// Measure lock acquisition separately from the posting write-lock window.
#[derive(Debug)]
pub(super) struct WriteTiming {
    /// Fixed operation name, never request data.
    operation: &'static str,
    /// Time the write lock was acquired.
    acquired: Instant,
    /// Time spent waiting to begin.
    wait: Duration,
}

impl WriteTiming {
    /// Acquire the immediate lock and start transaction timing.
    pub(super) fn begin(conn: &rusqlite::Connection, operation: &'static str) -> Result<Self> {
        let started = Instant::now();
        let begin = conn.execute_batch("BEGIN IMMEDIATE");
        let wait = started.elapsed();
        if begin.is_err() || wait >= SLOW {
            tracing::debug!(target: "db", event = "write_lock_wait", operation,
                wait_ms = %wait.as_millis(), failed = begin.is_err(),
                "SQLite immediate write-lock acquisition");
        }
        begin.context(operation)?;
        Ok(Self {
            operation,
            acquired: Instant::now(),
            wait,
        })
    }
}

impl Drop for WriteTiming {
    fn drop(&mut self) {
        let held = self.acquired.elapsed();
        if held >= SLOW || self.wait >= SLOW {
            // Explicit db=debug enables detailed write diagnostics; overload
            // does not produce an unbounded stream at normal info/warn levels.
            tracing::debug!(target: "db", event = "posting_transaction", operation = self.operation,
                wait_ms = %self.wait.as_millis(), transaction_ms = %held.as_millis(),
                "SQLite posting write-lock duration");
        }
    }
}

/// Fixed-label SQL timing for the measured read paths; parameters stay private.
pub(super) struct QueryTiming {
    /// Static operation label.
    operation: &'static str,
    /// Monotonic query start.
    started: Instant,
}

impl QueryTiming {
    /// Begin a read measurement without additional queries or allocations.
    pub(super) fn start(operation: &'static str) -> Self {
        Self {
            operation,
            started: Instant::now(),
        }
    }
}

impl Drop for QueryTiming {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if elapsed >= SLOW {
            tracing::debug!(target: "db", event = "slow_query", operation = self.operation,
                duration_ms = %elapsed.as_millis(), "SQLite read query exceeded 250 ms");
        }
    }
}
