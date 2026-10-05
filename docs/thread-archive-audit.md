# Thread/archive production-readiness audit

## Architecture before changes

Threads remain in `threads` with an `archived` boolean; posts, polls, submission
tokens and media references stay in place. There is no separate archive table,
archive timestamp, time-based expiry, or public unarchive action. The internal
unarchive helper preserves the lock until explicitly unlocked.

Public thread creation commits the thread, OP, optional poll, file publication
intent and coalesced durable `thread_prune` job in one immediate transaction.
Replies serialize thread state, reply count and bump-limit checks under SQLite's
write lock. Sage replies increment the count without bumping. Reaching the bump
limit stops future bumps; it does not itself archive a thread. Deleting replies
reduces the reply count but preserves bump time. Deleting the OP deletes its thread.

`workers::prune_threads` loads current board settings and archives overflow
non-sticky active threads by descending bump time. Sticky active threads do not
consume `max_threads`; locked threads do. Archiving atomically sets archived and
locked. `allow_archive || CONFIG.archive_before_prune` decides whether active
overflow is archived or deleted. The global safety net defaults to true.
Startup and periodic bounded reconciliation schedule any outstanding overflow;
creation and moderation can temporarily exceed limits pending maintenance.

Archive retention is a count cap, `max_archived_threads`, not a duration. All
archived threads, including manually archived sticky threads, participate; sticky
is not an archive preservation flag. Retention keeps most recently bumped
content, not most recently manually archived content. Deletion cascades database
references and commits a durable file-delete intent after checking all remaining
references. Filesystem replay checks references again and resumes after restart.
Archiving itself neither moves nor deletes media. Active-media pruning excludes
archives, and the normal media reconciliation pipeline remains authoritative.

`/{board}/archive` lists 20 threads per numbered page. It paginates threads before
aggregating post/media counts. `/{board}/thread/{id}` remains the stable URL across
archival and shares the active renderer, quoting, escaping, thumbnails and CSP.
Thread loads check the actual board. Board-password gates apply to archive,
thread, search and media routes. Catalog, index and unread history exclude
archives; board FTS and admin searches include archived posts. Replies and owner
edit/delete controls reject archived content under the write lock.

Administrators can archive and delete threads using session, origin and CSRF
checks. No moderator-scoped role or preservation/unarchive UI exists. Board
limits are exposed through established board settings; global archive policy
uses TOML/environment configuration. Workers validate limits before deletion.

## Findings and implementation evidence

| Problem and root cause | Repair | Regression coverage |
| --- | --- | --- |
| Equal bump timestamps had no explicit retention tie-breaker, allowing planner choice to disagree with visible ordering. | Active retention uses `bumped_at DESC, id DESC`; archive retention uses the existing archive order, `bumped_at DESC, id ASC`. | `archive_threshold_ties_sticky_locked_and_idempotency`, `live_deletion_ties_keep_the_visible_newest_threads`, `archive_pages_and_retention_share_tie_order_and_search_survives` |
| Maintenance read policy before acquiring its write lock, then archived and pruned in separate transactions. A failed prune could leave a partially completed maintenance pass. | Read current policy, archive/live-prune, archive-cap pruning and file intents under one immediate transaction; finalize files after commit. | `archive_and_prune_worker_rolls_back_as_one_operation`, `combined_retention_failure_rolls_back_archive_transition`, existing `delayed_prune_uses_only_current_limits` and `delayed_prune_uses_current_archived_limit` |
| Overflow recovery built an unbounded SQLite parameter list. Large restored/backlogged boards could fail maintenance. | Archive using a parameterized selection subquery; batch file collection and deletion into at most 500 IDs per statement. | `archive_recovery_handles_more_than_sqlite_parameter_limit` uses 33,000 threads. |
| Hand-written archive/prune transactions relied on individual rollback/commit paths. | RAII transactions roll back on body or commit failure and release the write lock. | `failed_archive_commit_releases_the_lock_and_rolls_back`, `archive_busy_failure_and_restart_are_recoverable`, `process_interruption_during_archive_recovers_without_content_loss` |
| Invalid administrator retention values were clamped to one or defaulted to 150, potentially deleting content; omitted fields reset existing caps. Direct DB callers also lacked destructive-limit validation. | Reject invalid explicit values with HTTP 400; preserve omitted caps; validate again at the archive/prune DB boundary. | `retention_settings_reject_invalid_values_and_preserve_omitted_limits`, `invalid_retention_http_settings_cannot_reduce_saved_caps`, `destructive_limits_fail_closed_at_database_boundary`; browser settings rejection assertions |
| Existing/manual/safety-net archives became inaccessible through the archive index when `allow_archive` was false. Navigation used the same incorrect condition. | Archive access and navigation are independent of automatic overflow policy. | `archive_index_remains_accessible_when_overflow_archiving_is_disabled`; browser automatic/manual archival scenarios |
| Archived polls still offered voting and accepted mutations, despite the thread being read-only. | Render poll results and `[archived]`; enforce `threads.archived=0` inside the vote INSERT itself. | `archived_poll_is_read_only_in_thread_and_vote_route`, extended vote membership/expiry regression, browser poll rejection assertions |
| Manual moderation could succeed without an audit record, and archive logs used the supplied board name. Manual archive/unsticky changes lacked an immediate durable retention request. | Session authorization, state mutation, canonical archive log and retention intent commit together. | `manual_archive_rolls_back_when_audit_logging_fails`; browser anonymous/CSRF rejection and forged-board audit/redirect checks |
| Archive counts and rows were separate snapshots; access preflight could become stale during a password change. | Revalidate board access and read board/count/page under one SQLite read snapshot. | `archived_pages_enforce_board_view_passwords` checks protected archive and stable thread URLs, forged cookies and post-password viewability; pagination regressions |
| Archived replies were reported as merely locked because that check ran first. | Prioritize archive status during preflight and the authoritative reply write check. | Extended `reply_rechecks_thread_state_after_independent_preparation`; browser checks the precise archived error. |

These changes preserve background enforcement: the active cap is authoritative
when maintenance runs, not a synchronous cap on thread creation. Sticky active
threads remain exempt, locked active threads remain eligible, and archived sticky
threads remain subject to archive retention. No time-based expiry, preservation
flag, separate archive table, or reopening interface was introduced.

## Database, concurrency and recovery

The only schema change replaces `idx_threads_board_sticky_bumped` with:

```sql
CREATE INDEX idx_threads_active_order
ON threads(board_id, sticky DESC, bumped_at DESC, id DESC)
WHERE archived = 0;
```

The existing strict schema-repair path recognizes the exact legacy index,
installs the replacement and removes the redundant old index in its migration
transaction. The existing upgrade regression reconstructs that legacy schema and
verifies data survives repair. No columns, foreign keys or dependency versions
changed. The existing archive index continues to support archive pagination and
retention order.

File-backed WAL tests race creators, replies and maintenance, and race
stickying/locking/deleting against archival. SQLite write transactions determine
the outcome; no application-wide mutex was added. Two overlapping worker passes
converge to the same caps. Tests also exercise lock contention, failed DELETE
triggers, deferred-foreign-key commit failure, reopening the database, and a
subprocess killed after its archive UPDATE but before commit. The latter verifies
the original active state returns intact and maintenance can safely retry.

New concurrency tests are
`overlapping_creators_and_archive_passes_preserve_every_thread`,
`concurrent_oldest_reply_and_archive_obey_the_committed_bump`, and
`concurrent_moderation_and_archive_are_serialized_by_sqlite`.
`sage_bump_limit_and_deleted_replies_preserve_archive_order` verifies sage,
bump-limit and reply-deletion behavior, including rejection of archived replies
even if an administrator clears the lock flag.

## Media, routing, search and security

Archive rendering uses the existing shared post renderer. The new
`archived_media_and_markup_use_the_same_safe_post_renderer` compares active and
archived reply output for image, video, audio, PDF, other-file and legacy media,
including companion audio, quotes, spoilers and hostile names/filenames.
`archived_media_survives_and_shared_paths_are_never_pruned` verifies archival
preserves references and retention never schedules deletion of a shared original,
thumbnail or companion audio path.

The unchanged media subsystem already tests missing-thumbnail fallback, pruned
originals, processing failures, HTTP validators/ranges, legacy redirects,
reference rechecks and interrupted durable cleanup. Those regressions are
included in the full test suite. Board restore rerenders `body_html` from escaped
raw body through the shared sanitizer; it does not trust archived HTML supplied
by a board-backup manifest. No separate archive rendering or conversion pipeline
was added.

Stable thread URLs remain valid after archival; wrong-board, negative,
nonexistent and pruned IDs fail through the existing route checks. Board archive
and archived thread pages retain view-password protection. Poll voting is now
read-only at the SQL write boundary. Administrator actions retain session,
origin and CSRF enforcement and now fail atomically when audit logging fails.
Archive requests remain parameterized, user display fields stay escaped, media
delivery retains path validation, and the existing CSP is unchanged.

Search still includes archives and uses the existing FTS index. Index, catalog
and unread activity still exclude archives. Pruning removes searchable posts
through existing cascades/triggers. No cache of archive rows was introduced;
cookie-dependent responses retain `Vary: Cookie`.

## Archive UI and administration

Archive navigation is always discoverable from board/index/catalog views. Rows
show thread number, state, reply and attachment counts, creation and last-bump
times. The page states its actual retention cap; empty-state copy is clearer.
Decorative thumbnails use empty alternative text and archive rows have visible
theme-aware keyboard focus outlines. Existing routes, numbered pagination,
element hooks, progressive enhancement and no-JS browsing are preserved.

Administrator guidance now explicitly says lowering the archive cap permanently
prunes overflow, archived sticky threads are included, and the global safety net
can override the automatic-archive checkbox. `SETUP.md` documents creation,
sage/bump limits, count retention, media cleanup, manual moderation, search and
live versus restart-required policy changes. The generated TOML comment correctly
describes in-place archival rather than a nonexistent archive table.

## Performance evidence

The explicit `archive_query_profile` test uses 10,000 threads and 210,000 posts,
including 9,900 archived threads. Both index candidates receive `ANALYZE` before
comparison. These local Rust 1.99/SQLite debug-build measurements are illustrative,
not production latency guarantees.

| Operation | Evidence |
| --- | --- |
| 200 active-retention selections before | 13.903 ms; `SEARCH threads USING INDEX idx_threads_archived (board_id=? AND archived=?)`, then `USE TEMP B-TREE FOR LAST TERM OF ORDER BY` |
| 200 selections after | 6.169 ms; `SEARCH threads USING COVERING INDEX idx_threads_active_order (board_id=? AND sticky=?)`; no temporary sort, approximately 2.25× faster |
| Archive page, offset 0 | 4.328 ms for 20 loads, approximately 0.216 ms/page |
| Archive page, offset 5,000 | 52.855 ms for 20 loads, approximately 2.643 ms/page |
| Archive page, offset 9,880 | 99.045 ms for 20 loads, approximately 4.952 ms/page |
| Archived thread and posts | 0.404 ms |
| FTS search including archives | 11.893 ms |
| Archive plus retention deleting 950 threads/19,950 posts | 268.596 ms |

Deep OFFSET cost increases, but the validated archive cap is 10,000 and this
measured cost does not justify replacing established numbered URLs with cursors.
The page-first aggregation remains in place. Separately, the 33,000-thread test
proves backlog maintenance avoids the SQLite bind-parameter scaling trap.

Reproduce profiling with:

```sh
RUSTCHAN_ARCHIVE_EVIDENCE=/tmp/rustchan-archive-performance.txt \
  cargo test --locked --lib db::threads::archive_tests::archive_query_profile \
  --all-features -- --ignored --exact
```

## Files changed

- `src/db/threads.rs` and new `src/db/threads/archive_tests.rs`: transitions, deterministic pruning, batching, boundary validation and lifecycle tests.
- `src/db/schema.rs`: partial active-order index and legacy upgrade regression.
- `src/db/posts.rs`: atomic archived-poll vote rejection.
- `src/workers/mod.rs`: atomic current-policy retention and worker rollback regression.
- `src/handlers/admin/content.rs`: atomic audited moderation and retention scheduling.
- `src/handlers/admin/settings/board.rs`: safe retention parsing and handler regressions.
- `src/handlers/board/catalog.rs` and `src/handlers/board/tests.rs`: archive accessibility, consistent authorized snapshot and routing regressions.
- `src/handlers/posting.rs` and `src/handlers/thread.rs`: precise archived reply/vote errors.
- `src/templates/board.rs`, `src/templates/thread.rs`, `src/templates/admin.rs`, `static/style.css`: archive metadata, navigation, read-only polls, administrator guidance and focus styling.
- `src/config/template.rs`, `SETUP.md`, `docs/thread-archive-audit.md`: corrected policy/operational documentation and audit evidence.
- `docs/strict-lint-exceptions.tsv`: remove the worker complexity expectation made obsolete by the focused simplification.

The new `tests/e2e/archive-production.spec.ts` stays local, as required by the
repository's existing Playwright ignore/check policy. No browser harness,
package manifest or tooling is force-added to Git.

## Validation results

Validated on macOS with `rustc 1.99.0 (b940084d7 2026-09-28)`.

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --locked --workspace --all-targets --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed; no new lint allowances |
| `cargo test --workspace --all-features` | Passed: 1,376 unit tests in each library/binary target, six integration tests and one doctest; zero failures. Seven tests are explicitly ignored in each unit target. The archive profiler was run separately; the interrupted child helper runs through its parent regression. |
| `cargo test --locked --lib db::threads::archive_tests --all-features` | Passed: 14 regressions; profiler and subprocess helper intentionally ignored |
| Explicit ignored archive profiler command above | Passed; before/after evidence recorded in this report |
| `RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps` | Passed |
| `cargo deny --locked check` | Passed advisories, bans, licenses and sources; existing missing-license metadata for `captcha` and duplicate-version warnings remain. Dependencies were unchanged. |
| `python3 tools/check_local_tooling.py` | Passed; local browser tooling remains untracked |
| `git diff --check` | Passed |

Browser validation used the existing local isolated-runtime Playwright harness:

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/archive-final \
  npx playwright test tests/e2e/archive-production.spec.ts --workers=2
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/archive-existing-final \
  npx playwright test tests/e2e/settings.spec.ts --grep 'bump/archive|thread sage' \
  --project=chromium --workers=2
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/archive-backlinks-final \
  npx playwright test tests/e2e/archive-production.spec.ts --grep 'automatic archival' \
  --workers=2
```

Results: 14/14 new scenarios across Chromium, WebKit, Firefox, mobile Firefox,
mobile WebKit, Firefox without JS and Chromium without JS; 2/2 unchanged existing
archive scenarios; 7/7 final automatic-archive scenarios after adding explicit
backlink navigation. Coverage includes real image uploads, automatic archival,
stable URLs, quotes/backlinks, media loading, search, pagination, restart, denied
replies/votes, manual moderation authorization/CSRF/canonical audit logs and
invalid settings. All nine built-in themes were checked at 1,280 and 390 pixels,
with overflow/focus assertions and visual screenshot review. No existing test or
diagnostic assertion was weakened.

One earlier parallel full-suite run hit a transient lock-acquisition failure in
the unchanged `config::admin::tests::atomic_save_validates_before_mutation_and_does_not_change_active_config`
test. The complete default-parallelism rerun passed. This was recorded rather
than changing unrelated configuration behavior or suppressing that test.

## Remaining limitations

- Creation can temporarily exceed the active cap while its durable maintenance
  job is pending. Failure/retry/reconciliation remain operational concerns.
- Archives are capped by count and bump order. There is no permanent/protected
  archive mode, archive timestamp, time retention or public reopening control.
- File removal follows the committed database transition through durable cleanup
  and reference checks. It cannot be one atomic transaction across SQLite and
  the filesystem; failures remain queued and logged for replay.
- The interrupted-process regression tests SQLite recovery from process death,
  not physical power loss or every storage failure. Invalid retention policy
  fails closed; this audit does not invent an automatic corrupted-database repair.
- Timing evidence is from this machine and debug fixtures. Deployments should
  monitor maintenance backlog and SQLite contention under their own workloads.
