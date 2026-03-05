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
[![Version](https://img.shields.io/badge/Version-1.0.6-0ea5e9?style=for-the-badge)](#changelog)

<br>

[**Quick Start**](#-quick-start) · [**Features**](#-features) · [**Configuration**](#-configuration) · [**Backup System**](#-backup--restore) · [**Deployment**](#-production-deployment) · [**Themes**](#-themes) · [**Changelog**](CHANGELOG.md)

<br>

</div>

---

RustChan is a fully-featured imageboard server compiled into a **single Rust binary**. Drop it on a VPS, a Raspberry Pi, or a local machine — it runs immediately with no containers, no runtime, and no package manager required. All persistent data lives in one directory next to the binary, making migrations a `cp -r`.

<br>

## ✦ Features

<table>
<tr>
<td width="50%" valign="top">

### 📋 Boards & Posting
- Multiple boards with per-board configuration
- Threaded replies with unique post numbers
- **Thread polls** — OP-only, 2–10 options, live percentage bar results, one vote per IP enforced at the DB level
- **Spoiler tags** — `[spoiler]text[/spoiler]` with click-to-reveal
- **Emoji shortcodes** — 25 built-in (`:fire:` → 🔥, `:think:` → 🤔, `:based:` → 🗿)
- **Cross-board links** — `>>>/board/123` styled in amber
- `**bold**`, `__italic__`, greentext, inline quote-links
- Tripcodes and secure user-deletable posts via deletion tokens
- Per-board NSFW tagging, bump limits, max thread caps
- Board index, catalog grid, full-text search, pagination

</td>
<td width="50%" valign="top">

### 🖼️ Media
- **Images:** JPEG, PNG, GIF, WebP
- **Video:** MP4, WebM — **auto-transcoded to WebM** when ffmpeg is present
- **Audio:** MP3, OGG, FLAC, WAV, M4A, AAC (up to 150 MB default)
- Auto-generated thumbnails with configurable max dimension
- Per-board upload directories (`boards/{board}/thumbs/`)
- Resizable inline image expansion (drag-to-resize)
- Two-layer file validation: Content-Type header + magic byte inspection
- Extension is never trusted

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🛡️ Admin Panel
- Board creation, settings editing, and deletion
- Thread sticky / lock toggles
- Post and thread deletion with file cleanup
- IP-hash-based ban system with optional expiry durations
- Word filters (pattern → replacement, site-wide)
- **Full backup & restore from the web UI** — no shell access needed
- Per-board backup controls on every board card
- Site-wide settings (greentext wall collapsing, etc.)

</td>
<td width="50%" valign="top">

### 🔒 Security
- **Argon2id** password hashing (`t=2, m=65536, p=2`)
- CSRF double-submit cookie pattern on every state-changing POST
- `HttpOnly` + `SameSite=Strict` session cookies with configurable `Secure` flag
- Raw IPs are **never stored** — salted SHA-256 hash only
- In-memory per-IP sliding window rate limiting via `DashMap`
- All user input HTML-escaped before rendering — no raw user HTML ever reaches the browser
- Tor onion service detection and startup hints

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🎨 Themes
Five built-in UI themes, user-selectable and persisted in `localStorage` with zero flash on load:
- **Terminal** — dark matrix-green monospace
- **Frutiger Aero** — frosted glass, Vista-era gradients
- **DORFic Aero** — torchlit amber stone, underground fortress
- **FluoroGrid** — fluorescent 80s office, sage + plum
- **NeonCubicle** — scanlines, lavender panels, orchid accents

</td>
<td width="50%" valign="top">

### 📊 Live Server Stats
Real-time terminal output while the server runs:
- Requests per second counter with in-flight count
- Active file upload progress bar with animated spinner
- Per-board thread & post counts with `(+N)` delta highlighting
- Users online (unique IPs in last 5 minutes)
- **Interactive keyboard console** while server runs:
  `[s]` stats · `[l]` boards · `[c]` create · `[d]` delete thread · `[h]` help · `[q]` quit

</td>
</tr>
</table>

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

# Set true to probe for Tor at startup and print onion service hints.
enable_tor_support = true

# Set true to hard-exit if ffmpeg is not found (default: warn only).
require_ffmpeg = false
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
| `CHAN_MAX_THREADS` | `150` | Max live threads per board before oldest is pruned |
| `CHAN_RATE_POSTS` | `10` | Max POSTs per rate window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window duration in seconds |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration in seconds (default: 8 h) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For` when behind nginx / Caddy |
| `CHAN_HTTPS_COOKIES` | *(same as `CHAN_BEHIND_PROXY`)* | Add `Secure` flag to session cookies |
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
- First-run configuration and board creation walkthrough
- Raspberry Pi SD card wear reduction via tmpfs WAL
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
| Video transcoding | ffmpeg (optional, degrades gracefully) |
| Password hashing | `argon2` crate — Argon2id |
| HTML rendering | Plain Rust `format!` — zero template engine overhead |
| Config | `settings.toml` + env var overrides via `once_cell::Lazy` |
| Logging | `tracing` + `tracing-subscriber` (stdout / journald) |

### Source Layout

```
src/
├── main.rs             — entry point, router, keyboard console, background stats ticker
├── config.rs           — settings.toml + env var resolution, first-run generation
├── db.rs               — all SQL queries (no ORM)
├── error.rs            — AppError → HTTP response conversion
├── models.rs           — DB row structs + BackupInfo
├── middleware/mod.rs   — rate limiting, CSRF, IP hashing, proxy trust
├── handlers/
│   ├── admin.rs        — admin panel, board/ban/filter/backup management
│   ├── board.rs        — board index, catalog, search, thread creation
│   └── thread.rs       — thread view, reply posting, poll voting
├── templates/mod.rs    — pure-Rust HTML generation (all five themes, theme picker JS)
└── utils/
    ├── crypto.rs       — Argon2id, CSRF tokens, session IDs, IP hashing
    ├── files.rs        — upload validation, thumbnail generation, file sizing
    ├── sanitize.rs     — HTML escaping, markup renderer (greentext, spoilers, links)
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
| **File safety** | Two-layer check: Content-Type header + magic byte inspection. File extension never trusted |
| **XSS** | All user input passes through `escape_html()` before insertion. Markup applied after escaping |
| **Path traversal** | Backup filenames validated to `[a-zA-Z0-9._-]` only before any filesystem operation |
| **Backup restore** | Uses `sqlite3_backup_init()` — no file swapping, no WAL corruption, no restart required |

<br>

## 📝 Post Markup Reference

```
>quoted text              greentext line
>>123                     reply link — jumps to post #123 on the same board
>>>/board/                cross-board index link (amber)
>>>/board/123             cross-board thread link (amber)
**text**                  bold
__text__                  italic
[spoiler]text[/spoiler]   hidden until clicked/hovered
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

**Latest — v1.0.6:**
- Complete web-based backup system — full and board backups saved to `rustchan-data/`, manageable entirely from the admin panel without touching the file explorer
- Per-action clarity: every button explicitly says **💾 save to server** or **⬇ download to computer**
- Board-level backup & restore with full row-ID remapping and transaction safety
- GitHub Actions CI across 5 targets: macOS x86/ARM, Linux x86/ARM64, Windows x86-64

<br>

---

<div align="center">

Built with 🦀 Rust &nbsp;·&nbsp; Powered by SQLite &nbsp;·&nbsp; Zero runtime dependencies

*Drop it anywhere. It just runs.*

</div>
