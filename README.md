<div align="center">

```
██████╗ ██╗   ██╗███████╗████████╗ ██████╗██╗  ██╗ █████╗ ███╗   ██╗
██╔══██╗██║   ██║██╔════╝╚══██╔══╝██╔════╝██║  ██║██╔══██╗████╗  ██║
██████╔╝██║   ██║███████╗   ██║   ██║     ███████║███████║██╔██╗ ██║
██╔══██╗██║   ██║╚════██║   ██║   ██║     ██╔══██║██╔══██║██║╚██╗██║
██║  ██║╚██████╔╝███████║   ██║   ╚██████╗██║  ██║██║  ██║██║ ╚████║
╚═╝  ╚═╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝
```

### A self-hosted imageboard engine. One binary. Zero runtime dependencies.

<br>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![SQLite](https://img.shields.io/badge/SQLite-WAL_Mode-003B57?style=for-the-badge&logo=sqlite&logoColor=white)](https://www.sqlite.org/)
[![Axum](https://img.shields.io/badge/Axum-0.8-7c3aed?style=for-the-badge)](https://github.com/tokio-rs/axum)
[![License: MIT](https://img.shields.io/badge/License-MIT-22c55e?style=for-the-badge)](LICENSE)
[![Version](https://img.shields.io/badge/Version-1.0.11-0ea5e9?style=for-the-badge)](#changelog)

<br>

[**Quick Start**](#-quick-start) · [**Features**](#-features) · [**Optional Integrations**](#-optional-integrations-ffmpeg--tor) · [**Configuration**](#-configuration) · [**Backup System**](#-backup--restore) · [**Deployment**](#-production-deployment) · [**Themes**](#-themes) · [**Changelog**](CHANGELOG.md)

<br>

</div>

---

RustChan is a fully-featured imageboard engine compiled into a **single Rust binary**. Deploy it on a VPS, a Raspberry Pi, or a local machine — no containers, no runtime, no package manager required. All persistent data lives in a single directory alongside the binary, making migrations as simple as `cp -r`.

Two external tools are supported as **optional enhancements**: [**ffmpeg**](#ffmpeg--video--audio-processing) for video transcoding and audio waveforms, and [**Tor**](#tor--onion-service) for anonymous `.onion` access. Neither is required — RustChan degrades gracefully without them.

<br>

## ✦ Features

<table>
<tr>
<td width="50%" valign="top">

### 📋 Boards & Posting
- Multiple boards with independent per-board configuration
- Threaded replies with globally unique post numbers
- **Thread polls** — OP-only, 2–10 options, live percentage bar results, one vote per IP
- **Spoiler tags** — `[spoiler]text[/spoiler]` with click-to-reveal
- **Dice rolling** — `[dice NdM]` resolved server-side at post time (e.g. `[dice 2d6]` → `🎲 2d6 ▸ ⚄ ⚅ = 11`)
- **Emoji shortcodes** — 25 built-in (`:fire:` → 🔥, `:think:` → 🤔, `:based:` → 🗿)
- **Cross-board links** — `>>>/board/123` with floating hover previews
- `**bold**`, `__italic__`, greentext, inline quote-links
- **Sage** — reply without bumping the thread
- **Post editing** — edit within a configurable window using your deletion token
- **Draft autosave** — reply text persisted to `localStorage` every 3 seconds; survives refreshes and crashes
- Tripcodes and user-deletable posts via deletion tokens
- Per-board NSFW tagging, bump limits, and thread caps
- Board index, catalog grid, full-text search, and pagination

</td>
<td width="50%" valign="top">

### 🖼️ Media
- **Images:** JPEG *(EXIF-stripped on upload)*, PNG, GIF, WebP
- **Video:** MP4, WebM — auto-transcoded to VP9+Opus WebM via ffmpeg; AV1 streams re-encoded to VP9
- **Audio:** MP3, OGG, FLAC, WAV, M4A, AAC (up to 150 MB default)
- **Image + audio combo posts** — attach both an image and an audio file simultaneously
- **Audio waveform thumbnails** — generated via ffmpeg's `showwavespic` filter for standalone audio uploads
- **Video embed unfurling** — per-board opt-in; YouTube, Invidious, and Streamable URLs render as thumbnail + click-to-play widgets
- Auto-generated thumbnails with configurable dimensions
- Resizable inline image expansion via drag-to-resize
- **Client-side auto-compression** — oversized files are compressed in-browser before upload with a live progress bar
- Two-layer file validation: Content-Type header + magic byte inspection (extensions are never trusted)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🛡️ Moderation & Administration
- Board creation, configuration, and deletion from the web panel
- Thread sticky and lock toggles
- **Per-post ban + delete** — single-click to ban an IP hash and remove the post simultaneously
- **Ban appeal system** — banned users can submit appeals; admins review from a dedicated queue with dismiss and accept+unban actions
- **IP history view** — paginated list of all posts from any IP hash across all boards
- **PoW CAPTCHA** — per-board opt-in SHA-256 proof-of-work for new thread creation; replies are exempt
- **Report system** — users can report posts; admins see an inbox with resolve and resolve+ban actions
- **Moderation log** — append-only audit trail of all admin actions, viewable from the panel
- Word filters (pattern → replacement, site-wide)
- **Full backup & restore** — entirely web-based with no shell access required
- **SQLite VACUUM** — one-click database compaction with before/after size reporting
- Per-board controls: editing, edit window, archiving, video embeds, PoW CAPTCHA

</td>
<td width="50%" valign="top">

### 🔒 Security
- **Argon2id** password hashing (`t=2, m=65536, p=2`)
- **Security headers** — CSP, HSTS (1 year + subdomains), and Permissions-Policy on all responses
- **CSRF** — double-submit cookie with constant-time token comparison (`subtle::ct_eq`)
- `HttpOnly` + `SameSite=Strict` session cookies with configurable `Secure` flag and `Max-Age`
- **Admin brute-force protection** — progressive lockout after 5 consecutive failed login attempts
- Raw IPs **never stored or logged** — HMAC-keyed SHA-256 hash used everywhere
- Per-IP sliding-window rate limiting on both POST and GET endpoints
- **JPEG EXIF stripping** — GPS, device IDs, and all metadata removed on upload
- All user input HTML-escaped before rendering; markup applied post-escape
- **Zip-bomb protection** — backup restore capped at 1 GiB per entry, 50,000 entries max
- **Redirect hardening** — backslash and encoded variants blocked on redirect parameters
- Path traversal prevention on all filesystem operations

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🗂️ Thread Lifecycle
- **Thread archiving** — overflow threads are archived (readable, locked, hidden from index) rather than deleted; configurable per board
- **Archive page** — `/{board}/archive` with thumbnails, reply counts, and pagination
- **Thread auto-update** — delta-compressed polling keeps reply counts, lock/sticky badges, and new posts in sync without full reloads
- **Floating new-reply pill** — "+N new replies ↓" notification; click to scroll, auto-dismisses after 30 seconds
- **"(You)" tracking** — posts you authored are marked with a `(You)` badge, persisted across refreshes
- Per-board toggle between archive-on-overflow and permanent deletion

</td>
<td width="50%" valign="top">

### 📱 Mobile & UX
- **Mobile reply drawer** — floating action button slides up a full-width reply panel on small screens
- **Cross-board hover previews** — `>>>/board/123` links show a floating popup with client-side caching
- **Five built-in themes** — user-selectable via a floating picker; persisted in `localStorage` with no flash
- **Live stats** — total posts, uploads, and content size displayed on the home page
- **Background worker system** — video transcoding, waveform generation, and thread cleanup run asynchronously without blocking requests
- **Interactive keyboard console** — `[s]` stats · `[l]` boards · `[c]` create · `[d]` delete · `[q]` quit

</td>
</tr>
</table>

<br>

<img width="1511" height="781" alt="RustChan board view" src="https://github.com/user-attachments/assets/0ad5ca51-9d7a-40a6-a754-dbdaebacf66a" />
<img width="1512" height="778" alt="RustChan thread view" src="https://github.com/user-attachments/assets/5ff2658c-8689-4895-8300-9d29effdb090" />
<img width="274" height="511" alt="RustChan mobile view" src="https://github.com/user-attachments/assets/7f467e5c-92a2-4764-a7e3-8790a1dcf3e4" />

<br>

## 🔌 Optional Integrations: ffmpeg & Tor

RustChan is fully functional without either tool. When detected at startup, additional capabilities activate automatically.

### ffmpeg — Video & Audio Processing

When ffmpeg is available on `PATH`:

- **MP4 → WebM transcoding** (VP9 + Opus) for maximum browser compatibility
- **AV1 WebM → VP9 re-encoding** for browsers without AV1 support
- **Audio waveform thumbnails** via the `showwavespic` filter
- **Video thumbnail extraction** from the first frame for catalog previews

Without ffmpeg, videos are served in their original format and audio posts use a generic icon. Set `require_ffmpeg = true` in `settings.toml` to enforce its presence at startup.

See **[SETUP.md — Installing ffmpeg](SETUP.md#installing-ffmpeg)** for platform-specific instructions.

### Tor — Onion Service

When `enable_tor_support = true` and a Tor daemon is running:

- The `.onion` address is read from the hidden-service `hostname` file and displayed on the home page and admin panel
- Setup hints are printed to the console if Tor is detected but not yet configured

Tor handles all onion routing independently — RustChan binds to its normal port while your `torrc` forwards `.onion` traffic to it.

See **[SETUP.md — Installing Tor](SETUP.md#installing-tor)** for configuration details.

<br>

## ⚡ Quick Start

```bash
# 1. Build
cargo build --release

# 2. Create an admin account
./rustchan-cli admin create-admin admin "YourStrongPassword!"

# 3. Create boards
./rustchan-cli admin create-board b    "Random"     "General discussion"
./rustchan-cli admin create-board tech "Technology" "Programming and hardware"

# 4. Start the server
./rustchan-cli
```

Open **`http://localhost:8080`** — the admin panel is at **`/admin`**.

On first launch, `rustchan-data/settings.toml` is generated with a fresh `cookie_secret` and all settings documented inline. Edit and restart to apply changes.

<br>

## 📁 Data Layout

All data lives in `rustchan-data/` alongside the binary. Nothing is written elsewhere unless explicitly overridden via environment variables.

```
rustchan-cli                              ← single self-contained binary
rustchan-data/
├── settings.toml                         ← instance configuration (auto-generated)
├── chan.db                               ← SQLite database (WAL mode)
├── full-backups/                         ← full site backups
│   └── rustchan-backup-20260304_120000.zip
├── board-backups/                        ← per-board backups
│   └── rustchan-board-tech-20260304_120000.zip
└── boards/
    ├── b/
    │   ├── <uuid>.<ext>                  ← uploaded files
    │   └── thumbs/
    │       └── <uuid>_thumb.jpg          ← auto-generated thumbnails
    └── tech/
        ├── <uuid>.<ext>
        └── thumbs/
```

<br>

## ⚙️ Configuration

### settings.toml

Auto-generated on first run. Edit and restart to apply.

```toml
# Site display name shown in the browser title and header.
forum_name = "RustChan"

# TCP port (binds to 0.0.0.0:<port>).
port = 8080

# Upload size limits (MB).
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150

# Auto-generated on first run. Do not change after first use —
# existing IP hashes and bans will become invalid.
cookie_secret = "<auto-generated 32-byte hex>"

# Display .onion address if a Tor daemon is running.
enable_tor_support = true

# Hard-exit if ffmpeg is not found (default: warn only).
require_ffmpeg = false

# WAL checkpoint interval in seconds (0 = disabled).
wal_checkpoint_interval_secs = 3600
```

### Environment Variables

All settings can be overridden via environment variables, which take precedence over `settings.toml`.

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_PORT` | `8080` | TCP port |
| `CHAN_BIND` | `0.0.0.0:8080` | Full bind address (overrides `CHAN_PORT`) |
| `CHAN_DB` | `rustchan-data/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `rustchan-data/boards` | Uploads directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | CSRF tokens and IP hashing key |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image upload size (MiB) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video upload size (MiB) |
| `CHAN_MAX_AUDIO_MB` | `150` | Max audio upload size (MiB) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension (px) |
| `CHAN_BUMP_LIMIT` | `500` | Replies before a thread stops bumping |
| `CHAN_MAX_THREADS` | `150` | Max threads per board before pruning/archiving |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window (seconds) |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (default: 8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For` behind a reverse proxy |
| `CHAN_HTTPS_COOKIES` | *(same as `CHAN_BEHIND_PROXY`)* | Set `Secure` flag on session cookies |
| `CHAN_WAL_CHECKPOINT_SECS` | `3600` | WAL checkpoint interval; `0` to disable |
| `RUST_LOG` | `rustchan-cli=info` | Log verbosity |

<br>

## 💾 Backup & Restore

The entire backup system is accessible from the admin panel — no shell access required.

### Full Site Backups

A full backup is a `.zip` containing a consistent SQLite snapshot (via `VACUUM INTO`) and all uploaded files.

| Action | Description |
|---|---|
| **💾 Save** | Creates a backup and writes it to `rustchan-data/full-backups/` |
| **⬇ Download** | Streams a saved backup to your browser |
| **↺ Restore (server)** | Restores from a file already on the server |
| **↺ Restore (upload)** | Restores from a `.zip` uploaded from your computer |
| **✕ Delete** | Permanently removes the backup file |

### Per-Board Backups

Board backups are self-contained: a `board.json` manifest plus the board's upload directory. Other boards are never affected.

**Restore behaviour:**
- **Board exists** → content is wiped and replaced from the manifest
- **Board doesn't exist** → created from scratch
- All row IDs are **remapped** on import to prevent collisions

> Restore uses SQLite's `sqlite3_backup_init()` API internally — pages are copied directly into the live connection, so no file swapping, WAL deletion, or restart is needed.

<br>

## 🧰 Admin CLI

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
./rustchan-cli admin ban       <ip_hash> "<reason>" [duration_hours]
./rustchan-cli admin unban     <ban_id>
./rustchan-cli admin list-bans
```

`<short>` is the board slug used in URLs (e.g. `tech` → `/tech/`). Lowercase alphanumeric, 1–8 characters.

<br>

## 🚀 Production Deployment

See **[SETUP.md](SETUP.md)** for a complete production guide covering:

- System user creation and hardened directory layout
- **systemd** service with security directives
- **nginx** reverse proxy with TLS via Let's Encrypt
- ffmpeg and Tor installation on Linux, macOS, and Windows
- First-run configuration walkthrough
- Raspberry Pi SD card wear reduction
- Security hardening checklist

### Cross-Compilation

```bash
# ARM64 (Raspberry Pi 4/5)
rustup target add aarch64-unknown-linux-gnu
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu

# Windows x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

The release profile enables `strip = true`, `lto = "thin"`, and `panic = "abort"`. Typical binary size: **12–18 MiB**.

<br>

## 🏗️ Architecture

RustChan is intentionally minimal — no template engine, no ORM, no JavaScript framework. HTML is rendered with plain Rust `format!` strings. The result is a single binary that starts in under a second.

| Layer | Technology |
|---|---|
| Web framework | [Axum](https://github.com/tokio-rs/axum) 0.8 |
| Async runtime | [Tokio](https://tokio.rs/) 1.x |
| Database | SQLite via [rusqlite](https://github.com/rusqlite/rusqlite) (bundled) |
| Connection pool | r2d2 + r2d2_sqlite (5-second acquisition timeout) |
| Image processing | [`image`](https://github.com/image-rs/image) crate |
| Video transcoding | ffmpeg (optional) |
| Audio waveforms | ffmpeg `showwavespic` filter (optional) |
| Password hashing | `argon2` crate (Argon2id) |
| Timing-safe comparison | `subtle` crate |
| HTML rendering | Plain Rust `format!` strings |
| Configuration | `settings.toml` + env var overrides via `once_cell::Lazy` |
| Logging | `tracing` + `tracing-subscriber` |

### Source Layout

```
src/
├── main.rs             — entry point, router, background tasks, keyboard console
├── config.rs           — settings.toml + env var resolution
├── db.rs               — all SQL queries (no ORM)
├── error.rs            — error handling and ban page rendering
├── models.rs           — database row structs
├── middleware/mod.rs    — rate limiting, CSRF, IP hashing, proxy trust
├── handlers/
│   ├── admin.rs        — admin panel, moderation, backup/restore, appeals
│   ├── board.rs        — board index, catalog, archive, search, thread creation
│   └── thread.rs       — thread view, replies, polls, editing
├── templates/mod.rs    — HTML generation (all themes, dynamic site name)
└── utils/
    ├── crypto.rs       — Argon2id, CSRF, sessions, IP hashing, PoW verification
    ├── files.rs        — upload validation, thumbnails, EXIF stripping, waveforms
    ├── sanitize.rs     — HTML escaping, markup (greentext, spoilers, dice, embeds)
    └── tripcode.rs     — SHA-256 tripcode generation
```

<br>

## 🔐 Security Model

| Concern | Implementation |
|---|---|
| **Passwords** | Argon2id (`t=2, m=65536, p=2`) — memory-hard, GPU-resistant |
| **Brute-force** | Progressive lockout after 5 failed admin login attempts per IP |
| **Sessions** | `HttpOnly`, `SameSite=Strict`, `Max-Age` aligned to server config |
| **CSRF** | Double-submit cookie with constant-time token comparison (`subtle::ct_eq`) |
| **Security headers** | CSP (`self`-only scripts/styles/media), HSTS (1 year + subdomains), Permissions-Policy |
| **IP privacy** | Raw IPs never stored or logged — HMAC-keyed SHA-256 hash used everywhere |
| **Rate limiting** | Sliding-window per hashed IP on all POST endpoints (10/min) and GET endpoints (60/min) |
| **Proxy support** | All handlers use proxy-aware IP extraction when `CHAN_BEHIND_PROXY=true` |
| **File safety** | Content-Type + magic byte validation; file extensions never trusted |
| **EXIF stripping** | All JPEG uploads re-encoded — GPS, device IDs, and all metadata discarded |
| **XSS** | All user input HTML-escaped before rendering; markup applied post-escape |
| **Zip-bomb protection** | Backup restore capped at 1 GiB per entry, 50,000 entries max |
| **Redirect hardening** | Backslash and percent-encoded variants blocked on `return_to` parameters |
| **Path traversal** | Backup filenames validated against `[a-zA-Z0-9._-]` before filesystem access |
| **Body limits** | Per-route limits on small endpoints (64 KiB) to prevent memory exhaustion |
| **Connection pool** | 5-second acquisition timeout prevents thread-pool exhaustion under load |
| **PoW CAPTCHA** | SHA-256 hashcash (20-bit difficulty), verified server-side with 5-minute grace window |

<br>

## 📝 Post Markup Reference

```
>quoted text              greentext line
>>123                     reply link to post #123
>>>/board/                cross-board index link
>>>/board/123             cross-board thread link (with hover preview)
**text**                  bold
__text__                  italic
[spoiler]text[/spoiler]   hidden until clicked or hovered
[dice NdM]                server-side dice roll (e.g. [dice 2d6] → 🎲 2d6 ▸ ⚄ ⚅ = 11)
:fire:  :think:  :based:  :kek:  …  (25 emoji shortcodes)
```

<br>

## 🎨 Themes

Five built-in themes, selectable via the floating picker on every page. Persisted in `localStorage` with no flash on load.

| Theme | Description |
|---|---|
| **Terminal** *(default)* | Dark background, matrix-green monospace, glowing accents |
| **Frutiger Aero** | Frosted glass panels, pearl-blue gradients, rounded corners |
| **DORFic Aero** | Dark stone walls, torchlit amber/copper glass panels |
| **FluoroGrid** | Pale sage, muted teal grid lines, dusty lavender panels |
| **NeonCubicle** | Cool off-white, horizontal scanlines, steel-teal borders |

<br>

## 📋 Changelog

See **[CHANGELOG.md](CHANGELOG.md)** for the full version history.

**Latest — v1.0.11:**
Security headers (CSP, HSTS, Permissions-Policy) · proxy-aware IP extraction on all handlers · GET rate limiting (60 req/min) · zip-bomb protection on restore · IP hashing everywhere · admin brute-force lockout · constant-time CSRF comparison · poll input caps · session cookie `Max-Age` · connection pool timeout · per-route body limits · open redirect hardening · worker exponential backoff · file dedup race fix · per-post ban+delete · ban appeal system · PoW CAPTCHA · video embeds · cross-board hover previews · new-reply pill · live thread metadata · "(You)" tracking

**v1.0.9:** Per-board editing toggle · configurable edit window · per-board archive toggle · AV1→VP9 transcoding fix

**v1.0.8:** Thread archiving · mobile reply drawer · dice rolling · sage · post editing · draft autosave · WAL checkpointing · VACUUM button · IP history

**v1.0.7:** EXIF stripping · image+audio combo posts · audio waveform thumbnails

**v1.0.6:** Web-based backup management · board-level backup/restore · GitHub Actions CI

**v1.0.5:** MP4→WebM auto-transcoding · home page stats · macOS Tor detection fix

<br>

---

<div align="center">

Built with 🦀 Rust &nbsp;·&nbsp; Powered by SQLite &nbsp;·&nbsp; Optional integrations: ffmpeg · Tor

*Drop it anywhere. It just runs.*

</div>