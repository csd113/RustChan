# Post and thread actions audit

## Pipeline mapped before implementation

| Entry point | Request and authorization | State and rendered outcome | Inverse/failure |
| --- | --- | --- | --- |
| Catalog three-dot menu | Delegated `main.js` disclosure; native POST forms; no-JS `<details>` fallback | Server supplies report OP ID, thread ID, board, CSRF, personal pin/hide labels | Outside click/Escape close; vertical positioning only; no keyboard arrow navigation |
| Report thread / post | `POST /report`; CSRF, board-view password, ban check, post-to-board and post-to-thread checks | `db::file_report` derives target metadata from posts; unique open report per post/reporter; moderator reports table escapes text; redirect to target thread | Resolution is admin-only; target deletion cascades reports; duplicate inserts succeed without distinction in confirmation |
| Personal Pin / Unpin | `POST /{board}/thread-preference`; CSRF, board-view access, matching active thread | `user_thread_preferences.pinned`; catalog sorts personal pins, then sticky, then bump | Explicit setters; zero/default rows removed; no effect on global sticky flag or other visitors |
| Personal Hide / Unhide | Same preference route and access checks | `user_thread_preferences.hidden`; active catalog split; `/{board}/hidden` restores threads | Reload persists through server DB; only catalog consumes preferences; no reply-hide feature |
| Global Sticky / Unsticky | Native `POST /admin/thread/action`; session-scoped CSRF plus trusted origin; admin session checked inside immediate transaction | `threads.sticky`; board/catalog active listing orders sticky, bump, ID; state and mod log commit together | Explicit idempotent setters; unsticky schedules retention; archive ordering deliberately ignores sticky |
| Lock / Unlock / Archive | Same admin route and transaction | Canonical thread flags; archive also locks; prune intent persisted | No moderator board scopes exist: admin accounts have instance-wide moderation rights |
| Edit / Delete / Ban+delete | Existing post controls, separate native POST paths with own ownership/admin checks and confirmation | Not currently inside catalog overflow; existing routes and hooks must remain | Existing self-action expiry and moderation tests cover them; no new destructive menu actions needed |

Personal preference identity reuses the existing IP/Tor identity hash. Loopback/unknown peers use the visitor cookie. It is independent of administrator account identity and deliberately remains so. Cookie clearing on public-IP identities does not erase database preferences. Foreign keys cascade preferences when their target thread is deleted; valid preferences need no time-based eviction. Boolean domain triggers reject malformed stored flags.

## Verified problems and implementation scope

- Catalog and hidden management fetch only the first 200 active threads before filtering/sorting, making older hidden/pinned preferences inaccessible.
- Board index ignores personal preferences, including its pagination and counts. Its summary fragment also leaves the thread container open, allowing later summaries/action wrappers to become nested; the missing closing tag is now fixed.
- Preference access/target checks and writes are separate autocommit operations, allowing archive/delete races.
- Form boards are lossily sanitized rather than rejected; actions use loose normalized strings.
- Popup positioning has no horizontal clamp, remains inside clipping ancestors, and does layout work on every scroll event. Closing scans every menu.
- A popup has no arrow/Home/End navigation and may remain open after focus moves away or history restores it.
- Report uses lossy board sanitation and silently treats duplicate submissions as newly filed. Report POSTs have no dedicated abuse limit when browsing-only limiting is enabled.
- Catalog ETags omit lock/media/reply metadata and hidden-management responses lack explicit personalized cache headers.
- Admin thread moderation ignores submitted board for target validation, logs forged board text for most actions, and performs a second redirect lookup after committing.

The user confirmed preserving personal Pin/Unpin and auditing global Sticky separately. Hide remains personal listing visibility; explicit thread/search/archive navigation remains available. No individual reply hiding, account preferences, thread move operation, SPA, third-party popup package, or schema redesign is introduced.



## 1–2. Menu findings and polish

The original menu could clip inside a card or transformed ancestor, overflow a narrow viewport horizontally, detach after layout changes, and retain stale open state through focus changes or history. It also scanned every menu when closing or repositioning. The controller now tracks one open menu, temporarily moves it to the document body, measures its width in viewport space, clamps both axes to the visual viewport, flips above when appropriate, and limits scrolling only when space requires it. Scroll/resize work is coalesced through one animation frame; one shared resize observer runs only while a menu is open to follow header, text-size, media, and page-layout changes. Closing restores its original DOM position and clears transient state.

The trigger remains a click/tap disclosure. Minimum popup width remains 150px, ordinary rows retain 6px/8px padding, and touch rows retain 38px minimum height with 9px/10px padding. Border, background, radius, hover, active, focus, and open states use existing theme tokens. The trigger stays visible when keyboard focus moves into the portaled menu. A scoped text rule prevents generic theme button styling from dimming menu labels; translucent panel tokens are painted over an opaque theme base so underlying card text/media cannot show through. Index and thread pages now reuse the catalog's actions and authoritative labels. Existing per-post report/edit/delete controls and native fallback forms remain.

## 3–4. Hide architecture and fixes

Hide is a personal **thread listing preference**, stored in SQLite, not moderation and not localStorage. It hides the OP and its reply previews together from active board index and catalog. Direct thread links, explicit search, archives, feeds, and read-only API navigation remain available. No per-reply hiding or thread moving subsystem exists.

The previous 200-thread cutoff and filtering after fetching caused inaccessible older preferences and inconsistent counts. Both active listings now filter hidden threads in SQL before pagination and aggregate only selected threads. The index count uses the same renderable-OP filter. Catalog and Hidden management read all applicable active threads. Hiding several threads, new replies, pin/sticky/lock changes, reloads, and multiple tabs remain consistent. A direct hidden thread explains the listing preference and offers Unhide. Archived threads retain valid preferences and permit clearing them from their direct page; new Pin/Hide actions are disabled and rejected. Deleting a target cascades preferences. Clearing both flags removes the default row. Existing Boolean-domain triggers remain the malformed-state boundary.

## 5–6. Personal pin and moderator Sticky

The user explicitly confirmed preserving personal Pin/Unpin. These actions update `user_thread_preferences.pinned` for the viewer; they never change `threads.sticky` or another viewer's preferences. Anonymous visitors can use personal pinning after normal CSRF, board-view, and target validation.

Moderator-wide Sticky/Unsticky remains a separate admin operation backed by the canonical `threads.sticky` field. There are no ordinary registered-user accounts or scoped moderator roles; all administrator accounts have instance-wide moderation rights. Anonymous/non-admin requests cannot perform global moderation. Existing POST-only routes and session-scoped admin CSRF remain enforced.

Active index and catalog default order is **personal pin descending, global sticky descending, bump time descending, thread ID descending**. The filter/order occurs before LIMIT/OFFSET, avoiding repeated pins across pages. Catalog alternative sorts retain pin/sticky precedence and now use thread ID as a deterministic final tie breaker. Unpin returns a thread to its global sticky/bump position. Archive order remains the existing bump-descending/ID-ascending rule; search, feeds, and APIs retain their established non-personal semantics.

## 7. Report findings and improvements

Report targets are validated as a complete board/thread/post tuple; stored report metadata and redirect IDs come from the actual post. Thread actions submit the OP ID, which can differ from the thread ID, and reply controls submit the reply ID. Anonymous reporting remains supported; board-view restrictions and bans still apply.

Reasons remain optional, trim whitespace, and retain the existing 256-Unicode-character truncation behavior. Controls are rejected; native form parsing and a 65,536-byte route limit bound request decoding. Moderator rendering escapes reasons, including script-like text and non-Latin input. Open duplicates remain idempotent, with a distinct “matching report is already open” confirmation instead of claiming a new report. Resolution remains admin-only and permits a later new report; target deletion cascades report rows.

A dedicated budget permits 20 new reports per reporter in a rolling hour, independently of browsing rate-limit settings. Budget checking and insertion share an immediate transaction; an existing duplicate is accepted without consuming another slot. Exceeding the budget returns a themed 429 and `Retry-After: 3600`. The existing reporter identity/retention model is preserved.

## 8. Authorization and security

Found a menu-action CSRF gap: the general public validator accepted a correctly signed token generated for a different visitor. Report and preference requests now require cookie binding when a CSRF cookie exists and reject cross-site fetch metadata and mismatched Origin/Referer. Cookie-disabled forms require same-origin evidence. The site's HTML no-referrer policy can make legitimate native POSTs send `Origin: null`; those are accepted only with a bound cookie or same-origin fetch metadata, after rejecting cross-site metadata. Regression tests exercise foreign-token relay, explicit foreign origin, missing evidence, privacy forms, raw CSRF, and admin scope separation.

Boards are validated completely rather than lossily sanitized; preference actions use a closed enum; IDs must be positive. Protected-board checks and target checks occur under the same write transaction as mutations. Global moderation validates submitted board against the canonical target board before changing anything, logging, or redirecting. A missing/broken board lookup now fails closed rather than committing with a fallback board. Return paths use the existing strict internal-path validator. No menu visibility assumption is an authorization boundary.

## 9. Database, queries, and cache

No dependency or preference-schema redesign was needed. Existing preference composite keys, thread/OP indexes, cascade foreign keys, and Boolean-domain triggers remain. Added `idx_reports_reporter_created(reporter_hash, created_at)` through the existing idempotent startup schema initialization, including existing databases.

An actual browser database's EXPLAIN QUERY PLAN shows active thread/OP indexes and the composite preference key, rather than per-thread preference queries. Personal ordering needs a temporary sort over the board's active rows; adding another global thread index cannot eliminate ordering by a different viewer's preference. The report budget uses its new covering index. Foreign-key checking returned no violations. Evidence is in `output/post-actions-query-plans.log`.

Index, catalog, and thread ETags now include personal flags and relevant authoritative metadata; catalog signatures include lock, OP content/media, reply counts, and timestamps. Personalized HTML is explicitly private, with revalidation when activity tracking is disabled and no-store when enabled. Hidden management is private/no-store and varies on Cookie. There is no separate action-state cache or optimistic client state requiring rollback.

## 10. Concurrency, consistency, and failures

Preference validation, flag updates, and default-row cleanup commit together in an immediate transaction. Reports serialize validation, the abuse budget, and insertion. Global moderation keeps target validation, state, pruning intent where applicable, and canonical audit logging in one immediate transaction, then redirects using the board already resolved inside it.

Board/thread render loads use read snapshots so counts, posts, flags, and preferences agree; catalog/Hidden reads also recheck board access in their snapshots. Simultaneous pin/hide preserves both flags; concurrent duplicate reports yield one row; deletion races leave no preference/report references. Concurrent Sticky/Unsticky matches the last committed audit record. Repeated inverse/set operations remain idempotent. Archive rejects new personal flags and contradictory lock/unlock/sticky changes while permitting inverse cleanup.

Failures return existing themed 400/403/404/409/500 responses or the explicit report-budget 429. Native forms navigate to authoritative results; no optimistic disappearance or false success state is introduced. Existing DB contention/error logging, session expiry, post ownership, destructive confirmation, and durable file cleanup mechanisms remain in place.

## 11. Accessibility

Triggers retain “Thread actions” accessible names and add a menu relationship, expanded state, and controlled ID. Action items have menu semantics. Arrow keys, Home, and End move through enabled actions; Escape closes and restores focus; Tab closes and follows native focus order; outside focus/click closes. Report opening records the original trigger and its existing dialog returns focus there. Disabled archived actions are skipped. One-open behavior and pagehide/pageshow cleanup prevent stale disclosures. Native no-JS details/forms, keyboard submission, and existing report controls remain usable.

## 12. Themes and responsive/browser findings

All nine built-in themes are exercised: forest, blue-sky, deep-orbit, terminal, dorfic, chanclassic, aero, neoncubicle, and fluorogrid. Menu, report input/confirmation, and hidden-management action contrast checks require at least 4.5:1. Screenshots cover 320px and 1280px in every theme; viewport geometry covers 320, 360, 375, 390, 430, 480, 768, 1280, and 1920px, all four edges, clipping ancestors, attached media, short/long content, keyboard interactions, history, and enlarged text. Additional tests use a 900×900 image, 60 replies, first/middle/final post report targets, nested scrolling, actual thread-update insertion, removed triggers, and reduced-motion emulation. The enlarged-text checks caught and fixed initial measurement and late header-layout movement.

The project's Chromium, WebKit, Firefox, mobile Firefox, mobile WebKit, Firefox no-JS, and Chromium no-JS configurations exercise native persistence/report/moderation paths, including Hide from index/catalog/thread, Back/Forward after Hide, and admin toolbar Sticky/Unsticky. JS-only geometry tests skip the two no-JS configurations intentionally; native workflows run there. Enlarged-text tests check both an already-open menu following a growing fixed header and reopening after layout settles; a Firefox repeat run passed three times. A mobile WebKit geometry/history repeat run also passed three times. Browser checks wait for the existing activity-page refresh after history restoration, then verify the authoritative state on the listing, direct thread, or Hidden management page actually restored by the browser. Screenshots and browser diagnostics are kept under ignored `output/playwright/` directories.

## 13. Tests and technical debt

Added Rust HTTP/database regressions in `src/handlers/board/action_tests.rs` for explicit setters, isolation, forged boards/IDs/actions/CSRF, protected boards, GET rejection, 205-thread ordering/pagination, multiple hidden targets and new replies, direct navigation, archive cleanup, constraints/cascades, duplicate and concurrent reports, Unicode bounds, oversized bodies, report budgets, token binding, and deletion races. Added admin regressions for canonical logging, repeated and concurrent Sticky/Unsticky, forged board/session/CSRF, archive safety, missing targets, and lookup rollback. Existing cache/report tests were updated to the new contracts, and a summary-fragment regression checks balanced containers. Browser tests require exactly one action owner per index thread.

Added the local `tests/e2e/post-actions.spec.ts` and adapted existing local action tests for portaled menu lookup and menuitem semantics. This repository intentionally ignores Playwright tooling; those files remain local and are not force-added. Removed the verified-unused Rust catalog split/sort helper, old menu bounds helper, and all-menu closing/reposition scans. No unrelated cleanup, commits, branches, or dependency changes were made.

## 14. Validation results

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --workspace --all-targets --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed, no lint exceptions added |
| `RUST_TEST_THREADS=1 cargo test --workspace --all-features` | Passed: 1,395 tests in each library/binary target, 5 CLI integration tests, 1 fixture integration test, and 1 doc test; 7 existing ignores in each unit-test target |
| `cargo test --workspace --all-features action_tests` | 10 passed in each unit-test target |
| `cargo test --workspace --all-features server::handlers::admin::content::tests` | 15 passed in each unit-test target |
| `cargo test --workspace --all-features server::server::assets::tests` | 4 passed in each unit-test target after the final CSS change |
| `cargo build --workspace --all-features` | Passed |
| Playwright action matrix described below | Passed: 43 cases, 20 intentional configuration skips; 7 browser configurations and all 9 built-in themes |
| `git diff --check` | Passed |
| `node --check static/main.js` | Passed |
| `python3 tools/check_local_tooling.py` | Passed; local browser tooling remains untracked |

Browser command: `RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/post-actions-verified npx playwright test tests/e2e/post-actions.spec.ts tests/e2e/audit-surfaces.spec.ts tests/e2e/audit-nojs-parity.spec.ts --grep 'personal pin/hide|menus follow|menu supports|all built-in themes|catalog.*(report|pin|hide)|public report|poll creation' --workers=2`.

The 20 browser skips comprise 12 existing no-JS-parity test gates on other JavaScript projects, 6 JavaScript-only geometry/theme cases on the two no-JS projects, and 2 existing surface-test gates on Chromium no-JS. The new native persistence/report/moderation workflow passes in all seven configurations, including both no-JS projects.

Rust logs are under `output/post-actions-*.log`; the complete serial run is `output/post-actions-tests-serial.log`. Browser results, screenshots, and HTML report are under `output/playwright/post-actions-verified/`, with the command log at `output/post-actions-browser-verified.log`. The earlier default parallel runs exposed the pre-existing shared media fault-injection race noted below; a full earlier parallel pass and isolated PDF test also passed. The final serial command runs every non-ignored test without skipping that test, and concurrency regressions still explicitly issue concurrent operations within their test bodies.

## 15. Deliberately preserved boundaries and remaining limits

- Personal preferences retain the existing IP/Tor identity model. Public users sharing an identity/IP can share preferences and a report budget; clearing public-IP browser cookies does not clear SQLite state. Migrating to account/browser-only identity would be a separate product change.
- Hide remains an active-listing preference. It does not censor direct links, search, archives, feeds, API access, individual replies, or moderation records. Valid archived preferences are retained and can be cleared explicitly.
- Report reason truncation remains compatible with existing behavior. The abuse budget counts retained report rows; resolution still counts for the hour, while deletion of target/report records can release that count. No extra historical identity ledger was introduced.
- Existing non-menu posting/voting CSRF compatibility, account/session architecture, self-action expiration, and file cleanup were not redesigned. Menu-triggered report/preference paths are hardened and privileged actions continue using session-scoped protection.
- Browser tests cover the project's desktop/mobile emulation and enlarged text. Physical device pinch-zoom, assistive-technology speech output, and transport outage injection were not manually exercised. Native POST errors and keyboard/focus semantics are covered.
- Seven pre-existing Rust tests are ignored by the repository; no ignore/lint bypass was added for this work. The default parallel runner can expose an existing shared thumbnail-fault-injection race in `primary_upload_accepts_pdf_when_board_enables_pdf`; the isolated test passes. The final complete suite is also run with `RUST_TEST_THREADS=1` to avoid that unrelated fixture race without changing media code.

## Files changed

| Area | Files |
| --- | --- |
| Database | `src/db/schema.rs`, `src/db/threads.rs` |
| Personal actions/reporting | `src/handlers/board.rs`, `src/handlers/board/access_preferences.rs`, `src/handlers/board/catalog.rs`, `src/handlers/board/pages.rs`, `src/handlers/board/create_thread.rs`, `src/handlers/board/reports.rs` |
| Rendering and moderation | `src/handlers/render.rs`, `src/handlers/thread.rs`, `src/handlers/admin/content.rs` |
| Templates and assets | `src/templates/board.rs`, `src/templates/thread.rs`, `static/main.js`, `static/style.css` |
| Rust regressions | `src/handlers/board/action_tests.rs`, `src/handlers/board/tests.rs`, `src/server/server/assets.rs` |
| Audit | `docs/post-actions-audit.md` |
| Local ignored browser files | `tests/e2e/post-actions.spec.ts`, `tests/e2e/audit-surfaces.spec.ts`, `tests/e2e/audit-nojs-parity.spec.ts` |
