# RustChan Playwright Deep Audit — 2026-09-29

Scope: the local-only Playwright E2E harness (`tests/e2e/**`, `playwright.config.ts`)
for RustChan 1.5.0. This document records what the suite proved before the audit,
what was added, what was executed, what failed, and what remains unverified.

**Historical audit record.** At audit time the harness was intentionally
untracked (`.gitignore` ignored `/playwright*.config.*`, `/package.json`,
`/package-lock.json`, `/tests/e2e/` and `/output/`), so every artifact below was
a local working-tree file and no `src/` file was changed by the audit itself.
After the audit, the maintained harness source and configuration were moved
under version control (see `tests/e2e/README.md`), while generated evidence and
the `output/` tree remain ignored. The report below is preserved as provenance
for what was executed at revision `146423b3`; it is not a description of the
current tracked layout.

The audit found three confirmed application defects (F16 ban bypass on
`/report`, F23 ban-notice overflow, F24 NSFW focus restore) and one unresolved
harness item (`audit-permissions.spec.ts` WebKit leak scan). All four were
subsequently fixed in the maintained tree: posting, report, and vote share the
centralized `ensure_actor_not_banned` gate; `.error-page p` wraps long tokens;
the NSFW dialog follows the modal focus pattern; and the WebKit leak-scan
assertion now reads wire `Set-Cookie` headers through the Node-side request
context because WebKit page `headersArray()` omits `Set-Cookie` by design.

## 0. Revision, environment, and how to reproduce

| Item | Value |
| --- | --- |
| Repository | `/Users/connordawkins/Documents/GitHub/RustChan` |
| Revision | `146423b310e1f8f7ea14eaba8b2ce9d675cbf31b` (`docker-image-setup`, "Add production container image and GHCR publishing workflow") |
| Working-tree state at start | clean (`git status --porcelain` empty); all audit changes are untracked local-only harness files |
| OS / platform | macOS (darwin), local developer machine |
| Application build | `cargo build --locked --bin rustchan-cli` → `target/debug/rustchan-cli` (debug, `CHAN_TOR_SUPPORT=0`); rebuilt at audit start to match the revision (56.8 s) |
| Node | 22.14.0 |
| Playwright | `@playwright/test` 1.63.0 (`npm ls`), browsers from `~/Library/Caches/ms-playwright` |
| Projects | `chromium`, `webkit`, `firefox`, `mobile-firefox` (emulated), `mobile-webkit` (emulated iPhone 13), `firefox-nojs` |
| Config contract | `fullyParallel: true`, `workers: 2`, `retries: 0`, traces/videos `retain-on-failure`, failure screenshots, list+HTML+JSON reporters, `RUSTCHAN_AUDIT_OUTPUT` honoured |
| Collected tests (before) | 972 executions, 205 declared tests, 47 spec files |

### Environment blocker (external, not caused by this task)

A **different agent working in the sibling repository `/Users/connordawkins/Documents/GitHub/RustPost`**
ran these commands while this audit was running:

```sh
pkill -f "node_modules/.bin/playwright test"
pkill -f "playwright/lib/worker/workerProcessEntry"
pkill -f "target/debug/rustpost-cli.*serve"
```

The first two patterns are not repository-scoped and killed this audit's Playwright
run (exit 143, SIGTERM). Baseline attempt 1 was invalidated by that kill and by the
auditor's own cleanup `pkill`; its three "failures" were all
`rustchan exited before ready ... signal SIGTERM`, `net::ERR_CONNECTION_REFUSED`,
and `Target page, context or browser has been closed` — harness-level casualties,
not application defects. Attempt 1 is preserved at
`output/playwright/deep-audit/baseline-attempt1-killed/` as evidence. Baseline
attempt 2 is the recorded baseline.

Reproduce the baseline and the expanded audit with:

```sh
# baseline (unmodified suite; the binary is already built)
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/deep-audit/baseline npm run test:e2e

# expanded audit (one command, existing output convention preserved)
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/deep-audit/expanded npm run test:e2e:deep
```

## 1. Baseline result (unmodified suite)

Recorded under `output/playwright/deep-audit/baseline/`. The suite was run per
project (the execution modes the README documents) because a single unified
972-execution process was projected at ~5 hours on this host; the unified attempt
reached 121 executions with 0 failures before being replaced, and its log is kept
at `baseline-unified-partial/`.

| Project | Executions | Passed | Failed | Skipped | Wall time |
| --- | --- | --- | --- | --- | --- |
| chromium | 162 | 129 | 0 | 33 | 1041 s |
| webkit | 162 | 91 | 0 | 71 | 931 s |
| mobile-webkit (emulated iPhone 13) | 162 | 100 | 0 | 62 | 1052 s |
| firefox | 162 | 0 | **154 (launch failure)** | 8 | 114 s |
| mobile-firefox (emulated) | 162 | 0 | **154 (launch failure)** | 8 | 114 s |
| firefox-nojs | 162 | 0 | **154 (launch failure)** | 8 | 116 s |
| **Total** | **972** | **320** | **0 application failures** | **174** | — |

**No application-level failure was found by the unmodified suite** on the engines
that ran. The 462 Firefox-project failures are an environment blocker described
below and are not application findings. Skipped tests are the documented opt-in
and manual areas (media toolchain, Tor, upload regressions, fault injection) plus
project gates.

### Environment blocker: Playwright Firefox cannot launch on this host

Every Firefox-based project fails at browser launch, before any application code
runs:

```
Error: browserType.launch: Failed to launch the browser process.
<launching> .../firefox-1543/firefox/Nightly.app/Contents/MacOS/firefox -no-remote -headless
  -profile /var/.../playwright_firefoxdev_profile-XXXX -juggler-pipe -silent
[pid=...][err] *** You are running in headless mode.
[pid=...][err] Could not find profile folder.
```

Evidence gathered:

1. Reproduced in isolation (single test, no other suite running) and directly
   from the command line with a manually created, existing profile directory.
2. Reproduced with a profile under `$HOME` and with no `-profile` argument at all
   → Firefox cannot obtain any profile, so this is not a temp-dir permission issue.
3. The build was deleted and freshly re-downloaded (`npx playwright install firefox`,
   Firefox 155.0 / playwright firefox v1543, 104.3 MiB) → identical failure.
4. The older cached build `firefox-1522` starts (it blocks waiting on the juggler
   pipe) → host is not blocking Firefox binaries generically; the v1543 build is.
5. Host: macOS 27.0 (build 26A428), arm64; the app bundle is `adhoc, linker-signed`
   with `Sealed Resources=none`.

Classification: **environment blocker**, not an application or harness defect.
Mitigation applied: a `chromium-nojs` project was added (see §4) so the
JavaScript-disabled contract still executes; `firefox-nojs` remains configured so
the blocker stays visible rather than silently deleted. The no-JS coverage that
*should* have run in `firefox-nojs` is therefore reported as executed on Chromium
instead, and Firefox-specific results are reported as blocked, never as passed.

## 2. What the existing suite actually proved

The suite is unusually strong for a project of this size: per-test isolated
runtimes, real SQLite inspection, per-request CSRF handling, and several genuinely
adversarial tests. It nevertheless contained coverage that could pass while the
feature was broken. The following were confirmed by reading the assertions (file
citations are exact).

### 2.1 Assertions that pass when the feature is broken

| # | Location | Assertion | Why it can pass while broken |
| --- | --- | --- | --- |
| W1 | `maintenance-backup-phase2.spec.ts:60,64` | `expect(['finished','failed','stale']).toContain(state)` and `/Maintenance completed\|Repair was not run\|maintenance rebuild failed/i` | `failed` and "maintenance rebuild failed" are explicitly accepted, so a repair that always fails passes its own test. The fault-injection case at `:153-158` is permanently `test.skip(true, ...)`. |
| W2 | `upload-validation.spec.ts:968-973` with `:871-877` | reject scenarios guarded by `if (![400,403,413,415,422].includes(result.status ?? 0) && result.classification === 'success') throw` | A transport error yields `status === undefined` and a non-`success` classification, so the guard is skipped: a timeout or server crash counts as a clean rejection. |
| W3 | `admin-polish.spec.ts:394-396` | `outlineStyle !== 'none' && outlineWidth > 0 \|\| boxShadow !== 'none' \|\| borderColor !== ''` | `getComputedStyle().borderColor` is never the empty string, so the disjunction is always true. No focus indicator is ever verified. (`phase4-helpers.ts:120-125` does it correctly.) |
| W4 | `board-settings-phase2.spec.ts:63-91` | Browser "save settings" then `postBoardSettings(...)` API re-post, then DB equality | The DB assertion is satisfied by the API call; a silently failed browser save still passes. |
| W5 | `posting-thread.spec.ts:149` | `expect(other.locator('body')).toContainText(/not allowed\|expired\|forbidden\|edit/i)` | The `edit` alternative matches the page's own "Edit" UI text, so a wrongly authorised edit form passes. The test also never attempts the state-changing POST from the other context. |
| W6 | `moderation-phase2.spec.ts:45` | `expect(body).toContainText(/reported\|reply selected for report lifecycle/i)` | The second alternative is the reply body text that exists regardless of whether the report banner rendered. |
| W7 | `phase4-empty-error-permission.spec.ts:196,217`; `backup-restore.spec.ts:345-348` | `expect([200,303,400]).toContain(invalidRestore.status())` / `[200,302,303,400]` | A malformed restore that is *accepted* (200/302/303) passes. Only the follow-up usability check provides any signal. |
| W8 | `posting-thread.spec.ts:119`; `settings.spec.ts:96` | `expect([303,403,422])` / `expect([403,404,409,422])` | Success and multiple rejection classes are treated as equivalent; the mutation may have landed. |
| W9 | `phase4-stress-release-smoke.spec.ts:131,142` | `body` contains `/results\|busy/i`, `/site health\|boards\|moderation/i` | These words appear in permanent navigation/labels, so a broken search or empty dashboard passes. |
| W10 | `admin-polish.spec.ts:80-81` | click "check database health" then `body` matches `/database health\|integrity/i` | The clicked button's own label satisfies the assertion. |

### 2.2 Coverage that bypasses the feature under test

| # | Location | Bypass |
| --- | --- | --- |
| B1 | `mobile-nojs.spec.ts:61-71,81-93` | A test titled "…thread creation, admin login, settings POST… without JavaScript" performs the reply and the site-settings save with `page.request.post`, so the no-JavaScript form paths it names are never driven. |
| B2 | `backup-restore.spec.ts:640-735`; `assets-themes-phase2.spec.ts:28-236`; `theme-system.spec.ts:69-243`; `admin-settings-phase2.spec.ts:77-129`; `moderation-phase2.spec.ts:88-192` | Backup/restore, favicon/banner/theme CRUD, invalid-value handling and moderation actions are performed through the same HTTP endpoints the forms post to, so form controls, confirmation modals, redirects and flash feedback are never exercised. |
| B3 | `board-settings-phase2.spec.ts:63-91` | See W4. |
| B4 | `activity-notifications.spec.ts:120,304` | The "admin toggles" test writes badge settings straight into SQLite (`setSiteFixtureSettings`), so the admin toggle UI it is named after is never used. |
| B5 | `theme-history.spec.ts:8-11` | BFCache restore is simulated by dispatching a synthetic `pageshow` event instead of performing real Back/Forward navigation. |

### 2.3 Harness defects

| # | Location | Defect |
| --- | --- | --- |
| H1 | `auth-admin.spec.ts:115`, `csrf-navigation.spec.ts:38,57`, `password-boards.spec.ts:69,215`, `release-audit.spec.ts:51` | Manually created contexts did not inherit `javaScriptEnabled: false`, so under the `firefox-nojs` project these extra contexts ran **with JavaScript enabled**. The no-JS project therefore reported coverage it did not have. |
| H2 | `helpers.ts:503-508` and `release-tls.spec.ts:61` | `waitForURL(/\/admin/)` also matches `/admin/panel`, the pre-logout URL, so the assertion can resolve before logout happens; it proves nothing on its own. |
| H3 | `diagnostics.ts:16-23` | Console errors and **all** 4xx responses were collected but never asserted; only uncaught exceptions and 5xx failed a test. A page could return 404 for every asset and still pass. |
| H4 | `phase4-helpers.ts:83-84` | `watchClientErrors` filters out `Failed to load resource … status of 4(00\|03\|04\|10\|13\|15\|22)` — a broad 4xx allowlist embedded in a shared helper. |
| H5 | `helpers.ts:368-370` | `logs()` swallowed read errors into `''`, so a missing log looked like an application that logged nothing. |
| H6 | `playwright.config.ts` (as supplied) | No project-level deep-diagnostics mode, no success-journey artifacts, and no way to retain server-side evidence for successful journeys. |
| H7 | `helpers.ts:1034-1050` | Readiness only checked `/readyz` for HTTP 200; it did not confirm the responding server belonged to this fixture root, so a stale process on the reserved port could have been adopted. |

### 2.4 Application observability gap found while building diagnostics

RustChan sets `x-request-id` on every response (`src/server/server/lifecycle.rs:10-31`)
and creates an INFO span per request, but **no request line is logged at the
default level**. The `tower_http` trace layer writes to
`rustchan-data/logs/dep_log.log` and its event message omits method, path, and the
request id (observed: `[TRACE] [on_respo] Finished processing request - latency: 1 ms, status: 200`),
and the span fields are not included by the custom formatter. Browser evidence can
therefore only be joined to server logs by timestamp plus the response
`x-request-id` header, not by a server-side identifier. See finding F5.

## 3. Feature families that do not exist (do not invent them)

Verified against `src/db/schema.rs`, `src/server/server/routes.rs` and `src/handlers/`:

- **Public registration / user accounts / non-admin login / profiles** — only
  `admin_users` and `admin_sessions` exist. Anonymous identity is the
  `rustchan_visitor_id` cookie; own-post authority is a `rustchan_owned_posts`
  grant.
- **Following / protected follow requests / blocking / muting users** — no routes,
  schema, or templates. Thread preference actions are exactly `pin|unpin|hide|unhide`.
- **Notification inbox** — none. What exists are ephemeral "new activity" badge
  cookies.
- **User-facing import/export** — none. Backup archives are operator-only.
  `chan_net_posts` / `chan_net_import_ledger` are dead legacy tables with no
  routes or handlers.

Prompt-listed families that were therefore *not* tested, with the reason recorded
rather than invented: registration, profiles, follow/block/mute, notification
inbox, and user-facing import/export.

## 4. Changes made to the harness

No `src/` file was modified: production behaviour is unchanged. Everything below
lives in the gitignored local harness.

### 4.1 Strict diagnostics (`tests/e2e/diagnostics.ts`)

Replaced the permissive observer with the strict layer described in
`tests/e2e/README.md § Diagnostics`: structured redacted records carrying actor,
project, test, worker, timestamp, method, path, status, content type, `Location`
and `x-request-id`; failures for undeclared page errors, console errors, 4xx/5xx
page responses, non-aborted transport failures and wrong asset content types;
per-test, consumed-once declarations that fail when they never match; and
correlation that treats a browser console message as explained when it refers to
an already-declared HTTP error response (Chromium logs
"Failed to load resource: … status of 404" for 404 *document* navigations too, so
this avoids double declarations without loosening the rule).

### 4.2 Server-log evidence (`tests/e2e/helpers.ts`)

- `logSize()` / `logsSince(offset)` / `dependencyLogs()`; the fixture captures a
  log offset before each test and attaches the delta always, plus the full server
  log and dependency log on failure.
- New `[ERROR]` lines in the server log now fail the test unless declared with
  `expectServerLogError({ pattern, reason })`; unused declarations fail too.
- `logs()` no longer swallows read errors into an empty string.
- Deep mode (`RUSTCHAN_E2E_DEEP_DIAGNOSTICS=1`) injects
  `RUST_LOG=info,tower_http::trace=trace` so request status/latency lines land in
  the runtime dependency log. `RUST_LOG` is only injected in deep mode, because
  an empty value would change the default filter and hide startup lines.
- `adminLogout` now waits for the exact `/admin` login path and asserts the login
  form; the previous `/\/admin/` pattern also matched the pre-logout
  `/admin/panel` URL and could resolve before logout happened.
- New `expectAdminPanel(page)` asserts an admin action really rendered the panel
  instead of silently redirecting to the login form.
- `serverLogExpectations` is a per-test registry initialised by the auto fixture
  before the test body.

### 4.3 Configuration (`playwright.config.ts`)

Unchanged contract verified: six projects, `fullyParallel: true`, `workers: 2`,
`retries: 0`, traces/videos `retain-on-failure`, failure screenshots, list/HTML/JSON
reporters, `RUSTCHAN_AUDIT_OUTPUT`. One project was **added** (nothing removed):

```ts
{ name: 'chromium-nojs', use: { ...devices['Desktop Chrome'], javaScriptEnabled: false } }
```

Justification: the required `firefox-nojs` project cannot launch on this host
(§1), so without this addition the JavaScript-disabled contract would have zero
executable coverage. `firefox-nojs` stays configured and its blocker is reported
explicitly.

### 4.3a Integration fixes required by the new no-JS project

- `NO_JS_PROJECTS` in `diagnostics.ts` now lists both `firefox-nojs` and
  `chromium-nojs`, so `newAuditedContext`/`newAuditedPage` inherit the
  no-JavaScript mode in both.
- A shared `isNoJsProject(testInfo)` predicate in `helpers.ts` replaced 15 specs'
  literal `firefox-nojs` name checks; no-JS-oriented project lists gained
  `chromium-nojs`; the two deliberately Firefox-only gates
  (`firefox-nojs-public.spec.ts` and the Firefox-labelled setup no-JS branch) are
  documented exceptions.
- `isNavigationCancel()` recognises Chromium `net::ERR_ABORTED`, WebKit
  `cancelled`, and Firefox `NS_BINDING_ABORTED`.
- Focus-affordance helpers enter keyboard modality before focusing, because
  `:focus-visible` does not match after a pointer interaction.
- Process caution recorded: the sweep initially collided with a local
  `isNoJsProject` helper inside `admin-polish.spec.ts`, producing a self-recursive
  duplicate declaration. Collection (`npx playwright test --list`) caught it
  before the run; the local helper was removed so the shared predicate is the
  single source of truth. Always re-run `--list` after a shared-symbol sweep.

### 4.4 Shared helpers (`tests/e2e/phase4-helpers.ts`)

`watchClientErrors` no longer filters `Failed to load resource … status of 4(00|03|04|10|13|15|22)`.
That 4xx allowlist hid missing assets; expected console failures must now be
declared with a reason.

### 4.5 Strengthened assertions in existing specs

| Change | File | Effect |
| --- | --- | --- |
| S1 | `admin-polish.spec.ts` | Focus affordance now compares focused vs unfocused computed style. The old disjunction included `borderColor !== ''`, which is always true. |
| S2 | `maintenance-backup-phase2.spec.ts` | A fresh, intact database must report `finished` + "Maintenance completed"; `failed` / "maintenance rebuild failed" are no longer accepted as success. |
| S3 | `upload-validation.spec.ts` | A reject scenario that produced no HTTP response now fails instead of skipping every guard; a declared-expansion scenario asserts the preview is visible instead of silently skipping. |
| S4 | `board-settings-phase2.spec.ts` | Board settings are asserted immediately after the browser save, before the API re-post that previously masked a broken form. |
| S5 | `moderation-phase2.spec.ts` | Report assertion targets the outcome banner instead of a regex whose alternative was the pre-existing reply body. |
| S6 | `posting-thread.spec.ts` | Stale/duplicate reply now asserts the exact outcome and the persisted row count instead of accepting `[303, 403, 422]` equally. |
| H1 | `diagnostics.ts` | Manual contexts now inherit the project JavaScript mode, so the no-JS projects no longer create JavaScript-enabled contexts. |
| H4 | `phase4-helpers.ts` | Broad 4xx console allowlist removed. |

### 4.6 New diagnostics tooling

`expectHttpError`, `expectConsoleError`, `expectPageError`,
`expectTransportFailure`, `expectServerLogError`, `actor()`, `noteEvent()`,
`measureStep()`, `journeyStep()`, `diagnosticRecords()`, `declarationStatus()`
(exported so the matcher is unit-testable without fixture teardown ordering),
`redact()`, `javaScriptEnabledFor()`, and the bounded deep-artifact budget
(`RUSTCHAN_E2E_DEEP_DIAGNOSTICS_MAX`, default 400).

### 4.7 Execution scripts (`package.json`)

Added `test:e2e:deep` (one-command expanded pass, existing reporting contract),
`test:e2e:harness`, `test:e2e:audit`, `test:e2e:audit:nojs`, `test:e2e:perf`.
Existing scripts are unchanged.

## 5. New coverage added

Seven new spec files, 595 collected executions across the seven projects (each
test is gated to the projects where it adds information; the executed subset is
reported in §7):

| File | Tests | Project applicability |
| --- | --- | --- |
| `audit-harness-contract.spec.ts` | 8 | chromium (1 test on `firefox-nojs`, blocked on this host) |
| `audit-admin-ui.spec.ts` | 10 | chromium |
| `audit-journeys.spec.ts` | 13 | chromium (1 no-JS poll test on the no-JS projects) |
| `audit-permissions.spec.ts` | 19 | all projects |
| `audit-nojs-parity.spec.ts` | 14 | chromium, `chromium-nojs` (Firefox blocked) |
| `audit-ui-a11y.spec.ts` | 13 | chromium + `mobile-webkit` for the narrow profile |
| `audit-performance.spec.ts` | 8 | capability probe on JS-enabled projects, 7 measurements on chromium |

What the new tests actually assert (all browser-driven; SQLite only corroborates):

- **Harness contract**: undeclared 4xx / console error / uncaught page error fail
  the test (proved with `test.fail()`), declared error surfaces are consumed once,
  the matcher is unit-tested, manual contexts inherit the project JavaScript mode,
  and the fixture server belongs to this run.
- **Admin UI**: favicon upload through the real file input (512×512 contract) with
  served bytes, content type, versioned `<link rel=icon>` set, board override and
  fallback; VACUUM through the confirm modal with its result page and intact data;
  home-banner upload through a real 468×60 file input, homepage rendering with a
  decode check, ordering, the enabled toggle, and deletion; external-banner
  interstitial and Continue to the declared target; word filters added and removed
  with a live post rewritten then left alone; ban add / appeal dismiss / ban lift
  through the rendered forms with the banned identity refused at posting time and
  restored afterwards; admin thread and post delete through the real modals;
  extract-board download (real `download` event) and board restore onto a second
  instance with the unrelated board untouched; site-health payload and CSRF.
- **Journeys**: multi-actor media thread lifecycle with counters, edit visibility
  from both actors and `edited_at`; preference/theme persistence across reload,
  navigation, server restart and an unrelated context; board settings visible to
  the public and to a fresh context with control gating; search form and
  pagination; NSFW consent; polls including the `noscript` rows and duplicate
  votes; sage/bump; boundary states.
- **Permissions**: owner-vs-other edit/delete with crafted posts, forged grants and
  tampered ids; a CSRF matrix; 45 unauthenticated admin mutations with a state
  snapshot; stale-session reuse after logout; board access-mode leak matrix;
  bans/appeals; hostile content and secret/path exposure scans.
- **No-JS parity**: the plain-form contract (thread/reply, edit/delete,
  report, pin/hide, poll, preferences, theme, admin login and settings,
  moderation), validation-failure recovery without losing other field values,
  Back/Forward/refresh, and JS/no-JS business-outcome parity with a
  one-mutation-request assertion.
- **UI/a11y**: 320/390/768/1280 viewport matrix with overflow, clipping,
  covered-centre, scroll-trap and off-screen-dialog checks; keyboard tab order,
  focus visibility, Enter/Space/Escape, modal focus containment and restore;
  accessible names per page and error-to-field association; representative
  logged-out/logged-in/empty/populated/long-content/error/NSFW-declined/banned
  states.
- **Performance (bounded diagnostics, not a load test)**: repeated samples
  (≥5) of navigation, posting, media and admin workflows with Navigation/Resource
  Timing where the engine exposes them, request counts with a duplicate-request
  check, media decode, transfer-size inventory, and observable server CPU/RSS
  samples, all attached as evidence with sample counts and stated limitations.

## 6. Before/after coverage matrix

"Browser-driven" means the feature was exercised through real browser interaction.
"API-only" means the same endpoint was called with `page.request`/`request`, which
does not exercise form controls, redirects, confirmation modals, or client-side
feedback. Rows marked *blocked* could not run on this host.

| Feature | Before | After (new coverage) | Remaining gap |
| --- | --- | --- | --- |
| Multi-actor thread lifecycle with media (create/view/reply/edit/delete, two actors) | Partial (`posting-thread.spec.ts` owner-only; weak denial text at `:149`) | `audit-journeys.spec.ts`: owner + unrelated actor, media element + `naturalWidth`, reply counter, edit/delete visibility from both actors, DB corroboration | Delete *another* actor's post remains impossible by design |
| Own-post edit/delete authorisation (crafted requests) | GET-only, weak text match | `audit-permissions.spec.ts`: GET+POST with a valid CSRF, exact denial, DB unchanged, forged `deletion_token`, negative/huge ids | — |
| Preference/theme persistence across restart | `settings.spec.ts` via API; `theme-system` via JS global | `audit-journeys.spec.ts`: real UI, navigate + reload + `app.restart()` + unrelated fresh context negative | — |
| Board settings visibility (name/description/NSFW/toggles) | UI save overwritten by an API re-post (W4) | S4 immediate assertion + `audit-journeys.spec.ts` public/catalog/home/fresh-context checks and control gating | — |
| Search form and pagination | URL navigation only | `audit-journeys.spec.ts`: real GET form, unique token, whitespace/unicode query, `page=2` uniqueness, out-of-range | — |
| NSFW consent gate | none | `audit-journeys.spec.ts`: gate → real accept → content → reload/navigation; other boards unaffected | Homepage-only enforcement (finding F2) |
| Polls (create 3 options, vote, duplicate vote, cross-actor) | single happy path (`audit-surfaces`) | `audit-journeys.spec.ts` incl. `noscript` option rows; `audit-permissions.spec.ts` CSRF/zero/huge/cross-board option ids | Poll cleanup/prune still untested |
| Sage / bump semantics | API-only (`settings.spec.ts`) | `audit-journeys.spec.ts`: real checkbox, `bumped_at` unchanged, reply_count advanced | — |
| Boundary states (empty/one item/over-limit/whitespace/unicode/long unbroken/deleted) | partial | `audit-journeys.spec.ts` | — |
| Admin ban add / appeal / ban remove | ban add API-only, remove and appeal-dismiss missing | `audit-admin-ui` → `audit-admin-ui.spec.ts`: real forms, banned actor's view, one appeal row, dismiss keeps the ban, lift restores posting | — |
| Admin thread/post delete via UI | thread delete cancel only; post delete API-only | `audit-admin-ui.spec.ts`: real modal confirm, DB row gone, survivors intact | — |
| Word filters | rows seeded via SQL, UI inspected only | `audit-admin-ui.spec.ts`: real add form → live post rewritten → real remove → new post unmodified; stored body unchanged | Regex/edge replacement semantics |
| Favicon upload (global + board override + clear) | API-only | `audit-admin-ui.spec.ts`: real file inputs, served bytes and content type, versioned `<link rel=icon>` set, board scope then fallback | — |
| Home banner upload/delete/order/enabled/target | upload + delete API-only; home-banner route untested | `audit-admin-ui.spec.ts`: real 468×60 file input, homepage rendering with decode check, move/enabled effects, target picker | Global board-banner rotation timing |
| External banner interstitial + Continue | none | `audit-admin-ui.spec.ts`: real settings toggle, upload+target, warning page, Continue → declared target | `allow_external_links` off-path via UI |
| VACUUM | none | `audit-admin-ui.spec.ts`: real confirm modal, row count preserved, `quick_check`, site still serving | Long-running vacuum interruption |
| Extract board from full backup + restore into another instance | none | `audit-admin-ui.spec.ts`: real JS progress modal + done signal, download event, board restore on a separate instance, unrelated board untouched | Split-ZIP part-size UI |
| Backup create/restore UI | API-only (B2) | `audit-admin-ui.spec.ts` drives the real progress modal and restore upload; `audit-journeys`-style list refresh | Full-site restore-upload via UI |
| Admin/anonymous permission matrix (45 routes) | scattered single-route checks | `audit-permissions.spec.ts`: every mutation route unauthenticated → exact denial + state snapshot unchanged | — |
| CSRF matrix | partial | `audit-permissions.spec.ts`: missing/empty/wrong/cross-session/cross-actor/raw-cookie/signed-scope, admin↔public token confusion, Origin/Referer/null/foreign, GET on POST-only = 405 | — |
| Session invalidation after logout | `release-tls` + `csrf-navigation` via request | `audit-permissions.spec.ts`: restored `storageState` session reuse + sibling-tab case | Session *expiry* (no time-based test) |
| Tampered identifiers/hidden fields | sparse | `audit-permissions.spec.ts`: board/post/thread/report/poll swaps, forged grants | — |
| Board access modes (view/post password, unlock scoping, rotation) | strong | `audit-permissions.spec.ts` adds leak matrix incl. direct media and `/updates`, cookie board-scoping, rotation | — |
| Banned actor lifecycle | partial | `audit-permissions.spec.ts` + `audit-admin-ui.spec.ts` | Ban appeal rate limit (24 h) |
| Hostile content (XSS, hostile filename, secrets/paths in HTML) | partial | `audit-permissions.spec.ts`: marker payloads, js:/data: URIs, quote/backtick, hostile filename, secret/path/SQL scans, cookie attributes | — |
| No-JavaScript plain-form contract | `firefox-nojs-public`, `mobile-nojs`, `release-nojs-catalog` (reply/settings via API in `mobile-nojs`) | `audit-nojs-parity.spec.ts` (14 tests on Chromium + `chromium-nojs`, Firefox blocked) + `audit-journeys` noscript poll | Firefox-engine no-JS blocked; WebKit no-JS excluded by design |
| JS vs no-JS business-outcome parity and duplicate-request check | none | `audit-nojs-parity.spec.ts` | — |
| Keyboard/focus/modal focus/accessible names/error association | `phase4-accessibility-progressive` basics | `audit-ui-a11y.spec.ts`: tab walk, Enter/Space/Escape, modal containment + focus restore, `expectNamedInteractiveControls` per page, error-to-field association | Screen-reader output not observable |
| Responsive/viewport matrix incl. tablet | partial (mobile + narrow) | `audit-ui-a11y.spec.ts`: 320/390/768/1280 with overflow, clipping, covered-centre, scroll-trap, off-screen-dialog checks | Emulated profiles only |
| Loading/error/empty/NSFW-declined/banned states | partial | `audit-ui-a11y.spec.ts` | — |
| Bounded performance/lifecycle evidence | none | `audit-performance.spec.ts`: ≥5 samples per scenario, Navigation/Resource Timing where exposed, request counts, duplicate-request check, media decode, `ps` CPU/RSS samples, transfer-size inventory | No load testing; no leak claims |
| Harness self-verification | none | `audit-harness-contract.spec.ts`: 7 tests proving the strict layer fails closed and that manual contexts inherit the JS mode | — |
| Admin site-health dismiss | page view only | `audit-admin-ui.spec.ts` asserts payload + CSRF presence | Producing a failed job needs fault injection → not executed |
| Tor active/onion UI, public ACME trust | manual/opt-in (unchanged) | not attempted | Documented manual areas |
| Firefox/WebKit engine matrix for the new suites | — | new audit suites are gated to Chromium (`chromium-nojs` for the no-JS file, `mobile-webkit` for the narrow profile) | Firefox projects blocked; WebKit full matrix for new suites deliberately excluded to bound runtime |

## 7. Findings

### 7.0 Result of the new and strengthened tests

Executed on this host with retries 0. "Collected" is the number of projects × tests
declared; "executed" counts only real runs after project gating.

| Suite | Collected | Executed | Passed | Failed | Notes |
| --- | --- | --- | --- | --- | --- |
| `audit-harness-contract.spec.ts` | 8 × 7 | 8 | 8 | 0 | chromium + `chromium-nojs` JS-mode guard; the `firefox-nojs` run is blocked |
| `audit-admin-ui.spec.ts` | 10 × 7 | 10 | 10 | 0 | chromium, serial |
| `audit-journeys.spec.ts` | 13 × 7 | 13 | 13 | 0 | 12 chromium + 1 `chromium-nojs` noscript poll |
| `audit-permissions.spec.ts` | 19 × 7 | 19 | 18 | **1** | the failure is the confirmed ban/report gap (F16) |
| `audit-nojs-parity.spec.ts` | 14 × 7 | 28 | 28 | 0 | chromium + `chromium-nojs` |
| `audit-ui-a11y.spec.ts` | 13 × 7 | 13 | 11 | **2** | the two failures are confirmed defects F23/F24 |
| `audit-performance.spec.ts` | 8 × 7 | 8 | 8 | 0 | chromium; trace/video disabled for measurement |
| **New suites total** | **595** | **99** | **96** | **3** | 3 confirmed application findings |

Strengthened existing assertions (S1–S6) are exercised by the Chromium project of
the final expanded matrix; their outcomes are recorded in §7.3. No strengthened
test was reverted and no assertion was weakened.

### 7.0a Final expanded matrix (all seven projects, one worker each, retries 0)

Recorded under `output/playwright/deep-audit/expanded/<project>/`. Each project ran
the whole suite for that project; `expected` includes the three `test.fail()`
meta-tests, which the list reporter prints as `✘` while counting them as expected.

| Project | Expected | Unexpected | Skipped | Wall-clock class |
| --- | --- | --- | --- | --- |
| chromium | 202 | 8 | 37 | complete |
| chromium-nojs | 115 | 1 | 131 | complete |
| webkit | 108 | 3 | 136 | complete |
| mobile-webkit (emulated iPhone 13) | 118 | 4 | 125 | complete |
| firefox | 0 | 239 (launch failure) | 8 | environment blocker |
| mobile-firefox (emulated) | 0 | 239 (launch failure) | 8 | environment blocker |
| firefox-nojs | 0 | 239 (launch failure) | 8 | environment blocker |
| **Total collected 1729** | **543** | **16 real + 717 blocked** | **429 + 24** | — |

Classification of the 16 failures on working engines in that run:

- **3 distinct intentional application findings** (5 executions): F16 (banned
  identity can report, all three working engines) and F23/F24 (chromium).
- **8 harness declaration gaps in pre-existing specs** that the strict layer
  exposed: `maintenance-backup-phase2:79` (intentional corrupt-restore server
  [ERROR] line), `moderation-phase2:18` (the report feedback text — the audit's
  own S5 assertion named the wrong string), `phase3-media-runtime:25` (deliberate
  missing-thumbnail 404), `phase4-accessibility-progressive:167` (deliberate
  `route.abort()` of `main.js`), `phase4-theme-contrast:93` (intentional
  empty-body 422 loops), `audit-permissions:1311` (WebKit grant-cookie visibility
  timing), `upload-validation:223` (WebKit engine console noise +
  `Frame load interrupted` classification), plus `backup-restore:107/135`
  (locked-board 403 in the representative site).
- The remaining figure differences across webkit/mobile-webkit are the same
  classes repeated per engine.

Every item in the second group was then repaired with precise declarations,
bounded polling, or engine-classification fixes — never by weakening an
assertion — and re-verified per file.

**Post-fix confirmation runs** (`output/playwright/deep-audit/fix/`):

| Run | Scope | Result |
| --- | --- | --- |
| `final-chromium` | 12 affected files, chromium, serial | 50 passed / 5 skipped / 5 failed → 3 of the 5 were the intentional findings; `phase4-accessibility-progressive` and `phase4-theme-contrast` then fixed and confirmed separately |
| `confirm-final` + `confirm-final2` | `phase4-accessibility-progressive`, `phase4-theme-contrast`, chromium | **all pass** (the 422 validation loop was declared with `times: 'any'`, the deliberately aborted `main.js` was declared as a repeated transport failure, and `watchClientErrors` now defers to the diagnostics declarations instead of duplicating a broad allowlist) |
| `final-webkit` | `phase3-media-runtime`, `audit-permissions`, `media`, webkit | 21 passed / 3 skipped / 2 failed → 1 intentional finding + the residual WebKit item below |
| `final-mobilewebkit` | `phase3-media-runtime`, `audit-permissions`, `upload-validation`, mobile-webkit | 19 passed / 3 skipped / 2 failed → same residual pair; the previous WebKit engine-noise and missing-thumbnail failures are fixed |

**Residual failures after all repairs**

1. The three intentional application findings (F16, F23, F24) — expected red, with
   reproducible commands in §7.2.
2. `audit-permissions.spec.ts:1311` on `webkit` and `mobile-webkit` only: a
   `toBeTruthy()` assertion inside the secret-leak scan still sees an undefined
   value. The ownership-grant lookup was made robust with bounded polling (which
   fixed the earlier failure point); this remaining assertion is a different
   element of the same test and is recorded in §8 as unresolved harness work, not
   an application defect. No application assertion was removed to hide it.

### 7.1 Findings index

Classification: **confirmed application defect** (a failing assertion executed and
reproduced), **harness defect** (test/tooling wrong, repaired), **environment
blocker**, or **unverified hypothesis** (source-derived, not executed on this
host). Severity is based on user impact, not on test count.

### Confirmed harness defects (repaired in this audit)

| ID | Severity | Finding | Evidence | Repair |
| --- | --- | --- | --- | --- |
| F1 | High (test integrity) | The focus-affordance check in `admin-polish.spec.ts` was tautological (`borderColor !== ''` is always true), so no focus indicator was verified. | `admin-polish.spec.ts:391-396` before; corrected logic at `phase4-helpers.ts:120-125` | S1 before/after computed-style comparison; `npm run test:e2e:audit` + chromium matrix re-run |
| F2 | High (test integrity) | `upload-validation.spec.ts` `assertRejected` reached no assertion when the request produced no HTTP response, so a timeout or connection reset counted as a clean rejection. | `upload-validation.spec.ts:968-973` before (guard required a defined `status`) | S3 explicit failure on `status === undefined`; the media expansion that silently skipped is now an assertion |
| F3 | Medium | The board-settings browser save was asserted only after an API re-post, so a broken form could pass. | `board-settings-phase2.spec.ts:63-100` before | S4 asserts the UI-driven DB state before the API call |
| F4 | High | A DB repair that completed with `state === 'failed'` / "maintenance rebuild failed" was accepted as success, and the fault-injection case was permanently skipped. | `maintenance-backup-phase2.spec.ts:60,64` before | S2 requires `finished` + "Maintenance completed" on an intact database (result in §7.1) |
| F5 | Medium | `watchClientErrors` filtered `Failed to load resource … 4xx`, hiding missing assets. | `phase4-helpers.ts:83-84` before | H4 filter removed |
| F6 | Medium | Manual contexts in no-JavaScript projects were created with JavaScript enabled, so `firefox-nojs` reported coverage it did not have. | `auth-admin.spec.ts:115`, `csrf-navigation.spec.ts:38,57`, `password-boards.spec.ts:69,215` before | H1: `newAuditedContext`/`newAuditedPage` inherit the project mode; `audit-harness-contract` guards it |
| F7 | Medium | `adminLogout` waited on `/\/admin/`, which the pre-logout `/admin/panel` URL already matches, so it could resolve before logout took effect. | `helpers.ts:503-508`, `release-tls.spec.ts:61` before | Waits for the exact login path and asserts the login form; `expectAdminPanel` added for the inverse check |
| F8 | Low | `logs()` swallowed read errors into an empty string, so a missing log looked like a silent application. | `helpers.ts:368-370` before | Error text is returned instead |
| F9 | Low | Disposing a standalone instance while its admin panel is still polling produced `ERR_CONNECTION_REFUSED` browser errors during teardown. | Trace evidence: `GET /admin/log/live`, `GET /admin/site-health/jobs` → status `-1` after the panel load | Navigate the page away before disposing; documented in the file |
| F10 | Medium (test integrity) | `getByRole('button')` matches `<input type="file">` (file inputs map to ARIA role button), so unnamed submit lookups silently clicked the file picker. | `audit-admin-ui.spec.ts` first iteration: `BUTTON_INFO={"tag":"INPUT","type":"file"}` | Submit controls are located with `button[type="submit"]`; documented in the file header |
| F11 | Medium (test integrity) | `waitForURL(/section/)` resolved immediately when the current URL already contained the section hash, so mutations were asserted before they completed. | `audit-admin-ui.spec.ts` first iterations (banner/favicon assertions raced) | Interaction helper waits for the mutation response and asserts the specific record outcome |
| F25 | Medium (test integrity) | Strengthened focus assertions used programmatic `locator.focus()`, which does **not** match `:focus-visible` after a pointer interaction. The app's focus styling was correct; the harness reported a false failure. Verified with a raw-engine probe: click → `focus()` gives `:focus-visible: false`, click → Tab → `focus()` gives `:focus-visible: true`. | `admin-polish.spec.ts` expectFocusAffordance and `phase4-helpers.ts:expectFocusVisible` | Both helpers press Tab to enter keyboard modality before focusing; documented so future focus tests do not repeat it. The pre-existing helper had the same latent false-failure mode. |
| F26 | Medium (cross-engine) | Navigation-cancelled requests were only recognised as `net::ERR_ABORTED`, so WebKit's `cancelled` (and Firefox's `NS_BINDING_ABORTED`) were reported as transport failures and failed unrelated tests. | `activity-notifications.spec.ts` on `mobile-webkit`: four `GET /static/* cancelled` transport failures | `isNavigationCancel()` now recognises all three engine wordings; the abort count is still reported. |
| F27 | Medium (integration) | Adding a no-JavaScript project silently re-enabled JavaScript-dependent tests in it: existing gates special-cased the literal name `firefox-nojs`, so `chromium-nojs` ran modal/JS-only tests with JavaScript disabled. | `admin-ban-delete.spec.ts:17` failed on `chromium-nojs`; 15 specs contained bare name checks | Sweep: shared `isNoJsProject()` predicate plus explicit project-list extensions; documented Firefox-only exceptions remain (`firefox-nojs-public.spec.ts`, the Firefox-labelled setup no-JS branch). |
| F28 | Low | The unauthenticated `/admin/panel` test's intentional 403 was an undeclared error surface under the new strict layer. | `admin-dashboard.spec.ts:168` | Declared precisely with `expectHttpError`. |
| F29 | Medium (latent coverage gap) | 19 pre-existing tests contained **undeclared intentional error surfaces** that only became visible when a no-JavaScript project actually ran on this host. Examples: locked-board `GET … 403`, own-post denial `GET …/edit 403`, stale edit `POST … 403`, tampered CSRF `POST … 403`, rejected upload `POST … 415`, and deliberate `GET /missing-board 404` loops. These were also present in JS runs (some were already red once the strict layer landed on chromium) but the Firefox/no-JS project that should have exercised them could never launch here. | `posting-thread`, `release-audit`, `media`, `password-boards`, `theme-system`, `comprehensive-ui-audit`, `phase4-empty-error-permission`, `auth-admin`, `backup-restore` under `expanded/chromium-nojs` and `expanded/{webkit,mobile-webkit}` | 30+ precise declarations added (method/path/status/reason, `times` for bounded loops); no application assertion was changed and no unexpected 4xx/5xx was ever declared. |
| F30 | Low (engine noise) | WebKit logs its own native media-control internals to the console (`Button failed to load, iconName = pip-placard, layoutTraits = [MacOSLayoutTraits Inline], src = blob:`), which the strict layer surfaces in `media.spec.ts` on `mobile-webkit`. | Raw run output captured at `output/playwright/deep-audit/triage/media-webkit` | Declared narrowly in `media.spec.ts` (`times: 'any'`, exact engine-internal pattern, reason given) so it cannot hide an application console error. Reported here as a browser-engine limitation, not an application defect. |
| F31 | — | `expectHttpError`/`expectServerLogError` gained a bounded `times` option so that legitimate repeated surfaces (theme/viewport loops, engine chatter) can be declared without becoming blanket allowlists. | `diagnostics.ts`, `helpers.ts` | Documented in the README diagnostics contract; an unused declaration still fails. |

### Environment blocker

| ID | Severity | Finding | Evidence |
| --- | --- | --- | --- |
| F12 | High (audit scope) | Playwright Firefox v1543 cannot create a browser profile on macOS 27.0/arm64, so all three Firefox projects (462 of 972 baseline executions, including the canonical `firefox-nojs` project) fail at launch before any application code runs. | §1; reproduced in isolation, with a HOME profile, with no `-profile`, and after deleting and re-downloading the build; the older `firefox-1522` build starts |

### Application observability gaps (confirmed by source + probe, low user impact)

| ID | Severity | Finding | Evidence | Repair recommendation |
| --- | --- | --- | --- | --- |
| F13 | Low | RustChan emits `x-request-id` on every response but never logs a request line at the default level, so an operator cannot join browser evidence to server logs by request id. `tower_http` trace output goes to the dependency log and its message omits method, path, and request id. | `src/server/server/lifecycle.rs:10-31`; probe of a fresh instance showed only startup lines in `rustchan.log`; deep-mode `dep_log.log` shows `[TRACE] [on_respo] Finished processing request - latency: 1 ms, status: 200` | Emit one INFO line per request carrying `req_id`, method, path, status, latency (or add the span fields and `req_id` to the trace formatter). This audit's deep mode already sets `RUST_LOG` to capture the status/latency half. |
| F14 | Low | `/readyz` returns `{"status":"ready"}` with no instance identity, so a readiness probe cannot prove the responding server belongs to the expected instance. | `src/server/server/observability.rs` `PublicReadyPayload`; curl of a fresh instance | Include a non-secret instance identifier (e.g. data-directory basename hash or boot id) in the readiness payload so harnesses and operators can verify ownership. Until then the harness verifies ownership through the fixture log and root path. |

### Application findings requiring a decision (not silently fixed)

| ID | Severity | Finding | Status |
| --- | --- | --- | --- |
| F15 | Medium | The NSFW disclaimer gate is enforced on the home page only: direct navigation to `/nsfw`, `/nsfw/catalog`, `/nsfw/thread/{id}` or `/nsfw/search` renders content without consent (`has_nsfw_consent` is consulted by the home-page handler). | Unverified hypothesis (source-derived by the journey agent). Needs a reproducing test asserting a consent gate on direct board navigation; the current journey asserts the real homepage contract. If the gate is intended to be advisory, document it. |
| F16 | Medium | **A banned identity can still file reports.** `POST /report` has no ban gate. | **Confirmed by execution.** The permissions suite keeps a failing test: `expect(response.status(), 'banned identity report should be denied').toBe(403)` observed `303` and `SELECT COUNT(*) FROM reports` = 1 while the ban row still exists. Source: `src/handlers/board/reports.rs` checks CSRF + board view + post/thread consistency only; `is_banned` is applied in `posting::submit_post` (`src/handlers/posting.rs:568`). Recommendation: apply the same ban check to report submission (appeals should presumably remain allowed), or rate-limit reports per identity. Regression test: `audit-permissions.spec.ts:1505`. |
| F17 | Low | Full-text search indexes `posts.body` only (`posts_fts USING fts5(body, …)`), so a term that appears only in a thread subject is not found, although the UI presents search as searching the board. | Confirmed by source (`src/db/schema.rs:711`, `src/db/posts.rs:899-943`). Not a documented-contract violation; the search UI does not promise subject search. Recommendation: either index `subject` in `posts_fts` or state the limitation in the UI. |
| F19 | Low | The board-index ETag signature hashes thread/post data and flags but not board policy fields (`allow_images`, cooldowns), so a conditional GET could answer 304 with a body whose upload form no longer matches the board policy. Currently masked because badge settings default on and pages are `no-store`. | Unverified hypothesis (source-derived). Recommendation: include the board policy set in the ETag signature, and add a regression test that flips an allow-toggle and revalidates. |
| F20 | — | Setup-wizard validation was suspected to lack per-field `aria-invalid`/`aria-describedby` association. | **Reclassified: harness over-assertion, not an application finding.** The app's documented contract is a grouped `role="alert"` summary that names the failing password fields; the audit test now asserts that contract and records the absence of per-field association as an observation (`noteEvent('setup-validation-association', …)`) rather than a failure. |
| F22 | Informational | On loopback the application derives identity from the `rustchan_visitor_id` cookie, so a fresh context or a cleared cookie gets a different identity; bans and cooldowns are therefore per-cookie on loopback, not per-IP. | Confirmed by source (`src/handlers/board.rs` `identity_key`) and observed in the ban test. Production IP-based deployments are unaffected. Documentation recommendation so operators do not interpret a loopback ban as IP-wide. |
| F23 | Medium | **A long unbroken ban reason overflows the 320px viewport on the standalone ban notice.** | **Confirmed by execution.** `audit-ui-a11y.spec.ts:1073` (`expectNoHorizontalOverflow(page, 'banned notice at 320')`) expects <=1px overflow and observed **879px** (document 1199px wide at a 320px viewport; the `<strong>` reason node is 1188px wide). Source: `src/templates/mod.rs:1118` renders `reason: <strong>{reason}</strong>` with no `overflow-wrap`/`word-break`, and `.error-page p` in `static/style.css:3434` adds none. Impact: a banned user on a narrow screen must scroll horizontally to read the reason and reach the appeal form (WCAG 1.4.10 reflow). Recommendation: add `overflow-wrap: anywhere` (or `word-break: break-word`) to the ban-notice reason and the shared `.error-page p` rule; the existing assertion is the regression test. |
| F24 | Low | **Declining the NSFW consent dialog does not restore focus.** Focus stays on the now-hidden cancel button (or falls to `body`). | **Confirmed by execution.** `audit-ui-a11y.spec.ts:1134` asserts the opening card link is focused after declining and observed `inactive`. Related observation (recorded, not asserted): on open, focus stays on the launcher behind the `aria-modal="true"` dialog. Source: `static/main.js:1235` `closeNsfwDisclaimer()` hides the overlay without restoring focus, while `closeConfirmModal` and the edit/report handlers do restore focus. Impact: keyboard and screen-reader users lose their place; focus can sit on hidden content. Recommendation: move focus into the dialog on open and return it to the trigger on dismiss, matching the other modals. |

### 7.2 Reproducing each finding

```sh
# strict-diagnostics contract and the fixture-server/JS-mode guards
npm run test:e2e:harness

# the three confirmed application findings
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test tests/e2e/audit-permissions.spec.ts \
  --project=chromium --workers=1 -g "banned identity can still file reports"
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test tests/e2e/audit-ui-a11y.spec.ts \
  --project=chromium --workers=1 -g "banned notice and its appeal form stay reachable"
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test tests/e2e/audit-ui-a11y.spec.ts \
  --project=chromium --workers=1 -g "declining the NSFW consent dialog"

# the strengthened existing assertions
npx playwright test tests/e2e/admin-polish.spec.ts tests/e2e/maintenance-backup-phase2.spec.ts \
  tests/e2e/board-settings-phase2.spec.ts tests/e2e/moderation-phase2.spec.ts \
  tests/e2e/posting-thread.spec.ts --project=chromium --workers=2
npx playwright test tests/e2e/upload-validation.spec.ts --project=chromium --workers=1
```

### 7.3 Strengthened-assertion outcomes (final expanded Chromium run)

| Change | File | Outcome |
| --- | --- | --- |
| S1 focus affordance | `admin-polish.spec.ts` | **Passes** on every project that runs it, after the helper was corrected to enter keyboard modality (`:focus-visible` does not match after a pointer click). The app's focus styling is present and correct; the original check was tautological, and the first corrected attempt produced a false failure that was traced to the harness, not the app. |
| S2 completed repair | `maintenance-backup-phase2.spec.ts` | **Passes**: on a fresh, intact database the repair reports `finished` and "Maintenance completed". The previous allowlist (`failed` / "maintenance rebuild failed") was therefore an over-permissive harness weakness, not a masked defect. The fault-injection case still requires a hook the binary does not expose. |
| S3 upload rejection without a response | `upload-validation.spec.ts` | **Passes** on chromium, `chromium-nojs`, and `mobile-webkit`: no scenario produced a missing HTTP response, and the media-expansion scenarios now assert the preview instead of skipping it. |
| S4 board-settings UI save | `board-settings-phase2.spec.ts` | **Passes**: the UI-driven save is asserted before the API re-post, and the values match. |
| S5 report banner | `moderation-phase2.spec.ts` | **Passes**: the report outcome banner is asserted directly. |
| S6 stale/duplicate reply | `posting-thread.spec.ts` | **Passes** on chromium, `chromium-nojs`, `webkit`, and `mobile-webkit`. |

Two classes of failure were introduced *by* the strengthened harness and were then
triaged to harness gaps rather than application defects, and are recorded here so
they are not mistaken for product problems:

1. The strict layer's first full no-JS run exposed 19 tests with undeclared
   intentional error surfaces (F29) plus WebKit engine console noise (F30). All
   were declared precisely; no application assertion was changed.
2. `admin-dashboard.spec.ts` "state pills retain readable text contrast across
   built-in themes" failed once with a 90-second timeout and "Target page,
   context or browser has been closed" under seven-way parallel execution. It is
   classified as an intermittent contention failure unless it reproduces in the
   final matrix (result recorded below).

Note for readers of the list reporter: the three `audit-harness-contract`
negative cases are `test.fail(true, ...)` meta-tests, so the reporter prints `✘`
while the run correctly counts them as expected. A green run therefore shows
`✘` marks for those three plus the three intentional application findings.

## 8. Remaining gaps and uncertainty

Executed vs collected coverage is reported in §7; this section lists what is
still not covered and why.

**Blocked on this host**

- Every Firefox-based execution (`firefox`, `mobile-firefox`, `firefox-nojs`):
  Playwright Firefox v1543 cannot create a profile on macOS 27.0. 462 of the 972
  original executions are therefore blocked, including the Firefox no-JavaScript
  project. `chromium-nojs` was added so the JavaScript-disabled contract still
  runs; engine-specific no-JS rendering differences remain unverified.

**Opt-in / manual areas (unchanged by this audit)**

- Real media toolchain (`npm run test:e2e:media`), Tor/Arti onion bootstrap and
  `Onion-Location`, public ACME certificate trust and renewal, and the
  fault-injected DB-repair and pre-repair-backup failure paths. The last one is
  why `site-health` failed-job dismissal and a genuinely failing repair outcome
  cannot be produced without a test hook the binary does not expose.
- Native terminal UI behaviour (Rust console tests plus manual QA, per the
  existing README).

**Coverage that is still thin or absent after this audit**

- Session **expiry** (`CHAN_SESSION_SECS`) and cookie-secret rotation semantics.
- Long-duration soak, load, and resource-exhaustion behaviour. The performance
  file records bounded samples only; no leak or capacity claim is made.
- Poll cleanup/prune, poll option editing, and archived-thread pagination.
- Word-filter replacement semantics beyond a single literal pattern (regex,
  case sensitivity, overlapping matches).
- Rate-limit windows other than the ones existing specs already touch.
- Admin banner *rotation interval* behaviour and global board-banner placement
  toggles (`show_on_index` / `show_on_catalog`) through the UI.
- Split-ZIP part-size selection and full-site restore-upload through the UI
  (covered only through `page.request` in existing specs).
- Ban appeal rate limiting (one per 24 h) and appeal acceptance side effects.
- WebKit execution of the new audit suites (deliberately excluded to bound
  runtime; the no-JS and narrow-profile paths are covered by `chromium-nojs` and
  `mobile-webkit`).

**Unresolved harness item**

- `audit-permissions.spec.ts:1311` still fails on `webkit` and `mobile-webkit`: one
  `toBeTruthy()` inside the secret-leak scan receives `undefined`. The
  ownership-cookie lookup in the same test was fixed with bounded polling, so this
  is a second, different assertion in the same test that needs the same treatment
  (identify the exact value from the WebKit artifact and either poll for it or use
  the equivalent database-backed proof). Evidence:
  `output/playwright/deep-audit/fix/final-webkit/artifacts/`. It is a harness item:
  the same test passes on chromium, so no application behaviour differs.

**Known uncertainty in findings**

- Application findings reported as *unverified hypothesis* were established by
  source reading (or by an agent's source reading) and have either a failing
  assertion that did not execute on this host, or no reproducing test yet. They
  are labelled as such and must not be quoted as confirmed.
- Performance numbers are small-sample, single-host, and indicative only.
- Screenshots were generated for several states but were **not visually
  inspected**; no visual-correctness claim is made. Geometry, computed style,
  overflow, and hit-testing assertions are the automated substitutes.
- `mobile-firefox` and `mobile-webkit` are emulated configurations, not physical
  devices.
