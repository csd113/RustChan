//! Non-queueing concurrency admission for expensive request work.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Non-queueing work allowance shared across clones of one application state.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the co-located type retains the existing crate-visible middleware facade"
)]
#[derive(Clone, Debug)]
pub(crate) struct WorkGate {
    /// Owned permits remain with blocking work after request cancellation.
    semaphore: Arc<Semaphore>,
}

impl WorkGate {
    /// Create an independent gate for one resource class.
    pub(crate) fn new(permits: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Reject overload immediately instead of creating an unbounded waiter queue.
    pub(crate) fn try_begin(&self) -> crate::error::Result<OwnedSemaphorePermit> {
        Arc::clone(&self.semaphore)
            .try_acquire_owned()
            .map_err(|_error| crate::error::AppError::DbBusy)
    }
}
