# RustChan hostile-input security audit — 2026-10-06

Defensive source review, implementation fixes, deterministic regressions, and local HTTP/browser/load validation. Testing used an isolated data directory and a listener bound to `127.0.0.1`; no production or third-party system was targeted. No commits or branches were created. No new crate was added; `hooks` and `blob` features were enabled on the existing rusqlite dependency.

## 1. Attack surface mapped

[The pre-change inventory](security-attack-surface.md) maps 122 method/path entries, their shared headers, permissions, CSRF, input/body limits, database/filesystem work, output contexts, and expensive-operation profiles. It includes posting, replies, reports, appeals, search, archive/catalog, previews/polling, ownership actions, admin authentication/settings/assets/accounts/moderation, setup, backups/restores, maintenance, update/restart, media, telemetry, embedded assets, and the HTTPS redirect listener. No alternate JSON posting, RSS, legacy posting, or separate anonymous upload route was found.

The final review followed shared helpers back through every caller, including both public upload routes, all five admin asset routes, full/saved/board restore paths, post previews/search/archive/admin rendering, password reauthentication, and plaintext/static TLS/ACME listener implementations.

## 2. Confirmed vulnerabilities and weaknesses

No CRITICAL or HIGH finding was confirmed. Severity reflects the demonstrated boundary and existing defenses, not a hypothetical worst-case chain.

| ID / severity | Boundary, consequence and cause | Correction | Regression evidence |
|---|---|---|---|
| F1 MEDIUM | Behind a trusted appending proxy, attacker-supplied leftmost X-Forwarded-For or preferred X-Real-IP could control identity. This undermined rate limits, bans and attribution. | Walk XFF from the trusted peer toward the first untrusted hop; authoritative malformed/duplicate XFF falls back to the peer rather than another forwarding header. | Spoofed-prefix, malformed-hop, IPv6 and direct-client tests; actual direct requests with rotating XFF still hit the same posting budget. |
| F2 MEDIUM | Markup transformations ran after hyperlink generation and could put generated tags/quotes inside URL attributes. Cached post HTML could retain the malformed markup. CSP limits script execution but does not make malformed HTML safe. | Apply formatting before generating URL attributes; escape affected cached content as bounded raw text while preserving valid cached formatting/dice rolls. | URL/markup and legacy-cache regressions; real Chromium found zero injected handlers, malicious image nodes or broken URL attributes. |
| F3 MEDIUM | Browsing/authentication/board-password counters and CAPTCHA challenges admitted arbitrary identities without a hard retained-state ceiling. Expiration alone did not bound burst memory. | Fixed-cap atomic rate tables; fixed-size hashed production keys, amortized expiry and fail-closed saturation. CAPTCHA capacity includes generation reservations. | Exact window boundaries, 16,384-entry saturation/recovery, 64 simultaneous reservations, CAPTCHA cap and reservation cleanup. |
| F4 MEDIUM | Concurrent failures could all pass password failure checks before expensive verification; CAPTCHA work could block an async worker. Expensive operations lacked independent admission bounds across entry points. | Reserve attempts before verification; nonqueueing two-worker password/CAPTCHA gates; move work to blocking workers and retain permits inside them. Include setup, unlock, account/secret/update reauthentication and board password creation. | Existing auth/setup/unlock tests plus gate sharing/release and cancelled-request blocking-work regression. |
| F5 MEDIUM | Small search result pages did not bound FTS count, sorting, deep OFFSET or parallel work. Broad prefix queries could process the entire matching corpus. | Query/term/page/result/count bounds, SQLite VM/time progress budgets, two concurrent searches and independent request budget. | Real search burst, query/pagination regressions, interruption/reusable-connection test and existing FTS plan/result tests. |
| F6 MEDIUM | Thread rendering collected every reply; catalog/hidden listings requested unlimited rows. A stored large thread amplified repeated read memory, rendering and response cost. | OP plus 200 replies per indexed cursor page; 1,000-row catalog/hidden/preference cap matching the allowed active-thread maximum. | 450-reply traversal and EXPLAIN assertion; full-router history/quote/maximum-ID tests; real no-JS traversal of 464 replies. |
| F7 MEDIUM | Admin favicon/banner handlers read multipart fields/files before checking the session and did not share public media admission. Unauthenticated parallel bodies could impose allocation/parser cost. | Authenticate before body consumption, bound part names/counts/text/unknown fields, reject duplicates, apply envelope guard and share media admission through processing. | Full-router stalled infinite-body rejection before reads; text limit−1/limit/limit+1, duplicate-part and permit-release tests. |
| F8 MEDIUM | Full restore materialized all post bodies from an up-to-8-GiB snapshot. Merely slicing SQLite TEXT also misses content after embedded NUL bytes. | Page 128 IDs/types at a time; incremental SQLite value reads check byte length before allocation, validate UTF-8 and enforce 4,096 Rust scalars before staged publication; board manifest bodies use the same scalar limit before filesystem work. | 300-row sanitization; 4,095/4,096/4,097 emoji/NUL boundaries, empty body, 65,536-byte NUL-prefixed body and invalid UTF-8. Board restore rejects oversize before extraction/staging/database mutation. Existing rollback/recovery tests remain. |
| F9 MEDIUM | Application middleware only runs after an HTTP head is parsed; native listeners lacked bounded open-socket admission and header-read deadlines. | 256 sockets per listener, transport admission before handshake, 15-second handshake/head limits; HTTP/2 stream/header caps. Request admission remains held through response streaming. | Acceptor saturation/release and streamed-response regressions; actual stalled head closed after 15.003 seconds while health reads succeeded. |
| F10 LOW | Internal return-path validation allowed controls/backslashes elsewhere in a leading-slash path, enabling browser normalization ambiguity or invalid redirect headers. | Reject controls, any backslash, protocol-relative paths and values over 8 KiB in the shared validator. | Internal/external/protocol-relative/control/encoded-query regressions; existing handler redirect tests. |
| F11 LOW | Anonymous upload errors and restore XHR 500 responses could include tool diagnostics or filesystem/internal details. Request tracing retained complete query strings. | Generic public internal/decoder errors with classified 413/415 behavior preserved; bounded path-only tracing and bounded restore header diagnostics. | Error-classification/privacy tests and actual corrupt-upload rejection. |
| F12 LOW | Missing login usernames skipped password verification, providing a cheap account-existence timing signal despite generic errors. | Verify a public dummy hash at current account parameters within the same rate/concurrency gate; a missing user can never yield an account ID. | Dummy matching/nonmatching password regression and parameter parity check; existing successful/failed login tests. |

Additional HARDENING: 64-KiB default forms, early declared-body rejection, 8-KiB URI limit, Host ambiguity rejection, early mutation origin/fetch checks, public signed-token cookie binding, and bounded startup browsing settings. Local testing also caught reserved form routes inheriting upload body/timeout allowances; the shared classifier and full-router regression now exclude those routes. The second pass corrected maximum SQLite post-ID focus so the newest reply remains reachable without cursor overflow.

## 3. Request-size protections

| Input | Final bound / enforcement |
|---|---|
| URI | 8,192 bytes before query parsing/logging; boundary tests at 8,191/8,192/8,193. |
| Headers | Existing 64-KiB aggregate/32-KiB value service bounds, 64-KiB native protocol bounds; duplicate Host and ambiguous transfer framing rejected. |
| Ordinary forms | 65,536 streaming bytes; early Content-Length rejection; 65,535/65,536/65,537 full-router tests. |
| Public posting | Existing 512-MiB aggregate payload / 516-MiB request envelope, board-specific media limits, 64 fields, 64-KiB text/unknown fields, bounded multipart headers/preamble/boundary. |
| Admin assets | Existing favicon 5-MiB/banner 8-MiB body caps, now early header enforcement plus shared parser/admission bounds. |
| Restore | 8-GiB actual upload/expanded archive budgets and existing staged validation; route extractor allowance cannot bypass actual streaming limits. |
| Text | Post body 4,096 Unicode scalars, subject 128, search 256, report 256, appeal 512; usernames/passwords and config fields retain backend validators. Login password additionally bounded at 1,024 bytes before verification. |

Limits are enforced in Rust, including no-JS forms. Body declarations never choose upload policy; route/method classification does. Existing media-specific and multipart limits remain authoritative inside the larger envelope.

## 4. Multipart hardening

The existing streaming public parser already bounds aggregate bytes, field count, duplicate upload/scalar slots, unknown-field discard, header/preamble bytes, rejection recovery, and a shared body deadline. It uses RAII temporary files and retains a bounded recoverable draft. These controls were preserved.

All admin favicon/banner paths now authenticate before iteration, reject duplicate names including unknown names, bound names to 128 bytes and total parts to 64, read scalar UTF-8 incrementally within 64 KiB, discard unknown fields within existing bounds, and use the existing envelope scanner. A permit covers parsing through blocking image publication. Session authorization is checked again before mutation.

Actual HTTP cases covered missing boundary, truncation, duplicate body, a 65,537-byte unknown field and invalid raw UTF-8. Existing automated cases cover split envelopes, malformed/oversized preambles/headers, quoted boundaries, duplicate file slots, zero-byte/unselected controls, board-specific exact limits and rejected-file cleanup.

## 5. Rate-limiting architecture

Independent fixed-window per-client budgets are charged before parsing or DB work, across all boards/threads: thread creation 6/minute; replies 30; report/appeal requests 30; searches 20; password entry points 20; CAPTCHA 20; other mutations 120. Failed requests also consume admission. GET and HEAD expensive reads both count.

Each table retains at most 16,384 entries, updates counters atomically, scans expiry at most once per minute, hashes identity plus a fixed action label, and rejects new identities when full. Authentication/unlock counters retain their existing configured failure windows. CAPTCHA holds at most 4,096 challenges/reservations with its existing five-minute TTL.

The configurable browsing policy, exemptions and posting cooldowns remain. Settings now validate 1–1,000,000 browsing requests and 1–86,400-second windows. Action admission uses the resolved transport/proxy/Tor identity rather than mutable visitor cookies. Existing cookie-bound posting/report identity semantics are preserved behind this independent admission boundary.

## 6. Posting and flood resistance

Both thread creation and reply routes share the production parser, transactional posting validation, media admission and independent action budgets. Sage and cross-board replies cannot escape the reply class. Board access, bans, CAPTCHA policy, cooldowns, bump limits, submission-token retry semantics and staged filesystem publication remain.

Identical legitimate submissions remain separate posts. Local tests accepted six identical new threads, then returned four 429 responses; accepted thirty replies, including sage, then returned five 429 responses. Rejected attempts did not create posts or corrupt reply counts. Existing concurrent posting/rollback tests cover publication and SQLite contention.

## 7. Report abuse

The new 30/minute report/appeal request budget covers invalid and duplicate targets before body/DB work. Existing reporting retains 20 new reports/hour per reporting identity, open-report deduplication, bounded reason text/control rejection, bans and board access, authoritative board/post/thread association, and an IMMEDIATE transaction around budget and insertion. Appeals retain their existing daily window. Separate legitimate reports remain possible.

Full-router tests exercised 30 invalid reports followed by 429. The mixed HTTP workload returned controlled 400/429 for invalid reports while ordinary reads succeeded. Existing duplicate/target/appeal tests passed.

## 8. Search abuse

Public search rejects queries above 256 scalars and pages above 500. The DB parser retains at most twelve alphanumeric terms, generates quoted AND/prefix syntax, clamps result requests to 1–100 and offsets to 0–9,999, and caps counts at 10,000. UI results at the cap show `10,000+` and request refinement.

Count and result operations each have a scoped progress callback: checks every 10,000 SQLite VM instructions, interruption at 200 checks or 500 ms. This is approximately two million VM instructions per operation, not a hard subprocess wall deadline. The callback is removed before returning a pooled connection. Broad queries produce a useful 400; saturation of the two-worker gate produces retryable 503; per-client excess produces 429. No anonymous waiter queue was added.

Existing FTS CROSS JOIN/index plans, board scoping, ordering, prefixes and ordinary results remain covered. Native FTS work between VM checkpoints can overshoot the cooperative wall allowance; this limitation is explicit.

## 9. SQL and query safety

Request-controlled values use bound parameters. Dynamic SQL reviewed consists of source-defined column lists, table/schema maintenance, fixed route options and allowlisted modes; raw search syntax and sort identifiers are not interpolated from a request. FTS input is compiled into a bounded restricted expression and passed as a parameter.

Malformed numeric IDs, negatives/zero, i64 maxima, overflow and nonnumeric values produce controlled 400/404. Search pages cannot overflow multiplication into a deep query. Thread history uses an indexed post-ID cursor; its EXPLAIN regression asserts `idx_posts_thread_live` and no temporary sorting B-tree. Maximum post-ID focus is tested through the full router.

## 10. XSS and output encoding

Reviewed post body, subject/name, filenames, board metadata, report/moderation reasons, search echo, archives, previews, admin output, recovery drafts and errors. Shared template text/attribute escaping and restricted HTTP(S) URL generation remain. Fixed F2 at the formatting order rather than relying on CSP. The cached-HTML compatibility check runs after a cheap byte cap; only affected legacy markup falls back to bounded escaped body text. Valid cached dice results are not recomputed.

The real browser rendered literal `<img ... onerror=...>`, quotes, ampersands and Unicode as text with zero injected image/handler nodes. Existing template/admin/archive tests and new URL/legacy/search-echo regressions passed. Restored HTML is regenerated from bounded raw text; imported cached HTML is not trusted.

## 11. Unicode

Text limits deliberately count Rust Unicode scalars, while envelopes, passwords, URI and header limits use bytes. Tests cover combining marks, ZWJ, bidi controls, variation/emoji/non-BMP content, oversized multibyte search and restore bodies, long quote/markup sequences, raw invalid UTF-8 and embedded NUL restore values. No identifier normalization was introduced: board/identifier allowlists retain their existing semantics. Grapheme counting is not used or promised.

## 12. Paths and filenames

Public storage uses generated controlled names, not original filenames. Existing filename sanitation/truncation, canonical containment, board-relative validation, symlink defenses, archive entry validation, backup filename checks and durable delete/publication journals remain. Media authorization is applied before serving protected board files.

A real valid PNG named `../../<audit>.png` was accepted and stored at a generated safe path with sanitized metadata. Corrupt traversal-looking, Unicode and misleading-extension filenames did not create attachments. Encoded media traversal returned 400. Existing path/archive/filename tests passed. Intentional allow-any-file boards retain their existing download policy.

## 13. MIME and media validation

The existing posting pipeline detects actual content, validates media type against board policy, and does not use the submitted extension/MIME as authorization. Existing image controls include 40-million-pixel admission, a 256-MiB decoder allocation policy and bounded animation frames. HEIF uses coded/output pixel bounds. Audio/container readers have bounded header/packet reads; FFmpeg/tool execution has deadlines, cancellation/process-group cleanup, output-file budgets and 64-KiB diagnostic retention. Existing media regressions passed.

The audit did not replace that pipeline. Shared admin/public media admission now closes an inconsistent entry point. Actual tests accepted a valid PNG and returned 415 for non-image bytes labeled `image/png` under several filename forms. The PDF policy test now verifies the content/signature of either documented PNG preview or SVG fallback; dedicated fallback tests remain.

## 14. Headers and trusted proxies

Forwarding trust requires both explicit behind-proxy mode and a CIDR-trusted immediate socket peer. XFF is resolved right-to-left through trusted hops; X-Real-IP is only a fallback when XFF is absent. Invalid authoritative chains fail back to the peer. Trusted X-Forwarded-Proto uses the last hop. Direct clients cannot choose their effective IP via forwarding headers. Operators must configure trusted CIDRs accurately and have proxies overwrite or correctly append their forwarding metadata, especially X-Real-IP/protocol.

Host validation rejects duplicates, credentials, whitespace, percent/separator ambiguity and nonnumeric/out-of-range ports while retaining valid IPv4/IPv6 authorities. Security-sensitive HTTPS redirect hosts remain configured. Request Host is not made a canonical site configuration. Content-Length is an early rejection hint; streaming limits remain authoritative. Actual duplicate Host and TE+CL requests returned 400.

## 15. Redirects

Shared internal return validation accepts bounded leading-slash paths, rejects protocol-relative/external paths, controls and all backslashes, and preserves encoded data in legitimate query strings. Existing stricter path validation inherits this boundary. Callers retain their existing route-specific fallbacks.

Configured external banner continuation is intentional external navigation with its existing target validation/warning flow; it is not a generic internal return-to mechanism. No arbitrary request Host was introduced into security-sensitive absolute redirects.

## 16. CSRF

Mutation inventory covered posting, report/appeal, polls, owner edit/delete, thread preferences, board unlock, setup, login/logout, all admin/moderation/settings/assets, maintenance, restore and update/restart. Existing admin scope/session binding, token checks, same-origin rules and cookie flags remain. Public signed form tokens must now match a present CSRF cookie; cookie-free signed forms remain supported. Explicit cross-site Origin/Referer or fetch metadata is rejected before body work; supported no-origin/null-origin flows still require handler CSRF.

GET polling/previews/read pages do not mutate content. Existing theme GET changes only a presentation cookie for the no-JS flow. Authorized backup GET can create a downloadable artifact; existing admin/CSRF controls and maintenance coordination apply. This compatibility behavior was reviewed and deliberately retained.

## 17. Memory exhaustion

Fixed retained-state caps, bounded forms/URI/multipart fields, socket/request gates, response windows and incremental restore reads reduce attacker-driven allocation. Thread responses contain at most 201 posts. Admin asset buffering stays within its existing 5/8-MiB envelopes and one shared media permit. CAPTCHA is finite. Searches materialize bounded pages. No allocation trusts Content-Length as capacity.

The local mixed test observed RSS from 40,144 to 53,152 KiB, including allocator/WAL/media caching; these are snapshots, not sampled peak measurements or a production sizing guarantee.

## 18. CPU exhaustion

Password, CAPTCHA and search each have two active worker permits; uploads/assets share the existing one-media-request permit. No unbounded admission waiter queue was added. Blocking closures own their permits so HTTP cancellation cannot release still-running work. Argon2 strength is unchanged; missing accounts now perform comparable password work inside the same bound. Search callbacks bound cooperative SQL work. Existing media subprocess deadlines and bounded parser controls remain.

## 19. Disk and temporary files

Existing RAII upload/archive temporary files, staged publication, failure cleanup, pending filesystem intent recovery, converted-output caps and post/thread deletion cleanup remain. Rejected local multipart/media requests left no temporary files. In the measured mixed fixture, the file count stayed at eleven; accepted posts/media and SQLite/WAL explain permitted disk growth.

Retention and free-space checks reduce growth but do not impose a universal storage quota or hard log-retention budget. Sustained accepted traffic and operator-created backups still require disk capacity planning. No claim of complete distributed disk-DoS prevention is made.

## 20. SQLite contention and resources

Public media work remains outside the short posting write transaction. Account reauthentication/new hashing now precede its IMMEDIATE transaction, with session/account/hash rechecks under the lock before mutation. Setup hashes before beginning its write transaction. Login drops the pooled connection before Argon2; update reauthentication does likewise.

Existing busy timeout/bounded pool checkout and retryable DbBusy responses remain; there is no new spin/retry loop. Search interruption clears pooled state. Restore staging/rollback protects the live DB from partially validated data. Local integrity checks returned `ok` and authoritative reply counts matched stored replies after posting/load; existing transaction rollback, interrupted-job and crash/recovery tests passed.

## 21. Parser and algorithmic complexity

Finite field/body/URI bounds precede parsers. Rust regex processing remains bounded by post length; tests exercise many quotes, repeated incomplete spoilers and combining/zero-width text. Post markup generates numeric reference links without a database read or backlink insertion for every marker; client backlink work is confined to the displayed posts. Post-preview requests authorize and load their explicit target separately. Missing in-window quote targets use canonical post redirects. New routing rewrite uses only generated numeric fragment attributes and a visible-ID set.

Restore pagination is keyset based and reads each body within a strict byte bound. Rate/CAPTCHA expiry scans are amortized rather than full-map scans per hostile request. Panic review found no new request-controlled unwrap/expect/panic; existing source-literal regex invariants and OS-randomness fail-closed termination are intentional.

## 22. Admin and moderator security

Admin authentication/authorization, scoped CSRF, typed actions/config validators, parameterized SQL, sensitive reauthentication and escaped stored-content rendering remain. Asset handlers now authorize before data consumption and again before publication. Account transactions recheck credentials/session after expensive work. Restore treats schema, media paths, text and cached HTML as untrusted and publishes only after preflight/staging validation. Board/saved-board/extracted-board restores enforce the same post-body scalar ceiling before staging or extraction; their JSON manifests retain the existing 64-MiB aggregate bound. Update/restart controls remain authenticated and coordinated; no external update installation was performed during this audit.

## 23. Errors and logging

Anonymous internal/TLS/restore-XHR 500 responses and decoder failures do not disclose internal paths/tool output. Useful 413/415 classifications and recognized safe malformed-image guidance remain. Wrapped application errors preserve their typed client status, including broad-search rejection. Detailed internal failures remain in server logs.

Request spans record at most 256 path characters and omit queries/cookies/tokens/bodies. Restore content-type/length diagnostics are bounded. Login username redaction remains. Existing log rotation is preserved; action budgets constrain repeated costly error paths, but there is no added universal byte quota for logs.

## 24. Security headers

Existing CSP, nosniff, frame protection, referrer policy, eligible-origin HSTS, media policy and private admin/session caching were reviewed and preserved. No CSP allowance was weakened and no inline script dependency was added. New action-limit responses use private/no-store and Retry-After; existing retryable 503 behavior is reused. Host/proxy changes protect the inputs to secure-cookie/HSTS decisions.

## 25. Authentication abuse

Pre-verification reservation closes the concurrent lockout race; retained counters and active hash work are bounded. Password sizes and stored PHC resource ceilings remain enforced. Missing users use the same current Argon2 cost class, with no possible dummy-account session. Generic errors, random sessions, expiry/revocation, logout and HttpOnly/SameSite/Secure policy remain covered by existing tests.

This removes the cheap missing-user branch; it does not claim exact constant-time end-to-end HTTP behavior across DB state or legacy password parameters.

## 26. Automated tests added and strengthened

New assembled-router tests cover legitimate/excessive posting, separate report/search budgets, declared/streamed form and URI boundaries, malformed Host/IDs/origin/fetch metadata, Unicode search/page limits and echo escaping, unauthenticated stalled admin uploads, bounded thread history/quote destinations/max-ID focus, response-lifetime admission, admin asset field boundaries/duplicates and permit release.

Helper/DB tests cover retained-state saturation/window/concurrent admission, active-work ownership under cancellation, CAPTCHA reservations, SQLite search interruption/reuse, indexed complete history traversal, malicious URL markup and legacy-cache compatibility, Unicode/quote expansion, redirect normalization controls, private upload errors, typed wrapped errors, bounded restore including NUL/invalid UTF-8, dummy authentication safety/parameter parity, capped board preference collection with individual preferences preserved, and board-restore rejection before extraction/filesystem/database mutation.

Existing multipart, media, CSRF, SQL, path, transactional rollback, cleanup, backup/restore and admin regressions were retained. No tests or lints were disabled. Independent authentication test fixtures now use distinct peer identities instead of accidentally sharing new production attempt reservations. A PDF preview policy test was corrected to validate both real preview outcomes by content; fallback-specific tests were preserved.

## 27. Local adversarial, browser and load testing

[Machine-readable results](security-audit-evidence-2026-10-06.json) record actual HTTP, Chromium and mixed-load evidence.

- Direct-mode rotating forged XFF: six controlled invalid posting responses then two 429s.
- Actual boundary/framing cases: malformed Host 400, URI 414, declared oversized form 413, cross-site action 403, numeric overflow 400, encoded media traversal 400, unauthenticated asset upload 403, malformed multipart/UTF-8 400, oversized unknown field 413, TE+CL and duplicate Host 400.
- Identical thread burst: six 303 / four 429; DB integrity `ok`, thirteen posts/thirteen threads at that checkpoint.
- Eight-worker search burst: seven 200 / thirteen 503 / twenty 429 in 0.013 seconds; normal reads remained usable. 503 is nonqueueing concurrency rejection, distinct from rate rejection.
- Partial HTTP head: connection closed after 15.003 seconds; an independent health request succeeded while it was stalled.
- Reply burst: thirty 303 / five 429; median accepted-reply latency 3.328 ms. Corrupt image bytes labeled PNG returned four 415s; a valid PNG with a traversal-looking filename returned 303.
- Mixed 160 requests with eight workers: eighty normal page reads all succeeded; totals 88×200, 12×503, 30×429, 30×400. Normal-read median 8.293 ms, maximum 22.204 ms; DB integrity and reply counts remained correct, no temporary files remained.
- Chromium posted a hostile-looking benign text thread and a normal sage reply. It found zero injected handlers/malicious image nodes. With JavaScript disabled, 464 replies were reachable exactly once over pages of 201/201/65 posts including the OP; quotes and latest/older navigation worked.

Multiple identities in the mixed tests were emitted by an explicitly configured local trusted proxy fixture; all sockets still went to 127.0.0.1. Direct untrusted-header rejection was tested separately. History bulk data was inserted into the isolated fixture solely for response/navigation verification. These small controlled runs are resilience/regression evidence, not maximum-throughput benchmarks.

## 28. Full validation commands and results

All commands passed on the final implementation using Rust 1.99.0, matching the project toolchain/MSRV.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | PASS |
| `cargo check --workspace --all-targets --all-features` | PASS |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | PASS |
| `cargo test --workspace --all-features` | PASS |
| `git diff --check` | PASS |

The complete test command passed **1,439 tests with zero failures and seven existing ignored cases in each of the library and CLI suites**, plus five integration tests, one native-update fixture test and one doctest. The two main suites exercise the shared code separately; their totals are not independent unique tests. All new security regressions ran in these suites. Targeted restore/cursor checks also passed during implementation. No test/lint was disabled or broadly suppressed.

Initial validation found an existing PDF preview fixture assuming only SVG despite the documented PNG renderer path; it now validates both outcomes by contents, while dedicated fallback tests remain. A settings-lock test failed once in the baseline and passed in subsequent complete runs without weakening the locking implementation. Earlier implementation/compiler/test-harness failures were repaired; the result table above describes the final source.

## 29. Remaining known limitations

- Per-client fixed windows reset on process restart. Distributed sources, Tor stream rotation and shared NAT identities affect per-client fairness/effectiveness; global work/socket/media/state bounds still apply. Saturated identity tables fail closed until expiry cleanup.
- Search deadlines are cooperative; native FTS work between checkpoints can exceed 500 ms. Broad legitimate queries can receive a refinement error. Gate overload is retryable 503; the gate uses the existing DbBusy response contract.
- Native Rust/media-library work is not a process sandbox and cannot universally be forcibly cancelled. Existing decoder/subprocess limits and active-work gates reduce, rather than eliminate, library/allocator/system-level risk.
- 256 open sockets per listener is a capacity bound, not guaranteed availability against a connection-saturating attacker. Network bandwidth, OS limits and distributed traffic remain outside application-only guarantees. Real local plaintext was exercised; static TLS/ACME compile and existing tests were validated without contacting a public ACME service.
- No universal disk quota, hard log byte retention or global lifetime report/post quota was added. Sustained admitted traffic can consume storage. Existing board/archive/media retention and backup policies remain operator controlled.
- Catalog/hidden output and active preference collection cap at 1,000 threads. An oversized legacy/restored board can have additional rows; normal allowed active-board settings fit this cap, and direct/thread/index navigation remains available.
- Scalar limits are not grapheme limits. Legacy malformed cached HTML uses escaped text and may lose its formatting until edited/restored. Oversized/invalid restored bodies now fail before live publication.
- Existing optional offline performance profiles remain explicitly ignored by the ordinary suite; crash-child tests marked ignored are invoked by their parent regressions. Snapshot latency/RSS evidence does not establish production peak resource usage.

## 30. Deliberately unchanged

Argon2 strength, board cooldown/CAPTCHA policy, legitimate duplicate-post semantics, report deduplication policy, public route/form/element hooks, configured browsing controls, no-JS flows, upload size/product policy, MIME decoding architecture, media streaming/ranges, staged recovery, and CSP were preserved. No User-Agent blacklist, arbitrary sleep, content censorship, new crate, commit, branch, deployment or external notification was added. The unbounded thread helper remains for offline backup/test consumers; public rendering uses the bounded helper. The final adversarial pass found no alternate unprotected posting/search/asset path.

## Changed files

- `Cargo.toml`
- `docs/security-attack-surface.md`
- `docs/security-audit-2026-10-06.md`
- `docs/security-audit-evidence-2026-10-06.json`
- `src/captcha.rs`
- `src/config.rs`
- `src/db/accounts.rs`
- `src/db/mod.rs`
- `src/db/posts.rs`
- `src/db/search_budget.rs`
- `src/db/user_thread_prefs.rs`
- `src/error.rs`
- `src/handlers/admin/accounts.rs`
- `src/handlers/admin/auth.rs`
- `src/handlers/admin/backup/http.rs`
- `src/handlers/admin/backup/restore_board.rs`
- `src/handlers/admin/backup/restore_full.rs`
- `src/handlers/admin/mod.rs`
- `src/handlers/admin/settings.rs`
- `src/handlers/admin/settings/appearance.rs`
- `src/handlers/admin/settings/banners.rs`
- `src/handlers/admin/settings/board.rs`
- `src/handlers/admin/updates.rs`
- `src/handlers/board.rs`
- `src/handlers/board/access_preferences.rs`
- `src/handlers/board/catalog.rs`
- `src/handlers/board/media.rs`
- `src/handlers/captcha.rs`
- `src/handlers/mod.rs`
- `src/handlers/render.rs`
- `src/handlers/setup.rs`
- `src/handlers/thread.rs`
- `src/middleware/csrf.rs`
- `src/middleware/ip.rs`
- `src/middleware/mod.rs`
- `src/middleware/rate_limit.rs`
- `src/middleware/rate_table.rs`
- `src/middleware/state.rs`
- `src/middleware/work_gate.rs`
- `src/server/server.rs`
- `src/server/server/connections.rs`
- `src/server/server/headers.rs`
- `src/server/server/lifecycle.rs`
- `src/server/server/router.rs`
- `src/server/server/router/security_tests.rs`
- `src/server/server/routes.rs`
- `src/templates/board.rs`
- `src/templates/thread.rs`
- `src/test_fixtures.rs`
- `src/utils/redirect.rs`
- `src/utils/sanitize.rs`
- `static/main.js`


## Requirement/evidence cross-check

| Requested phases | Evidence retained |
|---|---|
| 1: route inventory before edits | Pre-change 122-route attack-surface document and shared cost/control profiles. |
| 2–3: sizes and malformed multipart | Native/router boundaries, public/admin parser controls, exact-boundary and stalled-body regressions; actual HTTP rejection results. |
| 4–6: limiter, floods, reports | Atomic capped tables; separate action budgets; identical/sage posting and report simulations; persistent-state checks. |
| 7–8: search and SQL | Restricted bound FTS expression, count/page/progress limits, query-plan/result regressions, malformed ID and SQL-looking search data. |
| 9–10: output contexts and Unicode | Shared rendering review, markup ordering/cache fix, Chromium DOM assertions, text/byte boundaries, NUL and invalid UTF-8 regressions. |
| 11–14: path/filename/MIME/media bombs | Generated storage identifiers, canonical/archive containment, actual traversal-looking uploads, existing decoder/container/subprocess structural/deadline/cleanup tests. |
| 15–18: headers/Host/redirect/CSRF | Direct/proxy chain fixtures, wire framing tests, Host/return-path regressions, mutation inventory and scoped-token/cookie/origin tests. |
| 19–21: expensive parsing/quotes/bots | Bounded markup/FTS/media parsing, no per-marker DB work, independent expensive-action admission and optional CAPTCHA preserved. |
| 22–26: memory/CPU/disk/DB/responses | Finite state/work/socket/body/output bounds, restore incremental reads and board preflight, temporary cleanup, transaction/rollback/DbBusy tests, no-JS complete history traversal. |
| 27–30: logs/panics/leaks/admin | Bounded path-only logs, private error regressions, request-reachable panic review, pre-body authorization, transactional rechecks and safe admin rendering. |
| 31–35: state/defaults/arithmetic/headers/auth | Capped maps/CAPTCHA, startup bounds, cursor/pagination/geometry checks, preserved CSP/cookie/cache policies, password admission and dummy-account verification tests. |
| 36–38: adversarial regression/load matrix | Deterministic helper/full-router regressions plus local wire, Chromium and 160-request mixed workload evidence. |
| 39–41: substantive controls/performance/focused cleanup | Backend controls, retained duplicate/no-JS semantics, measured local read/reply latency, no new crate/lint waiver or unrelated architectural rewrite. |
| Final adversarial pass | Shared/alternate entry-point review; reserved form-route, max-ID, incremental NUL read, board preference and board-restore corrections with regressions. |
