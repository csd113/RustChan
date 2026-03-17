// chan_net/ledger.rs — Transaction deduplication ledger for federation imports.
//
// TxLedger tracks UUID transaction IDs from imported snapshots so that the
// same snapshot is never applied twice within a server session.
//
// Step 1.3

use std::collections::HashSet;
use uuid::Uuid;

// TODO: TxLedger is in-memory only. A server restart clears seen tx_ids,
// allowing a re-import of the same snapshot. DB persistence is a future extension.
// The unique DB index on chan_net_posts provides a DB-level deduplication
// safety net regardless of ledger state.
pub struct TxLedger {
    seen: HashSet<Uuid>,
}

impl TxLedger {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
        }
    }

    pub fn contains(&self, id: &Uuid) -> bool {
        self.seen.contains(id)
    }

    pub fn insert(&mut self, id: Uuid) {
        self.seen.insert(id);
    }
}
