# Changelog

All notable changes to RustChan will be documented in this file.

## [1.0.10] — 2026-03-06

### Added
- **Video embed unfurling** — when a post body contains a YouTube, Invidious, or
  Streamable URL, the markup parser now emits a `<span class="video-unfurl">`
  placeholder alongside the hyperlink, carrying `data-embed-type` and
  `data-embed-id` attributes.  On thread pages a new client-side script replaces
  each placeholder with a thumbnail + circular play button; clicking it swaps in
  the embedded iframe with autoplay.  YouTube thumbnails are loaded directly from
  `img.youtube.com`; Streamable shows a labelled placeholder until clicked.
  Invidious instances are detected by the standard `?v=` query parameter on any
  non-YouTube domain, so any self-hosted instance is automatically supported.  The
  feature is opt-in at the board level via a new **"Embed video links"** checkbox
  in the admin board-settings panel.  The embed JS is a no-op when the board flag
  is off, and the placeholder spans in existing `body_html` are simply hidden by
  CSS, so toggling the flag does not require re-rendering stored posts.  Backed by
  a new `allow_video_embeds` column on the `boards` table (default `0`) added via
  an additive SQLite migration.
- **Cross-board quotelink hover previews** — `>>>/board/123` links were previously
  rendered as styled amber anchors with no interactive preview.  They now carry
  `data-crossboard` and `data-pid` attributes and are wired by a new client-side
  script that fetches `GET /api/post/{board}/{thread_id}` on hover, renders the OP
  post in the same floating popup used by same-thread `>>N` quotelinks, and caches
  results for the lifetime of the page so repeat hovers are instant.  A loading
  placeholder is shown while the fetch is in flight; a terse error message is
  shown for non-existent threads.  The new `GET /api/post/{board}/{thread_id}`
  endpoint is rate-limited by the existing middleware and returns JSON
  `{"html":"…"}` containing the server-rendered OP post (thumbnail included,
  delete/admin controls stripped).  A new `db::get_op_post_for_thread` DB
  function powers the lookup.  The cross-board popup shares the popup `div` and
  positioning logic already used by same-thread quotelinks, so all five visual
  themes render correctly without additional CSS.
- **Spoiler text markup** — `[spoiler]text[/spoiler]` tags were already parsed by
  the markup pipeline and confirmed to produce `<span class="spoiler">` with CSS
  `background == color` (text invisible at rest, revealed on hover or click via
  `.spoiler:hover` / `.spoiler.revealed`).  No code change was required; this entry
  documents that the feature is fully implemented, tested (`test_spoiler` passes),
  and safe against XSS (the `[` and `]` delimiters survive `escape_html` unchanged
  because they are not HTML-special characters, and the rendered content is already
  escaped before the spoiler regex runs).
- **Floating new-reply pill** — when the auto-updater fetches new posts, a
  floating pill reading "+N new replies ↓" fades in over the thread.  Clicking
  it smooth-scrolls to the bottom of the page and dismisses the pill.  The pill
  also auto-dismisses when the user scrolls within 200 px of the bottom, or
  after 30 seconds.  This replaces reliance on the small status span in the nav
  bar, which was easy to miss — directly equivalent to 4chan X's new-post
  notification pill.
- **Delta-compressed thread state in the auto-update endpoint** — the
  `GET /:board/thread/:id/updates?since=N` response now carries a richer JSON
  envelope: `reply_count`, `bump_time`, `locked`, and `sticky` alongside the
  existing `html`/`last_id`/`count` fields.  The client consumes these to keep
  the nav-bar reply counter and lock/sticky badges in sync without a full page
  reload.  A new `R: N` reply counter has been added to the thread nav bar and
  is updated live on every poll cycle.  If the thread becomes locked while the
  user is watching, a lock notice is injected above the posts automatically.
- **"(You)" post tracking** — post IDs submitted by the current browser are
  persisted in `localStorage` under a per-thread key and survive page refreshes.
  A subtle `(You)` badge is rendered next to the post number of every post you
  authored, making it easy to spot replies to your own posts.  The mechanism
  works by setting a `rustchan_you_pending_<board>_<thread>` flag before the
  reply form submits; on the redirect landing, the post ID is extracted from
  the URL fragment and saved.  Badges are also re-applied whenever the
  auto-updater inserts new posts.

### Changed
- **`Board` model** — one new field: `allow_video_embeds: bool` (default `false`).
  All DB queries reading or writing board rows have been updated.  Board backup /
  restore manifests include the new field so the setting survives a round-trip;
  older backup zips that pre-date the field default it to `false` on restore via
  `#[serde(default)]`.

## [1.0.9] — 2026-03-06

### Added
- **Per-board post editing toggle** — each board now has an `allow_editing`
  flag (off by default) that gates whether users can edit their own posts.
  When disabled the edit link is hidden and the edit endpoints return an error
  immediately, regardless of the global edit-window logic.  The flag is
  exposed as a checkbox in the admin board-settings form (*Enable editing*).
- **Per-board edit window** — a companion `edit_window_secs` column on the
  `boards` table lets operators configure how long after posting a user may
  edit their own post on a per-board basis.  Setting it to `0` falls back to
  the server-wide default of 300 s (5 minutes).  The value is shown in the
  admin board-settings form as a number input (*Edit window (s)*) and is
  respected by both the edit-form handler and the edit-submit handler.
- **Per-board archive toggle** — a new `allow_archive` column on the `boards`
  table (default `1` on existing rows, i.e. archiving enabled) lets operators
  choose, per board, whether overflow threads are archived or permanently
  deleted when the board hits its `max_threads` limit.  The `ThreadPrune`
  background worker now reads this flag from the job payload and calls either
  `db::archive_old_threads` or `db::prune_old_threads` accordingly.  The
  admin board-settings form exposes this as a checkbox (*Enable archive*).

### Fixed
- **WebM AV1 → VP9 transcoding** — uploaded WebM files containing an AV1
  video stream are now detected via `ffprobe` and re-encoded to VP9 + Opus
  by the `VideoTranscode` background worker.  Previously, all WebM uploads
  were accepted as-is regardless of codec, meaning AV1 content would be
  served to browsers that do not support it.  VP8 and VP9 WebM files are
  identified and skipped cheaply so they are never unnecessarily re-encoded.
- **VP9 CRF rate-control conflict** (`exit status: 234`) — the `ffmpeg`
  transcode command previously combined `-b:v 0` (pure CRF mode) with
  `-maxrate 2M -bufsize 4M` (constrained-quality mode).  libvpx-vp9 treats
  these as mutually exclusive: setting a peak bitrate cap without a target
  bitrate causes the encoder to abort with *"Rate control parameters set
  without a bitrate"*.  The `-maxrate` and `-bufsize` flags have been
  removed; the transcoder now uses pure CRF 33 with unconstrained average
  bitrate (`-b:v 0`), which is the correct mode for quality-driven encoding.
- **`E0597` borrow lifetime in `db::prune_file_paths`** — the
  `stmt.query_map(…).collect()` expression at the end of a block created a
  temporary `MappedRows` iterator that outlived `stmt` (dropped at the
  closing brace), causing a compile error.  The result is now collected into
  an explicit `let` binding before the block ends, ensuring the iterator is
  fully consumed while `stmt` is still in scope.

### Changed
- **`Board` model** — three new fields: `allow_editing: bool`,
  `edit_window_secs: i64`, and `allow_archive: bool`.  All DB queries that
  read or write board rows have been updated accordingly.  Board backup /
  restore manifests also include these fields so settings survive a
  round-trip.

## [1.0.8] — 2026-03-05

### Added
- **Thread archiving** — when a board hits its `max_threads` limit, the oldest
  non-sticky threads are now moved to an *archived* state instead of being
  deleted.  Archived threads gain `archived = 1, locked = 1` in the database:
  they remain fully readable and are kept forever, but no new replies can be
  posted to them and they do not appear in the board index or catalog.  A new
  `GET /{board}/archive` page lists all archived threads for a board with
  pagination (20 per page), newest-bumped first, showing a thumbnail, subject,
  body preview, reply count, and creation date.  The archive is linked from the
  sticky catalog bar that appears on every board page.  A new
  `db::archive_old_threads` function replaces the old `prune_old_threads`; the
  background worker (`ThreadPrune` job) now calls it instead of deleting.  An
  additive SQLite migration adds the `archived INTEGER NOT NULL DEFAULT 0`
  column to `threads` and a covering index
  `idx_threads_archived(board_id, archived, bumped_at DESC)`.  All existing
  board-index and catalog queries gain `AND t.archived = 0` so they are
  unaffected by archived rows.  The `Thread` model gains an `archived: bool`
  field that is populated everywhere a thread row is mapped from the database.
- **Mobile-optimised reply drawer** — on viewports ≤ 767 px the desktop
  inline reply form toggle is hidden and replaced with a floating action button
  (FAB) fixed to the bottom-right corner labelled *✏ Reply*.  Tapping it
  slides a full-width drawer up from the bottom of the screen (max-height 80 vh,
  scroll-overflow enabled) containing the reply form.  A close button in the
  drawer header (✕) collapses it.  The `appendReply(id)` function that
  populates the `>>N` quote when tapping a post number is media-query aware: on
  mobile it opens and populates the drawer textarea rather than the desktop
  form.  All behaviour is implemented with vanilla JS and a `@media
  (max-width: 767px)` CSS block — no external dependencies.  The drawer slides
  with a CSS `transform: translateY` transition (0.22 s ease) and the FAB fades
  out while the drawer is open to avoid overlap.
- **Server-side dice rolling** — posts may now include `[dice NdM]` anywhere
  in their body (e.g. `[dice 2d6]`, `[dice 1d20]`).  The server rolls the
  dice using `OsRng` at the moment the post body is processed through
  `render_post_body`, and the result is embedded immutably in `body_html` so
  every reader sees the same rolls forever.  The rendered output is a `<span
  class="dice-roll">` element showing the notation, individual die results, and
  sum: e.g. `🎲 2d6 ▸ ⚄ ⚅ = 11`.  For d6 rolls each individual die is
  displayed as the corresponding Unicode die-face character (⚀–⚅); for all
  other dice sizes the value is shown as `【N】`.  Limits: 1–20 dice, 2–999
  sides; out-of-range values are clamped silently.  The feature is implemented
  entirely in `utils/sanitize.rs` as a pre-pass regex substitution inside
  `render_post_body` using `rand_core::OsRng` (already a transitive
  dependency) — no new dependencies are added.
- **Post sage** — the reply form now includes a *sage* checkbox. When checked,
  the reply is posted normally but does not bump the thread's `bumped_at`
  timestamp, so it does not rise in the board index regardless of its reply
  count relative to the bump limit.  Sage is parsed as a standard multipart
  checkbox field (`name="sage" value="1"`), stored nowhere server-side (it
  only controls whether `db::bump_thread` is called), and is a no-op when
  posting a new thread.  The label is rendered in a dimmed style with a brief
  "(don't bump thread)" hint to match the classic imageboard convention.
- **Post editing** — users may edit their own post within a 5-minute window
  after it was created, authenticated by the same deletion token they set (or
  were assigned) at post time.  A small *edit* link appears next to the delete
  form on every post while the window is open; clicking it navigates to
  `GET /{board}/post/{id}/edit`, which shows the current post body in a
  pre-filled textarea alongside a deletion-token input.  Submitting the form
  (`POST /{board}/post/{id}/edit`) verifies the token with constant-time
  comparison, re-validates and re-renders the body through the same word-filter
  and HTML-sanitisation pipeline as a normal post, then writes the updated
  `body`, `body_html`, and an `edited_at` Unix timestamp to the database.
  Invalid tokens or expired windows display an inline error without losing the
  typed text.  After a successful edit the user is redirected back to the post
  anchor in the thread.  An *(edited HH:MM:SS)* badge is appended to the
  post-meta line of any post whose `edited_at` is not NULL, with the full
  timestamp in the title attribute.  The feature is backed by an additive
  SQLite migration (`ALTER TABLE posts ADD COLUMN edited_at INTEGER`) and a
  new `db::edit_post` function that enforces the window check atomically.
  `EDIT_WINDOW_SECS = 300` is a public constant in `db.rs` for easy tuning.
- **Draft autosave** — the reply textarea contents are automatically
  persisted to `localStorage` every 3 seconds under the key
  `rustchan_draft_{board}_{thread_id}`.  On page load the saved draft is
  restored into the textarea so a refresh, accidental navigation, or browser
  crash does not lose a half-written reply.  The draft is cleared when the
  reply form is submitted.  All localStorage access is wrapped in try/catch so
  environments with storage disabled (e.g. private-browsing with strict
  settings) fail silently.  The script is injected once per thread page and
  does not affect new-thread forms or any other page type.
- **WAL checkpoint tuning** — a background Tokio task now runs
  `PRAGMA wal_checkpoint(TRUNCATE)` at a configurable interval to prevent
  SQLite's write-ahead log from growing unbounded under sustained write load.
  TRUNCATE mode performs a full checkpoint and then resets the WAL file to
  zero bytes, reclaiming disk space immediately.  The interval is set via
  `wal_checkpoint_interval_secs` in `settings.toml` (default: 3600, i.e.
  hourly) or the `CHAN_WAL_CHECKPOINT_SECS` environment variable; set to
  `0` to disable entirely.  The task is staggered to fire at half the
  configured interval after startup so it does not overlap with the session
  purge task.  Checkpoint pages/moved/backfill counts are logged at DEBUG
  level; failures are logged as warnings and do not crash the server.
- **SQLite VACUUM endpoint** — a new *"// database maintenance"* section in
  the admin panel shows the current database file size and provides a
  `POST /admin/vacuum` button that runs `VACUUM` to compact the database
  after bulk deletions.  The button requires a CSRF-token-protected form
  submission and an active admin session.  On completion a result page is
  shown with the before/after file size and the number of bytes reclaimed
  (and the percentage reduction).  The `db::get_db_size_bytes` helper
  (using `PRAGMA page_count * page_size`) and `db::run_vacuum` are exposed
  as public DB functions for use by any future tooling.
- **IP history view** — every post rendered in an admin session now has an
  `&#x1F50D; ip history` link beside the admin-delete button.  Clicking it
  opens `GET /admin/ip/{ip_hash}`, which lists every post that IP hash has
  ever made across all boards, newest first, with pagination (25 per page).
  Each row shows the timestamp, a clickable link to the exact post in its
  thread, an OP badge when applicable, a media type indicator, a 120-char
  body preview, and an inline admin-delete button.  The IP hash path
  component is validated (must be alphanumeric, ≤ 64 chars) to prevent
  information leakage through crafted URLs.  Two new DB functions support
  this: `count_posts_by_ip_hash` and `get_posts_by_ip_hash`.

## [1.0.7] — 2026-03-05

### Added
- **JPEG EXIF stripping** — all uploaded JPEG files are now re-encoded
  server-side through the `image` crate before being written to disk.
  Re-encoding produces a clean JFIF stream that contains only pixel data —
  no EXIF, XMP, IPTC, GPS coordinates, device serial numbers, or any other
  embedded metadata survive the process.  The stripping is transparent to
  users; image quality is preserved at the crate's default JPEG output
  quality (90).  If re-encoding fails for any reason (corrupt JPEG, OOM)
  the original bytes are saved instead and a warning is logged.
- **Multiple file attachments (image + audio combo)** — on boards where
  both images and audio are enabled, a second *"audio (+ image)"* file
  input is now shown in the new-thread and reply forms.  Submitting both
  fields simultaneously stores the image as the primary post file and the
  audio as a secondary attachment.  Only the image+audio combination is
  supported; other multi-file mixes (e.g. two images, video+audio) are
  rejected with a clear error message.  The secondary audio file is stored
  in four new database columns (`audio_file_path`, `audio_file_name`,
  `audio_file_size`, `audio_mime_type`) added via additive SQLite
  migrations, so existing databases are upgraded automatically on first
  startup.  Deleting a post also cleans up its secondary audio file.
- **Audio waveform thumbnail** — when a standalone audio file is uploaded
  (without a companion image) and ffmpeg is available, a static waveform
  PNG is now generated as the thumbnail instead of a generic SVG music-note
  icon.  The waveform is rendered using ffmpeg's `showwavespic` filter at
  `thumb_size × (thumb_size / 2)` pixels with the board's terminal-green
  colour scheme.  Falls back to the SVG placeholder if ffmpeg is
  unavailable or waveform generation fails.  When an audio file is
  uploaded *alongside* an image (the combo feature above), the image's
  thumbnail is reused as the audio's thumbnail — no separate waveform is
  generated for that case.

## [1.0.6] — 2026-03-04

### Added
- **Disk-based backup storage** — full backups are now saved to
  `rustchan-data/full-backups/` and board backups to
  `rustchan-data/board-backups/` on the server, keeping everything inside
  the database folder without requiring access to the file explorer.
- **In-panel backup management** — the admin panel's backup & restore
  sections now show a live table of all saved backup files (filename, size,
  creation time). Each row provides three actions accessible directly from
  the web interface without leaving the page:
  - **⬇ download** — stream the saved `.zip` to the browser.
  - **↺ restore** — restore the live database directly from the saved file
    on disk (no upload required).
  - **✕ delete** — permanently remove the backup file from the folder, with
    a confirmation prompt.
- **`POST /admin/backup/create`** — creates a full backup and saves it to
  `rustchan-data/full-backups/`; replaces the previous stream-to-browser
  GET endpoint for the in-panel workflow (old `GET /admin/backup` kept for
  backward compatibility).
- **`POST /admin/board/backup/create`** — saves a board backup to
  `rustchan-data/board-backups/` from the board card's new *save backup*
  button.
- **`GET /admin/backup/download/{kind}/{filename}`** — authenticated
  download of any saved backup by filename (`kind` = `full` or `board`).
  Filenames are validated to prevent path traversal.
- **`POST /admin/backup/delete`** — authenticated deletion of a backup
  file; physically removes the `.zip` from the appropriate folder.
- **`POST /admin/backup/restore-saved`** — restore a full backup from a
  file already saved on disk, without needing to re-upload the zip.
- **`POST /admin/board/backup/restore-saved`** — restore a single board
  from a saved board backup file on disk.
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
- **`rustfmt` compliance** — removed alignment whitespace throughout the
  codebase to pass `cargo fmt --check` in CI. Specific changes:
  - `src/config.rs`: removed column-aligned trailing `// bytes` comments on
    `max_image_size`, `max_video_size`, `max_audio_size` struct fields;
    removed extra space in `(max_video_mb  as usize)` and
    `(max_audio_mb  as usize)` expressions in `Config::from_env`.
  - `src/db.rs`: removed extra spaces before `// ignore "duplicate column"
    errors` inline comment.
  - `src/detect.rs`: removed extra spaces before inline comment on
    `.is_ok()` call.
  - `src/models.rs`: removed column-aligned trailing comments on `Board`
    struct fields (`short_name`, `name`, `allow_images`, `allow_video`,
    `allow_audio`, `created_at`) and `Post` struct field (`body_html`).
  - `src/handlers/board.rs`: removed extra spaces before inline comment on
    `thread_id: 0` struct field initialiser.
  - `src/handlers/admin.rs`: removed alignment padding in two
    `(header::CONTENT_TYPE, ...)` tuple literals (in `admin_backup` and
    `board_backup`); removed double space before `=` in two
    `let mut form_csrf: Option<String>  = None` declarations (in
    `admin_restore` and `board_restore`).

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
- **Report system** — every post now shows a small *report* button next to the
  delete form.  Clicking it opens a modal overlay where the user can optionally
  type a reason and submit with a single click.  Reports are written to a
  `reports` table (post_id, thread_id, board_id, reason, reporter IP hash,
  status) and persist across restarts.  The admin panel gains a *// report inbox*
  section at the top, showing all open reports with a content preview, the
  reason, and two quick-action buttons: **resolve** (mark closed, no further
  action) and **resolve + ban** (permanently bans the post author's IP hash in
  one click).  The report count is shown as a red badge in the section heading
  so the inbox is immediately visible even on a long panel page.  The route
  `POST /report` is CSRF-protected and validates that the post belongs to the
  named board before writing to the DB.

- **Moderation log** — every admin action now appends a row to a new `mod_log`
  table recording the admin's username, the action name
  (`delete_post`, `delete_thread`, `ban`, `sticky`, `lock`, `resolve_report`,
  …), the target type and ID, the board, and an optional detail string
  (e.g. a truncated post body preview or ban reason).  The mod log is viewable
  at `GET /admin/mod-log` as a paginated table (50 entries per page), linked
  from the admin panel under *// moderation log — view full log*.  Actions are
  logged in every spawn_blocking handler that mutates content, using a
  `require_admin_session_with_name` helper that returns both the admin_id and
  username in a single DB lookup.  The `mod_log` table is append-only —
  nothing in the UI can delete log entries.

- **Thread auto-update toggle** — a checkbox labelled *auto-update every 30s*
  appears at the bottom of every thread page (off by default).  When enabled,
  the browser polls `GET /{board}/thread/{id}/updates?since={last_post_id}`
  every 30 seconds.  The endpoint returns JSON `{html, last_id, count}`: the
  rendered HTML of any new posts, the highest post ID seen, and the count of
  new posts appended.  New posts are rendered without user-delete controls or
  admin controls to keep the response stateless (a full page reload restores
  all controls).  New HTML is appended to a `<div id="thread-posts">` container
  so existing posts are never re-rendered.  The `data-last-id` attribute on
  that container is updated after each successful fetch so consecutive polls
  are cumulative.  A status line next to the toggle shows the last check time
  and how many new replies arrived.  The JS runs inside an IIFE with no global
  pollution; `clearInterval` is called when the toggle is unchecked.

- **Background worker system** — CPU-heavy and slow file-processing tasks are
  now handled by an async worker pool instead of blocking HTTP requests.
  The system is SQLite-backed (`background_jobs` table), so jobs survive a
  server restart and are picked up again on next boot.  A pool of
  `min(available_cpus, 4)` Tokio workers claims jobs atomically via
  `UPDATE … RETURNING` so no two workers can double-process a job.  Workers
  sleep until a `tokio::sync::Notify` fires (triggered by `enqueue`) or a
  5-second poll timeout elapses, keeping CPU usage near zero when the queue
  is empty.  Job types and their new behaviour:
  - **`VideoTranscode`** — MP4 → WebM (VP9 + Opus) transcoding is now fully
    asynchronous.  On upload, the MP4 is saved immediately and the HTTP
    response returns; the worker transcodes it in the background, writes the
    WebM file, updates `posts.file_path` and `posts.mime_type`, refreshes the
    `file_hashes` dedup table, and removes the original MP4.  The JPEG
    thumbnail (first-frame extraction) is still generated synchronously during
    the upload request because it is fast (<1 s) and needed immediately.
  - **`AudioWaveform`** — ffmpeg waveform-PNG generation is now asynchronous.
    An SVG music-note placeholder is written immediately so the post renders
    at once; the worker generates the real waveform PNG and updates
    `posts.thumb_path` once complete.
  - **`ThreadPrune`** — overflow-thread deletion is now enqueued after a
    new thread is created, so the response returns before any files are
    deleted.  The prune logic itself (`db::prune_old_threads`) is unchanged.
  - **`SpamCheck`** — scaffolded hook for future spam/abuse analysis.
    Currently logs unusually long posts at `DEBUG` level; extend this worker
    to add auto-flagging or shadow-banning without touching the hot path.
  - Failed jobs are retried up to **3 times** with the last error recorded in
    `background_jobs.last_error`; permanently failed jobs remain in the table
    for inspection.  The `pending_job_count()` helper is available for
    dashboard / terminal-stats display.
- **Client-side auto-compression** — when a user selects an image or video
  that exceeds the board's file-size limit a modal overlay appears instead of
  waiting for the server to reject it.  The modal shows the file name, actual
  size, and board limit, then offers two choices:
  - **Cancel** — clears the file-input selection.
  - **Auto-compress** — compresses the file client-side to fit within the
    limit, shows a live progress bar, and replaces the file-input value with
    the compressed result so the user can post without re-selecting a file.
    No data is sent to the server until the user submits the form.
  Compression strategies by media type:
  - *Images* — Canvas API iterative re-encode as JPEG with progressively
    lower quality (0.88 → 0.42) then progressive scale reduction (×0.72 per
    step down to 8% of original) until the target size is reached or the
    iteration budget (22 passes) is exhausted.
  - *Videos* — `MediaRecorder` (VP9/VP8/WebM, whichever the browser
    supports) captures the video playing at real-time speed onto a hidden
    canvas with a bitrate calculated as `(targetBytes × 8 × 0.82) / duration`.
    The result is a `video/webm` blob.  Progress is shown as a percentage of
    the video's playback duration.  A minimum bitrate floor of 120 kbps is
    enforced to prevent unreadable output.
  The board's `max_image_size` and `max_video_size` limits are injected as JS
  constants directly from `CONFIG` so the client check always matches the
  server-side validation exactly.  The modal is rendered once per page (shared
  by both the new-thread and reply forms) and wired via `onchange="checkFileSize(this)"`
  on every primary file `<input>`.  All logic runs inside an IIFE — no global
  namespace pollution.  The feature degrades gracefully: if `MediaRecorder` or
  `createImageBitmap` are unavailable (rare, old browsers) the error is shown
  in the progress area; the user can still upload after cancelling.

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
