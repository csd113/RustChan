# Changelog

All notable changes to RustChan will be documented in this file.

---
## [1.1.0]

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