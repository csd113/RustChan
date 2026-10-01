# RustChan Playwright E2E Suite

The maintained Playwright harness belongs in version control: `playwright.config.ts`,
`package.json` / `package-lock.json`, `tests/e2e/**`, and
`.github/workflows/e2e.yml`. Generated run evidence stays ignored: browser
reports, `test-results/`, the `output/playwright/` runtime tree (including
preserved debug roots and audit output), traces, videos, screenshots, storage
state, and the rebranded-Firefox workaround bundle. Never force-add generated
artifacts.

Each test receives an isolated runtime under `output/playwright/runtimes/`,
which is removed on teardown.

The suite covers RustChan only: public pages, admin, posting, media, backups,
authentication, Tor-disabled UI, and no-JavaScript fallbacks.

## Install

Run from the repository root:

```sh
npm ci
npx playwright install
```

The global setup builds `target/debug/rustchan-cli` unless
`RUSTCHAN_E2E_SKIP_BUILD=1` is set. External target overrides are rejected. Each test copies the binary into a unique temporary
runtime, reserves its own loopback port, and writes settings, SQLite data,
uploads, logs, backups, downloads, fixtures, screenshots, traces, and videos
under the ignored output/playwright/runtimes directory.

Runtime disposal first closes audited pages on that runtime’s origin while its
listener is alive, including manually owned standalone runtimes. Pages belonging
to other runtimes remain open. This prevents background polling from racing a
server shutdown; diagnostics are neither disabled nor allowlisted.

Temporary RustChan runtime roots are removed during fixture teardown, including
after failed tests once logs and other failure artifacts are attached. Set
`RUSTCHAN_E2E_PRESERVE_ROOTS=1` to retain them for debugging; the harness prints
each preserved path.

## Test modes

Two supported modes share one configuration; retries stay at **zero** in both.

### Normal regression mode

Routine CI runs the maintained suite per engine, one project per job, with a
failure cap. Locally the closest equivalent is:

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test --project=chromium --workers=2 --max-failures=5
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test --project=webkit --workers=2 --max-failures=5
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test --project=firefox --workers=2 --max-failures=5
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test --project=chromium-nojs --workers=2 --max-failures=5
```

`npm run test:e2e:ci` is the bounded Chromium shorthand (`--max-failures=5`).
The tracked workflow `.github/workflows/e2e.yml` runs these four projects on
pull requests, pushes, and merge groups; evidence is uploaded only on failure.

### Deep audit mode

The full project matrix with bounded deep diagnostics (traces where enabled,
step screenshots for `deep-journey` tests, `RUST_LOG` request lines, timing
files, and the artifact budget), all under the existing `RUSTCHAN_AUDIT_OUTPUT`
convention:

```sh
RUSTCHAN_E2E_DEEP_DIAGNOSTICS=1 \
RUSTCHAN_AUDIT_OUTPUT=output/playwright/deep-audit/<run> \
RUSTCHAN_E2E_SKIP_BUILD=1 \
npm run test:e2e:deep
```

`npm run test:e2e:deep` is the all-projects matrix (`chromium`,
`chromium-nojs`, `webkit`, `mobile-webkit`, `firefox`, `mobile-firefox`,
`firefox-nojs`). The workflow exposes it as `workflow_dispatch` input
`mode=deep` and as the weekly scheduled pass. Deep diagnostics are expensive;
they are deliberately not part of the routine pull-request pass. A single
project can be run deeply with an explicit `--project`.

## Firefox on macOS 27 (Playwright issue #42768)

The stock Playwright 1.63 Firefox bundle shares `Name=Firefox` / `Vendor=Mozilla`
with the user's Firefox. On macOS 27, TCC denies this host access to
`~/Library/Application Support/Firefox`. Gecko accesses that app-data root even
with a valid temporary `-profile`; changing HOME, TMPDIR, profile location or
launch timing does not isolate it. A minimal stock `firefox.launch()` reproduces
`Could not find profile folder.` without RustChan. This is an upstream OS/bundle
identity defect, not ARM64 emulation or a server readiness failure:
[Playwright #42768](https://github.com/microsoft/playwright/issues/42768).

Normal commands work without shell setup:

```sh
npm run test:e2e:firefox
npm run test:e2e:firefox-nojs
```

`firefox-rebrand.ts` automatically isolates the app identity only on Darwin 27+
when the installed bundle still has the original Firefox/Mozilla identity. It
copies the exact installed browser revision to ignored output, changes only the
application descriptor's Name/Vendor, and generates a safely quoted launcher
that puts `-app` before Playwright's arguments (required by Gecko). Linux, older
macOS and an upstream-isolated identity use the stock binary. No launch retries,
sleeps, external browser selection, HOME changes or permission grants are used.
The content/revision keyed cache is published atomically and checked before reuse.
The main config and the alternate button-sizing config use the same helper.

The upstream issue is still open with a v1.64 label as of 2026-09-30. An original
identity on Playwright 1.64+ fails configuration with an explicit re-evaluation
message; an upstream-isolated identity bypasses the helper automatically.
`firefox-launch.spec.ts` checks OS/identity scope and actual browser launch plus
JavaScript/no-JavaScript behavior on every configured project. Remove the helper
and config wiring once the supported bundled browser fixes #42768 and the stock
Firefox/mobile/no-JS launch probes pass.

## Maintained source, fixtures and disposable evidence

Track the TypeScript specs/helpers/configs, helper `.mjs` scripts, npm manifest
and lockfile, and browser workflow. Required small sanitized regression fixtures
belong in `tests/e2e/fixtures/`; most current fixtures are generated deterministically
inside each isolated runtime by `helpers.ts`. Never copy runtime databases,
authentication state or downloaded browser bundles into source fixtures.

`output/`, `test-results/`, browser reports, caches, profiles, logs, videos,
traces and screenshots are disposable ignored evidence. The historical
`tests/e2e/AUDIT-*.md` notes also stay local because they refer to disposable runs.
The admin-polish scripts under `output/playwright/admin-polish/` use a fixed
local audit server/auth state and remain disposable review tools. Reusable
regressions are maintained as specs, including `admin-polling-lifecycle.spec.ts`
and `admin-table-scroll.spec.ts`; no preserved local evidence is deleted.

Wide admin tables intentionally retain dense native table columns and scroll
within named focusable regions at 320px. Keyboard arrows pan those regions;
child action keys retain native behavior. Native touch panning remains enabled,
focus outlines stay outside the container and there are no sticky cells. The
regression covers all 21 sections, nine themes on Chromium and the forest theme
on every browser/no-JS project. This behavior is intentional, not a layout defect.

The site-health poller now uses completion-based scheduling, at most one request
in flight, and beforeunload/pagehide cancellation. WebKit can throw synchronously
when an interval starts fetch during a provisional form navigation; pagehide
alone occurs too late. The lifecycle regression runs the next health deadline
at beforeunload, after production cancellation, without sleeps or retries.
BFCache pageshow resumes polling for restored documents.


## Diagnostics

`tests/e2e/diagnostics.ts` installs a strict, always-on browser diagnostics layer.
Every observation is recorded as a structured, redacted event carrying the acting
actor, project, test title, worker, timestamp, HTTP status, content type,
`Location`, and the server's `x-request-id`, and these rules apply:

| Observation | Default outcome |
| --- | --- |
| Uncaught page error (`weberror`) | **Test fails** |
| `console.error` | **Test fails** |
| `console.warning` | Recorded only |
| HTTP 4xx from page traffic | **Test fails** |
| HTTP 5xx from page traffic | **Test fails** |
| HTTP 3xx | Recorded (redirect observability) |
| Transport failure (DNS, refused, TLS, timeout) | **Test fails** |
| `net::ERR_ABORTED` | Recorded and counted; not a failure (navigation and teardown cancel requests by design) |
| Wrong content type for `/static/**.css`, `/static/**.js`, `/theme-css/**` | **Test fails** |
| New `[ERROR]` line in the fixture server log | **Test fails** |

`page.request.*` traffic is out of scope by construction: Playwright does not emit
context response events for `APIRequestContext`, so API-driven prerequisites and
negative tests need no allowlists.

A browser console message that refers to an already-declared HTTP error response is
treated as explained by that declaration. Chromium logs
`Failed to load resource: the server responded with a status of 404` for 404
*document* navigations as well as subresources, so this avoids requiring two
declarations for one response. An undeclared 4xx still fails, and its console
message with it.

### Declaring expected failures

A test that intentionally triggers one of the above declares it *before* the
action, naming the kind plus method/path/status or a message pattern:

```ts
expectHttpError({ method: 'GET', path: '/locked/catalog', status: 403, reason: 'the gate is the subject' });
expectConsoleError({ pattern: /Failed to load resource/, reason: 'simulated stylesheet failure' });
expectTransportFailure({ url: /api\/post/, reason: 'offline recovery path under test' });
expectPageError({ pattern: /deliberate/, reason: 'error boundary under test' });
expectServerLogError({ pattern: /repair failed/, reason: 'fault-injected repair path' });
noteEvent('evidence-key', 'free-form redacted evidence attached to the report');
```

`expectServerLogError` lives in `helpers.ts` (the app fixture owns the server
log); the rest live in `diagnostics.ts`. `noteEvent(kind, detail)` takes two
arguments and never fails a test — it attaches evidence only.

### Rendered-DOM facts that repeatedly bite test authors

- `<input type="file">` matches ARIA role `button`. An unnamed
  `getByRole('button')` can therefore click the file picker instead of the
  submit control; use `button[type="submit"]` for unnamed submits.
- The admin panel renders a deliberate theme-preview pair of `.admin-flash`
  elements ("Saved theme preview" / "Validation message preview") inside
  `.theme-preview-flashes`, so a bare `.admin-flash` locator matches two
  elements. Scope flash assertions by text.
- Admin controls live inside `<details>` disclosures (`admin-dropdown`,
  `backup-manual-details`, per-board cards). Open them through their real
  `<summary>` before interacting, and only click `:visible` summaries so the
  responsive mobile menu is never targeted.
- `waitForURL(/section/)` resolves immediately when the current URL already
  contains the same section hash. Wait for the mutation response or assert the
  record outcome instead.
- On loopback the actor identity is the `rustchan_visitor_id` cookie: a fresh
  context is a different actor, and bans/cooldowns are per-cookie. Bans are
  enforced on the posting path: a plain form POST by a banned identity returns
  HTTP 403 whose body is the ban page, while the XHR path returns
  `X-Rustchan-Redirect: /banned?reason=…`.
- Favicon uploads must be exactly 512×512 px (`src/favicon.rs`); banners must be
  at least 468×60 with the exact 468:60 aspect ratio (`src/banner.rs`).
- Dispose a standalone instance only after navigating the browser away from any
  admin page that polls (`/admin/log/live`, `/admin/site-health/jobs`), otherwise
  teardown produces `ERR_CONNECTION_REFUSED` browser errors.
- `ban_appeals.status` defaults to `'open'`, not `'pending'`.

Declarations are consumed by observed events, once each, in observation order. An
unused declaration fails the test, so expectations cannot silently rot into a
blanket allowlist. Mark a genuinely conditional declaration `optional: true` and
say why.

### Harness self-verification

`audit-harness-contract.spec.ts` proves the layer fails closed. Its negative cases
are marked `test.fail(true, ...)`: if the diagnostics ever stop failing on an
undeclared 4xx, console error, or uncaught page error, those tests unexpectedly
*pass* and the suite goes red. It also verifies the declaration matcher directly,
that manual contexts inherit the project JavaScript mode, and that the fixture
server belongs to the current run.

### Deep diagnostics (opt-in, bounded)

```sh
RUSTCHAN_E2E_DEEP_DIAGNOSTICS=1 RUSTCHAN_E2E_DEEP_DIAGNOSTICS_MAX=400 \
  npx playwright test tests/e2e/audit-journeys.spec.ts --project=chromium --workers=1
RUSTCHAN_E2E_TIMING=1 npx playwright test tests/e2e/audit-performance.spec.ts --project=chromium --workers=2
```

Deep mode keeps Playwright tracing enabled for the tests it runs, attaches bounded
screenshots for steps in tests annotated `deep-journey`, injects
`RUST_LOG=info,tower_http::trace=trace` so request status/latency lines land in the
runtime dependency log, attaches the dependency-log tail on failure, and writes
`timings.json` per test. The default screenshot budget is 400 per run. Timing runs
must stay separate from trace/video capture; `audit-performance.spec.ts` disables
both. `RUST_LOG` is only injected in deep mode: an empty value would change the
default filter and hide startup lines other tests assert.

### Artifact sensitivity

Failure artifacts contain synthetic credentials (`admin` / `AdminPass123!`),
session cookies, CSRF tokens, and request IDs. `redact()` strips credentials,
session cookie values, CSRF tokens, ownership grants, and `deletion_token` values
before anything is attached or written under `RUSTCHAN_AUDIT_OUTPUT`. All fixture
runtime roots, uploads, and databases are synthetic and local; do not add real
secrets, real user data, or external targets.

### Emulated configurations

`mobile-firefox` (custom viewport/touch/user agent) and `mobile-webkit` (iPhone 13
preset) are **emulated** configurations. They are not tests on physical Android or
iPhone hardware, and results must not be reported as such.

### No-JavaScript projects

`firefox-nojs` is the canonical no-JavaScript project. `chromium-nojs` was added by
the 2026-09-29 deep audit because the Playwright Firefox build could not launch on
that host (upstream #42768; see the Firefox section above) and keeps the
JavaScript-disabled contract covered on every host. Both projects are listed in
`NO_JS_PROJECTS` in `diagnostics.ts`, so manual contexts created through
`newAuditedContext`/`newAuditedPage` inherit the no-JavaScript mode there. A spec
that disables JavaScript locally with `test.use({ javaScriptEnabled: false })`
must still pass `javaScriptEnabled` explicitly when it creates a context, because
Playwright does not expose describe-level `use` overrides to fixtures.

## Deep Audit (2026-09-29)

`tests/e2e/AUDIT-deep-audit-2026-09-29.md` is the historical audit report: what
the suite proved at revision `146423b3`, the evidence, and the confirmed
findings. It is provenance, not a description of current behaviour. The three
confirmed application findings it recorded are fixed in the maintained suite:

- banned identities can no longer file reports or poll votes; ban enforcement
  is centralized in `src/handlers/board.rs::ensure_actor_not_banned` and used by
  the posting, report, and vote paths;
- the standalone ban notice wraps long unbroken reasons at narrow widths;
- the NSFW consent dialog moves focus into the dialog on open and restores it to
  the trigger on dismiss, like the other modals.

The strict-diagnostics declarations the audit added remain part of the contract.

Quick confidence passes:

```sh
npm run test:e2e:harness    # strict diagnostics self-verification
npm run test:e2e:audit      # maintained audit regressions on Chromium
npm run test:e2e:audit:nojs # no-JavaScript parity on Chromium with JS disabled
npm run test:e2e:perf       # bounded performance diagnostics, serial
```

## Terminal Console Compatibility

Browser fixtures deliberately start RustChan **headlessly**, with stdin disabled
and stdout/stderr piped into `server.log`. The Ratatui console and interactive
first-run setup require both stdin and stdout to be terminals, so they must not
activate in Playwright workers—even when `TERM`, `COLUMNS`, or `LINES` are
inherited from an interactive shell. Fixtures use CLI or web setup to create
administrators and boards, HTTP `/readyz` for readiness, and SIGTERM for shutdown.
Do not inherit stdio, allocate a PTY, send console shortcuts, or strip terminal
escape sequences from captured logs to make browser tests pass.

Run the focused console/headless compatibility pass:

```sh
npm run test:e2e:console
npx playwright test tests/e2e/auth-admin.spec.ts --project=chromium
```

It checks fresh web setup without terminal prompts, plain captured logs,
CLI-seeded admin/board access across restarts, and graceful signal-driven
shutdown. Startup failures include a bounded output tail to expose terminal
initialization errors directly.

Playwright does not render or drive the native terminal UI. Its navigation,
forms, masking, confirmations, log following, selection, and responsive layout
coverage lives in the Rust console tests:

```sh
cargo test --all-features server::console -- --test-threads=1
```

For interactive release QA, run a disposable instance in a real terminal and
check the 1–4 navigation tabs, C/A/D forms, Tab/Shift-Tab focus, F2 submission,
Escape cancellation/back navigation, Q confirmation, Ctrl-C, log scrolling and
follow mode, and terminal restoration. Exercise 40×10 (resize guidance), 44×14,
60×18, 80×24, and 120×40 dimensions, including resizing with a dialog open.
Never use a live instance for destructive workflow checks.

## Quick Smoke

This is the recommended fast confidence pass across desktop Chromium, desktop
WebKit, desktop Firefox, mobile WebKit, and Firefox with JavaScript disabled:

```sh
npx playwright test \
  tests/e2e/audit-surfaces.spec.ts \
  tests/e2e/phase3-media-runtime.spec.ts \
  tests/e2e/phase3-mobile-webkit-runtime.spec.ts \
  tests/e2e/firefox-nojs-public.spec.ts \
  --project=chromium \
  --project=webkit \
  --project=firefox \
  --project=mobile-webkit \
  --project=firefox-nojs
```

For a narrower preflight while iterating on harness code:

```sh
npx playwright test --list
npx playwright test tests/e2e/audit-surfaces.spec.ts --project=chromium
npx playwright test tests/e2e/firefox-nojs-public.spec.ts --project=firefox-nojs
```

The retained local npm shorthand runs a bounded Chromium pass with two workers and a five-failure cap:

```sh
npm run test:e2e:ci
```

The tracked browser workflow is `.github/workflows/e2e.yml` (normal regression
projects on PR/push, deep audit on `workflow_dispatch mode=deep` and the weekly
schedule). Failure diagnostics retain traces, screenshots, videos, server logs,
and browser errors; retries are disabled.

## Full Local Matrix

Run a single browser when investigating a failure:

```sh
npm run test:e2e:chromium
npm run test:e2e:webkit
npm run test:e2e:firefox-nojs
npx playwright test --project=firefox
npx playwright test --project=mobile-webkit
```

Run the whole local matrix:

```sh
npm run test:e2e
```

Run focused suites by domain:

```sh
npx playwright test tests/e2e/auth-admin.spec.ts tests/e2e/csrf-navigation.spec.ts --project=chromium
npx playwright test tests/e2e/posting-thread.spec.ts tests/e2e/password-boards.spec.ts --project=chromium
npx playwright test tests/e2e/backup-restore.spec.ts tests/e2e/maintenance-backup-phase2.spec.ts --project=chromium
npx playwright test tests/e2e/phase4-accessibility-progressive.spec.ts --project=chromium --project=mobile-webkit --project=firefox-nojs
```

## Opt-In Heavy Passes

Real media toolchain validation is disabled by default. The default harness sets
`CHAN_REQUIRE_FFMPEG=0` and points FFmpeg/ffprobe to sentinel binary names so
local codec installs do not affect deterministic browser tests.

```sh
npm run test:e2e:media
```

Override media tool paths when needed:

```sh
RUSTCHAN_E2E_FFMPEG_PATH=/path/to/ffmpeg \
RUSTCHAN_E2E_FFPROBE_PATH=/path/to/ffprobe \
npm run test:e2e:media
```

The media pass requires FFmpeg, ffprobe, and the `libwebp`, `libvpx-vp9`, and
`libopus` encoders. PDF renderers are optional; if Poppler `pdftoppm`, MuPDF
`mutool`, or macOS `qlmanage` is unavailable, the test asserts RustChan's SVG
PDF thumbnail fallback.

The upload matrix runs only against an isolated local runtime:

```sh
npx playwright test tests/e2e/upload-validation.spec.ts --project=chromium --project=firefox-nojs --project=mobile-webkit
```

The button sizing harness has its own config and artifact directory:

```sh
npx playwright test -c tests/e2e/button-sizing.config.ts
```

## Manual Or Skipped Areas

Some release checks intentionally remain manual because they depend on local
services, trusted certificates, or fault injection that the normal binary does
not expose:

- Active Tor/Arti onion bootstrap, onion copy behavior, `Onion-Location`, and
  already-onion suppression. Start a deterministic Tor/Arti release environment,
  enable Tor in RustChan, and use `phase3-static-proxy-tor.spec.ts` as the
  checklist for the browser assertions.
- Public certificate authority/ACME trust and renewal. `release-tls.spec.ts` now covers local HTTPS, secure sessions, restart/restoration, logout invalidation, and redirect-host rejection using a self-signed certificate trusted only by its isolated browser context.
- Fault-injected database repair failure and pre-repair backup failure. These
  require a binary or harness exposing deterministic failure hooks.
- Extreme file-size and codec performance limits. The upload and media suites
  cover policy and safe fallback behavior; final performance limits should be
  checked on representative release hardware.
- Long-duration soak and load runs. The complete bounded local matrix runs with zero retries.

## Suite Map

- `helpers.ts`: isolated RustChan runtime fixture, CLI setup, seeded boards,
  fixture media, CSRF/admin/post helpers, DB fixture helpers, server-log evidence
  (`logSize`/`logsSince`/`dependencyLogs`), and `expectServerLogError`.
- `diagnostics.ts`: strict browser diagnostics layer (see § Diagnostics).
- `firefox-rebrand.ts`: opt-in macOS-only workaround for Playwright issue #42768
  (see § Firefox on macOS 27).
- `audit-harness-contract.spec.ts`: meta-tests proving the diagnostics layer fails
  closed, that manual contexts inherit the project JavaScript mode, and that the
  fixture server belongs to this run. Chromium only.
- `audit-journeys.spec.ts`: complete multi-actor journeys and state transitions.
- `audit-permissions.spec.ts`: actor/resource permission matrix with crafted
  requests, CSRF matrix, stale sessions, tampered identifiers, hostile content.
- `audit-nojs-parity.spec.ts`: server-rendered plain-form contract and
  JavaScript/no-JavaScript business-outcome parity.
- `audit-ui-a11y.spec.ts`: bounded viewport matrix, keyboard/focus behaviour,
  modal focus management, accessible names and error associations.
- `audit-performance.spec.ts`: bounded repeated-sample timings, request counts,
  duplicate-request checks, transfer sizes, observable server CPU/RSS samples.
- `audit-admin-ui.spec.ts`: admin surfaces previously covered only through
  `page.request`, driven through the rendered forms.
- `console-headless.spec.ts`: fresh/headless startup, terminal-output isolation,
  web setup availability, and seeded administration across process restarts.
- `helpers-lifecycle.spec.ts`: graceful headless shutdown, process/port cleanup,
  and temporary-root disposal or explicit preservation.
- `activity-helpers.ts`: shared activity badge setup/assertions used by the
  broad activity audit and Phase 3 BFCache/restart checks.
- `phase4-helpers.ts`: safe-body/header assertions, overflow/focus/target
  checks, contrast checks, client-error watcher, and Phase 4 project gates.
- `auth-admin.spec.ts`: first-run startup, admin login/logout, stale pages,
  cookie attributes, context isolation, board creation/edit/delete, and invalid
  board names.
- `csrf-navigation.spec.ts`: CSRF scope, missing/invalid tokens, Origin/Referer
  policy, loopback/null-origin behavior, POST-only routes, and stale navigation.
- `posting-thread.spec.ts`: public browsing, catalog/thread navigation, posting,
  replies, escaping, duplicate/back-forward/refresh behavior, and own-post
  controls.
- `password-boards.spec.ts`: view-password, post-password, unlock cookie
  isolation, mobile WebKit secure-cookie behavior, and password changes.
- `media.spec.ts`, `phase3-media-runtime.spec.ts`, `media-toolchain.spec.ts`,
  and `upload-validation.spec.ts`: upload acceptance/rejection, media viewer,
  no-JS media fallbacks, media/download headers, Range requests, protected
  media denial, real FFmpeg/PDF toolchain behavior, and isolated upload
  validation.
- `activity-notifications.spec.ts` and `phase3-activity-cache.spec.ts`: activity
  badge visibility, clearing, settings toggles, restart persistence, BFCache,
  protected-board non-leakage, and Firefox no-JS clearing.
- `admin-settings-phase2.spec.ts`, `board-settings-phase2.spec.ts`,
  `settings.spec.ts`, and `assets-themes-phase2.spec.ts`: admin/site/board
  settings, media/banner/theme settings, cache-busting, search/catalog
  hardening, archive behavior, maintenance CSRF, and settings persistence.
- `moderation-phase2.spec.ts`, `admin-ban-delete.spec.ts`, and
  `admin-polish.spec.ts`: report flow, duplicate reports, ban/appeal lifecycle,
  moderation queues, styled ban+delete modal, no-JS fallback, and public/admin
  moderation controls.
- `backup-restore.spec.ts`, `maintenance-backup-phase2.spec.ts`, and
  `phase4-stress-release-smoke.spec.ts`: full/board backups, split backup
  options, downloads, restore into fresh runtimes, invalid restore rejection,
  scheduled backups, maintenance progress, and release-smoke restore.
- `phase3-static-proxy-tor.spec.ts`: static/runtime security headers,
  cache-policy checks, trusted proxy and secure-cookie behavior, Tor-disabled UI
  safety, plus manual Tor/TLS notes.
- `audit-surfaces.spec.ts`, `mobile-nojs.spec.ts`, `firefox-nojs-public.spec.ts`,
  `captcha.spec.ts`, `button-sizing.spec.ts`,
  `phase4-accessibility-progressive.spec.ts`,
  `phase4-responsive-long-content.spec.ts`,
  `phase4-theme-contrast.spec.ts`, and
  `phase4-empty-error-permission.spec.ts`: cross-browser reachable-surface
  audit, mobile/no-JS flows, CAPTCHA, control sizing, keyboard/focus/accessibility
  basics, responsive/long-content layout, built-in theme readability, and safe
  empty/error/permission states.

## Coverage Map

| System | Status | Main Coverage |
| --- | --- | --- |
| Setup/config | Strong | First-run runtime layout, bad settings fail-closed, settings persistence, isolated tests, external targets rejected |
| Terminal/browser boundary | Automated boundary + Rust/manual TUI | Headless startup/restart, plain logs, no interactive prompts, SIGTERM shutdown; native rendering and keyboard behavior use Rust console tests and real-terminal QA |
| Admin auth/CSRF | Strong | Login/logout, stale pages, cookie attributes, session CSRF, Origin/Referer/null-origin policy, POST-only mutations |
| Board access | Strong | Public boards, view-password/post-password boards, unlock cookie isolation, secure cookie mode, protected non-leakage |
| Posting/thread lifecycle | Strong | Thread/reply create, validation, duplicate prevention, navigation refresh/back-forward, lock/archive/delete/ban denials |
| Media/upload/viewer | Strong | Upload matrix, disabled policy, spoofed/oversized/traversal inputs, viewer, headers, downloads, Range, protected denial, opt-in real tools |
| Own-post controls | Strong | Edit/delete modal, no-JS edit/delete pages, ownership cookie isolation, expired/replied/locked/archived/deleted denials |
| Activity/cache | Strong | Home/board/catalog/thread badges, settings toggles, restart persistence, BFCache, protected non-leakage, Firefox no-JS clearing |
| Admin/settings | Strong | Site, board, media, backup, banner, theme, validation, TOML/writeback, admin-only and CSRF protection |
| Moderation | Strong | Reports, duplicate reports, resolution, delete, bans, appeals, mod log, ban+delete modal, no-JS fallbacks |
| Backup/restore | Strong | Directory/split full backups, board backups, saved restore, fresh-runtime restore, invalid restore, Tor key include/exclude |
| Maintenance | Adequate | DB check/repair status/progress/stale jobs/concurrent backup blocking; deterministic repair failure remains manual |
| Themes/assets | Strong | Built-in themes, site defaults, board overrides, favicons, banners, cache-busting, readability/contrast sanity |
| Tor/TLS/proxy | Partial | Trusted proxy, local HTTPS, secure sessions, restoration, and redirect-host checks are automated; active Tor and public certificate trust/renewal remain manual |
| Accessibility/responsive/error states | Adequate | Named controls, focus restore, target sizing, no-overlap checks, long content, mobile WebKit, safe 4xx/invalid states |

## Release audit additions

- `release-audit.spec.ts`: empty/nonempty statistics, malformed catalog storage, stale editing and locked-thread races, repeated Enter during an in-flight upload, signed CSRF behavior after cookie loss, update failure/recovery, and natural multi-board navigation with page lifecycle evidence.
- `release-nojs-catalog.spec.ts`: actual pin/unpin, report, and hide/unhide actions with JavaScript disabled; button containment at 1280, 390, and 320 pixels.
- `release-tls.spec.ts`: isolated HTTPS login, secure cookie attributes, server restart and session restoration, cross-context logout invalidation, and safe redirects.
- `auto-compress.spec.ts`: visible failure feedback and real silent/audio video compression, upload, browser playback, and audio-track preservation. WebKit asserts its unsupported-capture feedback.

Set `RUSTCHAN_AUDIT_OUTPUT=output/playwright/release-audit/<run>` for isolated JSON/HTML/artifact output. Browser console errors, request failures, and HTTP errors are attached for review; uncaught page errors fail the test. Negative-response tests intentionally generate some 4xx and aborted requests.
