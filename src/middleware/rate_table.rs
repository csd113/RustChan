//! Fixed-window budgets with bounded retention and atomic admission.

use super::RateTable;
use std::collections::HashMap;

/// Maximum retained identities in each abuse-control table.
const MAX_RATE_IDENTITIES: usize = 16_384;
/// Minimum interval between whole-table expiration scans.
const CLEANUP_INTERVAL_SECS: u64 = 60;

#[derive(Debug)]
/// One visitor/action counter and its exclusive expiration time.
struct Counter {
    /// Attempts admitted or rejected during this window.
    count: u32,
    /// First second outside the historical inclusive window.
    expires_at: u64,
}

#[derive(Debug, Default)]
/// Counters and cleanup scheduling protected by one short-lived lock.
pub(super) struct TableState {
    /// Fixed-size, hashed client/action keys; never request paths or cookies.
    entries: HashMap<String, Counter>,
    /// Last expiration scan, including scans of a saturated table.
    last_cleanup: Option<u64>,
}

impl RateTable {
    /// Reserve one attempt before work; saturation rejects new identities.
    pub(crate) fn record(&self, key: &str, now: u64, window: u64) -> u32 {
        let mut state = self.state.lock();
        Self::prune_state(&mut state, now);
        if !state.entries.contains_key(key) && state.entries.len() >= MAX_RATE_IDENTITIES {
            return u32::MAX;
        }
        let counter = state
            .entries
            .entry(key.to_owned())
            .or_insert_with(|| Counter {
                count: 0,
                expires_at: now.saturating_add(window).saturating_add(1),
            });
        if now >= counter.expires_at {
            counter.count = 0;
            counter.expires_at = now.saturating_add(window).saturating_add(1);
        }
        counter.count = counter.count.saturating_add(1);
        let count = counter.count;
        drop(state);
        count
    }

    /// Return a retry delay for exhausted budgets or a saturated identity table.
    pub(crate) fn retry_after(&self, key: &str, now: u64, limit: u32) -> Option<u64> {
        let mut state = self.state.lock();
        Self::prune_state(&mut state, now);
        match state.entries.get(key) {
            Some(counter) if now < counter.expires_at && counter.count >= limit => {
                Some(counter.expires_at.saturating_sub(now).max(1))
            }
            None if state.entries.len() >= MAX_RATE_IDENTITIES => Some(CLEANUP_INTERVAL_SECS),
            Some(_) | None => None,
        }
    }

    /// Clear an authenticated visitor's failure counter.
    pub(crate) fn remove(&self, key: &str) {
        let _removed = self.state.lock().entries.remove(key);
    }

    /// Periodically expire counters even when request activity stops.
    pub(crate) fn prune(&self, now: u64) {
        Self::prune_state(&mut self.state.lock(), now);
    }

    /// Scan at most once a minute; saturation never causes a scan on every request.
    fn prune_state(state: &mut TableState, now: u64) {
        if state
            .last_cleanup
            .is_some_and(|last| now.saturating_sub(last) < CLEANUP_INTERVAL_SECS)
        {
            return;
        }
        state.entries.retain(|_, counter| now < counter.expires_at);
        state.last_cleanup = Some(now);
    }

    #[cfg(test)]
    /// Install a deterministic failure counter without performing password work.
    pub(crate) fn insert(&self, key: String, value: (u32, u64), window: u64) {
        let _previous = self.state.lock().entries.insert(
            key,
            Counter {
                count: value.0,
                expires_at: value.1.saturating_add(window).saturating_add(1),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{RateTable, MAX_RATE_IDENTITIES};

    #[test]
    fn exact_window_boundary_and_saturation_are_bounded() {
        let table = RateTable::default();
        assert_eq!(table.record("visitor", 100, 60), 1);
        assert_eq!(table.record("visitor", 160, 60), 2);
        assert_eq!(table.retry_after("visitor", 160, 2), Some(1));
        assert_eq!(table.record("visitor", 161, 60), 1);
        for index in 1..MAX_RATE_IDENTITIES {
            assert_eq!(table.record(&index.to_string(), 161, 60), 1);
        }
        assert_eq!(table.record("overflow", 161, 60), u32::MAX);
        assert!(table.retry_after("overflow", 161, 2).is_some());
        assert_eq!(table.state.lock().entries.len(), MAX_RATE_IDENTITIES);
        assert_eq!(table.record("overflow", 222, 60), 1);
        assert_eq!(table.state.lock().entries.len(), 1);
    }

    #[test]
    fn concurrent_admission_never_exceeds_the_budget() -> anyhow::Result<()> {
        let table = RateTable::default();
        let admitted = std::thread::scope(|scope| {
            let handles = (0usize..64)
                .map(|_| {
                    let table = &table;
                    scope.spawn(move || table.record("visitor", 100, 60) <= 20)
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_error| anyhow::anyhow!("budget worker panicked"))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        })?;
        anyhow::ensure!(admitted.into_iter().filter(|admitted| *admitted).count() == 20);
        Ok(())
    }
}
