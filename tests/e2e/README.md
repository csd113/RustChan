# RustChan Playwright E2E Suite

The Playwright test source, helper/fixture generators, configuration, npm
manifest/lockfile, and CI workflow belong in Git. Generated `test-results/`,
`playwright-report/`, `node_modules/`, and `output/` directories stay ignored.
Put one-off play-test reports, probe scripts, snapshots, and disposable fixtures
under `output/`; keep reusable test source and required fixtures under `tests/`.

The suite covers RustChan only. ChanNet is not enabled, seeded, or tested.

## Install

Run from the repository root:

```sh
npm install
npx playwright install
```

The global setup builds `target/debug/rustchan-cli` unless
`RUSTCHAN_E2E_SKIP_BUILD=1` is set or `RUSTCHAN_UPLOAD_BASE_URL` points at an
already running target. Each worker copies the binary into a unique temporary
runtime, reserves its own loopback port, and writes settings, SQLite data,
uploads, logs, backups, downloads, fixtures, screenshots, traces, and videos
under isolated worker/test output directories.

Temporary RustChan runtime roots are removed during fixture teardown, including
after failed tests once logs and other failure artifacts are attached. Set
`RUSTCHAN_E2E_PRESERVE_ROOTS=1` to retain them for debugging; the harness prints
each preserved path.

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

CI runs the bounded Chromium project with two workers and a five-failure cap:

```sh
npm run test:e2e:ci
```

The dedicated workflow retains Playwright traces, screenshots, videos, and the
HTML report when that command fails.

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

The upload matrix can run against either an isolated local runtime or an
external RustChan target:

```sh
npx playwright test tests/e2e/upload-validation.spec.ts --project=chromium --project=firefox-nojs --project=mobile-webkit
RUSTCHAN_UPLOAD_BASE_URL=http://127.0.0.1:8080 npx playwright test tests/e2e/upload-validation.spec.ts --project=chromium
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
- Local HTTPS listener behavior with self-signed browser trust and redirect
  listener host-rejection checks. Run these in a release TLS fixture where the
  browser trusts the local certificate and the redirect listener is configured.
- Fault-injected database repair failure and pre-repair backup failure. These
  require a binary or harness exposing deterministic failure hooks.
- Extreme file-size and codec performance limits. The upload and media suites
  cover policy and safe fallback behavior; final performance limits should be
  checked on representative release hardware.
- Full all-spec, all-browser soak runs. Use `npm run test:e2e` locally when time
  allows; CI-style retries are not assumed by default.

## Suite Map

- `helpers.ts`: isolated RustChan runtime fixture, CLI setup, seeded boards,
  fixture media, CSRF/admin/post helpers, and DB fixture helpers.
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
  media denial, real FFmpeg/PDF toolchain behavior, and external-target upload
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
| Setup/config | Strong | First-run runtime layout, bad settings fail-closed, settings persistence, isolated workers, external upload target mode |
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
| Tor/TLS/proxy | Skipped/manual | Trusted proxy and secure-cookie checks are automated; active Tor and local trusted TLS browser flows remain manual |
| Accessibility/responsive/error states | Adequate | Named controls, focus restore, target sizing, no-overlap checks, long content, mobile WebKit, safe 4xx/invalid states |
