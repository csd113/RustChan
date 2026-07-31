# Strict Rust 1.91 and Clippy policy audit

Audit date: 2026-07-31

This report records the final uncommitted workspace state. The Rust 1.91 and
stable host validation is clean, the two intentionally visible dependency
audits are recorded in TSV ledgers, and the cross-target limitations are
identified below. No commit was created.

## 1. Exact manifest lint policy

The root package opts into the workspace policy with:

~~~toml
[lints]
workspace = true
~~~

The following is the exact persistent policy in Cargo.toml:

~~~toml
[workspace.lints.rust]
warnings = { level = "deny", priority = -3 }
future_incompatible = { level = "deny", priority = -2 }
rust_2018_idioms = { level = "deny", priority = -2 }
rust_2024_compatibility = { level = "deny", priority = -2 }
unused = { level = "deny", priority = -2 }

unsafe_code = "forbid"
missing_docs = "deny"
missing_debug_implementations = "deny"
non_ascii_idents = "deny"
unreachable_pub = "deny"
unused_lifetimes = "deny"
unused_qualifications = "deny"
let_underscore_drop = "deny"

# This rustc lint can report false positives for dependencies shared across
# library, binary, example, test, benchmark, and feature-specific targets.
# Keep it visible, but do not force fake imports or dependency distortions.
unused_crate_dependencies = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -3 }
pedantic = { level = "deny", priority = -2 }
cargo = { level = "deny", priority = -2 }

# This can represent unavoidable transitive version splits, especially in
# a dependency graph as large as Arti's. Audit every report without making
# unavoidable upstream duplication a hard failure.
multiple_crate_versions = "warn"

# Panic-driven control flow.
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
panic_in_result_fn = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
exit = "deny"

# Debugging output must not enter production code accidentally.
dbg_macro = "deny"
print_stdout = "deny"
print_stderr = "deny"

# Unsafe-code safeguards remain documented even though unsafe Rust is
# forbidden globally.
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
missing_safety_doc = "deny"

# Suppressions must be narrow, intentional, and documented.
allow_attributes = "deny"
allow_attributes_without_reason = "deny"

# Numeric conversions must be explicit and reviewed.
as_conversions = "deny"
cast_lossless = "deny"
cast_possible_truncation = "deny"
cast_possible_wrap = "deny"
cast_precision_loss = "deny"
cast_ptr_alignment = "deny"
cast_sign_loss = "deny"
char_lit_as_u8 = "deny"
fn_to_numeric_cast_any = "deny"
lossy_float_literal = "deny"

# Avoid unchecked indexing and UTF-8 string slicing.
indexing_slicing = "deny"
string_slice = "deny"

# Ownership and resource-management restrictions.
clone_on_ref_ptr = "deny"
mem_forget = "deny"
rc_buffer = "deny"
rc_mutex = "deny"

# Documentation, testing, and maintainability.
missing_docs_in_private_items = "deny"
missing_assert_message = "deny"
large_include_file = "deny"
verbose_file_reads = "deny"
wildcard_dependencies = "deny"
~~~

Rust 1.91 recognizes every persisted lint name. The contradictory
clippy::restriction group, the conflicting get_unwrap lint, and the unstable
clippy::nursery group are not persisted wholesale. Nursery is audited
separately.

There is an important rustc 1.91 interaction: a lint at ordinary warn level is
still part of the warnings group, so warnings = deny promotes it to an error.
The requested combination of “deny every warning” and “leave these two lints
non-gating at warn” therefore cannot be represented by manifest levels alone.
The exact requested manifest is retained; narrow target-level expectations
handle false-positive unused-dependency reports during normal builds, and the
forced audit commands in section 8 override those levels to produce visible,
non-gating warnings for both dependency categories.

## 2. Rust 1.91 MSRV inheritance and metadata

Cargo sees the MSRV through workspace package inheritance:

~~~toml
[package]
rust-version.workspace = true

[package.metadata]
msrv = "1.91"

[workspace]
resolver = "2"

[workspace.package]
rust-version = "1.91"
~~~

RustChan is currently the workspace's only package, so this covers the library,
the rustchan-cli binary, and the integration-test target. Cargo metadata reports
rust_version 1.91 for the package, while package.metadata.msrv keeps cargo-msrv
metadata aligned. The MSRV is enforced by Cargo rather than being merely
documented in a toolchain file.

## 3. Scoped expectations and remaining allows

The exhaustive line-by-line record is
[strict-lint-exceptions.tsv](./strict-lint-exceptions.tsv). Each row contains
the path, line, attribute form, lint or lints, concrete reason, and scope.

| Measure | Count |
| --- | ---: |
| Scoped attributes | 380 |
| Direct expect attributes | 379 |
| Conditional cfg_attr(expect) attributes | 1 |
| Remaining allow attributes | 0 |
| Files containing an exception | 84 |
| Function scopes | 350 |
| Struct scopes | 16 |
| Item scopes | 9 |
| Crate scopes | 3 |
| Module scopes | 1 |
| Let-binding scopes | 1 |

Some attributes name more than one lint, giving 390 lint references:

| Lint | References |
| --- | ---: |
| clippy::panic_in_result_fn | 187 |
| clippy::too_many_lines | 85 |
| clippy::too_many_arguments | 29 |
| clippy::cognitive_complexity | 25 |
| clippy::expect_used | 24 |
| clippy::struct_excessive_bools | 16 |
| clippy::fn_params_excessive_bools | 5 |
| unused_crate_dependencies | 3 |
| clippy::as_conversions | 3 |
| clippy::cast_precision_loss | 3 |
| clippy::exit | 2 |
| clippy::print_stderr | 2 |
| clippy::rc_buffer | 2 |
| clippy::module_inception | 1 |
| clippy::significant_drop_tightening | 1 |
| clippy::unnecessary_wraps | 1 |
| deprecated | 1 |

The only crate-level expectations are the three places where rustc emits
target-wide unused dependency diagnostics:

- src/lib.rs: Cargo exposes binary-only package dependencies to the library
  target, including the deliberate rustls-webpki security floor for
  RUSTSEC-2026-0049.
- src/main.rs: Cargo exposes the companion chan library and the direct
  rustls-webpki security floor to the standalone binary target.
- tests/cli_runtime.rs: the black-box integration test intentionally interacts
  only with the compiled CLI while Cargo exposes all package dependencies.

The single module exception preserves the established
crate::server::server path. The single conditional expectation documents that
terminal relaunch is fallible only on Linux and Windows. All other decisions,
including the two Arc<Vec<T>> public-API compatibility exceptions, are scoped
to the affected function, struct, item, or binding and carry a reason in the
ledger.

## 4. multiple_crate_versions audit

The exhaustive dependency/version/introducer/disposition record is
[strict-dependency-duplicates.tsv](./strict-dependency-duplicates.tsv).

| Feature configuration | Active duplicates | Metadata-only optional splits | Total rows |
| --- | ---: | ---: | ---: |
| No default features | 44 | 2 | 46 |
| Default / self-signed TLS | 47 | 2 | 49 |
| ACME only | 48 | 2 | 50 |
| All features | 53 | 4 | 57 |
| **Total** | **192** | **10** | **202** |

The ledger covers 57 unique duplicate families. Of the 202
configuration/family rows, 192 are active duplicate builds and 10 are
metadata-only optional splits that are not simultaneously built. Every row
records the resolved versions and principal introducers.

| Recorded constraint class | Rows | Disposition |
| --- | ---: | --- |
| Crypto/RNG and Arti | 56 | Unavoidable with Arti 0.44 and the current direct crypto stack |
| Target-specific | 40 | Unavoidable for the recorded platform consumers |
| Proc-macro/API | 28 | Incompatible derive/proc-macro generations |
| TLS/PKI and Arti | 20 | Requires upstream rustls/ACME/PKI/Arti convergence |
| Configuration/proc-macro | 20 | Incompatible upstream constraints |
| Media/CAPTCHA | 16 | Legacy CAPTCHA/media and current media stack generations |
| Collection ecosystem | 8 | Incompatible upstream generations |
| Reqwest/direct | 4 | Current reqwest/direct constraint split |
| Metadata-only optional split | 10 | No built duplicate in that configuration |

No active row was safely unifiable by changing only RustChan's direct
constraints. The retained splits are upstream semver, feature, target, or
links constraints; none justify downgrading Arti, changing SQLite's compatible
line, reintroducing native TLS, or distorting source imports.
The new direct rustix dependency resolves to the existing single 1.1.4 package
and therefore adds no duplicate family; the ledger refresh only updates the
four affected bitflags introducer descriptions.

## 5. unused_crate_dependencies audit

The exhaustive warning/disposition record is
[strict-unused-dependencies.tsv](./strict-unused-dependencies.tsv).

| Feature configuration | Warning rows |
| --- | ---: |
| No default features | 75 |
| Default / self-signed TLS | 78 |
| ACME only | 79 |
| All features | 82 |
| **Total** | **314** |

The rows cover 55 unique dependency names and split by Cargo target as follows:

| Target | Rows | Disposition |
| --- | ---: | --- |
| chan library | 102 | Dependencies are used by the standalone CLI/runtime graph |
| rustchan-cli binary | 8 | Retain chan plus the rustls-webpki security floor |
| cli_runtime integration test | 204 | Preserve the black-box target boundary |

The normalized dispositions are 98 library/runtime-boundary rows, 192
integration package/library exposure rows, 12 security-floor rows, 4 rustix
integration-exposure rows, 4 black-box chan-boundary rows, and 4 companion-chan
binary rows. All 314 are retained. Fake imports would conceal Cargo's
target-granularity behavior without improving the graph, so the three crate
expectations described in section 3 are used instead.

Genuinely removable direct dependencies were removed: once_cell was replaced
by std::sync::LazyLock, libc was removed with the unsafe FFI statvfs path, and
windows-sys was removed with the unsafe AllocConsole path. The safe in-process
filesystem query now directly uses the already-resolved rustix 1.1.4 package on
Unix. Hyper is optional and activated only by tls-acme. No dependency is
removed merely because it is absent from one target.

## 6. Important defects and risk paths fixed

The remediation is predominantly behavior-preserving lint work: explicit
imports, complete documentation, contextual Result propagation, checked
collection access, reviewed numeric conversions, explicit resource cleanup,
and test assertion messages. Important substantive corrections include:

- Panic and unsafe removal: there are no .unwrap( calls, panic! invocations, or
  unsafe blocks/functions in src or tests. The remaining invariant-driven
  .expect calls are covered by the scoped expectations and reasons in the
  exception ledger.
- TLS input handling: src/tls/self_signed.rs now propagates invalid
  subject-alternative-name construction as an application error instead of
  panicking.
- Unix capacity detection: src/utils/files/disk_space.rs replaces unsafe libc
  FFI with rustix 1.1.4's safe in-process statvfs API. Block-to-byte conversion
  saturates rather than wrapping, and metadata-query failure preserves the
  established fail-open preflight behavior.
- Windows startup: src/main.rs removes the unsafe AllocConsole call and
  windows-sys dependency in favor of a safe terminal relaunch path.
- Binary/library boundary preservation: the production chan library retains
  its original public module set, while src/main.rs continues to own the
  standalone runtime module graph. Runtime modules are included in chan only
  under cfg(test) so existing unit-test support does not expand the production
  library API.
- File and media bounds: src/banner.rs and
  src/utils/files/storage.rs validate RIFF/PNG chunk sizes and conversions;
  src/media/thumbnail.rs uses widened integer scaling instead of lossy float
  casts; backup byte/count arithmetic is checked.
- Numeric rendering: poll, vacuum, configuration-size, archive-count, and
  snapshot calculations use checked, saturating, or exact integer conversions
  where truncation, pointer width, or overflow mattered.
- Indexing and UTF-8: media stems, headers, hashes, onion-address assembly,
  sanitization, formatting, and archive/path parsing now use checked access,
  iterators, strip/split operations, or validated bounds instead of unchecked
  indexing and string slicing.
- Secret-safe Debug: Config has a complete custom Debug implementation that
  redacts cookie_secret, chan_net_api_key, and rustwave_url because URL userinfo
  can contain credentials. AdminAction has a custom Debug implementation that
  redacts create/reset passwords. Both have regression tests.
- Public API preservation: live_boards, live_boards_snapshot, and live_themes
  retain their original Arc<Vec<T>> return types. The backing statics carry the
  two narrow clippy::rc_buffer expectations rather than changing the public API
  to Arc<[T]>.

Security-sensitive backup, restore, path, symlink, authentication, moderation,
cookie, and configuration checks remain fail-closed; lint remediation did not
weaken those boundaries.

## 7. Tests added, changed, and results

Five net-new library regression tests account for the increase from the
original library suite:

- config_debug_redacts_secrets;
- admin_action_debug_redacts_plaintext_passwords;
- converts_available_blocks_to_bytes;
- saturates_unrepresentable_available_capacity;
- query_failure_skips_the_preflight.

The new tests/cli_runtime.rs integration target contains four black-box tests:

- help_and_version_do_not_create_runtime_state;
- explicit_data_dir_owns_all_initialized_runtime_state;
- relative_data_dir_is_rejected_before_filesystem_mutation;
- direct_https_process_exposes_only_https_and_redirect_with_secure_login_cookies.

The direct-HTTPS process test retains the disabled plaintext port reservation
for the child lifetime, preventing an unrelated process from masking a
regression. It retries only address-in-use handoff collisions, at most three
times, for the two ports that the child must acquire.

Existing unit tests were broadly converted to contextual Result returns and
message-bearing assertions rather than setup unwraps/panics. Existing
authentication, backup/restore, traversal/symlink, posting, thread, media, and
worker regressions remain in place. The single documentation test is the
MediaProcessor construction example in src/media/mod.rs.

Both Rust 1.91 and stable produced the same successful totals:

| Suite | Result per toolchain |
| --- | --- |
| Library unit tests | 987 passed |
| Binary unit tests | 987 passed |
| cli_runtime integration tests | 4 passed |
| Documentation tests | 1 passed |
| Failures / ignored regressions | 0 failures |

## 8. Commands and outcomes

### Toolchains and lint discovery

Toolchain preparation completed successfully with the requested commands:

~~~sh
rustup toolchain install 1.91.0 --profile minimal
rustup component add clippy rustfmt --toolchain 1.91.0
rustup component add clippy rustfmt --toolchain stable
~~~

The installed validation versions were:

~~~text
rustc 1.91.0 (f8297e351 2025-10-28)
cargo 1.91.0 (ea2d97820 2025-10-10)
clippy 0.1.91 (f8297e351a 2025-10-28)

rustc 1.97.1 (8bab26f4f 2026-07-14)
cargo 1.97.1 (c980f4866 2026-06-30)
clippy 0.1.97 (8bab26f4f6 2026-07-14)
~~~

They were confirmed with:

~~~sh
rustc +1.91.0 --version
cargo +1.91.0 --version
cargo +1.91.0 clippy --version
rustc +stable --version
cargo +stable --version
cargo +stable clippy --version

rustc +1.91.0 -W help
cargo +1.91.0 clippy -- -W help
rustc +stable -W help
cargo +stable clippy -- -W help
rustup target list --installed --toolchain stable
~~~

All commands succeeded. The help output confirmed that the persistent names
are supported by Rust 1.91; stable-only findings were not added to the
persistent policy. The installed-target query confirmed that all four target
standard-library components used in section 10 were present. No unrecorded
target-add command is claimed.

### Canonical formatting and automatic remediation

~~~sh
cargo +1.91.0 clippy --fix --locked --workspace --all-targets --all-features --allow-dirty --allow-staged
cargo +1.91.0 fmt --all
cargo +1.91.0 fmt --all -- --check
git diff --check
~~~

The Rust 1.91 fix pass completed successfully and produced no additional
automatic source edits. The final format and whitespace checks passed. Stable
rustfmt was not used as a second formatting authority.

### Exhaustive check and Clippy matrix

Every command below passed:

~~~sh
cargo +1.91.0 check --locked --workspace --all-targets
cargo +1.91.0 clippy --locked --workspace --all-targets
cargo +1.91.0 check --locked --workspace --all-targets --no-default-features
cargo +1.91.0 clippy --locked --workspace --all-targets --no-default-features
cargo +1.91.0 check --locked --workspace --all-targets --no-default-features --features tls-acme
cargo +1.91.0 clippy --locked --workspace --all-targets --no-default-features --features tls-acme
cargo +1.91.0 check --locked --workspace --all-targets --all-features
cargo +1.91.0 clippy --locked --workspace --all-targets --all-features

cargo +stable check --locked --workspace --all-targets
cargo +stable clippy --locked --workspace --all-targets
cargo +stable check --locked --workspace --all-targets --no-default-features
cargo +stable clippy --locked --workspace --all-targets --no-default-features
cargo +stable check --locked --workspace --all-targets --no-default-features --features tls-acme
cargo +stable clippy --locked --workspace --all-targets --no-default-features --features tls-acme
cargo +stable check --locked --workspace --all-targets --all-features
cargo +stable clippy --locked --workspace --all-targets --all-features
~~~

### Tests

~~~sh
cargo +1.91.0 test --locked --workspace --all-targets --all-features
cargo +1.91.0 test --locked --workspace --doc --all-features
cargo +stable test --locked --workspace --all-targets --all-features
cargo +stable test --locked --workspace --doc --all-features
~~~

All four commands passed with the totals in section 7.

### Forced warning audits and nursery

~~~sh
cargo +1.91.0 clippy --locked --workspace --all-targets --all-features -- -A warnings --force-warn unused-crate-dependencies --force-warn clippy::multiple-crate-versions
cargo +stable clippy --locked --workspace --all-targets --all-features -- -A warnings --force-warn unused-crate-dependencies --force-warn clippy::multiple-crate-versions
cargo +stable clippy --locked --workspace --all-targets --all-features -- -A warnings -W clippy::nursery
~~~

Both forced audits exited successfully and each intentionally emitted 88
warning lines for the dependency findings recorded in sections 4 and 5. The
stable nursery audit was clean: zero nursery warnings remained. Normal check
and Clippy commands passed with all deny-level findings resolved.

The target-level unused-dependency ledger was generated with these three
locked Rust 1.91 probe forms:

~~~sh
cargo +1.91.0 rustc --locked --lib FEATURE_ARGS -- --force-warn unused-crate-dependencies
cargo +1.91.0 rustc --locked --bin rustchan-cli FEATURE_ARGS -- --force-warn unused-crate-dependencies
cargo +1.91.0 rustc --locked --test cli_runtime FEATURE_ARGS -- --force-warn unused-crate-dependencies
~~~

Each form was run with `FEATURE_ARGS` set to no-default, default,
ACME-only, and all-features, for 12 successful probes. Their exact compiler
diagnostic keys match all 314 ledger rows: 75 no-default, 78 default, 79
ACME-only, and 82 all-features.

### Metadata and dependency-graph audits

The final metadata and graph were inspected with these command categories:

~~~sh
cargo +1.91.0 metadata --locked --no-deps --format-version 1

cargo +1.91.0 tree --locked --duplicates --target all
cargo +1.91.0 tree --locked --duplicates --target all --no-default-features
cargo +1.91.0 tree --locked --duplicates --target all --no-default-features --features tls-acme
cargo +1.91.0 tree --locked --duplicates --target all --all-features

cargo +1.91.0 tree --locked --all-features --edges features
cargo +1.91.0 tree --locked --all-features -i arti-client@0.44.0
cargo +1.91.0 tree --locked --all-features -i rustls-webpki@0.103.13
cargo +1.91.0 tree --locked --all-features -i rusqlite@0.39.0
cargo +1.91.0 tree --locked --all-features -i r2d2_sqlite@0.34.0
cargo +1.91.0 tree --locked --all-features -i openssl-sys
cargo +1.91.0 tree --locked --all-features -i native-tls
~~~

Metadata succeeded and confirmed one workspace package, Rust 1.91, the library,
binary, and cli_runtime targets, and the two-feature graph. The four duplicate
queries produced the inputs summarized in section 4. Feature and inverse-tree
queries confirmed the exact Arti, webpki, and SQLite lines described in section
13. The openssl-sys and native-tls inverse queries intentionally reported that
the package IDs did not match any package, confirming that neither is present.

## 9. Tested feature configurations

RustChan has two package features, tls-self-signed and tls-acme. The following
four configurations are therefore the exhaustive powerset, and each received
both check and all-target Clippy validation on Rust 1.91 and stable:

| Configuration | Cargo arguments | TLS compilation state | Result |
| --- | --- | --- | --- |
| No TLS feature | --no-default-features | Neither optional TLS mode | Passed on both |
| Default | no feature arguments | tls-self-signed | Passed on both |
| ACME only | --no-default-features --features tls-acme | tls-acme only | Passed on both |
| All features | --all-features | Self-signed and ACME code | Passed on both |

The CI feature-matrix job directly covers no-default, default, and ACME-only on
both toolchains; the quality job runs each toolchain's all-feature
check/Clippy for the fourth state. Hyper is activated only by tls-acme.

## 10. Tested target triples

Cross validation used cargo check, not tests, and preserved --locked,
--workspace, --all-targets, and --all-features:

~~~sh
cargo +stable check --locked --workspace --all-targets --all-features --target aarch64-apple-darwin
cargo +stable check --locked --workspace --all-targets --all-features --target aarch64-unknown-linux-gnu
cargo +stable check --locked --workspace --all-targets --all-features --target x86_64-unknown-linux-gnu
cargo +stable check --locked --workspace --all-targets --all-features --target x86_64-pc-windows-msvc
~~~

| Target | Outcome |
| --- | --- |
| aarch64-apple-darwin | Exit 0; also covered by native host checks/tests |
| aarch64-unknown-linux-gnu | Exit 0 compile check |
| x86_64-unknown-linux-gnu | Exit 101: ring could not find external x86_64-linux-gnu-gcc |
| x86_64-pc-windows-msvc | Exit 101: ring C compilation could not find the MSVC SDK assert.h header |

Isolated CARGO_TARGET_DIR values were used to prevent cross-target artifact
collisions. No cross-target test execution is claimed. CI retains native
platform smoke builds for aarch64 Linux, Apple silicon macOS, and x86_64 MSVC
Windows.

## 11. Incomplete validation and external caveats

All host formatting, check, Clippy, nursery, unit, integration, documentation,
and exhaustive feature validation completed. The only incomplete compile
checks are the two externally blocked targets in section 10:

- x86_64-unknown-linux-gnu needs an x86_64 Linux C cross-compiler, specifically
  x86_64-linux-gnu-gcc, in the host environment.
- x86_64-pc-windows-msvc needs a usable MSVC/Windows SDK header and C compiler
  environment; ring could not find assert.h.

The platform CI jobs provide the appropriate native SDK environments, but they
were not executed from this local workspace. Live Tor bootstrap, public ACME
issuance, external ffmpeg behavior, and Windows Explorer/console interaction
were not exercised by local unit tests. Unix free-space reporting is now an
in-process rustix call and has normal, overflow, and query-failure regression
coverage; it no longer depends on an external df executable or process locale.

The production chan library retains its original public module surface. The
standalone binary continues to compile its runtime modules locally; root
modules are public only within that non-linkable binary crate so
unreachable_pub can verify their nested APIs. The same runtime modules are
present in chan only under cfg(test) for existing unit-test support, so this
policy pass does not add a production library API surface.

## 12. Concise diff stat and changed paths

The tracked diff before adding the untracked audit artifacts is:

~~~text
139 files changed, 20,217 insertions(+), 10,403 deletions(-)
~~~

Tracked changed paths, grouped without omitting files:

- Root and CI: .github/workflows/ci.yml, .gitignore, Cargo.toml, Cargo.lock.
- src/{banner.rs,cache.rs,captcha.rs,config.rs,detect.rs,error.rs,favicon.rs,lib.rs,logging.rs,main.rs,models.rs,pending_fs.rs,test_fixtures.rs,test_support.rs,theme.rs,theme_builder.rs,workers/mod.rs}.
- src/chan_net/{command.rs,export.rs,import.rs,ledger.rs,mod.rs,poll.rs,refresh.rs,selective_snapshot.rs,snapshot.rs,status.rs}.
- src/config/template.rs.
- src/db/{admin.rs,banners.rs,boards.rs,chan_net.rs,fs_ops.rs,migrations.rs,mod.rs,pool.rs,posts.rs,schema.rs,setup.rs,themes.rs,threads.rs,types.rs,user_thread_prefs.rs}.
- src/handlers/{banner.rs,board.rs,captcha.rs,favicon.rs,mod.rs,posting.rs,render.rs,setup.rs,thread.rs}.
- src/handlers/admin/{auth.rs,backup.rs,content.rs,mod.rs,moderation.rs,settings.rs}.
- src/handlers/admin/backup/{archive.rs,common.rs,create.rs,downloads.rs,http.rs,listing.rs,restore_board.rs,restore_full.rs,saved_backup.rs,types.rs}.
- src/handlers/admin/settings/{appearance.rs,backup_settings.rs,banners.rs,board.rs,maintenance.rs,site.rs,themes.rs}.
- src/handlers/board/{access_preferences.rs,catalog.rs,create_thread.rs,media.rs,pages.rs,reports.rs,tests.rs}.
- src/media/{convert.rs,ffmpeg.rs,mod.rs,prune.rs,thumbnail.rs}.
- src/middleware/{backup_progress.rs,csrf.rs,ip.rs,mod.rs,normalize.rs,rate_limit.rs,state.rs,transport.rs}.
- src/server/{cli.rs,mod.rs,server.rs}.
- src/server/console/{dashboard.rs,input.rs,mod.rs,wizard.rs}.
- src/server/server/{assets.rs,headers.rs,lifecycle.rs,observability.rs,router.rs,routes.rs}.
- src/templates/{admin.rs,board.rs,forms.rs,mod.rs,thread.rs}.
- src/templates/admin/{appearance.rs,backups.rs,boards.rs,layout.rs,maintenance.rs,moderation.rs,site_health.rs}.
- src/tls/{acme.rs,mod.rs,self_signed.rs}.
- src/utils/{crypto.rs,files.rs,fs_security.rs,mod.rs,redirect.rs,sanitize.rs,tripcode.rs}.
- src/utils/files/{disk_space.rs,jpeg.rs,mime.rs,storage.rs}.
- src/utils/sanitize/formatting.rs.

The required new files remain untracked at this audit snapshot and must be
included in the eventual handoff:

- docs/strict-lint-exceptions.tsv;
- docs/strict-dependency-duplicates.tsv;
- docs/strict-unused-dependencies.tsv;
- docs/strict-rust-policy-audit.md;
- tests/cli_runtime.rs.

The .gitignore change makes tests/cli_runtime.rs trackable while leaving other
ad-hoc test/browser artifacts ignored. No commit was created.

## 13. Dependency and compatibility confirmations

- Arti remains at 0.44.0. arti-client still disables default features and
  explicitly enables tokio, rustls, onion-service-service, and flowctl-cc.
  tor-hsservice and tor-cell also remain on 0.44.0 with defaults disabled.
- Rust 1.91 remains the Cargo-visible workspace MSRV and cargo-msrv metadata
  value.
- The HTTP and Tor clients remain on pure-Rust rustls paths. Reqwest uses
  rustls-no-provider with defaults disabled; the Arti crates disable defaults;
  rustls and tokio-rustls explicitly use the ring provider. Dependency-tree
  inverse queries find neither openssl-sys nor native-tls in the all-feature
  graph.
- The default feature remains tls-self-signed. tls-acme remains optional and
  enables optional hyper plus rustls-acme; both modes compile separately and
  together.
- SQLite remains on rusqlite 0.39.0 with bundled and backup features plus
  r2d2_sqlite 0.34.0. This preserves compatibility with Arti's
  tor-dirmgr rusqlite >=0.36,<0.40 links constraint.
- rustls-webpki remains a direct, default-feature-disabled minimum of 0.103.13
  to hold the RUSTSEC-2026-0049 CRL distribution-point fix across future
  lockfile regeneration.
- Cargo.lock contains no package/version churn from this policy pass; its direct
  RustChan dependency list drops once_cell, libc, and windows-sys after their
  uses were removed and adds rustix, whose 1.1.4 package was already resolved
  transitively.

All deny-level Rust 1.91 and stable host findings are resolved, the intentionally
non-gating dependency findings are exhaustively recorded, and no commit was
created.
