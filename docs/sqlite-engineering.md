# SQLite engineering pass

## Baseline audit (2026-10-02, before production changes)

The starting tree was clean. Rust 1.99.0, rusqlite 0.39.0 (bundled SQLite,
backup support), r2d2 0.8.10 and r2d2_sqlite 0.34.0 are authoritative. No
backend replacement, extra service, new dependencies, or commits were introduced.

### Ownership and atomicity map

| Area | Baseline implementation | Ownership / correctness boundary |
| --- | --- | --- |
| Startup | `db/pool.rs`, `db/schema.rs`, `db/migrations.rs` | Every connection gets WAL, NORMAL, foreign keys, -32000 KiB cache, MEMORY temp store, 64 MiB mmap and 1000 ms busy timeout. Configurable pool defaults to 8; checkout timeout is 1 second. Schema installation/repair is transactional and structural verification rejects unexpected objects. |
| Thread / reply HTTP | `handlers/board/create_thread.rs`, `handlers/thread.rs`, `handlers/posting.rs` | Blocking workers hold a media semaphore through submission. A connection is checked out before submission and held through identity/body preparation, media probing/hash/thumbnail generation, DB commit, upload promotion, job enqueueing and configured pruning. |
| Thread / reply writes | `db/threads.rs` | BEGIN IMMEDIATE serializes token replay, dedup revalidation, thread/OP or reply/count updates, optional poll and filesystem intent. Reply locked/archived state is checked again under the write lock. Commit failures roll back before return. |
| Upload / dedup | `handlers/mod.rs`, `pending_fs.rs`, `db/fs_ops.rs` | Staged uploads get content hashes and a durable promotion record in the post transaction. Dedup reuse is board-scoped and checked again against current DB references. Promotion/replay retains the intent on failure. |
| Polls / moderation | `db/posts.rs`, `db/admin.rs`, `db/accounts.rs`, `db/boards.rs` | Unique poll voter and one-OP indexes plus schema domain/relationship triggers protect persisted invariants. Edit/delete/account mutations use immediate transactions when read-validation-write must serialize. |
| Jobs | `workers/mod.rs`, `db/posts.rs` | Atomic UPDATE RETURNING claim; bounded attempts; durable media state transitions, retry and interruption recovery. Heavy media prepare/render runs outside final completion transactions. Persistence retries handle pool/busy failures. |
| Pruning / filesystem deletion | `media/prune.rs`, `db/threads.rs`, `pending_fs.rs` | Short intent creation followed by revalidated finalization. Some finalization transactions intentionally include filesystem checks/deletion to exclude racing reference creation; removing these without a replacement protocol is unsafe. |
| Reconciliation | `media/reconcile.rs` | Consistent DB reference snapshot; filesystem inventory outside its read transaction, but the same connection stays borrowed. `data_version` comparisons are connection-local; checking on a newly borrowed connection is not equivalent. Repairs revalidate each candidate inside an immediate transaction. |
| Backups | `handlers/admin/backup/create.rs`, `downloads.rs` | VACUUM INTO includes committed WAL state. Full backup keeps its live pooled connection through file copies, hashing and manifest creation until late in the operation. Maintenance backup also retains it through packaging. |
| Restores | `handlers/admin/backup/restore_full.rs`, `restore_board.rs` | Preflight, snapshot, staged swaps, durable recovery markers and rollback preserve DB/filesystem coordination. Board restore holds a write transaction through its coordinated restore; restructure only with specific recovery evidence. |
| Updates | `updates/transaction.rs`, `updates/snapshot.rs` | Stopped-service VACUUM INTO and verified persistent-state inventory; transaction journal and interrupted-update recovery. No live pool is needed by the isolated updater. |
| FTS / repair | `db/schema.rs`, `db/admin.rs` | FTS5 external-content table and insert/update/delete triggers; explicit rebuild during repair. Backup precedes repair. Structural verification plus quick/integrity and foreign-key checks protect startup/restore. |
| WAL | `db/admin.rs`, maintenance handlers | SQLite default automatic checkpoint (1000 pages); operator TRUNCATE checkpoint reports busy/log/checkpointed. A recurring TRUNCATE checkpoint plus PRAGMA optimize runs hourly by default, staggered and skipped while requests/uploads/maintenance are busy; these two statements originally ran on an async worker. |

### Findings, ranked

1. **Avoidable pool occupancy during media preparation.** A scarce pooled
   connection remains owned during independent expensive work. The dedup query
   is the only DB access required by the primary-upload processor.
2. **Posting state can become stale.** Board access preflight occurs before
   multipart parsing; final DB writes do not recheck board access, bans or
   cooldown. The bump decision uses the pre-processing reply count and board
   bump limit, permitting an extra bump after concurrent replies cross the
   limit. Locked/archived reply state is already correctly revalidated.
3. **Board listing aggregates before pagination.** The thread SELECT joins all
   replies, groups all matching board threads, then orders and limits. Even a
   ten-thread page visits replies of threads outside that page. Profile before
   changing it; retain exact preview/count/latest-post semantics.
4. **Backups retain live connections during packaging.** Snapshot-based metadata
   extraction can potentially release the live pool earlier and also make
   metadata correspond to the actual snapshot.
5. **Limited contention attribution.** Pool/busy errors map to retryable 503
   but there is little timing evidence distinguishing checkout, hold time,
   write-lock acquisition and transaction work.
6. **Reconciliation lifetime is deliberate.** Its generation check requires
   the same SQLite connection. Do not shorten ownership by simply reacquiring
   and comparing `data_version` values from different connections.

Existing hot reads and job claiming already use cached prepared statements;
preview posts and thread-file collection already batch work. File-reference
OR queries have partial indexes on all three paths. Schema repair already
removes two historical duplicate indexes. Extra indexes will require populated
query-plan/timing evidence and the existing additive migration path.

### Durability interpretation

WAL + NORMAL preserves consistency and recovery after application crashes;
some acknowledged transactions may be lost after power loss or OS failure.
FULL adds a WAL sync per commit for stronger commit durability when the storage
stack honors sync. Neither setting fixes faulty storage or application-level
filesystem coordination. Actual power-loss behavior cannot be established by
killing an application process. macOS fullfsync is a distinct setting, so a
local FULL timing alone must not be portrayed as a universal hardware guarantee.
See [SQLite synchronous](https://sqlite.org/pragma.html#pragma_synchronous) and
[WAL](https://sqlite.org/wal.html). Pool/cache/durability defaults remain unchanged
until representative measurements support a change.

## 1. Baseline architecture

The audit above records the original connection, transaction, filesystem and
recovery boundaries. SQLite remains bundled and embedded; no new dependencies,
services, unsafe code, database abstraction or backend were introduced. The
pool remains configurable through settings and `CHAN_DB_POOL_SIZE`.

## 2. Problems found

The highest-impact measured issue was repeated FTS **prefix** lookups per board
post: a common-term count took 11.2 seconds on the populated fixture. Uploads
held connections through independent processing and nested spam-job checkouts
could exhaust a two-connection pool. Board pages aggregated unrequested replies.
Posting cooldown, access, ban and bump decisions could become stale before the
write lock. Backups held live connections through packaging. Recurring session
purges and checkpoint/optimize calls ran directly on async workers; checkpoint
logs mislabeled SQLite's `busy` flag as pages backfilled.

## 3. Changes implemented

- Posting borrows for initial reads, dedup lookup, atomic insertion, promotion
  metadata and job persistence independently. Media probing, hashes, encoding,
  staged-file promotion and candidate filesystem scans release the pool.
- Transactional revalidation checks current board identity/access, admin session,
  bans, cooldown, CAPTCHA/media/body policy and word filters. Changed preparation
  policy returns a recoverable conflict; locked/archived threads remain rejected.
- Bump decisions use the current reply count and current board limit under the
  immediate write lock, including simultaneous replies at the boundary.
- Thread pages materialize the ordered page before indexed per-thread aggregates.
  Missing OPs are excluded before pagination; preview, image count and latest-post
  semantics are covered against the original query, including timestamp ties
  (active IDs descend, archived IDs ascend).
- FTS drives the prefix-result/count joins; cached statements retain board scope,
  sanitization, ordering and pagination. No adaptive strategy remains.
- Two measured indexes use the existing transactional additive repair path:
  `(thread_id,id)` for polling and `(board_id,ip_hash,created_at DESC)` for cooldown.
- Full and maintenance backup packaging reads the exact read-only VACUUM snapshot
  after releasing the live connection. Authorization is rechecked before snapshot.
- Recurring checkpoint/optimize and session cleanup use blocking workers; checkpoint
  logs now report `log_pages`, `checkpointed_pages` and `busy` accurately.
- Thresholded pool/SQL/transaction tracing, pool/file-size metrics and opt-in
  offline workload, actual-upload and crash tests provide reproducible evidence.

## 4. Connection lifetime

| Phase | Before | After |
| --- | --- | --- |
| Initial validation | One submission-wide checkout | Scoped reads; release before independent preparation |
| Hash / decode / thumbnail | Live connection held | No connection; one scoped dedup lookup |
| Post commit | Same connection | Late checkout; revalidation and atomic commit; immediate release |
| Upload promotion / replay | Same connection through moves / verification | Filesystem work first; short metadata transaction only |
| Job persistence | Live checkout plus queue's nested checkout | Independent media-job scope; release before spam queue checkout |
| Media pruning | Connection held while inventory paths are checked | Collect rows; release; validate paths; borrow per mutation boundary |
| Full backup | Live checkout through packaging | Live VACUUM checkout only; standalone snapshot connection during packaging |

The one-connection regression acquires the sole connection inside independent
preparation and successfully schedules work after submission. Existing recovery
and deletion protocols retain filesystem I/O inside transactions where a racing
new reference must be excluded. Reconciliation deliberately retains one connection
for `data_version`: comparisons across different connections are invalid.

## 5. Transaction boundaries

Posting retains BEGIN IMMEDIATE because token replay, policy checks, dedup paths,
thread flags, counters, bump limits, OP/poll rows and durable filesystem/prune
intents require one serialized decision. Idempotent replay precedes revalidation;
preparation from a losing request is cleaned without creating another post. No
media command/encoding runs inside these transactions. Existing commit rollback
handling is preserved, and typed validation errors survive the boundary.

Upload finalization still commits hash metadata, optional-thumbnail repair and
intent removal together after promotion. Original-file pruning and deletion keep
their race-exclusion transaction. Restores/updater recovery protocols were audited
and retained. VACUUM INTO necessarily occupies its connection while SQLite makes
the consistent snapshot; independent packaging no longer occupies the live pool.

## 6. PRAGMA decisions

The production default changes from NORMAL to **FULL**. Across three optimized
runs, the pool-eight throughput medians show roughly 2%/6%/12.5% cost for
read-heavy/burst/mixed work. Actual media posting remains dominated by processing
CPU. This is a reasonable cost for stronger acknowledged-commit durability when
storage honors sync; it does not improve every failure mode or replace upload
recovery. The connection pool remains eight, configurable as before.

| Setting | Value | Rationale |
| --- | --- | --- |
| journal_mode | WAL | Readers can proceed alongside one writer; committed WAL state is included in snapshots. |
| synchronous | FULL, every pooled connection | WAL sync at commit; modest measured cost. Process-crash recovery does not establish power-loss guarantees. |
| foreign_keys | ON, every pooled connection | Relationship constraints are connection-local and must remain enforced. |
| cache_size | -32000 | 32,000 KiB target per connection; allocated on demand, roughly 250 MiB across eight fully populated caches. No evidence supports enlarging it. |
| temp_store | MEMORY | Existing bounded page/sort workloads; removed large pre-page aggregates rather than enlarging memory settings. |
| mmap_size | 67108864 | Existing 64 MiB mapping ceiling; virtual/shared mapping is distinct from private per-connection cache. |
| busy_timeout | 1000 ms | Bounded write-lock wait; overload remains retryable rather than hanging. |
| wal_autocheckpoint | SQLite default 1000 pages | Retained alongside existing idle scheduled/operator checkpoints. |

Pool checkout timeout remains one second. Pool warnings begin at 250 ms and are
rate-limited independently per category to one per 30 seconds per pool. Detailed
SQL, lock-wait and posting transaction timings require `db=debug`, use fixed
operation labels, and contain no SQL parameters or private request content.
Metrics add pool total/idle connections and physical DB/WAL bytes; `-1` means
unavailable. File-size sampling uses async filesystem APIs, and existing metrics
access control and deep schema-health semantics are unchanged.

## 7. Pool-size findings

Eight remains the default. Four sometimes achieves higher throughput and lower
p99, but eight greatly reduces checkout waiting and provides useful headroom for
background jobs. Twelve and sixteen do not consistently improve throughput or
latency; extra caches and competing writers make enlargement unjustified.

Median operations/second across three optimized runs, with eight workers:

| Pool | NORMAL read / burst / mixed | FULL read / burst / mixed |
| ---: | ---: | ---: |
| 2 | 3545 / 1861 / 2757 | 3598 / 1734 / 2734 |
| 4 | 5567 / 1900 / 3274 | 5699 / 1782 / 3018 |
| 8 | 4953 / 1914 / 3458 | 4854 / 1794 / 3027 |
| 12 | 5209 / 1878 / 3344 | 5092 / 1816 / 3030 |
| 16 | 4956 / 1894 / 3486 | 5064 / 1827 / 3171 |

In the third run FULL-eight checkout p95 was 3–5 microseconds versus roughly
3–8 milliseconds at pools two/four; 0–1 of 3200 checkouts exceeded 100 microseconds
at eight. This is a measurable-wait proxy, not a claim that every shorter
checkout had no wait. The first FULL-eight read run was a 2496 ops/s outlier;
repeats reached 4854 and 5546. Raw runs retain that variation.

## 8. Query-plan/index findings

SQLite 3.51.3, eight boards, 800 threads and 40,000 posts; every query is prepared
against the populated real schema. Twenty-five cached repetitions, nearest-rank
percentiles, with result-ID/order equivalence checks. Times below are medians.

| Production query | Before | After | Evidence |
| --- | ---: | ---: | --- |
| Board page, 10 threads | 5.372 ms | 0.144 ms | MATERIALIZE page; indexed OP lookup; bounded reply aggregates; removes temp GROUP BY and DISTINCT |
| Rare prefix search, full posts | 74.174 ms | 0.434 ms | One FTS scan, primary-key post lookups |
| Common prefix search, full posts | 56.798 ms | 13.300 ms | Preserved order/limit; eliminates repeated prefix evaluation |
| Missing prefix search, full posts | 38.956 ms | 0.076 ms | Empty FTS result exits before board scan |
| Rare prefix count | 157.500 ms | 0.258 ms | FTS-driven count |
| Common prefix count | 11241.770 ms | 8.623 ms | FTS-driven count |
| Missing prefix count | 38.762 ms | 0.010 ms | FTS-driven count |
| Live polling | 2.749 ms | 0.005 ms | thread/id range index; no order sort |
| Cooldown lookup | 0.154 ms | 0.002 ms | Covering board/IP/timestamp index |

Exact-term exploratory measurements initially showed a limited-common-term
regression for an FTS-first join. RustChan actually uses sanitized quoted prefix
terms. The final acceptance comparison uses that **actual syntax** and complete
post projections; all three term classes improve. The rejected adaptive probe is
absent from production. CROSS JOIN explicitly preserves the measured join order;
see [SQLite's optimizer guidance](https://sqlite.org/optoverview.html#manual_control_of_query_plans_using_cross_join).

Two indexes added 1,302,528 bytes (~10.7%) after compacting the dropped-index
fixture: 12,210,176 → 13,512,704 bytes. Isolated FULL write median/p95 changed
384/510 → 404/543 microseconds (~5%/~6.5%); the mixed/burst matrix is the
acceptance gate. Existing
thread/timestamp, cross-board IP and path indexes remain necessary. Historical
redundant indexes were already removed by repair; no speculative new latest-thread,
admin or dedup index was added. Reports, bans, jobs, path OR checks, file hashes
and admin logs already use useful indexes. Full backup/integrity inventories
necessarily scan their complete snapshot/data; metrics/admin deep checks remain
intentional expensive diagnostics.

## 9. WAL/checkpoint findings

The baseline sustained matrix retained WAL files up to ~174 MB while all final
PASSIVE checkpoints reported `busy=0` and backfilled all current frames. Physical
WAL bytes are a reusable high-water allocation, not a count of currently live
frames. The third optimized run retained at most 14.3 MB of physical WAL and completed
PASSIVE checkpoints in at most 3.263 ms, all with busy=0 and complete backfill.
The final acceptance run reached at most 15.80 MB WAL,
5.235 ms checkpoint latency and 123.1 MiB case-end RSS;
all final checkpoints completed with busy=0. No evidence supports another
checkpoint manager or changing the existing policy.

A held read snapshot deterministically prevents complete TRUNCATE (`busy=1`,
checkpointed < log); a concurrent VACUUM snapshot still includes later committed
boards. Releasing the reader permits complete TRUNCATE. Normal WAL readers remain
usable during an uncommitted writer; another writer fails within the existing
busy bound and maps to retryable overload. No new checkpoint manager was added.

## 10. Benchmark results

The SQL matrix uses eight concurrent workers, 400 operations each, all pools
2/4/8/12/16, NORMAL/FULL and 5%/70%/25% write mixes: 96,000 operations per matrix.
Reads use production board/catalog/thread/polling/FTS/moderation/poll/dedup helpers.
Writes include replies, counters, token records, FTS triggers, poll votes, durable
job enqueue/claim/complete and job cleanup. Actual PNG submissions separately
exercise new-thread, filesystem-intent/promotion and job paths with two producers
and four readers. No network service, FFmpeg or new dependency is required.

The tables below compare the original NORMAL/eight default with the final
FULL/eight default, thus including the durability cost. Total latency includes
checkout and, for writes, lock acquisition. Failed operations count in latency
but not successful throughput. The final acceptance matrix also verifies actual
non-OP row counts against every persisted thread reply counter.

| Workload / default | Ops/s | Total p50 / p95 / p99, ms | Checkout p95, µs | Write wait p95, ms | Write hold p95, ms | Busy / timeout | RSS, MiB | WAL, MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| read_heavy Before | 78 | 3.915 / 694.506 / 950.879 | 11 | 0.189 | 4.900 | 0 / 0 | 102.8 | 12.16 |
| read_heavy After | 5501 | 0.840 / 3.618 / 6.471 | 3 | 11.995 | 3.051 | 0 / 0 | 89.3 | 13.43 |
| write_burst Before | 224 | 2.070 / 57.696 / 807.621 | 5 | 4.654 | 3.199 | 0 / 0 | 90.5 | 165.82 |
| write_burst After | 1747 | 0.482 / 3.080 / 67.702 | 3 | 4.551 | 0.757 | 0 / 0 | 112.3 | 4.67 |
| mixed Before | 96 | 2.913 / 654.620 / 934.125 | 14 | 1.567 | 3.706 | 0 / 0 | 97.2 | 59.10 |
| mixed After | 3065 | 0.517 / 2.624 / 27.526 | 3 | 20.170 | 1.169 | 0 / 0 | 78.1 | 7.30 |

| Final FULL-eight workload | Read p50 / p95 / p99, ms | Write p50 / p95 / p99, ms | CPU, seconds | Checkpoint, ms | Checkpoint busy / log / backfilled |
| --- | ---: | ---: | ---: | ---: | --- |
| read_heavy | 0.821 / 3.438 / 4.272 | 2.501 / 12.846 / 50.634 | 3.50 | 0.498 | 0 / 43 / 43 |
| write_burst | 0.423 / 1.680 / 1.964 | 0.488 / 5.043 / 108.172 | 1.70 | 2.699 | 0 / 763 / 763 |
| mixed | 0.419 / 1.735 / 1.969 | 0.634 / 20.784 / 95.912 | 1.92 | 2.054 | 0 / 695 / 695 |

All 96,000 final acceptance operations succeeded across 30 cases: zero busy
errors and zero pool timeouts. Every case passed integrity, foreign keys, FTS,
schema and reply-counter checks. Across all four optimized matrices there was
one bounded busy error in 384,000 operations (the third run's experimental
NORMAL-eight burst); all FULL cases were error-free. The original 96,000-operation
matrix also had zero busy errors/timeouts. This supports the chosen default,
without claiming that experimental configurations never encounter saturation.

Write-lock **hold** p95 improves in all three default comparisons. Acquisition
wait grows in the much faster read-heavy/mixed workloads because more writes compete
per second; it is separately reported rather than hidden in transaction time.
The longest waits remain governed by the existing one-second busy timeout plus
scheduling overhead. Shortening holds cannot guarantee writer fairness.
Case-end maximum RSS increased
from 102.8 to 123.1 MiB across the full matrix (~20 MiB), with two additional
indexes and more useful cached data. Observed RSS stayed within that range;
these case-end samples do not establish a peak-memory ceiling.


Actual upload ownership evidence (milliseconds, 8 PNG posts per case):

| Pool | Before hold p95 / max | After FULL hold p95 / max | Reader checkout p95, before → FULL after | Pool timeouts, before → after |
| ---: | ---: | ---: | ---: | ---: |
| 2 | 1382.262 / 1382.262 | 1.626 / 4.428 | 374.829 → 0.023 | 20 → 0 |
| 4 | 357.447 / 357.447 | 1.991 / 5.394 | 0.058 → 0.005 | 0 → 0 |
| 8 | 695.155 / 695.155 | 1.670 / 2.451 | 0.047 → 0.005 | 0 → 0 |

All upload cases created exactly eight posts, finalized every durable upload
intent, and had no pending filesystem records. The eight FULL upload posting
p50/p95 values are reported in raw evidence; processing dominates that timing.

Raw evidence: [original matrix](sqlite-evidence/baseline.json),
[first optimized matrix](sqlite-evidence/optimized.json),
[second optimized matrix](sqlite-evidence/optimized-repeat.json),
[third optimized matrix](sqlite-evidence/final-matrix.json),
[final acceptance matrix](sqlite-evidence/acceptance-matrix.json),
[final query/index profile](sqlite-evidence/query-profile.json),
[exploratory index profile](sqlite-evidence/index-profile.json),
[original media](sqlite-evidence/media-baseline.json),
[final media](sqlite-evidence/media-optimized.json). The exploratory index profile's
exact-term FTS numbers do not represent production prefix syntax. CPU endpoints
in the first two optimized matrices include verification; use the last two
matrices for workload CPU comparisons.
The SQL matrix excludes HTTP and media CPU. PNG generation precedes timing;
production media processing is included. RSS is sampled at case end, not peak;
CPU time uses optional host `ps` and is available in the final optimized runs only;
timer endpoints exclude verification.
These are debug-build, shared-host measurements, not hardware-independent capacity
promises. Short media runs have only eight posts per case, so pool hold times
and timeout behavior carry more weight than their noisy posting percentiles.

## 11. Durability/recovery results

NORMAL and FULL both pass abrupt writer-process termination with 100 committed
replies plus an interrupted open transaction. Restart checks exact reply count,
integrity, foreign keys, synchronized FTS, autocommit/write-lock availability,
interrupted-job recovery and durable filesystem-intent replay. This verifies
application-process crash recovery, not storage power-cut durability.

The full suite exercises repair/rebuild, insert/edit/delete/restore FTS, dedup
races, one OP, poll-voter uniqueness, failed commits, backup/restore, software-update
snapshots and interrupted updates. Browser validation covers manual/automated
backup, full restore, rejected corrupt restore, native updates/restarts, media,
moderation and seven browser/no-JS projects.

## 12. Validation

Formatting required a minimal correction to three existing comma placements in
`server/assets.rs`; no UI behavior changed. Four heavy harness cases per Rust
target are explicitly ignored for normal CI; bounded concurrency and parent crash
regressions run normally. No lint policy was weakened or suppression added.

The seven-project browser matrix initially passed 104 applicable cases and failed
three copies of the same stale PNG-only assertion. The existing pipeline produced
lossless WebP, so the assertion now checks exact persisted MIME and PNG/WebP magic
bytes. The targeted seven-project rerun passed all three applicable cases; four
projects correctly skip that no-JS-only test. The other 75 matrix skips are
existing project applicability guards. No failing case was disabled. A separate
Chromium recovery/media/update suite passed 35 cases with three existing guards.
Together these are 142 passing browser executions.

The initial full Rust run exposed a genuine listing tie-order regression, fixed
with active-descending/archive-ascending ID order and full-field equivalence
coverage. A later unrestricted parallel run had one configuration supervisor
lease test fail acquiring a released lease. Its isolated rerun and a complete
four-thread suite passed (1330 tests in each library/binary target, seven
integration/doc tests). The cause of that transient lease failure is not proven;
no lease behavior or assertion was weakened. The final unrestricted rerun instead
had one PDF fallback-format assertion fail; its isolated rerun passed, and the
complete suite was rerun with the four-thread limit documented in
`docs/media-capabilities.md`. Its transient cause is also unproven; the PDF
assertion/implementation were preserved. Final-tree command results follow.

| Command / suite | Final result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --locked --workspace --all-targets --all-features` | Passed |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed |
| `cargo test --locked --workspace --all-features` | Final unrestricted run: 1329 passed, 1 PDF assertion failed; isolated rerun passed; complete four-thread rerun below |
| `cargo test --locked --workspace --all-features -- --test-threads=4` | Passed: 1330 library + 1330 CLI + 5 CLI integration + 1 native UI fixture + 1 doc = 2667; 8 explicitly invoked heavy cases ignored in normal suite |
| `cargo deny --locked check` | Passed advisories, bans, licenses and sources; existing reviewed warnings |
| `python3 -m unittest discover -s tools -p test_update_package.py` | 7 passed |
| `cargo test --locked --lib db:: -- --test-threads=4` | Focused DB regressions passed before final suite |
| `cargo test --locked --lib server::handlers::posting::tests:: -- --test-threads=4` | Posting policy/race/idempotency regressions passed before final suite |
| Ignored SQL matrix / query profile / PNG harness commands below | All passed with schema/integrity/FK/FTS checks; bounded experimental busy result documented |
| Chromium 11-spec posting/media/backup/maintenance/update suite | 35 passed, 3 applicability skips |
| Seven-project posting/password/no-JS/moderation matrix | 104 passed; 3 MIME assertion failures corrected, targeted rerun 3 passed; 75 applicability skips |
| Seven-project corrected upload-format case | 3 passed, 4 applicability skips |
| `git diff --check` | Passed |

Browser commands (existing repository Playwright harness):

```sh
RUSTCHAN_UPLOAD_REGRESSION_E2E=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/sqlite-engineering/chromium npx playwright test tests/e2e/posting-thread.spec.ts tests/e2e/submission-idempotency.spec.ts tests/e2e/upload-validation.spec.ts tests/e2e/upload-regressions.spec.ts tests/e2e/non-video-media.spec.ts tests/e2e/password-boards.spec.ts tests/e2e/moderation-phase2.spec.ts tests/e2e/backup-restore.spec.ts tests/e2e/maintenance-backup-phase2.spec.ts tests/e2e/software-updates.spec.ts tests/e2e/settings-restart.spec.ts --project=chromium --workers=2 --max-failures=5
RUSTCHAN_AUDIT_OUTPUT=output/playwright/sqlite-engineering/matrix npx playwright test tests/e2e/posting-thread.spec.ts tests/e2e/password-boards.spec.ts tests/e2e/audit-nojs-parity.spec.ts tests/e2e/moderation-phase2.spec.ts --workers=2 --max-failures=5
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/sqlite-engineering/upload-format npx playwright test tests/e2e/audit-nojs-parity.spec.ts --grep 'file-upload path stores and serves' --workers=2
```

Changed files:

| Files | Purpose |
| --- | --- |
| `src/db/{mod,pool,diagnostics,workload}.rs` | FULL per connection, bounded pool diagnostics, concurrency/crash tests and offline matrix/profile |
| `src/db/{posts,threads,schema}.rs` | FTS/page queries, two additive indexes, atomic posting revalidation/bump logic, equivalence tests |
| `src/handlers/{mod,posting,thread}.rs`, `src/handlers/board/create_thread.rs`, `src/handlers/posting/performance.rs` | Scoped posting/media ownership, policy race tests, actual PNG harness |
| `src/handlers/admin/backup/create.rs` | Release live connection after authorized snapshot; package snapshot metadata |
| `src/{pending_fs.rs,media/prune.rs}` | Scoped promotion, replay/startup cleanup and configured pruning while preserving recovery boundaries |
| `src/server/server.rs`, `src/server/server/{observability,assets}.rs` | Blocking recurring DB work, accurate checkpoint/pool metrics; required comma formatting |
| `tests/e2e/audit-nojs-parity.spec.ts` | Verify stored upload format and actual bytes |
| `docs/sqlite-engineering.md`, `docs/sqlite-evidence/*.json` | Audit, measured decisions and compact reproducible raw evidence |

Reproduce the offline evidence from the current tree (run serially to avoid
benchmark interference):

```sh
RUSTCHAN_DB_EVIDENCE=/tmp/rustchan-db-matrix.json cargo test --locked --lib db::workload::benchmark -- --ignored --exact
RUSTCHAN_DB_QUERY_EVIDENCE=/tmp/rustchan-db-queries.json cargo test --locked --lib db::workload::query_profile -- --ignored --exact
RUSTCHAN_DB_POST_EVIDENCE=/tmp/rustchan-db-media.json cargo test --locked --lib server::handlers::posting::performance::posting_connection_workload -- --ignored --exact
```

The crash child is started and killed by its normal parent regression, not
manually invoked. Generated databases are temporary and are never committed.

## 13. Remaining limitations

SQLite WAL still has one writer. Short transactions and scoped connections cannot
remove that limit. Consider PostgreSQL only when measurements show sustained
write-lock p95/p99 approaching the one-second busy bound, persistent retryable busy
errors despite short indexed writes, a growing durable job backlog attributable
to write contention, or a real requirement for multiple application servers to
share transactional state. Filesystem coordination would still need redesign for
multiple servers; replacing SQLite alone does not solve it.

A real storage power-loss campaign, release-build deployment-scale soak, sampled
peak RSS, diverse filesystems/devices and cross-platform timing are outside the
local evidence. Operators should watch pool wait/hold warnings, debug lock/SQL
labels, job queues and WAL growth under their actual workload. Existing deep
health/repair and exclusive restore work can be expensive by design. Nothing in
these measurements justifies a database migration today.
