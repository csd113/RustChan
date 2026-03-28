# Changelog

All notable changes to RustChan will be documented in this file.

---

## [1.1.0 alpha 3]

### Full-Screen TUI Console

The operator-facing terminal console has been rewritten from a scrolling line-input shell into a full-screen static TUI, matching the dashboard style introduced in RustHost.

#### Architecture

`src/server/console.rs` is deleted and replaced by a four-file module at `src/server/console/`:

| File | Responsibility |
|------|---------------|
| `mod.rs` | Alternate screen lifecycle, `RAW_MODE_ACTIVE` atomic, `ConsoleMode` / `WizardKind` enums, `start()`, `cleanup()`, `render()` loop |
| `dashboard.rs` | Pure render functions — no I/O, no DB calls; takes a `&ChanStats` snapshot and returns a formatted `String` |
| `input.rs` | Crossterm key reader (`50 ms` poll), `KeyEvent` enum, `spawn()` |
| `wizard.rs` | Interactive admin wizards; exits raw mode for `read_line`, re-enters it on completion |

A new `crossterm = "0.27"` dependency is added to `Cargo.toml`.

#### Dashboard Layout

On startup the terminal switches to the alternate screen and displays a live dashboard refreshed every 3 seconds (or immediately on `[R]`):

```
────────────────────────────────
 RustChan
────────────────────────────────

Status
 Server  : RUNNING (0.0.0.0:8080)
 Uptime  : 2h 14m 33s
 Memory  : 42.1 MiB

Activity
 Requests : 18 402    1.2/s    in-flight 3
 Online   : 7

Content
 Boards  : 4
 Threads : 831 (+2)
 Posts   : 12 047 (+8)

Storage
 Database : 94.3 MiB
 Uploads  : 1.22 GiB

  /g/  204t 3021p    /tech/  91t 1204p
  ⠹  2 file(s) uploading

────────────────────────────────
[H] Help [B] Boards [C] Create board [A] Admin [D] Del thread [L] Logs [Q] Quit
────────────────────────────────
```

Delta counts (`+N`) are coloured yellow. The upload spinner uses a Braille frame array. All ANSI helpers (`green`, `yellow`, `red`, `dim`, `bold`, `cyan`) are pure functions with no side effects.

#### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `H` | Help screen — full key reference |
| `R` | Force immediate stats refresh |
| `L` | Toggle log view (40 most recent lines) |
| `B` | Board list (ID / slug / name / NSFW / thread count / post count) |
| `C` | Create board wizard |
| `A` | Create admin wizard |
| `D` | Delete thread wizard |
| `Q` / `Esc` | Confirm-quit prompt |
| `Y` | Confirm quit |
| `N` | Cancel — return to dashboard |
| `Ctrl-C` | Force quit (no confirmation) |

#### ConsoleMode State Machine

```
Dashboard ──[L]──▶ LogView ──[L]──▶ Dashboard
         ──[H]──▶ Help
         ──[B]──▶ BoardList
         ──[Q]──▶ ConfirmQuit ──[Y]──▶ shutdown
                              ──[N]──▶ Dashboard
         ──[C/A/D]──▶ Wizard(_) ──(done)──▶ Dashboard
```

While any `Wizard(_)` mode is active, the render task skips frame output entirely — the wizard thread owns the terminal.

#### Wizard Flows

`kb_create_board`, `kb_create_admin`, and `kb_delete_thread` move from the old `console.rs` to `wizard.rs` unchanged. `run_wizard()` handles the terminal hand-off:

1. Disable raw mode, leave alternate screen
2. Run the wizard (blocking, `spawn_blocking`)
3. Re-enable raw mode, re-enter alternate screen, clear for a clean frame
4. Reset `ConsoleMode` to `Dashboard`

#### Stats Refresh

A background task in `server.rs` polls the database every 3 seconds and writes into `SharedStats` (`Arc<RwLock<ChanStats>>`). `KeyEvent::Reload` triggers an immediate refresh outside the timer. `render()` takes a read lock on `SharedStats` — no DB calls ever happen on the render path.

#### Terminal Safety

- `RAW_MODE_ACTIVE` atomic prevents double-cleanup
- `cleanup()` is called from both the `std::panic` hook (registered in `main.rs`) and the graceful shutdown path
- The terminal is always restored even on unexpected exits

### 🔄 Changed

- `src/server/mod.rs` — `pub mod console` now resolves to the sub-directory; `pub use console::cleanup` added for the panic hook path in `main.rs`
- `src/server/server.rs` — `spawn_keyboard_handler()` replaced by `console::start()`; `match cmd.as_str()` loop replaced by typed `match key_event` loop; stats refresh background task added
- `src/main.rs` — panic hook registers `crate::server::cleanup()`; explicit `console::cleanup()` call added after server future resolves

---

### Native HTTPS / TLS Support

RustChan can now serve itself directly over HTTPS without needing a reverse proxy in front of it. Two modes are available:

**Self-signed certificate** — enabled with two lines in `settings.toml`. A certificate is generated automatically on first run and saved to disk. Your browser will show a security warning (normal for self-signed), which you can accept. Good for local development and private installs.

**Let's Encrypt (ACME)** — for public servers with a real domain name. RustChan contacts Let's Encrypt automatically, proves it owns the domain, and gets a trusted certificate. No browser warning. Renews itself before it expires.

Both modes run alongside the existing HTTP server — adding HTTPS does not remove or break anything. Installs that do not add a `[tls]` section to `settings.toml` are completely unaffected.

### HTTP → HTTPS Redirect

When HTTPS is enabled, an optional redirect listener can be turned on. Any visitor who arrives on the plain HTTP port is automatically sent to the HTTPS address. Enable with `redirect_http = true` in the `[tls]` section.

### HSTS (automatic)

Once a visitor connects over HTTPS, their browser is instructed to always use HTTPS for future visits. No configuration needed — this activates automatically when TLS is running.

### Fixes

- **IP banning and rate limiting now work correctly over HTTPS** — the security features that track visitor IPs continue to work on HTTPS connections the same way they do on HTTP. No bans or limits are bypassed by switching to HTTPS.
- **Secure cookies enforced when TLS is active** — session and auth cookies are automatically marked `Secure` when HTTPS is enabled, preventing them from being sent over plain HTTP.

### Auto-Terminal Launch Support

RustChan now automatically opens in a terminal window when double-clicked, instead of silently failing. If already running in a terminal, it behaves as normal. Works on Windows, Linux, and macOS.

### fixes:

- **Files left behind on DB errors:** Disk fills forever. *Fix:* Show errors clearly, handle deletions properly.
- **Stuck tasks after crashes:** Jobs never restart. *Fix:* Auto-reset at startup, limit retries.
- **Huge text uploads crash memory:** Attackers overload server. *Fix:* Cap text fields at 64KB.
- **Multiple backups corrupt progress:** Overlapping runs mess up display. *Fix:* Add lock flag.
- **ZIP files write to wrong folders:** Hackers escape safe areas. *Fix:* Strict path checks.
- **Temp folder tricks break SQL:** Env vars inject bad chars. *Fix:* Use safe folder near DB.
- **Restore uploads fill disk unchecked:** No per-file limits. *Fix:* Cap at 4GB per file.
- **ZIP bombs explode RAM:** Bad peers unpack gigabytes. *Fix:* Limit entries to 8MB each.
- **FFmpeg hangs block everything:** No timeouts tie up workers. *Fix:* Add 2-min timeout, kill if stuck.
- **Leftover backup files after crashes:** Disk clogs on restart. *Fix:* Startup cleanup, use safe temp folder.

---

## [1.1.0 alpha 2]

The headline change in this release is a deep security and correctness audit of the Arti/Tor implementation introduced in alpha 1, resulting in six critical fixes, nine high-priority fixes, and a set of new operator-facing configuration options. Alongside that, this release includes reliability improvements to shutdown coordination, backup handling, multipart parsing, and the database layer.

---

### 🔒 Tor / Arti — Security & Correctness Audit

#### Architecture

The hidden service implementation from alpha 1 has been audited and corrected. The core architecture — bootstrapping Arti in-process, deriving a `.onion` address from a persistent Ed25519 keypair, and proxying inbound onion streams to the local HTTP port — is unchanged. What changed is correctness, isolation, and operational safety.

---

#### 🔴 Critical fixes

**Per-stream IP isolation for Tor users**

Previously every Tor user resolved to `127.0.0.1` as their client IP. The Arti proxy was a raw TCP passthrough (`copy_bidirectional`) with no HTTP awareness, so no header injection was possible. This meant all Tor users shared a single rate-limit bucket, ban entry, and post cooldown: banning one Tor user banned everyone on Tor simultaneously.

Fixed by introducing `TOR_STREAM_TOKENS`, a `DashMap<u16, Arc<str>>` in `detect.rs` keyed by the ephemeral local port of each proxy connection. When `proxy_tor_stream` connects to the local axum socket, the OS assigns an ephemeral source port; axum's `ConnectInfo` sees this as the peer port on the accepted socket. A random `tor:<hex>` token is inserted into the map under that port, and a `TokenGuard` RAII struct removes it when the task ends. Both `ClientIp::from_request_parts` and `extract_ip` now look up the peer port in `TOR_STREAM_TOKENS` when the connection is from loopback with `enable_tor_support=true`, returning the per-stream token instead of `127.0.0.1`. Every Tor stream now has its own isolated bucket for rate limiting, bans, and post cooldowns.

**Files:** `src/detect.rs`, `src/middleware/mod.rs`

---

**Tor-only mode (`tor_only` setting)**

With `enable_tor_support = true` and the default `bind_addr = 0.0.0.0:8080`, the HTTP server was reachable directly over clearnet simultaneously with the hidden service. An operator expecting a private Tor-only site had no way to enforce that without manually overriding `bind_addr`.

Added a new `tor_only` setting to `settings.toml`. When `tor_only = true` and `enable_tor_support = true`, `bind_addr` is silently overridden to `127.0.0.1:{port}` during config loading — the port is preserved, only the host changes. The override is logged at startup. Default remains `false` (dual-stack: clearnet and Tor both active), which is the correct default for an imageboard that wants to be reachable both ways.

```toml
# Restrict to Tor-only (hidden service). Clearnet access blocked.
# tor_only = false
```

**Files:** `src/config.rs`

---

**Graceful shutdown for the Tor task**

The Tor retry loop had no `CancellationToken`. During shutdown, `worker_cancel.cancel()` signaled every other background task but the Tor task continued running — sleeping through a backoff of up to 480 seconds. The shutdown code then hit a hard 10-second timeout and abandoned the task, leaving Tor circuits open without sending `RELAY_END` cells.

Fixed by adding a `cancel: CancellationToken` parameter to `detect_tor()`. Both the `run_arti(...)` call and the backoff sleep now use `tokio::select!` against the token, so the task exits promptly when shutdown is signaled. The `worker_cancel` variable in `run_server()` is moved to before the `detect_tor` call so it is available to pass in. The shutdown timeout is extended from 10s to 15s as a safety net for any in-flight `copy_bidirectional` draining — in practice the task exits in milliseconds once the token fires.

**Files:** `src/detect.rs`, `src/server/server.rs`

---

**`tor_client` and `onion_service` explicit keepalive**

`tor_client` is last used on the line that calls `launch_onion_service`. `onion_service` is last used inside the `HsId` retry block. Both have side-effectful `Drop` implementations: dropping `tor_client` closes all Tor circuits; dropping `onion_service` deregisters the hidden service from the Tor network. Both variables must stay alive through the entire stream loop.

Rust named `let` bindings drop at end of their enclosing scope (the function body), not at last-use, so this was not a live bug — but it was invisible and fragile. Added explicit `let _ = &tor_client; let _ = &onion_service;` keepalive borrows at the end of `run_arti`, after the stream loop exits, making the intent unambiguous and guarding against any future tooling that might warn about "unused" bindings.

**Files:** `src/detect.rs`

---

#### 🟠 High-priority fixes

**Onion address encoder: fixed checksum computation**

In `hsid_to_onion_address`, the two checksum bytes were extracted from the `Sha3_256` digest using an iterator with `.unwrap_or(0)` fallbacks. `Sha3_256` always produces 32 bytes so the fallback was dead code, but it masked the logic and would silently produce a wrong checksum if the digest size ever changed. Replaced with direct array indexing: `let hash: [u8; 32] = hasher.finalize().into(); let checksum = [hash[0], hash[1]];`.

Added a Python-verified cryptographic test vector for the all-zeros Ed25519 key:
```
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaam2dqd.onion
```
Verified with:
```python
import hashlib, base64
pub = bytes(32); ver = bytes([3])
chk = hashlib.sha3_256(b'.onion checksum' + pub + ver).digest()[:2]
print(base64.b32encode(pub+chk+ver).decode().lower().rstrip('=')+'.onion')
```

**Files:** `src/detect.rs`

---

**`Onion-Location` response header for Tor Browser**

Tor Browser reads the `Onion-Location` response header and automatically prompts the user to switch to the `.onion` address when browsing the clearnet version of a site. The header was never set anywhere in the codebase.

Added `onion_location_middleware` — an async middleware function that reads `state.onion_address`, and when the address is known and the response `Content-Type` is `text/html`, inserts `Onion-Location: http://<addr>` into the response headers. Wired into `build_router` via `axum_middleware::from_fn_with_state` at the outermost position so it fires on every HTML response. Non-HTML responses (static assets, JSON, media) are skipped.

**Files:** `src/server/server.rs`

---

**Configurable bootstrap timeout**

The Tor bootstrap timeout was hardcoded at 120 seconds. On censored networks using bridges or pluggable transports, directory fetch is slow and 120 seconds is insufficient — the task would time out, wait through exponential backoff, and retry indefinitely without ever succeeding.

Added `tor_bootstrap_timeout_secs` to `settings.toml` (default 120). The timeout error message now includes a hint to increase this value.

```toml
# Increase for censored networks or when using bridges.
# tor_bootstrap_timeout_secs = 120
```

**Files:** `src/config.rs`, `src/detect.rs`

---

**Configurable maximum concurrent Tor streams**

`MAX_CONCURRENT_TOR_STREAMS` was a hardcoded compile-time constant (`512`). Operators on resource-constrained hosts (low FD limits, limited RAM) had no way to reduce it without recompiling.

Added `tor_max_concurrent_streams` to `settings.toml` (default 512). When the limit is reached, `stream_req` is dropped explicitly — Arti sends a `RELAY_END` cell automatically on drop.

```toml
# Reduce if the process hits file descriptor limits.
# tor_max_concurrent_streams = 512
```

**Files:** `src/config.rs`, `src/detect.rs`

---

**Infrastructure errors distinguished from normal stream closure**

All errors from `proxy_tor_stream` were logged at `DEBUG` with the message "Tor: stream closed". This made it impossible to distinguish a normal client disconnect (expected, routine) from "local TCP connect failed" (axum has crashed or is unrestarted — requires operator attention).

Split error handling: connection failures to the local HTTP server now log at `ERROR` with a clear message ("Tor: cannot reach local HTTP server — is axum running?"). Normal stream closures (EOF, client disconnect, keep-alive expiry) continue to log at `DEBUG`.

**Files:** `src/detect.rs`

---

**Attempt counter reset after healthy session**

The exponential backoff retry counter (`attempt`) incremented on both crash exits and clean exits. After 4 clean reconnect cycles, the service was waiting 480 seconds between restart attempts — identical behavior to a crash loop. A clean exit after ≥60 seconds of healthy operation now resets `attempt = 0`.

**Files:** `src/detect.rs`

---

**`Arc<str>` for the local address string**

`local_addr` was a `String` cloned into every spawned proxy task — one heap allocation per Tor connection. Replaced with `Arc<str>`, making each clone an atomic reference count increment with no heap allocation.

**Files:** `src/detect.rs`

---

**Configurable service nickname**

The Arti onion service nickname was hardcoded to `"rustchan"`. When multiple instances share the same `arti_state/` directory (e.g. Docker volume mounts, CI), identical nicknames cause key collisions and one instance fails to start its onion service.

Added `tor_service_nickname` to `settings.toml` (default `"rustchan"`).

```toml
# Change when running multiple instances sharing the same arti_state/ directory.
# tor_service_nickname = "rustchan"
```

**Files:** `src/config.rs`, `src/detect.rs`

---

**Onion address omitted from structured INFO log**

The onion address was logged as a structured field at `INFO` level, causing it to appear in plaintext in the JSON log file (`rustchan.log`) and any log aggregator or forwarding pipeline it feeds into. For operators running a sensitive hidden service, this is unwanted metadata exposure.

The address is now logged as a structured field only at `DEBUG`. A bare `INFO` event ("Tor: hidden service active") fires without the address. The TTY banner and admin panel always show the full address.

**Files:** `src/detect.rs`

---

#### 🟡 Other Arti changes

- **`yield_now()` in rendezvous loop** — `stream_requests.next().await` runs in a tight async loop. Under a connection flood, the task could monopolize the Tokio executor thread between `next()` returning and the `tokio::spawn` call. Added `tokio::task::yield_now().await` at the top of the loop body.
- **Local connect timeout increased** — the timeout for `proxy_tor_stream` to connect to the local axum socket was 5 seconds. Under load the axum TCP accept queue fills and `connect()` can legitimately take longer. Increased to 15 seconds.
- **Dead `ToolStatus::Spawning` variant removed** — `Spawning` was added for the old subprocess-based Tor launcher. `detect_tor` now returns `Option<JoinHandle<()>>` and never produces this variant. Removed to prevent future code from adding unreachable match arms.
- **`stream_req.target()` call removed** — this method does not exist on `StreamRequest` in `tor-hsservice 0.40`. The diagnostic log line it produced has been removed.
- **`run_arti` refactored for line-count compliance** — the onion address publication and TTY banner block extracted into `async fn publish_onion_address()`, keeping `run_arti` under the clippy line-count threshold.

---

### 🔴 Critical — HTTP 500 errors on pages with gateway posts

Posts inserted via the ChanNet gateway carry no IP address. Pages that attempted to display or process these posts were crashing because `ip_hash` was typed as `String` and the `NULL` database value caused a panic.

Changed `ip_hash` to `Option<String>` throughout. `NULL` values are now handled gracefully — they render as an empty string in templates and are passed through backup/restore without modification.

**Files:** `src/models.rs`, `src/db/posts.rs`, `src/db/admin.rs`, `src/templates/thread.rs`, `src/templates/admin.rs`, `src/handlers/admin/backup.rs`, `src/handlers/backup.rs`

---

### 🟠 Reliability & Shutdown

**Worker lifecycle**

- Persisted `JoinHandle`s returned by the worker pool; shutdown now awaits each worker with a bounded per-worker timeout instead of a blind fixed sleep
- Signaling via `CancellationToken` threaded through every worker task
- Prevents corruption of in-progress FFmpeg transcodes during shutdown
- Added startup recovery to reset jobs stuck in `running` state after an unclean exit

**ChanNet server**

- Added graceful shutdown support to the ChanNet listener (port 7070)
- Unified shutdown signal with the main HTTP server so in-flight federation requests drain before the process exits

**Background tasks**

- All periodic background tasks (session purge, WAL checkpoint, IP prune, login-fail prune, VACUUM, poll cleanup, cache eviction) replaced infinite loops with `tokio::select!` against the worker cancel token
- Ensures every task exits cleanly on shutdown with no orphaned async tasks

**HTTP**

- Added request timeout middleware (30 seconds) to protect against slow-loris style attacks and stalled client connections

---

### 🟠 Multipart Handling

- Added strict per-field size limits: post body capped at ~100 KB, name/subject/other fields at ~4 KB
- Replaced unbounded `field.text()` calls with controlled byte-reading that returns `413` the moment the running total exceeds the limit
- Eliminated OOM risk under concurrent large-form submissions
- Hardened poll duration parsing: added overflow validation before the seconds multiplication step

**Files:** `src/handlers/mod.rs`

---

### 🟠 Backup System

- Replaced `VACUUM INTO` string-based SQL with the `rusqlite::backup` API — eliminates manual SQL escaping and improves cross-platform correctness
- Introduced RAII-style temporary file cleanup: backup artifacts are removed even on client disconnect, early termination, or runtime drop
- Database pool exhaustion during backup now returns 503 (retryable) instead of 500

**Files:** `src/handlers/admin/backup.rs`

---

### 🟡 Database

- Made connection pool size configurable; removed hardcoded pool limit
- `r2d2::Error` (pool exhaustion) now maps to `503 Service Unavailable` instead of 500
- Removed `unwrap_or(0)` silent fallback in DB initialization — replaced with proper error propagation
- Replaced `unchecked_transaction()` (DEFERRED) with `BEGIN IMMEDIATE` across `threads.rs`, `posts.rs`, `admin.rs`, `boards.rs` — eliminates mid-transaction lock upgrade failures under concurrent write load

**Files:** `src/db/mod.rs`, `src/db/threads.rs`, `src/db/posts.rs`, `src/db/admin.rs`, `src/db/boards.rs`

---

### 🟡 Logging

- Replaced rolling-never log strategy with a rotating appender to prevent unbounded disk growth
- Fixed log directory: logs now write to `rustchan-data/` instead of the executable folder
- Fixed log filename format: rotated files now named `rustchan.2024-01-15.log` instead of `rustchan.log.2024-01-15`

**Files:** `src/main.rs`, `src/logging.rs`

---

### 🟡 Other fixes

- **HTTP 304 responses** — removed `.unwrap_or_default()` from 304 response builders; replaced with explicit safe construction
- **Configuration file writes** — replaced non-atomic file writes with write-to-temp-then-rename pattern; prevents `settings.toml` corruption on crash

**Files:** `src/handlers/thread.rs`, `src/handlers/board.rs`, `src/config.rs`

---

## [1.1.0 alpha 1]

## 🌐 New: ChanNet API (Port 7070)

RustChan can now talk to other RustChans. Introducing the **ChanNet API** — a two-layer federation and gateway system living entirely on port 7070.

**Layer 1 — Federation** (`/chan/export`, `/chan/import`, `/chan/refresh`, `/chan/poll`): nodes sync with each other via ZIP snapshots. Push your posts out, pull theirs in, keep your mirror fresh.

**Layer 2 — RustWave Gateway** (`/chan/command`): the [RustWave](https://github.com/a2kiti/rustwave) audio transport client gets its own command interface. Send a typed JSON command, get a ZIP back. Supported commands: `full_export`, `board_export`, `thread_export`, `archive_export`, `force_refresh`, and `reply_push` (the only one that actually writes anything).

Text only — no images, no media, no binary data cross this interface by design. Full schema docs in `channet_api_reference.docx`.

---

## Architecture Refactor

This release restructures the codebase for maintainability. No user-facing
behavior has changed. Every route, every feature, every pixel is identical.
The only difference is where the code lives.

### The problem

`main.rs` had grown to 1,757 lines and owned everything from the HTTP router
to the ASCII startup banner. `handlers/admin.rs` hit 4,576 lines with 33
handler functions covering auth, backups, bans, reports, settings, and more.
Both files were becoming difficult to navigate and risky to modify.

### What changed

**Phase 1 — Cleanup**

- Removed unused `src/theme-init.js` (dead duplicate of `static/theme-init.js`)
- Moved `validate_password()` from `main.rs` to `utils/crypto.rs` alongside
  the other credential helpers
- Moved `first_run_check()` and `get_per_board_stats()` from `main.rs` into
  the `db` module, eliminating the only raw SQL that lived outside `db/`

**Phase 2 — Background work**

- Moved `evict_thumb_cache()` from `main.rs` to `workers/mod.rs` where it
  belongs alongside the other background maintenance operations

**Phase 3 — Console extraction**

- Created `src/server/` directory for server infrastructure
- Extracted terminal stats, keyboard console, startup banner, and all `kb_*`
  helpers to `server/console.rs` (~350 lines)

**Phase 4 — CLI extraction**

- Moved `Cli`, `Command`, `AdminAction` clap types and `run_admin()` to
  `server/cli.rs` (~250 lines)

**Phase 5 — Server extraction**

- Moved `run_server()`, `build_router()`, all 7 background task spawns,
  static asset handlers, HSTS middleware, request tracking, `ScopedDecrement`,
  and global atomics to `server/server.rs` (~800 lines)
- `main.rs` is now ~50 lines: runtime construction, CLI parsing, dispatch

**Phase 6 — Admin handler decomposition**

- Converted `handlers/admin.rs` to a module folder (`handlers/admin/`)
- Extracted `backup.rs` — all backup and restore handlers (~2,500 lines)
- Extracted `auth.rs` — login, logout, session management
- Extracted `moderation.rs` — bans, reports, appeals, word filters, mod log
- Extracted `content.rs` — post/thread actions, board management
- Extracted `settings.rs` — site settings, VACUUM, admin panel
- `admin/mod.rs` now contains only shared session helpers and re-exports

### By the numbers

```
File                Before        After
main.rs             1,757 lines   ~50 lines
handlers/admin.rs   4,576 lines   split across 6 files
server/ (new)       —             ~1,400 lines total
db/                 unchanged     + 2 functions from main.rs
workers/            unchanged     + evict_thumb_cache
utils/crypto.rs     unchanged     + validate_password
```

### What was not changed

`db/`, `templates/`, `utils/`, `media/`, `config.rs`, `error.rs`, `models.rs`,
`detect.rs`, `handlers/board.rs`, `handlers/thread.rs`, and `middleware/` are
all untouched. They were already well-structured.
```

## New Module: src/media/

### media/ffmpeg.rs — FFmpeg detection and subprocess execution

- Added detect_ffmpeg() for checking FFmpeg availability (synchronous, suitable for spawn_blocking)
- Added run_ffmpeg() shared executor used by all FFmpeg calls
- Added ffmpeg_image_to_webp() with quality 85 and metadata stripping
- Added ffmpeg_gif_to_webm() using VP9 codec, CRF 30, zero bitrate target, metadata stripped
- Added ffmpeg_thumbnail() extracting first frame as WebP at quality 80 with aspect-preserving scale
- Added probe_video_codec() via ffprobe subprocess (moved from utils/files.rs)
- Added ffmpeg_transcode_to_webm() using path-based API (replaces old bytes-in/bytes-out version)
- Added ffmpeg_audio_waveform() using path-based API (same refactor as above)

### media/convert.rs — Per-format conversion logic

- Added ConversionAction enum: ToWebp, ToWebm, ToWebpIfSmaller, KeepAsIs
- Added conversion_action() mapping each MIME type to the correct action
- Added convert_file() as the main entry point for all conversions
- PNG to WebP is attempted but original PNG is kept if WebP is larger
- All conversions use atomic temp-then-rename strategy
- FFmpeg failures fall back to original file with a warning (never panics, never returns 500)

### media/thumbnail.rs — WebP thumbnail generation

- All thumbnails output as .webp
- SVG placeholders used for video without FFmpeg, audio, and SVG sources
- Added generate_thumbnail() as unified entry point
- Added image crate fallback path for when FFmpeg is unavailable (decode, resize, save as WebP)
- Added thumbnail_output_path() for determining correct output path and extension
- Added write_placeholder() for generating static SVG placeholders by kind

### media/exif.rs — EXIF orientation handling (new file)

- Moved read_exif_orientation and apply_exif_orientation from utils/files.rs

### media/mod.rs — Public API

- Added ProcessedMedia struct with file_path, thumbnail_path, mime_type, was_converted, original_size, final_size
- Added MediaProcessor::new() with FFmpeg detection and warning log if not found
- Added MediaProcessor::new_with_ffmpeg() as lightweight constructor for request handlers
- Added MediaProcessor::process_upload() for conversion and thumbnail generation (never propagates FFmpeg errors)
- Added MediaProcessor::generate_thumbnail() for standalone thumbnail regeneration
- Registered submodules: convert, ffmpeg, thumbnail, exif

---

## Modified Files

### src/utils/files.rs

- Extended detect_mime_type with BMP, TIFF (LE and BE), and SVG detection including BOM stripping
- Rewrote save_upload to delegate conversion and thumbnailing to MediaProcessor
- GIF to WebM conversions now set processing_pending = false (converted inline, no background job)
- MP4 and WebM uploads still set processing_pending = true as before
- Removed dead functions: generate_video_thumb, ffmpeg_first_frame, generate_video_placeholder, generate_audio_placeholder, generate_image_thumb
- Removed relocated functions: ffprobe_video_codec, probe_video_codec, ffmpeg_transcode_webm, transcode_to_webm, ffmpeg_audio_waveform, gen_waveform_png
- EXIF functions kept as thin private delegates to crate::media::exif for backward compatibility
- Added mime_to_ext_pub() public wrapper for use by media/convert.rs
- Added apply_thumb_exif_orientation() for post-hoc EXIF correction on image crate thumbnails
- Added tests for BMP, TIFF LE, TIFF BE, SVG detection and new mime_to_ext mappings

### src/models.rs

- Updated from_ext to include bmp, tiff, tif, and svg

### src/lib.rs and src/main.rs

- Registered new media module

### src/workers/mod.rs

- Updated probe_video_codec call to use crate::media::ffmpeg::probe_video_codec
- Replaced in-memory transcode_to_webm with path-based ffmpeg_transcode_to_webm using temp file persist
- Replaced in-memory gen_waveform_png with path-based ffmpeg_audio_waveform using temp file persist
- File bytes now read from disk only for SHA-256 dedup step

### Cargo.toml

- Added bmp and tiff features to the image crate dependency


## [1.0.13] — 2026-03-08

## WAL Mode + Connection Tuning
**`db/mod.rs`**

`cache_size` bumped from `-4096` (4 MiB) to `-32000` (32 MiB) in the pool's `with_init` pragma block. The `journal_mode=WAL` and `synchronous=NORMAL` pragmas were already present.

---

## Missing Indexes
**`db/mod.rs`**

Two new migrations added at the end of the migration table:

- **Migration 23:** `CREATE INDEX IF NOT EXISTS idx_posts_thread_id ON posts(thread_id)` — supplements the existing composite index for queries that filter on `thread_id` alone.
- **Migration 24:** `CREATE INDEX IF NOT EXISTS idx_posts_ip_hash ON posts(ip_hash)` — eliminates the full-table scan on the admin IP history page and per-IP cooldown checks.

---

## Prepared Statement Caching Audit
**`db/threads.rs` · `db/boards.rs` · `db/posts.rs`**

All remaining bare `conn.prepare(...)` calls on hot or repeated queries replaced with `conn.prepare_cached(...)`: `delete_thread`, `archive_old_threads`, `prune_old_threads` (outer `SELECT`) in `threads.rs`; `delete_board` in `boards.rs`; `search_posts` in `posts.rs`. Every query path is now consistently cached.

---

## Transaction Batching for Thread Prune
Already implemented in the codebase. Both `prune_old_threads` and `archive_old_threads` already use `unchecked_transaction()` / `tx.commit()` to batch all deletes/updates into a single atomic transaction. No changes needed.

---

## RETURNING Clause for Inserts
**`db/threads.rs` · `db/posts.rs`**

`create_thread_with_op` and `create_post_inner` now use `INSERT … RETURNING id` via `query_row`, replacing the `execute()` + `last_insert_rowid()` pattern. The new ID is returned atomically in the same statement, eliminating the implicit coupling to connection-local state.

---

## Scheduled VACUUM
**`config.rs` · `main.rs`**

Added `auto_vacuum_interval_hours = 24` to config. A background Tokio task now sleeps for the configured interval (staggered from startup), then calls `db::run_vacuum()` via `spawn_blocking` and logs the bytes reclaimed.

---

## Expired Poll Cleanup
**`config.rs` · `main.rs` · `db/posts.rs`**

Added `poll_cleanup_interval_hours = 72`. A new `cleanup_expired_poll_votes()` DB function deletes vote rows for polls whose `expires_at` is older than the retention window. A background task runs it on the configured interval, preserving poll questions and options.

---

## DB Size Warning
**`config.rs` · `handlers/admin.rs` · `templates/admin.rs`**

Added `db_warn_threshold_mb = 2048`. The admin panel handler reads the actual file size via `std::fs::metadata`, computes a boolean flag, and passes it to the template. The template renders a red warning banner in the database maintenance section when the threshold is exceeded.

---

## Job Queue Back-Pressure
**`config.rs` · `workers/mod.rs`**

Added `job_queue_capacity = 1000`. The `enqueue()` method now checks `pending_job_count()` before inserting — if the queue is at or over capacity, the job is dropped with a `warn!` log and a sentinel `-1` is returned, avoiding OOM under post floods.

---

## Coalesce Duplicate Media Jobs
**`workers/mod.rs`**

Added an `Arc<DashMap<String, bool>>` (`in_progress`) to `JobQueue`. Before dispatching a `VideoTranscode` or `AudioWaveform` job, `handle_job` checks if the `file_path` is already in the map — if so it skips and logs. The entry is removed on both success and failure.

---

## FFmpeg Timeout
**`config.rs` · `workers/mod.rs`**

Replaced hardcoded `FFMPEG_TRANSCODE_TIMEOUT` / `FFMPEG_WAVEFORM_TIMEOUT` constants with `CONFIG.ffmpeg_timeout_secs` (default: `120`). Both `transcode_video` and `generate_waveform` now read this value at runtime so operators can tune it in `settings.toml`.

---

## Auto-Archive Before Prune
**`workers/mod.rs` · `config.rs`**

`prune_threads` now evaluates `allow_archive || CONFIG.archive_before_prune`. The new global flag (default `true`) means no thread is ever silently hard-deleted on a board that has archiving enabled at the global level, even if the individual board didn't opt in.

---

## Waveform Cache Eviction
**`main.rs` · `config.rs`**

A background task runs every hour (after a 30-min startup stagger). It walks every `{board}/thumbs/` directory, sorts files oldest-first by mtime, and deletes until total size is under `waveform_cache_max_mb` (default 200 MiB). A new `evict_thumb_cache` function handles the scan-and-prune logic; originals are never touched.

---

## Streaming Multipart
**`handlers/mod.rs`**

The old `.bytes().await` (full in-memory buffering) is replaced by `read_field_bytes`, which streams via `.chunk()` and returns a `413 UploadTooLarge` the moment the running total exceeds the configured limit — before memory is exhausted.

---

## ETag / Conditional GET
**`handlers/board.rs` · `handlers/thread.rs`**

Both handlers now accept `HeaderMap`, derive an ETag (board index: `"{max_bump_ts}-{page}"`; thread: `"{bumped_at}"`), check `If-None-Match`, and return `304 Not Modified` on a hit. The ETag is included on all 200 responses too.

---

## Gzip / Brotli Compression
**`main.rs` · `Cargo.toml`**

`tower-http` features updated to `compression-full`. `CompressionLayer::new()` added to the middleware stack — it negotiates gzip, Brotli, or zstd based on the client's `Accept-Encoding` header.

---

## Blocking Pool Sizing
**`main.rs` · `config.rs`**

`#[tokio::main]` replaced with a manual `tokio::runtime::Builder` that calls `.max_blocking_threads(CONFIG.blocking_threads)`. Default is `logical_cpus × 4` (auto-detected); configurable via `blocking_threads` in `settings.toml` or `CHAN_BLOCKING_THREADS`.

---

## EXIF Orientation Correction
**`utils/files.rs` · `Cargo.toml`**

`kamadak-exif = "0.5"` added. `generate_image_thumb` now calls `read_exif_orientation` for JPEGs and passes the result to `apply_exif_orientation`, which dispatches to `imageops::rotate90/180/270` and `flip_horizontal/vertical` as needed. Non-JPEG formats skip the EXIF path entirely.

### ✨ Added
- **Backup system rewritten to stream instead of buffering in RAM** — all backup operations previously loaded entire zip files into memory, risking OOM on large instances. Downloads now stream from disk in 64 KiB chunks (browsers also get a proper progress bar). Backup creation now writes directly to disk via temp files with atomic rename on success, so partial backups never appear in the saved list. Individual file archiving now streams through an 8 KiB buffer instead of reading each file fully into memory. Peak RAM usage dropped from "entire backup size" to roughly 64 KiB regardless of instance size.
- **ChanClassic theme** — a new theme that mimics the classic 4chan aesthetic: light tan/beige background, maroon/red accents, blue post-number links, and the iconic post block styling. Available in the theme picker alongside existing themes.
- **Default theme in settings.toml** — the generated `settings.toml` now includes a `default_theme` field so the server-side default theme can be set before first startup, without requiring admin panel access.
- **Home page subtitle in settings.toml** — `site_subtitle` is now present in the generated `settings.toml` directly below `forum_name`, allowing the home page subtitle to be configured at install time.
- **Default theme selector in admin panel** — the Site Settings section now includes a dropdown to set the site-wide default theme served to new visitors.

### 🔄 Changed
- **Admin panel reorganized** — sections are now ordered: Site Settings → Boards → Moderation Log → Report Inbox → Moderation (ban appeals, active bans, word filters consolidated) → Full Site Backup & Restore → Board Backup & Restore → Database Maintenance → Active Onion Address. Code order matches page order for easier future editing.
- **"Backup & Restore" renamed** to **"Full Site Backup & Restore"** to clearly distinguish it from the board-level backup section.
- **Ban appeals, active bans, and word filters** condensed into a single **Moderation** panel with clearly labelled subsections.

---

## [1.0.12] — 2026-03-07

### 🔄 Changed
- **Database module fixes** — `threads.rs`: added explicit `ROLLBACK` on failed `COMMIT` to prevent dirty transaction state. `mod.rs`: added `sort_unstable` + `dedup` to `paths_safe_to_delete` to eliminate duplicate path entries. `mod.rs`: added `media_type` and `edited_at` columns to the base `CREATE TABLE posts` schema to match the final migrated state. `admin.rs`: replaced inlined Post row mapper with shared `super::posts::map_post` to eliminate duplication. `admin.rs`: clarified `run_wal_checkpoint` doc comment on return tuple order.
- **Template module fixes** — `board.rs`: fixed archive thumbnail path prefix from `/static/` to `/boards/`. `board.rs`: moved `fmt_ts` to the top-level import, removed redundant local `use` inside `archive_page`. `thread.rs`: corrected misleading comment about embed and draft script loading. `thread.rs`: added doc comment documenting the `body_html` trust precondition on `render_post`. `forms.rs`: removed dead `captcha_js` variable and no-op string concatenation.
- **CSS cleanup** — removed 11 dead rules for classes never emitted by templates or JS (`.greentext`, `.quote-link`, `.admin-thread-del-btn`, duplicate `.media-expanded`, `.media-rotate-btn`, `.thread-id-badge`, `.quote-block`, `.quote-toggle`, `.archive-heading`, `.autoupdate-bar`, `.video-player`). Fixed two undefined CSS variable references (`--font-mono` → `--font`, `--bg-body` → `--bg`). Merged duplicate `.file-container` block into a single declaration.
- **Database module split** — the 2,264-line monolithic `db.rs` has been reorganized into five focused modules with zero call-site changes (all existing `db::` references compile unchanged):
  - `mod.rs` (466 lines) — connection pool, shared types (`NewPost`, `CachedFile`), schema initialization, shared helpers
  - `boards.rs` (293 lines) — site settings, board CRUD, stats
  - `threads.rs` (333 lines) — thread listing, creation, mutation, archiving, pruning
  - `posts.rs` (642 lines) — post CRUD, file deduplication, polls, job queue, worker helpers
  - `admin.rs` (558 lines) — admin sessions, bans, word filters, reports, mod log, ban appeals, IP history, maintenance
- **Template module split** — the 2,736-line monolithic template file has been reorganized into five focused modules with no changes to the public API (all existing handler code works without modification):
  - `mod.rs` (392 lines) — shared infrastructure: site name/subtitle statics, base layout, pagination, timestamp formatting, utility helpers
  - `board.rs` (697 lines) — home page, board index, catalog, search, and archive rendering
  - `thread.rs` (738 lines) — thread view, post rendering, polls, and post edit form
  - `admin.rs` (760 lines) — login page, admin panel, mod log, VACUUM results, IP history
  - `forms.rs` (198 lines) — new thread and reply forms, shared across board and thread pages

### 🔒 Security Fixes

**Critical**
- **PoW bypass on replies** — proof-of-work verification was only enforced on new threads but not on replies. Replies now require a valid PoW nonce when the board has CAPTCHA enabled.
- **PoW nonce replay** — the same proof-of-work solution could be submitted repeatedly. Used nonces are now tracked in memory and rejected within their 5-minute validity window. Stale entries are automatically pruned.

**High**
- **Removed inline JavaScript** — all inline `<script>` blocks and `onclick`/`onchange`/`onsubmit` attributes have been extracted into external `.js` files. The Content Security Policy now uses `script-src 'self'` with no `unsafe-inline`, closing a major XSS surface.
- **Backup upload size cap** — the restore endpoints previously accepted uploads of unlimited size, risking out-of-memory crashes. Both full and board restore routes are now capped at 512 MiB.

### 🐛 Fixes
- **Post rate limiting simplified** — removed the global `check_post_rate_limit` function that was silently overriding per-board cooldown settings. A board with `post_cooldown_secs = 0` now correctly means zero cooldown. The per-board setting is the sole post rate control.
- **API endpoints excluded from GET rate limit** — hover-preview requests (`/api/post/*`) were being counted against the navigational rate limit, causing false throttling on threads with many quote links. All `/api/` routes are now excluded alongside `/static/`, `/boards/`, and `/admin/`. The GET limiter now only covers page loads that a scraper would target (board index, catalog, archive, threads, search, home).
- **Trailing slash 404s** — several routes returned 404 when accessed with or without a trailing slash (board index, catalog, archive, thread pages, post editing). Added middleware to normalize trailing slashes so all URL variations resolve correctly. Bookmarks and manually typed URLs now work as expected.

---

## [1.0.11] — 2026-03-06

### 🔒 Security Fixes

**Critical**
- Added security headers (CSP, HSTS, Permissions-Policy) to block XSS and enforce HTTPS
- Fixed IP detection behind reverse proxies — bans and rate limits now actually work with nginx
- Added rate limiting to all read-only pages (60 req/min) to prevent denial-of-service
- Added zip-bomb protection on backup restore (max 1 GB per entry, max 50,000 entries)
- IP addresses are now hashed everywhere — raw IPs never appear in logs or memory
- Admin login now locks out after 5 failed attempts with increasing delays
- CSRF token comparison is now timing-safe to prevent token guessing
- Poll inputs now have size limits (10 options max, 128 chars each, 256-char question)

**High**
- Admin session cookies now expire properly instead of lasting forever
- Database connections now time out after 5 seconds instead of hanging forever under load
- Small endpoints (login, vote, report) now reject oversized requests at 64 KB instead of buffering 50 MB
- Fixed a redirect trick on logout that could send users to malicious sites via backslash URLs
- Report and appeal handlers now correctly detect IPs behind proxies
- Background workers now retry with smarter backoff instead of all hammering the database at once
- Fixed a race condition where two identical file uploads at the same time could cause a server error

### ✨ New Features
- **Ban + Delete button** on every post in admin view — one click to ban the user and remove the post
- **Ban appeal system** — banned users can submit an appeal; admins can accept or dismiss from the panel
- **Proof-of-Work CAPTCHA** — optional per-board anti-spam for new threads, solved automatically in the browser (~100ms)
- **Video embeds** — YouTube, Invidious, and Streamable links show a thumbnail with a play button; click to load the video
- **Cross-board quote previews** — hovering `>>>/board/123` links now shows a floating preview popup
- **Floating "new replies" pill** — shows how many new posts arrived while you're reading; click to scroll down
- **Live thread metadata** — reply count, lock status, and sticky badges update in real time without refreshing
- **"(You)" badges** — your own posts are marked so you can easily spot replies to them
- **Spoiler text** — wrap text in `[spoiler]...[/spoiler]` to hide it until hover/click

### 🔄 Changed
- Board model now includes video embed settings; older backups still work fine

---

## [1.0.9] — 2026-03-06

### ✨ New Features
- **Per-board editing toggle** — enable or disable post editing on each board independently
- **Per-board edit window** — set how long users have to edit posts (default: 5 minutes)
- **Per-board archive toggle** — choose whether old threads are archived or permanently deleted

### 🐛 Fixes
- WebM files with AV1 video are now automatically re-encoded to VP9 for browser compatibility
- Fixed a video transcoding crash caused by conflicting encoding settings
- Fixed a compile error in the thread pruning code

---

## [1.0.8] — 2026-03-05

### ✨ New Features
- **Thread archiving** — old threads are now archived instead of deleted; browse them at `/{board}/archive`
- **Mobile reply drawer** — on phones, a floating reply button opens a slide-up panel instead of the clunky desktop form
- **Dice rolling** — type `[dice 2d6]` in a post to roll dice server-side; results are permanent and visible to everyone
- **Sage** — check "sage" when replying to post without bumping the thread
- **Post editing** — edit your own posts within 5 minutes using your delete token; edited posts show a timestamp
- **Draft autosave** — reply text is saved to your browser automatically; survives refreshes and crashes
- **WAL checkpointing** — automatic database maintenance to prevent log files from growing forever
- **Database vacuum button** — compact the database from the admin panel after bulk deletions
- **IP history** — admins can click any post to see all posts from that IP across all boards

---

## [1.0.7] — 2026-03-05

### ✨ New Features
- **EXIF stripping** — all uploaded JPEGs are scrubbed of metadata (GPS, device info, etc.) automatically
- **Image + audio combo uploads** — attach both an image and an audio file to the same post
- **Audio waveform thumbnails** — audio-only posts now show a generated waveform image instead of a generic icon

---

## [1.0.6] — 2026-03-04

### ✨ New Features
- **Backup management UI** — backups are now saved on disk and manageable from the admin panel (download, restore, or delete)
- **Board-level backup & restore** — back up or restore individual boards without affecting anything else
- **GitHub Actions CI** — automated builds and tests on macOS, Linux, and Windows

### 🐛 Fixes
- Fixed several compile errors related to random number generation, route syntax, and code formatting
- All routes updated to Axum 0.8 syntax

---

## [1.0.5] — 2026-03-04

### ✨ New Features
- **Auto WebM transcoding** — MP4 uploads are automatically converted to WebM when ffmpeg is available
- **Homepage stats** — total posts, uploads, and content size displayed on the front page

### 🐛 Fixes
- Tor detection now works on macOS (Homebrew paths)
- Audio file picker no longer hides audio files in the browser
- Audio size limit raised to 150 MB for lossless formats

---

## [1.0.4] — 2026-03-03

### ✨ New Features
- **Thread IDs** — every thread gets a permanent number displayed as a badge
- **Cross-board links** — link to other boards/threads with `>>>/board/123`
- **Emoji shortcodes** — 25 codes like `:fire:` → 🔥 and `:kek:` → 🤣
- **Spoiler tags** — hide text behind a black box, revealed on hover
- **Thread polls** — create polls with 2–10 options; one vote per IP, results shown as bar charts
- **Resizable images** — drag the corner of expanded images to resize them
- **Organized uploads** — files are now stored in per-board folders

### 🐛 Fixes
- Greentext styling now works correctly
- Spoiler CSS no longer broken by post styling
- Poll inputs no longer overflow on narrow screens

---

## [1.0.3] — 2026-03-03

### 🔄 Changed
- Binary renamed from `rustchan` to `rustchan-cli` to fix macOS case-sensitivity issues

### ✨ New Features
- Live upload progress bar in terminal
- Requests-per-second counter in stats
- Per-board thread/post counts
- Highlighted new activity with yellow `(+N)` indicators
- Active users count (unique IPs in last 5 minutes)
- Interactive admin console with keyboard shortcuts

---

## [1.0.2] — 2026-03-03

### ✨ New Features
- **Report system** — report posts with a reason; admins see an inbox with resolve/ban buttons
- **Moderation log** — all admin actions are permanently logged and viewable
- **Thread auto-updater** — toggle auto-refresh to see new replies without reloading
- **Background worker system** — video transcoding, waveform generation, and thread cleanup now happen in the background without slowing down requests
- **Client-side auto-compression** — if your file is too big, the browser offers to compress it for you before uploading

### 🎨 Theme Tweaks
- Frutiger Aero: toned down from electric blue to softer pearl-slate
- NeonCubicle: replaced eye-burning cyan with muted steel-teal

---

## [1.0.1] — 2026-03-03

### ✨ New Features
- **Theme picker** with 5 themes: Terminal (default), Frutiger Aero, DORFic Aero, FluoroGrid, NeonCubicle
- Choice saved in browser and applied instantly

### 🎨 Theme Tweaks
- FluoroGrid and DORFic redesigned for better readability

---

## [1.0.0] — 2026-03-03

### 🎉 Initial Release
- Imageboard with boards, threads, images, and video uploads
- Tripcodes and anonymous deletion tokens
- Admin panel with moderation and bans
- Rate limiting and CSRF protection
- Configurable via `settings.toml` or environment variables
- SQLite database with connection pooling
- Nginx and systemd deployment configs included
