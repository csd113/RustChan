//! Non-queueing concurrency admission for expensive request work.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::WorkGate;

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
