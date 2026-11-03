//! Retention selects only independently verified full-site recovery points.

use crate::models::BackupInfo;

/// Keep the requested newest usable full-site backups. Other scopes and damaged
/// evidence neither displace a full backup nor become automatic deletion targets.
pub(super) fn removals(
    backups: Vec<BackupInfo>,
    keep: usize,
    mut verify: impl FnMut(&BackupInfo) -> bool,
) -> Vec<BackupInfo> {
    backups
        .into_iter()
        .filter(|backup| backup.scope == "Full site" && backup.verified)
        .filter(|backup| verify(backup))
        .skip(keep.max(1))
        .collect()
}
