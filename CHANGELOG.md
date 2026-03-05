# Changelog

All notable changes to RustChan will be documented in this file.

## [1.0.6] — 2026-03-04

### Added
- **Board-level backup** — each board in the admin panel now has a
  `⬓ backup /board/` button that downloads a self-contained zip of that
  board's data (`board.json` manifest + all uploaded files). Only the
  selected board is included; other boards are untouched.
- **Board-level restore** — new "// board backup & restore" section in the
  admin panel accepts a board backup zip and restores the board in-place.
  If the board already exists its content is wiped and replaced; if it does
  not exist it is created from scratch. All row IDs are remapped on import
  so there is no risk of collision with data already in the database.
  Other boards, bans, filters, and admin accounts are never affected.
- **`serde_json` dependency** — added to support the JSON manifest format
  used by board-level backups.
- **GitHub Actions CI workflow** (`.github/workflows/rust.yml`) — runs
  `cargo build` and `cargo test` on every push and pull-request across five
  targets: `macos-x86_64`, `macos-arm64`, `linux-x86_64`, `linux-arm64`
  (cross-compiled via `cross-rs`), and `windows-x86_64`. Clippy and
  `rustfmt` checks run on the Linux x86_64 job.

### Fixed
- **`rand_core::OsRng` compile error** — enabled the `getrandom` feature on
  `rand_core = "0.6"` so `OsRng` is available in `src/config.rs` and
  `src/utils/crypto.rs` (`E0432` / `E0425`).
- **Axum 0.8 route syntax** — migrated all route capture groups from the
  deprecated `/:param` syntax to the required `/{param}` syntax, fixing a
  runtime panic on startup (`Path segments must not start with ':'`).
- **`E0597` lifetime errors in board backup** — the six `rusqlite` query
  blocks in `board_backup` now collect results into a named `rows` binding
  before the enclosing block closes, ensuring `Statement` (`s`) is not
  dropped while `MappedRows` still holds a borrow.

## [1.0.5] - 2026-03-04

### Added
- **Automatic WebM transcoding** — when ffmpeg is present, all uploaded MP4 files are automatically transcoded to WebM (VP9 + Opus) before being saved. Already-WebM uploads are kept as-is. If ffmpeg is unavailable or transcoding fails, the original MP4 is saved as a fallback with a warning logged.
- **Home page stats section** — the index page now shows a `// Stats` panel at the bottom with five live counters: total posts, lifetime images uploaded, lifetime videos uploaded, lifetime audio files uploaded, and total size of active content in GB.

### Fixed
- **Tor detection on Homebrew** — the startup probe now checks `/opt/homebrew/bin/tor` (Apple Silicon) and `/usr/local/bin/tor` (Intel Mac) in addition to bare `tor` on PATH. Also changed from `.success()` to `.is_ok()` to handle tor builds that exit with code 1 for `--version` even when installed correctly.
- **Audio uploads blocked in browser** — the file input `accept` attribute was missing all audio MIME types, causing the OS file picker to hide audio files entirely. All audio types are now listed (`audio/mpeg`, `audio/ogg`, `audio/flac`, `audio/wav`, `audio/mp4`, `audio/aac`, `audio/webm`) along with their extensions as a fallback.
- **Audio size limit** — default `max_audio_size_mb` raised from 16 → 150 to accommodate lossless formats such as FLAC.
- **Audio size not shown in UI** — the file hint row below the upload input now includes audio formats and their size limit alongside the existing image and video hints.
- **Dead-code warning on `MediaType::from_ext`** — added `#[allow(dead_code)]` to suppress the compiler warning for this migration-use function.
- **Stats section letter-spacing** — removed `letter-spacing` from `.index-stat-value` (CSS letter-spacing adds a trailing gap after the last character, breaking number alignment) and reduced label tracking from `0.08em` to `0.04em`.

## [1.0.4] - 2026-03-03

### Added
- **Thread IDs** — every thread is now assigned a permanent numeric ID displayed as a badge (`Thread No.1234`) at the top of its page. Board index thread summaries show a clickable `[ #1234 ]` link beside each post number.
- **Cross-board links** — post bodies now parse `>>>/board/123` into a clickable link to that thread and `>>>/board/` into a board index link. Cross-board links are styled in amber to distinguish them from local reply links.
- **Emoji shortcodes** — 25 shortcodes supported in post bodies (e.g. `:fire:` → 🔥, `:think:` → 🤔, `:based:` → 🗿, `:kek:` → 🤣). Applied after HTML transforms to avoid conflicts.
- **Spoiler tags** — `[spoiler]text[/spoiler]` hides content behind a same-color block; clicking or hovering reveals it with a smooth transition.
- **Markup hint bar** — a compact row of syntax reminders is shown below the body textarea in the new thread form listing available markup options.
- **Thread polls** — the new thread form includes a collapsible `[ 📊 Add a Poll ]` section. Polls are OP-only, support 2–10 options (dynamically added/removed), and require a duration in hours or minutes (clamped to 1 minute–30 days). Votes are cast via a radio-button form, one vote per IP enforced at the database level. Results display as a percentage bar chart after voting or once the poll closes. Polls are anchored at `#poll` on their thread page.
- **Resizable expanded images** — expanded images support `resize: both`, allowing users to drag the corner to any size without reloading.
- **Per-board upload directories** — files are now stored under `rustchan-data/boards/{board}/` and thumbnails under `rustchan-data/boards/{board}/thumbs/` for clean per-board organisation.

### Changed
- **Data directory renamed** from `chan-data/` to `rustchan-data/` for clarity.
- **Upload directory renamed** from `uploads/` to `boards/` inside the data directory. The static file route changed from `/uploads/` to `/boards/` accordingly.
- **Bold** (`**text**`) and **italic** (`__text__`) markup now render correctly in all post bodies.

### Fixed
- Greentext CSS class mismatch — renderer emits `class="quote"` but the stylesheet only targeted `.greentext`; both are now covered.
- Spoiler CSS specificity — `.post-body` color was overriding the spoiler hide rule; selectors updated to `.post-body .spoiler`.
- Poll "Question" input overflowing the form on narrow layouts — label and input now use `width: 100%; box-sizing: border-box` and `min-width: 0`.

---

## [1.0.3] - 2026-03-03

### Changed
- **Binary renamed** from `rustchan` to `rustchan-cli` to avoid filesystem conflicts with the `RustChan/` source directory on case-insensitive filesystems (macOS).

### Added
- **Dynamic upload progress bar** — while a file upload is in progress, a live spinner and pulsing bar are shown in the terminal stats output (e.g. `⠹ UPLOAD  [██████░░░░]  2 file(s) uploading`).
- **Requests per second counter** — the stats line now includes a live req/s figure computed over the interval since the last tick (e.g. `4.5 req/s`).
- **Board-specific stats** — below the main stats line, per-board thread and post counts are shown (e.g. `/b/  threads:12 posts:89  │  /tech/  threads:5 posts:22`).
- **New-event highlighting** — when the stats tick detects newly created threads or posts since the last check, those counts are printed in bold yellow with a `(+N)` delta indicator.
- **Active connections / users online** — the stats output now shows the count of unique client IPs active within the last 5 minutes and lists up to 5 of them (e.g. `users online: 3  │  IPs: 192.168.1.2, 192.168.1.5`).
- **Keyboard-driven admin console** — an interactive prompt is available while the server is running. Commands: `[s]` show stats, `[l]` list boards, `[c]` create board, `[d]` delete thread, `[u]` clear thumbnail cache, `[h]` help, `[q]` quit hint.

---

## [1.0.2] - 2026-03-03

### Changed
- **Frutiger Aero**: Softened the background gradient from saturated electric sky-blue to a cooler, more muted pearl-slate. Border glow pulled back from `#38b6ff` to a dusty steel blue (`#6aaed6`). Glass panels now feel frosted rather than bright. Button styles added to match the new palette.
- **NeonCubicle**: Replaced blinding pure cyan (`#0FF0FF`) borders and hot magenta (`#FF00AA`) accents with muted steel-teal borders and a softer dusty rose/orchid for accents. Lavender panels desaturated slightly. Scanlines dialed back to 7% opacity.

---

## [1.0.1] - 2026-03-03

### Added
- Theme picker button fixed to the bottom-right corner of every page. Clicking it opens a panel with five selectable themes; the choice is persisted in `localStorage` and applied on load with no flash.
  - **Terminal** (default) — dark matrix-green monospace aesthetic.
  - **Frutiger Aero** — glossy sky-blue gradients, glassy panels with backdrop-filter blur, rounded corners, Segoe UI font.
  - **DORFic Aero** — dark hewn-stone background with warm amber/copper glassmorphic panels and torchlit glow. Underground fortress meets Vista-era frosted glass.
  - **FluoroGrid** — pale sage background with muted teal grid lines, dusty lavender panels, and plum accents evoking a fluorescent-lit 80s office.
  - **NeonCubicle** — off-white with horizontal scanlines, lavender panels, cyan borders, and hot magenta accents.

### Changed
- **FluoroGrid**: Softened from pure cyan/magenta to muted teal borders and dusty plum accents for a more comfortable reading experience.
- **DORFic**: Fully redesigned as *DORFic Aero* — dark stone walls, amber glass panels, copper glow borders, parchment text.

---

## [1.0.0] - 2026-03-03

Initial release.

### Features
- Imageboard-style boards with threaded posts and image/video uploads
- Tripcodes and secure deletion tokens for anonymous users
- Admin panel with board management, post moderation, and ban system
- Rate limiting and CSRF protection
- Configurable via `settings.toml` or environment variables
- SQLite backend with connection pooling
- Nginx and systemd deployment configuration included
