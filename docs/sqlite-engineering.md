# RustChan's database

RustChan stores boards, posts, accounts, and settings in a SQLite database. It is
included with the app; you do not need to install or manage a separate database
server. The default file is `chan.db` in your data directory. Uploads live in
separate files, so a database copy alone is not a complete site backup.

[All guides](../README.md#documentation) · [Backups](backups.md) · [Updates](software-updates.md)

## Start with the admin tools

Use the admin panel's maintenance controls for database status, integrity checks,
checkpointing, vacuuming, and guarded repair. Repair creates a backup and verifies
the result. Keep failed data for diagnosis.

For command-line status, use your installation's actual data path:

```sh
rustchan-cli --data-dir /absolute/path/to/data admin db-status
```

Run under the same account and configuration as the server. Do not edit the
schema version or delete SQLite/recovery files to bypass a startup error. Before
an upgrade or filesystem copy, follow [backup guidance](backups.md).
The rest of this guide explains the database for operators and contributors.

## Connections and durability

RustChan uses bundled SQLite through `rusqlite` and an `r2d2_sqlite` connection
pool. [`src/db/pool.rs`](../src/db/pool.rs) configures each connection:

| Setting | Value |
| --- | --- |
| Journal mode | WAL |
| Synchronization | FULL |
| Foreign keys | ON |
| Page-cache target | 32,000 KiB per connection |
| Temporary storage | MEMORY |
| Memory-mapped region | 64 MiB |
| SQLite busy timeout | 1 second |

WAL is SQLite's write-ahead log: committed changes can be in a file beside the
main database until a checkpoint copies them back. It allows readers while a
writer works, but SQLite still allows only one writer at a time. FULL
synchronization depends on the filesystem and device honoring sync requests;
process-crash checks do not prove power-loss durability on every storage system.

The pool defaults to eight connections. Request checkout waits at most one
second; the first usable startup connection gets a separate 30-second deadline.
Spare connections fill asynchronously. Pool exhaustion and contention produce
retryable busy responses rather than unlimited waits.

## Schema and queries

[`src/db/schema.rs`](../src/db/schema.rs) defines tables, indexes, constraints,
triggers, and structural verification. [`src/db/migrations.rs`](../src/db/migrations.rs)
tracks schemas by package release version.

Fresh installations create and verify the current structure. Recognized older
schemas receive transactional compatibility repairs and verification before the
version stamp changes. Unknown, partial, or corrupt schemas stop startup. Future
schema changes need a forward migration and preservation/recovery validation;
never lower the stamp or add arbitrary objects to bypass checks. Legacy
`chan_net_*` tables remain for compatibility; there is no active ChanNet service
or `--chan-net` option.

FTS5 indexes post bodies with external-content insert/update/delete triggers.
Search sanitizes and bounds prefix expressions and uses a cooperative query
budget. Board queries limit threads before aggregating replies. Poll-voter
uniqueness, one opening post per thread, and reply counts are enforced or verified
alongside normal write rules. Deep integrity checks and repair cost more than
ordinary reads.

## Transactions and filesystem coordination

The database and upload files must agree. RustChan records durable filesystem
intents so interrupted publication, deletion, or restore work can resume after
restart.

Posting releases pooled connections during independent decoding, hashing, and
preview work. A short `BEGIN IMMEDIATE` transaction rechecks thread state, board
policy, bans/cooldown, duplicate/retry state, and bump limits before writing the
post, counters, polls, receipts, and publication intents together. Submission
receipts last seven days to support retries; they are not a universal guarantee
that each submission is recorded exactly once.

Durable jobs use atomic claims and bounded retries. Expensive media work happens
before the completion write. [`src/pending_fs.rs`](../src/pending_fs.rs) and
[`src/db/fs_ops.rs`](../src/db/fs_ops.rs) coordinate staged uploads and deletion
with reference checks. Failures retain intents for replay. Some final filesystem
operations intentionally stay inside a write transaction to prevent racing
references; changing that requires equivalent recovery protection.

Media reconciliation inventories files outside its reference-read transaction,
then rechecks repair candidates in an immediate transaction. SQLite `data_version`
comparisons must use the same connection; a newly borrowed connection is not
equivalent. Retention also uses durable jobs and transactional archive/prune
changes. See [retention policy](administration.md#archives-and-retention).

## Maintenance and diagnostics

Checkpoint/optimize work runs on blocking workers, defaults to hourly, and skips
busy periods. SQLite's automatic WAL checkpointing also remains enabled. An active
reader can prevent full TRUNCATE, so a large WAL file can contain reusable space
rather than that many uncheckpointed changes. Investigate sustained growth using
checkpoint state, job backlogs, and pool/lock warnings before changing policy.

Detailed readiness and metrics are opt-in. Keep them behind a trusted monitoring
boundary; see [health and monitoring](deployment.md#health-and-monitoring).

## Backup and recovery

Application backups use `VACUUM INTO` to capture committed WAL data in a standalone
database snapshot. They release the live pooled connection before packaging
media and metadata. Never copy a running `chan.db` in isolation.

Restores check archive inventory, paths, schemas, relationships, and content
before replacing live data. Staged swaps, recovery records, and rollback keep
SQLite and files together. Accepting an archive format does not mean every
historical database schema is supported. Preserve journals and snapshots after
failure; see [interrupted restore recovery](backups.md#interrupted-restore-recovery).

For disaster recovery or an upgrade, stop the service and copy the whole data
directory plus external persistent paths. Keep the matching executable. Rollback
restores that complete stopped copy and executable together, rather than reversing
a migration. Follow [software updates](software-updates.md).

## Performance investigation

Measure contention, queue growth, and resource use in the actual deployment.
Historical debug-build measurements are not capacity promises. Running several
application servers against shared state would also need new filesystem
coordination; replacing SQLite alone would not make that setup work.
