# Theme system audit — 2026-09-06

Status: complete. Verified defects are fixed and the final Rust, frontend, dependency-policy, and full browser checks pass. No commits, uploads, deployments, or production-data mutations were performed.

## Architecture after the audit

The database theme catalog and site setting are authoritative configuration. Runtime snapshots provide enabled theme metadata to renderers. Each rendering decision resolves against one catalog snapshot. The visitor's `rustchan_theme` cookie wins over the board default, which wins over the site default. Invalid, disabled, or deleted values fall through to enabled Forest, the first enabled theme, then bundled Forest as the emergency fallback. Matching is case-insensitive with surrounding whitespace ignored; arbitrary partial or malformed slugs are not accepted as preferences.

The server renders `data-active-theme`, `data-default-theme`, `data-theme-slugs`, and the selected preference option. Terminal keeps its established absence of `data-theme`; other themes use that attribute. All nine built-ins live in `static/style.css`. Only custom themes receive a blocking `/theme-css/{slug}` link, and `data-theme-css-slugs` tells live switching which themes require that link. `theme-init.js` only enables progressive enhancement. Local storage is a compatibility mirror, never a preference source.

Both the legacy picker hook and the visible preferences select use one preference-saving path. Custom CSS loads before its theme is committed; superseded loads are canceled and errors retain the prior theme. DOM markers, selectors, picker state, and the storage mirror update together. Cookie mirroring supports immediate navigation; background preference requests are serialized so older responses cannot be the final saved selection. Failed loads/saves produce visible status messages. Native links and POST forms remain the no-JavaScript persistence path.

Custom CSS requires revalidation. HTML theme ETags include the effective default, active theme, catalog revision, and static asset version. Request-aware error rendering runs before compression, preserves HTTP status and security headers, supplies valid preference-form CSRF state, and uses `private, no-store`. Persisted browser-history documents revalidate through the existing restoration handler.

RustChan has no operating-system-following theme mode or account-specific theme database preference. “Default” means board/site inheritance. No new OS mode, visual redesign, dependency, commit, or deployment was introduced.

## Verified defects and fixes

| Defect and evidence | Root cause and correction |
|---|---|
| Server stylesheet URL changed immediately after initial load; baseline browser assertion failed. | Both startup scripts reapplied the theme and overwrote the versioned link. Startup now respects server rendering; built-ins need no extra link. |
| Live selection left `data-active-theme` unchanged, and the legacy picker could diverge from the form and persistence. | Separate application/persistence functions updated only some state and swallowed fetch failures. One validated, serialized form path now owns live selection. |
| A failed or superseded custom stylesheet could leave the page marked as the new theme while showing an old/base palette. | Attributes changed before CSS loaded. Replacement CSS is staged, committed on load, and canceled when superseded; failure leaves the previous selection intact. |
| Disabled Terminal remained the configured active default; baseline browser assertion failed. | Terminal had a special exemption from enabled-theme validation. All default candidates now follow the same validation. |
| An all-disabled catalog requested a missing fallback stylesheet and exposed an empty theme select. | Built-in CSS was unnecessarily served through the enabled database endpoint. Built-ins now use bundled CSS, and the empty catalog displays a disabled, correctly labeled Forest fallback option. |
| Editing custom CSS could retain the previous stylesheet for an hour; changing the catalog could reuse HTML containing stale options. | Dynamic theme CSS used the generic static cache policy; HTML theme signatures only represented the active slug. CSS now revalidates and HTML signatures include theme/configuration/asset revisions. |
| Renaming a theme migrated database references but left its CSS targeting the old slug. | CSS selectors and embedded builder overrides were not retargeted. Exact `data-theme` attribute selectors are updated transactionally; builder metadata is regenerated consistently. |
| Light builder themes, including previously saved generated themes, advertised dark native controls. | Generated CSS hard-coded `color-scheme: dark`. Scheme now follows input-background brightness, including a narrowly targeted serving-time correction for existing generated CSS. Stored custom CSS is not rewritten merely by reading it. |
| Malformed builder metadata and preview input could insert CSS declarations outside the intended preview rule. | Deserialization checked types but not color/radius domains, and JavaScript interpolated raw field values. Metadata now validates those domains; preview values accept hex colors and bounded integer radii only. CSP and raw-CSS administration permissions were not relaxed. |
| Error, ban, appeal-result, rate-limit, and restore-redirect pages lost the visitor's theme; generic unmatched routes had no themed page. | Standalone templates lacked request preference context or bypassed the shared layout. They now preserve theme state, and application errors receive request-aware rendering. Generic fallback errors use the same path. |
| Error-page preference forms lacked usable CSRF state; the first shared rate-limit layout also omitted it and board-default context. | Standalone rendering lacked request context. Request-aware rendering now supplies signed form tokens through the existing cookie/security policy, including rate-limit notices. Board-scoped ban notices also retain their board default. |
| Light-theme success banners/new-reply indicators and generic admin borders/error accents bypassed theme colors. | Shared components contained fixed terminal-era colors. Shared colors now derive from the active palette; existing intentional per-theme styling is retained. |
| Without JavaScript, builder previews inherited the surrounding site's palette/font/spacing, and selecting a preset did not apply its defaults. Metadata also ignored its own configured color in the live preview. | Preview styles and preset application existed only in JavaScript, and metadata reused the muted-text field. The server now supplies preview colors, typography, and density; both paths use the configured metadata/notice colors. A native “Save preset defaults” action applies the selected preset. Regular save preserves manual edits. |
| Restored search/other pages could keep an old theme after preferences changed; the dedicated persisted-pageshow baseline test failed. | History revalidation was gated on activity badges/page metadata. Persisted documents now use the same revalidation path regardless of page type. |
| Keyboard focus lost its outline when WebKit focused the search input; the complete browser suite caught this. | Generic controls and Forest, Blue Sky, and Deep Orbit overrides suppressed the existing shared `:focus-visible` outline. Removing those suppressions restores the theme-aware indicator without weakening the browser assertion. |

## Files changed

| File | Purpose |
|---|---|
| `src/db/themes.rs` | Transactional CSS/metadata retargeting on rename; old light-builder compatibility; regression tests. |
| `src/error.rs` | Carries safe application error/notice content for request-aware rendering. |
| `src/handlers/admin/auth.rs` | Test-only isolation: the lockout fixture now uses its own peer address so it cannot lock out parallel successful-login tests. Production authentication is unchanged. |
| `src/handlers/admin/backup/http.rs` | Themed restore redirect content while retaining the existing Refresh response. |
| `src/handlers/admin/settings/themes.rs` | Explicit native preset-save action and its regression test. |
| `src/handlers/board.rs` | Visitor theme on ban pages; exposes existing preference decoding to request rendering. |
| `src/handlers/board/access_preferences.rs` | Dynamic custom CSS revalidation; reuse of existing CSRF helper. |
| `src/handlers/board/media.rs` | Missing-post errors use request-aware application errors. |
| `src/handlers/board/reports.rs` | Themed appeal responses and correct error response path. |
| `src/middleware/rate_limit.rs` | Shared themed rate-limit page; allows theme assets needed to render it. |
| `src/server/server/headers.rs` | Request-aware themed error/notice rendering before compression. |
| `src/server/server/router.rs` | Installs that middleware and a themed unmatched-route fallback. |
| `src/templates/admin/appearance.rs` | Server-rendered builder preview and native preset-save control. |
| `src/templates/mod.rs` | Consistent snapshot-based theme resolution, fallback presentation, custom-only stylesheet links, ETags, and themed standalone renderers; regression tests. |
| `src/theme_builder.rs` | Native-control scheme, builder metadata validation, and regression tests. |
| `static/admin.css` | Theme-aware generic admin borders/error colors; preserves shared keyboard focus outlines. |
| `static/admin.js` | Validated live preview values and consistent replacement of the initial preview style. |
| `static/main.js` | Unified live selection, staged/cancelable custom stylesheets, state synchronization, and history restoration. |
| `static/style.css` | Theme-aware shared banners/indicator colors; restores keyboard focus outlines; correct default-theme documentation. |
| `static/theme-init.js` | Removes duplicate theme application; retains progressive-enhancement class initialization. |
| `tests/e2e/theme-system.spec.ts` | Theme state, failures/races, CRUD/cache/fallback, no-JS builder, page/contrast/focus matrix. Local-only under the repository's existing ignore policy. |
| `tests/e2e/theme-history.spec.ts` | Persisted-history event contract and fresh-context cookie/storage precedence. Local-only under the existing ignore policy. |
| `docs/theme-system-audit-2026-09-06.md` | This audit and validation record. |

## Regression coverage and exercised surfaces

New Rust tests cover strict enabled-slug matching/default precedence, CSS selector renaming, builder metadata/advanced-selector migration, compatibility for old generated light CSS, native-control schemes, invalid metadata colors/radii, and explicit no-JavaScript preset saving versus manual saving.

The browser matrix covers Forest, Blue Sky, Deep Orbit, Terminal, DORFic, ChanClassic, Frutiger Aero, NeonCubicle, and FluoroGrid; custom raw and generated themes; create/edit/rename/delete/disable; site and board defaults; all-disabled fallback; invalid/deleted cookies; cookie restoration in fresh contexts; conflicting/unavailable local storage; live selection, failed stylesheets, rejected saves, rapid choices, reload/navigation, and persisted-history revalidation.

Representative themed routes include home, board index, catalog, search, thread, admin login/panel, ban notice, missing board, and unmatched route. Existing application suites cover posting/replies, dynamically inserted posts/navigation, polls, reports, edit/delete/confirmation dialogs, media controls, authentication, moderation, setup, protected boards, appeals, errors, backup/restore redirects, responsive layouts, and keyboard behavior. Color/readability assertions supplement screenshots; they are not a claim that arbitrary administrator CSS meets WCAG.

Browser projects: desktop Chromium, Firefox, WebKit; mobile Firefox (390×844) and iPhone-13 WebKit emulation; desktop Firefox with JavaScript disabled. The existing theme workshop layout matrix additionally exercises widths 320, 360, 390, 430, 768, 1024, 1280, and 1440 pixels. Selected Forest, Blue Sky, and Terminal thread screenshots were visually inspected.

## Validation record

Final Rust/frontend results:

| Command | Result |
|---|---|
| `cargo fmt --all --check` | Passed. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed without lint suppression. |
| `cargo test --workspace --all-features` | Passed: 1,175 library unit tests, 1,175 binary unit tests, 5 CLI integration tests, 1 documentation test; zero failures/ignored tests. |
| `cargo build --locked --bin rustchan-cli` | Passed; browser tests use this rebuilt application. |
| `cargo deny --locked check` | Passed: advisories, bans, licenses, and sources; existing duplicate-version warnings remain. |
| `node --check static/main.js`, `node --check static/admin.js`, `node --check static/theme-init.js` | All passed. |
| `git diff --check` | Passed. |

Final complete browser result: **588 passed, 384 skipped, 0 failed, 0 flaky**, across **972 cases in 24.4 minutes**. The process exited successfully and the JSON report contains no runner errors. Playwright retries remained disabled. The final preview and rate-limit regressions each also passed in all 6 projects before the complete run began.

| Browser project | Passed | Skipped | Failed |
|---|---:|---:|---:|
| Chromium desktop | 129 | 33 | 0 |
| WebKit desktop | 91 | 71 | 0 |
| Firefox desktop | 89 | 73 | 0 |
| Firefox mobile | 89 | 73 | 0 |
| WebKit iPhone emulation | 100 | 62 | 0 |
| Firefox without JavaScript | 90 | 72 | 0 |

Reproducible final browser command:

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_AUDIT_OUTPUT=output/playwright/theme-accepted-complete npx playwright test --workers=6
```

The build command above completed before this run. Its JSON results and HTML report are under `output/playwright/theme-accepted-complete/`; earlier completed failure evidence is retained under `output/playwright/theme-complete-suite/`. Rust validation logs are `/tmp/rustchan-theme-{fmt,clippy,tests,build}-closure.log`.

Prior evidence, retained rather than hidden:

- Initial browser regressions: 3 expected failures reproducing URL rewriting, lost error preference, and disabled Terminal selection.
- Dedicated theme matrix after fixes: **46 passed, 2 intentional JavaScript-only skips** across six projects.
- History regression: failed before the fix; **5 passed, 1 no-JavaScript skip** after it.
- Complete Rust suite before the final additional regression cases: **1,172 + 1,172 unit tests, 5 CLI integration tests, 1 documentation test passed**.
- Formatting initially required changes; only touched Rust files were formatted and the check rerun.
- Strict Clippy initially identified an avoidable slice and redundant closure; both were fixed, without lint suppression.
- One early targeted Rust run encountered an existing shared-global theme-fixture race in an admin template assertion. Later complete runs passed; browser tests each own an isolated application/database.
- One new Rust test initially assumed the NeonCubicle builder preset was light. Inspection confirmed that preset is intentionally dark; the test was corrected to its actual baseline palette.
- New browser harness failures were corrected explicitly: synthetic indicators now have unique test IDs; tests use static TypeScript imports; save rejection uses an explicit CSRF-style 403 so the suite's strict unexpected-5xx guard remains active.
- An exploratory full browser run was interrupted after **152 passed, 54 skipped**, with 2 in-flight tests interrupted and 734 not run, to restart the complete suite against the finished implementation. It is not counted as a completed validation pass.
- The first completed 966-case browser run ended with **580 passed, 384 skipped, 2 failed**. One failure demonstrated the focus-outline defect above. The other was a new test's storage initializer running on `about:blank`; it now writes only on the application's origin, retaining strict browser-error checks.
- A later Rust run passed all 1,175 library tests but failed one binary successful-login test (1,174 passed): the parallel lockout test shared its loopback peer. Giving the lockout fixture a unique peer eliminated the demonstrated interference; the subsequent complete Rust run passed as recorded above.
- The focused theme/history rerun after the focus and harness corrections passed **63 tests with 3 JavaScript-only skips**. Final review then completed server preview typography/spacing/notice colors and added corresponding browser assertions. A full run started during that review was explicitly interrupted after **19 passed, 4 skipped, 4 interrupted, 939 not run**; it is not counted as a pass.
- The strengthened preview test passed in all **6 browser projects**. A further full run was interrupted at **36 passed, 9 skipped, 4 interrupted, 917 not run** when request-path review identified the rate-limit CSRF/board-default gap. Dedicated rate-limit assertions now verify cookie precedence, board fallback, signed form state, and the existing history-return hook. A new library renderer's visibility was corrected to match the existing public notice renderers after Clippy identified it as unreachable in the library target; no lint was suppressed.

## Limits

Physical iOS/Android devices, actual Safari/Chrome installations, forced-colors/high-contrast OS modes, and assistive-technology reading order were not exhaustively tested. Browser projects use Playwright's bundled engines and emulated viewports. JavaScript-free history snapshots cannot execute a restoration handler; fresh navigation/reload remains server-authoritative.

The dedicated theme-history regression dispatches a persisted `pageshow` event to deterministically verify the restoration contract; actual BFCache admission varies by browser. The existing full-suite navigation tests additionally exercise natural back/forward restoration. Visual/contrast checks cover representative controls and routes, not every possible combination of administrator content and browser-native UI.

Raw administrator CSS remains an intentionally powerful customization surface. Arbitrary custom palettes, unusual selectors, external resources blocked by CSP, and deliberate unreadable overrides cannot be guaranteed visually usable. Rename retargeting supports exact `data-theme` attribute selectors (quoted/unquoted and optional whitespace); semantic selectors or references embedded in arbitrary author-defined CSS conventions still require author review. No CSS-parser dependency was added.

The 384 browser skips retain existing project-specific and environment-dependent conditions (for example live Tor/onion and media-toolchain opt-ins), plus 3 JavaScript-only cases in the new regressions. Physical Windows/Linux builds and optional live Tor/media-toolchain suites were not run. No production runtime data was used; browser mutation tests use isolated disposable instances. Existing local-only browser-test infrastructure remains ignored, as configured by this repository.
