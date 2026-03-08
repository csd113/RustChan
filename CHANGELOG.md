# Changelog

All notable changes to RustChan will be documented in this file.

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