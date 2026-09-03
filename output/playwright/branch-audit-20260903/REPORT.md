# RustChan branch play-test report

**Overall: PASS WITH CAVEATS.** No regression attributable to the current branch was found. Running public, posting, administrative, CSRF/session, backup/restore, networking, media, persistence, and no-JavaScript workflows passed. The caveats are an existing empty-site statistics bug and the environment/coverage limits below.

No tracked source, test, configuration, or dependency files were changed. No commits were made.

## Scope and environment

- Branch: `v1.4.1`, commit `e5f8f0906b93dd47b3c767dd350b020ee4e542f8`; working tree initially clean.
- Baseline inspected: `main` at `29eb55f21aab2c9c85b6035343052b4cadf01033`, plus the most recent three commits. The branch diff spans 129 files, including handler visibility/module placement, cookies, IP extraction, background work, database access, and tracing.
- Run: September 2, 2026, approximately 22:16–22:39 America/Vancouver (September 3 UTC).
- macOS 26.6.2, build 25G83, Apple Silicon/aarch64.
- Rust/Cargo 1.98.0; Node.js 22.14.0; npm 10.9.2; Playwright 1.60.0.
- Application built from the current working tree with `cargo build --workspace --all-features`; browser fixtures copied that binary into isolated runtimes. `RUSTCHAN_E2E_SKIP_BUILD=1` prevented the harness from replacing the all-features binary with a default-feature build.
- Main manual instance: `http://127.0.0.1:62305`, using a fresh `--data-dir`. Its location is recorded in `instance.json`. It was stopped cleanly; its disposable database, settings, uploads, and retained backups remain available for inspection.
- Existing browser fixtures allocated their own temporary directories and loopback ports. Existing developer data was not used or overwritten.
- FFmpeg/ffprobe 9.0.1. The default `/opt/homebrew/bin/ffmpeg` lacks libwebp; its supported fallbacks were exercised. The six real-media tests used the installed `/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg` and matching ffprobe, with WebP, VP9, and Opus support. Poppler `pdftoppm` was available for PDF previews.

## Browser and automated results

The complete existing browser suite finished in 18.1 minutes: **348 passed, 342 skipped, 0 failed, 0 flaky**. No retries were enabled. The separate real-media/opt-in upload pass added **6 passed, 0 failed**: **354 passing browser test executions in total**.

| Project | Engine version | Passed | Skipped |
|---|---|---:|---:|
| Chromium | 148.0.7778.96 | 89 | 26 |
| Desktop WebKit | 26.4 | 51 | 64 |
| Desktop Firefox | 150.0.2 | 44 | 71 |
| Mobile Firefox | 150.0.2, 390×844 | 44 | 71 |
| Mobile WebKit | 26.4, iPhone 13 profile | 56 | 59 |
| Firefox, JavaScript disabled | 150.0.2 | 64 | 51 |
| Additional real-media pass, Chromium | 148.0.7778.96 | 6 | 0 |

A separate headed Playwright CLI session reported Chrome 152.0.0.0 in its user agent. It exercised desktop and 390×844 layouts. Screenshots were inspected for functional usability.

Most skips are deliberate per-project selections, so a test skipped on one engine runs on its intended engine. Real-media and upload opt-ins were run separately. The remaining gaps are stated below. Exact reasons are in `browser-summary.json`; complete individual results and attachments are in `browser-results.json` and `browser-report/index.html`.

Commands, with artifact/report destinations omitted here for readability:

```sh
RUSTCHAN_E2E_SKIP_BUILD=1 npx playwright test --reporter=list,json,html

RUSTCHAN_E2E_SKIP_BUILD=1 RUSTCHAN_E2E_MEDIA_TOOLCHAIN=1 \
RUSTCHAN_UPLOAD_REGRESSION_E2E=1 \
RUSTCHAN_E2E_FFMPEG_PATH=/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg \
RUSTCHAN_E2E_FFPROBE_PATH=/opt/homebrew/opt/ffmpeg-full/bin/ffprobe \
npx playwright test tests/e2e/media-toolchain.spec.ts \
  tests/e2e/upload-regressions.spec.ts tests/e2e/auto-compress.spec.ts \
  --project=chromium --workers=1 --reporter=list,json
```

The media toolchain preflight and its self-test also passed with ffmpeg-full selected.

## Workflows exercised

| Area | Runtime verification and result |
|---|---|
| Public navigation | Home/board list, board index, catalog, threads, search, archive/hidden views, IDs/anchors, redirects, pagination, back/forward and reload. Passed, apart from the pre-existing empty-site statistics fallback described below. |
| Posting | Visible browser forms created threads and replies, text-only posts, Unicode/escaped content, supported attachments, polls/votes, and audio plus optional image using separate supported upload slots. Empty/oversized/invalid submissions and duplicate submission tokens were rejected safely. Reload did not duplicate posts. |
| Ownership and moderation | Own-post edit/delete controls, confirmation dialogs and fallback forms; ownership grants/deletion-token boundaries; foreign-context, expired, replied-to OP, locked and archived denials; sticky/sage/bumping/order; reports, duplicate reports, resolution, thread/post deletion, bans, unbans, appeals and moderation logs. Passed. |
| Board policy | Board creation/editing/deletion, per-media allowances/limits, cooldowns, archive/bump limits, theme overrides, view-password and post-password access, password rotation, and protected-content/media leak checks. Passed. |
| Preferences/activity | NSFW consent/hide preferences, themes, inherited/default/custom themes, new-thread/reply badges, unread clearing, hidden/pinned thread controls, cache headers/ETags, refresh and browser history restoration. Passed. |
| No JavaScript | Real Firefox contexts with JavaScript disabled exercised navigation, thread/reply creation, validation, CSRF forms, redirects, uploads, own edit/delete, reports, unlocks, preferences, polls, admin login/settings, moderation and backup/restore. Core paths did not require XHR. Script-failure degradation also passed where the existing suite covers it. |
| Admin | Login/logout, dashboard, site settings, boards, moderation, media settings, themes/theme editing, favicons/banners, site health, database checks/repair status/progress, maintenance exclusion, backup UI and job/progress endpoints. Valid forms and redirects worked; unauthenticated/cross-session/bad-CSRF requests were denied. |
| CSRF/cookies | Fresh token issuance; stable existing public token across navigation; an earlier form still valid; unrelated cookie and admin session preserved when public CSRF was regenerated; HTTP cookie flags; invalid/missing tokens; session-scoped admin CSRF; cross-session/logout rejection; invalid and simulated-expired sessions. Passed. |
| Networking | Forty-six supplemental assertions covered direct peers, untrusted forwarding headers, trusted X-Forwarded-For/X-Real-IP precedence, stored posting identity, proxied HTTPS cookie/HSTS policy, request limiting/recovery, and board-unlock failure limits. Successful unlock after two failures reset the failure count; the fifth subsequent failure returned 429 with Retry-After. Correct credentials did not bypass active lockout; a different trusted client IP was independent. |
| Native HTTPS | Real self-signed TLS listener served login and public posting. CSRF/session cookies were Secure; session cookie was HttpOnly. HTTPS-only mode disabled the main plaintext application listener. The separate HTTP redirect listener preserved path/query and replaced an untrusted Host with a configured safe host. Clean SIGTERM shutdown. |
| Media | GIF animation asset, PNG, image/audio companion uploads, video with audio, real audio/MKV variants, PDFs, generic attachments, thumbnails, waveform previews, image/video compression, background transcodes, stale original redirects, MIME/type/size errors, media expand/collapse, browser downloads and range requests. Failed uploads left no new posts/staged files in the corresponding checks. |
| Persistence | Manual instance stopped with SIGTERM and restarted against the same data. Boards, threads, posts, site settings and admin rows matched; SQLite integrity_check returned `ok`; all nine media/thumbnail files retained their hashes and returned HTTP 200. The old admin session remained valid. Saved browser state was loaded after closing/reopening the browser and still authenticated, then logout invalidated it. A restarted MP4 played to its 2-second end with readyState 4 and no media error. |

Examples of the manually inspected artifacts: `manual-thread.png`, `manual-admin-mobile.png`, `mobile-animated-reply.png`, `mobile-combo-media.png`, and `playback-after-restart.log`.

## Backup and restore

Existing browser tests passed for full directory snapshots, split ZIP output, saved board backups, scheduled backup creation, malformed/incomplete archives, and restoration into new disposable runtimes followed by restart. They checked metadata/manifests, inventories/checksums, representative boards/posts/media, site/admin configuration and auxiliary assets. Board-only restore preserved unrelated site data. Tor-key inclusion/exclusion was tested with disposable fixtures; this was not a live onion-service reachability test.

An additional 18-assertion HTTP probe exercised the manual instance after its restart:

- Set retention to two via the admin form, created three backups, and confirmed the oldest disappeared both from disk and the immediately refreshed listing.
- Inspected the newest manifest and confirmed all representative boards were included.
- Extracted the audio/image board through `/admin/backup/extract-board`.
- Missing token, wrong token and missing admin session each returned 403 without preventing the subsequent authorized download.
- Authorized download returned a valid 20,398-byte ZIP containing board JSON, image, audio and thumbnail. ZIP CRC checking passed.
- Reusing the token returned 403; the consumed ZIP and token files were removed.
- The backup progress endpoint remained available.

Evidence: `backup-token-results.json`, `backup-token-probe.log`, `extracted-combo.zip`, plus the browser backup/restore results.

## Findings and log review

**Branch regressions: none found.** Consequently, no production fixes or tracked regression tests were added. Supplemental audit scripts are artifacts, not changes to the repository's test harness.

**Pre-existing issue:** a completely empty posts table makes the homepage display “site statistics are temporarily unavailable” and log `Failed to query site stats`. The media-count `SUM(...)` expressions in `get_site_stats` return NULL for zero rows, but the result is read as non-null integers. The function body is byte-for-byte identical on `main`; comparison evidence is in `preexisting-stats-evidence.json`. Statistics rendered once posts existed. This was documented without expanding the task into an unrelated fix.

Other observed messages were explained by configuration or deliberate negative-test fixtures:

- `/favicon.ico` returned 404 before any favicon was configured. This generated a browser console resource error; favicon upload/replacement/removal coverage passed.
- The default FFmpeg lacks libwebp, triggering image-crate or generic thumbnail fallbacks. The full-toolchain pass succeeded with ffmpeg-full. One tiny MP4 was intentionally retained because its transcoded output was larger; it still played after restart.
- The base harness deliberately points FFmpeg/ffprobe at missing sentinel executables. Some unsupported HEIF and malformed PDF fixtures therefore used generic previews.
- Missing-media warnings followed the existing `phase3-media-runtime.spec.ts` test deliberately deleting an original file. Several posts shared the deduplicated asset, explaining repeated warnings in that worker's subsequent page reads.
- Expected login/origin/CSRF denials, malformed restore errors, upload-size rejections and restore-related cookie-secret rotation warnings appeared during negative/restore tests.
- Disposable Tor-key backup fixtures use a one-second bootstrap timeout and synthetic key material. Their bootstrap timeout/keystore permission errors do not establish live Tor functionality.

123 runtime logs were captured while fixtures ran, in addition to the manual/network/TLS logs and failure-capable test attachments. The collector started after the first few browser cases, so it is not a complete trace of every successful case. No unexplained panic, route/extractor failure, template failure, migration error, database lock error, or background-task failure was found in the captured logs. Handler/admin/board/worker logging appeared under the expected categories.

## Coverage limits

- Live Tor/onion access and ChanNet federation were not exercised; the existing suite keeps ChanNet disabled. No ACME issuance or publicly trusted certificate deployment was attempted. Native TLS used test-local self-signed certificate acceptance, without changing system trust.
- WebKit was Playwright's macOS engine with an iPhone profile, not a physical iPhone/iPad or the installed Safari app. Mobile Firefox was viewport/touch emulation. The real-media pass ran on Chromium, while other engines received the existing critical smoke/fallback coverage.
- Browser history restoration was exercised, but real BFCache admission/eviction under every browser condition is not guaranteed by these tests.
- Optional missing peer metadata cannot be created through a normal socket connection. The existing immediately-ready extractor test covered both present and absent ConnectInfo and passed in the final Rust run.
- Expired post/session states were fixture-controlled where waiting would be impractical. Active board-password lockout enforcement and successful counter reset were tested, but a full 15-minute lockout expiration was not waited out.
- The pre-repair backup/hard-repair fault-injection hook is cfg(test)-only, so its browser case remains skipped. Disk exhaustion, power failure, genuinely multi-GiB split archives, long-duration soak and production-scale concurrency were not simulated. The existing suite did exercise concurrent submissions and bounded high-volume navigation/posting.
- This branch does not expose a poster-selected per-board flag field. Its available board feature toggles were tested.
- External video-provider availability and streaming were not tested; local embedded media playback and the application's media/embedding controls were exercised.

## Final validation

All requested commands ran after the browser matrix and passed:

| Command | Result |
|---|---|
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | PASS |
| `cargo test --workspace --all-features` | PASS — 2,288 test executions: 1,141 library + 1,141 binary + 5 CLI integration + 1 doctest; 0 failures/ignored |
| `cargo build --workspace --all-features` | PASS |
| `git diff --check` | PASS |

The library and binary run the same crate-local unit suite, so the 2,288 total is executions, not 2,288 distinct test cases. Full command exit codes/timings are in `validation-results.json`; logs are `fmt-final.log`, `clippy-final.log`, `test-final.log`, `build-final.log` and `diff-check-final.log`.

## Files and final state

- Tracked files changed: **none**.
- Fixes/regression tests added to the repository: **none needed**.
- Generated audit artifacts: this `output/playwright/branch-audit-20260903/` directory, including the report, summaries, logs, screenshots, disposable fixtures, supplemental probe scripts and browser report.
- Temporary runtimes were isolated from developer data. Browser fixtures performed their normal teardown. The main manual instance and all supplemental servers were stopped cleanly; the main disposable data location is in `instance.json`.
- **No commits were made.**
