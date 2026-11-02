#![expect(
    clippy::redundant_pub_crate,
    reason = "access coordination is intentionally crate-scoped behind the middleware module"
)]
//! Drainable access to the live database and its associated files.

use std::sync::atomic::{AtomicBool, Ordering};
use std::{future::Future, sync::Arc};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// Shared ownership keeps detached blocking work inside the drain boundary.
pub(crate) type AccessLease = Arc<OwnedRwLockReadGuard<()>>;

tokio::task_local! {
    /// Access inherited by blocking work and explicitly spawned request jobs.
    static ACCESS: AccessLease;
}

/// Admission for live-state users and exclusive backup/recovery operations.
#[derive(Clone, Debug, Default)]
pub(crate) struct DataAccess {
    lock: Arc<RwLock<()>>,
    blocked: Arc<AtomicBool>,
}

impl DataAccess {
    /// Admit work immediately, rejecting maintenance and failed recovery.
    pub(crate) fn try_read(&self) -> Option<AccessLease> {
        if self.is_blocked() {
            return None;
        }
        let lease = Arc::new(Arc::clone(&self.lock).try_read_owned().ok()?);
        (!self.is_blocked()).then_some(lease)
    }

    /// Close admission and drain existing work, with a bounded wait.
    pub(crate) async fn exclusive(&self) -> Option<OwnedRwLockWriteGuard<()>> {
        if self.is_blocked() {
            return None;
        }
        let lease = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            Arc::clone(&self.lock).write_owned(),
        )
        .await
        .ok()?;
        (!self.is_blocked()).then_some(lease)
    }

    /// Require successful startup recovery before admitting further work.
    pub(crate) fn block(&self) {
        self.blocked.store(true, Ordering::Release);
    }

    /// Whether manual or startup recovery is required.
    pub(crate) fn is_blocked(&self) -> bool {
        self.blocked.load(Ordering::Acquire)
    }
}

/// Propagate access through the asynchronous request or worker operation.
pub(crate) async fn scope<T>(lease: AccessLease, work: impl Future<Output = T>) -> T {
    ACCESS.scope(lease, work).await
}

/// Retain the caller's access even if its awaiting future is cancelled.
pub(crate) fn spawn_blocking<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let lease = ACCESS.try_with(Arc::clone).ok();
    tokio::task::spawn_blocking(move || match lease {
        Some(lease) => ACCESS.sync_scope(lease, work),
        None => work(),
    })
}

/// Preserve request access in deliberately detached administrative jobs.
pub(crate) fn spawn<F>(work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let lease = ACCESS.try_with(Arc::clone).ok();
    tokio::spawn(async move {
        match lease {
            Some(lease) => scope(lease, work).await,
            None => work.await,
        }
    })
}
