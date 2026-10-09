# Database architecture and operations

RustChan uses bundled SQLite through `rusqlite`, with an `r2d2_sqlite` pool.
There is no separate database service. The default database is `chan.db` in the
selected data directory. The server combines database transactions with durable
filesystem intents for media publication, deletion, restore, and recovery.

## Connections and durability

[`src/db/pool.rs`](../src/db/pool.rs) initializes every pooled connection with:

| Setting | Value |
| --- | --- |
| Journal mode | WAL |
| Synchronization | FULL |
| Foreign keys | ON |
| Page-cache target | 32,000 KiB per connection |
| Temporary storage | MEMORY |
| Memory-mapped region | 64 MiB |
| SQLite busy timeout | 1 second |

The pool defaults to eight connections. Request checkout waits at most one
second; the first usable startup connection has a separate 30-second deadline.
Spare connections fill asynchronously. Pool exhaustion and SQLite contention
produce retryable busy responses rather than unbounded waits.

WAL permits readers during a writer transaction, but SQLite still admits only
one writer. FULL synchronization strengthens commit durability subject to the
filesystem and storage device honoring sync requests. Process-crash recovery
coverage does not establish power-loss durability on every storage system.

## Schema and queries

[`src/db/schema.rs`](../src/db/schema.rs) owns the canonical tables, indexes,
relationship/domain constraints, triggers, and structural verifier. Schema
versions follow the package release version through
[`src/db/migrations.rs`](../src/db/migrations.rs).

Fresh installs create and verify the current structure. Recognized earlier
baselines receive transactional compatibility repairs and are verified before
being stamped. Unknown, partial, or corrupt schemas fail closed. Future schema
changes require a forward migration and preservation/recovery tests. Never
manually lower the version stamp or add arbitrary objects to bypass verification.
Legacy `chan_net_*` tables remain for database/backup compatibility; there is no
active ChanNet service or `--chan-net` option.

FTS5 indexes post bodies with external-content insert/update/delete triggers.
Search uses sanitized, bounded prefix expressions and a cooperative query budget.
Board pages limit selected threads before aggregating replies. Poll-voter
uniqueness, one opening post per thread, and reply counters are enforced or
verified alongside normal write invariants. Deep integrity/repair operations
are intentionally more expensive than ordinary page reads.

## Transactions and filesystem coordination

Posting releases pooled connections during independent decoding, hashing, and
preview work. A short `BEGIN IMMEDIATE` transaction rechecks current thread state,
board policy, bans/cooldown, duplicate/retry state, and bump limits before writing
posts, counters, polls, receipts, and publication intents together.

Submission receipts are retained for seven days to support retry handling;
this is not a global exactly-once posting guarantee. Durable jobs use atomic
claims and bounded retries. Expensive media work precedes the completion write.

[`src/pending_fs.rs`](../src/pending_fs.rs) and
[`src/db/fs_ops.rs`](../src/db/fs_ops.rs) coordinate staged upload promotion and
reference-checked deletion. Failure retains the intent for replay. Some final
filesystem operations deliberately remain inside a write transaction to exclude
racing references; shortening them requires an equivalent recovery protocol.

Media reconciliation inventories files outside its reference read transaction,
then revalidates repair candidates in an immediate transaction. SQLite
`data_version` observations must use the same connection; checking a newly
borrowed connection is not equivalent. Thread retention uses durable background
work and transactional archive/prune transitions. See
[archive policy](../SETUP.md#thread-archives-and-retention) for operator behavior.

## Maintenance and diagnostics

Checkpoint/optimize maintenance runs on blocking workers, defaults to hourly,
and skips busy periods. SQLite also retains its normal automatic WAL checkpoint
behavior. An active reader can prevent complete TRUNCATE; physical WAL bytes can
represent reusable high-water allocation rather than uncheckpointed live frames.
Investigate sustained growth with checkpoint state, job backlog, and pool/lock
warnings before changing policy.

The administrator's maintenance controls provide status, integrity checks,
checkpoint, vacuum, and guarded repair. Use `rustchan-cli --data-dir
/absolute/path/to/data admin db-status` for schema and SQLite status. Repair
creates a backup and verifies the result; preserve failed data for diagnosis.
Detailed readiness and metrics are opt-in and belong behind a trusted boundary.
See [observability](../SETUP.md#observability-endpoints).

## Backup and recovery

Application backups use `VACUUM INTO` to capture committed WAL state in a
standalone database snapshot. The live pooled connection is released before
packaging media and metadata. Do not copy a running `chan.db` in isolation.

Restores validate archive inventory, paths, schema, relationships, and content
before live publication. Staged swaps, recovery markers, and rollback coordinate
SQLite with files. Accepted backup formats do not imply that arbitrary historical
schemas are supported. Preserve journals and snapshots when recovery fails;
never bypass validation by deleting recovery state.

For disaster recovery or an upgrade, stop the service and back up the entire
data directory plus configured external persistent paths. Keep the matching old
executable. Rollback restores that complete stopped backup and executable
together, rather than reverse-migrating the database. See
[backup guidance](../SETUP.md#backups-and-updating) and
[software updates](software-updates.md).

## Performance investigation

The existing opt-in Rust workloads exercise production SQL and media ownership
without adding a benchmark service. Run them serially and keep their output
outside version control:

```sh
RUSTCHAN_DB_EVIDENCE=/tmp/rustchan-db-matrix.json cargo test --locked --lib db::workload::benchmark -- --ignored --exact
RUSTCHAN_DB_QUERY_EVIDENCE=/tmp/rustchan-db-queries.json cargo test --locked --lib db::workload::query_profile -- --ignored --exact
RUSTCHAN_DB_POST_EVIDENCE=/tmp/rustchan-db-media.json cargo test --locked --lib server::handlers::posting::performance::posting_connection_workload -- --ignored --exact
```

Measure the actual deployment's contention, queue growth, and resource use.
Historical debug-build measurements are not capacity guarantees. Multiple
application servers sharing transactional state would also require redesign of
filesystem coordination; replacing SQLite alone would not provide that topology.
