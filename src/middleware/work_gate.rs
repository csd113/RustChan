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

#[cfg(test)]
mod tests {
    use super::WorkGate;

    #[test]
    fn cloned_gates_share_capacity_and_release_after_work() -> anyhow::Result<()> {
        let gate = WorkGate::new(2);
        let cloned = gate.clone();
        let first = gate.try_begin()?;
        let second = cloned.try_begin()?;
        anyhow::ensure!(matches!(
            gate.try_begin(),
            Err(crate::error::AppError::DbBusy)
        ));
        drop(first);
        let replacement = cloned.try_begin()?;
        anyhow::ensure!(gate.try_begin().is_err());
        drop(second);
        drop(replacement);
        anyhow::ensure!(gate.try_begin().is_ok());
        Ok(())
    }
    #[tokio::test]
    async fn cancelled_requests_cannot_release_running_blocking_work() -> anyhow::Result<()> {
        let gate = WorkGate::new(1);
        let permit = gate.try_begin()?;
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (finished, finished_rx) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _started = started.send(());
            let _released = release_rx.recv();
            drop(permit);
            let _finished = finished.send(());
        });
        started_rx.await?;
        task.abort();
        anyhow::ensure!(gate.try_begin().is_err());
        release.send(())?;
        finished_rx.await?;
        task.await?;
        anyhow::ensure!(gate.try_begin().is_ok());
        Ok(())
    }
}
