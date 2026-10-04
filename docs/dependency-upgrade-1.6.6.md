# Dependency and upgrade review for 1.6.6

Reviewed against stable upstream releases on 2026-10-03. Rust 1.99 remains the
supported compiler. No OxideAV or OxiMedia dependencies were introduced.

## Dependency changes

All 67 direct root-manifest dependencies were checked against the crates.io
registry, including optional and development dependencies. The vendored AAC
manifest and build/audit tools were checked separately. Seventeen root dependency
requirements changed:

| Dependency | Previous requirement | New requirement |
| --- | --- | --- |
| tokio | 1.53.1 | 1.53.2 |
| rusqlite | 0.39.0 | 0.40.2 |
| r2d2_sqlite | 0.34.0 | 0.35.0 |
| toml | 1.1.5 | 1.1.6 |
| argon2 | 0.5.3 | 0.6.0 |
| flate2 | 1.1.9 | 1.1.10 |
| reqwest | 0.13.4 | 0.13.5 |
| clap | 4.6.6 | 4.6.7 |
| uuid | 1.26.0 | 1.27.0 |
| thiserror | 2.0.20 | 2.0.21 |
| arti-client, tor-hsservice, tor-cell | 0.46.0 | 0.47.0 |
| tokio-rustls | 0.26.5 | 0.26.6 |
| rustls | 0.23.44 | 0.23.45 |
| hyper-util | 0.1.20 | 0.1.21 |
| rustix | 1.1.4 | 1.1.5 |

The lockfile selects TOML 1.1.6+spec-1.1.0; its metadata suffix does not indicate a
prerelease. The vendored AAC implementation remains Symphonia 0.6.1 with its
existing source patch; only its lazy_static lower bound changed to 1.5.1.

Arti 0.47 permits rusqlite >=0.36,<0.41, allowing the newest application SQLite
pair without duplicate bundled SQLite versions. Argon2 0.6 required the new PHC
parser/error names and byte-salt API. Salt generation still uses OS entropy;
Argon2id v19, memory/time/parallelism settings and verification cost limits remain
unchanged. A PHC fixture generated independently with Argon2 0.5.3 verifies with
the new implementation, including rejection of a wrong password.

`cargo update --dry-run --verbose` reports no remaining compatible updates. Four
transitive packages remain behind a newer stable release because their latest
upstream parents restrict them:

| Package | Selected | Newer stable | Upstream restriction |
| --- | --- | --- | --- |
| derive-deftly / derive-deftly-macros | 1.12.1 | 1.13.0 | Arti uses ~1.12.0 |
| generic-array | 0.14.7 | 0.14.9 | crypto-common 0.1.7 pins =0.14.7 |
| matchit | 0.8.4 | 0.8.6 | Axum 0.8.9 pins =0.8.4 |

These constraints were retained rather than forking upstream dependencies.
The resolved graph has no dependency requiring a compiler newer than Rust 1.99,
and no openssl, native-tls or aws-lc-rs package with all features enabled.

## Build and distribution tooling

GitHub Actions were updated to current stable releases and remain pinned to full
commit IDs: checkout 7.0.1, upload-artifact 7.0.1, download-artifact 8.0.1,
rust-cache 2.9.2, install-action 2.87.23, action-gh-release 3.0.3,
setup-buildx-action 4.4.1, login-action 4.6.0, metadata-action 6.2.0 and
build-push-action 7.4.0. The toolchain action uses its reviewed stable commit with
explicit Rust 1.99.0. Hosted runner support and major-release behavior were
reviewed against upstream release notes.

cargo-deny is 0.20.2 and cargo-audit is 0.22.2; cross remains at its latest stable
0.2.5. Container builder/runtime bases use Debian 13 Trixie. Runtime FFmpeg comes
from that stable distribution, retaining distribution security maintenance;
upstream FFmpeg is not compiled into the application image. Optional system
media tools remain operator-managed outside containers.

Locked license/source/advisory policy passes with the existing narrowly
documented RSA timing and unmaintained paste exceptions. The Arti RSA exposure
was re-reviewed; this change does not add RSA private-key decryption. No advisory
exception or license allowance was broadened.

## Upgrade compatibility corrections

An isolated Linux fixture using the actual published 1.6.0 and 1.6.5 executables
reproduced startup failure on a canonical 1.6.0 database: the two newer post-query
indexes were missing, and the repair allowlist did not recognize the 1.6.0 stamp.
Supported stable prior package stamps now reach the existing narrowly checked
repairs. Complete structural/domain/integrity checks still reject unknown drift;
regressions verify post preservation, repeated migration and rejection without
stamping success.

The unchanged published 1.6.5 artifact works after the documented offline
two-index preparation. The fixture preserved administrator authentication, board,
thread/post rows, settings, media and a private runtime file's contents/mode, then
restored the matching full stopped data and 1.6.0 executable and verified readiness.
The 1.6.5 manifest signature verified against the 1.6.0 public key; archive and
executable sizes/hashes matched the signed manifest. No published artifact or tag
was changed. See [the operator procedure](software-updates.md#manual-upgrade-of-a-source-install).

A separate static audit found that the published 1.6.5 manifest advertises
minimum updater 1.6.0 even though managed application startup now requires the
closed `Started` request introduced after 1.6.0. The old controller has no such
request and cannot acknowledge it. The manual/source fixtures above do not use
that native IPC path. The accepted replacement must correct signed compatibility
bounds and test old-controller adoption; migration repair alone does not establish
every managed upgrade path.

The independent intermittent CLI startup failure used a one-second request
checkout deadline while waiting for the first initialized connection. First
initialization now has a bounded 30-second deadline; normal request checkout
remains one second. Slow-first-connection, unusable-database, blocked-spare,
capacity, WAL/FULL durability and overload regressions pass.

Native Linux source builds now include the fixed official Ed25519 public
verification key. A fixture from the published 1.6.5 release verifies with that
embedded identity; modified manifest bytes and a modified key fail verification.
Only the public key is included. Trust is not established by downloading a new
key alongside a release. Container/native installation boundaries remain enforced.
The install confirmation explicitly states that the official prebuilt executable
replaces local source modifications while retaining application data/configuration.

The single-binary recovery policy is accepted: normal operations restart only
RustChan; exceptional loss of descendant-exit proof can require a host reboot.
The implementation retains a durable launch record before spawning writers and
blocks same-boot recovery while writer lifetime remains uncertain. A free lock
or missing PID cannot clear that uncertainty. Restarting only RustChan cannot
substitute for a changed kernel boot or proven complete descendant cleanup.

The same-executable full controller and process monitor are integrated. The
separate updater binary target has been removed; package manifests require
updater 1.6.6 and `--update-info` advertises protocol 2. The user authorized
protected main integration and the 1.6.6 release, with native reboot/power-loss
testing handled separately. Actual kernel boot recovery remains unvalidated;
the accepted exceptional reboot fallback does not establish that proof.

## Validation

The ordinary-UID Linux Rust foundation suite passed 47 checks. It includes
descriptor closure on malformed/truncated/extra-rights/EOF frames, CLOEXEC and
close-only lease handoff, real competing-process admission during lock
conversion, and injected ENOSPC failures at the real lifetime write/rename/fsync
boundaries. A double-forked process that starts a new session and closes its
descriptors is reaped before lifetime completion. That test first caught an
incorrect process-group wait and passed after the wait was corrected to cover
every child. The original failing run remains preserved.

SQLite/configuration transaction regressions prove that a failed replacement
controller initialization rolls back before commit, while a lost acknowledgment
after commit preserves accepted writes and the healthy replacement. These are
actual Rust/filesystem/SQLite tests, not full native update or power-loss proof.
The actual Linux executable also passes a disposable foreground fixture on
Linux-local storage: independent admin CLI admission while serving, a competing
server rejected before database/configuration changes, protected root-owned
executable serving in manual mode, preserved stdin, parent-death cleanup, and
surviving-monitor descendant cleanup after guardian SIGKILL. Combined root and
guardian loss retains an uncertain lifetime and refuses a same-boot cold start
without changing database/configuration bytes. Exact local source bytes are
retained and hash-verified. An interrupted initial retention can be replayed
before any application writer starts. The earlier macOS bind-mount fixture failed
the strict private-directory check and its red evidence remains retained; those
checks were not relaxed.

The end-to-end offline fixture now passes actual executable installation,
newest full-controller handoff, durable canonical commit/public admission and
complete post-migration rollback. A compiled 1.6.6 source installs a compiled
1.6.7 candidate and atomically publishes it at the original source path. The
new controller's exact kernel identity replaces the old one while the root
remains active. Candidate health-check requests receive HTTP 503; public
admission opens only after Succeeded commits and the newest controller activates.
Admin IPC and matching independent CLI work after installation.

A compiled 1.6.8 fixture then changes the database/schema, settings, media and
private runtime file before deliberately failing. Rollback restores 1.6.7,
validates database integrity/foreign keys, removes failed migration state and
preserves file hashes/modes and an acknowledged write made after the successful
installation. A subsequent stopped/offline CLI and cold launch select the newest
committed controller and preserve the rollback result. Evidence is retained in
`output/dependency-upgrade-1.6.6/native-update-run-3/`.

The final protocol-2 runtime repeat also passed in
`output/dependency-upgrade-1.6.6/native-update-run-6-complete-inputs/`.
Both signed synthetic packages were complete and hash-checked before startup;
all three compiled fixture binaries contained the offline fetch substitution.
The earlier shared-cache mix-up and incomplete-input runs remain available and
are not counted as successful rollback evidence. Production trust stayed unchanged.

The fixture replaces network fetch and the embedded public key only in a copied,
ignored build context, using a synthetic Ed25519 identity. Actual signature,
archive, ownership, transaction and readiness checks run. Its deliberate failed
candidate changes only disposable fixture data. No production trust, official
release key, published asset or tag was changed. Production updater regressions
pass 47 tests with two ignored process helpers. This does not establish native
kernel reboot/power-loss behavior or final release approval.

Final macOS workspace regressions passed 1,349 tests in each library/application
target, with four existing ignored tests in each, all five CLI process tests,
the policy and rendered-fixture regressions, and the documentation test. Strict
all-target/all-feature Clippy, warning-free documentation, and both no-default
and ACME-only feature checks passed on macOS. Linux strict all-target/all-feature
Clippy and warning-free documentation passed;
the complete Linux unit suite passed as ordinary UID 501: 1,363 passed and six
existing helpers ignored. Packaging/signing regressions passed all seven tests.
Evidence is retained in the `final-workspace-tests-3.log`,
`final-linux-unit-tests-3.log`, `final-macos-clippy-2.log`,
`final-macos-docs-2.log`, `final-no-default-features-2.log`, and
`final-packaging-tests-2.log` files under `output/dependency-upgrade-1.6.6/`.
Earlier failed checks remain available. The Linux test runner was corrected to
use its own writable executable directory, the external `/bin/kill` needed by
existing liveness assertions, and an init process to reap orphaned children;
application assertions and timeouts were not relaxed.

The final complete Chromium gate passed all 325 tests: 287 expected results,
38 skips, zero unexpected failures and zero flaky results, in 17 minutes
44 seconds. Both the dashboard-theme and long-content cases that timed out in
earlier complete runs passed. The suite used its original two workers, zero
retries, assertions and timeouts. Reports, browser lifecycle logs and host
resource samples are retained in `browser-chromium-final-clean/`.

The preceding complete run's 90-second dashboard timeout remains recorded as a
failure in `browser-chromium-final-complete/`. Its trace already contained the
expected open attribute, a 75-millisecond successful dashboard response and
prompt assets. Chromium's `Runtime.callFunctionOn` did not return until page
teardown; the installed driver's first assertion evaluation was unbounded by
the assertion timeout. Three unchanged isolated cases and three unchanged
desktop-clock-then-contrast sequences passed before the clean full run. The
precise renderer/OS cause remains unestablished; no speculative application or
harness patch was made. See `browser-dashboard-stall-diagnosis.json`. All failed
evidence remains preserved.

Actual native Linux reboot/power-loss testing remains unperformed and is being
handled separately under the user's release decision. No host installation or
reboot was performed. This is an explicit validation limitation.

End-of-work cleanup removed the idle Cargo target after a preservation assertion
failed to stop the sequence. The exact macOS executable from the final passing
browser run (SHA-256 `a7dbdfc00fa37ea05780dd27c0f8bd7407d971136568ef556fbd0a9926e4f34f`)
was not retained; 21 preserved runtime copies had no matching build. Its recorded
test results, source and build metadata remain available, but exact-byte replay
is limited. The preserved earlier all-feature macOS executable is a distinct
build. Linux fixture executables and all recovery/browser evidence remain intact.

Before the updater redesign, strict all-target/all-feature compilation, Clippy
and documentation passed. The dependency/migration baseline full workspace suite
passed: 1,346 unit tests in each library/application target,
five CLI process tests, the deny-policy regression and the documentation test;
four existing unit tests remain explicitly ignored in each target. Packaging
and signing regressions passed. The full Chromium run had 286 passes, 38 skips
and one long-content timeout; that unchanged case passed alone and in three
repeats. The full run is still recorded as failed. Focused WebKit/no-JavaScript
update and backup/restore coverage had 33 passes and one skip. Native kernel
reboot/power-loss validation remains separate follow-up work; those container
fixtures do not establish real boot or power-loss behavior.

Local instructions and the complete browser harness remain ignored and
untracked. Their preserved copies and regression evidence are local artifacts;
they are not added to release commits or container contexts.
