// chan_net/ledger.rs — Transaction deduplication ledger for federation imports.
//
// TxLedger tracks UUID transaction IDs from imported snapshots so that the
// same snapshot is never applied twice within a server session.

use std::collections::HashSet;
use uuid::Uuid;

#[derive(Default)]
pub struct TxLedger {
    seen: HashSet<Uuid>,
}

impl TxLedger {
    pub fn contains(&self, id: &Uuid) -> bool {
        self.seen.contains(id)
    }

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
