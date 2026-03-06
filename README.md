<div align="center">

```
██████╗ ██╗   ██╗███████╗████████╗ ██████╗██╗  ██╗ █████╗ ███╗   ██╗
██╔══██╗██║   ██║██╔════╝╚══██╔══╝██╔════╝██║  ██║██╔══██╗████╗  ██║
██████╔╝██║   ██║███████╗   ██║   ██║     ███████║███████║██╔██╗ ██║
██╔══██╗██║   ██║╚════██║   ██║   ██║     ██╔══██║██╔══██║██║╚██╗██║
██║  ██║╚██████╔╝███████║   ██║   ╚██████╗██║  ██║██║  ██║██║ ╚████║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝
```

### A self-hosted imageboard. One binary. Zero runtime dependencies.

<br>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![SQLite](https://img.shields.io/badge/SQLite-WAL_Mode-003B57?style=for-the-badge&logo=sqlite&logoColor=white)](https://www.sqlite.org/)
[![Axum](https://img.shields.io/badge/Axum-0.8-7c3aed?style=for-the-badge)](https://github.com/tokio-rs/axum)
[![License: MIT](https://img.shields.io/badge/License-MIT-22c55e?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-1.0.10-0ea5e9?style=for-the-badge)](#changelog)

<br>

[**Quick Start**](#-quick-start) · [**Features**](#-features) · [**Optional Integrations**](#-optional-integrations-ffmpeg--tor) · [**Configuration**](#-configuration) · [**Backup System**](#-backup--restore) · [**Deployment**](#-production-deployment) · [**Themes**](#-themes) · [**Changelog**](CHANGELOG.md)

<br>

</div>

---

RustChan is a fully-featured imageboard server compiled into a **single Rust binary**. Drop it on a VPS, a Raspberry Pi, or a local machine — it runs immediately with no containers, no runtime, and no package manager required. All persistent data lives in one directory next to the binary, making migrations a `cp -r`.

Two external tools plug in as **optional enhancements**: [**ffmpeg**](#ffmpeg--video--audio-processing) for video transcoding and audio waveforms, and [**Tor**](#tor--onion-service) for anonymous `.onion` access. Neither is required — RustChan degrades gracefully without either.

<br>

## ✦ Features

<table>
<tr>
<td width="50%" valign="top">

### 📋 Boards & Posting
- Multiple boards with independent per-board configuration
- Threaded replies with unique post numbers across the instance
- **Thread polls** — OP-only, 2–10 options, live percentage bar results, one vote per IP enforced at the DB level
- **Spoiler tags** — `[spoiler]text[/spoiler]` with click-to-reveal
- **Dice rolling** — `[dice NdM]` rolled server-side at post time and embedded immutably in the rendered HTML (e.g. `[dice 2d6]` → `🎲 2d6 ▸ ⚄ ⚅ = 11`); d6 faces shown as Unicode die characters, other sizes as `【N】`
- **Emoji shortcodes** — 25 built-in (`:fire:` → 🔥, `:think:` → 🤔, `:based:` → 🗿)
- **Cross-board links** — `>>>/board/123` styled in amber with live hover-preview popup
- `**bold**`, `__italic__`, greentext, inline quote-links
- **Post sage** — reply without bumping the thread
- **Post editing** — edit your own post within a configurable time window using your deletion token; *(edited HH:MM:SS)* badge appended on any edited post
- **Draft autosave** — reply textarea persisted to localStorage every 3 seconds; restored on refresh or accidental navigation
- Tripcodes and user-deletable posts via deletion tokens
- Per-board NSFW tagging, bump limits, max thread caps
- Board index, catalog grid, full-text search, pagination

</td>
<td width="50%" valign="top">

### 🖼️ Media
- **Images:** JPEG *(EXIF-stripped on upload)*, PNG, GIF, WebP
- **Video:** MP4, WebM — **auto-transcoded to VP9+Opus WebM** when ffmpeg is present; AV1 WebM streams are re-encoded to VP9 for broad compatibility
- **Audio:** MP3, OGG, FLAC, WAV, M4A, AAC (up to 150 MB default)
- **Image + audio combo posts** — attach both an image and an audio file to the same post simultaneously
- **Audio waveform thumbnails** — when ffmpeg is present, standalone audio uploads generate a static waveform PNG using ffmpeg's `showwavespic` filter instead of a generic placeholder icon
- **Video embed unfurling** — per-board opt-in; YouTube, Invidious, and Streamable URLs in post bodies are replaced inline with a thumbnail + click-to-play iframe widget, positioned before the post body like a native webm with the original URL preserved as a link; YouTube thumbnails appear in the catalog and board index too
- Auto-generated thumbnails with configurable max dimension
- Resizable inline image expansion (drag-to-resize)
- Two-layer file validation: Content-Type header + magic byte inspection; file extension is never trusted

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🛡️ Moderation & Admin Panel
- Board creation, settings editing, and deletion
- Thread sticky / lock toggles
- **Per-post inline ban+delete** — every post shows a ⛔ button in admin view; a browser prompt collects the reason and duration, then atomically bans the post author's IP hash and deletes the post (or the entire thread if it's the OP) in a single action with no copy-pasting required
- **Ban appeal system** — banned users see a textarea on the ban page to submit an appeal (max 512 chars); all open appeals queue in a dedicated section of the admin panel with **✕ dismiss** and **✓ accept + unban** buttons; accepting immediately removes the ban; a 24-hour per-IP cooldown prevents appeal spam
- **IP history view** — a 🔍 link beside every admin-visible post opens a paginated history of all posts from that IP hash across all boards
- **PoW CAPTCHA** — per-board opt-in; new thread creation requires a SHA-256 hashcash proof-of-work solved entirely in the browser (~50–200 ms) before the form submits; replies are intentionally exempt; solutions verified server-side with a 5-minute grace window for clock skew
- Word filters (pattern → replacement, site-wide)
- **Full backup & restore** — entirely web-based; no shell access needed
- Site-wide settings: site name, home page subtitle, greentext wall collapsing
- **SQLite VACUUM** — one-click database compaction with before/after size display
- Per-board controls: editing toggle, edit window, archive toggle, video embeds toggle, PoW CAPTCHA toggle

</td>
<td width="50%" valign="top">

### 🔒 Security
- **Argon2id** password hashing (`t=2, m=65536, p=2`) — memory-hard, GPU-resistant
- CSRF double-submit cookie pattern on every state-changing POST
- `HttpOnly` + `SameSite=Strict` session cookies with configurable `Secure` flag
- Raw IPs are **never stored** — salted SHA-256 hash keyed to `cookie_secret` only
- In-memory per-IP sliding window rate limiting via `DashMap`
- **JPEG EXIF stripping** — all uploaded JPEGs are re-encoded through the `image` crate; GPS coordinates, device serial numbers, camera metadata, and all other EXIF/XMP/IPTC data are discarded before the file is saved
- All user input HTML-escaped before rendering — no raw user HTML ever reaches the browser
- Backup filenames validated to `[a-zA-Z0-9._-]` only before any filesystem operation

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🗂️ Thread Lifecycle
- **Thread archiving** — when a board hits its thread cap, overflowing threads move to an archived state (readable, locked, hidden from the index) rather than being permanently deleted; configurable per board
- **Archive page** — `/{board}/archive` lists all archived threads with thumbnails, reply counts, and pagination; linked from every board page
- Per-board toggle between archive-on-overflow and hard-delete-on-overflow
- **Per-board post editing** — independently enable/disable per board; configure the edit window in seconds (0 falls back to the server default of 5 minutes)
- Thread auto-update with **delta-compressed state** — reply count, lock/sticky badges, and new posts stay live without a full page reload
- **Floating new-reply pill** — "+N new replies ↓" fades in when the auto-updater detects new posts; click to scroll, auto-dismisses at the bottom of the page or after 30 seconds
- **"(You)" post tracking** — posts you authored in the current browser get a `(You)` badge that persists across page refreshes via localStorage

</td>
<td width="50%" valign="top">

### 📱 Mobile & UX
- **Mobile reply drawer** — on viewports ≤ 767 px, a floating ✏ Reply button slides up a full-width drawer from the bottom of the screen; tapping a post number populates the `>>N` quote directly inside the drawer textarea
- **Cross-board quotelink hover previews** — hovering a `>>>/board/123` link fetches and renders the OP post in a floating popup, with client-side caching so repeat hovers are instant
- **Five built-in themes**, user-selectable via a floating picker; persisted in localStorage with zero load flash
- **Live home page stats** — total posts, images, videos, audio files, and active content size displayed on the index page
- **Interactive keyboard console** — `[s]` stats · `[l]` boards · `[c]` create board · `[d]` delete thread · `[h]` help · `[q]` quit

</td>
</tr>
</table>

<br>

## 🔌 Optional Integrations: ffmpeg & Tor

RustChan is fully functional without either of these tools. Install them and additional capabilities activate automatically at startup.

### ffmpeg — Video & Audio Processing

When ffmpeg is detected on `PATH`, RustChan will:

- **Transcode MP4 uploads to WebM** (VP9 + Opus) automatically for maximum browser compatibility — the original MP4 is never stored
- **Re-encode AV1 WebM** uploads to VP9+Opus, ensuring playback on browsers without AV1 support
- **Generate audio waveform thumbnails** — standalone audio uploads (MP3, FLAC, OGG, etc.) display a colour-matched waveform PNG instead of a generic music-note placeholder
- **Generate video thumbnails** from the first frame of WebM files for catalog and index previews

Without ffmpeg, uploaded videos are stored and served in their original format, and audio posts show a generic icon. RustChan logs a warning at startup if ffmpeg is absent but continues normally. Set `require_ffmpeg = true` in `settings.toml` to make its absence a hard startup error instead.

See **[SETUP.md — Installing ffmpeg](SETUP.md#installing-ffmpeg)** for step-by-step instructions on Linux, macOS, and Windows.

### Tor — Onion Service

When `enable_tor_support = true` is set in `settings.toml` and a Tor daemon is running, RustChan will:

- **Detect Tor at startup** by checking common install paths (system PATH, Homebrew on macOS, etc.)
- **Read the `.onion` address** from the hidden-service `hostname` file and display it on the home page and in the admin panel so users can copy it easily
- **Print setup hints** to the console at startup if Tor is installed but a hidden service has not yet been configured

Tor handles all onion routing independently — RustChan simply binds to its normal port and reads the address file. Your `torrc` tells Tor to forward `.onion` traffic to that port.

See **[SETUP.md — Installing Tor](SETUP.md#installing-tor)** for installation and hidden-service configuration on Linux, macOS, and Windows.

<br>

## ⚡ Quick Start

```bash
# 1. Build
cargo build --release

# 2. Create your first admin account
./rustchan-cli admin create-admin admin "YourStrongPassword!"

# 3. Create some boards
./rustchan-cli admin create-board b    "Random"     "General discussion"
./rustchan-cli admin create-board tech "Technology" "Programming and hardware"

# 4. Start the server
./rustchan-cli
```

Open **`http://localhost:8080`** — the admin panel is at **`/admin`**.

On first launch, `rustchan-data/settings.toml` is generated automatically with a freshly-generated `cookie_secret` and every setting documented inline. Edit it and restart to apply changes.

<br>

## 📁 Data Layout

Everything lives in `rustchan-data/` next to the binary. Nothing is written elsewhere unless you override paths via environment variables.

```
rustchan-cli                              ← single self-contained binary
rustchan-data/
├── settings.toml                         ← instance config (auto-generated on first run)
├── chan.db                               ← SQLite database (WAL mode)
├── full-backups/                         ← full site backups (saved from admin panel)
│   └── rustchan-backup-20260304_120000.zip
├── board-backups/                        ← per-board backups (saved from admin panel)
│   └── rustchan-board-tech-20260304_120000.zip
└── boards/
    ├── b/
    │   ├── <uuid>.<ext>                  ← uploaded files
    │   └── thumbs/
    │       └── <uuid>_thumb.jpg         ← auto-generated thumbnails
    └── tech/
        ├── <uuid>.<ext>
        └── thumbs/
```

<br>

## ⚙️ Configuration

### `settings.toml`

Auto-generated on first run. Edit and restart to apply.

```toml
# Site display name — shown in the browser title, header, and home page.
forum_name = "RustChan"

# TCP port (binds to 0.0.0.0:<port>).
port = 8080

# Upload size limits in megabytes.
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150

# Auto-generated on first run. DO NOT change after your first post —
# all existing IP hashes and bans will become invalid.
cookie_secret = "<auto-generated 32-byte hex>"

# Set true to detect a running Tor daemon and display the .onion
# address on the home page and admin panel.
enable_tor_support = true

# Set true to hard-exit if ffmpeg is not found (default: warn only).
require_ffmpeg = false

# How often (seconds) to run PRAGMA wal_checkpoint(TRUNCATE) to prevent
# the SQLite WAL from growing unbounded. Set to 0 to disable.
wal_checkpoint_interval_secs = 3600
```

### Environment Variables

All settings can be overridden with environment variables, which take precedence over `settings.toml`. Recommended for secrets in production (e.g. via systemd's `Environment=` directive).

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_PORT` | `8080` | TCP port |
| `CHAN_BIND` | `0.0.0.0:8080` | Full bind address (overrides `CHAN_PORT`) |
| `CHAN_DB` | `<exe-dir>/rustchan-data/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `<exe-dir>/rustchan-data/boards` | Uploads directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | **Required in production.** CSRF tokens & IP hashing. |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image upload size (MiB) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video upload size (MiB) |
| `CHAN_MAX_AUDIO_MB` | `150` | Max audio upload size (MiB) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension in pixels |
| `CHAN_BUMP_LIMIT` | `500` | Reply count after which a thread stops bumping |
| `CHAN_MAX_THREADS` | `150` | Max live threads per board before oldest is pruned/archived |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window duration in seconds |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration in seconds (default: 8 h) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For` when behind nginx / Caddy |
| `CHAN_HTTPS_COOKIES` | *(same as `CHAN_BEHIND_PROXY`)* | Add `Secure` flag to session cookies |
| `CHAN_WAL_CHECKPOINT_SECS` | `3600` | WAL checkpoint interval in seconds; `0` to disable |
| `RUST_LOG` | `rustchan-cli=info` | Log verbosity (`=debug` for verbose output) |

<br>

## 💾 Backup & Restore

RustChan's backup system is **entirely web-based** — no shell access or file explorer needed. Every backup action is available directly from the admin panel.

### Full Site Backups

A full backup is a `.zip` containing a consistent SQLite snapshot (via `VACUUM INTO`, safe under live writes) plus all uploaded files and thumbnails.

| Action | Description |
|---|---|
| **💾 Save to server** | Creates the backup and writes it to `rustchan-data/full-backups/` |
| **⬇ Download to computer** | Streams a saved server-side backup as a `.zip` to your browser |
| **↺ Restore from server** | Restores the live DB from a saved file — no re-upload, no restart needed |
| **↺ Restore from local file** | Upload a `.zip` from your computer to restore directly |
| **✕ Delete** | Permanently removes the `.zip` from the server filesystem |

### Per-Board Backups

Board backups are self-contained: a `board.json` manifest (all posts, threads, polls, votes, file hash records) plus that board's upload directory. Other boards are never touched.

Each board card in the admin panel has both a **💾 Save to server** and a **⬇ Download to computer** button for quick one-click access.

**Restore behaviour:**
- Board **exists** → content is wiped and replaced; settings updated from the manifest
- Board **doesn't exist** → created from scratch with the manifest's configuration
- All row IDs are **remapped** on import — zero collision risk with existing data

> **How restore works internally:** RustChan uses SQLite's `sqlite3_backup_init()` API rather than file swapping. This copies pages directly into the live connection's open file descriptors, so every pooled connection immediately reads the restored data. No file renaming, no WAL deletion, no restart required.

<br>

## 🧰 Admin CLI

Board and account management is also available from the command line — useful for scripting and provisioning.

```bash
# Admin accounts
./rustchan-cli admin create-admin   <username> <password>
./rustchan-cli admin reset-password <username> <new-password>
./rustchan-cli admin list-admins

# Boards
./rustchan-cli admin create-board <short> <name> [description] [--nsfw]
./rustchan-cli admin delete-board <short>
./rustchan-cli admin list-boards

# Bans
./rustchan-cli admin ban       <ip_hash> "<reason>" [duration_hours]   # omit hours = permanent
./rustchan-cli admin unban     <ban_id>
./rustchan-cli admin list-bans
```

`<short>` is the board slug used in URLs (e.g. `tech` → `/tech/`). Lowercase alphanumeric, 1–8 characters.

<br>

## 🚀 Production Deployment

See **[SETUP.md](SETUP.md)** for a complete, step-by-step production guide covering:

- System user creation and hardened directory layout
- **systemd** service with security directives (`NoNewPrivileges`, `PrivateTmp`, `ProtectSystem=strict`)
- **nginx** reverse proxy with TLS via Let's Encrypt
- Installing **ffmpeg** on Linux, macOS, and Windows
- Installing **Tor** and configuring a hidden service on Linux, macOS, and Windows
- First-run configuration and board creation walkthrough
- Raspberry Pi SD card wear reduction
- Security hardening checklist
- Troubleshooting reference

### Cross-Compilation

```bash
# ARM64 — Raspberry Pi 4/5
rustup target add aarch64-unknown-linux-gnu
cargo install cross   # uses Docker for the cross-linker
cross build --release --target aarch64-unknown-linux-gnu

# Windows x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

The release profile sets `strip = true`, `lto = "thin"`, and `panic = "abort"`. Typical stripped binary: **12–18 MiB**.

<br>

## 🏗️ Architecture

RustChan is intentionally minimal. No template engine, no ORM, no JavaScript framework. HTML is rendered with plain Rust `format!` strings. The result is a single binary that starts in under a second.

| Layer | Technology |
|---|---|
| Web framework | [Axum](https://github.com/tokio-rs/axum) 0.8 |
| Async runtime | [Tokio](https://tokio.rs/) 1.x |
| Database | SQLite via [rusqlite](https://github.com/rusqlite/rusqlite) — bundled, no system library needed |
| Connection pool | r2d2 + r2d2_sqlite |
| Image processing | [`image`](https://github.com/image-rs/image) crate (JPEG, PNG, GIF, WebP) |
| Video transcoding | ffmpeg (optional — degrades gracefully) |
| Audio waveforms | ffmpeg `showwavespic` filter (optional) |
| Onion address display | Tor hidden-service hostname file (optional) |
| Password hashing | `argon2` crate — Argon2id |
| HTML rendering | Plain Rust `format!` — zero template engine overhead |
| Config | `settings.toml` + env var overrides via `once_cell::Lazy` |
| Logging | `tracing` + `tracing-subscriber` (stdout / journald) |

### Source Layout

```
src/
├── main.rs             — entry point, router, keyboard console, background tasks
├── config.rs           — settings.toml + env var resolution, first-run generation
├── db.rs               — all SQL queries (no ORM)
├── error.rs            — AppError → HTTP response conversion; ban page rendering
├── models.rs           — DB row structs + BackupInfo + BanAppeal
├── middleware/mod.rs   — rate limiting, CSRF, IP hashing, proxy trust
├── handlers/
│   ├── admin.rs        — admin panel, board/ban/filter/backup/appeal management
│   ├── board.rs        — board index, catalog, archive, search, thread creation, ban appeals
│   └── thread.rs       — thread view, reply posting, poll voting, post editing
├── templates/mod.rs    — pure-Rust HTML generation (all five themes, live site name/subtitle)
└── utils/
    ├── crypto.rs       — Argon2id, CSRF, session IDs, IP hashing, PoW verification
    ├── files.rs        — upload validation, thumbnail generation, EXIF stripping, waveforms
    ├── sanitize.rs     — HTML escaping, markup renderer (greentext, spoilers, dice, embeds)
    └── tripcode.rs     — SHA-256 tripcode system
```

<br>

## 🔐 Security Model

| Concern | Implementation |
|---|---|
| **Passwords** | Argon2id (`t=2, m=65536, p=2`) — memory-hard, GPU-resistant. ~200 ms on a Raspberry Pi 4 |
| **Sessions** | `HttpOnly`, `SameSite=Strict`, path-scoped to `/admin`. Configurable duration (default 8 h) |
| **CSRF** | Double-submit cookie pattern — every POST validates `_csrf` against the session cookie |
| **IP privacy** | Raw IPs never stored. A salted SHA-256 keyed to `cookie_secret` is stored instead |
| **Rate limiting** | In-memory sliding window per hashed IP. Default: 10 POSTs / 60 seconds |
| **File safety** | Two-layer check: Content-Type header + magic byte inspection. Extension never trusted |
| **EXIF stripping** | All JPEG uploads re-encoded via `image` crate — GPS, device ID, all metadata discarded |
| **XSS** | All user input passes through `escape_html()` before insertion. Markup applied after escaping |
| **Path traversal** | Backup filenames validated to `[a-zA-Z0-9._-]` only before any filesystem operation |
| **Backup restore** | Uses `sqlite3_backup_init()` — no file swapping, no WAL corruption, no restart required |
| **PoW CAPTCHA** | SHA-256 hashcash at 20-bit difficulty, verified server-side with a 5-minute grace window |

<br>

## 📝 Post Markup Reference

```
>quoted text              greentext line
>>123                     reply link — jumps to post #123 on the same board
>>>/board/                cross-board index link (amber, hover preview)
>>>/board/123             cross-board thread link (amber, hover preview)
**text**                  bold
__text__                  italic
[spoiler]text[/spoiler]   hidden until clicked/hovered
[dice NdM]                server-side dice roll  e.g. [dice 2d6] → 🎲 2d6 ▸ ⚄ ⚅ = 11
:fire:  :think:  :based:  :kek:  …  (25 emoji shortcodes)
```

<br>

## 🎨 Themes

Five built-in themes, user-selectable via the floating picker in the bottom-right corner of every page. Choice persists in `localStorage` with no load flash.

| Theme | Aesthetic |
|---|---|
| **Terminal** *(default)* | Dark matrix-green. Monospace font, glowing green accents, scanline body texture |
| **Frutiger Aero** | Frosted glass panels, pearl-blue gradients, rounded corners — Vista-era glassmorphism |
| **DORFic Aero** | Dark hewn-stone walls, torchlit amber/copper glass panels — Dwarf Fortress meets Vista |
| **FluoroGrid** | Pale sage background, muted teal grid lines, dusty lavender panels, plum accents |
| **NeonCubicle** | Cool off-white, horizontal scanlines, steel-teal borders, soft orchid accents |

<br>

## 📋 Changelog

See **[CHANGELOG.md](CHANGELOG.md)** for the full version history.

**Latest — v1.0.10:**
- Per-post inline ban+delete (⛔ button on every post in admin view)
- Ban appeal system — appeal form on ban page, admin queue with dismiss/accept+unban, 24h cooldown
- PoW CAPTCHA for new threads (per-board opt-in; replies exempt; 5-minute server-side grace window)
- Video embed unfurling — YouTube/Invidious/Streamable URLs become webm-style thumbnail+iframe widgets; thumbnails appear in catalog and board index
- Cross-board quotelink hover previews with client-side result caching
- Floating "+N new replies" pill; delta-compressed live thread state
- "(You)" post tracking persisted across page refreshes

**v1.0.9:** Per-board post editing toggle, configurable edit window, per-board archive toggle

**v1.0.8:** Thread archiving with `/{board}/archive` page · Mobile reply drawer · Server-side dice rolling · Post sage · Post editing with deletion-token auth · Draft autosave · WAL checkpoint background task · SQLite VACUUM from admin panel · IP history view

**v1.0.7:** JPEG EXIF stripping on upload · Image+audio combo posts · Audio waveform thumbnails via ffmpeg

**v1.0.6:** Full web-based backup system — full and per-board backups, in-panel management, download/restore/delete · GitHub Actions CI across 5 targets

**v1.0.5:** Automatic MP4→WebM transcoding via ffmpeg · Home page live stats panel · Tor detection on Homebrew (Apple Silicon + Intel)

<br>

---

<div align="center">

Built with 🦀 Rust &nbsp;·&nbsp; Powered by SQLite &nbsp;·&nbsp; Optional integrations: ffmpeg · Tor

*Drop it anywhere. It just runs.*

</div>
