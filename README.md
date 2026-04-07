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
[![Version](https://img.shields.io/badge/Version-1.1.0-0ea5e9?style=for-the-badge)](#changelog)

<br>

[**Quick Start**](#-quick-start) · [**Features**](#-features) · [**ChanNet API**](#-channet-api) · [**Optional Integrations**](#-optional-integrations-ffmpeg-and-tor) · [**Configuration**](#-configuration) · [**Backup System**](#-backup--restore) · [**Deployment**](#-production-deployment) · [**Themes**](#-themes) · [**Changelog**](CHANGELOG.md)

<br>

</div>

---

RustChan is a fully-featured imageboard engine compiled into a **single Rust binary**. Deploy it on a VPS, a Raspberry Pi, or a local machine — no containers, no runtime, no package manager required. All persistent data lives in a single directory alongside the binary, making migrations as simple as `cp -r`.

**[ffmpeg](#ffmpeg--video--audio-processing)** is supported as an optional enhancement for video transcoding and audio waveforms. **[Tor](#tor--onion-service)** onion service hosting is built in via [Arti](https://gitlab.torproject.org/tpo/core/arti) — no system `tor` installation required. Both degrade gracefully when disabled.

<br>

## ✦ Features

<table>
<tr>
<td width="50%" valign="top">

### 📋 Boards & Posting
- Multiple boards with independent per-board configuration
- Threaded replies with globally unique post numbers
- **Thread polls** — OP-only, 2–10 options, live percentage bar results, one vote per IP; expired vote rows cleaned up automatically
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
- Trailing slash normalization — all URL variants resolve correctly

</td>
<td width="50%" valign="top">

### 🖼️ Media
- **Images:** JPEG *(EXIF-stripped and orientation-corrected on upload)*, PNG, GIF, WebP, BMP, TIFF, SVG
- **Video:** MP4, WebM — auto-transcoded to VP9+Opus WebM via ffmpeg; AV1 streams re-encoded to VP9
- **GIF → WebM** — animated GIFs are converted to WebM inline at upload time (VP9, no background job)
- **Audio:** MP3, OGG, FLAC, WAV, M4A, AAC (up to 150 MB default)
- **Image + audio combo posts** — attach both an image and an audio file simultaneously
- **Audio waveform thumbnails** — generated via ffmpeg's `showwavespic` filter for standalone audio uploads
- **Waveform cache eviction** — background task prunes oldest thumbnails when the cache exceeds `waveform_cache_max_mb` (default 200 MiB); originals never touched
- **Video embed unfurling** — per-board opt-in; YouTube, Invidious, and Streamable URLs render as thumbnail + click-to-play widgets
- Auto-generated **WebP thumbnails** with configurable dimensions; SVG placeholders used for video (without ffmpeg), audio, and SVG sources
- Resizable inline image expansion via drag-to-resize
- **Client-side auto-compression** — oversized files are compressed in-browser before upload with a live progress bar
- **Streaming multipart** — uploads are validated against size limits in flight; never fully buffered in RAM; per-field text caps prevent OOM from oversized form fields
- Two-layer file validation: Content-Type header + magic byte inspection (extensions are never trusted); BMP, TIFF, and SVG magic bytes supported

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
- **PoW CAPTCHA** — per-board opt-in SHA-256 proof-of-work for all posts (threads and replies); nonce replay blocked within the 5-minute validity window
- **Report system** — users can report posts; admins see an inbox with resolve and resolve+ban actions
- **Moderation log** — append-only audit trail of all admin actions, viewable from the panel
- Word filters (pattern → replacement, site-wide)
- **Full Site Backup & Restore** — entirely web-based, no shell access required; all operations stream from disk, never buffering the full backup in RAM
- **Scheduled VACUUM** — automatic database compaction on a configurable interval; reclaimed bytes logged
- **DB size warning** — admin panel shows a red banner when the database exceeds `db_warn_threshold_mb`
- **Expired poll cleanup** — background task purges stale vote rows on a configurable schedule
- Per-board controls: editing, edit window, archiving, video embeds, PoW CAPTCHA

</td>
<td width="50%" valign="top">

### 🔒 Security
- **Argon2id** password hashing (`t=2, m=65536, p=2`)
- **Security headers** — CSP (`script-src 'self'`, no `unsafe-inline`), HSTS (1 year + subdomains) on HTTPS responses, and Permissions-Policy on all responses
- **Inline JS eliminated** — all JavaScript extracted to external `.js` files; CSP fully enforced
- **CSRF** — double-submit cookie with constant-time token comparison (`subtle::ct_eq`)
- `HttpOnly` + `SameSite=Strict` session cookies with configurable `Secure` flag and `Max-Age`
- **Admin brute-force protection** — progressive lockout after 5 consecutive failed login attempts
- **PoW nonce replay prevention** — used nonces tracked in memory for the 5-minute validity window; stale entries auto-pruned
- Raw IPs **never stored or logged** — HMAC-keyed SHA-256 hash used everywhere
- Per-IP sliding-window rate limiting on POST endpoints (10/min) and page-load GET endpoints (60/min); `/api/` routes excluded from GET limiting
- **JPEG EXIF stripping + orientation correction** — GPS, device IDs, and all metadata removed; rotation normalized on upload
- All user input HTML-escaped before rendering; markup applied post-escape
- **Zip-bomb protection** — backup restore capped at 1 GiB per entry, 50,000 entries max
- **Backup upload size cap** — full and board restore endpoints reject uploads over 512 MiB
- **Redirect hardening** — backslash and encoded variants blocked on redirect parameters
- Path traversal prevention on all filesystem operations
- **Job queue back-pressure** — queue capped at `job_queue_capacity` entries; excess jobs dropped with a log warning, never causing OOM

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🗂️ Thread Lifecycle
- **Thread archiving** — overflow threads are archived (readable, locked, hidden from index) rather than deleted; configurable per board
- **Global `archive_before_prune` flag** — ensures no thread is silently hard-deleted on any archiving-enabled instance, even if the individual board didn't opt in
- **Archive page** — `/{board}/archive` with thumbnails, reply counts, and pagination
- **Thread auto-update** — delta-compressed polling keeps reply counts, lock/sticky badges, and new posts in sync without full reloads
- **ETag / Conditional GET** — board index and thread pages return `304 Not Modified` on cache hits; ETags included on all 200 responses
- **Response compression** — gzip, Brotli, or zstd negotiated automatically via `Accept-Encoding`
- **Floating new-reply pill** — "+N new replies ↓" notification; click to scroll, auto-dismisses after 30 seconds
- **"(You)" tracking** — posts you authored are marked with a `(You)` badge, persisted across refreshes
- Per-board toggle between archive-on-overflow and permanent deletion

</td>
<td width="50%" valign="top">

### 📱 Mobile & UX
- **Mobile reply drawer** — floating action button slides up a full-width reply panel on small screens
- **Cross-board hover previews** — `>>>/board/123` links show a floating popup with client-side caching
- **Modular theme system** — built-in themes plus admin-created custom themes, all surfaced through one floating picker and persisted in `localStorage` with no flash
- **Theme controls** — site-wide defaults, per-board defaults, and runtime theme enable/disable are all configurable from the admin panel
- **Site subtitle** — `site_subtitle` in `settings.toml` customises the home page tagline at install time
- **Live stats** — total posts, uploads, and content size displayed on the home page
- **Background worker system** — video transcoding, waveform generation, and thread cleanup run asynchronously; duplicate media jobs coalesced; configurable ffmpeg timeout; exponential backoff on retries
- **Full-screen TUI console** — replaces the old scrolling line-input shell with a static full-screen dashboard; live panels for server status, request rate, online users, content counts, per-board breakdowns, storage sizes, and active upload progress; keyboard shortcuts: `[H]` help · `[R]` force-reload stats · `[L]` log view · `[B]` board list · `[C]` create board · `[A]` create admin · `[D]` delete thread · `[Q]` quit (with confirmation); wizard flows for board/admin creation and thread deletion temporarily exit raw mode for line-input, then restore the dashboard cleanly on completion; panic hook and graceful shutdown both call `cleanup()` to guarantee terminal restoration

</td>
</tr>
</table>

<br>

<img width="1511" height="781" alt="RustChan board view" src="https://github.com/user-attachments/assets/0ad5ca51-9d7a-40a6-a754-dbdaebacf66a" />
<img width="1512" height="778" alt="RustChan thread view" src="https://github.com/user-attachments/assets/5ff2658c-8689-4895-8300-9d29effdb090" />
<img width="274" height="511" alt="RustChan mobile view" src="https://github.com/user-attachments/assets/7f467e5c-92a2-4764-a7e3-8790a1dcf3e4" />

<br>

## 🌐 ChanNet API

RustChan includes a two-layer federation and gateway system that runs automatically on **port 7070** alongside the main web server. No additional configuration is required to enable it — if you want to federate with another RustChan node or integrate with a RustWave client, just start talking to port 7070.

> **Text only.** No images, no media, and no binary data cross the ChanNet interface by design. All payloads are ZIP archives containing structured text (JSON manifests + plain `.txt` post bodies). Full schema documentation is in `channet_api_reference.docx`.

### Layer 1 — Node Federation

These endpoints let RustChan nodes sync content with each other. All responses are ZIP archives.

| Endpoint | Method | Description |
|---|---|---|
| `/chan/export` | `GET` | Export all posts from this node as a ZIP snapshot |
| `/chan/import` | `POST` | Import a ZIP snapshot from a remote node |
| `/chan/refresh` | `POST` | Pull fresh content from a known remote and apply it locally |
| `/chan/poll` | `GET` | Lightweight poll — returns only new content since a given timestamp |

**Quick example — pull content from a remote node:**

```bash
# Export your node's posts
curl http://localhost:7070/chan/export -o my-export.zip

# Import a ZIP from another node
curl -X POST http://localhost:7070/chan/import \
     -H "Content-Type: application/zip" \
     --data-binary @remote-export.zip

# Refresh from a peer (supply the peer's export URL as the body)
curl -X POST http://localhost:7070/chan/refresh \
     -H "Content-Type: text/plain" \
     -d "http://peer.example.com:7070/chan/export"

# Poll for posts newer than a Unix timestamp
curl "http://localhost:7070/chan/poll?since=1741900000" -o delta.zip
```

### Layer 2 — RustWave Gateway

The `/chan/command` endpoint exposes a typed JSON command interface for the [RustWave](https://github.com/a2kiti/rustwave) audio transport client. Send a JSON command, receive a ZIP back. `reply_push` is the only command that writes anything to the database.

```bash
# Full export via command interface
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{"command": "full_export"}' \
     -o full.zip

# Export a single board
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{"command": "board_export", "board": "b"}' \
     -o board-b.zip

# Export a single thread
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{"command": "thread_export", "board": "b", "thread_id": 42}' \
     -o thread-42.zip

# Export the archive for a board
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{"command": "archive_export", "board": "b"}' \
     -o archive.zip

# Force a refresh from a peer
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{"command": "force_refresh", "peer": "http://peer.example.com:7070"}' \
     -o result.zip

# Push a reply (the only write command)
curl -X POST http://localhost:7070/chan/command \
     -H "Content-Type: application/json" \
     -d '{
       "command": "reply_push",
       "board": "b",
       "thread_id": 42,
       "body": "Hello from RustWave"
     }' \
     -o result.zip
```

### Firewall Note

Port 7070 is for node-to-node communication and RustWave integration. If you are running a public-facing instance and do not need federation, block port 7070 externally:

```bash
sudo ufw deny 7070/tcp
```

If you do want to federate with other nodes, allow port 7070 selectively rather than opening it to the world.

---

## 🔌 Optional Integrations: ffmpeg and Tor

RustChan is fully functional without any of these tools. When enabled or detected at startup, additional capabilities activate automatically.

### ffmpeg — Video & Audio Processing

When ffmpeg is available on `PATH`:

- **MP4 → WebM transcoding** (VP9 + Opus) for maximum browser compatibility
- **AV1 WebM → VP9 re-encoding** for browsers without AV1 support
- **Audio waveform thumbnails** via the `showwavespic` filter
- **Video thumbnail extraction** from the first frame for catalog previews

Without ffmpeg, videos are served in their original format and audio posts use a generic icon. Set `require_ffmpeg = true` in `settings.toml` to enforce its presence at startup. The ffmpeg execution timeout is configurable via `ffmpeg_timeout_secs` (default: 120).

See **[SETUP.md — Installing ffmpeg](SETUP.md#installing-ffmpeg)** for platform-specific instructions.

### Tor — Onion Service

RustChan includes **built-in Tor onion service support via [Arti](https://gitlab.torproject.org/tpo/core/arti)** — no system `tor` installation required. Set `enable_tor_support = true` in `settings.toml` and restart. On first launch RustChan will:

1. Download ~2 MB of Tor directory data and bootstrap to the network (~30 seconds)
2. Generate a persistent Ed25519 keypair in `rustchan-data/runtime/tor/state/keys/`
3. Derive your permanent `.onion` address from that keypair and start the hidden service
4. Begin accepting and proxying inbound onion connections to the local HTTP port

The `.onion` address appears on the home page and in the admin panel as soon as the service is ready. Subsequent starts are ready in ~5 seconds using the cached consensus in `rustchan-data/runtime/tor/cache/`.

**Back up `rustchan-data/runtime/tor/state/keys/`** — this directory contains your service keypair. Losing it means a new `.onion` address on the next start. Delete it intentionally to rotate to a new address.

See **[SETUP.md — Tor](SETUP.md#tor--onion-service)** for details on key management and migrating from a previous system `tor` installation.

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

By default, RustChan stores its runtime state in `rustchan-data/` next to the binary. `CHAN_DB` and `CHAN_UPLOADS` can move the database and upload tree, but the generated settings file, logs, Tor state, and TLS state still live under `rustchan-data/`.

```
rustchan-cli                              ← single self-contained binary
rustchan-data/
├── settings.toml                         ← auto-generated config file
├── chan.db                               ← SQLite database (or elsewhere via CHAN_DB)
├── chan.db-wal                           ← SQLite WAL file while writes are active
├── chan.db-shm                           ← SQLite shared-memory sidecar
├── logs/
│   └── rustchan.YYYY-MM-DD.log           ← daily rotated human-readable logs
├── backups/
│   ├── full/                             ← full site backups
│   │   └── rustchan-backup-20260304_120000.zip
│   └── boards/                           ← per-board backups
│       └── rustchan-board-tech-20260304_120000.zip
├── runtime/
│   ├── tls/
│   │   ├── dev/
│   │   │   ├── self-signed.crt           ← auto-generated localhost dev cert
│   │   │   └── self-signed.key
│   │   └── acme/                         ← ACME cache when [tls.acme] is enabled
│   ├── tor/
│   │   ├── state/                        ← Tor onion-service key material/state
│   │   └── cache/                        ← Tor cache data
│   ├── favicon/                          ← generated global favicon assets
│   └── tmp/
│       └── board-downloads/              ← temporary admin backup download files
└── boards/
    ├── .pending/                         ← crash-safe staging area for uploads/restores
    ├── b/
    │   ├── <uuid>.<ext>                  ← uploaded files
    │   └── thumbs/
    │       ├── <uuid>.webp               ← image/video thumbnails
    │       └── <uuid>.png                ← generated audio waveforms
    └── tech/
        ├── <uuid>.<ext>
        └── thumbs/
```

<br>

## ⚙️ Configuration

### settings.toml

`settings.toml` is generated on first run in `rustchan-data/settings.toml`. Edit it and restart RustChan to apply changes. Environment variables still take precedence.

```toml
# Site identity
forum_name = "RustChan"
site_subtitle = "select board to proceed"

# Default theme served to first-time visitors.
# Options: terminal, aero, dorfic, fluorogrid, neoncubicle, chanclassic
default_theme = "fluorogrid"

# Main HTTP port.
port = 8080

# Upload size limits (MiB).
max_image_size_mb = 8
max_video_size_mb = 50
max_audio_size_mb = 150

# Auto-generated on first run. Do not change after first use unless you want
# to invalidate CSRF tokens, IP hashes, and ban lookups.
cookie_secret = "<auto-generated 32-byte hex>"

# Built-in Tor onion service (via Arti — no system tor required).
enable_tor_support = true
# tor_only = false
# tor_bootstrap_timeout_secs = 120
# tor_max_concurrent_streams = 512
# tor_service_nickname = "rustchan"

# Media processing / feature flags.
require_ffmpeg = false
# ffmpeg_path = "/usr/local/bin/ffmpeg"
# ffprobe_path = "/usr/local/bin/ffprobe"
# enable_any_file_uploads_feature = false
ffmpeg_timeout_secs = 120

# Maintenance / background work.
wal_checkpoint_interval_secs = 3600
auto_vacuum_interval_hours = 24
poll_cleanup_interval_hours = 72
db_warn_threshold_mb = 2048
job_queue_capacity = 1000
waveform_cache_max_mb = 200
archive_before_prune = true
blocking_threads = 0
db_pool_size = 8

# ChanNet / RustWave integration.
# rustwave_url = "http://localhost:7071"
# chan_net_bind = "127.0.0.1:7070"

# TLS / HTTPS. The generated template includes this section enabled by default.
[tls]
enabled = true
port = 8443
# redirect_http = true
# http_port = 8080

# [tls.acme]
# enabled = true
# staging = true
# domains = ["example.com"]
# email = "admin@example.com"
# cache_dir = "runtime/tls/acme"
```

### Environment Variables

All settings can be overridden via environment variables, which take precedence over `settings.toml`.

| Variable | Default | Description |
|---|---|---|
| `CHAN_FORUM_NAME` | `RustChan` | Site display name |
| `CHAN_SITE_SUBTITLE` | `select board to proceed` | Initial home page subtitle |
| `CHAN_DEFAULT_THEME` | `fluorogrid` | Default theme for new visitors |
| `CHAN_PORT` | `8080` | TCP port |
| `CHAN_HOST` | `0.0.0.0` | Host used when `CHAN_BIND` is unset |
| `CHAN_BIND` | `0.0.0.0:8080` | Full bind address override |
| `CHAN_DB` | `rustchan-data/chan.db` | SQLite database path |
| `CHAN_UPLOADS` | `rustchan-data/boards` | Uploads directory |
| `CHAN_COOKIE_SECRET` | *(from settings.toml)* | CSRF tokens and IP hashing key |
| `CHAN_MAX_IMAGE_MB` | `8` | Max image upload size (MiB) |
| `CHAN_MAX_VIDEO_MB` | `50` | Max video upload size (MiB) |
| `CHAN_MAX_AUDIO_MB` | `150` | Max audio upload size (MiB) |
| `CHAN_THUMB_SIZE` | `250` | Thumbnail max dimension (px) |
| `CHAN_BUMP_LIMIT` | `500` | Replies before a thread stops bumping |
| `CHAN_MAX_THREADS` | `150` | Max threads per board before pruning/archiving |
| `CHAN_RATE_GETS` | `60` | Max GETs per rate-limit window per IP |
| `CHAN_RATE_WINDOW` | `60` | Rate-limit window (seconds) |
| `CHAN_SESSION_SECS` | `28800` | Admin session duration (default: 8 hours) |
| `CHAN_BEHIND_PROXY` | `false` | Trust `X-Forwarded-For` behind a reverse proxy |
| `CHAN_TRUSTED_PROXY_CIDRS` | `127.0.0.1/32,::1/128` | Comma-separated CIDRs allowed to supply forwarding headers |
| `CHAN_HTTPS_COOKIES` | *(auto: true when behind proxy or TLS enabled)* | Set `Secure` flag on session cookies |
| `CHAN_PUBLIC_HOSTS` | *(empty)* | Comma-separated public hosts accepted by the HTTP→HTTPS redirect listener |
| `CHAN_TOR_SUPPORT` | `true` | Enable the built-in Arti onion service |
| `CHAN_TOR_ONLY` | `false` | Bind loopback-only and serve exclusively over Tor |
| `CHAN_TOR_BOOTSTRAP_TIMEOUT` | `120` | Tor bootstrap timeout (seconds) |
| `CHAN_TOR_MAX_STREAMS` | `512` | Max simultaneous inbound Tor streams |
| `CHAN_TOR_NICKNAME` | `rustchan` | Onion service nickname under `runtime/tor/state/` |
| `CHAN_REQUIRE_FFMPEG` | `false` | Exit at startup if ffmpeg is unavailable |
| `CHAN_FFMPEG_PATH` | `ffmpeg` | ffmpeg executable path |
| `CHAN_FFPROBE_PATH` | `ffprobe` | ffprobe executable path |
| `CHAN_ENABLE_ANY_FILE_UPLOADS_FEATURE` | `false` | Master switch for arbitrary file uploads |
| `CHAN_WAL_CHECKPOINT_SECS` | `3600` | WAL checkpoint interval; `0` to disable |
| `CHAN_AUTO_VACUUM_HOURS` | `24` | Scheduled VACUUM interval (hours); `0` to disable |
| `CHAN_POLL_CLEANUP_HOURS` | `72` | Expired poll vote cleanup interval (hours) |
| `CHAN_DB_WARN_THRESHOLD_MB` | `2048` | DB size warning threshold (MiB) |
| `CHAN_JOB_QUEUE_CAPACITY` | `1000` | Max pending background jobs |
| `CHAN_FFMPEG_TIMEOUT_SECS` | `120` | Max duration for a single ffmpeg job |
| `CHAN_WAVEFORM_CACHE_MAX_MB` | `200` | Max total thumbnail/waveform cache size (MiB) |
| `CHAN_BLOCKING_THREADS` | `cpus × 4` | Tokio blocking thread pool size |
| `CHAN_ARCHIVE_BEFORE_PRUNE` | `true` | Archive globally before any hard-delete |
| `CHAN_DB_POOL_SIZE` | `8` | SQLite connection pool size |
| `CHAN_RUSTWAVE_URL` | `http://localhost:7071` | RustWave base URL |
| `CHAN_NET_BIND` | `127.0.0.1:7070` | ChanNet listener bind address |
| `CHAN_NET_MAX_BODY` | `10485760` | Max `/chan/import` body size in bytes |
| `CHAN_NET_COMMAND_MAX_BODY` | `8192` | Max `/chan/command` body size in bytes |
| `CHAN_NET_API_KEY` | *(empty)* | Enables authenticated `/chan/refresh` and `/chan/poll` |
| `RUST_LOG` | `rustchan-cli=info` | Log verbosity |

<br>

## 💾 Backup & Restore

The entire backup system is accessible from the admin panel — no shell access required. All backup operations stream from disk in 64 KiB chunks; peak RAM overhead is roughly 64 KiB regardless of instance size. Backups are written to disk as temp files with an atomic rename on success, so partial backups never appear in the saved list.

### Full Site Backups

A full backup is a `.zip` containing a consistent SQLite snapshot (via `VACUUM INTO`) and all uploaded files.

| Action | Description |
|---|---|
| **💾 Save** | Creates a backup and writes it to `rustchan-data/backups/full/` |
| **⬇ Download** | Streams a saved backup to your browser |
| **↺ Restore (server)** | Restores from a file already on the server |
| **↺ Restore (upload)** | Restores from a `.zip` uploaded from your computer (max 512 MiB) |
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
- Raspberry Pi SD card wear reduction and blocking thread tuning
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

RustChan is still a single-binary Axum app, but the current codebase has grown beyond the original "server + templates + db" shape. It now includes a full admin panel, crash-safe filesystem staging for uploads/restores, background media workers, optional TLS, and an optional ChanNet/RustWave sidecar listener.

| Layer | Technology |
|---|---|
| Web framework | [Axum](https://github.com/tokio-rs/axum) 0.8 |
| Async runtime | [Tokio](https://tokio.rs/) 1.x with a configurable blocking pool |
| Database | SQLite via [rusqlite](https://github.com/rusqlite/rusqlite) |
| Connection pool | `r2d2` + `r2d2_sqlite` (configurable pool size) |
| Rendering | Plain Rust HTML string rendering in `src/templates/` |
| Media pipeline | `image` crate, `kamadak-exif`, and optional `ffmpeg` / `ffprobe` |
| Background work | In-process worker queue for transcodes, waveforms, and cache cleanup |
| Password hashing | `argon2` crate (Argon2id) |
| Timing-safe comparison | `subtle` crate |
| Middleware | `tower-http` plus custom middleware for CSRF, IP handling, rate limiting, and state |
| Logging | `tracing` + `tracing-subscriber` with stdout formatting and daily file rotation |
| Configuration | `settings.toml` + env var overrides via `std::sync::LazyLock` |
| TLS | `rustls`, self-signed dev certs, optional ACME, optional manual certs |
| Tor onion service | [Arti](https://gitlab.torproject.org/tpo/core/arti) in-process |
| Federation | Optional ChanNet / RustWave listener and import/export pipeline |

### Source Layout

```
src/
├── main.rs              — binary entry point, runtime construction, CLI dispatch
├── lib.rs               — shared library surface for server/admin code
├── config.rs            — settings loading, env overrides, validation
├── config/template.rs   — generated settings.toml template
├── logging.rs           — terminal + file logging setup
├── detect.rs            — startup diagnostics for ffmpeg, Tor, TLS, and environment checks
├── error.rs             — app error types and error responses
├── models.rs            — shared row/view models
├── pending_fs.rs        — durable pending filesystem ops for uploads/restores
├── server/
│   ├── mod.rs           — server subsystem exports
│   ├── cli.rs           — clap CLI and admin subcommands
│   ├── server.rs        — startup, listeners, background tasks, shutdown
│   ├── console/
│   │   ├── mod.rs       — alternate-screen TUI orchestration
│   │   ├── dashboard.rs — pure render functions: dashboard, log view, help, board list, confirm quit
│   │   ├── input.rs     — crossterm input handling
│   │   └── wizard.rs    — interactive admin wizards
│   └── server/          — router, lifecycle, assets, headers, observability
├── media/
│   ├── mod.rs           — media pipeline public API
│   ├── convert.rs       — conversion/transcode decisions
│   ├── exif.rs          — EXIF orientation handling
│   ├── ffmpeg.rs        — ffmpeg/ffprobe execution helpers
│   └── thumbnail.rs     — thumbnail and waveform generation helpers
├── handlers/
│   ├── admin/
│   │   ├── mod.rs       — admin route wiring and shared helpers
│   │   ├── auth.rs      — login/logout/session handling
│   │   ├── backup.rs    — full-site and per-board backup/restore
│   │   ├── content.rs   — board/thread/post admin actions
│   │   ├── moderation.rs — bans, reports, filters, appeals, mod log
│   │   └── settings.rs  — site settings and maintenance actions
│   ├── mod.rs           — shared posting/upload helpers
│   ├── board.rs         — board index, catalog, archive, search, thread creation
│   ├── posting.rs       — pending upload finalization helpers
│   ├── render.rs        — shared page rendering helpers
│   ├── thread.rs        — thread view, replies, polls, editing
│   └── favicon.rs       — favicon handlers
├── db/
│   ├── mod.rs           — DB exports and shared helpers
│   ├── pool.rs          — pool creation and first-run checks
│   ├── schema.rs        — full schema bootstrap
│   ├── migrations.rs    — incremental schema fixes
│   ├── fs_ops.rs        — pending filesystem op persistence
│   ├── boards.rs        — boards and site settings
│   ├── threads.rs       — thread lifecycle, archive, pruning
│   ├── posts.rs         — posts, files, polls, background jobs
│   ├── admin.rs         — admins, sessions, bans, reports, filters
│   ├── chan_net.rs      — ChanNet persistence helpers
│   └── user_thread_prefs.rs — per-user thread preferences
├── chan_net/            — import/export/refresh/poll/command handlers
├── middleware/          — CSRF, IP extraction, rate limiting, normalization, state
├── templates/
│   ├── mod.rs           — base layout and shared helpers
│   ├── admin.rs         — admin UI rendering
│   ├── board.rs         — board and index page rendering
│   ├── forms.rs         — posting forms
│   └── thread.rs        — thread/post rendering
├── tls/                 — self-signed and ACME TLS support
├── workers/             — background job queue and media/cache workers
└── utils/
    ├── crypto.rs        — password hashing, sessions, CSRF, IP hashing
    ├── files.rs         — upload storage and validation helpers
    ├── sanitize.rs      — escaping and post markup rendering
    ├── sanitize/formatting.rs — formatting parser details
    ├── tripcode.rs      — tripcode generation
    └── files/           — MIME, disk, JPEG, and storage helpers
```

<br>

## 🔐 Security Model

| Concern | Implementation |
|---|---|
| **Passwords** | Argon2id (`t=2, m=65536, p=2`) — memory-hard, GPU-resistant |
| **Brute-force** | Progressive lockout after 5 failed admin login attempts per IP |
| **Sessions** | `HttpOnly`, `SameSite=Strict`, `Max-Age` aligned to server config |
| **CSRF** | Double-submit cookie with constant-time token comparison (`subtle::ct_eq`) |
| **Security headers** | CSP (`script-src 'self'`, no `unsafe-inline`), HSTS (1 year + subdomains) on HTTPS responses, Permissions-Policy |
| **Inline JavaScript** | Fully eliminated — all JS in external files; CSP enforced with no `unsafe-inline` |
| **IP privacy** | Raw IPs never stored or logged — HMAC-keyed SHA-256 hash used everywhere |
| **Rate limiting** | Sliding-window per hashed IP: POST endpoints (10/min), page-load GETs (60/min); `/api/` routes excluded |
| **Proxy support** | All handlers use proxy-aware IP extraction when `CHAN_BEHIND_PROXY=true` |
| **File safety** | Content-Type + magic byte validation; file extensions never trusted |
| **EXIF stripping** | All JPEG uploads re-encoded — GPS, device IDs, and all metadata discarded; EXIF orientation applied before strip |
| **XSS** | All user input HTML-escaped before rendering; markup applied post-escape |
| **Zip-bomb protection** | Backup restore capped at 1 GiB per entry, 50,000 entries max |
| **Backup upload cap** | Full and board restore endpoints reject uploads over 512 MiB |
| **Redirect hardening** | Backslash and percent-encoded variants blocked on `return_to` parameters |
| **Path traversal** | Backup filenames validated against `[a-zA-Z0-9._-]` before filesystem access |
| **Body limits** | Per-route limits on small endpoints (64 KiB) to prevent memory exhaustion |
| **Connection pool** | Configurable pool size; 5-second acquisition timeout; pool exhaustion returns 503 (not 500) |
| **PoW CAPTCHA** | SHA-256 hashcash (20-bit difficulty), verified server-side with 5-minute grace window; covers threads and replies |
| **PoW nonce replay** | Used nonces tracked in memory; stale entries auto-pruned after the validity window expires |
| **Job queue** | Capped at `job_queue_capacity`; excess jobs logged and dropped, never causing OOM |
| **Streaming uploads** | Multipart fields validated against size limits in flight; per-field text caps (~100 KB body, ~4 KB name/subject) prevent OOM from oversized forms |
| **Request timeout** | Middleware terminates slow or stalled client connections; guards against slowloris-style attacks |
| **Gateway IP safety** | ChanNet gateway posts carry no IP address; `ip_hash` is nullable throughout — `NULL` rendered as empty string, never causes a 500 |
| **Atomic config writes** | `settings.toml` written via temp-file-then-rename; config never partially written on crash |

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

Built-in and admin-created custom themes are selectable from the floating picker on every page. Theme choice is persisted in `localStorage` with no flash on load, while the server also tracks a site-wide default and optional per-board defaults.

| Theme | Description |
|---|---|
| **Terminal** *(default)* | Dark background, matrix-green monospace, glowing accents |
| **Frutiger Aero** | Frosted glass panels, pearl-blue gradients, rounded corners |
| **DORFic Aero** | Dark stone walls, torchlit amber/copper glass panels |
| **Forest** | Deep woodland greens, warm brown panels, parchment text |
| **FluoroGrid** | Pale sage, muted teal grid lines, dusty lavender panels |
| **NeonCubicle** | Cool off-white, horizontal scanlines, steel-teal borders |
| **ChanClassic** | Light tan/beige background, maroon accents, blue post-number links — classic imageboard styling |

<br>

## 📋 Changelog

See **[CHANGELOG.md](CHANGELOG.md)** for the full version history.

**Latest — v1.1.0:**
ChanNet API on port `7070` · full-screen operator dashboard · native HTTPS with self-signed or Let's Encrypt support · optional HTTP to HTTPS redirects and HSTS · stronger Tor support with per-stream isolation and Tor-only mode · optional arbitrary file uploads with safe download handling · faster search, previews, and thread updates · safer posting, restore, upload, and background-job flows · cleaner server, admin, backup, middleware, and media internals

**v1.0.13:**
Scheduled VACUUM · expired poll vote cleanup · DB size warning banner · job queue back-pressure · duplicate media job coalescing · configurable ffmpeg timeout · global `archive_before_prune` flag · waveform cache eviction · streaming multipart · ETag / Conditional GET (304) · gzip/Brotli/zstd response compression · manual Tokio blocking pool sizing · EXIF orientation correction · streaming backup I/O (peak RAM ~64 KiB) · **ChanClassic** theme · `default_theme` + `site_subtitle` in `settings.toml` · default theme selector in admin panel · admin panel reorganised · prepared statement caching audit · `RETURNING` clause for inserts · 32 MiB SQLite page cache · two new DB indexes (`idx_posts_thread_id`, `idx_posts_ip_hash`)

**v1.0.12:** Database module split into 5 focused files · template module split into 5 focused files · PoW bypass on replies fixed (critical) · PoW nonce replay protection · inline JS fully eliminated (`script-src 'self'` CSP) · backup upload size cap (512 MiB) · post rate limiting simplified · `/api/` routes excluded from GET rate limit · trailing slash 404s fixed

**v1.0.11:** Security headers (CSP, HSTS, Permissions-Policy) · proxy-aware IP extraction on all handlers · GET rate limiting (60 req/min) · zip-bomb protection on restore · IP hashing everywhere · admin brute-force lockout · constant-time CSRF comparison · poll input caps · session cookie `Max-Age` · connection pool timeout · per-route body limits · open redirect hardening · worker exponential backoff · file dedup race fix · per-post ban+delete · ban appeal system · PoW CAPTCHA · video embeds · cross-board hover previews · new-reply pill · live thread metadata · "(You)" tracking · spoiler text

**v1.0.9:** Per-board editing toggle · configurable edit window · per-board archive toggle · AV1→VP9 transcoding fix

**v1.0.8:** Thread archiving · mobile reply drawer · dice rolling · sage · post editing · draft autosave · WAL checkpointing · VACUUM button · IP history

**v1.0.7:** EXIF stripping · image+audio combo posts · audio waveform thumbnails

**v1.0.6:** Web-based backup management · board-level backup/restore · GitHub Actions CI

**v1.0.5:** MP4→WebM auto-transcoding · home page stats · macOS Tor detection fix

<br>

---

<div align="center">

Built with 🦀 Rust &nbsp;·&nbsp; Powered by SQLite &nbsp;·&nbsp; Optional: ffmpeg &nbsp;·&nbsp; Tor built-in via Arti

*Drop it anywhere. It just runs.*

</div>
