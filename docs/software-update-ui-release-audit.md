# Software Updates UI and v1.6.0 release audit

Audit date: 2026-10-01. Candidate repository: `csd113/RustChan`.

## Prerequisite and scope

The actual updater implementation task was `01a0f571-4bb7-7792-bd00-42f61a935f3d`
(`Read pasted text file`), completed turn `01a0f571-5bf6-7b31-9ff0-7b05af358744`.
Its final handoff is retained locally at `output/software-update-handoff.txt`.
The independent audit began only after that task completed and its RustChan
processes and validations stopped. The older public-interface audit was a separate
completed task. Unrelated project processes were left alone.

The release comparison is the actual published `v1.5.0` through the candidate
history and completed updater changes. The intended release branch is `main`.
Notes cover the integrated container, correctness, terminal, admin, public posting/
reading, browser infrastructure and updater work; no unmerged branch is listed as
shipped. Cargo and its lockfile are prepared for `1.6.0`; README screenshot
provenance remains its actual v1.5.0 Docker capture.

## Browser audit

A real headed Chromium session used a disposable loopback server with a copied
binary, temporary data directory, test credentials and container/check-only mode.
Native confirmation was injected from the exact production Rust renderer into the
authenticated real admin shell. Install submissions were blocked or intercepted;
no production server or native updater was invoked. The maintained Playwright
regressions use the same isolated data and exact native-renderer approach.

All nine themes were checked at 1280, 390 and 320px: Forest, Blue Sky, Deep Orbit,
Terminal, DORFic, ChanClassic, Frutiger Aero, NeonCubicle and FluoroGrid. Both
deployment-managed and verified native forms fit their cards without document
overflow; password, confirmation and actions remain reachable. Focus outlines
measure at least 2px; mobile action heights measure 42px. Safari's default keyboard
preference uses Option-Tab to reach buttons; regular input Tab order is preserved.
Incorrect confirmation is rejected, and the original session/CSRF/password gates
remain in place. The layout needed no style changes. A real success-state
regression did require a focused behavior fix: the native transaction retains
its discovery journal after installation, so matching `Available` alone left the
admin notice visible for the version now running. A shared stable-version check
now gates both the notification and native install control. Current, older,
prerelease and malformed candidates cannot offer a stale installation; rollback
still shows notice of a genuinely newer release. A failing browser regression
captured the old behavior before the correction.

Availability, no update, rejected verification, unavailable release checks,
unsupported/container deployments, all eleven active transaction phases, four
terminal outcomes, idle state and malformed persisted state were exercised.
Long failure text wraps at 320px. The actual release check is a native document
POST/navigation, not a separate synthetic `checking` transaction phase; an
intercepted pending check and failed-check retry were exercised without reaching
GitHub. Enhanced success/rollback and real temporary server outages/reconnection
use persisted state; native POST/reload paths are exercised without JavaScript.

Admin-only disclosure was verified on the authenticated panel and status/check/
install routes. Anonymous contexts and forged public-session cookies cannot read
update data. RustChan has no ordinary authenticated account or reduced moderator
role: only full administrator sessions authorize these routes. Public `/readyz`
reports only readiness and the running package version; the application endpoint
is `/readyz`, not `/ready`.

The surrounding audit covers all 22 admin tasks, narrow local table scrolling,
dashboard/state contrast, configuration navigation/persistence, moderation,
polling lifecycle and manual backup/restore. A real small backup in the disposable
manual instance opened its existing progress dialog: initial focus and Tab/Shift-
Tab containment were verified at 320px, and Done closed it and reloaded Backups.
Escape does not dismiss that existing progress dialog; this is not recorded as a
passed Escape-dismissal test. Updater snapshots remain metadata-only in the
existing Backups task.

## Evidence

Local ignored evidence is under `output/playwright/v160-ui-audit/`:

- `before/`: 54 theme/width screenshots, nine native-card captures, a visually
  reviewed contact sheet, manual measurements and a backup-dialog screenshot.
- `regressions/`, `harness-recheck/`, `native-keyboard-recheck/`: initial expanded
  attempts and their traces are retained. Added audit-fixture failures were
  corrected by forcing real document navigation, using the actual engine fixture
  for WebKit keyboard behavior, and beginning functional submission from a fresh
  document after the viewport matrix. Assertions, timeouts and retries were not
  relaxed. The later stale-notice regression required the focused semantic fix above.
- `final-matrix/`: the combined surrounding-admin/public release audit, 235 passed,
  87 explicit conditional skips, no failures (322 executions). This run preceded
  the later stale-notice correction.
- `updater-final-state/`: corrected updater and real backup/restore, 68 passed,
  two intentional no-JavaScript reconnect skips, no failures (70 executions).
- `check-submission-final-state/`: final native check pending/failure/retry,
  seven passed across every browser profile, no failures.
- `validation-final-state/`: final source commands/results; previous attempts are
  retained in `validation/`.

The theme images retain the existing layout. `stale-badge-before/` contains the
failing assertion, DOM trace and screenshot for the completed-release notice;
`updater-final-state/` contains the corresponding corrected result and rollback
regression. The left index is independently scrollable, so the full-page screenshot
alone does not expose every navigation entry; the assertion and trace establish
badge presence. No unrelated visual redesign is claimed.

## Validation and release gate

Final source validation passed:

| Gate | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| Locked workspace/all-target/all-feature check | Passed |
| Strict workspace/all-target/all-feature Clippy (`-D warnings`, all/pedantic/nursery/cargo) | Passed on host Rust 1.98.1 and Linux ARM64 Rust 1.91 |
| `RUST_TEST_THREADS=1 cargo test --locked --workspace --all-features` | 2,509 passed: 1,251 library, 1,251 application, five CLI, one renderer and one doctest; none failed or ignored |
| Locked debug and optimized release builds of both binaries | Passed |
| Locked dependency policy | Passed; permitted duplicate dependency advisories remain informational |
| Offline update packaging/signing | Seven passed, including version/tag and architecture rejection |
| Admin/public JavaScript syntax, workflow YAML, browser harness checks, whitespace | Passed |
| Final corrected updater and backup/restore browser coverage | 75 passed, two explicit no-JavaScript automatic-reconnect skips, zero failures across 77 executions in two runs |

The combined surrounding-admin/public baseline separately passed 235 cases with
87 explicit conditional skips and no failures (322 executions). It preceded the
stale-notice fix; final updater/state/browser results above validate that correction.
All seven maintained profiles launched and ran: Chromium, WebKit, Firefox, mobile
Firefox, mobile WebKit, Firefox without JavaScript and Chromium without JavaScript.

Local Linux/Rust 1.91 strict lint passed on the final source with a read-only
repository mount. Earlier full Linux Docker validation passed both library and
application suites but repeatedly timed out in two CLI bootstrap database pools
after those suites. Isolated unchanged CLI tests passed five of five; the attempted
one-connection fixture override did not resolve the full-run failure and was removed.
The final CLI change only adds initialization stderr to a failure assertion. This
local full Docker run is not a pass or proof of native CI behavior. Required native
GitHub checks must validate the final candidate before protected integration.

The first PR head's required Windows smoke exposed three native-only helpers
compiled without Unix guards; strict dead-code enforcement rejected the build.
The correction gates those helpers and their hash import with `cfg(unix)` without
relaxing lint or changing native behavior. Host strict lint passed, as did 46 updater/template tests and the exact native
renderer integration test. Every required native/browser check must run on the
new final PR head. Earlier-head passes do not satisfy that integration gate.

The implementation task's earlier broad matrix was 1,290 passed, 913 explicit
skips and two initial failures; both passed its focused final recheck. Its original
full headless run is not described as an entirely green run. One unchanged
headless Firefox modified-click/new-tab limitation was reproduced outside RustChan
on this macOS host; headed Firefox passed the original assertion. Final independent
results above are reported separately.

Native systemd/polkit privileges, actual migration binaries, process kills/reboot
and complete host recovery still require disposable Linux/systemd staging before
production use. Renderer fixtures, injected transaction/service failures and unit
syntax checks do not claim those live-host checks. Optional media-toolchain
coverage previously lacked host FFmpeg `libwebp` support and is not claimed here.

The candidate uses the normal protected `main` pull-request route. Quality, Tests
and the Linux ARM64, macOS Apple Silicon and Windows x86_64 platform checks must
succeed on the final head before integration; protection is not bypassed. Browser
CI supplies the native Linux maintained-suite result separately from this local
macOS audit.

Stable publication is held for the operator-provisioned signing secret. Repository
secret-name listing succeeded and was empty at audit time; no v1.6.0 tag or release
was created by this audit. A successful source integration is not a published
binary release. The existing signing gate cannot be skipped or converted to an
unsigned/prerelease substitute.

The required stable release signing secret is `RUSTCHAN_UPDATE_SIGNING_KEY`.
Its private Ed25519 PEM is operator-provisioned, kept out of Git and copied offline;
the native trusted public key is independently authenticated. The exact setup is
in `docs/software-updates.md` under Release signing. No real signing key was
generated, read, uploaded or configured by this audit. Ephemeral synthetic signing
keys belong only to the existing offline packaging tests.

## Browser CI findings and focused corrections

The first complete native CI run on `d975b906c09ce5ef3114ee18b47ea2611393ead3`
passed Quality, Tests (2,509 cases), all three platform smoke jobs and Dependency
Audit. Chromium and Chromium no-JavaScript browser jobs passed. Firefox and WebKit
browser jobs failed and therefore that head was not integrated or released.

Firefox's sole failure was a real native-audio test: intact PCM WAV data reached
the browser, but `OnMediaSinkAudioError` stopped playback without an output device.
An isolated official Playwright 1.63 Linux container reproduced the media-sink
failure and then passed real playback with a PulseAudio virtual sink. Both normal
and deep browser CI now configure that ephemeral output. Audio is not mocked or
muted; the playback and error-recovery assertions remain intact.

WebKit exposed one application focus regression: resizing open mobile preferences
back to desktop focused a summary before WebKit had painted its visible layout.
The existing painted-layout focus restoration now covers that transition and
retains guards against focusing a reopened mobile modal. The original keyboard
regression passed on Linux WebKit; its reviewed after screenshot is
`output/playwright/public-polish/v160-focus-after-webkit-preferences-desktop-restored.png`.
The original GitHub failure screenshot/trace remains in the downloaded WebKit
artifact. This is a focused shared-script correction with no redesign.

Two harness races were corrected: report submission now waits for the distinct
`reported=1` redirect instead of an already-matching thread URL, and accepted CSRF
submission waits for its thread redirect before returning to the board. The report
row, status, persistence, token renewal and tamper-rejection assertions remain.
The updater theme matrix is split by viewport into three bounded tests, preserving
all nine themes at 1280/390/320px, screenshots, focus and overflow assertions,
90-second budgets and zero retries. Linux WebKit passed all four originally failed
areas, including all three viewport/theme cases.

Supplemental Linux engines connect through Playwright's loopback proxy to the
isolated local app. Full remote test results are not substituted for native CI:
`Download.path()` is unavailable through that connection, and an intentional
outage can report a SOCKS error instead of direct connection refusal. Those adapter
limitations remain explicit failures in the supplemental run; native GitHub jobs
must pass without weakening their assertions or changing their exclusions.

Supplemental Linux results: 54 passed, two existing no-JavaScript skips and four
explicit remote-adapter failures (three local download-path calls and one outage
error classification). Separate Linux WebKit focus evidence passed 1/1. Strict
host lint, JavaScript syntax, all workflow YAML, whitespace and the optimized
both-binary rebuild passed after the focused corrections. The complete protected
GitHub native/browser jobs still gate the final PR head.

The final focused macOS run after these browser corrections passed 101 cases with
four existing no-JavaScript enhancement skips and no failures (105 executions
across all seven profiles). It covers all eleven updater cases plus moderation,
preferences focus, signed public CSRF recovery and real native WAV playback.
Evidence is retained at `output/playwright/v160-ui-audit/mac-browser-fixes/`.
Offline packaging/signing remains 7/7; the optimized CLI identifies itself as
`rustchan-cli 1.6.0`. The supplemental Linux container was stopped after testing.

## Final session-revocation fixture correction

On `0bf98e3`, every required native job and Dependency Audit passed. Full native
Linux browser results were: Firefox 173 passed/148 conditional skips; WebKit 175
passed/146 skips; Chromium no-JavaScript 164 passed/157 skips. Chromium had 282
passes, 38 skips and one actual failure. Its three intentionally failing harness
contract cases behaved as declared and are not additional unexpected failures.

Chromium's account-reset behavior assertions passed, but the target admin tab
polled Site Health after its session was intentionally revoked. The server
correctly returned 403; strict diagnostics rejected this unrelated background
request. The trace records reset POST 303 at 09:13:52.768Z and target health/jobs
403 at 09:13:57.252Z. No application authorization defect was found.

The fixture now navigates the target to a public page without logging out before
reset. Added assertions prove the client retains its admin cookie and the server
still has exactly one target session before mutation. Existing reauthentication,
zero sessions after reset, protected-panel 403, recovery login and redaction
assertions remain intact. No error exclusions, timeouts, retries or skips were
added. Three independent repetitions across seven local profiles passed 21/21;
actual Linux Chromium repetitions passed 3/3 with strict diagnostics. Evidence is
in `output/playwright/v160-ui-audit/session-revocation-fix/` and
`linux-session-revocation-fix/`; the original failure trace/screenshot remains in
the downloaded Chromium artifact. Application code is unchanged by this fixture
correction, and full native/browser CI must pass again on the new final head.
