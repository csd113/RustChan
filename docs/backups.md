# Backup and restore safety

Saved full-site backups are published as verified only after checking their
payload checksums, database baseline, restored post bodies, and referenced media.
Automatic retention counts usable full-site backups independently of board
exports and leaves incomplete or damaged evidence available for inspection.
Pre-maintenance snapshots remain evidence of the original database, including
damage that the repair operation is intended to fix.

Backup and restore drain normal requests, background maintenance, and media jobs
before accessing live state exclusively. Normal requests receive HTTP 503 during
that exclusive phase. The drain has a two-minute deadline; an operation fails
without changing live data if existing work cannot finish in time. Uploads are
received before this exclusive phase. The authenticated backup progress endpoint
remains available. Blocking work retains its access if the client disconnects.

Local archive uploads are limited to 8 GiB of uploaded data and 8 GiB of extracted
payload. Transfer conversion enforces the same cumulative extraction budget.
Legacy ZIP downloads must fit this restore budget. Individual board exports are limited to a 64 MiB JSON manifest; larger boards can still be recovered through a full-site restore. For larger sites, use a saved
directory or split-ZIP backup and **Restore saved backup**: its extraction budget
comes from the verified inventory, rather than the local-upload limit. Restore
also needs free space for the converted archive, staged files, and live database
safety snapshot; an I/O failure aborts without publishing an incomplete backup.

## Interrupted restore recovery

Before replacing live state, restore syncs its staging files and a private
database safety snapshot, then writes `runtime/restore-recovery.json` under the
data directory. The record identifies the pending filesystem operation and the
safety snapshot. Filesystem swaps persist their directory renames before
discarding displaced files.

If database rollback or filesystem completion fails, RustChan retains recovery
evidence and closes live-state admission. Restart attempts the journaled swap
before starting requests or media workers. If recovery succeeds, startup verifies
the database before clearing the record. If the record has no matching pending
restore, or reconciliation fails, startup stops and reports the retained snapshot
location for manual recovery. Successful startup recovery retains that snapshot
for inspection.

For manual recovery, stop RustChan and preserve the complete data directory,
the recovery record, the indicated database snapshot and any sidecars, and the
staged/displaced directories named in `pending_fs_ops`. Reconcile the database
and its matching media together before clearing the recovery record. Removing
the record alone does not complete a restore.

## Focused regression checks

`cargo test --test backup_regressions --all-features` checks cancellation-safe
draining, failed-recovery admission, streaming budgets, retained rollback files,
retention selection, staging cleanup, and interrupted swap replay using small
temporary fixtures. These checks are wired as a dedicated integration target;
the previously removed application suites remain disabled.
