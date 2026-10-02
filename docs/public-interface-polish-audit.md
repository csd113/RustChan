# Public interface polish audit — 2026-09-30

Current video pipeline note (2026-10-02): FFmpeg extracts PNG video frames; Rust encodes their WebP thumbnails. FFmpeg WebP encoder detection and installation requirements have been removed. WebM conversion retains VP9 + Opus with independent AV1 decoder/encoder diagnostics. Historical validation results below describe the implementation at the time of that audit.


Scope: home, board indexes, catalog, threads, search, archives, preferences, media, and polls. Admin changes were excluded; shared behavior was checked with the maintained regression harness. ChanNet is already removed from this branch; no obsolete infrastructure was restored.

The initial `interface-audit` working tree was clean. At the audit handoff, all changes were local and uncommitted. No dependencies, framework, route renames, deployment, or visual redesign were introduced.

## Baseline and findings

The documented build and maintained `tests/e2e` workflow launched the actual RustChan binary in unique loopback runtimes, with synthetic SQLite data, uploads, credentials, settings, and disabled Tor/background maintenance. Existing protections were examined first: server-side CSRF, one-use CAPTCHA, upload limits, submission-token atomicity, input recovery, native forms, quote highlighting, thumbnail fallbacks, dialog containment, and cookie-backed preferences were retained.

The initial Chromium posting/upload/idempotency baseline passed all nine selected tests. Findings below are functional defects; no subjective visual changes were necessary.

| Reproduction | Impact | Contained fix and regression |
| --- | --- | --- |
| Create a native thread with three poll options and a nondefault duration, then submit a wrong CAPTCHA. | Rejection retained body/name/subject but reset the poll. | Retain escaped question/options and original duration controls; reopen the populated poll. `public-polish-native.spec.ts`, forms unit tests. |
| Reject a native post with attachments. | Browser clears file controls after navigation without explaining reselection. | Show attachment reselection advice only when files were submitted; preserve text/sage. Native posting regressions. |
| Submit a parsed reply after its thread is locked/deleted, or with invalid CSRF. | Standalone failure lacked a copyable submitted draft. | Preserve status/security checks and display an escaped readonly draft plus recovery advice; dedicated ban/appeal pages remain intact. Native posting regressions. |
| Reject a native oversized multipart request before parsing finishes. | Firefox could reset the connection; after receiving 413, Back could restore an empty form. | Retain received text and read trailing bounded poll controls for an escaped copyable draft. One shared 8 MiB/1 s discard allowance includes excess upload and remaining controls; no excess media is staged, original rejection and aggregate/field/concurrency limits remain enforced. Native 413/copy/fresh-form/smaller-file retry regression plus cap/timeout tests. |
| Select a file and fill text/poll fields, then choose a new CAPTCHA. | Full-page refresh silently discards unsent choices and attachments. | Refresh only CAPTCHA fields in place with JavaScript, retain the submission token/files/other controls, explain refresh failure; native refresh warns about copying before navigation. Posting regressions. |
| Upload receives an unconfirmed HTTP 200, aborts, times out, or fails synchronously. | Unconfirmed response could reload away input; abort was silent; a synchronous send exception could leave controls busy. | Retain input, restore controls on failure, give an uncertainty/check-before-retry action, and keep success navigation tied to the server redirect. Actual SQLite-delayed request and token-reusing retry regression. |
| Trigger a second submit while an upload is pending. | Busy handler could fall through into native submission. | Prevent pending submissions; keep controls busy through confirmed navigation. Posting regression. |
| Upload progress event has no computable total. | Full progress bar could imply measured completion. | Show unmeasured upload state; announce upload/server-wait status using actual upload completion, without invented server percentages. |
| A validation response returns submitted reply text while another tab has written stale autosave. | Client restore could overwrite the newer server-rendered text. | Prefer nonempty server-rendered text over shared autosave. Posting regression. |
| Activate a local quote/backlink and use browser Back, or hover a quote into a dense post. | Enhanced clicks bypassed native history; an oversized preview could cover its trigger. | Retain native fragment/history navigation and constrain previews to available viewport space. Reading regression. |
| Activate a search result post number or a quote whose target is outside the results. | Links had no complete-thread context and could appear deleted. | Context-aware search links resolve through existing thread/post routes; preserve modified-click navigation. Reading and renderer regressions. |
| Vote in a poll. | Existing `#poll` redirect had no target element. | Add the target and header scroll margin. Reading regression. |
| Upload a filename longer than the existing 100-character bound. | Truncation could remove the extension. | Preserve a bounded extension and Unicode stem with unchanged forbidden-character replacements and length limit. Reading and sanitization regressions. |
| Load a stored audio/video file the browser cannot play, including before the application script finishes loading. | Native controls could remain unusable without an application explanation. | Add a compact announced failure message and original-file download action; recognize existing failure state when wiring and clear it after successful reload. Real WAV/H.264 playback, corruption, download, and recovery regressions; use primary theme text for readable captions. |
| Add a sage reply to an older thread and select catalog “last reply”. | Sorting reused the bump timestamp and ignored the newest reply. | Compute the latest post time in the existing grouped thread query and use it for this catalog choice; preserve bump ordering, image counts, and saved preferences. Catalog and database regressions. |
| Open preferences by keyboard at mobile width, dismiss it, or resize an open panel. | Focus remained outside the overlay; mobile panel lacked dialog semantics/containment. | Use the existing dialog trap with mobile-only metadata, initial Close focus, and restoration after native details has rendered closed. Visual regression. |
| Open a mobile composer with reduced motion enabled. | Explicit smooth scroll ignored the preference. | Shared scroll behavior also covers errors and new-reply navigation. Visual regression. |

## Evidence and verification

Generated reports, diagnostics, traces, videos, runtime logs, and before/after screenshots stay ignored under `output/playwright/public-polish/`. Representative screenshots were visually inspected, including native poll recovery, uncertain upload response, dense Unicode threads, search context, portrait plus audio attachments, report dialogs, preferences, and native unavailable/oversized states.

Built-in themes: Forest, Blue Sky, Deep Orbit, Terminal, DORFic, ChanClassic, Frutiger Aero, NeonCubicle, and FluoroGrid. Each was exercised at 1280×800, 320×720, and 667×320 in the configured browser projects. The theme audit checks document overflow, post/metadata readability, focus, and dialog controls while retaining each theme's appearance.

Final Rust/source validation:

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed. |
| `cargo check --workspace --all-targets --all-features` | Passed. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed without new lint suppressions. |
| `cargo test --workspace --all-features` | Passed: 1,227 library + 1,227 binary + 5 runtime integration + 1 documentation test = 2,460 executions; none failed or ignored. |
| `cargo build --locked --bin rustchan-cli` | Passed; refreshed embedded CSS/JavaScript for the browser runs. |
| `cargo test --lib --all-features rejected_upload` | Passed all 7 size/field/aggregate/deadline/draft recovery regressions. |
| `cargo test --lib --all-features listing_last_post_includes_non_bumping_text_and_audio_without_counting_them_as_images` | Passed; active/archive aggregates and image counts retained. |
| `node --check static/main.js` and `git diff --check` | Passed. |
| `node tests/e2e/media-toolchain-check.mjs --self-test` | Passed. |
| `node tests/e2e/media-toolchain-check.mjs` | Failed host preflight: FFmpeg lacks `libwebp`; full optional media-toolchain suite not launched or counted as passed. |

Final browser runs used `RUSTCHAN_E2E_SKIP_BUILD=1`, unique `RUSTCHAN_AUDIT_OUTPUT` directories, the freshly rebuilt binary, and the existing Playwright configuration with zero automatic retries:

| Maintained-harness selection | Passed | Explicit skips | Failed | Evidence directory |
| --- | ---: | ---: | ---: | --- |
| `npx playwright test tests/e2e/public-polish-native.spec.ts --workers=1` | 56 | 0 | 0 | `native-final-latest/` |
| `npx playwright test tests/e2e/public-polish-posting.spec.ts tests/e2e/public-polish-visual.spec.ts --workers=2` | 67 | 24 | 0 | `posting-visual-final/` |
| `npx playwright test tests/e2e/public-reading-polish.spec.ts tests/e2e/public-catalog-polish.spec.ts --workers=1` | 59 | 4 | 0 | `reading-complete/` |
| `npx playwright test tests/e2e/submission-idempotency.spec.ts tests/e2e/phase3-media-runtime.spec.ts tests/e2e/phase4-empty-error-permission.spec.ts --workers=2` | 21 | 35 | 0 | `shared-final/` |
| `npx playwright test tests/e2e/media.spec.ts --project=chromium --project=webkit --workers=1` | 6 | 0 | 0 | `media-maintained-final/` |
| **Final total** | **209** | **63** | **0** | No browser launch failures or automatic retries. |

The five new public specs cover all seven configured profiles: Chromium, WebKit, Firefox, mobile Firefox, mobile WebKit, Firefox without JavaScript, and Chromium without JavaScript. Their total is 182 passes and 28 enhancement-only skips. Native posting tests explicitly disable JavaScript in all seven profiles. Real WAV and H.264 native playback was verified in all seven; enhanced corruption/download/recovery runs in the five JavaScript profiles. All nine themes were exercised at desktop, narrow portrait, and short landscape widths; new audio error text/download actions also met the measured 4.5 contrast threshold in all nine themes.

Additional broader maintained-suite evidence from earlier in implementation: 196 passed / 140 explicit project/mode skips across twelve specs (`regressions/`), plus eight passed / thirteen applicability skips for search, board pagination, preferences, sage and poll journeys (`public-journeys/`). These are separate earlier runs, not added to the final total. Shared error/permission, media and atomic-submission paths were rechecked against the final implementation as listed above.

Earlier failures remain archived. They include the reproduced Firefox transport/history failures, the DORFic caption contrast defect, an early media failure before script initialization, test fixtures that did not match route body limits, pointer-click focus assumptions in WebKit, and intermittent native modified-click behavior under concurrent runs. Corrected keyboard tests and an unchanged-timeout clean reading matrix passed; click diagnostics verified real Meta input with `defaultPrevented: false`. No browser skips or failed attempts were counted as passes.

Representative before screenshots: `before-chromium-chanclassic-thread-320.png`, `before-chromium-report-landscape.png`, native recovery captures under `native-before-poll/`, and catalog reproduction under `catalog-before/screenshots/`. Representative inspected after screenshots:

- `after-native-oversized-chromium.png` and `after-native-oversized-mobile-firefox.png`: received draft/poll and explicit reselection recovery.
- `captcha-refreshed-after-chromium.png` and `uncertain-response-after-chromium.png`: preserved enhanced composer.
- `after-chromium-chanclassic-thread-320.png` and `after-mobile-webkit-preferences-landscape.png`: density, wrapping and reachable focused controls.
- `reading-complete/screenshots/chromium-wav-corrupt-dorfic.png` and `reading-complete/screenshots/mobile-webkit-video-corrupt.png`: readable native-media failure actions.
- `reading-complete/screenshots/firefox-catalog-last-reply.png` and `reading-complete/screenshots/mobile-firefox-search-page-restored.png`: sorting and restored search context.

All paths in this section are relative to `output/playwright/public-polish/`; each run also retains its `results.json`, HTML report and diagnostics.

## Changed files

- `src/db/threads.rs`: latest-post aggregate and database regression.
- `src/models.rs`: latest-post time in the existing thread model.
- `src/handlers/board/create_thread.rs`, `src/handlers/thread.rs`: posting error and draft recovery.
- `src/handlers/mod.rs`: shared bounded multipart recovery and parser regressions.
- `src/handlers/render.rs`: existing test factory updated for the thread field.
- `src/templates/board.rs`, `src/templates/thread.rs`: search context, catalog sort data, poll anchor, renderer regressions and test factories.
- `src/templates/forms.rs`: retained poll/draft controls, native recovery advice and upload status semantics.
- `src/utils/sanitize.rs`: extension-preserving bounded filename sanitization and regressions.
- `static/main.js`, `static/style.css`: posting/CAPTCHA recovery, quote history/previews, mobile dialog focus, reduced-motion scrolling, catalog sorting and readable media failure feedback.
- `tests/e2e/public-polish-posting.spec.ts`, `tests/e2e/public-polish-native.spec.ts`, `tests/e2e/public-reading-polish.spec.ts`, `tests/e2e/public-catalog-polish.spec.ts`, `tests/e2e/public-polish-visual.spec.ts`: maintained-harness public journey regressions.
- `CHANGELOG.md`, `docs/public-interface-polish-audit.md`: change summary, reproductions, evidence and coverage limits.

## Coverage boundaries

- Mobile Firefox/WebKit are emulated viewport/touch/user-agent profiles, not physical devices. Physical mobile keyboards, screen-reader announcement order, and browser-native zoom UI were not exercised.
- No-JavaScript CAPTCHA refresh is a document navigation: its visible warning explains copying unsent content first. Browsers cannot programmatically restore file selections into a newly rendered native form.
- Malformed, stalled, or sufficiently large rejected streams may exceed the bounded recovery allowance. Only received controls can be saved; partial recovery says so explicitly. Browser Back is unreliable after some native upload errors, so delivered error pages provide copyable drafts. Abrupt transport failures can prevent an error page from arriving at all. Upload size, aggregate, field, timeout, CSRF, and concurrency checks remain enforced.
- Full optional media-toolchain preflight cannot pass on this host because FFmpeg lacks `libwebp`; it is not counted as a passing codec/transcoding matrix.
- CAPTCHA expiry rejection is checked through the real consumed-challenge HTTP path and existing timestamp-expiry Rust tests. A five-minute elapsed browser expiry wait is not counted as exercised.
- Skips are reported separately from passes. Superseded attempts retain evidence of test-authoring assumptions, stale binary runs, native-details focus timing, and browser interception behavior; they are not final passes or silently retried tests.
