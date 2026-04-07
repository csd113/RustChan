# Changelog

All notable changes to RustChan will be documented in this file.

## [1.1.3]

### Improved

- Homepage board cards and board catalog thread cards now keep a more consistent square visual rhythm: the main content rail is wider on desktop, homepage NSFW badges sit beside board IDs for faster scanning, and catalog size toggles once again distinguish compact and large thread cards while preserving more uniform tile heights.
- HTTP timeout handling is now more robust across the full request pipeline: `GET` and `HEAD` requests keep the fast 30-second cutoff, while slower write paths such as uploads, restores, and admin `POST`s are now covered by a longer request timeout instead of bypassing timeout protection entirely.
- Proxy-aware HTTPS detection is now stricter and operator-configurable: `X-Forwarded-*` headers are trusted only from explicitly allowed proxy CIDRs, with loopback remaining the safe default.
- Admin session cookie issuance is now wired through real connection metadata on login and restore flows, eliminating header-only protocol trust and keeping direct-access and proxied deployments aligned.
- HTTP to HTTPS redirects are now more robust on manual-certificate deployments bound to wildcard addresses, with explicit public-host configuration for production domains that are not discoverable from the local bind address.
- The shared site footer now stays pinned to the bottom of the viewport through a dedicated fixed-footer layout, while preserving the original homepage card grid and overall 1.1.2-style page flow.
- Theme CSS internals are cleaner and safer to maintain: the fixed footer now uses one shared height variable with safe-area-aware body padding, Frutiger Aero and NeonCubicle now share one glass-pill navigation implementation, and the Forest theme now centralizes repeated surface, link, button, and input colors behind theme-scoped variables.
- Mobile header polish is tighter on board pages: the search bar now stretches to the same visual rails as the Home and Boards controls instead of ending short on narrow screens.
- The theme picker now lives in a footer-docked control bar on both desktop and mobile, giving theme switching one consistent home and keeping it from floating over page content.
- Backup and media-processing observability are stronger: posts now expose pending and failed async media state, `/readyz` and `/metrics` report media backlog, backup freshness, and maintenance activity, and the admin panel surfaces backup verification health instead of assuming saved ZIPs are restorable.
- Heavy admin maintenance now coordinates through a shared maintenance gate and less aggressive background scheduling, so backups, restores, integrity checks, repair, and scheduled `VACUUM`/WAL work are less likely to pile onto live request traffic or each other.
- Full backup recovery is now more flexible without adding scheduler clutter: new full backups record the boards they contain, and the admin panel can derive a single-board restore or downloadable board backup directly from a saved full-site archive.

### Fixed

- Requests coming directly from untrusted public peers can no longer spoof `X-Forwarded-Proto` to make the app believe they arrived over HTTPS.
- Built-in self-signed TLS recovery is now resilient to partially missing or corrupted dev-cert files: if the stored cert/key pair cannot be reused, RustChan regenerates a fresh pair instead of failing startup outright.
- Timeout coverage no longer leaves upload-heavy and admin mutation endpoints outside the request-timeout middleware.
- Mobile layout resilience is stronger across the updated style system: the header board menu now follows the real wrapped header height instead of a fixed offset, admin board-settings forms collapse cleanly to one column on narrow screens, and wide admin tables stay usable on phones through horizontal scrolling.
- The admin panel is now substantially more mobile-friendly: dropdown headings wrap instead of running offscreen, board action controls stack cleanly on narrow screens, create-board and moderation forms fit the viewport, and the heaviest admin tables no longer force excessive horizontal overflow.
- Admin login is now more robust on plain `http://` deployments and local-network mobile access: insecure login redirects can recover through a short-lived bootstrap handoff instead of failing when the browser drops the freshly issued admin session cookie before `/admin/panel` loads.
- Admin login no longer fails with a `403` after the CSS refactor on plain `http://` deployments: the login page now reissues its CSRF cookie using the real request scheme so browsers do not drop the cookie before `/admin/login` is processed.
- Mobile media expansion behaves more predictably: tapping a video thumbnail now keeps playback inline on the page instead of collapsing back or jumping toward fullscreen, the filename remains the explicit open-in-new-tab path for fullscreen viewing, and image/video close buttons now use a smaller control footprint.
- Mobile image and video viewing now matches desktop more closely: the old floating media viewer has been removed, images and videos expand inline on the page with the same close-button flow as desktop, and the blue double-arrow/expand overlay is no longer shown over media on touch layouts.
- Desktop and mobile audio MiniPlayers now use the attached post image as album art for image+audio combo posts, while audio-only posts continue falling back to the current favicon artwork.
- Duplicate threads and replies are now prevented on unstable connections: post forms carry a per-render submission token, successful submissions are recorded server-side, and a retried POST now redirects back to the already-created post instead of inserting a second copy when the first response was lost in transit.
- Board search no longer fails when the FTS join exposes duplicate column names, and search queries are now normalized consistently so lowercase searches such as `ai` also match uppercase post text like `AI`.
- Background media processing now degrades more honestly under pressure: queue-capacity drops and permanent worker failures are persisted onto the post, failed previews fall back to the original file link, and operators can see pending and failed media work instead of silently missing thumbnails or waveforms.
- Saved backups are now verified before they are exposed as healthy: full backups include a manifest and SQLite-header checks, board backups are validated before save and in the admin listing, and backup freshness/verification status is now visible in both the admin UI and readiness metrics.
- Admin backup progress polling no longer conflicts with the maintenance lock during an active backup or restore, and failed backup builds now clean up temporary artifacts instead of leaving behind stale `.tmp` ZIPs or database snapshots.
- Saved full backups are now easier to work with on desktop and mobile: backup table actions stack more cleanly, per-row board extraction stays collapsed until needed, and older full backups without the new board index can still be extracted by entering a board short name manually.

### Validation

- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --quiet`

## [1.1.2]

### Added

- Shared board ordering controls, backed by a persistent `display_order` field, so admins can reorder boards once and see the same order reflected across the homepage, top header board list, and admin panel.
- Live upload progress bars for post media uploads and admin restore uploads, covering image/video/audio post forms plus full-site and per-board backup restore uploads from local files.
- Modular theme infrastructure backed by a runtime theme registry, database-managed theme records, dynamic `/theme-css/{theme}` delivery, and board-level default theme support so built-in and custom themes can be managed through one system.
- Admin theme management for enabling and disabling built-in themes, creating custom themes, editing custom theme metadata and CSS, deleting custom themes, and choosing both site-wide and per-board default themes.

### Improved

- Runtime data layout is now tidier under `rustchan-data/`, with backups grouped into `backups/full` and `backups/boards`, and generated operational state grouped under `runtime/` for Tor, TLS, favicon assets, and temporary admin files.
- Homepage admin board reordering is now available through a subtler per-card toggle instead of always-visible controls, keeping the feature accessible without cluttering the board list.
- Board navigation and admin ordering now split SFW and NSFW boards into separate groups, with independent per-group move controls and safer reordering when a board is retagged between normal and NSFW.
- Post headers now render subjects inline ahead of poster names, with theme-appropriate subject colors and separators so titles remain distinct from usernames across Terminal, DORFic, ChanClassic, Frutiger Aero, FluoroGrid, and NeonCubicle.
- Theme presentation is more polished through reordered theme-picker menus, softer ChanClassic header link contrast, and rounder shared controls in Frutiger Aero and NeonCubicle so top-level navigation matches those themes' bubbly styling better.
- Theme resolution, rendering, and picker behavior are now centralized around the live theme registry, so normal pages, admin pages, ban pages, JS bootstrap, no-JS fallbacks, startup seeding, and runtime cache refreshes all follow the same precedence rules.
- Theme picker and admin theme controls are now fully data-driven, so adding, renaming, disabling, or reordering themes no longer requires parallel hardcoded edits across Rust templates, handlers, and client JavaScript.
- Theme-related admin and test internals are leaner through one shared admin dashboard snapshot loader, one shared live-theme synchronization path, a unified CSS response path for built-in and custom themes, shared CSRF jar-check handling, and a reusable `Board` test fixture.
- Admin theme management is cleaner and easier to use through a redesigned themes panel layout, separate built-in and custom theme sections, clearer built-in/custom editing affordances, and a documented custom-theme starter scaffold that explains RustChan's scoped theme variables and common override selectors.
- Catalog page presentation is cleaner through centered sort/display selectors and larger board-description text on both board headers and homepage board cards.
- The admin site-settings layout is tidier, with the save button aligned into the form action row instead of floating awkwardly above the global favicon controls.
- Database maintenance is more user-friendly through a clearer integrity/repair results page and deeper admin repair tooling that now rebuilds SQLite indexes plus the `posts_fts` search table and triggers instead of only reporting a bare integrity status.

### Fixed

- Existing installs now migrate old runtime folders automatically at startup, so prior `full-backups`, `board-backups`, `arti_state`, `arti_cache`, `tls`, `favicon`, and temp backup-download directories continue working under the new layout without manual moves.
- Backup, Tor, TLS, favicon, admin UI, and documentation paths now consistently point at the reorganized filesystem structure instead of the older scattered folder names.
- Admin-panel live access addresses now wrap correctly on mobile instead of overflowing offscreen, and the console live-log renderer now avoids panic-prone slicing flagged by strict Clippy.
- The long-greentext collapse toggle now works as a true per-board setting instead of a global site-wide flag, with migration/backfill support for existing installs and backup/restore compatibility for the new board field.
- Client-side auto-compress is safer for oversized media: animated images are no longer silently flattened, transparent images avoid destructive JPEG fallback when the browser cannot preserve alpha, and video re-encoding now has stronger cleanup and timeout handling so the modal is less likely to get stuck.
- Board search no longer crashes on punctuation-heavy input such as `'`, `"`, or `>>1`; the search layer now normalizes free-form input into FTS-safe terms and returns ordinary empty results when nothing usable remains.
- Spoilers on legacy posts now keep working under the stricter CSP by upgrading older inline-click spoiler markup to the shared delegated `data-action` handler at runtime.
- Board backup restore now preserves archived-thread state, so threads that were already in a board archive stay archived after restore instead of being pulled back onto the live board index.
- Admin board delete and board restore now surface SQLite corruption failures more clearly, and the new integrity/repair tools are wired into the admin maintenance flow to help diagnose FTS/index corruption before destructive operations.
- Theme validation drift is eliminated: duplicated hardcoded theme lists, mismatched validators, and stale per-layer defaults were replaced with registry-backed validation and one canonical fallback path.
- Renaming or deleting custom themes now updates dependent site and board defaults safely instead of leaving stale references behind, and saved cookie or localStorage themes now fall back cleanly when a theme is disabled or removed.

## [1.1.1]

### Added

- Mobile-only board picker in the header, homepage NSFW consent overlay flow, and a no-JS theme fallback for slower or restricted browsers.
- Server-backed theme switching with explicit `return_to` routing and better backup/restore diagnostics across admin upload paths.
- Restore route request logging, board backup manifest inspection logs, and larger multipart restore coverage in the route test harness.
- Per-board archived-thread retention limit in the admin panel, with a default cap of `150` archived threads per board.
- Automatic favicon generation from a single `512x512` upload, with global site icons plus optional per-board favicon overrides.

### Improved

- Mobile interaction quality for reply, media expansion, archive rows, catalog controls, board descriptions, and header layout without changing the desktop interface.
- Poster ID chips on boards with IDs enabled now use stronger per-ID color separation so different posters are easier to tell apart without breaking theme compatibility.
- NSFW disclaimer copy and action-button styling now read more clearly across themes, including light-theme contrast improvements for the consent button.
- Audio posting UX now leads with an audio-first upload flow, clearer field labels, and an explicit optional cover-image slot instead of the previous mixed primary/secondary upload wording.
- Tor and mobile resilience through safer identity bucketing, less brittle theme persistence, JS-degraded fallbacks, and better cache revalidation for board, catalog, and thread pages.
- Generated `settings.toml` readability by regrouping settings into clearer related sections, and log organization by moving runtime logs into `rustchan-data/logs/`.
- Backup and restore internals by deduplicating board restore into one shared core and full-site restore into one shared execution path with rollback-aware filesystem swaps.
- Automatic archive trimming now deletes media only after the last remaining post reference is gone, so deduplicated uploads shared across multiple threads are preserved safely until truly unused.
- Admin favicon controls now use a compact inline layout with live previews and clearer replace/clear actions for both global and board-specific icons.

### Fixed

- Mobile photo uploads now preserve correct orientation for both stored images and generated thumbnails.
- Admin archive, pin, thread deletion, board restore, and full restore flows now refresh more reliably without requiring manual cookie or cache clearing.
- Firefox and localhost admin restore uploads no longer fail on `Origin: null` or loopback host alias mismatches when valid session and CSRF state are present.
- Linked image+audio posts now render as one combined media block, use the uploaded image as the audio thumbnail, autoplay the attached song when the image is expanded, keep playing when the image is collapsed, and size the audio seek bar to the same width as the linked image.
- Secondary combo-audio uploads now preserve FLAC bytes as-is without FFmpeg re-encoding, while still reusing the linked image thumbnail for the post presentation.
- Theme picker, board menu, catalog sort controls, and top-bar alignment no longer overflow or misplace themselves on mobile and Tor Browser.
- Thumbnail hover and click hitboxes no longer stretch left of the visible image after closing expanded media.
- OP quotelinks now render the `(OP)` marker with tighter spacing so they display as `>>123 (OP)` instead of looking over-separated.
- Backup/restore logging now respects the app’s actual tracing targets instead of being silently filtered out.
- Board index, catalog, and thread tab titles now use clearer board-aware formatting, and full-site restore no longer wipes the current global favicon when restoring an older backup that did not include favicon data.

### Validation

- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -- -D warnings -W clippy::all -W clippy::pedantic -W clippy::nursery`
- `env -u RUSTC_WRAPPER cargo test --quiet`

## [1.1.0]

### Added

- ChanNet API for federation and RustWave gateway commands on port `7070`.
- Full-screen operator dashboard with live stats, logs, boards, shortcuts, and setup flows.
- Native HTTPS support with self-signed and Let's Encrypt options, plus optional HTTP to HTTPS redirects and HSTS.
- Stronger Tor support with per-stream isolation, Tor-only mode, better startup and shutdown handling, and `Onion-Location`.
- Optional arbitrary file uploads with safe download-only handling for non-media files.

### Improved

- Faster board search, batched thread previews, cached thread updates, and lower job-queue overhead.
- Safer posting, polling, replies, restores, uploads, and ChanNet imports through better transactions and rollback handling.
- Cleaner internals across server, admin, backup, middleware, media, and schema code, with a new in-memory route test harness.
- Better operator tooling with `/healthz`, `/readyz`, `/metrics`, `X-Request-ID`, cleaner logs, and more reliable FFmpeg and bind-address handling.

### Fixed

- Proxy-aware IP handling now blocks spoofed `X-Real-IP` and `X-Forwarded-For` values from untrusted clients.
- Rate limiting now covers more write and preview paths, closing easy abuse and DoS gaps.
- HTTPS deployments now enforce secure cookies, safer redirects, and more consistent HSTS behavior.
- Restore, upload, temp-file, and background-job edge cases were cleaned up to avoid partial state, stuck jobs, and unsafe paths.
- Admin feedback, upload-disabled UI, error messages, and login logging are now more consistent and safer.

### Security

- Restore validation, upload serving, backup handling, and appeal flows were tightened to reduce traversal, duplication, and data-leak risks.
- `Onion-Location`, CAPTCHA wording, and HTTPS documentation now match real runtime behavior.

### Breaking Changes

- HTTP to HTTPS redirects now use configured and trusted hosts instead of echoing arbitrary `Host` headers.

### Validation

- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -- -D warnings -W clippy::all -W clippy::pedantic -W clippy::nursery`
- `cargo test`


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
