# Conservative compatibility audit

Audited RustChan 1.4.1 at `3af3f2a` and the accompanying uncommitted changes.
No dependencies, persisted formats, public APIs, routes, configuration keys,
CLI options, cookies, or migration eligibility rules are removed by this audit.

## Method and limits

Searched Rust sources, static assets, tests, tracked documentation, configuration,
Cargo metadata, and hidden CI files for compatibility terminology, aliases,
re-exports, Serde defaults, feature/platform gates, version handling, and
fallbacks. Followed references through readers, writers, startup, restore,
fixtures, and Git history. Repository-wide searches preceded each removal and
rename, including the lint-exception inventory. No repository AGENTS.md or
standalone migration/script directory was present; the supplied project
instructions were applied.

Existing installations, offline backups, old browser tabs, gateways, and users
of the public `chan` library cannot be inventoried from this checkout. A current
writer no longer emitting a format is not proof that its reader is dead. There
is no repository evidence of a fleet-wide migration or compatibility sunset.
The tables group related paths with the same evidence and disposition; ordinary
variables named `old`, SQL `OLD` rows, temporary files, rollback paths, and
current failure recovery are not evidence of obsolete behavior.

## Archive and database compatibility boundary

| Input | Expected behavior in this checkout | Evidence and limits |
| --- | --- | --- |
| Full ZIP with `backup.json` and root `chan.db`, manifest versions 1, 2, 3 | Retain acceptance of valid archives, subject to manifest counts, path/size checks and a valid or recognized repairable database. Missing optional banner, Tor-key and board-index metadata receives existing defaults. | `backup/safety.rs` verifies a v1 fixture, tests v2 without Tor metadata, and verifies v3 metadata. `restore_full.rs` covers v2 restore and current v3 restore. These are synthetic fixtures, not an exhaustive corpus of historical release databases. |
| Full ZIP without `backup.json` | Already rejected. | `verify_full_backup_zip_rejects_missing_manifest`; this audit does not broaden or narrow that boundary. |
| Board ZIP with `board.json`, manifest versions 1 and 2 | Retain valid imports and missing-field defaults, including archived state, media/access/banner settings and optional post metadata. | `board_manifest.rs`, `parse_board_backup_manifest_from_zip`, v1 restore fixtures, current v2 writer in `create.rs`, the older-PDF-setting test and explicit archived-row restoration. Protected-board validation stays fail closed. |
| Saved `rustchan-backup-v4` directory and split-ZIP roots | Retain both supported modes, metadata, checksum/part verification, board extraction, and conversion into the shared restore archive. | `storage::verify_saved_backup`, `archive.rs`, full/directory and split-ZIP verification tests. The format string is an external identifier, not obsolete module naming. |
| v4 transfer ZIP containing `db/rustchan.sqlite3` | Retain conversion to the v3 full-restore archive layout. | `archive::convert_transfer_zip_to_full_restore_archive` and uploaded full-restore handler; introduced in `fc60d79`, described in CHANGELOG. This is an active input path, not a dead decoder. Dedicated positive transfer-conversion coverage is limited in the current Rust suite. |
| `single_zip` or `legacy_zip` in a saved-v4 root | Already rejected for listing verification and saved restore; enum decoding and diagnostics remain. Standalone ZIPs in `full/` and `boards/` remain separate supported inputs. | Initial mode gates in `listing.rs` and `storage.rs`, standalone listing/download/restore branches, README backup layout. Invalid saved roots can still appear as unverified entries. |
| Other full/board numeric manifest versions | No new promise made. | Their parsers deserialize a `u32` without an explicit supported-version range. Acceptance is currently structural, not a version allowlist. Retain behavior; defining a stricter version policy is separate compatibility work. |
| Fresh or structurally current database | Create or adopt the current package baseline. | `migrations.rs` uses `CARGO_PKG_VERSION`; `schema.rs` verifies structure, metadata and persisted invariants before successful normalization. |
| Recognized historical database drift | Retain exact-shape repairs for eligible metadata: absent version, integer 1–41, 1.3.0, 1.4.0, and current baseline. | `is_known_legacy_schema_version`, board/theme reconstruction, additive indexes/domain triggers, historical-v41, rollback and domain-migration tests. Version eligibility alone does not make an arbitrary old schema upgradeable. |
| Partial, corrupt or unknown structural database layout | Already rejected without manufacturing a successful upgrade. | Schema rejection tests, backup snapshot verification and full-restore normalization. SETUP describes the baseline reset; history `df09271`, `c42ff7e`, `a3b1376` explains squashing and subsequent recognized drift repair. The original pre-baseline migration chain is not present. |

Archive version and database schema version are independent. Consequently,
“v1/v2/v3 full archives remain supported” does **not** mean that every database
ever emitted by those releases is restorable. No backup decoder or database
repair was deleted on that assumption. Older full archives without a board
index still support manual board extraction, as explicitly tested and documented
in CHANGELOG. Older archives without favicon/Tor assets must not erase current
identity/assets; restore opt-in, defaults and related tests remain unchanged.

## Findings

| Area | Compatibility behavior | Status | Evidence | Action |
| --- | --- | --- | --- | --- |
| Backup listing | Second rejection of standalone ZIP modes after the same modes have already returned an error | Remove now | `validate_saved_backup_metadata` rejects these immutable enum values before its final branch; `037d80a` introduced both checks together. No input can reach the second rejection. | Removed the duplicate branch; preserved the initial error, validation order and directory/split checks. Added mode-rejection regression coverage. |
| Backup test helpers | Two forwarding layers left by conversion from panic-based fixtures to `Result` | Remove now | `1a4e0e1` introduced forwarding functions; `96a12e0` renamed them. Each private implementation had one caller, its identically typed wrapper, under `cfg(test)`. No serialization, configuration or external API depends on those private names. | Folded bodies into the existing fixture helper entry points; removed both private `_result` functions. Fixture data and call sites remain. |
| Database test names | Four names claimed a 1.4.0 output baseline while asserting the compiled release | Not compatibility code | Assertions use `baseline_schema_version()`/`CARGO_PKG_VERSION`; historical input 41 remains meaningful. | Renamed only the output-baseline wording to `release_baseline`; updated matching lint inventory entries. |
| Full/board backups | Older manifests, optional fields, board-index fallback, archive conversion and saved ZIP locations | Keep intentionally | Archive matrix above; README explicitly retains legacy ZIP directories; CHANGELOG promises older board settings and index-free extraction. Offline backups can outlive all running installations. | Kept all decoders, defaults, extraction paths, filename validation and compatibility fixtures. |
| v4 backup metadata | Serialized storage/scope/file-kind variants, optional runtime paths, ZIP entry paths, inclusion bits and manifest fields | Keep intentionally | Serde input types and strict verification in `storage.rs`; absent entry-path metadata falls back to logical paths. Some variants are rejected in saved-restore contexts, but are still decoded for diagnostics or used by current exports. | Preserved serialized vocabulary, path fallback and rejection rules. |
| Database repair | Historical board defaults/checks, theme index, redundant indexes, domain-trigger installation and counter repair | Keep intentionally | `repair_known_legacy_baseline_drift`; exact-shape checks and historical-v41/data-preservation/rollback/domain tests. Neither a clean fresh DB nor version stamping proves old backups are gone. | Retained all repairs, guards, version reading/stamping and old-shape test fixtures. |
| Runtime layout/Tor | Move `full-backups`, `board-backups`, `tmp-board-downloads`, `arti_state`, `arti_cache`, `tls`, `favicon`, `banner` into grouped paths | Keep intentionally | `RUNTIME_LAYOUT_MIGRATIONS`, startup call, `edd64c7`, CHANGELOG upgrade instructions. `1670652` explicitly keeps migration destinations independent of custom backup storage. | Retained migration and conflict handling; no Tor identity or existing backup moved by the audit. |
| Durable filesystem journal | Missing optional/digest fields, thumbnails formerly in required paths, restore payloads without additional swaps/private-permission flags | Keep intentionally | Serde defaults and replay in `pending_fs.rs`; legacy missing-thumbnail repair and legacy full-restore JSON tests. Interrupted installations may replay old persisted operations. | Kept decoders, file validation and recovery. Restored journals being scrubbed does not make startup journals dead. |
| Background jobs/media | Historical thread-prune payloads, duplicates, malformed jobs and pending-media inconsistencies | Keep intentionally | Startup/periodic recovery in `server.rs` and `workers`; duplicate-coalescing and media fixed-point tests. Normalization is bounded and retryable, not proof every job has migrated. | Retained recovery, persisted payload handling and tests. |
| Activity settings | Old `new_activity_notifications_enabled` TOML/DB key and `CHAN_NEW_ACTIVITY_NOTIFICATIONS` seed three newer badge settings | Keep intentionally | `Config::from_env`, startup seeding and DB getters. Settings files, environments and restored databases can retain the original key; seeding does not rewrite all external sources. | Preserved precedence, defaults and old inputs. No dedicated combined legacy-key/env precedence test was found. |
| TLS/config defaults | Configurations lacking newer TLS fields, including `require_https` | Keep intentionally | Serde defaults; `tls_config_without_require_https_defaults_to_optional_https`; `4fc899c`. Old operator settings remain realistic. | Kept defaults; no plaintext/HTTPS behavior change. |
| CLI | `create-board --no-audio` after audio became opt-in | Keep intentionally | Clap flag and conflict rule, default/explicit-disable tests, `01045ea`. Shell scripts and operators can still pass this option. | Kept CLI contract. |
| Board settings | Historical `edit_window_secs`, audio companion fields, media classifications and optional metadata | Keep intentionally | DB/model/backup fields and restore writers; fixed self-action grace window intentionally coexists with serialized edit-window data. Media/rendering/prune paths still read companion attachments. | No schema/manifest field or media fallback removed. |
| Passwords and sessions | Supported Argon2 PHC algorithms/versions/parameters; session, deletion, board-unlock and CSRF token formats | Keep intentionally | `utils/crypto.rs`, middleware and auth handlers; capped PHC verification, scoped/unscoped CSRF tests, full-restore session purge/fresh-session tests. Stored hashes and live cookies are external state. | Retained hashes, domain separators, verifier limits, token names and security checks. No auth behavior affected. |
| ChanNet | Optional gateway message IDs and deterministic replay tokens for older clients | Keep intentionally | `command.rs` documents older gateways; `db/chan_net.rs` hashes historical requests and persists `channet-reply-v1:`/`channet-reply-id-v1:` tokens. Replay and stable-ID tests exist; `fb9276a` introduced transition support. | Kept fallback and exact cryptographic/domain identifiers. No gateway migration guarantee exists. |
| Themes | Raw-CSS editor, old built-in default set, saved builder metadata/native-control fixes | Keep intentionally | Admin UI explicitly promises older CSS themes remain editable without migration; `db/themes.rs` and theme-editor tests protect saved settings. | Preserved editor mode, stored CSS, theme slugs and normalization. |
| Persisted post HTML | Upgrade spoiler markup lacking `data-action`, removing old inline handlers blocked by CSP | Keep intentionally | `static/main.js` initial/dynamic hooks; `body_html` is persisted and used in render paths. Restore recomputation alone does not rewrite every live installation's stored HTML. | Kept upgrade hook and current CSP-safe interaction. |
| Public Rust aliases | `BoardAccessMode::requires_post_password`, reusable module re-exports and `Arc<Vec<…>>` live-state APIs | Keep intentionally | Exported through `chan`; two production handler callers and helper tests; explicit API-compatibility comments for live-state types. No public deprecation/sunset evidence. | Kept APIs and tests; did not equate absence of in-repository users with absence of external users. |
| Schema-tolerant statistics | Column discovery and fallback queries for older `posts`/`threads` schemas | Candidate for future removal | Normal startup enforces the baseline, but `get_site_stats` is a public library function accepting arbitrary connections and has an explicit missing-column test. | Kept until its external/pre-upgrade use and supported input contract can be retired explicitly. |
| Theme browser storage | Write-only localStorage theme mirror alongside authoritative cookie state | Candidate for future removal | `static/main.js` writes the mirror; current initialization/tests use cookies. No current reader found, but old tabs/scripts and the explicit compatibility intent have no sunset evidence. | Kept pending an explicit browser/client compatibility policy. |
| URLs and form inputs | Trailing-slash normalization, post redirects, upload `file` slot, XHR/HTML responses and no-JS forms | Keep intentionally | Route tables, multipart consumers, templates and route tests still expose these behaviors. Bookmarks, existing forms and integrations can depend on them. | Preserved paths, form fields, hooks, redirects and fallbacks. No retired route alias was established. |
| Optional tools/platforms | ffmpeg/ffprobe/renderer fallbacks, GIF/SVG previews, process cleanup, disk-space checks, TLS features | Not compatibility code | Current optional-tool deployments and Linux/macOS/Windows support use these branches. Cargo features are `tls-self-signed` and `tls-acme`, with active implementations. | Retained; not old-RustChan behavior. |
| Deprecated upstream API | ACME low-level acceptor and Tokio/futures I/O adapter | Not compatibility code | `tls/acme.rs` documents why the manual server loop needs the acceptor; `server.rs` consumes it and the adapter. The deprecation is upstream, not evidence of an unreachable RustChan path. | Kept existing narrow lint expectation and TLS architecture. |
| Other terminology | Temporary files, rollback `restore-old` names, cache defaults, HTTP framing fallback, config error/logging wrapper | Not compatibility code | Current recovery, output and error-handling paths; the config wrapper logs failures rather than merely forwarding an identical result. No outstanding `TODO`/`FIXME`/`remove after` compatibility deadline was found. | Left current behavior intact. |

## Changes and verification

Changed files: `src/handlers/admin/backup/listing.rs`,
`src/handlers/admin/backup/storage.rs`, `src/db/schema.rs`,
`docs/strict-lint-exceptions.tsv`, and this report.

Removed only the unreachable duplicate rejection and two test-only forwarding
layers. No tests or fixtures were removed. Four tests were renamed without
changing their assertions. The new listing regression accepts valid directory
metadata and confirms both standalone ZIP modes still produce `BadRequest`.
No new dependencies or Git commit were created.

| Validation | Result |
| --- | --- |
| `cargo test --workspace --all-features saved_backup_listing_rejects_standalone_zip_modes` | Passed in both unit-test targets. |
| `cargo fmt --all --check` | Passed after formatting only the touched listing file with `rustfmt --edition 2021 src/handlers/admin/backup/listing.rs`. |
| `cargo fmt --all -- --check` | Passed. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed. |
| `cargo test --workspace --all-features` | Passed: 1,182 unit tests in each of the library and CLI targets, five process integration tests, one doctest. No failures or harness-ignored tests. |
| `cargo build --workspace --all-features` | Passed on the local macOS host. |
| `git diff --check` | Passed. |
| Removed/renamed identifier searches | No stale source, active test, documentation, fixture or CI references. A search including ignored files found only historical test logs under `test-results/` and `output/playwright/`; these audit records were preserved. |

The full test run includes 42 schema-test executions, six migration-test
executions, 264 backup-test executions, 58 filesystem-journal-test executions,
52 config-test executions and 20 CSRF-test executions (these totals include
the shared tests compiled into both unit-test targets), plus authentication,
session restore and live HTTPS-cookie coverage. Optional-tool tests can return
early when their external tool is unavailable; the harness result does not
establish every optional tool or foreign-platform branch was exercised.
No Linux/Windows cross-build was performed in this local audit.
