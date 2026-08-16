//! Transaction deduplication ledger for federation imports.
//
// TxLedger tracks UUID transaction IDs from imported snapshots so that the
// same snapshot is never applied twice within a server session.

use std::collections::HashSet;
use uuid::Uuid;

/// In-memory set of federation transaction IDs accepted this process lifetime.
#[derive(Debug, Default)]
pub struct TxLedger {
    /// Transaction IDs already applied successfully.
    seen: HashSet<Uuid>,
}

impl TxLedger {
    /// Returns whether a transaction ID was already applied.
    #[must_use]
    pub fn contains(&self, id: &Uuid) -> bool {
        self.seen.contains(id)
    }

    /// Records a transaction ID after its database write succeeds.
    pub fn insert(&mut self, id: Uuid) {
        self.seen.insert(id);
    }
}

impl FromIterator<Uuid> for TxLedger {
    fn from_iter<T: IntoIterator<Item = Uuid>>(iter: T) -> Self {
        Self {
            seen: iter.into_iter().collect(),
        }
    }
}
