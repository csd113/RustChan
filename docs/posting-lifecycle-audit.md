# Posting and thread lifecycle audit

Audit date: 2026-10-06. Scope: public creation/replies, owner mutations,
moderation, quoting, ordering, retention, retry behavior, SQL/filesystem recovery,
and browser workflows. No dependencies, commits, branches, or broad redesign were
introduced. The starting checkout was clean.

## 1. Existing architecture

`POST /{board}` and `POST /{board}/thread/{id}` share the bounded multipart parser
in `handlers/mod.rs` and the posting service in `handlers/posting.rs`. The parser
validates CSRF, UTF-8, control/upload multiplicity and byte budgets, and owns
temporary uploads. The posting service checks identity, bans, access, cooldown,
CAPTCHA, text/poll rules and media policy. Expensive preparation releases its
pooled connection. A policy callback rechecks mutable settings after acquiring
SQLite's writer lock.

`db/threads.rs` creates the thread/OP or reply in an immediate transaction. This
bundle includes the submission receipt, counters/bump, optional poll, filesystem
publication intent, required media work and, for new threads, durable coalesced
retention work. Search updates use database triggers. Thread/post IDs are distinct
global AUTOINCREMENT identifiers; an OP post ID must not be assumed to equal its
thread ID. Replies display their post IDs rather than a separate reply sequence.

Owner edit/delete routes check signed ownership grants and time windows.
Administrator routes check session, origin and scoped CSRF. Deletions commit
durable cleanup intents; filesystem operations happen after SQL commit and
recheck shared-file references. Retention workers read current board settings
under their own immediate transaction. Startup and periodic reconciliation
recover outstanding work.

Server-rendered and upload-enhanced forms share this backend. Native success is
303 followed by GET; upload XHR success is 204 with `X-Rustchan-Redirect`. Handled
XHR posting rejections intentionally use transport 200 with JSON `error` and
`X-Rustchan-Error-Status`; the browser renders an error and does not navigate as
if accepted. Some earlier validation paths return an ordinary error status.
HTML cache signatures include posts, counts, state, media and poll changes;
cookie-sensitive views use the existing Vary/private-cache rules.

## 2. State model

| Current state | Public reply | Owner edit/delete | Administrator / retention behavior |
| --- | --- | --- | --- |
| Active, unlocked | Allowed after current policy checks | Allowed within ownership/window rules; OP self-delete requires no replies | Lock, sticky, archive or delete |
| Active, locked | Rejected, including sage and media-only replies | Rejected | Unlock, archive or delete; participates in active retention |
| Active, sticky | Same as its lock state | Same as its lock state | Exempt from active thread cap; global sticky differs from personal pin |
| Archived | Rejected independently of the lock flag | Rejected | Readable at the same URL; deletion/archive retention allowed |
| Deleted / destructively pruned | Missing target; rejected | Missing target | Cascaded content removed; audit history and bounded retry evidence retained |

Sticky and locked can coexist. Archiving normally sets both archived and locked;
the internal unlock helper cannot make an archived thread publicly replyable.
There is no public move or unarchive posting workflow. Personal pin/hide state
affects presentation, not posting authorization or global retention protection.

## 3. Enforced invariants

- A committed creation includes one thread and one matching OP. Every reply has
  an existing parent and matching board; foreign keys and domain triggers protect
  ownership, and the reply API now explicitly rejects the OP flag.
- Current parent state and mutable posting policy are checked under the same
  SQLite write lock as insertion. Closed/missing targets cannot be bypassed by a
  stale form or crafted request.
- A successful receipt names one canonical operation and target. Reuse for
  another board, actor, operation or reply target cannot create content.
- SQL failure, including commit failure, rolls back posts, counters, bump,
  receipts, required jobs and filesystem intents and releases the transaction.
- Successful SQL commit is not subsequently reported as failed because auxiliary
  spam scheduling or a redundant post read fails.
- Stored reply counts equal persisted non-OP rows; listing media counts equal
  their established primary-file count. Sage increments counters without bumping.
- Deletion preserves intentionally retained audit history and prevents an old
  accepted token from resurrecting content during receipt retention.
- SQL and filesystem publication are not a shared transaction: durable intents
  make publication/deletion recoverable, and media workers wait for publication.

These guarantees concern authorized application operations with SQLite foreign
keys/triggers enabled. They do not promise survival of arbitrary external database
corruption, deleted backups, or irrecoverable storage loss.

## 4. Bugs found and repaired

| Finding | Repair |
| --- | --- |
| Cascading content deletion erased the only submission receipt, permitting the same new-thread token to create replacement content. | Historical receipt table and BEFORE DELETE triggers on posts, threads and boards. Parent triggers handle cascade ordering. |
| A successful reply retry was rejected after lock/archive before receipt lookup. | Replay lookup precedes early parent-state validation; new submissions still use the authoritative transactional check. |
| Receipts were not bound to operation/target; wrong-scope token collisions became database errors. | Explicit operation/target validation and typed 409 conflicts for scope reuse. |
| A thread removed during preparation became an internal error at insertion. | Missing parent produces typed 404 and staged-media rollback. |
| Required deferred media jobs and state were recorded after posting commit. | Persist media intent and processing state in the creation transaction. |
| Workers could claim a media job before its staged source was published. | Claim query excludes media sources still named by upload-finalize intents. |
| Auxiliary scheduling and a post-commit read could turn accepted posting into apparent failure. | Capture creation time before commit; warn on auxiliary scheduling/notification failures. |
| Ban-and-delete trusted submitted identity and OP/board classification; ban, deletion and audit records could partially succeed. | Resolve and validate the stored target, then commit authorization, ban, deletion and both audit entries together. |
| Administrator deletion could succeed without an audit record or after stale authorization. | One immediate transaction covers session, target, deletion and audit. |
| Owner action board permissions/settings could change between preparation and mutation. | Recheck permission and edit preparation policy inside the mutation transaction. |
| Repeated scalar multipart controls used last-value-wins behavior. | Reject repeated scalar controls; retain intentional repeated poll options and established upload-slot rules. |
| Quotes linked overflowing/nonpositive/Unicode or embedded malformed IDs. | Require positive ASCII i64 IDs, canonical targets, valid boundaries and valid board syntax; invalid references remain escaped text. |

## 5. Race conditions

Write transactions define the winner: if a reply commits before a lock/delete/
prune transaction, the later action operates on that committed state; if the
state transition commits first, insertion sees the closed or missing parent and
rejects. Deletion may legitimately remove an earlier successful reply. A retry
after that deletion reports the missing canonical target without creating another
post. Moderation authorization and audit failure now share this serialization.

Preparation races with board access, media settings, CAPTCHA policy, filters,
cooldown, bans and session loss are covered by policy revalidation. Concurrent
identical submissions serialize receipt lookup and insertion under the writer
lock. Losing prepared uploads are cleaned rather than published twice.

## 6. Transaction-boundary changes

Required audio/video job JSON, media state and response creation time moved into
the post transaction. Administrator delete and inline ban/delete now use RAII
immediate transactions for all required SQL work. Owner actions gained a narrow
validation callback invoked inside their existing immediate transaction. Existing
atomic reply counter/bump, OP/poll, FTS and durable pruning boundaries remain.
Filesystem finalization and expensive media/body preparation stay outside the
SQL write transaction.

## 7. Thread creation

Existing empty/media, board-access, size, poll and CSRF checks remain authoritative.
No image-required board rule or separate disabled-board flag exists; password
access and configured media permissions determine availability. Creation now
also atomically records required media work and safely handles deleted receipts
and wrong-operation reuse. Failed media scheduling and deferred commit failure
leave zero new threads/posts/receipts. Prune intent remains a required part of
public creation, so scheduling failure cannot leave permanent unhandled overflow.

## 8. Replies

Current board/thread ownership, lock, archive, cooldown and access are enforced
at insertion. Missing targets now return a normal recoverable 404. A reply cannot
be inserted with `is_op=true`. Successful retries return the original post anchor
even if the thread later closes; deleted-target retries cannot recreate a row.
Creation timestamps needed for ownership cookies come from the transaction.

## 9. Bump and sage

Existing semantics were preserved. Normal replies bump only when the current
pre-insertion reply count is below `boards.bump_limit`; sage replies never bump.
The checkbox accepts `1`, case-insensitive `on`, or case-insensitive `true`, not
arbitrary name/email text. Media-only replies follow the same rule. Deletion
reduces reply count without rewinding bump time; it can make later replies
eligible to bump again under the established count rule. Edits and moderation
state changes do not bump. Active ordering is sticky, bump time, ID descending;
archive order is bump descending, ID ascending. Existing tie/pagination and
concurrent bump-limit tests remain in the full suite.

## 10. Quotes

`>>id` remains a local fragment. `>>>/board/id` resolves through that board's post
redirect and supports explicit same-board cross-thread and cross-board links;
`>>>/board/` links its index. Search rendering scopes otherwise local links.
Parsing now rejects invalid numeric ranges, Unicode numerals, negative/zero IDs,
oversized board names and quote-like embedded suffixes. Leading zeros resolve to
the canonical ID. No eager quote-target database lookup was added: missing or
deleted targets cannot make submission fail, and protected resolver/preview
routes retain access checks. Existing HTML escaping remains intact. Backlinks
are derived in the browser and deduplicated/rebuilt after updates, not stored
foreign-key records. Browser coverage checks repeated quotes, OP quoting,
crosslink routing, malformed references, escaping and reload behavior.

## 11. Lock and unlock

Fresh locked replies are rejected regardless of UI, sage or attachment. Current
state is checked again after preparation under the write lock. Unlock permits a
previously rejected token to be retried because rejection did not consume it.
An already accepted token is replayed rather than rejected after locking. Archive
is checked independently, so clearing only `locked` cannot reopen an archive.
The existing HTTP retry suite exercises lock/reject/unlock/corrected retry.

## 12. Deletion

Administrator and inline ban/delete actions use the actual database post, board,
thread, OP flag and posting identity. Forged details are rejected before mutation.
Audit insertion failure rolls back bans and content deletion. Owner actions
retain token/window/closed-thread/OP-with-replies restrictions and now recheck
current board permission. Database cascades remove posts, reports, polls/votes,
personal thread preferences and live receipts; FTS triggers remove search rows.
Moderation history remains. Shared media cleanup keeps the durable, reference-
checked deletion pipeline. Historical receipts intentionally have no content
foreign keys. Existing redirect routes and ban/delete reply anchors are preserved.

## 13. Pruning

Active `max_threads` applies to non-sticky, non-archived threads. Locked active
threads count; sticky active threads are exempt. Overflow chooses the oldest
bump/ID according to deterministic active ordering. Current archive policy
selects archive versus destructive prune. `max_archived_threads` is a count cap
and includes archived sticky threads. Both limits are validated before destructive
maintenance. Archive retention follows archive ordering. Creation commits a
coalesced durable job; workers/startup reconciliation converge to current caps.
Temporary overflow during deferred maintenance is an existing deliberate behavior.
Tests race 25 creators at cap one, verify one pending intent, then verify exact
retention convergence; existing worker tests exercise real automatic maintenance.

## 14. Limits and boundaries

Body limits count Rust Unicode scalar values, not bytes or grapheme clusters:
4096 characters; empty text requires an accepted attachment. Names are trimmed
and truncated to 64 characters, subjects to 128. The multipart parser additionally
limits text fields to 64 KiB, controls to 64, total payload to 512 MiB plus bounded
framing, and validates board/global per-type upload limits. Submission tokens
are bounded to 128 ASCII alphanumeric/underscore/hyphen characters. Existing
boundary/UTF-8/media tests cover rejection and cleanup. Thread cap/tie tests and
concurrent bump-limit tests verify the supported count boundaries.

There is no configured hard reply cap or image-count cap in this version.
`bump_limit` stops bumps, not replies; inventing a new 500-reply limit would change
the product. Upload byte caps and thread/archive caps remain the applicable limits.

## 15. Duplicate protection

Opaque form tokens are bound to actor, board, operation and reply target.
SQLite serializes duplicate lookup/create; no body-text deduplication was added.
Deleted receipts retain accepted IDs for the existing seven-day receipt horizon,
with indexed expiry cleanup on new receipt insertion. Live receipts have the
same opportunistic expiry model. A deleted canonical target returns 404. Failed
validation/commit leaves the token reusable for a corrected submission. Native
PRG, XHR redirects and existing busy-button guards remain. Distinct tokens still
allow intentionally identical content. Legacy empty-token clients remain supported
without an idempotency guarantee.

## 16. Stale forms

The database wins over embedded browser state. Lock, archive, delete, changed
board access/media/filter/CAPTCHA policy, bans, cooldown and revoked admin bypass
are evaluated again before insertion. Owner actions recheck board permission at
mutation. Native error recovery supplies escaped copyable drafts; XHR preserves
the current form and renders a rejection. File selections must be made again
after native navigation. Real browser tests submit attachment-backed forms after
lock/archive/delete and assert saved text and zero inserted rows/filesystem intents.

## 17. Counters

`threads.reply_count` is stored and changes with transactional reply insert/delete.
Board thread counts and listing `image_count` are computed. The latter is the
legacy count of non-NULL primary `file_path`, including video/PDF/general files;
secondary/audio-only slots do not all represent an additional image. This naming
and display behavior were deliberately retained. New invariant checks compare
stored reply counts to COUNT(non-OP), listing media counts to COUNT(primary file),
and verify one OP, board ownership, foreign keys and transaction release after
every operation. Bursts mix text and image rows and preserve sage ordering.

## 18. SQLite, recovery and performance

Production/test pools use file-backed WAL, foreign keys, synchronous FULL, a
one-second busy timeout and bounded pool checkout. Immediate posting/mutation
transactions avoid deferred write-lock upgrades. Busy errors remain retryable
503 with `Retry-After: 1` on the native error path; XHR exposes 503 through its
handled-error contract. A new contention test holds the writer, checks zero
partial writes and then successfully retries the same token. Deferred-FK commit
failure tests verify rollback and subsequent reuse. Existing archive tests cover
process interruption, restart, overlapping workers and maintenance commit failure.

EXPLAIN QUERY PLAN on an installed current test database showed primary-key index
searches in both receipt UNION branches, the covering `idx_threads_active_order`
for active ordering, and `idx_submission_tombstones_created` for expiry. No eager
quote-resolution N+1 queries or application-wide posting mutex were introduced.
The final review found full receipt scans for board/thread/post deletion. Three
target indexes now support the preservation triggers and foreign-key cascades;
the upgrade test also reconstructs their absence in the previous schema. Final
plans show `SEARCH ... USING INDEX idx_post_submissions_board/thread/post` for
all three target lookups, replacing the observed full-table scans.
The media claim guard scans outstanding publication intents, which are normally
short-lived; persistent storage trouble can delay jobs until reconciliation.
The exact additive schema repair recognizes only the new receipt objects; its
upgrade regression preserves existing live tokens and subsequent deleted history.

## 19. Security and resource checks

CSRF, scoped admin CSRF/origin, board password grants, signed ownership cookies,
parameterized SQL and current-state checks remain the boundaries. Repeated scalar
controls and malformed tokens now fail closed. Inline bans cannot select a
different actor via hidden fields or turn a reply into OP deletion. Quotes accept
validated IDs after escaping; malformed input stays text. Existing bounded
multipart/framing/field parsing, UTF-8 rejection, media sniffing, canonical path
validation, upload admission, timeouts, cooldown, CAPTCHA and filesystem reference
checks were reviewed and retained. No new aggressive rate limit was added.
HTTP framing is handled by the existing Axum/HTTP stack and deployment proxy;
this pass does not claim a separate proxy request-smuggling penetration test.

## 20. UX and compatibility

Deleted-during-preparation requests now receive a clear missing-target error.
Receipt reuse gives an actionable reload conflict. Accepted posts retain success
when auxiliary background work fails. Existing inline/generic draft recovery,
upload progress, double-submit controls, keyboard/modal behavior, mobile layout,
form field names, element IDs, routes, CSS/JS hooks and no-JS fallbacks are retained.
Browser validation caught and corrected a temporary ban/delete anchor regression.
No broad visual redesign was made. XHR's established handled-error transport
contract is tested explicitly, rather than mistaking transport 200 for acceptance.

## 21. Added and expanded regression coverage

New tracked suites: `src/db/threads/lifecycle_tests.rs` (10 tests) and
`src/handlers/posting/lifecycle_tests.rs` (5 tests). Additional regressions cover
schema upgrade, repeated scalar multipart controls, malformed quote grammar,
moderation audit rollback and forged inline-ban targets. An old deletion fallback
test now verifies fail-closed preservation when board lookup breaks. Obsolete
best-effort deletion board fallback helpers/tests were removed.

The existing local Playwright harness was expanded with
`tests/e2e/posting-lifecycle.spec.ts` (three workflows) and strengthened forged-board
archive assertions. Browser tooling is intentionally ignored and prohibited from
tracking by `tools/check_local_tooling.py`; it was not force-added. Tracked Rust
regressions remain available to CI. Existing posting, database, media, deletion,
archive, authorization, FTS and cache tests also run in the complete Rust suite.

## 22. Concurrency scenarios exercised

- Barrier-synchronized mixed text/image sage bursts of 2, 5, 10, 25 and 64 clients:
  every accepted ID unique, exact reply/media counts, no bump, valid parent rows.
- Twenty-five simultaneous thread creations at cap one: durable coalesced work,
  deterministic newest survivor after pruning, no permanent unmanaged overflow.
- Reply against lock, archive, deletion and pruning: eight repetitions per state
  with a barrier; only committed-before-transition or typed rejection outcomes.
- Three fresh HTTP runtimes: eight identical thread submissions, eight identical
  replies, eight distinct thread submissions and eight distinct replies per run;
  exact canonical redirects, row counts and receipt counts.
- Existing service races include cooldown contention and concurrent identical
  uploads; existing DB races cover bump limits, moderation/archive and overlapping
  retention workers. Preparation callbacks force stale policy/session/parent
  windows without timing-only sleeps.
- A fixed-seed 160-operation sequence checks invariants after lock/unlock,
  image/text/sage reply, deletion and sticky changes, then verifies deleted search
  state. Fault injection covers required media job failure, auxiliary spam job
  failure, commit failure and moderation audit failure.

## 23. Validation

Final command results follow. The complete Rust suite includes the dedicated
lifecycle/concurrency suites and the existing posting, database, archive,
filesystem, authorization and recovery regressions.

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --workspace --all-targets --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed |
| `cargo test --workspace --all-features` | Passed: 1,413 tests per lib/binary target; 5 integration, 1 native-render fixture and 1 doctest; 2,833 executed, 14 pre-existing ignored performance/child fixtures |
| `cargo build --locked --bin rustchan-cli` | Passed |
| Playwright posting/thread, dedicated lifecycle, submission idempotency, archive, admin ban/delete and password-board suites; Chromium, Chromium no-JS, mobile WebKit, two workers | Passed: 46 workflows, 8 project-specific skips |
| Final rebuilt-binary lifecycle/archive/CAPTCHA recheck; Chromium, Chromium no-JS, mobile WebKit and desktop WebKit | Passed: 24 checks, 4 CAPTCHA project skips; includes explicit no-JS CAPTCHA contexts in Chromium/WebKit |
| `git diff --check` | Passed |
| `python3 tools/check_local_tooling.py` | Passed |

Toolchain: Rust/cargo 1.99.0, matching the repository policy. No lints or tests were
disabled to pass validation. Browser project-specific skips distinguish no-JS
modal coverage, mobile-only cookie coverage and engine-independent HTTP races.
The full suite exposed one transient failure in the unrelated configuration-save
lock regression; it passed in isolation and the final complete rerun. New index
upgrade checks initially exposed a missing table-index repair allowlist entry;
that migration was corrected and both legacy/current upgrade regressions pass.
Final Rust output: `/tmp/rustchan-posting-lifecycle-rust-tests.log`.
Final browser output: `/tmp/rustchan-posting-lifecycle-browser-final-pass.log`;
JSON/HTML/traces: `/tmp/rustchan-posting-lifecycle-browser-final-pass/`.
Final schema/CAPTCHA browser output:
`/tmp/rustchan-posting-lifecycle-browser-schema-captcha.log`; artifacts use the
same name without `.log`.

Browser commands, using the existing local harness and the freshly built binary:

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=/tmp/rustchan-posting-lifecycle-browser-final-pass npx playwright test \
  tests/e2e/posting-thread.spec.ts tests/e2e/posting-lifecycle.spec.ts \
  tests/e2e/submission-idempotency.spec.ts tests/e2e/archive-production.spec.ts \
  tests/e2e/admin-ban-delete.spec.ts tests/e2e/password-boards.spec.ts \
  --project=chromium --project=chromium-nojs --project=mobile-webkit --workers=2
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=/tmp/rustchan-posting-lifecycle-browser-schema-captcha npx playwright test \
  tests/e2e/posting-lifecycle.spec.ts tests/e2e/archive-production.spec.ts tests/e2e/captcha.spec.ts \
  --project=chromium --project=chromium-nojs --project=mobile-webkit --project=webkit --workers=2
```

## 24. Deliberately retained limitations

- Retention is asynchronous and may temporarily exceed a cap; durable intents
  and existing reconciliation converge it. Making creation synchronously delete
  content would change established latency and retention behavior.
- No new hard reply/image cap, board-disabled switch, moderator-scoped role,
  thread move or public unarchive feature was invented.
- Receipt protection is bounded to seven days with opportunistic cleanup. A
  fresh or empty token, browser draft submitted from another independent form,
  or a restored backup missing newer receipts is not a global exactly-once
  guarantee. Changing identity cannot replay the original receipt and returns
  a conflict for a still-retained token. This avoids blocking legitimate identical posts.
- CAPTCHA challenges remain process-local and single-attempt. Simultaneous use
  before the first commit may reject the losing attempt; retries after a stored
  successful receipt bypass another challenge check. Multi-instance CAPTCHA
  coordination was not added.
- Filesystem/SQL atomicity relies on the existing durable journal and recovery.
  Processing failure preserves accepted source media with an explicit failed
  state; a full background queue retains its established skipped-processing
  failure state. Auxiliary spam-job failure is logged after acceptance.
- The additive receipt table, triggers and indexes change the exact baseline
  schema. Older unpatched binaries may reject that schema; database rollback
  across binary versions needs the project's existing compatible-backup process.
- Local `>>id` scope, computed backlink behavior, legacy media-count meaning,
  second-resolution bumps and numbered-page movement under live activity remain.
- Concurrency evidence is from isolated local SQLite WAL runtimes and browser
  engines, not a distributed deployment, every possible crash instruction, or
  production load benchmark. The existing subprocess archive recovery tests are
  included. Browser harness changes remain local by repository policy.

## Files changed

Tracked implementation: `src/db/posts.rs`, `src/db/schema.rs`, `src/db/threads.rs`,
`src/handlers/admin/content.rs`, `src/handlers/admin/moderation.rs`,
`src/handlers/mod.rs`, `src/handlers/posting.rs`, `src/handlers/thread.rs`,
`src/utils/sanitize.rs`.

New tracked regressions: `src/db/threads/lifecycle_tests.rs`,
`src/handlers/posting/lifecycle_tests.rs`. Report: `docs/posting-lifecycle-audit.md`.

Local ignored browser changes: `tests/e2e/posting-lifecycle.spec.ts`,
`tests/e2e/archive-production.spec.ts`.
