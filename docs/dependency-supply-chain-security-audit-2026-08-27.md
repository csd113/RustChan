# RustChan Dependency and Software-Supply-Chain Security Audit

Audit date: 2026-08-27 (America/Vancouver)  
Audited repository state: branch `v1.4.1`, commit `695ca0f771468e79895b21d99667e3957ceab631`, plus the uncommitted remediations described below  
Package: `rustchan` 1.4.0  
Primary release targets: Linux x86-64, Linux ARM64, macOS Apple Silicon, and Windows x86-64 MSVC

## Executive Summary

This audit found no demonstrated remote-code-execution, authentication-bypass, authorization-bypass, SQL-injection, command-injection, arbitrary-file-write, or TLS-verification-bypass path caused by the audited dependency graph or RustChan's use of it. It did find several practical availability and robustness weaknesses at anonymous upload and HTTP boundaries, two dependency versions that required immediate patching, and weaker but material supply-chain controls in CI and release automation.

The ranked findings use the severity at discovery, before remediation:

| Severity | Findings before remediation | Residual findings after remediation |
|---|---:|---:|
| Critical | 0 | 0 |
| High | 1 | 0 |
| Medium | 7 | 1 |
| Low | 4 | 3 |
| **Total** | **12** | **4** |

Exploitability classification at discovery:

- **Confirmed exploitable:** 0. No finding was promoted to this class without a safe end-to-end reproduction of the vulnerable condition in RustChan.
- **Likely exploitable:** 3 availability paths reachable by an unauthenticated remote client: unbounded concurrent image decoding, the reachable `h2` HTTP/2 denial-of-service advisory, and malformed/oversized multipart envelope handling. All three are remediated.
- **Potentially exploitable:** 5 paths whose prerequisites or end-to-end impact were not safely demonstrated: subprocess output/lifecycle exhaustion, authenticated ChanNet archive/import exhaustion and partial state, oversized federation responses, `chacha20` x86 undefined behavior, and a database-controlled Argon2 PHC cost bomb. All five are remediated.
- **Present but currently unreachable:** 1 known vulnerability, `rsa` 0.9.10 / RUSTSEC-2023-0071. The crate is compiled through Arti, but the vulnerable private-key decryption operation was not found on a RustChan path.
- **Build-time only:** `paste` 1.0.15 is an active proc macro with an unmaintained advisory; no runtime exploit is associated with the notice. `bincode` 2.0.1 is lockfile-only and absent even from the all-target/all-feature activated graph.

The two dependencies requiring immediate upgrades were upgraded:

- `h2` 0.4.15 to 0.4.16, removing RUSTSEC-2026-0258.
- `chacha20` 0.10.1 to 0.10.2, replacing the yanked x86 SIMD implementation with the upstream correction.

There are no remaining immediate dependency upgrades justified by a known security defect. `cargo outdated` also identified newer direct versions of `argon2`, `rustls-webpki`, and `uuid`; none was upgraded merely for being newer because the audited versions have no applicable fixed security defect and major-version churn would not address a demonstrated risk.

The principal residual supply-chain risk is release provenance: release jobs are now validated and action/tool versions are pinned, but a maintainer can still tag an unmerged commit, `cross` may select mutable container images, and release artifacts have checksums but no SBOM, signature, or artifact attestation. The Cargo graph itself has strong source integrity: all 661 external lockfile packages are checksum-pinned crates.io packages; there are no Git, path, alternate-registry, patch, or replacement dependencies.

In count form, the residual supply-chain concerns are three: incomplete release provenance (F-09), external executable patch/provenance responsibility (F-11), and repository enforcement/monitoring gaps (F-12). The `rsa` and `paste` upstream maintenance/advisory state is additionally monitored, but no reachable runtime vulnerability was established for either affected operation.

## Scope and Method

The audit covered the sole workspace manifest, lockfile, every direct/optional/target dependency declaration, activated feature and target graphs, dependency sources, duplicate versions, custom build targets, proc macros, native `links` packages, the three tracked GitHub Actions workflows, release tooling, and external media/PDF executables invoked by RustChan. It also traced dependency APIs from anonymous HTTP, uploads, federation, stored authentication state, filesystem state, and background jobs.

Evidence was collected with `cargo metadata`, `cargo tree` (including reverse and duplicate trees), `cargo audit` 0.22.1, the RustSec advisory database, `cargo deny` 0.19.0, `cargo outdated`, source/call-site searches, lockfile parsing, workflow linting, action-reference verification, controlled malformed-input tests, and live read-only GitHub repository-policy queries. The RustSec database contained 1,226 advisories at commit `6420e39260b3d771b049954cf5d52b57e2118da4` (2026-08-27T14:01:44+02:00).

`cargo geiger` was not used. Unsafe/native exposure was instead inventoried from Cargo metadata and reviewed by reachability and targeted source inspection. That gives stronger application-context evidence than a raw unsafe-line count but is not a formal audit of every unsafe block in 662 packages. Likewise, this was not a maintainer-identity or crate-publication forensics engagement; absence of suspicious behavior means none was found in the inspected sources and metadata, not that compromise is cryptographically impossible.

### Dependency inventory

| Item | Result |
|---|---:|
| Workspace manifests / members | 1 / 1 |
| Direct dependencies (base / target-specific / total) | 55 / 1 / 56 |
| Direct optional dependencies | 5 |
| Lockfile packages | 662 (RustChan plus 661 external) |
| External packages with registry checksum | 661 of 661 |
| Git / alternate-registry / external path dependencies | 0 / 0 / 0 |
| `[patch]` / `[replace]` entries | 0 / 0 |
| Raw proc-macro / custom-build / `links` packages | 52 / 82 / 6 |

The post-remediation manifest SHA-256 is `680f670e1d20b54e27b78a323b4156e8cb032d4ea09699a68f24f96389591420`; the lockfile SHA-256 is `bd447bb405734387295423e7ff717d888026cbe171837895eccc80e4bfcd9c7e`.

### Activated graphs

Counts include the RustChan root and normal, build, and development edges:

| Target/features | Packages | Proc macros | Custom builds | Native `links` |
|---|---:|---:|---:|---:|
| macOS ARM64, default | 525 | 42 | 44 | 3 |
| macOS ARM64, no default features | 516 | not separately materialized | not separately materialized | not separately materialized |
| macOS ARM64, all features | 544 | 42 | 46 | 3 |
| Linux x86-64, all features | 542 | 43 | 47 | 3 |
| Linux ARM64, all features | 541 | 42 | 47 | 3 |
| Windows x86-64 MSVC, all features | 556 | 45 | 49 | 3 |
| All targets, all features | 627 | 48 | 75 | 5 |

The default-feature delta is nine certificate/self-signed-TLS packages. Enabling all features adds 19 ACME and async client packages, including `rustls-acme` 0.15.4, `rcgen` 0.13.2, `x509-parser` 0.18.1, and two `webpki-roots` versions. No debug-only parser or test backdoor was found in the release feature sets.

## Ranked Findings

### F-01 — Anonymous image uploads could monopolize decoder memory and CPU

- **Severity:** High
- **Confidence:** High
- **Exploitability:** Likely exploitable before remediation; remediated
- **Dependency:** `image` 0.25.10 (direct); transitive codec implementations selected by its `jpeg`, `png`, `gif`, `webp`, `bmp`, `tiff`, and `ico` features
- **Fixed version:** Not a dependency advisory; RustChan-side limits were required
- **Dependency path:** `rustchan -> image 0.25.10 -> selected pure-Rust decoders`
- **Vulnerable functionality:** Image dimension discovery, decoding, and thumbnail generation without a shared concurrency limit or explicit decoder allocation/pixel ceiling
- **RustChan call sites:** `parse_post_multipart`, `process_primary_upload`, `validate_upload_for_storage`, `validate_decodable_image`, `generate_thumbnail`, and `image_crate_thumbnail`
- **Entry point / privilege:** Anonymous `POST /{board}` thread creation and `POST /{board}/{thread}` replies with image bytes; no account or elevated role required
- **Exploit class / impact:** Resource-exhaustion denial of service. A small, highly expansive or simply very large-dimension image could drive decoder allocation and CPU, while concurrent requests multiplied the cost.

The pre-existing byte-size checks constrained persisted input but did not provide a decoded-pixel budget and did not serialize the expensive decode/processor phase. This is meaningful because compressed byte count is not a reliable upper bound on decoded image memory.

Remediation implemented:

- Added a fail-fast, process-wide one-permit `MediaUploadGate`. It is acquired when the first non-empty media byte is staged, retained through the non-cancellable blocking processing closure, and returns `503 Service Unavailable` with `Retry-After` on overlap. Empty browser file controls do not consume it.
- Added a 40,000,000-pixel maximum and an `image` decoder `max_alloc` of 256 MiB for both validation and thumbnail decoding.
- Introduced opaque `ValidatedUpload` handoff so the already validated temporary path, media classification, size, and metadata are reused instead of running the expensive validation twice. This is an application invariant, not a claim that a hostile local user can never race the temporary file.

Regression evidence includes `media_upload_gate_rejects_overlap_and_recovers_after_drop`, `media_upload_gate_permit_outlives_cancelled_join_waiter`, `multipart_parser_holds_media_gate_from_first_upload_byte`, `multipart_parser_does_not_gate_empty_file_control`, `untrusted_image_decoder_allocation_budget_is_bounded`, and `image_dimension_budget_accepts_8k_and_rejects_over_40_megapixels`.

### F-02 — Reachable `h2` HTTP/2 denial of service (RUSTSEC-2026-0258)

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Likely exploitable before remediation; patched
- **Dependency:** `h2` 0.4.15 to 0.4.16 (transitive)
- **Advisory:** [RUSTSEC-2026-0258](https://rustsec.org/advisories/RUSTSEC-2026-0258.html)
- **Dependency paths:** `rustchan -> hyper 1.11.0 -> h2`; the same `hyper`/`h2` stack is activated through `axum`, `axum-server`, `hyper-util`, and `reqwest`/`hyper-rustls`
- **Vulnerable functionality:** Malformed HTTP/2 protocol state could trigger a server panic/availability failure in affected `h2` versions
- **RustChan call site:** The public server explicitly enables Hyper's HTTP/2 builder in `src/server/server.rs`; outbound Reqwest also activates the package
- **Entry point / privilege:** Anonymous remote HTTP/2 connection to the public listener
- **Exploit class / impact:** Remote denial of service; no evidence of confidentiality or code-execution impact

`Cargo.lock` was updated to `h2` 0.4.16. Reverse-tree checks show only 0.4.16, and `cargo tree -i h2@0.4.15` no longer matches any package. A dedicated weaponized protocol fixture was not added: the advisory version boundary and removal of the affected version are the primary proof, while normal HTTP/2 server tests continue to pass.

### F-03 — Public multipart request/envelope processing was insufficiently bounded

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Likely exploitable before remediation; remediated
- **Dependencies:** `axum` 0.8.9 `multipart` -> `multer` 3.1.0; `tower-http` 0.7.0 (direct)
- **Fixed version:** No applicable advisory; configuration and application streaming limits were required
- **Dependency path:** `rustchan -> axum::extract::Multipart -> multer 3.1.0`; request cap supplied by `rustchan -> tower-http::limit`
- **Vulnerable functionality:** Public posting accepted multipart streams without a whole-request cap, field-count/aggregate budget, and an early cap for an unterminated preamble or field-header envelope
- **RustChan call sites:** `create_thread`, `create_reply`, `parse_post_multipart`, public route assembly in `src/server/server/routes.rs`
- **Entry point / privilege:** Anonymous thread/reply multipart requests
- **Exploit class / impact:** Connection, memory, temporary-storage, and CPU exhaustion; especially malformed multipart that delays field production while continuing to stream bytes

The parser already enforced board-specific file limits after a file field was recognized, but that did not bound bytes before the first valid field or the entire request. The fix enables `tower-http`'s `limit` feature and applies a 516 MiB transport ceiling (512 MiB aggregate field budget plus 4 MiB envelope allowance), a streaming KMP-based envelope scanner, 64 KiB limits for preamble and each field-header envelope, a 200-byte boundary limit, at most 64 fields, 64 KiB text/unknown-field limits, and a ten-minute whole-upload timeout. The scanner retains only pattern/state, not the body. The aggregate maximum intentionally preserves existing configured large-media behavior; the early media gate prevents multiple expensive media stages.

Tests cover exact and first-excess boundaries, chunk-split delimiters, oversized declared bodies, unterminated headers, oversized preamble, aggregate bytes, field counts, duplicate upload slots, unknown fields, empty file controls, and route-level `413` behavior. Representative tests are `public_post_routes_reject_oversized_declared_bodies`, `public_post_routes_reject_oversized_multipart_field_headers`, `public_multipart_envelope_scanner_rejects_unterminated_large_headers`, and `public_multipart_budget_enforces_aggregate_bytes_and_field_count`.

### F-04 — Media subprocess pipes and descendant lifecycle could exhaust the service

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Potentially exploitable before remediation; remediated
- **Dependencies / executables:** Rust standard/Tokio process APIs controlling FFmpeg, FFprobe, `pdftoppm`, `mutool`, and `qlmanage`
- **Current/fixed version:** Application configuration defect; external executable versions remain operator-managed
- **Dependency path:** Untrusted upload -> RustChan media validation/worker -> `std::process::Command` or `tokio::process::Command` -> external parser/transcoder
- **Vulnerable functionality:** Captured stdout/stderr could grow without a retained-output ceiling; timeouts or future cancellation could leave descendants alive
- **RustChan call sites:** `run_std_command_with_timeout`, `wait_for_ffmpeg_output`, media probe/transcode/waveform paths, and PDF renderer paths
- **Entry point / privilege:** Anonymous media upload; execution occurs synchronously during validation or later in a background job
- **Exploit class / impact:** Memory/pipe exhaustion and orphaned processor processes causing persistent CPU/memory exhaustion

Commands were already constructed as argv arrays rather than shell strings, so this was not command injection. The remediation places media processes in a process group, drains both pipes to EOF while retaining at most 64 KiB from each, terminates the group on timeout/cancellation, and reaps the direct child. This applies to synchronous and asynchronous processor paths.

Regression evidence includes `blocking_subprocess_output_is_retained_within_limit`, `async_subprocess_output_is_retained_within_limit`, `blocking_timeout_kills_parent_and_descendant`, `async_timeout_kills_parent_and_descendant`, `cancellation_kills_parent_and_descendant`, and `repeated_timeouts_do_not_accumulate_descendants`. The descendant tests are platform-gated where process groups are available.

### F-05 — ChanNet snapshot import needed decompression, object, concurrency, schema, and atomicity boundaries

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Potentially exploitable before remediation; remediated
- **Dependencies:** `zip` 8.6.0, `serde_json`, `rusqlite` 0.39.0 (direct); `libsqlite3-sys` 0.37.0 (transitive native binding)
- **Fixed version:** No applicable crate advisory; RustChan-side validation and transaction design were required
- **Dependency path:** Authenticated `/chan/import` or polling -> ZIP/Deflate -> JSON deserialization -> SQLite writes
- **Vulnerable functionality:** Declared ZIP sizes were not a sufficient expansion bound; arrays could materialize too many objects; concurrent imports multiplied work; transaction-ID claiming and content mutation were not one failure-atomic unit; snapshot identifiers/text/integer conversion needed a validated domain boundary
- **RustChan call sites:** `unpack_snapshot`, `parse_bounded_snapshot_array`, `validate_snapshot`, `commit_snapshot`, `chan_import`, and `chan_poll`
- **Entry point / privilege:** A caller with the ChanNet API key, or a configured gateway returning a malicious snapshot. This is not an anonymous endpoint.
- **Exploit class / impact:** Decompression/JSON/SQLite resource exhaustion, inconsistent partial import state, replay-ledger desynchronization, and unsafe integer/domain coercion

The import now accepts exactly three unique whitelisted entries (`boards.json`, `posts.json`, `metadata.json`), never extracts paths to disk, checks declared and actual decompressed bytes, and caps the total at 64 MiB (boards 8 MiB, posts 64 MiB, metadata 64 KiB). A bounded Serde sequence visitor permits at most 4,096 boards and 100,000 posts before materializing another object. ZIP/JSON work runs in `spawn_blocking` under a fail-fast one-permit `ChanImportGate`, shared by direct import and poll; cloned guards keep the permit alive if the awaiting task is cancelled.

Validation is linear in boards plus posts. It requires unique declared board IDs of one to eight lowercase ASCII letters/digits, requires every post to reference a declared board, caps title/author/content at 64/255/32,768 characters, requires metadata `post_count` to match exactly, and uses checked `u64` to `i64` conversions for remote IDs and timestamps. Errors do not reflect attacker-sized identifiers. The durable transaction-ID claim, board inserts, and post inserts now share one SQLite transaction. Duplicate post rows within an otherwise accepted snapshot remain intentionally idempotent through `INSERT OR IGNORE`; poll counts therefore describe declared posts in accepted snapshots, not necessarily newly inserted rows.

Tests cover expansion beyond the byte limit, first object beyond the count limit, hostile identifiers, undeclared references, integer overflow, oversized-error reflection, shared-gate overlap/recovery, forced failure on the second post, rollback of board/post/claim rows, successful retry, and typed replay rejection with unchanged rows. The key failure-atomicity test is `snapshot_commit_rolls_back_content_and_claim_on_mid_import_failure`.

Residual operational caveats are bounded rather than security blockers: a valid 100,000-post import still performs many SQLite inserts; zero IDs, nil transaction IDs, and extreme but representable timestamps are semantically accepted; and the new 64-character imported-title policy may reject a snapshot produced by a less-strict historical CLI path.

### F-06 — Federation gateway response bodies were unbounded

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Potentially exploitable before remediation; remediated
- **Dependency:** `reqwest` 0.13.4 -> Hyper/Rustls stack (direct/transitive)
- **Fixed version:** No dependency advisory; streaming application limits were required
- **Dependency path:** RustChan scheduled/admin ChanNet operation -> configured RustWave gateway -> Reqwest response body
- **Vulnerable functionality:** Buffering a gateway-controlled response without enforcing a `Content-Length` and actual streamed-byte maximum
- **RustChan call sites:** `chan_poll`, `chan_refresh`, and shared bounded response helpers
- **Entry point / privilege:** Malicious or compromised configured gateway, DNS/network compromise that still satisfies TLS trust, or administrator misconfiguration; not a per-request anonymous URL
- **Exploit class / impact:** Memory and bandwidth exhaustion

Poll responses are capped by `CONFIG.chan_net_max_body`; refresh JSON responses are capped at 64 KiB. Both reject oversized declared lengths and stop when streamed chunks exceed the remaining budget. The shared Reqwest client has a 30-second timeout. `gateway_response_buffer_rejects_chunk_beyond_remaining_budget` covers the critical boundary; import/poll route tests exercise the integrated flow.

### F-07 — Yanked `chacha20` x86 SIMD implementation contained undefined behavior

- **Severity:** Medium
- **Confidence:** High that affected code was built/reachable; Low that a remote attacker could shape it into a security impact
- **Exploitability:** Potentially exploitable before remediation; patched
- **Dependency:** `chacha20` 0.10.1 to 0.10.2 (transitive)
- **Advisory/source:** Upstream [`stream-ciphers` correction PR #580](https://github.com/RustCrypto/stream-ciphers/pull/580); 0.10.1 was yanked
- **Dependency path:** `rustchan -> arti-client 0.45.0` and many `tor-*` crates -> `rand 0.10.2 -> chacha20`
- **Vulnerable functionality:** The 0.10.1 x86 SSE2/SSE4.1 backend could invoke target-feature instructions through an unsound boundary, producing undefined behavior on affected builds/CPUs
- **RustChan call site:** Arti client/onion-service randomness and upstream Tor utility code; RustChan does not call `chacha20` directly
- **Entry point / privilege:** Runtime Tor activity on x86 deployments; no proven attacker-controlled nonce/key/instruction-selection path
- **Exploit class / impact:** Memory-safety/process-integrity concern or crash in an unsafe SIMD implementation. No practical RustChan exploit was demonstrated.

`Cargo.lock` now resolves only 0.10.2; 0.10.1 no longer appears in reverse or duplicate trees. The complete workspace test suite and x86-target dependency analysis are the compatibility evidence; no attempt was made to provoke undefined behavior on an affected CPU.

### F-08 — Stored Argon2 PHC strings could select excessive verification cost

- **Severity:** Low
- **Confidence:** High
- **Exploitability:** Potentially exploitable before remediation; remediated
- **Dependency:** `argon2` 0.5.3 (direct)
- **Fixed version:** Application validation defect, not a crate advisory
- **Dependency path:** Stored administrator password PHC string -> `verify_password` -> `argon2::PasswordVerifier`
- **Vulnerable functionality:** The PHC string selects memory, time, and parallelism parameters; blindly accepting it lets corrupted/restored database state force excessive resource use on login
- **RustChan call site:** `src/utils/crypto.rs::verify_password`
- **Entry point / privilege:** Requires control of restored/configured database content, administrator workflow, or pre-existing local/database write access; a remote password alone cannot choose the PHC parameters
- **Exploit class / impact:** Authentication-path CPU/memory denial of service

Verification now parses and validates the supported Argon2 algorithm, version, parameters, salt, and hash before work, then enforces `m <= 65,536 KiB`, `t <= 4`, and `p <= 4`. New RustChan hashes remain Argon2id v0x13; accepting other algorithms understood by the existing Argon2 verifier preserves compatibility with stored hashes without letting them bypass the cost caps. Wrong passwords remain a normal `false`; malformed/unsupported hashes are errors. `verify_password_rejects_excessive_phc_cost_before_hashing` proves rejection occurs before hashing.

### F-09 — Release provenance and source-to-tag policy remain incomplete

- **Severity:** Medium
- **Confidence:** High
- **Exploitability:** Supply-chain hardening concern; unresolved residual
- **Dependency/tooling:** GitHub Actions, `cross` 0.2.5, hosted runner images, GitHub Releases
- **Current/fixed version:** Actions and installers are SHA/version pinned; no single fixed version resolves the policy gap
- **Dependency path:** Maintainer/tag or build-host control -> release workflow -> distributed archives
- **Vulnerable functionality:** A tag can point to a commit not merged to the protected default branch; default `cross` container image references and hosted runner images are not content-digest pinned; output has SHA-256 checksums but no signed provenance/SBOM/attestation
- **RustChan call sites:** `.github/workflows/release.yml`
- **Entry point / privilege:** Compromised maintainer token, repository administration, action/build-host compromise, or upstream mutable image compromise
- **Exploit class / impact:** Substitution of source, toolchain, native build inputs, or released binaries

Implemented hardening includes a release-validation job, package-version/tag equality, Rust 1.91.0 formatting/Clippy/tests/`cargo deny`, a dependency edge from every build to validation, exact `cross` 0.2.5 installation, exact cargo security-tool versions with no fallback, and immutable 40-hex action SHAs. Release checksums are verified before publication. GitHub's [secure-use guidance](https://docs.github.com/en/actions/reference/security/secure-use?learn=getting_started&learnProduct=actions) treats full commit-SHA action references as the immutable form.

Recommended remaining work is governance/operator work: require release tags to resolve to protected default-branch commits, use a protected release environment with approval, pin or build `cross` images by digest, generate an SBOM, and add GitHub artifact attestations or another verifiable signing/provenance mechanism. GitHub documents immutable action references as a security control and provides [artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).

### F-10 — `rsa` Marvin Attack advisory is present but private decryption is unreachable

- **Severity:** Low residual
- **Confidence:** Medium-High for current unreachability
- **Exploitability:** Present but currently unreachable
- **Dependency:** `rsa` 0.9.10 (transitive); no fixed release is available
- **Advisory:** [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html), upstream [issue #626](https://github.com/RustCrypto/RSA/issues/626)
- **Dependency path:** Representative paths are `rustchan -> arti-client/tor-hsservice -> tor-key-forge or tor-llcrypto -> rsa`, and `ssh-key-fork-arti -> rsa`
- **Vulnerable functionality:** Timing side channel in PKCS#1 v1.5 private-key decryption that can become a decryption oracle
- **RustChan call site:** No direct RSA call. Source/call-site review found public verification and generic key generation/signing support in the Arti graph, but no `Pkcs1v15Encrypt`, OAEP private decrypt, `decrypt_blinded`, or request-driven RSA decryption oracle on RustChan's v3 onion-service path.
- **Entry point / privilege:** None identified for the vulnerable operation
- **Exploit class / impact:** If a future Arti/RustChan path begins attacker-observable private RSA decryption, recovery of protected plaintext or key-related information could become possible

Because no fixed `rsa` version exists and removing Arti would be a disproportionate behavior change, the advisory remains a dated, documented exception in `deny.toml` and the audit command. It must be re-reviewed on every Arti/RSA update or any introduction of RSA decryption. This is not reported as exploitable merely because the package exists.

### F-11 — External media/PDF parsers are outside Cargo patch enforcement

- **Severity:** Low residual
- **Confidence:** High
- **Exploitability:** Environment-dependent supply-chain/runtime concern
- **Dependencies:** FFmpeg/FFprobe and optional Poppler `pdftoppm`, MuPDF `mutool`, macOS `qlmanage`
- **Current versions observed on the audit host:** FFmpeg/FFprobe 9.0.1, Poppler 26.05.0, no `mutool`, system `/usr/bin/qlmanage`
- **Fixed version:** Track vendor security releases; no applicable vulnerable host version was established by this audit
- **Dependency path:** Anonymous media/PDF upload -> RustChan argv construction -> installed external executable
- **Vulnerable functionality:** Large native parsers consume hostile media bytes; their patch state and binary provenance are not represented in `Cargo.lock`
- **RustChan call site:** Media validation, thumbnailing, transcoding, waveform generation, and version probes
- **Entry point / privilege:** Anonymous uploader for enabled media types; local/package-manager attacker for binary substitution
- **Exploit class / impact:** Parser crash, resource exhaustion, or native-code compromise if an installed tool contains an applicable vulnerability

RustChan now supplies timeouts, process-group cleanup, bounded retained output, deterministic executable names, and argv rather than shell construction. Operators must still install trusted, patched tools and restrict `PATH`/service-account write access. Upstream publishes [FFmpeg releases](https://ffmpeg.org/download.html) and [security notices](https://ffmpeg.org/security.html). This audit did not assert that a current executable is vulnerable without a version-applicable advisory and reachable codec proof.

### F-12 — Repository enforcement and monitoring controls are weaker than the workflow definitions

- **Severity:** Low residual
- **Confidence:** High (dated 2026-08-27 settings snapshot)
- **Exploitability:** Supply-chain governance concern
- **Dependency/tooling:** GitHub repository settings, Dependabot, secret scanning, GitHub Actions policy
- **Current version:** Not applicable
- **Dependency path:** Repository contributor/maintainer or compromised credential -> workflow/dependency change -> protected branch/build
- **Vulnerable functionality:** Organization/repository policy does not require action SHA pinning, allows all actions, has no rulesets, and has Dependabot security updates plus secret scanning/push protection disabled
- **RustChan call site:** Repository settings and `.github/workflows/*.yml`
- **Entry point / privilege:** Contributor plus a review/control failure, compromised maintainer, or malicious upstream action reference introduced in a later change
- **Exploit class / impact:** Delayed advisory response, secret exposure, or future mutable-action supply-chain exposure

The `main` branch does require strict successful `Quality`, `Tests`, and three platform-smoke checks; administrator enforcement is enabled. There are no required approvals or CODEOWNERS approval. The standalone audit job is not a required check, so `cargo deny --locked check` was also added to the already required `Quality` job. All 25 action uses in the three tracked workflows are now full immutable SHAs, but repository policy does not prevent a future regression.

Recommended controls are enabling Dependabot security updates, secret scanning and push protection where available, requiring review for dependency/workflow changes, and enforcing immutable action references at organization/repository policy level. These are administrator changes and were not made by this source patch.

## Dependency Security Table

“Current” is the post-remediation version/configuration. A recommendation of “keep” means no security-driven upgrade is presently warranted.

| Dependency | Current | Recommended | Direct/transitive | Security issue | Reachable? | Severity | Action |
|---|---|---|---|---|---|---|---|
| `h2` | 0.4.16 | 0.4.16 or later compatible patched release | Transitive | RUSTSEC-2026-0258 HTTP/2 DoS affected 0.4.15 | Yes, public HTTP/2 server | Medium | **Upgraded**; deny regression through audit/lock checks |
| `chacha20` | 0.10.2 | 0.10.2 or later compatible release | Transitive via `rand`/Arti | Yanked 0.10.1 x86 SIMD undefined behavior | Runtime code is reachable; attacker control uncertain | Medium | **Upgraded**; confirmed old version absent |
| `image` | 0.25.10 | Keep; retain application limits | Direct | Decoder memory/pixel/concurrency exhaustion | Yes, anonymous image uploads | High | **Mitigated** with 40 MP, 256 MiB allocation, and one-at-a-time gate |
| `axum` / `multer` | 0.8.9 / 3.1.0 | Keep; retain streaming budgets | Direct / transitive | Multipart envelope, aggregate, and field count not fully bounded by prior app configuration | Yes, anonymous posting | Medium | **Mitigated** with request/envelope/field/timeout limits |
| `tower-http` | 0.7.0 with `limit` | Keep | Direct | Body-limit feature was not enabled | Yes, public posting routes | Medium | **Feature enabled** and route caps applied |
| `zip` | 8.6.0 with `deflate-flate2-zlib-rs` | Keep | Direct | Decompression/object exhaustion; generic `deflate` also pulled unnecessary `zopfli` | Yes, authenticated ChanNet import/poll | Medium | **Mitigated** with exact entries, byte/object caps, and narrowed feature |
| `zopfli` | Absent (was 0.8.3) | Remain absent | Former transitive | Expensive compression implementation was unnecessary for snapshot decode | No needed runtime path | Informational | **Removed** by ZIP feature narrowing |
| `reqwest` | 0.13.4, Rustls only | Keep; retain response limits | Direct | Federation response could be unbounded | Yes, configured gateway | Medium | **Mitigated** with declared/streamed limits and timeout |
| `argon2` | 0.5.3 | Keep unless a planned API migration justifies 0.6 | Direct | Stored PHC could select excessive cost | Database-controlled auth state | Low | **Mitigated** with pre-verification parameter caps |
| `rsa` | 0.9.10 | No fixed version; monitor Arti/RustCrypto | Transitive via Arti | RUSTSEC-2023-0071 Marvin timing attack | Vulnerable private decrypt not found | Low | Dated exception; re-audit on updates or new RSA-decrypt use |
| `paste` | 1.0.15 | Remove when Arti upstream does | Transitive proc macro | RUSTSEC-2024-0436 unmaintained | Build-time only; no known exploit | Low/maintenance | Dated exception; monitor upstream |
| `bincode` | 2.0.1 lock-only | Allow upstream to remove; do not activate casually | Optional transitive lock entry | RUSTSEC-2025-0141 unmaintained | No, absent all-target/all-feature tree | Informational | Documented inactive; monitor graph changes |
| `rustls-webpki` | 0.103.14 | 0.103.15 during routine maintenance, not emergency | Direct | Newer patch exists; audited version is beyond applicable RUSTSEC-2026-0049 fix threshold | Yes, TLS validation | None found | No security-motivated churn; retain verification defaults |
| `libsqlite3-sys` | 0.37.0 bundled | Follow compatible `rusqlite`/Arti resolution | Transitive native | Large native SQL surface; no applicable advisory found | Yes, all database input | Informational | Parameterization/transactions reviewed; patch via dependency updates |
| `ring` | 0.17.14 | Keep patched | Transitive native/unsafe | Security-critical crypto/native code | Yes, TLS/Tor/certificates | None found | Single version; no applicable advisory |
| `zstd-sys` | 2.0.16+zstd.1.5.7 | Keep patched | Transitive native | Native decompressor/compressor in Tor and HTTP-compression paths | Network data reaches it when zstd is selected | Informational | Single version; retain size/time boundaries in callers |

External tools and GitHub Actions are addressed under F-09, F-11, and the supply-chain section because they are not Cargo dependencies.

## Update Analysis

Only two dependency upgrades were security-required, and both were patch-level lockfile changes:

| Dependency | Audited -> recommended | Why | Compatibility / migration | Old copy remains? |
|---|---|---|---|---|
| `h2` | 0.4.15 -> 0.4.16 | Fix RUSTSEC-2026-0258 in the public HTTP/2 stack | Semver-compatible patch update. RustChan has no direct `h2` API calls, so no source migration or expected behavior break was required. Full server tests passed. | No; target/all-feature reverse checks find only 0.4.16. |
| `chacha20` | 0.10.1 -> 0.10.2 | Replace the yanked, unsound x86 SIMD implementation | Semver-compatible patch update below upstream `rand`/Arti. RustChan has no direct `chacha20` API calls, so no source migration was required. Full tests passed on the resolved graph. | No; target/all-feature reverse checks find only 0.10.2. |

Other update observations were deliberately not treated as vulnerabilities:

- `argon2` 0.5.3 -> 0.6.0 is a semver-major API transition. A planned migration would need to re-review PHC parsing/verifier APIs and stored-hash compatibility. The demonstrated risk was safely fixed by pre-verification cost caps; no applicable 0.5.3 advisory warrants major churn.
- `rustls-webpki` 0.103.14 -> 0.103.15 is a patch update and can be taken during routine maintenance. The audited version is already past the applicable RUSTSEC-2026-0049 fix threshold, so it is not an emergency security remediation.
- `uuid` 1.24.1 -> 1.26.0 is within major version 1 and expected to be source-compatible, but no advisory/correctness defect relevant to RustChan was identified.
- `rsa` 0.9.10 has no fixed release for RUSTSEC-2023-0071. Its currently unreachable private-decryption operation is documented and monitored rather than papered over with a nonexistent upgrade.
- Updating direct `rusqlite` to 0.40.x is currently incompatible with Arti's `< 0.40` constraint because both versions would link different `libsqlite3-sys` families under the unique `sqlite3` native-link name. That is an upstream coordination task, not a safe isolated upgrade.

## Unreachable Advisories and Non-Applicable Warnings

### `rsa` 0.9.10 — RUSTSEC-2023-0071

This is a real known vulnerability present in the activated graph, but presence is not reachability. The Marvin attack needs repeated, observable private RSA PKCS#1 v1.5 decryption behavior. RustChan has no direct `rsa` use. Inspection of reverse paths and Arti-related call sites found RSA public verification and generic key support, not an HTTP/ChanNet/Tor path that feeds attacker ciphertext into the affected private-decryption API. RustChan's onion service is v3 and no RSA-decryption oracle was identified. Classification: **present but currently unreachable**, not false positive. Confidence is not absolute because upstream feature behavior can change, so the exception is dated rather than permanent.

### `bincode` 2.0.1 — RUSTSEC-2025-0141

The unmaintained warning appears because `bincode` is an optional dependency of `typed-index-collections` retained in the lockfile. `cargo tree --target all --all-features -i bincode@2.0.1` produces no activated path. No RustChan input is deserialized with it. Classification: **lockfile-only / currently unreachable**. It remains worth watching because future upstream feature changes could activate it.

### `paste` 1.0.15 — RUSTSEC-2024-0436

`paste` is activated as a procedural macro through Arti-related crates and therefore executes on the build host. The advisory is an unmaintained notice, not a reported memory-safety or malicious-code vulnerability, and its expansion is not linked into runtime. Classification: **build-time only maintenance risk**. It is intentionally documented in `deny.toml` pending upstream removal.

### Other advisory/version checks ruled out

- The snapshot ZIP version is 8.6.0 and is not in the affected `< 2.3.0` range for RUSTSEC-2025-0168. RustChan does not use a generic extract-to-filesystem API for ChanNet snapshots.
- `rustls-webpki` 0.103.14 is newer than the 0.103.10 fixed threshold for RUSTSEC-2026-0049. [Upstream release history](https://github.com/rustls/webpki/releases) and [security advisories](https://github.com/rustls/webpki/security) were checked.
- Neither `h2` 0.4.15 nor `chacha20` 0.10.1 remains anywhere in the lockfile or activated duplicate graph.
- No OpenSSL, `openssl-sys`, `native-tls`, `aws-lc-rs`, or `aws-lc-sys` package is activated for any target/all-feature graph. `openssl-probe` is a certificate-location helper and is not OpenSSL linkage.

## Feature-Flag and Attack-Surface Audit

Security-relevant direct feature resolution after remediation:

| Package | Enabled configuration | Assessment |
|---|---|---|
| `arti-client` 0.45.0 | Defaults off; `tokio`, `rustls`, `onion-service-service`, `flowctl-cc` | Deliberate Tor/onion-service surface; avoids default backend expansion |
| `axum` 0.8.9 | `multipart` | Required for public posting; bounded by RustChan middleware/parser budgets |
| `axum-server` 0.8.0 | `tls-rustls-no-provider` | Avoids implicit provider choice; Rustls provider is selected deliberately |
| `captcha` 1.0.0 | Defaults off | Its old `image` 0.24.9 / `png` 0.17.16 path generates CAPTCHAs; it does not decode attacker uploads |
| `hyper` 1.11.0 | `http1`, `http2`, `server` | Both public protocols are intentional; patched `h2` is enforced by lockfile |
| `image` 0.25.10 | Defaults off; `jpeg`, `png`, `gif`, `webp`, `bmp`, `tiff`, `ico` | Broad upload compatibility is intentional; unsafe/resource risk controlled by byte/pixel/allocation/concurrency caps |
| `reqwest` 0.13.4 | Defaults off; `multipart`, `json`, `rustls-no-provider` | No native TLS; used for bounded federation calls |
| `rusqlite` 0.39.0 | `bundled`, `backup` | Bundled SQLite improves version consistency but compiles native C; backup is an intentional admin feature |
| `rustls` 0.23.43 | Defaults off; `logging`, `ring`, `std`, `tls12` | One explicit crypto provider; TLS 1.2 retained for compatibility, with no verification bypass found |
| `rustls-webpki` 0.103.14 | Defaults off | Explicit validation library; no dangerous feature found |
| `tokio-rustls` 0.26.4 | Defaults off; `logging`, `ring`, `tls12` | Matches server/client Rustls policy |
| `tower-http` 0.7.0 | `fs`, `set-header`, `compression-full`, `limit`, `trace` | `limit` added. `compression-full` activates more codecs/native zstd surface, but it is used for response compression and no safe removal was proven in this audit. |
| `zip` 8.6.0 | Defaults off; `deflate-flate2-zlib-rs` | Narrowed from generic `deflate`, removing unnecessary `zopfli`; no encryption or broad codec defaults |

Two duplicate media stacks remain for distinct functions: `image` 0.25.10/`png` 0.18.1 handles uploaded images, while `captcha` with defaults disabled pulls `image` 0.24.9/`png` 0.17.16 only for generated CAPTCHA output. There is no attacker-image decode path through the older pair, so a forced major CAPTCHA migration was not justified.

The all-feature delta is almost entirely optional ACME support. It creates `rcgen` 0.13.2 beside default self-signed `rcgen` 0.14.9 and two WebPKI root versions. These are upstream API constraints, not evidence that two TLS stacks validate the same connection inconsistently. All paths use Rustls; no native TLS alternative is enabled.

## Unsafe and Native-Code Exposure

Cargo metadata identified 52 proc-macro packages, 82 packages with a custom build target, and six raw `links` packages across the lockfile. Activated release graphs are smaller: 42–45 proc macros, 44–49 build scripts, and exactly three native-link packages on each release target:

- `libsqlite3-sys` 0.37.0 -> `sqlite3`
- `ring` 0.17.14 -> `ring_core_0_17_14_`
- `zstd-sys` 2.0.16+zstd.1.5.7 -> `zstd`

The all-target analysis additionally activates wasm-only `sqlite-wasm-rs` and `wasm-bindgen-shared`; neither ships in the four native release artifacts. The raw `defmt` link declaration is likewise not a native release link.

Risk was ranked by untrusted-data reachability rather than unsafe-code count:

| Surface | Untrusted data path | Result |
|---|---|---|
| Image codecs | Anonymous upload -> pure-Rust codec/parser and optimized routines | High resource risk remediated with decoder/pixel/concurrency budgets; no applicable memory-safety advisory found |
| `chacha20` SIMD | Arti/Tor runtime -> unsafe x86 backend | Affected 0.10.1 removed; only 0.10.2 remains |
| SQLite C | All HTTP/federation state -> parameterized Rusqlite -> bundled SQLite | Native high-value surface; no applicable advisory or injection path found; import writes made atomic |
| `ring` | Public TLS and Tor crypto -> native/assembly crypto | Single patched version, no applicable advisory; inputs reach verification but no insecure configuration found |
| zstd C | HTTP compression/Tor directory data -> `zstd-sys` | Network data can reach native decompression; package is single-version and patched, with higher-level request/response bounds |
| External FFmpeg/Poppler/MuPDF/Quick Look | Anonymous hostile media -> separate native process | Isolation is process-level rather than memory-safe; timeouts/output/process-group cleanup added; patching remains operator responsibility |

Targeted active build-script inspection and source searches found ordinary code generation, compiler/linker discovery, and bundled native compilation. No inspected active dependency build script downloaded a binary, contacted the network, wrote outside Cargo build/output locations, or executed a repository-controlled shell command based on untrusted runtime data. This statement is evidence-limited: Cargo build scripts inherently execute with build-user privileges, environment variables and compiler/toolchain paths are trusted build inputs, and a compromised registry package would still execute before runtime controls apply.

## Duplicate Dependency Analysis

`cargo tree -d` reports 45 duplicate names on the host all-feature graph and 57 on the all-target/all-feature graph. Normalizing by package ID/version (to exclude resolver/build-context repeats at the same version) gives:

- Host all features: 39 names, 88 package/version IDs, 49 extra versions.
- All targets/all features: 51 names, 114 package/version IDs, 63 extra versions.

Security-relevant splits were individually traced:

- `image` 0.24.9 vs 0.25.10 and `png` 0.17.16 vs 0.18.1: generated CAPTCHA versus attacker-upload decode, as explained above.
- `tower-http` 0.6.11 vs 0.7.0: Reqwest internal support versus RustChan's direct server middleware.
- `rcgen` 0.13.2 vs 0.14.9 and WebPKI-root splits: optional ACME versus self-signed/default certificate paths.
- Crypto/API-family splits such as `cipher`, `digest`, `sha2`, `sha3`, `der`, `spki`, `rand`, `rand_chacha`, and `rand_core` are constrained by Arti and other upstream APIs. The vulnerable `chacha20` itself is single-version and patched.

No old parallel copy remains for `h2`, `chacha20`, `multer`, `zip`, `rustls`, `rustls-webpki`, `ring`, `rsa`, `argon2`, `rusqlite`, or `libsqlite3-sys`. Deduplicating the remaining graph would reduce binary/build surface, but no safe direct constraint was found that materially improves security without forcing upstream API migrations. The audit therefore did not churn dependencies for aesthetic deduplication.

## Dependency-Configuration and Attack-Path Review

The following table records high-impact classes that were investigated but did not become findings. “Not found” is scoped to the audited code and graph; it is not a proof about all future code or all upstream implementation bugs.

| Threat class | Entry points and dependency API reviewed | Outcome / evidence |
|---|---|---|
| TLS verification bypass | Reqwest/Rustls clients, Axum server TLS, ACME/self-signed paths | No `danger_accept_invalid_certs`, custom permissive verifier, or native-TLS fallback found. Trust roots and Rustls verification remain enabled. |
| Weak randomness/token generation | Session, deletion, CSRF, salts, IDs; OS RNG wrappers and Arti RNG graph | RustChan uses OS randomness and fails closed on entropy failure. No deterministic/test RNG was found in production token generation. Patched `chacha20` removes the identified unsafe RNG backend. |
| Weak password hashing | Argon2 hash and verify configuration | New hashes use Argon2id with 64 MiB, time 2, parallelism 2. Stored-hash verification now caps parameters before work. |
| SQL injection | Posting, authentication, moderation, ChanNet import/reply, search, and backup/restore Rusqlite call sites | Security-sensitive values use bound parameters. Dynamic SQL observed in audited paths constructs fixed clauses or validated internal identifiers; no untrusted raw SQL interpolation was established. |
| Command/shell injection | FFmpeg/FFprobe/PDF renderer and probe construction | Commands use explicit executable plus argv, not a shell. Untrusted media is passed as a filesystem argument after staging/validation. No attacker-controlled executable name or shell fragment was found. |
| Archive path traversal | ChanNet ZIP ingestion and component lookup | Snapshot import accepts exactly three unique names and reads them into bounded memory; it does not extract archive paths to disk. Path traversal is therefore not applicable to this path. |
| Decompression/parser bombs | ZIP, JSON, images, media probes, HTTP compression | Concrete ZIP/image/multipart/response gaps became F-01/F-03/F-05/F-06 and were fixed. External parser risk remains F-11. No unbounded unsafe deserializer such as active Bincode was found. |
| SSRF / unsafe redirects | ChanNet/RustWave Reqwest calls and configurable URLs | Gateway destinations come from server configuration rather than an anonymous request parameter. No per-request arbitrary URL fetch was found. A malicious configured gateway remains a trusted-boundary risk and its body is now bounded. |
| Header injection/request smuggling | Axum/Hyper server, response headers, user strings | Header values are parsed through typed HTTP APIs; server header sizes are bounded. No raw user-controlled CRLF header construction or conflicting manual transfer framing was found. Patched `h2` removes the known protocol DoS. |
| XSS/parser sanitizer bypass | Post formatting/sanitization and templates | No applicable dependency advisory or new bypass was found. RustChan retains bounded body sanitization and template escaping. This was source review, not browser-fuzzer proof for every HTML grammar edge. |
| Cookie/session compromise | Axum-extra cookies and RustChan session/CSRF helpers | Cryptographic tokens use OS randomness/HMAC-style signing paths and scoped CSRF checks. No dependency-level cookie parser advisory or predictable session ID was found. Deployment-dependent `Secure` behavior still depends on correct TLS configuration. |
| Filesystem traversal/symlink/tempfile attacks | Upload staging, validated storage handoff, board media paths, worker output installation | Board/path components are validated; temporary uploads use `tempfile`; finalization validates managed paths. `ValidatedUpload` prevents accidental duplicate validation but does not claim to defeat an attacker with local filesystem write privileges. |
| Unsafe integer conversion | ChanNet remote IDs/timestamps/counts | Newly checked before SQLite conversion; over-range inputs are rejected. Other Rust conversions reviewed in the touched paths remain bounded or fallible. |
| Regex DoS | Search/sanitization/formatting regex use | No attacker-selected regex compilation was found; patterns are static and text inputs are already bounded. No applicable regex advisory was present. |
| Authentication/authorization bypass | Public posting, ChanNet API key, admin password, moderation boundaries | No dependency-caused bypass was found. ChanNet heavy work remains behind API-key authentication; upload controls do not bypass posting authorization/CSRF logic. |

## Supply-Chain Findings

### Cargo sources and dependency confusion

All 661 sourced lockfile packages use `registry+https://github.com/rust-lang/crates.io-index` and have checksums. RustChan is the sole source-less workspace package. There are no Git dependencies, unpinned revisions, alternate registries, external path dependencies, source replacements, `[patch]`, or `[replace]` sections. No suspicious near-name/typosquat package was identified from the manifest/lock inventory.

`deny.toml` now evaluates all features, denies yanked crates, denies unknown registries and Git sources, and allows only crates.io. CI and release use the committed lockfile and `--locked` for dependency-resolving checks/builds. This substantially reduces dependency-confusion and unexpected-source risk. It does not protect against a compromised crates.io account/package version already approved into a future lockfile, a compromised build host, or malicious code in a checksum-pinned version.

### Build scripts, proc macros, generated/native code

Build scripts and proc macros execute before RustChan's runtime sandboxing and therefore remain privileged supply-chain inputs. The activated set is large mainly because of Arti, serialization derivations, TLS/crypto, and bundled native libraries. `paste` is specifically documented as an unmaintained build-time proc macro. No dependency-controlled precompiled executable or network-fetching build behavior was found in the active release paths. `libsqlite3-sys`, `ring`, and `zstd-sys` compile or link bundled/native code, making compiler, linker, environment, and host toolchain integrity part of the trust boundary.

### GitHub Actions and release tooling

All 25 `uses:` references in the three tracked workflows are full 40-character commit SHAs and were resolved against their upstream repositories. Human-readable version comments are non-binding. `cargo-audit` 0.22.1, `cargo-deny` 0.19.0, `cross` 0.2.5, and Rust 1.91.0 are exact where used for CI/release validation; install-action fallback is disabled. The audit workflow runs on dependency-related pull requests/pushes, merge queues, a weekly schedule, and manual dispatch.

Residual reproducibility limits are documented in F-09 and F-12. In particular, the audit job uses a moving `stable` Rust toolchain, GitHub-hosted runner labels move over time, `cargo install cross` still trusts crates.io and its dependency lock, and default cross container images are not digest-pinned. Release artifacts are deterministic enough to verify within one workflow using SHA-256, but this audit did not establish bit-for-bit reproducible builds across hosts.

## Attack-Surface Reduction

Implemented reductions:

- Narrowed ZIP from generic `deflate` to `deflate-flate2-zlib-rs`, removing `zopfli` 0.8.3 and its unnecessary compression implementation from the graph.
- Enabled only the existing `tower-http` dependency's `limit` feature; no new crate was added.
- Kept `image`, `captcha`, Reqwest, Rustls, Tokio-Rustls, Arti, and ZIP default features disabled where already intentional.
- Added application budgets rather than introducing replacement parsers or broad dependency upgrades.
- Removed affected `h2` and `chacha20` versions rather than layering only partial mitigations.

Considered but not changed:

- Removing image formats would materially change board behavior and was not shown necessary after explicit decoder limits.
- Removing HTTP/2 would reduce protocol surface but is unnecessary after the patched dependency and would change public compatibility.
- Replacing `captcha` solely to deduplicate `image`/`png` would be unrelated major churn; its old image stack does not decode attacker files.
- Removing Arti would eliminate RSA/paste and much of the duplicate/build-time graph, but would remove an intentional onion-service feature and is not proportionate to the currently unreachable RSA operation.
- Disabling `compression-full` may reduce codec/native surface, but actual response-encoding compatibility was not characterized sufficiently to call it safe. A future focused performance/compatibility change could narrow it.

## Changes Made

No new runtime dependency was added. The tracked patch changes these 28 existing files plus this report:

| Area | Files | Exact security change |
|---|---|---|
| Dependency graph/policy | `Cargo.toml`, `Cargo.lock`, `deny.toml` | `h2` 0.4.15 -> 0.4.16; `chacha20` 0.10.1 -> 0.10.2; remove `zopfli` through ZIP feature narrowing; enable Tower HTTP limit; all-feature/yanked/source policy and dated exceptions |
| CI/release | `.github/workflows/audit.yml`, `.github/workflows/ci.yml`, `.github/workflows/release.yml` | Immutable action SHAs, exact security tools/cross/toolchain, required-quality dependency policy, broader audit triggers, release validation/tag-version gate |
| Multipart/posting | `src/handlers/mod.rs`, `src/handlers/board.rs`, `src/handlers/board/create_thread.rs`, `src/handlers/thread.rs`, `src/server/server/routes.rs`, `src/test_support.rs` | Request/envelope/field/aggregate/time limits, early media gate, guard handoff, route/parser regressions |
| Image/media storage | `src/media/mod.rs`, `src/media/process.rs`, `src/media/thumbnail.rs`, `src/utils/files.rs`, `src/utils/files/storage.rs`, `src/workers/mod.rs` | Decoder/pixel caps, opaque validated upload, bounded processor pipes, timeout/cancellation/process-group cleanup |
| Shared state/server | `src/middleware/mod.rs`, `src/middleware/state.rs`, `src/server/server.rs` | Media and ChanNet concurrency gates, application wiring, cancellation-safe guards |
| ChanNet | `src/chan_net/import.rs`, `src/chan_net/mod.rs`, `src/chan_net/poll.rs`, `src/chan_net/refresh.rs`, `src/chan_net/snapshot.rs`, `src/db/chan_net.rs` | Bounded snapshot/response parsing, validated domains, one atomic import transaction, replay behavior, concurrency and regression coverage |
| Authentication | `src/utils/crypto.rs` | Validate and cap stored Argon2 parameters before verification |
| Report | `docs/dependency-supply-chain-security-audit-2026-08-27.md` | Evidence, classification, remediation, validation, and residual-risk record |

The ignored local `.cargo/audit.toml` used during the audit was also synchronized with the dated RSA reachability rationale, but `.gitignore` excludes it and it is **not** part of the repository patch. CI supplies the explicit RSA exception on its command line and `deny.toml` contains the tracked policy.

## Validation

Final validation is recorded below after all source and report edits. Commands are listed exactly enough to distinguish configured policy passes from diagnostic failures.

Tool versions used were Rust/Cargo 1.97.1 on the audit host, the repository's CI toolchain Rust/Cargo 1.91.0, `cargo-audit` 0.22.1, `cargo-deny` 0.19.0, `cargo-outdated` 0.17.0, and `actionlint` 1.7.12.

### Rust build, formatting, linting, and tests

| Command | Result |
|---|---|
| `cargo fmt --all --check` | **Pass** (exit 0). |
| `cargo check --workspace --all-targets --all-features` | **Pass** (exit 0). |
| `cargo build --locked --workspace --all-features` | **Pass** (exit 0). |
| `cargo +1.91.0 clippy --locked --workspace --all-targets --all-features` | **Pass** (exit 0), matching the repository's CI Clippy policy/toolchain. |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | **Does not pass** (exit 101): the requested stricter command reports 395 `error:` headings across the repository-wide lint backlog. The first diagnostic is an untouched long documentation paragraph in `src/templates/mod.rs`; the dominant class is existing `redundant_pub_crate` visibility. A location comparison against every added Rust line in `git diff --unified=0` found **zero diagnostics on added lines**. This broad lint cleanup was intentionally not mixed into the security patch. The complete final log is local at `/tmp/rustchan-final-strict-clippy.log`. |
| `cargo test --workspace --all-features` | The final unchanged rerun **passed**: 1,104 library tests, 1,104 binary tests, 4 integration tests, and 1 doctest (2,213 total). The immediately preceding first invocation had one order-sensitive binary-target failure, `admin_login_rejects_raw_readable_csrf_cookie_without_signed_form_token` (1,103 other binary tests passed); the failing test then passed in isolation in both library and binary targets, and the exact full command passed on rerun. This transient result is recorded rather than concealed. |
| `cargo +1.91.0 test --locked --workspace --all-features -- --quiet` | **Pass**: the same 1,104 + 1,104 + 4 + 1 test counts on the CI toolchain and locked graph. |

Focused regression runs also passed for the new security boundaries. These include multipart envelope/request limits and media-gate lifetime, 40-megapixel/decoder allocation limits, synchronous/asynchronous bounded subprocess output and process-tree cleanup, snapshot decompression/object/schema limits, ChanNet import-gate contention, atomic rollback/retry/replay, bounded gateway responses, and excessive Argon2 PHC cost. The final atomic snapshot test passed independently in both unit-test targets before the full suites.

### Dependency and advisory tooling

| Command | Result |
|---|---|
| `cargo fetch --locked` | **Pass**; the committed lockfile resolves without mutation. |
| `cargo audit --no-fetch --deny unsound --deny yanked --file Cargo.lock --ignore RUSTSEC-2023-0071` | **Pass** (exit 0), scanning 662 packages against 1,226 advisories. It reports two allowed maintenance warnings: inactive `bincode` and build-time `paste`. The only ignored vulnerability is the separately justified, currently unreachable RSA operation. |
| `cargo audit --no-fetch --file /Users/connordawkins/Documents/GitHub/RustChan/Cargo.lock` from `/tmp` with no repository config/ignore | **Expected diagnostic failure** (exit 1): exactly one vulnerability, `rsa` 0.9.10 / RUSTSEC-2023-0071, plus the same two maintenance warnings. This proves the exception is not hiding another advisory. |
| `cargo deny --locked check` | **Pass**: `advisories ok, bans ok, licenses ok, sources ok`. It emits policy warnings for `captcha`'s missing manifest license field and reviewed duplicates, but no denied advisory, yanked crate, license, or source. |
| Reverse `cargo tree --locked --target all --all-features` checks | **Pass**: `h2@0.4.15`, `chacha20@0.10.1`, and `zopfli` are absent; reverse paths for `h2@0.4.16` and `chacha20@0.10.2` are present. |
| `cargo tree -d`, target/feature `cargo tree`, and `cargo metadata` inventories | **Pass**; produced the package/proc-macro/build-script/native-link and duplicate counts in this report for host, all release targets, and the all-target graph. |
| `cargo outdated --workspace --depth 1 --exclude rusqlite --exclude r2d2_sqlite` | **Pass**; reports `argon2` 0.6.0, `rustls-webpki` 0.103.15, and `uuid` 1.26.0 as newer, with no demonstrated security reason for blind upgrade. |
| Unfiltered `cargo outdated --workspace --depth 1` | **Tooling-resolution failure** (exit 1): its temporary independent resolution selects direct `rusqlite` 0.40.2/`libsqlite3-sys` 0.38.2 while Arti requires `rusqlite < 0.40` and its SQLite link family, so Cargo rejects two `links = "sqlite3"` packages. The committed locked graph itself builds and tests successfully. |

### Workflow and patch integrity

| Check | Result |
|---|---|
| `actionlint .github/workflows/audit.yml .github/workflows/ci.yml .github/workflows/release.yml` | **Pass** with `actionlint` 1.7.12. |
| Ruby YAML parsing of the same three tracked workflows | **Pass**. |
| Action-reference shape check | **Pass**: all 25 tracked `uses:` references end in a 40-character lowercase hexadecimal SHA. Their refs were also checked against upstream repositories during the audit. |
| `git diff --check` | **Pass**; no whitespace errors. |

The tracked workflows were statically validated but were not dispatched on GitHub during this local audit. The four release-target dependency graphs were resolved, but full cross-platform release binaries were not built locally; CI/release remains the authoritative execution environment for those artifacts.

## Residual Risk and Audit Limitations

- No finding was labeled confirmed exploitable because controlled tests proved the new security boundaries and failure behavior, not a complete pre-fix weaponized exploit. The three anonymous paths have sufficiently direct reachability to be classified likely exploitable availability issues.
- External processor safety depends on operating-system process semantics and installed binary provenance. Windows cannot use Unix process groups in the same way; direct-child timeout/reaping still applies, but descendant containment should be provided by service/job-object/container policy where needed.
- The 512 MiB aggregate public multipart allowance is intentionally high to preserve configured media behavior. It is finite and serialized at the expensive media phase, but operators should set reverse-proxy connection/body/rate limits appropriate to their capacity.
- The media/import gates are per RustChan process. A multi-process deployment needs an external/global capacity control if all instances share constrained CPU, memory, storage, or SQLite state.
- A local attacker able to replace temporary/upload files, alter the database, change `PATH`, or write the service configuration is outside the remote web threat model and can still affect parsers and authentication state. RustChan now validates more of that state but is not a host sandbox.
- GitHub settings are a dated snapshot and can change without a source diff. Branch protection requires build checks but lacks approval/CODEOWNERS requirements; administrator policy changes remain outstanding.
- `cargo outdated` could not produce one unfiltered graph because its independent resolver selected `rusqlite` 0.40.2/`libsqlite3-sys` 0.38.2, conflicting with Arti's `rusqlite < 0.40` native `links = "sqlite3"` constraint. A filtered direct-dependency run succeeded. This is a tooling-resolution limitation, not a failure of the committed locked graph.
- The audit reviewed active features and all release targets plus an all-target graph. It did not fuzz every upstream codec/protocol implementation, prove bit-reproducible artifacts, audit every line of transitive unsafe code, or independently verify every crate maintainer identity.

## Conclusion

The post-remediation Cargo graph contains no known reachable RustSec vulnerability or yanked package under the configured policy. The most exposed dependency-security risks—anonymous image/multipart resource exhaustion and the affected HTTP/2 stack—are bounded or patched, and authenticated federation/archive work is now bounded, serialized, and failure-atomic. The remaining accepted RustSec vulnerability is the documented, currently unreachable RSA private-decryption flaw; the remaining substantive risks are release provenance, repository governance, external executable patching, and ordinary residual parser/host compromise risk.
